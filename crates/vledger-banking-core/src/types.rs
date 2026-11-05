//! Normalized types shared across all banking core connectors.
//!
//! These types are defined from VectorLedger's perspective — what the ledger
//! engine needs — not from any particular banking core's perspective.  Every
//! connector maps its source system's fields into these types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Currency ──────────────────────────────────────────────────────────────────

/// ISO 4217 currency code (e.g. "USD", "EUR", "GBP").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Currency(pub String);

impl Currency {
    /// Returns the number of decimal places (minor unit precision) for this
    /// currency.  Defaults to 2 for unknown currencies (safe for most fiat).
    pub fn minor_unit_precision(&self) -> u32 {
        match self.0.to_uppercase().as_str() {
            "USD" | "EUR" | "GBP" | "CAD" | "AUD" | "NZD" => 2,
            "JPY" | "KRW" | "VND" | "CLP"                 => 0,
            "BTC"                                           => 8,
            "ETH"                                           => 18,
            _                                               => 2,
        }
    }
}

impl std::fmt::Display for Currency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── MinorUnits ────────────────────────────────────────────────────────────────

/// An amount expressed as an integer in the currency's minor units.
///
/// VectorLedger stores all amounts as `i64` minor units — never as decimals.
/// $10.00 USD = `MinorUnits(1000)`.  -$5.50 USD = `MinorUnits(-550)`.
///
/// # Conversion from a decimal string
///
/// Use [`MinorUnits::from_decimal_str`] to parse the string values returned
/// by the Jack Henry Consumer API (e.g. `"1000.00"`, `"-15.50"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MinorUnits(pub i64);

impl MinorUnits {
    /// Parse a decimal string (e.g. `"1000.00"`, `"-15.50"`) into minor units
    /// for the given currency precision.
    ///
    /// # Errors
    /// Returns an error string if the value cannot be parsed or overflows
    /// `i64`.
    pub fn from_decimal_str(
        raw: &str,
        precision: u32,
    ) -> Result<Self, String> {
        // Strip whitespace
        let s = raw.trim();

        // Determine sign
        let (negative, s) = if let Some(stripped) = s.strip_prefix('-') {
            (true, stripped)
        } else {
            (false, s.strip_prefix('+').unwrap_or(s))
        };

        // Split on decimal point
        let (integer_part, fractional_part) = match s.split_once('.') {
            Some((i, f)) => (i, f),
            None => (s, ""),
        };

        // Parse integer part
        let integer: i64 = integer_part
            .parse()
            .map_err(|_| format!("invalid integer part '{integer_part}'"))?;

        // Pad or truncate fractional part to match precision
        let frac_str = format!("{:0<width$}", fractional_part, width = precision as usize);
        let frac_str = &frac_str[..precision as usize];

        let fractional: i64 = if precision == 0 {
            0
        } else {
            frac_str
                .parse()
                .map_err(|_| format!("invalid fractional part '{frac_str}'"))?
        };

        // Combine
        let multiplier: i64 = 10i64.pow(precision);
        let value = integer
            .checked_mul(multiplier)
            .and_then(|v| v.checked_add(fractional))
            .ok_or_else(|| format!("amount '{raw}' overflows i64"))?;

        Ok(MinorUnits(if negative { -value } else { value }))
    }

    /// Return the absolute (unsigned) value.
    pub fn abs(self) -> Self {
        MinorUnits(self.0.abs())
    }

    /// Return true if the amount is negative.
    pub fn is_negative(self) -> bool {
        self.0 < 0
    }

    /// Return the raw i64 value for posting to VectorLedger.
    pub fn as_i64(self) -> i64 {
        self.0
    }
}

impl std::fmt::Display for MinorUnits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── Account types ─────────────────────────────────────────────────────────────

/// Broad account classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum AccountType {
    /// Standard deposit account (checking, savings, money market).
    Deposit,
    /// Loan or line of credit.
    Loan,
    /// Investment or brokerage account.
    Investment,
    /// Any other account type not covered above.
    Other(String),
}

impl std::fmt::Display for AccountType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccountType::Deposit    => write!(f, "Deposit"),
            AccountType::Loan       => write!(f, "Loan"),
            AccountType::Investment => write!(f, "Investment"),
            AccountType::Other(s)   => write!(f, "Other({s})"),
        }
    }
}

/// Account lifecycle status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountStatus {
    Active,
    Closed,
    Frozen,
    Unknown,
}

/// A normalized account record.
///
/// Every connector maps its source system's account object into this type.
/// Fields that have no equivalent in the source system are set to `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedAccount {
    /// Unique account identifier from the source system (used as the
    /// VectorLedger account code and for transaction queries).
    pub id: String,

    /// Masked account number for display (e.g. `"x5107"`).
    pub masked_number: Option<String>,

    /// Human-readable account name (e.g. `"Greens 0001"`).
    pub name: String,

    /// Broad account type.
    pub account_type: AccountType,

    /// More specific subtype (e.g. `"Checking"`, `"Savings"`).
    pub account_sub_type: Option<String>,

    /// Current balance in minor units.
    pub balance: MinorUnits,

    /// Available balance in minor units.
    pub available_balance: Option<MinorUnits>,

    /// Account currency.
    pub currency: Currency,

    /// Account lifecycle status.
    pub status: AccountStatus,

    /// Routing number (ABA) for the institution.
    pub routing_number: Option<String>,

    /// Institution identifier (maps to VectorLedger domain).
    pub institution_id: String,

    /// Institution name.
    pub institution_name: String,

    /// When this record was fetched from the source system.
    pub fetched_at: DateTime<Utc>,

    /// Raw source data preserved for debugging and audit purposes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

// ── Transaction types ─────────────────────────────────────────────────────────

/// Transaction type classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionType {
    Pos,
    Check,
    ECheck,
    Fee,
    Interest,
    Tax,
    Transfer,
    Online,
    Atm,
    Unknown,
}

impl From<&str> for TransactionType {
    fn from(s: &str) -> Self {
        match s.to_uppercase().as_str() {
            "POS"      => TransactionType::Pos,
            "CHECK"    => TransactionType::Check,
            "ECHECK"   => TransactionType::ECheck,
            "FEE"      => TransactionType::Fee,
            "INTEREST" => TransactionType::Interest,
            "TAX"      => TransactionType::Tax,
            "TRANSFER" => TransactionType::Transfer,
            "ONLINE"   => TransactionType::Online,
            "ATM"      => TransactionType::Atm,
            _          => TransactionType::Unknown,
        }
    }
}

impl std::fmt::Display for TransactionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TransactionType::Pos      => "POS",
            TransactionType::Check    => "CHECK",
            TransactionType::ECheck   => "ECHECK",
            TransactionType::Fee      => "FEE",
            TransactionType::Interest => "INTEREST",
            TransactionType::Tax      => "TAX",
            TransactionType::Transfer => "TRANSFER",
            TransactionType::Online   => "ONLINE",
            TransactionType::Atm      => "ATM",
            TransactionType::Unknown  => "UNKNOWN",
        };
        write!(f, "{s}")
    }
}

/// Settlement status of a transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionStatus {
    /// Transaction posted and settled.
    Posted,
    /// Transaction is pending at the institution.
    Pending,
    /// Previously pending, now confirmed posted.
    Reconciled,
    /// Transaction was never pending — posted directly.
    NoReconciliation,
}

impl From<&str> for TransactionStatus {
    fn from(s: &str) -> Self {
        match s {
            "Reconciled"      => TransactionStatus::Reconciled,
            "NoReconciliation"=> TransactionStatus::NoReconciliation,
            "InstitutionPending" | "BannoPending" => TransactionStatus::Pending,
            _                 => TransactionStatus::Posted,
        }
    }
}

/// A normalized transaction record.
///
/// Every connector maps its source system's transaction object into this type.
/// This is what gets posted to VectorLedger as a journal entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedTransaction {
    /// Unique transaction identifier from the source system.
    /// Used as the VectorLedger idempotency key — guarantees exactly-once
    /// posting regardless of how many times the connector runs.
    pub id: String,

    /// The account this transaction belongs to.
    pub account_id: String,

    /// Amount in minor units.  Positive = credit to account.
    /// Negative = debit from account.
    pub amount: MinorUnits,

    /// Transaction currency.
    pub currency: Currency,

    /// Transaction type.
    pub transaction_type: TransactionType,

    /// Human-readable description (memo, display name, or filtered memo).
    pub description: String,

    /// When the transaction was posted at the institution.
    pub posted_at: DateTime<Utc>,

    /// Settlement status.
    pub status: TransactionStatus,

    /// Running balance after this transaction, in minor units.
    pub running_balance: Option<MinorUnits>,

    /// Source system sequence number within the account.
    pub sequence: Option<i64>,

    /// Check number, if applicable.
    pub check_number: Option<String>,

    /// Merchant name, if available.
    pub merchant_name: Option<String>,

    /// When this record was last updated in the source system.
    pub last_updated: DateTime<Utc>,

    /// Raw source data preserved for debugging and audit purposes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

impl NormalizedTransaction {
    /// Returns true if this transaction is a credit to the account
    /// (positive amount on a deposit account).
    pub fn is_credit(&self) -> bool {
        self.amount.0 >= 0
    }

    /// Returns true if this transaction is a debit from the account.
    pub fn is_debit(&self) -> bool {
        self.amount.0 < 0
    }

    /// Returns the absolute amount regardless of sign.
    pub fn absolute_amount(&self) -> MinorUnits {
        self.amount.abs()
    }
}

// ── Query parameters ──────────────────────────────────────────────────────────

/// Parameters for querying accounts from a connector.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountQuery {
    /// If set, return only accounts for this user ID.
    pub user_id: Option<String>,
    /// If true, include inactive/closed accounts.
    pub include_inactive: bool,
}

/// Parameters for querying transactions from a connector.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransactionQuery {
    /// The account to query transactions for.
    pub account_id: String,
    /// Return only transactions posted on or after this instant.
    pub since: Option<DateTime<Utc>>,
    /// Return only transactions posted on or before this instant.
    pub until: Option<DateTime<Utc>>,
    /// Maximum number of transactions to return (connector may cap this).
    pub limit: Option<usize>,
    /// Offset for pagination.
    pub offset: Option<usize>,
}

// ── Connector health ──────────────────────────────────────────────────────────

/// Status reported by a connector's health check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorStatus {
    /// True if the connector can reach the source system.
    pub healthy: bool,
    /// Human-readable status message.
    pub message: String,
    /// Name of the connector (e.g. `"vledger-banno-connector"`).
    pub connector_name: String,
    /// Name of the source system (e.g. `"Jack Henry Garden"`).
    pub source_system: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usd_positive_parses_correctly() {
        let m = MinorUnits::from_decimal_str("1000.00", 2).unwrap();
        assert_eq!(m.as_i64(), 100_000);
    }

    #[test]
    fn usd_negative_parses_correctly() {
        let m = MinorUnits::from_decimal_str("-15.50", 2).unwrap();
        assert_eq!(m.as_i64(), -1_550);
    }

    #[test]
    fn no_decimal_point_parses_correctly() {
        let m = MinorUnits::from_decimal_str("100", 2).unwrap();
        assert_eq!(m.as_i64(), 10_000);
    }

    #[test]
    fn single_decimal_place_padded_correctly() {
        let m = MinorUnits::from_decimal_str("10.5", 2).unwrap();
        assert_eq!(m.as_i64(), 1_050);
    }

    #[test]
    fn zero_precision_currency_ignores_fractional() {
        let m = MinorUnits::from_decimal_str("1000.99", 0).unwrap();
        assert_eq!(m.as_i64(), 1_000);
    }

    #[test]
    fn zero_amount_parses_correctly() {
        let m = MinorUnits::from_decimal_str("0.00", 2).unwrap();
        assert_eq!(m.as_i64(), 0);
    }

    #[test]
    fn abs_of_negative_is_positive() {
        let m = MinorUnits(-550);
        assert_eq!(m.abs().as_i64(), 550);
    }

    #[test]
    fn is_negative_and_is_credit() {
        assert!(MinorUnits(-100).is_negative());
        assert!(!MinorUnits(100).is_negative());
    }

    #[test]
    fn usd_precision_is_two() {
        assert_eq!(Currency("USD".to_string()).minor_unit_precision(), 2);
    }

    #[test]
    fn jpy_precision_is_zero() {
        assert_eq!(Currency("JPY".to_string()).minor_unit_precision(), 0);
    }

    #[test]
    fn transaction_type_from_str() {
        assert_eq!(TransactionType::from("TRANSFER"), TransactionType::Transfer);
        assert_eq!(TransactionType::from("POS"), TransactionType::Pos);
        assert_eq!(TransactionType::from("unknown_value"), TransactionType::Unknown);
    }

    #[test]
    fn transaction_status_from_str() {
        assert_eq!(
            TransactionStatus::from("NoReconciliation"),
            TransactionStatus::NoReconciliation
        );
        assert_eq!(
            TransactionStatus::from("Reconciled"),
            TransactionStatus::Reconciled
        );
        assert_eq!(
            TransactionStatus::from("InstitutionPending"),
            TransactionStatus::Pending
        );
    }
}
