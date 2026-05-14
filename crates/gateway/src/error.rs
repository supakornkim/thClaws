//! Shared error type. Handlers map this to JSON envelopes via
//! `IntoResponse`.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("not configured: {0}")]
    NotConfigured(String),
    #[error("upstream: {0}")]
    Upstream(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            Error::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m.clone()),
            Error::NotConfigured(m) => (StatusCode::SERVICE_UNAVAILABLE, m.clone()),
            Error::Upstream(m) => (StatusCode::BAD_GATEWAY, m.clone()),
            Error::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            Error::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m.clone()),
        };
        (status, Json(json!({"error": msg}))).into_response()
    }
}

pub type Result<T> = std::result::Result<T, Error>;
