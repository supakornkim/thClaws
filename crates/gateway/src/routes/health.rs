use axum::{response::IntoResponse, Json};
use serde_json::json;

use crate::state::AppState;

pub async fn healthz() -> impl IntoResponse {
    Json(json!({"status": "ok", "name": "thclaws-gateway"}))
}

pub async fn readyz(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> impl IntoResponse {
    // Readiness == at least one upstream credential configured. A
    // gateway with zero providers wired up is "ok but useless"; k8s
    // can use this to surface a misdeploy.
    let cfg = state.config();
    let any_upstream = cfg.openai_api_key.is_some()
        || cfg.anthropic_api_key.is_some()
        || cfg.google_api_key.is_some()
        || cfg.openrouter_api_key.is_some()
        || cfg.tavily_api_key.is_some()
        || cfg.hal_api_key.is_some();
    if any_upstream {
        (axum::http::StatusCode::OK, Json(json!({"ready": true}))).into_response()
    } else {
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ready": false, "reason": "no upstream credentials configured"})),
        )
            .into_response()
    }
}
