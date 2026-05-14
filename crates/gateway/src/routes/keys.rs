//! `/v1/keys` lifecycle.
//!
//! - `POST /v1/keys` — body `{ "id_token": "<google-jwt>", "label": "desktop" }`
//!   verifies the id_token, mints a fresh access key bound to `sub`,
//!   and returns the plaintext **once**. Subsequent calls only see the
//!   hash; lost keys must be revoked + re-minted.
//! - `GET /v1/keys` — Bearer auth, lists the caller's non-revoked keys.
//! - `DELETE /v1/keys/{id}` — Bearer auth, revokes a key the caller owns.

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::auth::require_bearer;
use crate::error::{Error, Result};
use crate::keys::mint_key;
use crate::sso::verify_id_token;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct MintRequest {
    pub id_token: String,
    #[serde(default)]
    pub label: Option<String>,
}

pub async fn mint(
    State(state): State<AppState>,
    Json(req): Json<MintRequest>,
) -> Result<impl IntoResponse> {
    let cfg = state.config();
    // verify_id_token auto-detects the issuer (Google or Azure) from
    // the inbound token's `iss` claim and validates against the
    // matching configured client id. Subject is namespaced —
    // `google:<sub>` or `azure:<tid>:<oid>` — so two users with the
    // same Microsoft `oid` across different tenants don't collide,
    // and no Google `sub` can ever be impersonated by an Azure token
    // sharing the raw value.
    let verified = verify_id_token(state.http(), cfg, &req.id_token).await?;
    let (plaintext, hash) = mint_key();
    let row = state
        .keys()
        .mint(hash, &verified.subject, req.label.as_deref())
        .await
        .map_err(|e| Error::Internal(format!("key store: {e}")))?;
    Ok(Json(json!({
        "id": row.id,
        "key": plaintext,
        "label": row.label,
        "created_at": row.created_at,
        "subject": {
            "issuer": verified.issuer,
            "id": verified.subject,
            "email": verified.email,
            "email_verified": verified.email_verified,
        },
    })))
}

pub async fn list(State(state): State<AppState>, headers: HeaderMap) -> Result<impl IntoResponse> {
    let authed = require_bearer(&state, &headers).await?;
    let rows = state
        .keys()
        .list_for_user(&authed.user_sub)
        .await
        .map_err(|e| Error::Internal(format!("key store: {e}")))?;
    let payload: Vec<_> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "label": r.label,
                "created_at": r.created_at,
                "last_used": r.last_used,
            })
        })
        .collect();
    Ok(Json(json!({"keys": payload})))
}

pub async fn revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse> {
    let authed = require_bearer(&state, &headers).await?;
    let removed = state
        .keys()
        .revoke(id, &authed.user_sub)
        .await
        .map_err(|e| Error::Internal(format!("key store: {e}")))?;
    Ok(Json(json!({"revoked": removed})))
}
