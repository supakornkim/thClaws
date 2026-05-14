//! Tavily takes its API key in the *body* (`api_key` field), not a
//! header. To stay a pure passthrough, the gateway forwards what the
//! client sent — the client is expected to put its own key in the
//! body OR the gateway can be configured to rewrite. For v1 we leave
//! the body untouched and use header-style auth on the wire too
//! (Bearer is accepted by Tavily as of mid-2024); upstream rejects
//! cleanly if the API ever stops accepting it.

use axum::{
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderName},
    response::Response,
};

use crate::error::{Error, Result};
use crate::proxy::{header_value, rest_to_path, run_provider, UpstreamSpec};
use crate::state::AppState;

pub async fn proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(rest): Path<String>,
    req: Request,
) -> Response {
    run_provider(state, headers, rest, req, |state, rest| {
        build_spec(state, &rest)
    })
    .await
}

fn build_spec<'a>(state: &'a AppState, rest: &str) -> Result<UpstreamSpec<'a>> {
    let key = state
        .config()
        .tavily_api_key
        .as_deref()
        .ok_or_else(|| Error::NotConfigured("TAVILY_API_KEY not set on gateway".into()))?;
    Ok(UpstreamSpec {
        upstream_base: &state.config().tavily_base_url,
        upstream_path: rest_to_path(rest),
        auth_header: (
            HeaderName::from_static("authorization"),
            header_value(&format!("Bearer {key}"))?,
        ),
        extra_headers: Vec::new(),
    })
}
