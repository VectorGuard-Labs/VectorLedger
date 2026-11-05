//! Raw JSON response types for the Banno Consumer API.
//!
//! These types mirror the exact JSON structure returned by the API.
//! They are deserialized first, then mapped into the normalized types
//! in `mapping.rs`.  Keeping them separate means the mapping logic is
//! easy to test independently of the HTTP layer.

use serde::{Deserialize, Serialize};

// ── Accounts ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
pub struct AccountsResponse {
    pub accounts: Vec<RawAccount>,
    #[serde(rename = "inactivatedAccountIds", default)]
    pub inactivated_account_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RawAccount {
    pub id: String,
    pub numbers: Option<String>,
    pub name: Option<String>,
    #[serde(rename = "accountType")]
    pub account_type: Option<String>,
    #[serde(rename = "accountSubType")]
    pub account_sub_type: Option<String>,
    pub balance: Option<String>,
    #[serde(rename = "availableBalance")]
    pub available_balance: Option<String>,
    pub status: Option<String>,
    #[serde(rename = "accountStatus")]
    pub account_status: Option<String>,
    #[serde(rename = "routingNumber")]
    pub routing_number: Option<String>,
    #[serde(rename = "fetchedDate")]
    pub fetched_date: Option<String>,
    pub institution: Option<RawInstitution>,
    #[serde(rename = "interestRate")]
    pub interest_rate: Option<String>,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RawInstitution {
    pub id: String,
    pub name: String,
}

// ── Transactions ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
pub struct TransactionsResponse {
    pub transactions: Vec<RawTransaction>,
    #[serde(rename = "inactivatedTransactionIds", default)]
    pub inactivated_transaction_ids: Vec<String>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
    pub account: Option<RawAccountSummary>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RawTransaction {
    pub id: String,
    #[serde(rename = "accountId")]
    pub account_id: Option<String>,
    #[serde(rename = "type")]
    pub transaction_type: Option<String>,
    pub subtype: Option<String>,
    pub amount: Option<String>,
    pub currency: Option<String>,
    pub merchant: Option<RawMerchant>,
    #[serde(rename = "datePosted")]
    pub date_posted: Option<String>,
    pub date: Option<String>,
    #[serde(rename = "pendingStatus")]
    pub pending_status: Option<String>,
    #[serde(rename = "checkNumber")]
    pub check_number: Option<String>,
    pub memo: Option<String>,
    #[serde(rename = "filteredMemo")]
    pub filtered_memo: Option<String>,
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(rename = "runningBalance")]
    pub running_balance: Option<String>,
    pub sequence: Option<i64>,
    #[serde(rename = "lastUpdated")]
    pub last_updated: Option<String>,
    pub notes: Option<String>,
    #[serde(rename = "billId")]
    pub bill_id: Option<String>,
    #[serde(rename = "paymentId")]
    pub payment_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RawMerchant {
    pub name: Option<String>,
    pub id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RawAccountSummary {
    #[serde(rename = "accountType")]
    pub account_type: Option<String>,
    #[serde(rename = "accountSubType")]
    pub account_sub_type: Option<String>,
}
