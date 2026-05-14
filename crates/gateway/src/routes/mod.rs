//! HTTP route table. One module per concern.

pub mod health;
pub mod keys;

use axum::{
    routing::{any, delete, get, post},
    Router,
};

use crate::state::AppState;

pub fn build_router(state: AppState) -> Router {
    use tower_http::trace::TraceLayer;

    Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .route("/v1/keys", post(keys::mint).get(keys::list))
        .route("/v1/keys/{id}", delete(keys::revoke))
        // Per-provider proxy fanout. `any` matches every HTTP method
        // the upstream supports; the trailing wildcard captures the
        // rest of the path so it lands verbatim on the upstream.
        .route("/openai/{*rest}", any(crate::proxy::openai::proxy))
        .route("/anthropic/{*rest}", any(crate::proxy::anthropic::proxy))
        .route("/google/{*rest}", any(crate::proxy::google::proxy))
        .route("/openrouter/{*rest}", any(crate::proxy::openrouter::proxy))
        .route("/tavily/{*rest}", any(crate::proxy::tavily::proxy))
        .route("/hal/{*rest}", any(crate::proxy::hal::proxy))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}
