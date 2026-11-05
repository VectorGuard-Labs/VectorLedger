//! Low-level HTTP client for the Banno Consumer API.
//!
//! Handles authenticated GET requests with automatic token expiry detection.
//! Does not handle token refresh — that is the connector's responsibility.

use tracing::debug;

use vledger_banking_core::CoreBankingError;

use crate::response::{AccountsResponse, TransactionsResponse};

/// Make an authenticated GET request and deserialize the JSON response.
async fn get_json<T>(
    client: &reqwest::Client,
    url: &str,
    access_token: &str,
) -> Result<T, CoreBankingError>
where
    T: serde::de::DeserializeOwned,
{
    debug!(url = %url, "GET");

    let resp = client
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| CoreBankingError::Network(e.to_string()))?;

    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(CoreBankingError::Auth(format!(
            "HTTP {status} — token may be expired or missing required scope"
        )));
    }
    if status != 200 {
        let body = resp.text().await.unwrap_or_default();
        return Err(CoreBankingError::Api { status, body });
    }

    resp.json::<T>()
        .await
        .map_err(|e| CoreBankingError::Parse {
            field: "response_body",
            reason: e.to_string(),
        })
}

/// Fetch all accounts for a user.
pub async fn fetch_accounts(
    client: &reqwest::Client,
    api_base: &str,
    user_id: &str,
    access_token: &str,
) -> Result<AccountsResponse, CoreBankingError> {
    let url = format!("{api_base}/users/{user_id}/accounts");
    get_json(client, &url, access_token).await
}

/// Fetch transactions for an account, with optional date range and pagination.
pub async fn fetch_transactions(
    client: &reqwest::Client,
    api_base: &str,
    user_id: &str,
    account_id: &str,
    access_token: &str,
    since: Option<&str>,
    until: Option<&str>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<TransactionsResponse, CoreBankingError> {
    let mut url = format!("{api_base}/users/{user_id}/accounts/{account_id}/transactions");

    let mut params: Vec<String> = Vec::new();
    if let Some(s) = since  { params.push(format!("since={s}")); }
    if let Some(u) = until  { params.push(format!("until={u}")); }
    if let Some(l) = limit  { params.push(format!("limit={l}")); }
    if let Some(o) = offset { params.push(format!("offset={o}")); }

    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }

    get_json(client, &url, access_token).await
}
