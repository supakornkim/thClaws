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
        .anthropic_api_key
        .as_deref()
        .ok_or_else(|| Error::NotConfigured("ANTHROPIC_API_KEY not set on gateway".into()))?;
    // anthropic-version is required on every messages call. We default
    // here so a caller that forgot the header still gets a working
    // request; an explicit value from the client overrides because
    // request headers land in the HeaderMap before our extras.
    let mut extra: Vec<(HeaderName, axum::http::HeaderValue)> = Vec::new();
    extra.push((
        HeaderName::from_static("anthropic-version"),
        header_value("2023-06-01")?,
    ));
    Ok(UpstreamSpec {
        upstream_base: &state.config().anthropic_base_url,
        upstream_path: rest_to_path(rest),
        auth_header: (HeaderName::from_static("x-api-key"), header_value(key)?),
        extra_headers: extra,
    })
}
