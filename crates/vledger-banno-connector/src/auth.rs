//! OAuth 2.0 Authorization Code + PKCE implementation for the Banno API.
//!
//! This module handles:
//! - PKCE code verifier and challenge generation
//! - Authorization URL construction
//! - Local redirect server to catch the authorization code
//! - Token exchange (code → access_token + id_token)
//! - User ID extraction from the identity token

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tracing::{debug, info};

use crate::config::{BannoConfig, REQUIRED_SCOPES};
use vledger_banking_core::CoreBankingError;

// ── PKCE ─────────────────────────────────────────────────────────────────────

/// A PKCE code verifier and its derived challenge.
#[derive(Debug, Clone)]
pub struct PkceChallenge {
    /// The random secret — sent in the token exchange request.
    pub verifier: String,
    /// SHA-256(verifier), base64url-encoded — sent in the auth URL.
    pub challenge: String,
}

impl PkceChallenge {
    /// Generate a fresh, cryptographically random PKCE pair.
    pub fn generate() -> Self {
        // Generate 60 random bytes → hex string (120 chars) → take first 80
        let mut bytes = [0u8; 60];
        rand::rngs::OsRng::default().fill_bytes(&mut bytes);
        let verifier: String = bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
            .chars()
            .take(80)
            .collect();

        let challenge = Self::derive_challenge(&verifier);
        PkceChallenge { verifier, challenge }
    }

    /// Derive the challenge from a verifier string.
    pub fn derive_challenge(verifier: &str) -> String {
        let hash = Sha256::digest(verifier.as_bytes());
        URL_SAFE_NO_PAD.encode(hash)
    }
}

// ── Token response ────────────────────────────────────────────────────────────

/// Successful token response from the Banno token endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub id_token: String,
    pub expires_in: u64,
    pub scope: String,
    pub token_type: String,
}

/// Stored OAuth session — access token plus the user ID extracted from the
/// identity token.
#[derive(Debug, Clone)]
pub struct OAuthSession {
    pub access_token: String,
    pub user_id: String,
    /// Seconds from Unix epoch when the access token expires.
    pub expires_at: u64,
}

impl OAuthSession {
    /// Returns true if the access token is expired or within 60 seconds of
    /// expiry.
    pub fn is_expired(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now + 60 >= self.expires_at
    }
}

// ── Authorization URL ─────────────────────────────────────────────────────────

/// Build the authorization URL the user must visit in their browser.
pub fn build_auth_url(config: &BannoConfig, pkce: &PkceChallenge, state: &str) -> String {
    let params = [
        ("client_id", config.client_id.as_str()),
        ("redirect_uri", config.redirect_uri.as_str()),
        ("scope", REQUIRED_SCOPES),
        ("response_type", "code"),
        ("state", state),
        ("code_challenge", pkce.challenge.as_str()),
        ("code_challenge_method", "S256"),
    ];

    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlenccode(v)))
        .collect::<Vec<_>>()
        .join("&");

    format!("{}?{}", config.auth_url(), query)
}

/// Minimal URL percent-encoder for OAuth parameter values.
fn urlenccode(s: &str) -> String {
    let mut encoded = String::new();
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' => encoded.push(byte as char),
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

// ── Redirect server ───────────────────────────────────────────────────────────

/// Start a one-shot TCP listener on `localhost:{port}` and wait for the OAuth
/// redirect.  Returns the `code` query parameter.
///
/// This is a minimal synchronous implementation — it reads one HTTP request
/// and closes the connection.
pub fn wait_for_redirect(port: u16, expected_state: &str) -> Result<String, CoreBankingError> {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
        .map_err(|e| CoreBankingError::Network(format!("cannot bind to port {port}: {e}")))?;

    info!(port, "Waiting for OAuth redirect...");

    let (mut stream, _) = listener
        .accept()
        .map_err(|e| CoreBankingError::Network(format!("accept error: {e}")))?;

    // Read the HTTP request line
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| {
        CoreBankingError::Network(format!("stream clone error: {e}"))
    })?);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| CoreBankingError::Network(format!("read error: {e}")))?;

    debug!(request_line = %request_line.trim(), "Received redirect");

    // Send a minimal HTTP 200 response so the browser shows something
    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nAuthenticated. You can close this tab.";
    stream.write_all(response.as_bytes()).ok();

    // Parse query parameters from the request line
    // Format: GET /dynamic?code=XXX&state=YYY HTTP/1.1
    let params = parse_query_from_request_line(&request_line)?;

    // Verify state to prevent CSRF
    let returned_state = params.get("state").map(String::as_str).unwrap_or("");
    if returned_state != expected_state {
        return Err(CoreBankingError::Auth(format!(
            "state mismatch: expected '{expected_state}', got '{returned_state}'"
        )));
    }

    params
        .get("code")
        .cloned()
        .ok_or_else(|| CoreBankingError::Auth("no authorization code in redirect".to_string()))
}

/// Parse query parameters from an HTTP request line.
/// e.g. `GET /dynamic?code=ABC&state=XYZ HTTP/1.1`
fn parse_query_from_request_line(
    line: &str,
) -> Result<HashMap<String, String>, CoreBankingError> {
    let path = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| CoreBankingError::Auth("malformed HTTP request line".to_string()))?;

    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");

    let mut params = HashMap::new();
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            params.insert(k.to_string(), urldecode(v));
        }
    }
    Ok(params)
}

/// Minimal URL percent-decoder.
fn urldecode(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let h1 = chars.next().unwrap_or('0');
            let h2 = chars.next().unwrap_or('0');
            if let Ok(byte) = u8::from_str_radix(&format!("{h1}{h2}"), 16) {
                result.push(byte as char);
            }
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }
    result
}

// ── Token exchange ────────────────────────────────────────────────────────────

/// Exchange an authorization code for an access token and identity token.
pub async fn exchange_code(
    client: &reqwest::Client,
    config: &BannoConfig,
    code: &str,
    pkce: &PkceChallenge,
) -> Result<TokenResponse, CoreBankingError> {
    let params = [
        ("client_id", config.client_id.as_str()),
        ("client_secret", config.client_secret.as_str()),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", config.redirect_uri.as_str()),
        ("code_verifier", pkce.verifier.as_str()),
    ];

    let resp = client
        .post(&config.token_url())
        .form(&params)
        .send()
        .await
        .map_err(|e| CoreBankingError::Network(e.to_string()))?;

    let status = resp.status().as_u16();
    if status != 200 {
        let body = resp.text().await.unwrap_or_default();
        return Err(CoreBankingError::Api { status, body });
    }

    resp.json::<TokenResponse>()
        .await
        .map_err(|e| CoreBankingError::Parse {
            field: "token_response",
            reason: e.to_string(),
        })
}

// ── Identity token parsing ────────────────────────────────────────────────────

/// Extract the `sub` claim (user ID) from a JWT identity token without
/// verifying the signature.  Signature verification is handled by the
/// Banno authorization server — we trust the token we just received over TLS.
pub fn extract_user_id(id_token: &str) -> Result<String, CoreBankingError> {
    // JWT format: header.payload.signature (all base64url-encoded)
    let payload_b64 = id_token
        .split('.')
        .nth(1)
        .ok_or_else(|| CoreBankingError::Auth("malformed identity token".to_string()))?;

    // Add padding if needed
    let padding = (4 - payload_b64.len() % 4) % 4;
    let padded = format!("{}{}", payload_b64, "=".repeat(padding));

    let decoded = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .or_else(|_| {
            base64::engine::general_purpose::STANDARD.decode(&padded)
        })
        .map_err(|e| CoreBankingError::Auth(format!("base64 decode error: {e}")))?;

    let claims: serde_json::Value = serde_json::from_slice(&decoded)
        .map_err(|e| CoreBankingError::Auth(format!("JWT payload parse error: {e}")))?;

    claims["sub"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| CoreBankingError::Auth("'sub' claim missing from identity token".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_correct_for_known_verifier() {
        // Verified against the Jack Henry portal's built-in checker
        let verifier = "e517c32aee2356891326604e79ad7d358154e124c157d762cbc8896fb13bfbc5d93a335cc27df714a9280e8249cbc3507143b3b7829d3fe9f62b9fce";
        let challenge = PkceChallenge::derive_challenge(verifier);
        assert_eq!(challenge, "4lKn4LVhzJzjx_BttEPuMcracgFKVKbTMmSKYAvA24Y");
    }

    #[test]
    fn pkce_generate_produces_valid_verifier_length() {
        let pkce = PkceChallenge::generate();
        assert!(pkce.verifier.len() >= 43, "verifier too short");
        assert!(pkce.verifier.len() <= 128, "verifier too long");
    }

    #[test]
    fn pkce_challenge_is_43_plus_chars() {
        let pkce = PkceChallenge::generate();
        assert!(pkce.challenge.len() >= 43);
    }

    #[test]
    fn extract_user_id_from_known_token() {
        // The id_token payload we saw in the actual Garden response
        let id_token = "eyJhbGciOiJFUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6InNpZy1lYzItMCJ9.\
            eyJzdWIiOiI3NWI2ODMwZC1kNmFiLTRlODktOTNkNi1hOWQ5YWM3NzUwM2MiLCJhdF9oYXNoIjoiNl8yTVVDUkhmdlB5Q25vaVZTaGV1ZyIsImF1ZCI6IjBjMjRmN2Q0LTQyNGYtNGM0YS1hMDNlLTYxNWRjN2NmZGIzYyIsImV4cCI6MTc5Mzc2MTQxNCwiaWF0IjoxNzkzNzU3ODE0LCJpc3MiOiJodHRwczovL2RpZ2l0YWwuZ2FyZGVuLWZpLmNvbS9hL2NvbnN1bWVyL2FwaS92MC9vaWRjIn0.\
            QQrSZCY_PaHoaYOdaz8jGieocwAT2oHUoDqQwCFjf46pP6mkGP-Y63RL4qcjpYrhs30LtbpIXbLWe2P7jv7NNA";
        let user_id = extract_user_id(id_token).unwrap();
        assert_eq!(user_id, "75b6830d-d6ab-4e89-93d6-a9d9ac77503c");
    }

    #[test]
    fn parse_query_extracts_code_and_state() {
        let line = "GET /dynamic?code=test_code_123&state=abc&iss=https%3A%2F%2Fexample.com HTTP/1.1";
        let params = parse_query_from_request_line(line).unwrap();
        assert_eq!(params.get("code").unwrap(), "test_code_123");
        assert_eq!(params.get("state").unwrap(), "abc");
    }

    #[test]
    fn urldecode_percent_encoded() {
        assert_eq!(urldecode("hello%20world"), "hello world");
        assert_eq!(urldecode("a%2Bb"), "a+b");
    }

    #[test]
    fn session_is_expired_when_past_expiry() {
        let session = OAuthSession {
            access_token: "tok".to_string(),
            user_id: "user".to_string(),
            expires_at: 0, // epoch — always expired
        };
        assert!(session.is_expired());
    }

    #[test]
    fn session_is_not_expired_when_far_future() {
        let session = OAuthSession {
            access_token: "tok".to_string(),
            user_id: "user".to_string(),
            expires_at: u64::MAX,
        };
        assert!(!session.is_expired());
    }
}
