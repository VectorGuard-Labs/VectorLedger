//! `vledger-banno-connector` — Jack Henry Banno Consumer API connector.
//!
//! Implements [`CoreBankingConnector`] for the Jack Henry Banno Consumer API,
//! mapping Jack Henry's JSON responses into the normalized types defined in
//! `vledger-banking-core`.
//!
//! # Authentication
//!
//! The Banno Consumer API uses OAuth 2.0 Authorization Code flow with PKCE.
//! Because this flow requires a browser redirect, the connector supports two
//! authentication modes:
//!
//! - **Interactive** (`BannoConnector::authenticate`) — opens the auth URL,
//!   starts a local HTTP server to catch the redirect, exchanges the code for
//!   tokens, and stores them internally.
//! - **Token injection** (`BannoConnector::with_token`) — for testing and
//!   scripted use: provide an access token directly.
//!
//! # Usage
//!
//! ```no_run
//! use vledger_banno_connector::{BannoConnector, BannoConfig};
//! use vledger_banking_core::{CoreBankingConnector, types::{AccountQuery, TransactionQuery}};
//!
//! async fn example() {
//!     let config = BannoConfig::from_env().unwrap();
//!     let mut connector = BannoConnector::new(config);
//!     connector.authenticate().await.unwrap();
//!
//!     let accounts = connector.fetch_accounts(&AccountQuery::default()).await.unwrap();
//!     for account in &accounts {
//!         let txns = connector.fetch_transactions(&TransactionQuery {
//!             account_id: account.id.clone(),
//!             ..Default::default()
//!         }).await.unwrap();
//!         println!("{}: {} transactions", account.name, txns.len());
//!     }
//! }
//! ```

pub mod config;
pub mod auth;
pub mod client;
pub mod connector;
pub mod mapping;
pub mod response;

pub use config::BannoConfig;
pub use connector::BannoConnector;
