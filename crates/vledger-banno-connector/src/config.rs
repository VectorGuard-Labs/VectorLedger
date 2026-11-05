//! Configuration for the Banno connector.

use serde::{Deserialize, Serialize};

/// Base URL for the Garden test institution.
pub const GARDEN_BASE_URL: &str = "https://digital.garden-fi.com";

/// OAuth token endpoint path.
pub const TOKEN_PATH: &str = "/a/consumer/api/v0/oidc/token";

/// OAuth authorization endpoint path.
pub const AUTH_PATH: &str = "/a/consumer/api/v0/oidc/auth";

/// Consumer API base path.
pub const API_PATH: &str = "/a/consumer/api/v0";

/// OAuth scopes required for accounts and transactions.
pub const REQUIRED_SCOPES: &str = "openid \
    https://api.banno.com/consumer/auth/accounts.readonly \
    https://api.banno.com/consumer/auth/transactions.detail.readonly";

/// Configuration for the Banno Consumer API connector.
///
/// In production, load from environment variables via [`BannoConfig::from_env`].
/// In tests, construct directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BannoConfig {
    /// OAuth client ID from the Jack Henry developer dashboard.
    pub client_id: String,

    /// OAuth client secret. Never commit this value.
    pub client_secret: String,

    /// Redirect URI registered in the developer dashboard.
    /// Must match exactly — including scheme, host, port, and path.
    pub redirect_uri: String,

    /// Base URL of the Banno environment.
    /// Defaults to the Garden test institution.
    pub base_url: String,

    /// Local port to listen on for the OAuth redirect during interactive auth.
    /// Defaults to 8080.
    pub redirect_port: u16,

    /// The user ID to use for API calls.
    /// Set automatically after authentication from the identity token `sub` claim.
    pub user_id: Option<String>,
}

impl BannoConfig {
    /// Load configuration from environment variables.
    ///
    /// Required env vars:
    /// - `BANNO_CLIENT_ID`
    /// - `BANNO_CLIENT_SECRET`
    ///
    /// Optional env vars (with defaults):
    /// - `BANNO_REDIRECT_URI` (default: `http://localhost:8080/dynamic`)
    /// - `BANNO_BASE_URL` (default: Garden test institution)
    /// - `BANNO_REDIRECT_PORT` (default: `8080`)
    pub fn from_env() -> Result<Self, String> {
        let client_id = std::env::var("BANNO_CLIENT_ID")
            .map_err(|_| "BANNO_CLIENT_ID environment variable not set".to_string())?;
        let client_secret = std::env::var("BANNO_CLIENT_SECRET")
            .map_err(|_| "BANNO_CLIENT_SECRET environment variable not set".to_string())?;

        let redirect_uri = std::env::var("BANNO_REDIRECT_URI")
            .unwrap_or_else(|_| "http://localhost:8080/dynamic".to_string());
        let base_url = std::env::var("BANNO_BASE_URL")
            .unwrap_or_else(|_| GARDEN_BASE_URL.to_string());
        let redirect_port = std::env::var("BANNO_REDIRECT_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8080u16);

        Ok(BannoConfig {
            client_id,
            client_secret,
            redirect_uri,
            base_url,
            redirect_port,
            user_id: None,
        })
    }

    /// Construct the full token endpoint URL.
    pub fn token_url(&self) -> String {
        format!("{}{}", self.base_url, TOKEN_PATH)
    }

    /// Construct the full authorization endpoint URL.
    pub fn auth_url(&self) -> String {
        format!("{}{}", self.base_url, AUTH_PATH)
    }

    /// Construct the full API base URL.
    pub fn api_base(&self) -> String {
        format!("{}{}", self.base_url, API_PATH)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_constructed_correctly_for_garden() {
        let config = BannoConfig {
            client_id: "test-id".to_string(),
            client_secret: "test-secret".to_string(),
            redirect_uri: "http://localhost:8080/dynamic".to_string(),
            base_url: GARDEN_BASE_URL.to_string(),
            redirect_port: 8080,
            user_id: None,
        };
        assert_eq!(
            config.token_url(),
            "https://digital.garden-fi.com/a/consumer/api/v0/oidc/token"
        );
        assert_eq!(
            config.auth_url(),
            "https://digital.garden-fi.com/a/consumer/api/v0/oidc/auth"
        );
        assert_eq!(
            config.api_base(),
            "https://digital.garden-fi.com/a/consumer/api/v0"
        );
    }
}
