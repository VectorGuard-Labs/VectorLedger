//! `vledger-ingest` — ingestion pipeline from banking connectors to VectorLedger.
//!
//! # Pipeline
//!
//! ```text
//! CoreBankingConnector
//!   │  fetch_accounts()
//!   │  fetch_transactions()
//!   ▼
//! IngestPipeline::run()
//!   │  ensure_accounts_exist()   — create missing accounts in VectorLedger
//!   │  ensure_contra_account()   — create EXTERNAL_TRANSFER if missing
//!   │  ingest_transactions()     — post journal entries (idempotent)
//!   ▼
//! LedgerStore::post_entry()      — BLAKE3 chain extended, WAL fsynced
//!   │
//!   ▼
//! IngestReport
//!   │  verify_chain()            — BLAKE3 chain integrity confirmed
//!   │  generate_audit_package()  — Ed25519-signed Merkle commitment
//!   ▼
//! demo output
//! ```

pub mod error;
pub mod pipeline;
pub mod report;
pub mod setup;

pub use error::IngestError;
pub use pipeline::IngestPipeline;
pub use report::IngestReport;
