//! thClaws Gateway — provider proxy with SSO-minted access keys.
//!
//! Single-process Axum server. Per-provider passthrough routes
//! inject the matching upstream credential from env and stream
//! bytes both directions. Bearer tokens on every authenticated
//! route are SSO-minted personal access keys looked up in
//! Postgres (or an in-memory stub when `DATABASE_URL` is absent).
//!
//! Layout:
//! - `config.rs` — env-driven `Config`
//! - `state.rs` — shared `AppState` (Config + KeyStore + http client)
//! - `keys/` — Postgres + in-memory key store, hashing, mint/list/revoke
//! - `auth.rs` — Bearer middleware that resolves a key to a sub
//! - `sso.rs` — Google id_token verify (JWKS + aud + iss + exp)
//! - `proxy/` — one module per upstream (openai, anthropic, google,
//!   openrouter, tavily, hal). Each is a small handler that rewrites
//!   the URL + auth header and streams bytes back.
//! - `routes/` — Axum handlers (`POST /v1/keys`, `GET/DELETE /v1/keys`,
//!   `GET /healthz`, `GET /readyz`).

use std::net::SocketAddr;
use tracing::info;

mod auth;
mod config;
mod error;
mod keys;
mod proxy;
mod routes;
mod sso;
mod state;

use crate::config::Config;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    init_tracing();
    let config = Config::from_env()?;
    let bind: SocketAddr = config.bind.parse()?;
    let state = AppState::new(config).await?;
    let app = routes::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind(bind).await?;
    info!(addr = %bind, "thclaws-gateway listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,hyper=warn"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(true))
        .init();
}
