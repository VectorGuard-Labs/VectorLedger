//! Ingestion run report.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Summary of a completed ingestion run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestReport {
    /// When this run started.
    pub started_at: DateTime<Utc>,
    /// When this run completed.
    pub completed_at: DateTime<Utc>,

    /// Name of the connector used.
    pub connector: String,
    /// Name of the source system.
    pub source_system: String,

    /// Number of accounts fetched from the source.
    pub accounts_fetched: usize,
    /// Number of accounts created in VectorLedger (new this run).
    pub accounts_created: usize,
    /// Number of accounts that already existed (skipped).
    pub accounts_existing: usize,

    /// Number of transactions fetched from the source.
    pub transactions_fetched: usize,
    /// Number of journal entries newly posted to VectorLedger.
    pub entries_posted: usize,
    /// Number of transactions skipped (idempotency key already posted).
    pub entries_skipped: usize,
    /// Number of transactions that failed to post.
    pub entries_failed: usize,

    /// Total entries in VectorLedger after this run.
    pub total_ledger_entries: usize,

    /// Current BLAKE3 chain tip hex after this run.
    pub chain_tip: String,

    /// Whether chain integrity verification passed after this run.
    pub chain_verified: bool,

    /// Path to the generated audit package (if produced).
    pub audit_package_path: Option<String>,
}

impl IngestReport {
    /// Print a human-readable summary to stdout.
    pub fn print_summary(&self) {
        println!();
        println!("╔══════════════════════════════════════════════════════════════╗");
        println!("║  VectorLedger Ingestion Report                               ║");
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║  Source:      {:<47} ║", self.source_system);
        println!("║  Connector:   {:<47} ║", self.connector);
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║  ACCOUNTS                                                    ║");
        println!("║    Fetched:   {:<47} ║", self.accounts_fetched);
        println!("║    Created:   {:<47} ║", self.accounts_created);
        println!("║    Existing:  {:<47} ║", self.accounts_existing);
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║  TRANSACTIONS                                                ║");
        println!("║    Fetched:   {:<47} ║", self.transactions_fetched);
        println!("║    Posted:    {:<47} ║", self.entries_posted);
        println!("║    Skipped:   {:<47} ║", self.entries_skipped);
        println!("║    Failed:    {:<47} ║", self.entries_failed);
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║  LEDGER STATE                                                ║");
        println!("║    Total entries: {:<43} ║", self.total_ledger_entries);
        let chain_display = if self.chain_tip.len() > 43 {
            format!("{}...", &self.chain_tip[..40])
        } else {
            self.chain_tip.clone()
        };
        println!("║    Chain tip: {:<47} ║", chain_display);
        let verified_str = if self.chain_verified {
            "✅ VERIFIED"
        } else {
            "❌ FAILED"
        };
        println!("║    Integrity: {:<47} ║", verified_str);
        if let Some(ref path) = self.audit_package_path {
            let display = if path.len() > 47 {
                format!("...{}", &path[path.len()-44..])
            } else {
                path.clone()
            };
            println!("╠══════════════════════════════════════════════════════════════╣");
            println!("║  AUDIT PACKAGE                                               ║");
            println!("║    {:<58} ║", display);
        }
        println!("╠══════════════════════════════════════════════════════════════╣");
        let duration = self.completed_at
            .signed_duration_since(self.started_at)
            .num_milliseconds();
        println!("║  Duration: {:<51} ║", format!("{duration} ms"));
        println!("╚══════════════════════════════════════════════════════════════╝");
        println!();
    }
}
