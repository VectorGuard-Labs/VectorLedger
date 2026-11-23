# VectorLedger web gateway

Lets the browser UI (`../index.html`) read a running ledger. Browsers cannot open the raw TLS socket on port 5433, so this process holds that connection and exposes a small read-only HTTP API plus the UI on the same origin. No dependencies; Node 18 or newer.

## Run

```bash
# 1. start the ledger as usual
vledger start --data-dir ./vledger-data --bind 127.0.0.1:5433 --with-proofs

# 2. create a read-only analyst account (auditor can run VERIFY_* but cannot post)
vledger user create --username analyst1 --role auditor

# 3. start the gateway, pinning the ledger's certificate
VLEDGER_CA_CERT=./vledger-data/catalog/tls_cert.pem node gateway/server.mjs

# 4. open http://127.0.0.1:8088 and sign in with the ledger account
```

Opening `index.html` directly from disk still works, in demo mode only.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `VLEDGER_HOST` / `VLEDGER_PORT` | `127.0.0.1` / `5433` | Ledger native protocol |
| `VLEDGER_CA_CERT` | none (required for loopback) | PEM used to verify the ledger certificate |
| `VLEDGER_TLS_SERVERNAME` | `localhost` when a CA is pinned and the host is an IP | Name checked against the certificate (the generated cert is valid for `localhost`) |
| `VLEDGER_TLS_INSECURE=1` | off | Skip certificate checks. Loopback ledger only |
| `GATEWAY_BIND` | `127.0.0.1:8088` | Listen address |
| `GATEWAY_TLS_CERT` / `GATEWAY_TLS_KEY` | none | Required when `GATEWAY_BIND` is not loopback |
| `COUNTERPARTY_ALLOWLIST` | none | JSON array of approved counterparty IDs, used for rule R6 |
| `GATEWAY_INDEX_HTML` | `../index.html` | UI file to serve |
| `VERIFY_CHAIN_TIMEOUT_MS` | `1800000` (30 min) | How long to wait for a full `VERIFY_CHAIN()` or a Merkle request. Point lookups keep a 15 s limit |
| `MERKLE_MAX_RANGE` | `1000000` | Largest `/api/merkle` range. The ledger holds the whole range in memory (about 0.7 GB per million entries) |

## Large ledgers

Measured on this machine (release build, SSD): entry lookups and the per-entry checks take under 20 ms at 1M entries and do not grow with ledger size. A full `VERIFY_CHAIN()` takes about 1-1.4 s per million entries while the database is in the OS cache. It holds the ledger's read lock for its whole run, so new postings wait until it finishes. On a large production ledger, run the full check off-hours or use the server's scheduled check, and use per-entry investigation during the day.

## API (all JSON, session cookie required except health and login)

| Endpoint | Ledger statements issued |
|---|---|
| `POST /api/login` `{username,password}` | native `auth` frame |
| `POST /api/logout`, `GET /api/me`, `GET /api/health` | none |
| `GET /api/entries/:seq/investigation` | `SELECT * FROM ledger WHERE sequence = N` (with proof), `VERIFY_ENTRY(N)`, `VERIFY_CHAIN(N-1, N)`, previous entry, `ledger_lines` for N |
| `GET /api/verify-chain` | `SELECT VERIFY_CHAIN()` |
| `GET /api/merkle?from=&to=` | `SELECT MERKLE_ROOT(from, to)` |
| `GET /api/balance/:code` | `SELECT BALANCE('code')` |

## Security properties

- Users sign in with their own ledger account. The ledger's roles and WORM audit log apply per user. The gateway stores no passwords, only an in-memory session mapping a random cookie to the ledger token.
- Only the fixed statements above are sent. Sequence numbers must match `^[1-9][0-9]{0,14}$`; account codes must match `^[A-Za-z0-9_\-.:]{1,128}$`.
- Cookie is `HttpOnly; SameSite=Strict` (plus `Secure` over HTTPS). Login and logout also need `X-Requested-With: vl-ui` and a same-origin `Origin` header.
- 5 failed logins per client address in 5 minutes returns 429, on top of the ledger's own lockout. Sessions expire after 15 minutes idle or 55 minutes total.
- The gateway refuses to start on a non-loopback address without TLS, and never disables certificate checks for a non-loopback ledger.
- Ledger text (descriptions, metadata) is HTML-escaped before the page displays it.

## What the UI derives from the ledger, and what it does not

From the ledger: content/chain hash validity, link to the previous entry, timestamp order, amount (sum of debit lines, minor units), status, reversal flag, Merkle result.

Not from the ledger: the $50,000 threshold, the counterparty allow-list (only if `COUNTERPARTY_ALLOWLIST` is set and the entry metadata has `counterparty_id`), the per-account `require_four_eyes` flag (not exposed over SQL; the page uses entry status only), the draft wording, and the Approve/Reject buttons. The Merkle result is computed and checked by the ledger process itself, so treat it as an internal consistency check.
