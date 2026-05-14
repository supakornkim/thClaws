//! Bearer-token auth. Pulls `Authorization: Bearer <key>` out of the
//! request, hashes, and looks up in the [`crate::keys::KeyStore`].
//! Returns the resolved subject (`user_sub`) for handlers that want
//! to know who's calling.

use axum::http::HeaderMap;

use crate::error::{Error, Result};
use crate::keys::hash_key;
use crate::state::AppState;

#[derive(Debug)]
pub struct AuthedUser {
    pub user_sub: String,
}

/// Validate the request's access key. Three header schemes are
/// accepted so each desktop provider can keep its native auth shape:
///
/// - `Authorization: Bearer <key>` — OpenAI / OpenRouter clients
/// - `x-api-key: <key>` — Anthropic clients
/// - `x-goog-api-key: <key>` — Gemini clients
///
/// Returns the resolved user on hit, `Error::Unauthorized` on
/// missing / unknown / revoked.
pub async fn require_bearer(state: &AppState, headers: &HeaderMap) -> Result<AuthedUser> {
    let token = extract_access_key(headers)
        .ok_or_else(|| Error::Unauthorized("missing access key header".into()))?;
    if token.is_empty() {
        return Err(Error::Unauthorized("empty access key".into()));
    }
    let hash = hash_key(&token);
    let row = state
        .keys()
        .lookup_and_touch(&hash)
        .await
        .map_err(|e| Error::Internal(format!("key store: {e}")))?
        .ok_or_else(|| Error::Unauthorized("unknown or revoked key".into()))?;
    Ok(AuthedUser {
        user_sub: row.user_sub,
    })
}

/// Pull the access key out of whichever header the client used.
/// Tries `Authorization: Bearer …`, then `x-api-key`, then
/// `x-goog-api-key` — first non-empty value wins.
fn extract_access_key(headers: &HeaderMap) -> Option<String> {
    if let Some(raw) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        let trimmed = raw.trim();
        if let Some(rest) = trimmed
            .strip_prefix("Bearer ")
            .or_else(|| trimmed.strip_prefix("bearer "))
        {
            let token = rest.trim().to_string();
            if !token.is_empty() {
                return Some(token);
            }
        }
    }
    for hdr in ["x-api-key", "x-goog-api-key"] {
        if let Some(raw) = headers.get(hdr).and_then(|v| v.to_str().ok()) {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::keys::mint_key;

    fn test_config() -> Config {
        Config {
            bind: "127.0.0.1:0".into(),
            database_url: None,
            google_client_id: None,
            azure_client_id: None,
            openai_api_key: None,
            anthropic_api_key: None,
            google_api_key: None,
            openrouter_api_key: None,
            tavily_api_key: None,
            hal_api_key: None,
            openai_base_url: "https://example.invalid".into(),
            anthropic_base_url: "https://example.invalid".into(),
            google_base_url: "https://example.invalid".into(),
            openrouter_base_url: "https://example.invalid".into(),
            tavily_base_url: "https://example.invalid".into(),
            hal_base_url: "https://example.invalid".into(),
        }
    }

    #[tokio::test]
    async fn missing_header_returns_unauthorized() {
        let state = AppState::with_memory(test_config());
        let headers = HeaderMap::new();
        let err = require_bearer(&state, &headers).await.unwrap_err();
        assert!(matches!(err, Error::Unauthorized(_)));
    }

    #[tokio::test]
    async fn non_bearer_scheme_returns_unauthorized() {
        let state = AppState::with_memory(test_config());
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Basic abc".parse().unwrap());
        let err = require_bearer(&state, &headers).await.unwrap_err();
        assert!(matches!(err, Error::Unauthorized(_)));
    }

    #[tokio::test]
    async fn unknown_key_returns_unauthorized() {
        let state = AppState::with_memory(test_config());
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer gw_v1_unknown".parse().unwrap());
        let err = require_bearer(&state, &headers).await.unwrap_err();
        assert!(matches!(err, Error::Unauthorized(_)));
    }

    #[tokio::test]
    async fn valid_key_resolves_user_sub() {
        let state = AppState::with_memory(test_config());
        let (plaintext, hash) = mint_key();
        state
            .keys()
            .mint(hash, "user-1", Some("test"))
            .await
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {plaintext}").parse().unwrap(),
        );
        let authed = require_bearer(&state, &headers).await.unwrap();
        assert_eq!(authed.user_sub, "user-1");
    }

    #[tokio::test]
    async fn x_api_key_header_is_accepted() {
        let state = AppState::with_memory(test_config());
        let (plaintext, hash) = mint_key();
        state
            .keys()
            .mint(hash, "user-anthropic", None)
            .await
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", plaintext.parse().unwrap());
        let authed = require_bearer(&state, &headers).await.unwrap();
        assert_eq!(authed.user_sub, "user-anthropic");
    }

    #[tokio::test]
    async fn x_goog_api_key_header_is_accepted() {
        let state = AppState::with_memory(test_config());
        let (plaintext, hash) = mint_key();
        state.keys().mint(hash, "user-gemini", None).await.unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-goog-api-key", plaintext.parse().unwrap());
        let authed = require_bearer(&state, &headers).await.unwrap();
        assert_eq!(authed.user_sub, "user-gemini");
    }
}
