//! Per-provider proxy fanout.
//!
//! Each upstream gets a thin wrapper that:
//! 1. Authenticates the inbound request (gateway access key).
//! 2. Checks the matching upstream credential is configured.
//! 3. Calls [`forward`] with a per-provider [`UpstreamSpec`] —
//!    URL rewrite, auth header injection, request-/response-header
//!    filtering.
//!
//! Streaming is preserved end-to-end: the inbound body is forwarded
//! via `reqwest::Body::wrap_stream`, the upstream response body is
//! returned through `axum::body::Body::from_stream` so SSE / chunked
//! responses stream through without buffering.

pub mod anthropic;
pub mod google;
pub mod hal;
pub mod openai;
pub mod openrouter;
pub mod tavily;

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures::TryStreamExt;

use crate::auth::require_bearer;
use crate::error::{Error, Result};
use crate::state::AppState;

/// Per-provider routing description. The proxy entry point fills
/// this in and hands off to [`forward`].
pub struct UpstreamSpec<'a> {
    pub upstream_base: &'a str,
    pub upstream_path: String,
    /// Header name + value to inject on the upstream request (e.g.
    /// `("authorization", "Bearer sk-…")` or
    /// `("x-api-key", "sk-ant-…")`). Pre-populated by the caller so
    /// the provider-specific scheme (Bearer / x-api-key / x-goog-api-key)
    /// lives next to the route definition.
    pub auth_header: (HeaderName, HeaderValue),
    /// Extra static headers — used by Anthropic which requires
    /// `anthropic-version: …` on every request.
    pub extra_headers: Vec<(HeaderName, HeaderValue)>,
}

/// Hop-by-hop headers that MUST be stripped per RFC 7230 §6.1 — and a
/// handful of headers that are inappropriate to forward (host,
/// content-length is recomputed by reqwest, the inbound bearer that
/// authorises *us*).
const STRIPPED_REQUEST_HEADERS: &[&str] = &[
    "host",
    "content-length",
    // All three auth schemes the gateway recognises as the access
    // key carrier (see `auth::require_bearer`). The forwarder injects
    // the matching upstream credential per provider, so the inbound
    // gateway-access-key must NOT leak through.
    "authorization",
    "x-api-key",
    "x-goog-api-key",
    "connection",
    "keep-alive",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

const STRIPPED_RESPONSE_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Forward a request to `spec.upstream_base + spec.upstream_path`,
/// inject the upstream auth, stream the response back. The inbound
/// request body is forwarded chunk-for-chunk (no in-memory buffer).
pub async fn forward(state: &AppState, spec: UpstreamSpec<'_>, req: Request) -> Response {
    match forward_inner(state, spec, req).await {
        Ok(resp) => resp,
        Err(e) => e.into_response(),
    }
}

async fn forward_inner(state: &AppState, spec: UpstreamSpec<'_>, req: Request) -> Result<Response> {
    let (parts, body) = req.into_parts();
    let method = parts.method;
    let mut upstream_url =
        String::with_capacity(spec.upstream_base.len() + spec.upstream_path.len());
    upstream_url.push_str(spec.upstream_base.trim_end_matches('/'));
    if !spec.upstream_path.starts_with('/') {
        upstream_url.push('/');
    }
    upstream_url.push_str(&spec.upstream_path);
    if let Some(q) = parts.uri.query() {
        upstream_url.push('?');
        upstream_url.push_str(q);
    }

    // Inbound body → reqwest stream. Use the http-body-util data
    // stream so chunked uploads (e.g. file PDFs to Anthropic) don't
    // buffer in memory.
    let data_stream = body.into_data_stream();
    let req_body =
        reqwest::Body::wrap_stream(data_stream.map_ok(|c| -> Bytes { c }).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::Other, format!("body stream: {e}"))
        }));

    let mut out_headers = HeaderMap::new();
    for (name, value) in parts.headers.iter() {
        if STRIPPED_REQUEST_HEADERS
            .iter()
            .any(|s| name.as_str().eq_ignore_ascii_case(s))
        {
            continue;
        }
        out_headers.insert(name.clone(), value.clone());
    }
    out_headers.insert(spec.auth_header.0, spec.auth_header.1);
    for (n, v) in spec.extra_headers {
        out_headers.insert(n, v);
    }

    let method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|e| Error::Internal(format!("method: {e}")))?;
    let resp = state
        .http()
        .request(method, &upstream_url)
        .headers(out_headers)
        .body(req_body)
        .send()
        .await
        .map_err(|e| Error::Upstream(format!("send: {e}")))?;

    let status = StatusCode::from_u16(resp.status().as_u16())
        .map_err(|e| Error::Internal(format!("status: {e}")))?;
    let mut resp_headers = HeaderMap::new();
    for (name, value) in resp.headers().iter() {
        if STRIPPED_RESPONSE_HEADERS
            .iter()
            .any(|s| name.as_str().eq_ignore_ascii_case(s))
        {
            continue;
        }
        let n = HeaderName::from_bytes(name.as_str().as_bytes())
            .map_err(|e| Error::Internal(format!("hdr name: {e}")))?;
        let v = HeaderValue::from_bytes(value.as_bytes())
            .map_err(|e| Error::Internal(format!("hdr value: {e}")))?;
        resp_headers.insert(n, v);
    }
    let stream = resp.bytes_stream().map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::Other, format!("upstream stream: {e}"))
    });
    let body = Body::from_stream(stream);
    let mut out = Response::builder().status(status);
    {
        let headers_mut = out.headers_mut().expect("builder headers");
        *headers_mut = resp_headers;
    }
    Ok(out
        .body(body)
        .map_err(|e| Error::Internal(format!("response build: {e}")))?)
}

/// Common entry: authenticates the inbound caller, builds the
/// per-provider spec via `build`, then forwards. Each provider's
/// `proxy` function reduces to one call to this.
pub async fn run_provider<F>(
    state: AppState,
    headers: HeaderMap,
    rest: String,
    req: Request,
    build: F,
) -> Response
where
    F: FnOnce(&AppState, String) -> Result<UpstreamSpec<'_>>,
{
    if let Err(e) = require_bearer(&state, &headers).await {
        return e.into_response();
    }
    let spec = match build(&state, rest) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    forward(&state, spec, req).await
}

/// Helper used by every provider — pull the wildcard segment out of
/// the matched path. axum strips the leading `/`, so we glue it back.
pub fn rest_to_path(rest: &str) -> String {
    let mut s = String::with_capacity(rest.len() + 1);
    s.push('/');
    s.push_str(rest);
    s
}

#[allow(dead_code)]
pub(crate) fn header_value(s: &str) -> Result<HeaderValue> {
    HeaderValue::from_str(s).map_err(|e| Error::Internal(format!("hdr value: {e}")))
}

#[allow(dead_code)]
pub(crate) fn _take_state(_state: State<AppState>, _method: Method) {}
