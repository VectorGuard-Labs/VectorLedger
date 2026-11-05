//! The `CoreBankingConnector` trait — the interface every connector implements.
//!
//! VectorLedger's ingestion pipeline depends only on this trait.  The concrete
//! connector types (Jack Henry, FIS, etc.) live in separate crates and are
//! never imported by VectorLedger directly.

use crate::error::CoreBankingError;
use crate::types::{
    AccountQuery, ConnectorStatus, NormalizedAccount,
    NormalizedTransaction, TransactionQuery,
};

/// The interface every banking core connector must implement.
///
/// # Design contract
///
/// - Implementations are responsible for authentication and token refresh.
///   Callers do not manage tokens directly — they call [`authenticate`] once
///   and then use the other methods.
/// - All methods are `async` — implementations may make network calls.
/// - Errors are returned as [`CoreBankingError`] — never panicked.
/// - Idempotency is the caller's responsibility: the connector returns raw
///   source data; the ingestion layer uses [`NormalizedTransaction::id`] as
///   the VectorLedger idempotency key.
///
/// [`authenticate`]: CoreBankingConnector::authenticate
pub trait CoreBankingConnector: Send + Sync {
    /// Authenticate with the source system and store the resulting credentials
    /// internally.  Must be called before [`fetch_accounts`] or
    /// [`fetch_transactions`].
    ///
    /// [`fetch_accounts`]: CoreBankingConnector::fetch_accounts
    /// [`fetch_transactions`]: CoreBankingConnector::fetch_transactions
    fn authenticate(
        &mut self,
    ) -> impl std::future::Future<Output = Result<(), CoreBankingError>> + Send;

    /// Fetch accounts matching the given query parameters.
    fn fetch_accounts(
        &self,
        params: &AccountQuery,
    ) -> impl std::future::Future<Output = Result<Vec<NormalizedAccount>, CoreBankingError>> + Send;

    /// Fetch transactions for a specific account.
    fn fetch_transactions(
        &self,
        params: &TransactionQuery,
    ) -> impl std::future::Future<Output = Result<Vec<NormalizedTransaction>, CoreBankingError>> + Send;

    /// Check that the connector can reach the source system.
    /// Should not require a valid auth token — useful for startup checks.
    fn health_check(
        &self,
    ) -> impl std::future::Future<Output = ConnectorStatus> + Send;

    /// Human-readable name of this connector (e.g. `"vledger-banno-connector"`).
    fn name(&self) -> &'static str;

    /// Human-readable name of the source system (e.g. `"Jack Henry Garden"`).
    fn source_system(&self) -> &'static str;
}
