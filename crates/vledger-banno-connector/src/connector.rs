//! `BannoConnector` — implements [`CoreBankingConnector`] for Jack Henry Banno.

use tracing::info;

use vledger_banking_core::{
    CoreBankingConnector, CoreBankingError,
    types::{
        AccountQuery, ConnectorStatus,
        NormalizedAccount, NormalizedTransaction,
        TransactionQuery,
    },
};

use crate::{
    auth::{self, OAuthSession, PkceChallenge},
    client,
    config::BannoConfig,
    mapping,
};

/// Jack Henry Banno Consumer API connector.
///
/// # Example
///
/// ```no_run
/// use vledger_banno_connector::{BannoConnector, BannoConfig};
/// use vledger_banking_core::{CoreBankingConnector, types::AccountQuery};
///
/// async fn example() {
///     let config = BannoConfig::from_env().unwrap();
///     let mut connector = BannoConnector::new(config);
///     connector.authenticate().await.unwrap();
///     let accounts = connector.fetch_accounts(&AccountQuery::default()).await.unwrap();
///     println!("Found {} accounts", accounts.len());
/// }
/// ```
pub struct BannoConnector {
    config: BannoConfig,
    http: reqwest::Client,
    session: Option<OAuthSession>,
}

impl BannoConnector {
    /// Create a new connector from configuration.
    /// Call [`authenticate`] before making API calls.
    ///
    /// [`authenticate`]: BannoConnector::authenticate
    pub fn new(config: BannoConfig) -> Self {
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .expect("failed to build HTTP client");

        BannoConnector {
            config,
            http,
            session: None,
        }
    }

    /// Inject a pre-existing access token and user ID directly.
    ///
    /// Useful for testing or when the token was obtained through an external
    /// OAuth flow (e.g. the Python auth.py script).
    pub fn with_token(mut self, access_token: &str, user_id: &str) -> Self {
        self.session = Some(OAuthSession {
            access_token: access_token.to_string(),
            user_id: user_id.to_string(),
            expires_at: u64::MAX, // assume valid — caller is responsible
        });
        self
    }

    /// Return the current session, or an error if not authenticated.
    fn session(&self) -> Result<&OAuthSession, CoreBankingError> {
        self.session
            .as_ref()
            .ok_or(CoreBankingError::NotAuthenticated)
    }
}

impl CoreBankingConnector for BannoConnector {
    /// Perform the interactive OAuth 2.0 Authorization Code + PKCE flow.
    ///
    /// Prints the authorization URL to stdout, starts a local HTTP server
    /// on `config.redirect_port`, waits for the redirect, exchanges the
    /// code for tokens, and stores the session internally.
    async fn authenticate(&mut self) -> Result<(), CoreBankingError> {
        let pkce = PkceChallenge::generate();
        let state = format!("vledger-{}", uuid::Uuid::new_v4().simple());

        let auth_url = auth::build_auth_url(&self.config, &pkce, &state);

        println!("\n╔══════════════════════════════════════════════════════════════╗");
        println!("║  VectorLedger — Jack Henry Authentication Required           ║");
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║  Open the following URL in your browser to authenticate:     ║");
        println!("╚══════════════════════════════════════════════════════════════╝");
        println!("\n{auth_url}\n");
        println!("Waiting for redirect on port {} ...", self.config.redirect_port);

        // Catch the redirect (blocking — spawned via spawn_blocking below)
        let port = self.config.redirect_port;
        let state_clone = state.clone();
        let code = tokio::task::spawn_blocking(move || {
            auth::wait_for_redirect(port, &state_clone)
        })
        .await
        .map_err(|e| CoreBankingError::Auth(format!("spawn_blocking error: {e}")))?
        .map_err(|e| e)?;

        info!("Authorization code received. Exchanging for token...");

        let token_resp =
            auth::exchange_code(&self.http, &self.config, &code, &pkce).await?;

        let user_id = auth::extract_user_id(&token_resp.id_token)?;

        info!(user_id = %user_id, "Authentication successful");

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.session = Some(OAuthSession {
            access_token: token_resp.access_token,
            user_id,
            expires_at: now + token_resp.expires_in,
        });

        Ok(())
    }

    /// Fetch all accounts for the authenticated user.
    async fn fetch_accounts(
        &self,
        _params: &AccountQuery,
    ) -> Result<Vec<NormalizedAccount>, CoreBankingError> {
        let session = self.session()?;

        if session.is_expired() {
            return Err(CoreBankingError::Auth(
                "access token is expired — call authenticate() again".to_string(),
            ));
        }

        let resp = client::fetch_accounts(
            &self.http,
            &self.config.api_base(),
            &session.user_id,
            &session.access_token,
        )
        .await?;

        resp.accounts
            .iter()
            .map(mapping::map_account)
            .collect()
    }

    /// Fetch transactions for a specific account.
    async fn fetch_transactions(
        &self,
        params: &TransactionQuery,
    ) -> Result<Vec<NormalizedTransaction>, CoreBankingError> {
        let session = self.session()?;

        if session.is_expired() {
            return Err(CoreBankingError::Auth(
                "access token is expired — call authenticate() again".to_string(),
            ));
        }

        let since = params
            .since
            .map(|dt| dt.to_rfc3339());
        let until = params
            .until
            .map(|dt| dt.to_rfc3339());

        let resp = client::fetch_transactions(
            &self.http,
            &self.config.api_base(),
            &session.user_id,
            &params.account_id,
            &session.access_token,
            since.as_deref(),
            until.as_deref(),
            params.limit,
            params.offset,
        )
        .await?;

        resp.transactions
            .iter()
            .map(|tx| mapping::map_transaction(tx, &params.account_id))
            .collect()
    }

    /// Check connectivity to the Banno environment.
    async fn health_check(&self) -> ConnectorStatus {
        let url = format!("{}/health", self.config.base_url);
        let healthy = self.http.get(&url).send().await.is_ok();
        ConnectorStatus {
            healthy,
            message: if healthy {
                "Banno environment reachable".to_string()
            } else {
                format!("Cannot reach {}", self.config.base_url)
            },
            connector_name: self.name().to_string(),
            source_system: self.source_system().to_string(),
        }
    }

    fn name(&self) -> &'static str {
        "vledger-banno-connector"
    }

    fn source_system(&self) -> &'static str {
        "Jack Henry Banno Consumer API"
    }
}
