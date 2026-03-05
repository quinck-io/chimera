use std::sync::Arc;

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL};
use chrono::{Duration, Utc};
use rsa::RsaPrivateKey;
use rsa::pss::SigningKey;
use rsa::signature::RandomizedSigner;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::sync::RwLock;
use tracing::{debug, warn};

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("token exchange failed: {0}")]
    TokenExchange(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JwtClaims {
    pub sub: String,
    pub iss: String,
    pub jti: String,
    pub aud: String,
    pub nbf: i64,
    pub iat: i64,
    pub exp: i64,
}

/// Create a signed PS256 (RSASSA-PSS + SHA-256) JWT for OAuth token exchange.
pub fn create_jwt(
    private_key: &RsaPrivateKey,
    client_id: &str,
    authorization_url: &str,
) -> Result<String> {
    let now = Utc::now() - Duration::seconds(30);
    let claims = JwtClaims {
        sub: client_id.to_string(),
        iss: client_id.to_string(),
        jti: uuid::Uuid::new_v4().to_string(),
        aud: authorization_url.to_string(),
        nbf: now.timestamp(),
        iat: now.timestamp(),
        exp: (now + Duration::minutes(5)).timestamp(),
    };

    let header = serde_json::json!({"typ": "JWT", "alg": "PS256"});
    let header_b64 = BASE64URL.encode(header.to_string().as_bytes());

    let payload = serde_json::to_string(&claims).context("serializing JWT claims")?;
    let payload_b64 = BASE64URL.encode(payload.as_bytes());

    let message = format!("{header_b64}.{payload_b64}");
    let signing_key = SigningKey::<Sha256>::new(private_key.clone());
    let signature = signing_key.sign_with_rng(&mut rsa::rand_core::OsRng, message.as_bytes());
    let sig_b64 = BASE64URL.encode(rsa::signature::SignatureEncoding::to_bytes(&signature));

    Ok(format!("{message}.{sig_b64}"))
}

#[derive(Debug, Clone)]
struct TokenState {
    access_token: String,
    expires_at: chrono::DateTime<Utc>,
}

impl TokenState {
    fn is_valid(&self) -> bool {
        self.expires_at > Utc::now() + Duration::minutes(5)
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<i64>,
}

pub struct TokenManager {
    client: reqwest::Client,
    authorization_url: String,
    private_key: RsaPrivateKey,
    client_id: String,
    state: Arc<RwLock<Option<TokenState>>>,
}

impl TokenManager {
    pub fn new(
        client: reqwest::Client,
        authorization_url: String,
        private_key: RsaPrivateKey,
        client_id: String,
    ) -> Self {
        Self {
            client,
            authorization_url,
            private_key,
            client_id,
            state: Arc::new(RwLock::new(None)),
        }
    }

    /// Get a valid access token, refreshing if necessary.
    pub async fn get_token(&self) -> Result<String> {
        {
            let state = self.state.read().await;
            if let Some(ts) = state.as_ref()
                && ts.is_valid()
            {
                return Ok(ts.access_token.clone());
            }
        }

        self.refresh().await
    }

    /// Force a token refresh (e.g., after a 401).
    pub async fn refresh(&self) -> Result<String> {
        debug!(
            client_id = %self.client_id,
            authorization_url = %self.authorization_url,
            "refreshing OAuth token"
        );

        let jwt = create_jwt(&self.private_key, &self.client_id, &self.authorization_url)?;

        let resp = self
            .client
            .post(&self.authorization_url)
            .header(
                "Content-Type",
                "application/x-www-form-urlencoded; charset=utf-8",
            )
            .header("Accept", "application/json")
            .body(
                serde_urlencoded::to_string([
                    (
                        "client_assertion_type",
                        "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
                    ),
                    ("client_assertion", &jwt),
                    ("grant_type", "client_credentials"),
                ])
                .context("encoding form body")?,
            )
            .send()
            .await
            .map_err(|e| AuthError::TokenExchange(e.to_string()))
            .context("sending token exchange request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AuthError::TokenExchange(format!("{status}: {body}")).into());
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .map_err(|e| AuthError::TokenExchange(e.to_string()))
            .context("parsing token exchange response")?;

        let expires_in = token_resp.expires_in.unwrap_or(3600);
        let new_state = TokenState {
            access_token: token_resp.access_token.clone(),
            expires_at: Utc::now() + Duration::seconds(expires_in),
        };

        debug!(expires_in_secs = expires_in, "token refreshed successfully");

        let mut state = self.state.write().await;
        *state = Some(new_state);

        Ok(token_resp.access_token)
    }

    /// Invalidate the cached token (used when a 401 is received).
    pub async fn invalidate(&self) {
        warn!("invalidating cached token");
        let mut state = self.state.write().await;
        *state = None;
    }
}

#[cfg(test)]
#[path = "auth_test.rs"]
mod auth_test;
