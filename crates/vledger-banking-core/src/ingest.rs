//! Ingestion record — the unit of work handed to VectorLedger.
//!
//! After a connector fetches and normalizes transactions, the ingestion
//! pipeline converts each [`NormalizedTransaction`] into an [`IngestRecord`]
//! that contains everything needed to post a double-entry journal entry to
//! VectorLedger.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{Currency, MinorUnits, NormalizedTransaction, TransactionStatus};

/// A single unit of work for VectorLedger's ingestion pipeline.
///
/// Each `IngestRecord` maps to exactly one double-entry journal entry.
/// The connector layer produces these; the VectorLedger layer consumes them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestRecord {
    /// VectorLedger idempotency key — set to the source transaction ID.
    /// Guarantees exactly-once posting regardless of retries.
    pub idempotency_key: String,

    /// The debit account code in VectorLedger.
    pub debit_account: String,

    /// The credit account code in VectorLedger.
    pub credit_account: String,

    /// Amount in minor units (always positive — sign encoded in dr/cr routing).
    pub amount: MinorUnits,

    /// Currency.
    pub currency: Currency,

    /// Human-readable description posted to the ledger entry.
    pub description: String,

    /// Effective date of the transaction.
    pub effective_at: DateTime<Utc>,

    /// Settlement status.
    pub status: TransactionStatus,

    /// Optional external reference (e.g. check number, payment ID).
    pub external_ref: Option<String>,

    /// Source connector name for audit trail.
    pub source_connector: String,

    /// Source system name for audit trail.
    pub source_system: String,
}

impl IngestRecord {
    /// Build an [`IngestRecord`] from a [`NormalizedTransaction`].
    ///
    /// # Account routing
    ///
    /// VectorLedger requires double-entry: every transaction must have both a
    /// debit and a credit account.  The routing convention for banking
    /// connectors is:
    ///
    /// - **Credit to account** (positive amount on deposit account):
    ///   - Debit:  `EXTERNAL_TRANSFER` (funds coming in from outside)
    ///   - Credit: `<account_id>` (the customer's account)
    ///
    /// - **Debit from account** (negative amount on deposit account):
    ///   - Debit:  `<account_id>` (the customer's account)
    ///   - Credit: `EXTERNAL_TRANSFER` (funds going out)
    ///
    /// The `EXTERNAL_TRANSFER` account must exist in VectorLedger before
    /// ingestion begins.  It acts as the contra-account for all external
    /// fund movements.
    pub fn from_transaction(
        tx: &NormalizedTransaction,
        connector_name: &str,
        source_system: &str,
    ) -> Self {
        let (debit_account, credit_account) = if tx.is_credit() {
            // Money coming into the account
            ("EXTERNAL_TRANSFER".to_string(), tx.account_id.clone())
        } else {
            // Money leaving the account
            (tx.account_id.clone(), "EXTERNAL_TRANSFER".to_string())
        };

        IngestRecord {
            idempotency_key: tx.id.clone(),
            debit_account,
            credit_account,
            amount: tx.absolute_amount(),
            currency: tx.currency.clone(),
            description: tx.description.clone(),
            effective_at: tx.posted_at,
            status: tx.status.clone(),
            external_ref: tx.check_number.clone(),
            source_connector: connector_name.to_string(),
            source_system: source_system.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Currency, MinorUnits, TransactionStatus, TransactionType};
    use chrono::Utc;

    fn make_tx(amount: i64, account_id: &str) -> NormalizedTransaction {
        NormalizedTransaction {
            id: "test-id-001".to_string(),
            account_id: account_id.to_string(),
            amount: MinorUnits(amount),
            currency: Currency("USD".to_string()),
            transaction_type: TransactionType::Transfer,
            description: "Test transaction".to_string(),
            posted_at: Utc::now(),
            status: TransactionStatus::Posted,
            running_balance: None,
            sequence: Some(1),
            check_number: None,
            merchant_name: None,
            last_updated: Utc::now(),
            raw: None,
        }
    }

    #[test]
    fn credit_routes_external_to_account() {
        let tx = make_tx(100_000, "CHECKING-001");
        let record = IngestRecord::from_transaction(&tx, "test-connector", "test");
        assert_eq!(record.debit_account, "EXTERNAL_TRANSFER");
        assert_eq!(record.credit_account, "CHECKING-001");
        assert_eq!(record.amount.as_i64(), 100_000);
    }

    #[test]
    fn debit_routes_account_to_external() {
        let tx = make_tx(-50_000, "CHECKING-001");
        let record = IngestRecord::from_transaction(&tx, "test-connector", "test");
        assert_eq!(record.debit_account, "CHECKING-001");
        assert_eq!(record.credit_account, "EXTERNAL_TRANSFER");
        assert_eq!(record.amount.as_i64(), 50_000); // absolute
    }

    #[test]
    fn idempotency_key_is_transaction_id() {
        let tx = make_tx(100_000, "CHECKING-001");
        let record = IngestRecord::from_transaction(&tx, "test-connector", "test");
        assert_eq!(record.idempotency_key, "test-id-001");
    }
}
