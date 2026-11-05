//! VectorLedger account setup for the ingestion pipeline.
//!
//! Before any transactions can be ingested, the chart of accounts must contain:
//! 1. One account per banking account (mapped from `NormalizedAccount`).
//! 2. An `EXTERNAL_TRANSFER` contra-account used as the double-entry
//!    counterpart for all external fund movements.

use tracing::{info, warn};
use vledger_ledger::{Account, AccountId, AccountType, LedgerStore};

use vledger_banking_core::types::NormalizedAccount;

use crate::error::IngestError;

/// The VectorLedger account code used as the contra-account for all
/// external fund movements (deposits from and withdrawals to the outside world).
pub const EXTERNAL_TRANSFER_CODE: &str = "EXTERNAL_TRANSFER";

/// The domain used for system/infrastructure accounts.
pub const SYSTEM_DOMAIN: &str = "system";

/// Ensure the `EXTERNAL_TRANSFER` contra-account exists.
/// Returns the account ID (existing or newly created).
pub fn ensure_contra_account(store: &mut LedgerStore) -> Result<AccountId, IngestError> {
    // Check if it already exists by searching all accounts
    for acct in store.all_accounts() {
        if acct.code == EXTERNAL_TRANSFER_CODE {
            info!(
                account_id = %acct.id,
                "EXTERNAL_TRANSFER account already exists"
            );
            return Ok(acct.id);
        }
    }

    // Create it
    let mut acct = Account::new(
        EXTERNAL_TRANSFER_CODE,
        "External Transfer — contra-account for external fund movements",
        AccountType::Suspense,
        "USD",
        SYSTEM_DOMAIN,
    );
    // Suspense accounts are allowed to go negative (they net to zero over time)
    acct.require_non_negative_balance = false;

    let id = store.create_account(acct)?;
    info!(account_id = %id, "Created EXTERNAL_TRANSFER contra-account");
    Ok(id)
}

/// Result of ensuring a single banking account exists in VectorLedger.
pub struct AccountEnsureResult {
    pub account_id: AccountId,
    pub created: bool,
}

/// Ensure a `NormalizedAccount` exists in VectorLedger.
///
/// Uses the source account's `id` as the VectorLedger account code so the
/// mapping is stable and deterministic across runs.
///
/// Returns the VectorLedger `AccountId` and whether it was newly created.
pub fn ensure_account(
    store: &mut LedgerStore,
    normalized: &NormalizedAccount,
) -> Result<AccountEnsureResult, IngestError> {
    // Check if already exists using the source account ID as the code
    for acct in store.all_accounts() {
        if acct.code == normalized.id {
            return Ok(AccountEnsureResult {
                account_id: acct.id,
                created: false,
            });
        }
    }

    // Map the banking account type to a VectorLedger AccountType
    let account_type = map_account_type(normalized);

    // Build the human-readable name
    let name = format!(
        "{} ({}) — {}",
        normalized.name.trim(),
        normalized
            .account_sub_type
            .as_deref()
            .unwrap_or(&normalized.account_type.to_string()),
        normalized.institution_name,
    );

    let mut acct = Account::new(
        normalized.id.clone(),
        name,
        account_type,
        normalized.currency.0.clone(),
        normalized.institution_id.clone(),
    );

    // Asset accounts can go negative during ingestion if we receive a debit
    // before the initial funding credit — relax the constraint for now.
    // A production deployment would set this based on business rules.
    acct.require_non_negative_balance = false;

    let id = store.create_account(acct)?;
    info!(
        account_id = %id,
        source_account_id = %normalized.id,
        name = %normalized.name.trim(),
        "Created VectorLedger account"
    );

    Ok(AccountEnsureResult {
        account_id: id,
        created: true,
    })
}

/// Map a `NormalizedAccount`'s type to a VectorLedger `AccountType`.
fn map_account_type(normalized: &NormalizedAccount) -> AccountType {
    use vledger_banking_core::types::AccountType as BankAccountType;
    match &normalized.account_type {
        BankAccountType::Deposit => {
            // Checking and savings accounts are Assets from the account holder's
            // perspective — they represent money the holder is owed by the bank.
            AccountType::Asset
        }
        BankAccountType::Loan => {
            // Loans are Liabilities — money owed to the bank.
            AccountType::Liability
        }
        BankAccountType::Investment => {
            // Investment accounts are Assets.
            AccountType::Asset
        }
        BankAccountType::Other(_) => {
            warn!(
                account_type = ?normalized.account_type,
                "Unknown banking account type — mapping to Suspense"
            );
            AccountType::Suspense
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;
    use vledger_banking_core::types::{
        AccountStatus, AccountType as BankAccountType, Currency, MinorUnits, NormalizedAccount,
    };
    use chrono::Utc;

    fn open_test_store(dir: &TempDir) -> LedgerStore {
        std::fs::create_dir_all(dir.path().join("wal")).unwrap();
        std::fs::create_dir_all(dir.path().join("pages")).unwrap();
        LedgerStore::open(dir.path()).unwrap()
    }

    fn make_normalized_account(id: &str) -> NormalizedAccount {
        NormalizedAccount {
            id: id.to_string(),
            masked_number: Some("x5107".to_string()),
            name: "Test Checking Account".to_string(),
            account_type: BankAccountType::Deposit,
            account_sub_type: Some("Checking".to_string()),
            balance: MinorUnits(100_000),
            available_balance: Some(MinorUnits(100_000)),
            currency: Currency("USD".to_string()),
            status: AccountStatus::Active,
            routing_number: Some("999988961".to_string()),
            institution_id: "garden-institution-id".to_string(),
            institution_name: "Garden".to_string(),
            fetched_at: Utc::now(),
            raw: None,
        }
    }

    #[test]
    fn contra_account_created_on_first_call() {
        let dir = TempDir::new().unwrap();
        let mut store = open_test_store(&dir);
        let id = ensure_contra_account(&mut store).unwrap();
        // Should find it immediately on second call
        let id2 = ensure_contra_account(&mut store).unwrap();
        assert_eq!(id, id2, "same account ID returned on second call");
    }

    #[test]
    fn ensure_account_created_and_idempotent() {
        let dir = TempDir::new().unwrap();
        let mut store = open_test_store(&dir);
        let normalized = make_normalized_account("e4ab6531-d442-4928-bf05-1db1b8ef5ac0");

        let result1 = ensure_account(&mut store, &normalized).unwrap();
        assert!(result1.created);

        let result2 = ensure_account(&mut store, &normalized).unwrap();
        assert!(!result2.created, "second call should not create a new account");
        assert_eq!(result1.account_id, result2.account_id);
    }

    #[test]
    fn deposit_account_maps_to_asset() {
        let normalized = make_normalized_account("test-id");
        let account_type = map_account_type(&normalized);
        assert_eq!(account_type, AccountType::Asset);
    }

    #[test]
    fn loan_account_maps_to_liability() {
        let mut normalized = make_normalized_account("test-id");
        normalized.account_type = BankAccountType::Loan;
        let account_type = map_account_type(&normalized);
        assert_eq!(account_type, AccountType::Liability);
    }
}
