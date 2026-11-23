#!/usr/bin/env node
// Create a ledger user over the native protocol, on any port.
// `vledger user create` always probes 127.0.0.1:5433, so it cannot be used when
// the ledger runs on another port or another service owns 5433. Secrets are read
// from the environment so they stay out of shell history and `ps`.
//
//   VLEDGER_PORT=5443 VLEDGER_CA_CERT=<data-dir>/catalog/tls_cert.pem \
//   ADMIN_PASSWORD=... NEW_USER=analyst1 NEW_PASSWORD=... [NEW_ROLE=auditor] \
//   node gateway/create-user.mjs
import tls from 'node:tls';
import fs from 'node:fs';

const e = process.env;
const host = e.VLEDGER_HOST || '127.0.0.1', port = Number(e.VLEDGER_PORT || 5433);
const need = k => { if (!e[k]) { console.error(`missing ${k}`); process.exit(2); } return e[k]; };
const adminUser = e.ADMIN_USER || 'admin', adminPw = need('ADMIN_PASSWORD');
const user = need('NEW_USER'), pw = need('NEW_PASSWORD'), role = e.NEW_ROLE || 'auditor';
if (!e.VLEDGER_CA_CERT) { console.error('missing VLEDGER_CA_CERT (path to <data-dir>/catalog/tls_cert.pem)'); process.exit(2); }

const sock = tls.connect({ host, port, ca: fs.readFileSync(e.VLEDGER_CA_CERT), minVersion: 'TLSv1.3',
  servername: e.VLEDGER_TLS_SERVERNAME || (/^[\d.:]+$/.test(host) ? 'localhost' : undefined) });
sock.setEncoding('utf8'); sock.setTimeout(15000, () => sock.destroy(new Error('timeout')));
const frames = [{ auth: { username: adminUser, password: adminPw } }];
let buf = '', step = 0, token;
sock.on('secureConnect', () => sock.write(JSON.stringify(frames[0]) + '\n'));
sock.on('data', d => {
  buf += d; let i;
  while ((i = buf.indexOf('\n')) >= 0) {
    const r = JSON.parse(buf.slice(0, i)); buf = buf.slice(i + 1);
    if (!r.ok) { console.error('failed:', r.error); process.exit(1); }
    if (step++ === 0) { token = r.token; sock.write(JSON.stringify({ token, admin: { op: 'create_user', username: user, password: pw, role } }) + '\n'); }
    else { console.log(r.message); sock.destroy(); }
  }
});
sock.on('error', err => { console.error('error:', err.message); process.exit(1); });
