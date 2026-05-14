//! id_token verification for SSO-driven `POST /v1/keys` mints.
//!
//! Supports two issuers — auto-detected from the unverified `iss`
//! claim of the inbound token so the public API stays a single
//! endpoint:
//!
//! - **Google** (`accounts.google.com` / `https://accounts.google.com`)
//!   — JWKS at `https://www.googleapis.com/oauth2/v3/certs`, `aud`
//!   must equal the gateway's `GOOGLE_CLIENT_ID`. Subject is the
//!   `sub` claim (globally unique).
//! - **Azure / Microsoft Entra ID multi-tenant**
//!   (`https://login.microsoftonline.com/{tid}/v2.0`) — JWKS at
//!   `https://login.microsoftonline.com/{tid}/discovery/v2.0/keys`
//!   per-tenant, `aud` must equal `AZURE_CLIENT_ID`. Subject is
//!   `tid:oid` (object id is only unique within a tenant, so we
//!   include the tenant to keep subjects unique across orgs).
//!
//! Why we verify **id_tokens** and not access tokens: id_tokens are
//! OIDC identity assertions explicitly meant for relying-party
//! consumption (stable claims contract, JWKS-signed, audience-bound).
//! Access tokens are scope-bound API credentials, often opaque on
//! Azure v1 endpoints, and explicitly documented as
//! not-for-validation-by-clients. Treating an access token as an
//! identity assertion would couple us to whichever Graph scope the
//! desktop happened to request and break the moment the upstream
//! IdP swaps formats.
//!
//! ## Why we peek the payload before verifying
//!
//! `iss` decides which JWKS endpoint to fetch, and for Azure also
//! determines the tenant id used in the JWKS URL. We have to peek
//! `iss` (and Azure's `tid` claim) from the unverified payload to
//! pick the right verifier, then run the proper signature + claim
//! validation. Peeking is safe because we don't trust the peeked
//! value for anything besides routing — the subsequent `decode`
//! call enforces signature + iss + aud + exp.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine as _};
use jsonwebtoken::{decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::Value;

use crate::config::Config;
use crate::error::{Error, Result};

const GOOGLE_ISS: &[&str] = &["https://accounts.google.com", "accounts.google.com"];
const GOOGLE_JWKS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";

const AZURE_ISS_PREFIX: &str = "https://login.microsoftonline.com/";
const AZURE_ISS_SUFFIX: &str = "/v2.0";

/// What `POST /v1/keys` records on the minted key after a successful
/// verify. `subject` is the canonical user identifier the lookup /
/// list / revoke routes scope on; `issuer` is informational.
#[derive(Debug, Clone)]
pub struct VerifiedSubject {
    pub issuer: &'static str,
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct GoogleClaims {
    pub sub: String,
    pub aud: String,
    pub iss: String,
    pub exp: usize,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub email_verified: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct AzureClaims {
    /// Per-app pairwise subject (not stable across apps in the same
    /// tenant — we use `oid` + `tid` for our subject instead).
    #[serde(default)]
    #[allow(dead_code)]
    pub sub: String,
    pub aud: String,
    pub iss: String,
    pub exp: usize,
    /// Stable object id within the tenant.
    pub oid: String,
    /// Tenant id.
    pub tid: String,
    #[serde(default)]
    pub email: Option<String>,
    /// Preferred username — usually the user's UPN/email-like
    /// identifier. Microsoft does not mark email as verified in
    /// id_tokens the way Google does; we surface `preferred_username`
    /// as the email-equivalent field when `email` is missing.
    #[serde(default, rename = "preferred_username")]
    pub preferred_username: Option<String>,
}

/// Public entry point. Auto-detects the issuer and dispatches.
pub async fn verify_id_token(
    http: &reqwest::Client,
    config: &Config,
    id_token: &str,
) -> Result<VerifiedSubject> {
    let payload = peek_payload(id_token)?;
    let iss = payload
        .get("iss")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Unauthorized("id_token has no iss claim".into()))?;

    if GOOGLE_ISS.contains(&iss) {
        let aud = config
            .google_client_id
            .as_deref()
            .ok_or_else(|| Error::NotConfigured("GOOGLE_CLIENT_ID not set on gateway".into()))?;
        let claims = verify_google_id_token(http, id_token, aud).await?;
        return Ok(VerifiedSubject {
            issuer: "google",
            subject: format!("google:{}", claims.sub),
            email: claims.email,
            email_verified: claims.email_verified,
        });
    }

    if let Some(tid) = extract_azure_tenant(iss) {
        let aud = config
            .azure_client_id
            .as_deref()
            .ok_or_else(|| Error::NotConfigured("AZURE_CLIENT_ID not set on gateway".into()))?;
        // Cross-check: the tid claim on the payload must agree with
        // the tid embedded in the issuer URL. Otherwise a token issued
        // for tenant A could lie about its tenant in `iss` and be
        // verified against tenant B's JWKS — defense in depth.
        let claim_tid = payload
            .get("tid")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Unauthorized("Azure id_token missing tid claim".into()))?;
        if claim_tid != tid {
            return Err(Error::Unauthorized(format!(
                "Azure tid mismatch: iss embeds {tid}, claim is {claim_tid}"
            )));
        }
        let claims = verify_azure_id_token(http, id_token, aud, tid).await?;
        let email = claims.email.or(claims.preferred_username);
        return Ok(VerifiedSubject {
            issuer: "azure",
            subject: format!("azure:{}:{}", claims.tid, claims.oid),
            email,
            // Microsoft Entra doesn't publish a `email_verified` claim
            // by default; leave it None to make the absence visible
            // upstream rather than asserting a value we don't have.
            email_verified: None,
        });
    }

    Err(Error::Unauthorized(format!(
        "unrecognised id_token issuer: {iss}"
    )))
}

/// Backwards-compat shim — the original entry point still works for
/// Google-only callers (current `routes::keys::mint` migrates to the
/// unified `verify_id_token` shape; this helper stays for tests +
/// direct-Google call sites).
pub async fn verify_google_id_token(
    http: &reqwest::Client,
    id_token: &str,
    expected_aud: &str,
) -> Result<GoogleClaims> {
    let header = decode_header(id_token)
        .map_err(|e| Error::Unauthorized(format!("bad id_token header: {e}")))?;
    let kid = header
        .kid
        .ok_or_else(|| Error::Unauthorized("id_token has no kid".into()))?;

    let jwks = fetch_jwks(http, GOOGLE_JWKS_URL).await?;
    let key = jwks_key_for_kid(&jwks, &kid)?;

    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[expected_aud]);
    validation.set_issuer(GOOGLE_ISS);
    validation.validate_exp = true;

    let token = decode::<GoogleClaims>(id_token, &key, &validation)
        .map_err(|e| Error::Unauthorized(format!("id_token verify: {e}")))?;
    Ok(token.claims)
}

async fn verify_azure_id_token(
    http: &reqwest::Client,
    id_token: &str,
    expected_aud: &str,
    tenant_id: &str,
) -> Result<AzureClaims> {
    let header = decode_header(id_token)
        .map_err(|e| Error::Unauthorized(format!("bad id_token header: {e}")))?;
    let kid = header
        .kid
        .ok_or_else(|| Error::Unauthorized("id_token has no kid".into()))?;

    let jwks_url = format!("https://login.microsoftonline.com/{tenant_id}/discovery/v2.0/keys");
    let jwks = fetch_jwks(http, &jwks_url).await?;
    let key = jwks_key_for_kid(&jwks, &kid)?;

    let expected_iss = format!("{AZURE_ISS_PREFIX}{tenant_id}{AZURE_ISS_SUFFIX}");
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[expected_aud]);
    validation.set_issuer(&[expected_iss.as_str()]);
    validation.validate_exp = true;

    let token = decode::<AzureClaims>(id_token, &key, &validation)
        .map_err(|e| Error::Unauthorized(format!("id_token verify: {e}")))?;
    Ok(token.claims)
}

fn peek_payload(id_token: &str) -> Result<Value> {
    let mut parts = id_token.split('.');
    let _header = parts
        .next()
        .ok_or_else(|| Error::Unauthorized("id_token: missing header segment".into()))?;
    let payload = parts
        .next()
        .ok_or_else(|| Error::Unauthorized("id_token: missing payload segment".into()))?;
    if parts.next().is_none() {
        return Err(Error::Unauthorized(
            "id_token: missing signature segment".into(),
        ));
    }
    let bytes = B64
        .decode(payload)
        .map_err(|e| Error::Unauthorized(format!("id_token payload base64: {e}")))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Unauthorized(format!("id_token payload JSON: {e}")))?;
    Ok(value)
}

/// Extract the tenant id from an Azure v2 issuer URL.
/// `https://login.microsoftonline.com/{tid}/v2.0` → `Some(tid)`.
/// Returns `None` when the shape doesn't match — caller then falls
/// through to "unrecognised issuer".
fn extract_azure_tenant(iss: &str) -> Option<&str> {
    let middle = iss.strip_prefix(AZURE_ISS_PREFIX)?;
    let tid = middle.strip_suffix(AZURE_ISS_SUFFIX)?;
    if tid.is_empty() || tid.contains('/') {
        None
    } else {
        Some(tid)
    }
}

async fn fetch_jwks(http: &reqwest::Client, url: &str) -> Result<JwkSet> {
    http.get(url)
        .send()
        .await
        .map_err(|e| Error::Upstream(format!("jwks fetch: {e}")))?
        .error_for_status()
        .map_err(|e| Error::Upstream(format!("jwks fetch: {e}")))?
        .json()
        .await
        .map_err(|e| Error::Upstream(format!("jwks parse: {e}")))
}

fn jwks_key_for_kid(jwks: &JwkSet, kid: &str) -> Result<DecodingKey> {
    let jwk = jwks
        .find(kid)
        .ok_or_else(|| Error::Unauthorized(format!("kid {kid} not in JWKS")))?;
    DecodingKey::from_jwk(jwk).map_err(|e| Error::Unauthorized(format!("jwk → key: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_token(payload: &Value) -> String {
        // Header + signature segments are dummies — only `peek_payload`
        // is exercised here, no signature verification runs in these
        // tests. Real signature verification lives behind the network
        // call to JWKS and is exercised via integration / wiremock.
        let header = B64.encode(b"{\"alg\":\"RS256\",\"kid\":\"x\"}");
        let body = B64.encode(payload.to_string().as_bytes());
        let sig = B64.encode(b"sig");
        format!("{header}.{body}.{sig}")
    }

    #[test]
    fn peek_payload_extracts_iss_claim() {
        let token = make_token(&json!({"iss": "accounts.google.com", "sub": "1234"}));
        let payload = peek_payload(&token).unwrap();
        assert_eq!(payload["iss"], "accounts.google.com");
        assert_eq!(payload["sub"], "1234");
    }

    #[test]
    fn peek_payload_rejects_malformed_jwt() {
        // Missing the third (signature) segment.
        assert!(peek_payload("aa.bb").is_err());
        // Not base64-decodable payload.
        assert!(peek_payload("aa.@@@.cc").is_err());
    }

    #[test]
    fn extract_azure_tenant_matches_v2_shape() {
        assert_eq!(
            extract_azure_tenant("https://login.microsoftonline.com/abc-123/v2.0"),
            Some("abc-123")
        );
    }

    #[test]
    fn extract_azure_tenant_rejects_google_or_truncated_shapes() {
        assert_eq!(extract_azure_tenant("https://accounts.google.com"), None);
        assert_eq!(
            extract_azure_tenant("https://login.microsoftonline.com/v2.0"),
            None
        );
        assert_eq!(
            extract_azure_tenant("https://login.microsoftonline.com/tid/extra/v2.0"),
            None
        );
    }

    #[tokio::test]
    async fn verify_rejects_token_with_no_iss() {
        let cfg = Config {
            bind: "127.0.0.1:0".into(),
            database_url: None,
            google_client_id: Some("test-google".into()),
            azure_client_id: Some("test-azure".into()),
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
        };
        let http = reqwest::Client::new();
        let token = make_token(&json!({"sub": "x"}));
        let err = verify_id_token(&http, &cfg, &token).await.unwrap_err();
        assert!(matches!(err, Error::Unauthorized(_)));
    }

    #[tokio::test]
    async fn verify_rejects_unknown_issuer() {
        let cfg = Config {
            bind: "127.0.0.1:0".into(),
            database_url: None,
            google_client_id: Some("test-google".into()),
            azure_client_id: Some("test-azure".into()),
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
        };
        let http = reqwest::Client::new();
        let token = make_token(&json!({"iss": "https://attacker.example.com"}));
        let err = verify_id_token(&http, &cfg, &token).await.unwrap_err();
        match err {
            Error::Unauthorized(msg) => assert!(msg.contains("unrecognised id_token issuer")),
            other => panic!("expected Unauthorized, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_rejects_azure_tid_mismatch() {
        let cfg = Config {
            bind: "127.0.0.1:0".into(),
            database_url: None,
            google_client_id: None,
            azure_client_id: Some("test-azure".into()),
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
        };
        let http = reqwest::Client::new();
        // iss embeds tenant A, but the `tid` claim says tenant B.
        let token = make_token(&json!({
            "iss": "https://login.microsoftonline.com/tenant-a/v2.0",
            "tid": "tenant-b",
            "oid": "user-1",
            "aud": "test-azure",
            "exp": 9_999_999_999_u64,
            "sub": "abc",
        }));
        let err = verify_id_token(&http, &cfg, &token).await.unwrap_err();
        match err {
            Error::Unauthorized(msg) => assert!(msg.contains("tid mismatch")),
            other => panic!("expected Unauthorized, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_rejects_when_azure_client_id_unset() {
        let cfg = Config {
            bind: "127.0.0.1:0".into(),
            database_url: None,
            google_client_id: Some("test-google".into()),
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
        };
        let http = reqwest::Client::new();
        let token = make_token(&json!({
            "iss": "https://login.microsoftonline.com/abc-123/v2.0",
            "tid": "abc-123",
            "oid": "user-1",
        }));
        let err = verify_id_token(&http, &cfg, &token).await.unwrap_err();
        assert!(matches!(err, Error::NotConfigured(_)));
    }
}
