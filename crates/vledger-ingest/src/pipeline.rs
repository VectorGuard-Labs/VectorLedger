//! The ingestion pipeline — connects a `CoreBankingConnector` to VectorLedger.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use tracing::{error, info, warn};
use uuid::Uuid;

use vledger_audit::{AuditEventKind, AuditLog};
use vledger_banking_core::{
    CoreBankingConnector, IngestRecord,
    types::{AccountQuery, TransactionQuery},
};
use vledger_crypto::hash::hash_to_hex;
use vledger_ledger::{Amount, JournalEntryBuilder, LedgerStore};

use crate::error::IngestError;
use crate::report::IngestReport;
use crate::setup::{
    ensure_account, ensure_contra_account, EXTERNAL_TRANSFER_CODE,
};

/// The ingestion pipeline.
///
/// Connects a [`CoreBankingConnector`] to a [`LedgerStore`], ingests all
/// accounts and transactions, verifies the BLAKE3 chain, and optionally
/// generates a cryptographic audit package.
pub struct IngestPipeline {
    /// Path to the VectorLedger data directory.
    data_dir: PathBuf,
    /// Path to write the audit package. `None` = skip.
    audit_package_path: Option<PathBuf>,
    /// Domain to use for ingested journal entries.
    domain: String,
}

impl IngestPipeline {
    /// Create a new pipeline targeting the given VectorLedger data directory.
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
            audit_package_path: None,
            domain: "banno-garden".to_string(),
        }
    }

    /// Set the path for the audit package output.
    pub fn with_audit_package(mut self, path: impl AsRef<Path>) -> Self {
        self.audit_package_path = Some(path.as_ref().to_path_buf());
        self
    }

    /// Set the VectorLedger domain for ingested entries.
    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = domain.into();
        self
    }

    /// Run the full ingestion pipeline against the given connector.
    ///
    /// The connector must already be authenticated before calling this method.
    pub async fn run<C>(&self, connector: &C) -> Result<IngestReport, IngestError>
    where
        C: CoreBankingConnector,
    {
        let started_at = Utc::now();
        info!(
            connector = connector.name(),
            source = connector.source_system(),
            data_dir = %self.data_dir.display(),
            "Starting ingestion run"
        );

        // ── Open the audit log ────────────────────────────────────────────
        let audit_log_path = self.data_dir.join("audit").join("audit.log");
        std::fs::create_dir_all(self.data_dir.join("audit"))
            .map_err(IngestError::Io)?;
        let audit_log = AuditLog::open(&audit_log_path)
            .map_err(|e| IngestError::Audit(e.to_string()))?;

        // ── Open the ledger store ─────────────────────────────────────────
        let mut store = LedgerStore::open(&self.data_dir)?;

        // ── Ensure infrastructure accounts exist ──────────────────────────
        let _contra_id = ensure_contra_account(&mut store)?;

        // ── Step 1: Fetch accounts ────────────────────────────────────────
        info!("Fetching accounts from {}", connector.source_system());
        let normalized_accounts = connector
            .fetch_accounts(&AccountQuery::default())
            .await?;

        let accounts_fetched = normalized_accounts.len();
        info!(count = accounts_fetched, "Accounts fetched");

        // Ensure each account exists in VectorLedger and build the
        // source_account_id → VectorLedger AccountId map.
        let mut source_to_ledger: HashMap<String, uuid::Uuid> = HashMap::new();
        let mut accounts_created = 0usize;
        let mut accounts_existing = 0usize;

        for normalized in &normalized_accounts {
            let result = ensure_account(&mut store, normalized)?;
            source_to_ledger.insert(normalized.id.clone(), result.account_id);
            if result.created {
                accounts_created += 1;
                // Audit: account created
                let _ = audit_log.append(AuditEventKind::AccountCreated {
                    account_id: result.account_id,
                    account_code: normalized.id.clone(),
                    domain: normalized.institution_id.clone(),
                    caller_id: connector.name().to_string(),
                });
            } else {
                accounts_existing += 1;
            }
        }

        // Look up the contra account ID
        let contra_id = store
            .all_accounts()
            .find(|a| a.code == EXTERNAL_TRANSFER_CODE)
            .map(|a| a.id)
            .ok_or_else(|| IngestError::AccountSetup(
                "EXTERNAL_TRANSFER account not found after setup".to_string(),
            ))?;

        // ── Step 2: Fetch and ingest transactions ─────────────────────────
        let mut transactions_fetched = 0usize;
        let mut entries_posted = 0usize;
        let mut entries_skipped = 0usize;
        let mut entries_failed = 0usize;

        for normalized_account in &normalized_accounts {
            let ledger_account_id = match source_to_ledger.get(&normalized_account.id) {
                Some(id) => *id,
                None => {
                    warn!(
                        source_account = %normalized_account.id,
                        "No VectorLedger account found — skipping transactions"
                    );
                    continue;
                }
            };

            info!(
                source_account = %normalized_account.id,
                ledger_account = %ledger_account_id,
                "Fetching transactions"
            );

            let txns = connector
                .fetch_transactions(&TransactionQuery {
                    account_id: normalized_account.id.clone(),
                    ..Default::default()
                })
                .await?;

            transactions_fetched += txns.len();
            info!(
                count = txns.len(),
                account = %normalized_account.id,
                "Transactions fetched"
            );

            for tx in &txns {
                // Build the IngestRecord — handles dr/cr routing
                let record = IngestRecord::from_transaction(
                    tx,
                    connector.name(),
                    connector.source_system(),
                );

                // Resolve the debit and credit account IDs
                let debit_id = resolve_account_id(
                    &record.debit_account,
                    &source_to_ledger,
                    contra_id,
                    ledger_account_id,
                );
                let credit_id = resolve_account_id(
                    &record.credit_account,
                    &source_to_ledger,
                    contra_id,
                    ledger_account_id,
                );

                // Convert MinorUnits to VectorLedger Amount
                let amount_val = record.amount.as_i64().abs();
                let amount = match Amount::new(amount_val) {
                    Some(a) => a,
                    None => {
                        warn!(
                            tx_id = %tx.id,
                            amount = amount_val,
                            "Zero amount — skipping transaction"
                        );
                        entries_skipped += 1;
                        continue;
                    }
                };

                // Build the journal entry
                let mut builder = JournalEntryBuilder::new(
                    &record.description,
                    &self.domain,
                )
                .debit(debit_id, amount, record.currency.0.clone())
                .credit(credit_id, amount, record.currency.0.clone())
                .idempotency_key(&record.idempotency_key)
                .effective_at(record.effective_at);

                if let Some(ref ext_ref) = record.external_ref {
                    builder = builder.external_ref(ext_ref);
                }

                // Embed source metadata as JSON
                let metadata = serde_json::json!({
                    "source_connector": record.source_connector,
                    "source_system": record.source_system,
                    "source_transaction_id": tx.id,
                    "source_account_id": tx.account_id,
                    "transaction_type": format!("{}", tx.transaction_type),
                }).to_string();
                builder = builder.metadata(metadata);

                let entry = builder.build();

                // Record the current entry count before posting so we can
                // detect whether a new entry was actually written or the
                // idempotency key fired and returned the existing sequence.
                let count_before = store.entry_count();

                // Post to VectorLedger
                match store.post_entry(entry) {
                    Ok(seq) => {
                        let count_after = store.entry_count();
                        if count_after == count_before {
                            // entry_count did not increase — idempotency key
                            // was already present; nothing new was written.
                            entries_skipped += 1;
                            info!(
                                seq = seq,
                                tx_id = %tx.id,
                                "Entry skipped (idempotency key already posted)"
                            );
                        } else {
                            entries_posted += 1;
                            info!(
                                seq = seq,
                                tx_id = %tx.id,
                                amount = amount_val,
                                "Entry posted"
                            );
                            // Audit: entry posted
                            let _ = audit_log.append(AuditEventKind::EntryPosted {
                                entry_id: Uuid::new_v4(),
                                entry_sequence: seq,
                                domain: self.domain.clone(),
                                amount_sum: amount_val,
                                caller_id: connector.name().to_string(),
                            });
                        }
                    }
                    Err(vledger_ledger::LedgerError::IdempotencyConflict { .. }) => {
                        entries_skipped += 1;
                    }
                    Err(e) => {
                        error!(tx_id = %tx.id, error = %e, "Failed to post entry");
                        entries_failed += 1;
                    }
                }
            }
        }

        // ── Step 3: Verify chain integrity ────────────────────────────────
        info!("Verifying BLAKE3 hash chain integrity...");
        let chain_verified = match store.verify_chain_integrity() {
            Ok(()) => {
                info!("✅ Hash chain verified successfully");
                true
            }
            Err(e) => {
                error!("❌ Hash chain verification FAILED: {e}");
                false
            }
        };

        let chain_tip = hash_to_hex(store.chain_tip());
        let total_ledger_entries = store.entry_count();

        // Drop the store before generating audit package (releases advisory lock)
        drop(store);

        // ── Step 4: Generate audit package ────────────────────────────────
        let audit_package_path_str = if let Some(ref pkg_path) = self.audit_package_path {
            info!(path = %pkg_path.display(), "Generating audit package...");
            match generate_audit_package(&self.data_dir, pkg_path) {
                Ok(()) => {
                    info!("✅ Audit package generated");
                    Some(pkg_path.to_string_lossy().to_string())
                }
                Err(e) => {
                    error!("Failed to generate audit package: {e}");
                    None
                }
            }
        } else {
            None
        };

        let completed_at = Utc::now();

        let report = IngestReport {
            started_at,
            completed_at,
            connector: connector.name().to_string(),
            source_system: connector.source_system().to_string(),
            accounts_fetched,
            accounts_created,
            accounts_existing,
            transactions_fetched,
            entries_posted,
            entries_skipped,
            entries_failed,
            total_ledger_entries,
            chain_tip,
            chain_verified,
            audit_package_path: audit_package_path_str,
        };

        Ok(report)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Resolve an account code from an `IngestRecord` into a VectorLedger `AccountId`.
///
/// - `EXTERNAL_TRANSFER` → returns `contra_id`
/// - Anything else → looks up in `source_to_ledger`, falls back to `fallback_id`
fn resolve_account_id(
    code: &str,
    source_to_ledger: &HashMap<String, uuid::Uuid>,
    contra_id: uuid::Uuid,
    fallback_id: uuid::Uuid,
) -> uuid::Uuid {
    if code == EXTERNAL_TRANSFER_CODE {
        contra_id
    } else {
        source_to_ledger.get(code).copied().unwrap_or(fallback_id)
    }
}

/// Generate an audit package using the vledger audit_package module.
/// We call the same logic as `vledger audit-package` CLI command.
fn generate_audit_package(
    data_dir: &Path,
    output_path: &Path,
) -> Result<(), IngestError> {
    use vledger_ledger::LedgerStore;
    use vledger_crypto::{
        hash::hash_to_hex,
        merkle::merkle_root,
        ZERO_HASH,
    };

    let store = LedgerStore::open(data_dir)?;
    let entry_count = store.entry_count();

    // Build Merkle leaves from all entry canonical bytes
    let mut leaf_bytes: Vec<Vec<u8>> = Vec::new();
    let mut first_sequence = 0u64;
    let mut last_sequence = 0u64;

    store.stream_entries(|e| {
        if first_sequence == 0 { first_sequence = e.sequence; }
        last_sequence = e.sequence;
        leaf_bytes.push(e.canonical_bytes());
        Ok(())
    }).map_err(IngestError::Ledger)?;

    let root: [u8; 32] = if leaf_bytes.is_empty() {
        ZERO_HASH
    } else {
        merkle_root(&leaf_bytes)
    };
    let root_hex = hash_to_hex(&root);
    let chain_tip = hash_to_hex(store.chain_tip());
    let generated_at = Utc::now().to_rfc3339();

    // Sign the commitment
    let mut sign_payload = Vec::new();
    sign_payload.extend_from_slice(&root);
    sign_payload.extend_from_slice(store.chain_tip());
    sign_payload.extend_from_slice(&(entry_count as u64).to_le_bytes());

    let (root_sig_hex, pubkey_hex) = match store.sign_bytes(&sign_payload) {
        Some((sig, pk)) => (hex::encode(sig), hex::encode(pk)),
        None => (String::new(), String::new()),
    };

    let package = serde_json::json!({
        "meta": {
            "format_version":  2,
            "package_type":    "commitment",
            "generated_at":    generated_at,
            "vledger_version": env!("CARGO_PKG_VERSION"),
            "entry_count":     entry_count,
            "first_sequence":  first_sequence,
            "last_sequence":   last_sequence,
            "chain_tip":       chain_tip,
            "merkle_root":     root_hex,
            "root_signature":  root_sig_hex,
            "signing_pubkey":  pubkey_hex,
            "algorithm":       "BLAKE3 + Ed25519",
            "tenant":          "",
            "description":     "VectorLedger-Ingest auto-generated commitment",
            "period_start":    "",
            "period_end":      "",
        }
    });

    let json_str = serde_json::to_string_pretty(&package)
        .map_err(|e| IngestError::Other(format!("serialize audit package: {e}")))?;
    std::fs::write(output_path, json_str).map_err(IngestError::Io)?;

    info!(
        entry_count,
        merkle_root = %root_hex,
        path = %output_path.display(),
        "Audit package written"
    );
    Ok(())
}
