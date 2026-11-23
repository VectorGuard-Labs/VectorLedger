# Decommissioning VectorLedger

How to retire a VectorLedger deployment safely: keep what you are required to keep, prove it is intact, then remove the software, secrets, and data.

This guide applies to every tier. It also covers the case where your paid license has already lapsed.

> **Read this first.** Two steps in this guide cannot be undone: deleting the data directory (step 11) and destroying the master key. Do not start step 11 until the checklist at the end of step 10 is complete and a different person has signed it off.

## Contents

1. [What a lapsed license changes](#1-what-a-lapsed-license-changes)
2. [Before you start](#2-before-you-start)
3. [Plan, approvals, and legal holds](#3-plan-approvals-and-legal-holds)
4. [Stop writers, then the server](#4-stop-writers-then-the-server)
5. [Verify the ledger](#5-verify-the-ledger)
6. [Export the data](#6-export-the-data)
7. [Produce audit and compliance evidence](#7-produce-audit-and-compliance-evidence)
8. [Take a verified backup](#8-take-a-verified-backup)
9. [Prove the archive restores](#9-prove-the-archive-restores)
10. [Archive, and the go/no-go checklist](#10-archive-and-the-gono-go-checklist)
11. [Remove software, secrets, and data](#11-remove-software-secrets-and-data)
12. [Record the decommission](#12-record-the-decommission)
13. [Known limitations](#13-known-limitations)

---

## 1. What a lapsed license changes

Your data is never locked behind a license. When a license expires or is cancelled, the server falls back to the **Free** tier at the next UTC midnight, without a restart. The ledger, its hash chain, and all of your data stay readable and writable.

Only paid *features* turn off. For decommissioning, this is what matters:

| Command | Free | Starter | Growth / Enterprise |
|---|---|---|---|
| `vledger export` (full data export) | yes | yes | yes |
| `vledger backup`, `backup-verify`, `restore` | yes | yes | yes |
| `vledger verify`, `reconcile`, `hold list`, `status` | yes | yes | yes |
| `vledger audit-package`, `audit-proof`, `verify-audit-package` | yes | yes | yes |
| `vledger audit-export` | last 30 days | last 90 days | unlimited |
| `vledger compliance-report` | no | no | yes |
| pgwire listener, MCP server | no | yes | yes |
| replication (`start-primary`, `start-replica`) | no | no | yes |
| HSM key sources at `init`, `rotate-keys` | no | no | Enterprise only |

The `export`, `backup`/`restore`, `verify`/`reconcile`/`hold`/`status`, and `audit-package`/`audit-proof`/`verify-audit-package` rows were run against a Free-tier data directory while this guide was written. The Free-tier `compliance-report` refusal and the `audit-export` 30-day window were also observed. The Starter and Growth/Enterprise cells come from reading the license code and were not tested.

The HSM license check applies only to `init` and `rotate-keys`. The offline commands in this guide read the master key through `keys/key_source.json` and do not check the license, so a lapsed Enterprise customer is not locked out of an HSM-backed ledger. That was confirmed from the source code, not against a real HSM.

**Consequences for you:**

- If your license is still active, run steps 6 to 8 **before** it lapses. In particular, `audit-export` older than 30 days and `compliance-report` are unavailable on Free.
- If your license has already lapsed, you can still do everything except unlimited `audit-export` and `compliance-report`. For the audit log, export it in 30-day windows with `--from` and `--to`, starting as soon as possible, because the 30-day window slides forward each day. Alternatively, the audit log file `audit/audit.log` is included in the backup in step 8, and `audit-package` still works.
- Keep `license.json` in the data directory until every export is finished. Delete it in step 11.

## 2. Before you start

You need:

- Shell access as a user that can read the data directory (usually `/var/lib/vledger/data` or `/opt/vledger/data`; this guide uses `$DATA`).
- Access to the master key source named in `$DATA/keys/key_source.json` (PyHSM, Vault, AWS KMS, environment variable, or file). Every offline command below needs it.
- Enough free disk for roughly 3x the data directory size (export + backup + restore test).
- A destination for the archive that supports immutability (WORM), see step 10.

```bash
export DATA=/var/lib/vledger/data          # adjust to your deployment
export OUT=/secure/decommission-$(date +%Y%m%d)
mkdir -p "$OUT" && chmod 700 "$OUT"
cat "$DATA/keys/key_source.json"           # note which backend holds the master key
vledger license --data-dir "$DATA"         # note the tier and expiry date
```

Every command below takes `--data-dir "$DATA"` (also `-d`). Offline commands open the data directory directly and take an exclusive lock, so **the server must be stopped** (step 4).

## 3. Plan, approvals, and legal holds

1. Get written sign-off from compliance, legal, and finance. Confirm the retention period that applies to your ledger. Retention rules for financial records commonly run for years, and the archive from steps 6 to 10 has to be kept for that whole period.
2. List legal holds. Do not decommission while a hold is active unless legal confirms the archive satisfies it.

   ```bash
   vledger hold list --data-dir "$DATA"
   vledger retention show --data-dir "$DATA"
   ```

3. Resolve or reject every pending four-eyes approval. The `foureyes/` queue is **not** included in backups or in `vledger export`.

   ```bash
   ls -l "$DATA/foureyes/"                 # should be empty or contain only completed items
   ```

4. Decide who owns the successor system and when it goes live. Plan a final cutover time after which no new entries are posted.

## 4. Stop writers, then the server

Stop everything that can write, in this order. Do not use `kill -9`: it can lose transactions that are not yet fsynced.

```bash
# 1. Upstream writers: bank connectors, ingest jobs, MCP/AI agents, application clients
sudo systemctl stop vledger-ingest vledger-mcp

# 2. Replication (Growth/Enterprise): stop the replica first, then the primary
#    (services started with `vledger start-replica` / `vledger start-primary`)

# 3. The server
sudo systemctl stop vledger
systemctl is-active vledger                # expect: inactive
```

If you do not use systemd, send `SIGTERM` to the process and wait for it to exit:

```bash
kill -TERM "$(cat /var/run/vledger.pid)"
timeout 60 tail --pid="$(cat /var/run/vledger.pid)" -f /dev/null
```

Disable the units so nothing restarts them while you work:

```bash
sudo systemctl disable vledger vledger-mcp vledger-ingest
```

## 5. Verify the ledger

Do not archive a ledger without knowing whether it is intact.

```bash
vledger verify    --data-dir "$DATA"        # WAL and hash-chain check
vledger reconcile --data-dir "$DATA" --format json --output "$OUT/reconcile.json"
```

- `verify` must end with `✓ Verification complete`. Write down the chain tip it prints.
- `reconcile` exits non-zero if any account balance differs from the journal. Investigate any discrepancy before continuing.
- `reconcile` appends a run record to the WAL, so run it **before** the backup so the backup contains it.

If either command reports a problem, stop. Open a support case with VectorGuard Labs (security@vectorguardlabs.com for suspected tampering) before you change anything.

## 6. Export the data

`vledger export` writes everything needed to rebuild the ledger in another system. It works on every tier and does not need a running server.

```bash
vledger export --data-dir "$DATA" --output "$OUT/export"
```

It verifies the hash chain first and refuses to write anything if the chain is broken (use `--allow-unverified` only on instruction from support; the manifest then records `chain_verified: false`). The output directory must not exist or must be empty. Files are created with mode `0600` in a `0700` directory.

| File | Contents |
|---|---|
| `entries.jsonl` | One complete journal entry per line, including lines, hashes (hex), metadata. This is the canonical archive. |
| `journal_lines.csv` | One row per journal line, ready to load into a database or spreadsheet. |
| `accounts.csv` | Chart of accounts, including the legal-hold flag. |
| `balances.csv` | Balance per account as cached by VectorLedger and as recomputed from the exported lines, plus a `matches` column. |
| `manifest.json` | Counts, first and last sequence, chain tip, whether the chain verified, license tier, and a BLAKE3 digest for every file. |

Notes:

- All amounts are integers in minor units (cents for USD). Balances follow normal-balance sign: Asset and Expense accounts are debit-normal; the other types are credit-normal.
- The command exits non-zero if any balance in `balances.csv` does not match the journal.
- CSV files contain text exactly as stored. If you open them in a spreadsheet, treat any cell that starts with `=`, `+`, `-`, or `@` as data, not a formula. Prefer `entries.jsonl` for anything automated.
- The export writes a `LedgerExported` event to the audit log.

**Reconcile against the successor system.** After loading the data into its new home, compare entry count, line count, and every account balance with `manifest.json` and `balances.csv`. Do not sign off until they match.

## 7. Produce audit and compliance evidence

```bash
# Signed Merkle commitment over the whole ledger (fast at any size)
vledger audit-package --data-dir "$DATA" \
  --output "$OUT/final-commitment.json" \
  --tenant "<your organisation>" \
  --description "Decommission $(date +%Y-%m-%d)"
vledger verify-audit-package --file "$OUT/final-commitment.json"

# WORM audit log (JSON and CSV). Free tier: last 30 days only.
vledger audit-export --data-dir "$DATA" --format json --output "$OUT/audit-log.json"
vledger audit-export --data-dir "$DATA" --format csv  --output "$OUT/audit-log.csv"

# Compliance evidence (requires the compliance_report feature)
vledger compliance-report --data-dir "$DATA" --standard soc2    --format markdown --output "$OUT/soc2.md"
vledger compliance-report --data-dir "$DATA" --standard pci-dss --format markdown --output "$OUT/pci-dss.md"
```

The commitment package lets an auditor later prove that any single entry belonged to this ledger without access to the database:

```bash
vledger audit-proof --data-dir "$DATA" --commitment "$OUT/final-commitment.json" --sequence <N>
vledger verify-audit-package --file vledger-entry-proof-<N>.json
```

`audit-export` prints a warning and narrows the window when your tier does not allow the range you asked for. Read the output; a successful exit does not mean the full range was exported.

## 8. Take a verified backup

The backup is an encrypted archive that can restore the ledger in VectorLedger itself. Keep it in addition to the export, not instead of it.

```bash
vledger backup --data-dir "$DATA" --output "$OUT/final.tar"
vledger backup-verify --data-dir "$DATA" --from "$OUT/final.tar"
```

`backup-verify` must report `Manifest: OK`, `Content: N OK, 0 FAILED`, and `Result: PASS`.

Two things travel with the archive:

- `final.tar.key` is the backup key, wrapped by your master key. **Without it, and without the master key, the archive cannot be restored.** Store them together.
- The backup contains `wal/`, `pages/`, `catalog/` (users, rules, retention), `audit/`, and the public signing key. It does **not** contain private signing keys, `foureyes/`, TLS certificates, or `license.json`. With an HSM, Vault, or KMS backend the keys already live outside the data directory. With the development-only `file` key source, copy `keys/` separately and store it as securely as the archive.

## 9. Prove the archive restores

An archive that has never been restored is not an archive. Do this on an isolated machine, or in a scratch directory on this host, using the key source configured for this deployment.

```bash
vledger restore --data-dir "$DATA" --from "$OUT/final.tar" --target /tmp/restore-test
vledger verify  --data-dir /tmp/restore-test
vledger export  --data-dir /tmp/restore-test --output /tmp/restore-export
```

`restore` loads the master key through the key source in `--data-dir`, then writes into `--target`. Then compare the restored export with the original:

```bash
for f in accounts.csv balances.csv entries.jsonl journal_lines.csv; do
  cmp "$OUT/export/$f" /tmp/restore-export/$f && echo "$f identical"
done
```

All four files must be identical, and the chain tip printed by `export` must equal the one from step 5. Then securely remove `/tmp/restore-test` and `/tmp/restore-export`.

## 10. Archive, and the go/no-go checklist

Copy the contents of `$OUT` to immutable storage (WORM bucket with object lock, write-once media, or your records-management system). Store the **key material** (or your access to the HSM/Vault/KMS key) under separate custody from the data, with a named owner and a retention date.

Compute and store a checksum of everything you archived:

```bash
(cd "$OUT" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 shasum -a 256 > SHA256SUMS)   # sha256sum on Linux if shasum is absent
```

**Go/no-go checklist. Every box must be ticked, and signed off by someone other than the person who did the work.**

- [ ] Retention period confirmed in writing; archive retention date recorded
- [ ] No active legal holds, or legal has approved proceeding
- [ ] No pending four-eyes items
- [ ] `verify` and `reconcile` clean; chain tip recorded
- [ ] Export complete; `manifest.json` stored; balances match `balances.csv`
- [ ] Successor system loaded and reconciled (entry count, line count, every balance)
- [ ] Commitment package verified; audit log and compliance reports exported
- [ ] Backup verified with `backup-verify`; `.key` sidecar stored with it
- [ ] Restore drill passed and restored export is identical to the original
- [ ] Archive copied to immutable storage; `SHA256SUMS` stored; copy re-read and re-checked
- [ ] Master key custody assigned; key will **not** be destroyed before the retention period ends

## 11. Remove software, secrets, and data

Everything above this point is reversible. From here on it is not.

### 11.1 Remove the software

VectorLedger ships as a single binary; the MCP server and ingest tool are subcommands of it.

```bash
sudo rm -f /usr/local/bin/vledger "$HOME/.local/bin/vledger" /opt/vledger/bin/vledger
sudo rm -f /etc/systemd/system/vledger.service \
           /etc/systemd/system/vledger-mcp.service \
           /etc/systemd/system/vledger-ingest.service
sudo systemctl daemon-reload
sudo rm -f /var/run/vledger.pid
crontab -l | grep -i vledger            # remove any backup/verify/ingest jobs
```

Also remove:

- MCP client configuration (for example `.mcp.json`, `.kiro/settings/mcp.json`, or the equivalents for other agent tools) on developer and agent machines.
- The service account (`vledger`) and its home or log directories, such as `/opt/vledger/logs`.
- Firewall and security-group rules for the native listener (default `127.0.0.1:5433`), pgwire (`5432`), the MCP server (default `127.0.0.1:3000`), metrics (default `127.0.0.1:9090`), and the replication port set in `replication.json`.
- Monitoring and alerting targets that scrape the metrics endpoint.

### 11.2 Revoke access and secrets

- Vault: revoke the token and policy used by VectorLedger; delete the secret path **only** after the retention decision in step 10.
- AWS: remove IAM roles and KMS grants VectorLedger used. Do **not** delete the KMS key yet (see 11.4).
- PyHSM: remove the VectorLedger caller ID and mTLS client certificate; stop the socket or endpoint if nothing else uses it.
- Banno / Jack Henry: revoke the OAuth client and delete `BANNO_*` values from every environment file (for example `/opt/vledger/auth/token.env`).
- LLM keys for the AI features (`OPENAI_API_KEY`, `XAI_API_KEY`, and similar): revoke them at the provider.
- TLS certificates and private keys issued for the server (a self-signed pair is generated into `keys/` at `vledger init`; CA-signed certificates live wherever you placed them).
- Cancel the subscription with VectorGuard Labs (pavon@vectorguardlabs.com) so billing and renewal emails stop.

### 11.3 Delete the live data

Only after the step 10 checklist is signed off:

```bash
sudo rm -rf "$DATA"                      # wal/ pages/ indexes/ catalog/ audit/ keys/ foureyes/ vledger.db license.json
```

Also delete plaintext remnants: old backups and their `.key` sidecars that are not part of the archive, `keys/master_key.hex` if the `file` key source was used, temporary files, replica copies on other hosts, and any scratch copies from the restore drill.

### 11.4 The master key

The master key decrypts every page and WAL record. Destroying it makes any leftover ciphertext (old disk snapshots, cloud volume snapshots, offsite copies) permanently unreadable. It also makes **your own archive unreadable**, so it is the last thing you do.

- If the archive must remain restorable in VectorLedger: keep the key for the full retention period. The plaintext export from step 6 does not depend on it, but treat that export as the more sensitive copy and protect it accordingly.
- When the retention period ends, or if you decide the plaintext export alone is the archive, destroy the key:
  - Vault: `vault kv metadata delete <path>` (removes all versions).
  - AWS KMS: `aws kms schedule-key-deletion --key-id <id> --pending-window-in-days 30` (7 days is the minimum; there is a waiting period by design).
  - PyHSM / hardware HSM: use your HSM administrator's key-destruction procedure and keep the log.
  - `file` backend: `keys/master_key.hex` is deleted with the data directory; note that deleting a file does not reliably erase it from SSDs or snapshots.

Snapshots and backups outside VectorLedger (cloud volume snapshots, filesystem snapshots, offsite tapes) are not touched by any command here. Delete them through your cloud or backup tooling.

## 12. Record the decommission

Keep this record with the archive. Auditors will ask for it.

| Item | Value |
|---|---|
| Deployment / environment | |
| Date and time of final write | |
| Ledger entry count, first and last sequence | (from `manifest.json`) |
| **Chain tip (hex)** | (from `vledger verify` / `manifest.json`) |
| Commitment package file and its SHA-256 | |
| Archive location, storage class, retention end date | |
| Master key location and custodian | |
| Successor system and reconciliation result | |
| Software removed on (hosts) | |
| Secrets revoked (list) | |
| Data directory deleted on / by | |
| Master key destruction date (or "retained until ...") | |
| Approved by (compliance / legal / finance) | |
| Executed by / independently verified by | |

## 13. Known limitations

- `vledger export` and the other ledger commands are offline: they require the server to be stopped.
- The backup does not include `foureyes/`, `license.json`, TLS certificates, or private signing keys (see step 8).
- Restoring a backup requires access to the original master key source. A restore on a new host needs that source reachable from the new host.
- The compliance-report and unlimited audit-export features are unavailable once a license has lapsed.
- The paid-tier commands (`compliance-report`, `start-primary`, `start-replica`) and HSM, Vault, and KMS key sources were not exercised when this guide was written. All commands were tested end to end only with the `file` key source on a Free-tier data directory.
- This guide has not been reviewed by an external auditor. Have your own compliance team confirm the retention and evidence requirements that apply to you.
