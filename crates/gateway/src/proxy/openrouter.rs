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
        .openrouter_api_key
        .as_deref()
        .ok_or_else(|| Error::NotConfigured("OPENROUTER_API_KEY not set on gateway".into()))?;
    Ok(UpstreamSpec {
        upstream_base: &state.config().openrouter_base_url,
        upstream_path: rest_to_path(rest),
        auth_header: (
            HeaderName::from_static("authorization"),
            header_value(&format!("Bearer {key}"))?,
        ),
        extra_headers: Vec::new(),
    })
}
