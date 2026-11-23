#!/usr/bin/env node
// VectorLedger web gateway.
//
// Browsers cannot open the raw TLS socket on port 5433, so this process holds
// the connection to `vledger start` and exposes a small, read-only HTTP API
// plus the static UI (../index.html) on the same origin.
//
// Design rules:
//   * Users log in with their own VectorLedger account. The gateway never
//     stores passwords. RBAC and the WORM audit log therefore apply per user.
//   * Only a fixed set of read-only SQL statements is ever sent. User input is
//     validated (integers, account-code charset) before it is interpolated.
//   * No dependencies. Node >= 18.
//
// Configuration (environment variables):
//   VLEDGER_HOST            ledger host                      (default 127.0.0.1)
//   VLEDGER_PORT            ledger native TLS port           (default 5433)
//   VLEDGER_CA_CERT         PEM file used to verify the ledger certificate.
//                           For a default install use <data-dir>/catalog/tls_cert.pem
//   VLEDGER_TLS_SERVERNAME  name to verify against the cert  (default "localhost" when
//                           VLEDGER_CA_CERT is set and the host is an IP address)
//   VLEDGER_TLS_INSECURE=1  skip certificate verification. Loopback hosts only.
//   GATEWAY_BIND            host:port to listen on           (default 127.0.0.1:8088)
//   GATEWAY_TLS_CERT/KEY    PEM files. Required when GATEWAY_BIND is not loopback.
//   COUNTERPARTY_ALLOWLIST  path to a JSON array of approved counterparty IDs (optional)
//   GATEWAY_INDEX_HTML      path to the UI file              (default ../index.html)

import http from 'node:http';
import https from 'node:https';
import tls from 'node:tls';
import net from 'node:net';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const env = process.env;

// ── configuration ─────────────────────────────────────────────────────────────
const LEDGER_HOST = env.VLEDGER_HOST || '127.0.0.1';
const LEDGER_PORT = Number(env.VLEDGER_PORT || 5433);
const isLoopback = h => ['127.0.0.1', '::1', 'localhost', '[::1]'].includes(h);
const [BIND_HOST, BIND_PORT] = (() => {
  const b = env.GATEWAY_BIND || '127.0.0.1:8088';
  const i = b.lastIndexOf(':');
  return [b.slice(0, i).replace(/^\[|\]$/g, ''), Number(b.slice(i + 1))];
})();
const INDEX_PATH = env.GATEWAY_INDEX_HTML || path.join(here, '..', 'index.html');

const SESSION_IDLE_MS = 15 * 60 * 1000;
const SESSION_MAX_MS = 55 * 60 * 1000; // ledger tokens live 60 min
const LEDGER_TIMEOUT_MS = 15_000;
// A full VERIFY_CHAIN streams every entry and sends nothing until it finishes
// (about 1-1.4 s per million entries on an SSD, more when the database does not
// fit in RAM). It needs a much longer idle timeout than point lookups.
const VERIFY_CHAIN_TIMEOUT_MS = Number(env.VERIFY_CHAIN_TIMEOUT_MS || 30 * 60 * 1000);
// MERKLE_ROOT loads the whole range into server memory (about 0.7 GB per
// million entries), so cap the range a browser user can request.
const MERKLE_MAX_RANGE = Number(env.MERKLE_MAX_RANGE || 1_000_000);
const MAX_LINE_BYTES = 5 * 1024 * 1024;
const MAX_BODY_BYTES = 8 * 1024;

function fail(msg) { console.error(`gateway: ${msg}`); process.exit(1); }

if (!Number.isInteger(LEDGER_PORT) || LEDGER_PORT < 1 || LEDGER_PORT > 65535) fail('invalid VLEDGER_PORT');
if (!Number.isInteger(BIND_PORT) || BIND_PORT < 1 || BIND_PORT > 65535) fail('invalid GATEWAY_BIND');
if (!Number.isInteger(VERIFY_CHAIN_TIMEOUT_MS) || VERIFY_CHAIN_TIMEOUT_MS < 1000) fail('invalid VERIFY_CHAIN_TIMEOUT_MS');
if (!Number.isInteger(MERKLE_MAX_RANGE) || MERKLE_MAX_RANGE < 1) fail('invalid MERKLE_MAX_RANGE');

let caPem = null;
if (env.VLEDGER_CA_CERT) {
  try { caPem = fs.readFileSync(env.VLEDGER_CA_CERT); } catch (e) { fail(`cannot read VLEDGER_CA_CERT: ${e.message}`); }
}
const insecure = env.VLEDGER_TLS_INSECURE === '1';
if (insecure && !isLoopback(LEDGER_HOST)) fail('VLEDGER_TLS_INSECURE=1 is only allowed for loopback ledger hosts');
if (!caPem && !insecure && isLoopback(LEDGER_HOST)) {
  fail('set VLEDGER_CA_CERT=<data-dir>/catalog/tls_cert.pem (or VLEDGER_TLS_INSECURE=1 for a throwaway local test)');
}
if (insecure) console.warn('gateway: WARNING ledger TLS certificate verification is disabled (loopback only).');
const tlsServername = env.VLEDGER_TLS_SERVERNAME || (caPem && net.isIP(LEDGER_HOST) ? 'localhost' : undefined);

let httpsOpts = null;
if (!isLoopback(BIND_HOST)) {
  if (!env.GATEWAY_TLS_CERT || !env.GATEWAY_TLS_KEY) {
    fail('GATEWAY_BIND is not loopback: GATEWAY_TLS_CERT and GATEWAY_TLS_KEY are required (passwords must not travel in clear text)');
  }
  httpsOpts = { cert: fs.readFileSync(env.GATEWAY_TLS_CERT), key: fs.readFileSync(env.GATEWAY_TLS_KEY), minVersion: 'TLSv1.2' };
} else if (env.GATEWAY_TLS_CERT && env.GATEWAY_TLS_KEY) {
  httpsOpts = { cert: fs.readFileSync(env.GATEWAY_TLS_CERT), key: fs.readFileSync(env.GATEWAY_TLS_KEY), minVersion: 'TLSv1.2' };
}
const SECURE_COOKIE = !!httpsOpts;

let allowlist = null;
if (env.COUNTERPARTY_ALLOWLIST) {
  try {
    const a = JSON.parse(fs.readFileSync(env.COUNTERPARTY_ALLOWLIST, 'utf8'));
    if (!Array.isArray(a) || !a.every(x => typeof x === 'string')) throw new Error('must be a JSON array of strings');
    allowlist = new Set(a);
  } catch (e) { fail(`COUNTERPARTY_ALLOWLIST: ${e.message}`); }
}

// ── ledger wire protocol (newline-delimited JSON over TLS) ───────────────────
class LedgerError extends Error {
  constructor(message, kind = 'ledger') { super(message); this.kind = kind; }
}

function openLedger(timeoutMs = LEDGER_TIMEOUT_MS) {
  return new Promise((resolve, reject) => {
    const sock = tls.connect({
      host: LEDGER_HOST,
      port: LEDGER_PORT,
      ca: caPem || undefined,
      servername: tlsServername,
      rejectUnauthorized: !insecure,
      minVersion: 'TLSv1.3',
    });
    const onErr = e => reject(new LedgerError(`cannot connect to ledger: ${e.message}`, 'unavailable'));
    sock.once('error', onErr);
    sock.once('secureConnect', () => { sock.removeListener('error', onErr); resolve(new LedgerConn(sock)); });
    sock.setTimeout(timeoutMs, () => sock.destroy(new Error(`no response from ledger within ${Math.round(timeoutMs / 1000)}s`)));
  });
}

class LedgerConn {
  constructor(sock) {
    this.sock = sock; this.buf = ''; this.waiter = null; this.closed = null;
    sock.setEncoding('utf8');
    sock.on('data', d => {
      this.buf += d;
      if (this.buf.length > MAX_LINE_BYTES) { sock.destroy(new Error('response too large')); return; }
      this._drain();
    });
    const onEnd = e => { this.closed = e || new Error('connection closed by ledger'); this._drain(); };
    sock.on('error', onEnd); sock.on('close', () => onEnd());
  }
  _drain() {
    if (!this.waiter) return;
    const i = this.buf.indexOf('\n');
    if (i >= 0) {
      const line = this.buf.slice(0, i); this.buf = this.buf.slice(i + 1);
      const w = this.waiter; this.waiter = null; w.resolve(line);
    } else if (this.closed) {
      const w = this.waiter; this.waiter = null; w.reject(new LedgerError(this.closed.message, 'unavailable'));
    }
  }
  send(frame) {
    return new Promise((resolve, reject) => {
      if (this.waiter) return reject(new Error('concurrent send'));
      this.waiter = {
        resolve: line => { try { resolve(JSON.parse(line)); } catch { reject(new LedgerError('invalid response from ledger', 'unavailable')); } },
        reject,
      };
      this.sock.write(JSON.stringify(frame) + '\n');
      this._drain();
    });
  }
  close() { this.sock.destroy(); }
}

async function ledgerLogin(username, password) {
  const c = await openLedger();
  try {
    const r = await c.send({ auth: { username, password } });
    if (!r.ok || !r.token) throw new LedgerError(r.error || 'authentication failed', 'auth');
    return { token: r.token, role: r.role || 'unknown' };
  } finally { c.close(); }
}

// Runs fixed, pre-validated SQL statements on one connection with a session token.
async function withLedger(session, fn, timeoutMs) {
  const c = await openLedger(timeoutMs);
  const q = async (sql, withProof = false) => {
    const r = await c.send({ token: session.ledgerToken, sql, with_proof: withProof });
    if (!r.ok) {
      const kind = /token|authentication|session|expired/i.test(r.error || '') ? 'auth' : 'ledger';
      throw new LedgerError(r.error || 'ledger error', kind);
    }
    const cols = r.columns || [];
    return {
      message: r.message || '',
      proof: r.proof || null,
      rows: (r.rows || []).map(row => Object.fromEntries(cols.map((k, i) => [k, row[i]]))),
    };
  };
  try { return await fn(q); } finally { c.close(); }
}

// ── input validation ─────────────────────────────────────────────────────────
const SEQ_RE = /^[1-9][0-9]{0,14}$/;          // fits in a JS safe integer
const ACCOUNT_RE = /^[A-Za-z0-9_\-.:]{1,128}$/; // same rule the SDKs use
const seqParam = s => (SEQ_RE.test(s) ? String(s) : null);

// ── derived investigation facts ──────────────────────────────────────────────
const isFail = r => /^INTEGRITY FAILURE/i.test(r.message);

async function investigate(q, seq) {
  const n = Number(seq);
  const out = { sequence: n, notes: [], fields: {} };

  const entryRes = await q(`SELECT * FROM ledger WHERE sequence = ${seq}`, true);
  const entry = entryRes.rows[0];
  if (!entry) return { found: false, sequence: n };
  out.found = true;
  out.entry = {
    id: entry.id, status: entry.status, description: entry.description, domain: entry.domain,
    effective_at: entry.effective_at, posted_at: entry.posted_at, external_ref: entry.external_ref,
    content_hash: entry.content_hash, chain_hash: entry.chain_hash, metadata: entry.metadata,
  };

  // 1. content_hash / chain_hash recomputed by the ledger from the stored entry
  const ve = await q(`SELECT VERIFY_ENTRY(${seq})`);
  out.fields.hash_match = ve.rows[0]
    ? { value: ve.rows[0].status === 'VALID' ? 'yes' : 'no', source: 'ledger', detail: ve.message }
    : { value: null, source: 'ledger', detail: ve.message || 'no result' };

  // 2. link to the previous entry
  if (n === 1) {
    out.fields.prior_hash_match = { value: 'yes', source: 'ledger', detail: 'first entry; no predecessor' };
  } else {
    try {
      const vc = await q(`SELECT VERIFY_CHAIN(${n - 1}, ${n})`);
      out.fields.prior_hash_match = { value: vc.rows[0] && vc.rows[0].status === 'OK' && !isFail(vc) ? 'yes' : 'no', source: 'ledger', detail: vc.message };
    } catch (e) {
      if (e.kind === 'auth') throw e;
      out.fields.prior_hash_match = { value: null, source: 'ledger', detail: e.message };
    }
  }

  // 3. Merkle proof as reported by the ledger. It is built and checked by the
  //    server itself, so it shows internal consistency, not independent proof.
  out.fields.merkle = entryRes.proof
    ? { value: entryRes.proof.verified ? 'yes' : 'no', source: 'ledger (server-computed)', detail: `root ${entryRes.proof.root_hex}` }
    : { value: null, source: 'ledger', detail: 'ledger returned no proof; start it with --with-proofs' };

  // 4. timestamp order vs the previous entry
  if (n === 1) {
    out.fields.timestamp = { value: 'yes', source: 'ledger', detail: 'first entry' };
  } else {
    const prev = (await q(`SELECT * FROM ledger WHERE sequence = ${n - 1}`)).rows[0];
    const a = prev && Date.parse(prev.posted_at), b = Date.parse(entry.posted_at);
    out.fields.timestamp = prev && !Number.isNaN(a) && !Number.isNaN(b)
      ? { value: b >= a ? 'yes' : 'no', source: 'ledger', detail: `posted_at ${entry.posted_at} vs previous ${prev.posted_at}` }
      : { value: null, source: 'ledger', detail: 'previous entry not available' };
  }

  // 5. amount (sum of debit lines, minor units) and currency
  const lines = (await q(`SELECT * FROM ledger_lines WHERE sequence = ${seq}`)).rows;
  const debits = lines.filter(l => l.dr_cr === 'Debit');
  const currencies = [...new Set(lines.map(l => l.currency))];
  out.lines = lines.map(l => ({ account_id: l.account_id, dr_cr: l.dr_cr, amount: l.amount, currency: l.currency }));
  if (currencies.length !== 1) {
    out.fields.amount = { value: null, source: 'ledger', detail: `entry has ${currencies.length} currencies; amount not derived` };
  } else {
    let total = 0n;
    for (const l of debits) total += BigInt(l.amount);
    out.fields.amount = { value: total.toString(), source: 'ledger', detail: `sum of ${debits.length} debit line(s), minor units, ${currencies[0]}` };
    out.currency = currencies[0];
  }
  if (out.currency && out.currency !== 'USD') out.notes.push(`Currency is ${out.currency}; the rules page assumes USD and a USD threshold.`);

  // 6. reversal / four-eyes from the entry status
  const reversal = entry.status === 'Reversal' || entry.status === 'Reversed';
  out.fields.reversal = { value: reversal ? 'yes' : 'no', source: 'ledger', detail: `status ${entry.status}` };
  // The ledger is append-only: corrections can only be new entries. A silent edit
  // would surface as a hash mismatch above instead.
  out.fields.correction = { value: reversal ? 'new_entry' : '', source: 'ledger', detail: 'append-only ledger' };
  out.fields.foureyes_required = {
    value: entry.status === 'PendingApproval' ? 'yes' : 'no', source: 'ledger (partial)',
    detail: 'derived from entry status only; the per-account require_four_eyes flag is not exposed over SQL',
  };

  // 7. identifiers
  out.fields.txn_id = { value: entry.external_ref || entry.id, source: 'ledger', detail: entry.external_ref ? 'external_ref' : 'entry id' };
  out.fields.domain = { value: entry.domain, source: 'ledger', detail: '' };

  // 8. counterparty comes from entry metadata and an operator-supplied allow-list
  let cp = null;
  try {
    const m = entry.metadata ? JSON.parse(entry.metadata) : null;
    if (m && typeof m === 'object') cp = m.counterparty_id ?? m.counterparty ?? null;
  } catch { /* metadata is free-form */ }
  out.fields.counterparty = { value: typeof cp === 'string' ? cp : null, source: 'ledger metadata', detail: cp ? '' : 'no counterparty in metadata; enter it manually' };
  if (typeof cp === 'string' && allowlist) {
    out.fields.counterparty_approved = { value: allowlist.has(cp) ? 'yes' : 'no', source: 'gateway allow-list', detail: 'COUNTERPARTY_ALLOWLIST file' };
  } else if (typeof cp !== 'string') {
    // The ledger carries no counterparty for this entry, so rule R6 cannot be evaluated.
    out.fields.counterparty_approved = { value: 'na', source: 'gateway', detail: 'entry has no counterparty in metadata; R6 is not evaluated' };
  } else {
    out.fields.counterparty_approved = { value: null, source: 'analyst', detail: 'counterparty present but no allow-list configured; answer manually' };
  }
  return out;
}

// ── sessions ─────────────────────────────────────────────────────────────────
const sessions = new Map();
setInterval(() => {
  const now = Date.now();
  for (const [id, s] of sessions) if (now - s.lastUsed > SESSION_IDLE_MS || now - s.created > SESSION_MAX_MS) sessions.delete(id);
}, 60_000).unref();

function getSession(req) {
  const m = /(?:^|;\s*)vl_sid=([a-f0-9]{64})/.exec(req.headers.cookie || '');
  if (!m) return null;
  const s = sessions.get(m[1]);
  if (!s) return null;
  const now = Date.now();
  if (now - s.lastUsed > SESSION_IDLE_MS || now - s.created > SESSION_MAX_MS) { sessions.delete(m[1]); return null; }
  s.lastUsed = now; s.id = m[1];
  return s;
}

const loginFails = new Map(); // ip -> [timestamps]
function loginThrottled(ip) {
  const now = Date.now();
  const arr = (loginFails.get(ip) || []).filter(t => now - t < 5 * 60_000);
  loginFails.set(ip, arr);
  return arr.length >= 5;
}

// ── http helpers ─────────────────────────────────────────────────────────────
const SEC_HEADERS = {
  'X-Content-Type-Options': 'nosniff',
  'Referrer-Policy': 'no-referrer',
  'Cache-Control': 'no-store',
  'X-Frame-Options': 'DENY',
  'Content-Security-Policy': [
    "default-src 'none'",
    "script-src 'unsafe-inline'", // the page uses inline handlers; all ledger data is escaped before insertion
    "style-src 'unsafe-inline' https://fonts.googleapis.com",
    'font-src https://fonts.gstatic.com',
    "connect-src 'self'",
    "img-src 'self' data:",
    "base-uri 'none'", "form-action 'none'", "frame-ancestors 'none'", "object-src 'none'",
  ].join('; '),
};

function send(res, status, body, headers = {}) {
  const isObj = typeof body === 'object' && !Buffer.isBuffer(body);
  const data = isObj ? JSON.stringify(body) : body;
  res.writeHead(status, { ...SEC_HEADERS, 'Content-Type': isObj ? 'application/json; charset=utf-8' : 'text/html; charset=utf-8', ...headers });
  res.end(data);
}

function readJson(req) {
  return new Promise((resolve, reject) => {
    let size = 0; const chunks = [];
    req.on('data', c => { size += c.length; if (size > MAX_BODY_BYTES) { reject(Object.assign(new Error('body too large'), { status: 413 })); req.destroy(); } else chunks.push(c); });
    req.on('end', () => { try { resolve(JSON.parse(Buffer.concat(chunks).toString('utf8') || '{}')); } catch { reject(Object.assign(new Error('invalid JSON'), { status: 400 })); } });
    req.on('error', reject);
  });
}

// CSRF defence for state-changing requests: custom header + same-origin Origin.
function sameOriginPost(req) {
  if (req.headers['x-requested-with'] !== 'vl-ui') return false;
  const origin = req.headers.origin;
  if (!origin) return false;
  try { return new URL(origin).host === req.headers.host; } catch { return false; }
}

function mapError(res, e) {
  if (e.kind === 'auth') return send(res, 401, { ok: false, error: e.message });
  if (e.kind === 'unavailable') return send(res, 502, { ok: false, error: e.message });
  return send(res, 400, { ok: false, error: e.message });
}

// ── routes ───────────────────────────────────────────────────────────────────
async function handle(req, res) {
  const url = new URL(req.url, 'http://x');
  const p = url.pathname;

  if (req.method === 'GET' && (p === '/' || p === '/index.html')) {
    try { return send(res, 200, fs.readFileSync(INDEX_PATH)); } catch { return send(res, 500, { ok: false, error: 'UI file not found' }); }
  }
  if (!p.startsWith('/api/')) return send(res, 404, { ok: false, error: 'not found' });

  if (req.method === 'GET' && p === '/api/health') {
    return send(res, 200, { ok: true, service: 'vledger-gateway', ledger: `${LEDGER_HOST}:${LEDGER_PORT}`, counterparty_allowlist: !!allowlist });
  }

  if (req.method === 'POST' && p === '/api/login') {
    if (!sameOriginPost(req)) return send(res, 403, { ok: false, error: 'cross-origin request refused' });
    const ip = req.socket.remoteAddress || 'unknown';
    if (loginThrottled(ip)) return send(res, 429, { ok: false, error: 'too many failed logins; wait a few minutes' });
    let body;
    try { body = await readJson(req); } catch (e) { return send(res, e.status || 400, { ok: false, error: e.message }); }
    const { username, password } = body;
    if (typeof username !== 'string' || typeof password !== 'string' || !username || !password || username.length > 128 || password.length > 1024) {
      return send(res, 400, { ok: false, error: 'username and password required' });
    }
    try {
      const { token, role } = await ledgerLogin(username, password);
      const id = crypto.randomBytes(32).toString('hex');
      const now = Date.now();
      sessions.set(id, { username, role, ledgerToken: token, created: now, lastUsed: now });
      return send(res, 200, { ok: true, username, role },
        { 'Set-Cookie': `vl_sid=${id}; HttpOnly; SameSite=Strict; Path=/; Max-Age=${SESSION_MAX_MS / 1000}${SECURE_COOKIE ? '; Secure' : ''}` });
    } catch (e) {
      if (e.kind === 'auth') { const a = loginFails.get(ip) || []; a.push(Date.now()); loginFails.set(ip, a); }
      return mapError(res, e);
    }
  }

  const s = getSession(req);
  if (req.method === 'POST' && p === '/api/logout') {
    if (!sameOriginPost(req)) return send(res, 403, { ok: false, error: 'cross-origin request refused' });
    if (s) sessions.delete(s.id);
    return send(res, 200, { ok: true }, { 'Set-Cookie': `vl_sid=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0${SECURE_COOKIE ? '; Secure' : ''}` });
  }
  if (req.method === 'GET' && p === '/api/me') {
    return s ? send(res, 200, { ok: true, username: s.username, role: s.role }) : send(res, 401, { ok: false, error: 'not logged in' });
  }
  if (!s) return send(res, 401, { ok: false, error: 'not logged in' });
  if (req.method !== 'GET') return send(res, 405, { ok: false, error: 'method not allowed' });

  try {
    let m;
    if ((m = /^\/api\/entries\/([^/]+)\/investigation$/.exec(p))) {
      const seq = seqParam(m[1]);
      if (!seq) return send(res, 400, { ok: false, error: 'sequence must be a positive whole number' });
      return send(res, 200, { ok: true, ...(await withLedger(s, q => investigate(q, seq))) });
    }
    if (p === '/api/verify-chain') {
      const r = await withLedger(s, q => q('SELECT VERIFY_CHAIN()'), VERIFY_CHAIN_TIMEOUT_MS);
      return send(res, 200, { ok: true, verified: !!r.rows[0] && r.rows[0].status === 'OK' && !isFail(r), message: r.message, result: r.rows[0] || null });
    }
    if (p === '/api/merkle') {
      const from = seqParam(url.searchParams.get('from') || ''), to = seqParam(url.searchParams.get('to') || '');
      if (!from || !to || Number(from) > Number(to)) return send(res, 400, { ok: false, error: 'from and to must be positive whole numbers, from <= to' });
      if (Number(to) - Number(from) + 1 > MERKLE_MAX_RANGE) {
        return send(res, 400, { ok: false, error: `range too large: at most ${MERKLE_MAX_RANGE} entries per request (MERKLE_MAX_RANGE); use vledger audit-package for whole-ledger commitments` });
      }
      const r = await withLedger(s, q => q(`SELECT MERKLE_ROOT(${from}, ${to})`), VERIFY_CHAIN_TIMEOUT_MS);
      return send(res, 200, { ok: true, message: r.message, result: r.rows[0] || null });
    }
    if ((m = /^\/api\/balance\/([^/]+)$/.exec(p))) {
      let code; try { code = decodeURIComponent(m[1]); } catch { code = ''; }
      if (!ACCOUNT_RE.test(code)) return send(res, 400, { ok: false, error: 'invalid account code' });
      const r = await withLedger(s, q => q(`SELECT BALANCE('${code}')`));
      return send(res, 200, { ok: true, message: r.message, result: r.rows[0] || null });
    }
    return send(res, 404, { ok: false, error: 'not found' });
  } catch (e) {
    if (e.kind === 'auth') sessions.delete(s.id);
    return mapError(res, e);
  }
}

const server = (httpsOpts ? https.createServer(httpsOpts, wrap) : http.createServer(wrap));
function wrap(req, res) {
  handle(req, res).catch(e => { console.error('gateway: unhandled', e.message); if (!res.headersSent) send(res, 500, { ok: false, error: 'internal error' }); });
}
server.on('error', e => fail(e.code === 'EADDRINUSE' ? `${BIND_HOST}:${BIND_PORT} is already in use; stop the other process or set GATEWAY_BIND` : e.message));
server.listen(BIND_PORT, BIND_HOST, () => {
  console.log(`gateway: ${httpsOpts ? 'https' : 'http'}://${BIND_HOST}:${BIND_PORT}  ->  ledger ${LEDGER_HOST}:${LEDGER_PORT}`);
});
