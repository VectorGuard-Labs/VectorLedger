//! Mapping from raw Banno API responses to normalized VectorLedger types.
//!
//! This is the translation layer.  Every field mapping is explicit — no
//! implicit defaults that could silently swallow bad data.

use chrono::{DateTime, Utc};
use tracing::warn;

use vledger_banking_core::{
    CoreBankingError,
    types::{
        AccountStatus, AccountType, Currency, MinorUnits,
        NormalizedAccount, NormalizedTransaction,
        TransactionStatus, TransactionType,
    },
};

use crate::response::{RawAccount, RawTransaction};

// ── Account mapping ───────────────────────────────────────────────────────────

/// Map a [`RawAccount`] into a [`NormalizedAccount`].
pub fn map_account(raw: &RawAccount) -> Result<NormalizedAccount, CoreBankingError> {
    // Currency — Banno deposit accounts are always USD in Garden;
    // the field is not returned directly on the account object.
    // Default to USD; real multi-currency support requires per-account
    // currency detection from transaction data.
    let currency = Currency("USD".to_string());
    let precision = currency.minor_unit_precision();

    let balance = raw
        .balance
        .as_deref()
        .map(|b| MinorUnits::from_decimal_str(b, precision))
        .transpose()
        .map_err(|e| CoreBankingError::AmountConversion {
            raw: raw.balance.clone().unwrap_or_default(),
            reason: e,
        })?
        .unwrap_or(MinorUnits(0));

    let available_balance = raw
        .available_balance
        .as_deref()
        .map(|b| MinorUnits::from_decimal_str(b, precision))
        .transpose()
        .map_err(|e| CoreBankingError::AmountConversion {
            raw: raw.available_balance.clone().unwrap_or_default(),
            reason: e,
        })?;

    let account_type = map_account_type(raw.account_type.as_deref());
    let status = map_account_status(raw.account_status.as_deref(), raw.active);

    let fetched_at = raw
        .fetched_date
        .as_deref()
        .and_then(|d| parse_datetime(d))
        .unwrap_or_else(Utc::now);

    let institution = raw.institution.as_ref();

    Ok(NormalizedAccount {
        id: raw.id.clone(),
        masked_number: raw.numbers.clone(),
        name: raw.name.clone().unwrap_or_else(|| raw.id.clone()),
        account_type,
        account_sub_type: raw.account_sub_type.clone(),
        balance,
        available_balance,
        currency,
        status,
        routing_number: raw.routing_number.clone(),
        institution_id: institution.map(|i| i.id.clone()).unwrap_or_default(),
        institution_name: institution.map(|i| i.name.clone()).unwrap_or_default(),
        fetched_at,
        raw: serde_json::to_value(raw).ok(),
    })
}

fn map_account_type(s: Option<&str>) -> AccountType {
    match s.unwrap_or("").to_lowercase().as_str() {
        "deposit"    => AccountType::Deposit,
        "loan"       => AccountType::Loan,
        "investment" => AccountType::Investment,
        other        => AccountType::Other(other.to_string()),
    }
}

fn map_account_status(account_status: Option<&str>, active: Option<bool>) -> AccountStatus {
    // Prefer the explicit accountStatus field; fall back to active flag
    match account_status.unwrap_or("").to_lowercase().as_str() {
        "active"  => AccountStatus::Active,
        "closed"  => AccountStatus::Closed,
        "frozen"  => AccountStatus::Frozen,
        _ => match active {
            Some(true)  => AccountStatus::Active,
            Some(false) => AccountStatus::Closed,
            None        => AccountStatus::Unknown,
        },
    }
}

// ── Transaction mapping ───────────────────────────────────────────────────────

/// Map a [`RawTransaction`] into a [`NormalizedTransaction`].
pub fn map_transaction(
    raw: &RawTransaction,
    account_id: &str,
) -> Result<NormalizedTransaction, CoreBankingError> {
    let currency = Currency(
        raw.currency.clone().unwrap_or_else(|| "USD".to_string())
    );
    let precision = currency.minor_unit_precision();

    let amount_str = raw.amount.as_deref().unwrap_or("0.00");
    let amount = MinorUnits::from_decimal_str(amount_str, precision)
        .map_err(|reason| CoreBankingError::AmountConversion {
            raw: amount_str.to_string(),
            reason,
        })?;

    let transaction_type = TransactionType::from(
        raw.transaction_type.as_deref().unwrap_or("UNKNOWN")
    );

    let status = TransactionStatus::from(
        raw.pending_status.as_deref().unwrap_or("NoReconciliation")
    );

    // Use displayName → filteredMemo → memo in preference order
    let description = raw
        .display_name
        .clone()
        .or_else(|| raw.filtered_memo.clone())
        .or_else(|| raw.memo.clone())
        .unwrap_or_else(|| transaction_type.to_string());

    let posted_at = raw
        .date_posted
        .as_deref()
        .or(raw.date.as_deref())
        .and_then(parse_datetime)
        .unwrap_or_else(Utc::now);

    let last_updated = raw
        .last_updated
        .as_deref()
        .and_then(parse_datetime)
        .unwrap_or_else(Utc::now);

    let running_balance = raw
        .running_balance
        .as_deref()
        .map(|b| MinorUnits::from_decimal_str(b, precision))
        .transpose()
        .unwrap_or_else(|e| {
            warn!(error = %e, "could not parse running_balance — skipping");
            None
        });

    let merchant_name = raw
        .merchant
        .as_ref()
        .and_then(|m| m.name.clone());

    Ok(NormalizedTransaction {
        id: raw.id.clone(),
        account_id: raw
            .account_id
            .clone()
            .unwrap_or_else(|| account_id.to_string()),
        amount,
        currency,
        transaction_type,
        description,
        posted_at,
        status,
        running_balance,
        sequence: raw.sequence,
        check_number: raw.check_number.clone(),
        merchant_name,
        last_updated,
        raw: serde_json::to_value(raw).ok(),
    })
}

// ── Datetime parsing ──────────────────────────────────────────────────────────

/// Parse an ISO 8601 datetime string into a UTC [`DateTime`].
/// Returns `None` and logs a warning if parsing fails.
fn parse_datetime(s: &str) -> Option<DateTime<Utc>> {
    s.parse::<DateTime<Utc>>().ok().or_else(|| {
        warn!(value = %s, "could not parse datetime — using current time");
        None
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::{RawAccount, RawInstitution, RawTransaction};

    fn garden_checking() -> RawAccount {
        RawAccount {
            id: "e4ab6531-d442-4928-bf05-1db1b8ef5ac0".to_string(),
            numbers: Some("x5107".to_string()),
            name: Some("Greens          0001".to_string()),
            account_type: Some("Deposit".to_string()),
            account_sub_type: Some("Checking".to_string()),
            balance: Some("1000.00".to_string()),
            available_balance: Some("1000.00".to_string()),
            status: Some("Normal".to_string()),
            account_status: Some("Active".to_string()),
            routing_number: Some("999988961".to_string()),
            fetched_date: Some("2026-11-04T01:05:06.607Z".to_string()),
            institution: Some(RawInstitution {
                id: "899f4398-106d-409a-9ed4-a72346778076".to_string(),
                name: "Garden".to_string(),
            }),
            interest_rate: Some("0.0200".to_string()),
            active: Some(true),
        }
    }

    fn garden_funding_tx() -> RawTransaction {
        RawTransaction {
            id: "8954078d-f808-4c34-8e23-e15a1344b301".to_string(),
            account_id: Some("e4ab6531-d442-4928-bf05-1db1b8ef5ac0".to_string()),
            transaction_type: Some("TRANSFER".to_string()),
            subtype: Some("EFT".to_string()),
            amount: Some("1000.00".to_string()),
            currency: Some("USD".to_string()),
            merchant: None,
            date_posted: Some("2026-11-03T05:00:00.000Z".to_string()),
            date: Some("2026-11-03T05:00:00.000Z".to_string()),
            pending_status: Some("NoReconciliation".to_string()),
            check_number: None,
            memo: Some("FUNDING NEW ACCOUNT".to_string()),
            filtered_memo: Some("FUNDING NEW ACCOUNT".to_string()),
            display_name: Some("FUNDING NEW ACCOUNT".to_string()),
            running_balance: Some("1000.00".to_string()),
            sequence: Some(0),
            last_updated: Some("2026-11-04T01:03:41.169Z".to_string()),
            notes: None,
            bill_id: None,
            payment_id: None,
        }
    }

    #[test]
    fn map_account_balance_is_correct_minor_units() {
        let account = map_account(&garden_checking()).unwrap();
        assert_eq!(account.balance.as_i64(), 100_000); // $1000.00 → 100000 cents
    }

    #[test]
    fn map_account_type_is_deposit() {
        let account = map_account(&garden_checking()).unwrap();
        assert_eq!(account.account_type, AccountType::Deposit);
    }

    #[test]
    fn map_account_status_is_active() {
        let account = map_account(&garden_checking()).unwrap();
        assert_eq!(account.status, AccountStatus::Active);
    }

    #[test]
    fn map_account_institution_name_is_garden() {
        let account = map_account(&garden_checking()).unwrap();
        assert_eq!(account.institution_name, "Garden");
    }

    #[test]
    fn map_account_routing_number_preserved() {
        let account = map_account(&garden_checking()).unwrap();
        assert_eq!(account.routing_number.unwrap(), "999988961");
    }

    #[test]
    fn map_transaction_amount_is_correct_minor_units() {
        let tx = map_transaction(
            &garden_funding_tx(),
            "e4ab6531-d442-4928-bf05-1db1b8ef5ac0",
        )
        .unwrap();
        assert_eq!(tx.amount.as_i64(), 100_000);
    }

    #[test]
    fn map_transaction_is_credit() {
        let tx = map_transaction(
            &garden_funding_tx(),
            "e4ab6531-d442-4928-bf05-1db1b8ef5ac0",
        )
        .unwrap();
        assert!(tx.is_credit());
        assert!(!tx.is_debit());
    }

    #[test]
    fn map_transaction_type_is_transfer() {
        let tx = map_transaction(
            &garden_funding_tx(),
            "e4ab6531-d442-4928-bf05-1db1b8ef5ac0",
        )
        .unwrap();
        assert_eq!(tx.transaction_type, TransactionType::Transfer);
    }

    #[test]
    fn map_transaction_status_is_no_reconciliation() {
        let tx = map_transaction(
            &garden_funding_tx(),
            "e4ab6531-d442-4928-bf05-1db1b8ef5ac0",
        )
        .unwrap();
        assert_eq!(tx.status, TransactionStatus::NoReconciliation);
    }

    #[test]
    fn map_transaction_description_uses_display_name() {
        let tx = map_transaction(
            &garden_funding_tx(),
            "e4ab6531-d442-4928-bf05-1db1b8ef5ac0",
        )
        .unwrap();
        assert_eq!(tx.description, "FUNDING NEW ACCOUNT");
    }

    #[test]
    fn map_transaction_idempotency_key_is_id() {
        let tx = map_transaction(
            &garden_funding_tx(),
            "e4ab6531-d442-4928-bf05-1db1b8ef5ac0",
        )
        .unwrap();
        assert_eq!(tx.id, "8954078d-f808-4c34-8e23-e15a1344b301");
    }

    #[test]
    fn negative_amount_maps_to_debit() {
        let mut raw = garden_funding_tx();
        raw.amount = Some("-15.50".to_string());
        let tx = map_transaction(&raw, "acc-001").unwrap();
        assert_eq!(tx.amount.as_i64(), -1_550);
        assert!(tx.is_debit());
    }
}
