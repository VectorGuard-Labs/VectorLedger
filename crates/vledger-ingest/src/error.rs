//! Error types for the ingestion pipeline.

use thiserror::Error;
use vledger_banking_core::CoreBankingError;
use vledger_ledger::LedgerError;

#[derive(Debug, Error)]
pub enum IngestError {
    /// An error from the banking connector layer.
    #[error("connector error: {0}")]
    Connector(#[from] CoreBankingError),

    /// An error from the VectorLedger ledger engine.
    #[error("ledger error: {0}")]
    Ledger(#[from] LedgerError),

    /// An error opening or writing the audit log.
    #[error("audit log error: {0}")]
    Audit(String),

    /// An amount conversion failed (e.g. string → minor units).
    #[error("amount conversion error: {0}")]
    Amount(String),

    /// A required account could not be found or created.
    #[error("account setup error: {0}")]
    AccountSetup(String),

    /// Hash chain verification failed after ingestion.
    #[error("chain verification failed: {0}")]
    ChainVerification(String),

    /// Filesystem or I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Generic error with message.
    #[error("{0}")]
    Other(String),
}
