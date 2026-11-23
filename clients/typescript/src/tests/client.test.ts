/**
 * Unit tests for the VectorLedger TypeScript client.
 * Uses Node.js built-in test runner (no external test framework needed).
 */

import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { Row, VledgerError } from "../types";
import type { VledgerRawResponse } from "../types";

// ── Row tests ────────────────────────────────────────────────────────────────

describe("Row", () => {
  it("get() returns the correct value", () => {
    const row = new Row(["a", "b"], [1, 2]);
    assert.equal(row.get("a"), 1);
    assert.equal(row.get("b"), 2);
  });

  it("get() throws for unknown column", () => {
    const row = new Row(["x"], ["hello"]);
    assert.throws(() => row.get("z"), /Column 'z' not found/);
  });

  it("getOrDefault() returns default for unknown column", () => {
    const row = new Row([], []);
    assert.equal(row.getOrDefault("missing", 99), 99);
    assert.equal(row.getOrDefault("missing"), null);
  });

  it("toObject() converts to plain object", () => {
    const row = new Row(["id", "name"], [42, "Cash"]);
    assert.deepEqual(row.toObject(), { id: 42, name: "Cash" });
  });
});

// ── VledgerError tests ────────────────────────────────────────────────────────

describe("VledgerError", () => {
  it("carries the sql field", () => {
    const err = new VledgerError("account not found", "SELECT BALANCE('X')");
    assert.equal(err.sql, "SELECT BALANCE('X')");
    assert.match(err.message, /account not found/);
    assert.equal(err.name, "VledgerError");
  });
});

// ── QueryResult parsing ───────────────────────────────────────────────────────

import { VledgerClient } from "../client";

// @ts-expect-error — testing private static
const parseResult = (raw: VledgerRawResponse) => VledgerClient._parseResult(raw);

describe("VledgerClient._parseResult", () => {
  it("parses a basic SELECT response", () => {
    const raw: VledgerRawResponse = {
      ok:            true,
      columns:       ["balance"],
      rows:          [[99000]],
      rows_affected: 1,
      message:       "balance = 99000",
    };
    const result = parseResult(raw);
    assert.equal(result.rows.length, 1);
    assert.equal(result.rows[0].get("balance"), 99000);
    assert.equal(result.rowsAffected, 1);
    assert.equal(result.proof, null);
  });

  it("parses proof when present", () => {
    const raw: VledgerRawResponse = {
      ok:       true,
      columns:  ["sequence"],
      rows:     [[1]],
      rows_affected: 1,
      message:  "ok",
      proof:    { root_hex: "deadbeef", leaf_count: 1, verified: true },
    };
    const result = parseResult(raw);
    assert.notEqual(result.proof, null);
    assert.equal(result.proof!.rootHex, "deadbeef");
    assert.equal(result.proof!.verified, true);
  });

  it("handles empty result set", () => {
    const raw: VledgerRawResponse = { ok: true };
    const result = parseResult(raw);
    assert.equal(result.rows.length, 0);
    assert.equal(result.rowsAffected, 0);
  });
});

// ── Authentication ────────────────────────────────────────────────────────────

import * as net from "node:net";
import { VledgerAuthError } from "../types";

/** Plaintext fake server that enforces the real handshake. */
function authServer(): Promise<{ port: number; frames: any[]; close: () => void }> {
  const frames: any[] = [];
  const sockets = new Set<net.Socket>();
  const srv = net.createServer((sock) => {
    sockets.add(sock);
    let authed = false;
    let buf = "";
    sock.on("data", (d) => {
      buf += d.toString("utf8");
      let i: number;
      while ((i = buf.indexOf("\n")) >= 0) {
        const req = JSON.parse(buf.slice(0, i));
        buf = buf.slice(i + 1);
        frames.push(req);
        if (!authed) {
          if (req.auth?.username === "alice" && req.auth?.password === "s3cr3t") {
            authed = true;
            sock.write(JSON.stringify({ ok: true, token: "tok123", role: "auditor", message: "authenticated" }) + "\n");
            continue;
          }
          sock.end(JSON.stringify({ ok: false, error: "Authentication error: invalid credentials" }) + "\n");
          return;
        }
        if (req.token !== "tok123") {
          sock.end(JSON.stringify({ ok: false, error: "invalid or expired session token" }) + "\n");
          return;
        }
        sock.write(JSON.stringify({ ok: true, columns: ["n"], rows: [[1]], rows_affected: 1, message: "ok" }) + "\n");
      }
    });
  });
  return new Promise((resolve) =>
    srv.listen(0, "127.0.0.1", () =>
      resolve({
        port: (srv.address() as net.AddressInfo).port,
        frames,
        close: () => { sockets.forEach((s) => s.destroy()); srv.close(); },
      }),
    ),
  );
}

describe("VledgerClient authentication", () => {
  it("logs in first and sends the token on every request", async () => {
    const s = await authServer();
    const db = await VledgerClient.connect({ port: s.port, tls: false, username: "alice", password: "s3cr3t" });
    try {
      const r = await db.query("SELECT 1");
      assert.equal(r.rows[0].get("n"), 1);
      assert.deepEqual(s.frames[0], { auth: { username: "alice", password: "s3cr3t" } });
      assert.equal(s.frames[1].token, "tok123");
      assert.equal(s.frames[1].sql, "SELECT 1");
    } finally { db.close(); s.close(); }
  });

  it("rejects a wrong password with VledgerAuthError", async () => {
    const s = await authServer();
    try {
      await assert.rejects(
        VledgerClient.connect({ port: s.port, tls: false, username: "alice", password: "nope" }),
        (e: unknown) => e instanceof VledgerAuthError && /invalid credentials/.test((e as Error).message),
      );
    } finally { s.close(); }
  });

  it("an unauthenticated query is refused by an auth-enforcing server", async () => {
    const s = await authServer();
    const db = await VledgerClient.connect({ port: s.port, tls: false });
    try {
      await assert.rejects(db.query("SELECT 1"), VledgerError);
    } finally { db.close(); s.close(); }
  });

  it("requires username and password together", async () => {
    await assert.rejects(VledgerClient.connect({ tls: false, username: "alice" }), VledgerAuthError);
  });
});
