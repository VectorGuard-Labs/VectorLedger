//! Error types for the banking core connector layer.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreBankingError {
    /// Authentication failed or token is expired.
    #[error("authentication error: {0}")]
    Auth(String),

    /// The remote API returned an unexpected HTTP status.
    #[error("API error {status}: {body}")]
    Api { status: u16, body: String },

    /// Network or I/O failure.
    #[error("network error: {0}")]
    Network(String),

    /// A field value could not be parsed into the expected type.
    #[error("parse error for field '{field}': {reason}")]
    Parse { field: &'static str, reason: String },

    /// An amount string could not be converted to minor units.
    #[error("amount conversion error: '{raw}' — {reason}")]
    AmountConversion { raw: String, reason: String },

    /// The connector received data for an unsupported currency.
    #[error("unsupported currency: {0}")]
    UnsupportedCurrency(String),

    /// The connector is not yet authenticated — call authenticate() first.
    #[error("connector is not authenticated")]
    NotAuthenticated,

    /// A required field was missing from the API response.
    #[error("missing required field: {0}")]
    MissingField(&'static str),

    /// An operation was attempted that the connector does not support.
    #[error("unsupported operation: {0}")]
    Unsupported(String),
}
