//! `vledger-banking-core` — vendor-agnostic banking connector interface.
//!
//! This crate defines the shared types and traits that every banking core
//! connector must implement to feed data into VectorLedger.  No connector
//! (Jack Henry, FIS, Fiserv, Temenos, …) should be imported here — this
//! crate is intentionally free of vendor-specific code.
//!
//! # Architecture
//!
//! ```text
//! Jack Henry Consumer API  →  vledger-banno-connector  ─┐
//! FIS / Fiserv             →  vledger-fis-connector   ──┼─→  vledger-banking-core  →  VectorLedger
//! Any core                 →  vledger-[core]-connector ─┘
//! ```
//!
//! Each connector implements [`CoreBankingConnector`] and maps its source
//! system's data model into [`NormalizedAccount`] and [`NormalizedTransaction`].
//! VectorLedger ingests from those normalized types and never imports connector
//! crates directly.

pub mod error;
pub mod types;
pub mod connector;
pub mod ingest;

pub use error::CoreBankingError;
pub use types::{
    NormalizedAccount, NormalizedTransaction,
    AccountType, AccountStatus,
    TransactionType, TransactionStatus,
    MinorUnits, Currency,
};
pub use connector::CoreBankingConnector;
pub use ingest::IngestRecord;
