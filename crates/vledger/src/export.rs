//! `vledger export` — full, tier-independent export of ledger data.
//!
//! Intended for decommissioning, migration to another system, and long-term
//! archival. Writes a directory containing:
//!
//! | File                | Contents                                                   |
//! |---------------------|------------------------------------------------------------|
//! | `accounts.csv`      | Chart of accounts, including legal-hold flag               |
//! | `balances.csv`      | Cached and recomputed balance per account                  |
//! | `entries.jsonl`     | One complete journal entry per line (canonical archive)    |
//! | `journal_lines.csv` | Flattened lines, convenient for loading into other systems |
//! | `manifest.json`     | Counts, chain tip, verification result, BLAKE3 per file    |
//!
//! The export is offline: it opens the data directory directly, so the
//! server must be stopped. Hash-chain integrity is verified first; a broken
//! chain aborts the export unless `--allow-unverified` is given, in which
//! case the manifest records `chain_verified: false`.
//!
//! The export is not gated by license tier, so a customer whose license has
//! lapsed can always retrieve their own ledger data.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use vledger_ledger::{AccountType, JournalEntry, LedgerError, LedgerStore};

/// Version of the on-disk export layout. Bump when files or columns change.
pub const EXPORT_FORMAT_VERSION: u32 = 1;

const FILE_ACCOUNTS: &str = "accounts.csv";
const FILE_BALANCES: &str = "balances.csv";
const FILE_ENTRIES: &str = "entries.jsonl";
const FILE_LINES: &str = "journal_lines.csv";
const FILE_MANIFEST: &str = "manifest.json";

/// Options for [`run`].
pub struct ExportOptions {
    pub output: PathBuf,
    pub allow_unverified: bool,
}

#[derive(Serialize)]
struct ExportLine<'a> {
    id: String,
    account_id: String,
    currency_code: &'a str,
    amount_minor_units: String,
    dr_cr: &'static str,
    memo: &'a Option<String>,
}

/// JSON shape for `entries.jsonl`. Hashes are hex so the file is
/// self-describing and does not depend on Rust's array serialization.
#[derive(Serialize)]
struct ExportEntry<'a> {
    sequence: u64,
    id: String,
    status: String,
    description: &'a str,
    domain: &'a str,
    effective_at: String,
    posted_at: String,
    external_ref: &'a Option<String>,
    idempotency_key: &'a Option<String>,
    reverses_entry_id: Option<String>,
    reversed_by_entry_id: Option<String>,
    approved_by: &'a Option<String>,
    metadata: &'a Option<String>,
    content_hash: String,
    prev_hash: String,
    chain_hash: String,
    lines: Vec<ExportLine<'a>>,
}

#[derive(Serialize)]
struct FileRecord {
    name: String,
    bytes: u64,
    blake3: String,
}

#[derive(Serialize)]
struct Manifest {
    export_format_version: u32,
    vledger_version: &'static str,
    exported_at: String,
    license_tier: String,
    account_count: u64,
    entry_count: u64,
    journal_line_count: u64,
    first_sequence: Option<u64>,
    last_sequence: Option<u64>,
    chain_tip_hex: String,
    chain_verified: bool,
    chain_verification_error: Option<String>,
    balance_mismatches: u64,
    amounts_note: &'static str,
    files: Vec<FileRecord>,
}

fn is_debit_normal(t: &AccountType) -> bool {
    matches!(t, AccountType::Asset | AccountType::Expense)
}

fn secure_create(path: &Path) -> Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
        .with_context(|| format!("cannot create {}", path.display()))
}

fn blake3_file(path: &Path) -> Result<(u64, String)> {
    let mut f = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((total, hasher.finalize().to_hex().to_string()))
}

/// Create the output directory. It must not exist, or must be empty, so an
/// export can never overwrite or mix with earlier files.
fn prepare_output_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        let mut it =
            std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))?;
        if it.next().is_some() {
            bail!(
                "output directory {} is not empty; choose a new or empty directory",
                dir.display()
            );
        }
    } else {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Export everything in `data_dir` into `opts.output`. Returns the manifest's
/// chain tip (hex) so callers can print it for the decommission record.
pub fn run(data_dir: &Path, opts: &ExportOptions) -> Result<String> {
    if !data_dir.exists() {
        bail!("Not initialised at: {}", data_dir.display());
    }

    let ledger = LedgerStore::open(data_dir).context(
        "Failed to open ledger. If the server is running, stop it first \
         (the server holds an exclusive lock on the data directory).",
    )?;

    // 1. Integrity first: never silently archive a broken chain.
    let (chain_verified, chain_err) = match ledger.verify_chain_integrity() {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    };
    if !chain_verified && !opts.allow_unverified {
        bail!(
            "hash chain verification FAILED: {}\n\
             Refusing to export. Investigate first, or re-run with \
             --allow-unverified to export anyway (the manifest will record the failure).",
            chain_err.unwrap_or_default()
        );
    }

    prepare_output_dir(&opts.output)?;
    let out = &opts.output;

    // 2. Accounts.
    let mut accounts: Vec<_> = ledger.all_accounts().cloned().collect();
    accounts.sort_by(|a, b| a.code.cmp(&b.code).then(a.id.cmp(&b.id)));
    let type_by_id: HashMap<_, _> = accounts.iter().map(|a| (a.id, a.account_type)).collect();

    {
        let mut w =
            csv::Writer::from_writer(BufWriter::new(secure_create(&out.join(FILE_ACCOUNTS))?));
        w.write_record([
            "id",
            "code",
            "name",
            "account_type",
            "currency_code",
            "status",
            "domain",
            "parent_id",
            "legal_hold",
            "require_non_negative_balance",
            "require_four_eyes",
            "exposure_limit",
            "created_at",
        ])?;
        for a in &accounts {
            w.write_record([
                a.id.to_string(),
                a.code.clone(),
                a.name.clone(),
                format!("{:?}", a.account_type),
                a.currency_code.clone(),
                format!("{:?}", a.status),
                a.domain.clone(),
                a.parent_id.map(|p| p.to_string()).unwrap_or_default(),
                a.legal_hold.to_string(),
                a.require_non_negative_balance.to_string(),
                a.require_four_eyes.to_string(),
                a.exposure_limit.map(|l| l.to_string()).unwrap_or_default(),
                a.created_at.to_rfc3339(),
            ])?;
        }
        w.flush()?;
    }

    // 3. Entries and lines, streamed in sequence order (constant memory).
    let mut entries_w = BufWriter::new(secure_create(&out.join(FILE_ENTRIES))?);
    let mut lines_w =
        csv::Writer::from_writer(BufWriter::new(secure_create(&out.join(FILE_LINES))?));
    lines_w.write_record([
        "entry_sequence",
        "entry_id",
        "entry_status",
        "domain",
        "effective_at",
        "posted_at",
        "description",
        "external_ref",
        "line_id",
        "account_id",
        "dr_cr",
        "amount_minor_units",
        "currency_code",
        "memo",
    ])?;

    let mut recomputed: HashMap<_, i128> = HashMap::new();
    let mut entry_count = 0u64;
    let mut line_count = 0u64;
    let mut first_seq: Option<u64> = None;
    let mut last_seq: Option<u64> = None;

    let ser = |e: &dyn std::fmt::Display| LedgerError::Serialization(format!("export: {e}"));

    ledger
        .stream_entries(|e: JournalEntry| {
            let status = format!("{:?}", e.status);
            let rec = ExportEntry {
                sequence: e.sequence,
                id: e.id.to_string(),
                status: status.clone(),
                description: &e.description,
                domain: &e.domain,
                effective_at: e.effective_at.to_rfc3339(),
                posted_at: e.posted_at.to_rfc3339(),
                external_ref: &e.external_ref,
                idempotency_key: &e.idempotency_key,
                reverses_entry_id: e.reverses_entry_id.map(|u| u.to_string()),
                reversed_by_entry_id: e.reversed_by_entry_id.map(|u| u.to_string()),
                approved_by: &e.approved_by,
                metadata: &e.metadata,
                content_hash: hex::encode(e.content_hash),
                prev_hash: hex::encode(e.prev_hash),
                chain_hash: hex::encode(e.chain_hash),
                lines: e
                    .lines
                    .iter()
                    .map(|l| ExportLine {
                        id: l.id.to_string(),
                        account_id: l.account_id.to_string(),
                        currency_code: &l.currency_code,
                        amount_minor_units: l.amount.as_i128().to_string(),
                        dr_cr: match l.dr_cr {
                            vledger_ledger::entry::DrCr::Debit => "debit",
                            vledger_ledger::entry::DrCr::Credit => "credit",
                        },
                        memo: &l.memo,
                    })
                    .collect(),
            };
            serde_json::to_writer(&mut entries_w, &rec).map_err(|x| ser(&x))?;
            entries_w.write_all(b"\n").map_err(|x| ser(&x))?;

            for l in &e.lines {
                let amt = l.amount.as_i128();
                let debit = matches!(l.dr_cr, vledger_ledger::entry::DrCr::Debit);
                lines_w
                    .write_record([
                        e.sequence.to_string(),
                        e.id.to_string(),
                        status.clone(),
                        e.domain.clone(),
                        e.effective_at.to_rfc3339(),
                        e.posted_at.to_rfc3339(),
                        e.description.clone(),
                        e.external_ref.clone().unwrap_or_default(),
                        l.id.to_string(),
                        l.account_id.to_string(),
                        if debit { "debit" } else { "credit" }.to_string(),
                        amt.to_string(),
                        l.currency_code.clone(),
                        l.memo.clone().unwrap_or_default(),
                    ])
                    .map_err(|x| ser(&x))?;

                if let Some(t) = type_by_id.get(&l.account_id) {
                    let delta = if is_debit_normal(t) == debit {
                        amt
                    } else {
                        -amt
                    };
                    *recomputed.entry(l.account_id).or_insert(0) += delta;
                }
                line_count += 1;
            }

            first_seq.get_or_insert(e.sequence);
            last_seq = Some(e.sequence);
            entry_count += 1;
            Ok(())
        })
        .context("failed while streaming entries")?;
    entries_w.flush()?;
    lines_w.flush()?;
    drop(entries_w);
    drop(lines_w);

    // 4. Balances: cached value vs. value recomputed from the exported lines.
    let mut mismatches = 0u64;
    {
        let mut w =
            csv::Writer::from_writer(BufWriter::new(secure_create(&out.join(FILE_BALANCES))?));
        w.write_record([
            "account_id",
            "code",
            "currency_code",
            "account_type",
            "normal_balance",
            "balance_minor_units",
            "recomputed_minor_units",
            "matches",
        ])?;
        for a in &accounts {
            let cached = ledger.balance(&a.id);
            let recalc = recomputed.get(&a.id).copied().unwrap_or(0);
            let ok = cached == recalc;
            if !ok {
                mismatches += 1;
            }
            w.write_record([
                a.id.to_string(),
                a.code.clone(),
                a.currency_code.clone(),
                format!("{:?}", a.account_type),
                if is_debit_normal(&a.account_type) {
                    "debit"
                } else {
                    "credit"
                }
                .to_string(),
                cached.to_string(),
                recalc.to_string(),
                ok.to_string(),
            ])?;
        }
        w.flush()?;
    }

    // 5. Manifest with a BLAKE3 digest of every data file.
    let mut files = Vec::new();
    for name in [FILE_ACCOUNTS, FILE_BALANCES, FILE_ENTRIES, FILE_LINES] {
        let (bytes, digest) = blake3_file(&out.join(name))?;
        files.push(FileRecord {
            name: name.to_string(),
            bytes,
            blake3: digest,
        });
    }

    let license = vledger_license::LicenseStore::load_or_free(data_dir);
    let tip_hex = hex::encode(ledger.chain_tip());
    let manifest = Manifest {
        export_format_version: EXPORT_FORMAT_VERSION,
        vledger_version: env!("CARGO_PKG_VERSION"),
        exported_at: chrono::Utc::now().to_rfc3339(),
        license_tier: license.tier.to_string(),
        account_count: accounts.len() as u64,
        entry_count,
        journal_line_count: line_count,
        first_sequence: first_seq,
        last_sequence: last_seq,
        chain_tip_hex: tip_hex.clone(),
        chain_verified,
        chain_verification_error: chain_err,
        balance_mismatches: mismatches,
        amounts_note: "All amounts are integers in minor units (e.g. cents). \
                       Balances use normal-balance sign: Asset/Expense are \
                       debit-normal, all other types credit-normal.",
        files,
    };
    let mut mf = secure_create(&out.join(FILE_MANIFEST))?;
    mf.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
    mf.write_all(b"\n")?;
    mf.sync_all()?;

    // 6. Record the export in the WORM audit log (best effort, like backup).
    let audit_path = data_dir.join("audit").join("audit.log");
    if let Ok(log) = vledger_audit::AuditLog::open(&audit_path) {
        let _ = log.append(vledger_audit::AuditEventKind::LedgerExported {
            path: out.display().to_string(),
            entry_count,
            chain_tip_hex: tip_hex.clone(),
            caller_id: "vledger-export".to_string(),
        });
    }

    println!("── VectorLedger Export ─────────────────────────");
    println!("  Output         : {}", out.display());
    println!("  Accounts       : {}", accounts.len());
    println!("  Entries        : {entry_count}");
    println!("  Journal lines  : {line_count}");
    println!("  Chain tip      : {tip_hex}");
    println!(
        "  Chain verified : {}",
        if chain_verified { "yes" } else { "NO" }
    );
    println!(
        "  Balance check  : {}",
        if mismatches == 0 {
            "all balances match".to_string()
        } else {
            format!("{mismatches} MISMATCH(ES) — see balances.csv")
        }
    );
    println!("  Manifest       : {}", out.join(FILE_MANIFEST).display());
    println!("────────────────────────────────────────────────");

    if mismatches > 0 {
        bail!(
            "{mismatches} account balance(s) differ from the journal; export written but flagged"
        );
    }
    Ok(tip_hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vledger_ledger::{Account, Amount, JournalEntryBuilder};

    fn seeded() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(data.join("wal")).unwrap();
        std::fs::create_dir_all(data.join("pages")).unwrap();
        let mut store = LedgerStore::open(&data).unwrap();
        let cash = store
            .create_account(Account::new("CASH", "Cash", AccountType::Asset, "USD", "t"))
            .unwrap();
        let rev = store
            .create_account(Account::new(
                "REV",
                "Revenue",
                AccountType::Income,
                "USD",
                "t",
            ))
            .unwrap();
        for cents in [1_000i64, 2_500, 99] {
            let amt = Amount::new(cents).unwrap();
            let e = JournalEntryBuilder::new("sale, with \"quotes\"", "t")
                .debit(cash, amt, "USD")
                .credit(rev, amt, "USD")
                .build();
            store.post_entry(e).unwrap();
        }
        drop(store);
        (dir, data)
    }

    #[test]
    fn export_writes_all_files_and_matching_manifest() {
        let (dir, data) = seeded();
        let out = dir.path().join("export");
        let tip = run(
            &data,
            &ExportOptions {
                output: out.clone(),
                allow_unverified: false,
            },
        )
        .unwrap();

        let m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(m["entry_count"], 3);
        assert_eq!(m["journal_line_count"], 6);
        assert_eq!(m["account_count"], 2);
        assert_eq!(m["chain_verified"], true);
        assert_eq!(m["balance_mismatches"], 0);
        assert_eq!(m["chain_tip_hex"], tip);

        // Every recorded digest matches the file on disk.
        for f in m["files"].as_array().unwrap() {
            let (bytes, digest) = blake3_file(&out.join(f["name"].as_str().unwrap())).unwrap();
            assert_eq!(f["blake3"], digest);
            assert_eq!(f["bytes"], bytes);
        }

        // entries.jsonl: 3 parseable lines with hex hashes of 64 chars.
        let jl = std::fs::read_to_string(out.join("entries.jsonl")).unwrap();
        let rows: Vec<serde_json::Value> = jl
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["chain_hash"].as_str().unwrap().len(), 64);
        assert_eq!(rows[0]["description"], "sale, with \"quotes\"");

        // Balances: asset +3599 (debit-normal), revenue +3599 (credit-normal).
        let bal = std::fs::read_to_string(out.join("balances.csv")).unwrap();
        assert!(bal.contains("CASH") && bal.matches("3599").count() >= 4);
    }

    #[cfg(unix)]
    #[test]
    fn export_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, data) = seeded();
        let out = dir.path().join("export");
        run(
            &data,
            &ExportOptions {
                output: out.clone(),
                allow_unverified: false,
            },
        )
        .unwrap();
        let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(out.clone()), 0o700);
        assert_eq!(mode(out.join("entries.jsonl")), 0o600);
        assert_eq!(mode(out.join("manifest.json")), 0o600);
    }

    #[test]
    fn export_refuses_non_empty_output_dir() {
        let (dir, data) = seeded();
        let out = dir.path().join("export");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("stale.txt"), b"x").unwrap();
        let err = run(
            &data,
            &ExportOptions {
                output: out.clone(),
                allow_unverified: false,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("not empty"));
        assert!(!out.join("manifest.json").exists());
    }

    #[test]
    fn export_refuses_tampered_chain_unless_allowed() {
        let (dir, data) = seeded();
        {
            let mut store = LedgerStore::open(&data).unwrap();
            assert!(store.tamper_entry_for_demo(2, "forged".into()));
        }
        let out = dir.path().join("export");
        let err = run(
            &data,
            &ExportOptions {
                output: out.clone(),
                allow_unverified: false,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("verification FAILED"));
        assert!(!out.exists(), "nothing may be written on refusal");

        run(
            &data,
            &ExportOptions {
                output: out.clone(),
                allow_unverified: true,
            },
        )
        .unwrap();
        let m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(m["chain_verified"], false);
    }

    #[test]
    fn export_fails_for_missing_data_dir() {
        let dir = TempDir::new().unwrap();
        let err = run(
            &dir.path().join("nope"),
            &ExportOptions {
                output: dir.path().join("o"),
                allow_unverified: false,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("Not initialised"));
    }
}
