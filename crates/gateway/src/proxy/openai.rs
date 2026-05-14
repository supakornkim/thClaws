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
        .openai_api_key
        .as_deref()
        .ok_or_else(|| Error::NotConfigured("OPENAI_API_KEY not set on gateway".into()))?;
    Ok(UpstreamSpec {
        upstream_base: &state.config().openai_base_url,
        upstream_path: rest_to_path(rest),
        auth_header: (
            HeaderName::from_static("authorization"),
            header_value(&format!("Bearer {key}"))?,
        ),
        extra_headers: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::keys::{hash_key, mint_key};
    use crate::routes::build_router;
    use crate::state::AppState;
    use axum::body::Body;
    use axum::http::{Request as AxumRequest, StatusCode};
    use tower::ServiceExt;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cfg_with_upstream(openai_base: String) -> Config {
        Config {
            bind: "127.0.0.1:0".into(),
            database_url: None,
            google_client_id: None,
            azure_client_id: None,
            openai_api_key: Some("sk-upstream".into()),
            anthropic_api_key: None,
            google_api_key: None,
            openrouter_api_key: None,
            tavily_api_key: None,
            hal_api_key: None,
            openai_base_url: openai_base,
            anthropic_base_url: "https://example.invalid".into(),
            google_base_url: "https://example.invalid".into(),
            openrouter_base_url: "https://example.invalid".into(),
            tavily_base_url: "https://example.invalid".into(),
            hal_base_url: "https://example.invalid".into(),
        }
    }

    #[tokio::test]
    async fn proxy_rejects_without_bearer() {
        let upstream = MockServer::start().await;
        let state = AppState::with_memory(cfg_with_upstream(upstream.uri()));
        let app = build_router(state);

        let req = AxumRequest::builder()
            .method("POST")
            .uri("/openai/v1/chat/completions")
            .body(Body::from("{}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn proxy_forwards_with_upstream_credential_injected() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer sk-upstream"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{\"ok\":true}"))
            .mount(&upstream)
            .await;

        let state = AppState::with_memory(cfg_with_upstream(upstream.uri()));
        let (plaintext, hash) = mint_key();
        state
            .keys()
            .mint(hash, "user-1", Some("test"))
            .await
            .unwrap();
        // Sanity: the hash → user_sub link is what auth.rs relies on.
        assert!(state
            .keys()
            .lookup_and_touch(&hash_key(&plaintext))
            .await
            .unwrap()
            .is_some());

        let app = build_router(state);
        let req = AxumRequest::builder()
            .method("POST")
            .uri("/openai/v1/chat/completions")
            .header("authorization", format!("Bearer {plaintext}"))
            // Inbound bearer must NOT make it to the upstream; only
            // the gateway's `sk-upstream` should appear there.
            .header("content-type", "application/json")
            .body(Body::from("{\"model\":\"gpt-4o\"}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(&body[..], b"{\"ok\":true}");
    }

    #[tokio::test]
    async fn proxy_returns_503_when_upstream_key_not_configured() {
        let upstream = MockServer::start().await;
        let mut cfg = cfg_with_upstream(upstream.uri());
        cfg.openai_api_key = None;
        let state = AppState::with_memory(cfg);
        let (plaintext, hash) = mint_key();
        state.keys().mint(hash, "user-1", None).await.unwrap();

        let app = build_router(state);
        let req = AxumRequest::builder()
            .method("POST")
            .uri("/openai/v1/chat/completions")
            .header("authorization", format!("Bearer {plaintext}"))
            .body(Body::from("{}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
