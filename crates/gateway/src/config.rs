//! Env-driven config. `Config::from_env` is the single source of
//! truth for runtime knobs — main.rs and tests both read through it
//! to avoid scattered `std::env::var` calls.

use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    pub database_url: Option<String>,

    /// Google OAuth client id the gateway verifies inbound id_tokens
    /// against (the `aud` claim). MUST match the desktop's
    /// `GOOGLE_CLIENT_ID` — the desktop signs in with that client,
    /// and the gateway only trusts tokens minted for it.
    pub google_client_id: Option<String>,

    /// Azure / Microsoft Entra ID application (client) id. Multi-tenant
    /// app: any tenant's users can sign in, but `aud` on the inbound
    /// id_token must equal this value. `None` → Azure SSO at
    /// `/v1/keys` is rejected with "Azure not configured".
    pub azure_client_id: Option<String>,

    /// Per-upstream credentials. None means "no key configured →
    /// this provider's proxy route returns 503". Set the ones you
    /// want active in the gateway's k8s Secret.
    pub openai_api_key: Option<String>,
    pub anthropic_api_key: Option<String>,
    pub google_api_key: Option<String>,
    pub openrouter_api_key: Option<String>,
    pub tavily_api_key: Option<String>,
    pub hal_api_key: Option<String>,

    /// Upstream base URLs (override-able for testing). Production
    /// defaults are the providers' public endpoints.
    pub openai_base_url: String,
    pub anthropic_base_url: String,
    pub google_base_url: String,
    pub openrouter_base_url: String,
    pub tavily_base_url: String,
    pub hal_base_url: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            bind: env::var("BIND").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            database_url: opt("DATABASE_URL"),
            google_client_id: opt("GOOGLE_CLIENT_ID"),
            azure_client_id: opt("AZURE_CLIENT_ID"),
            openai_api_key: opt("OPENAI_API_KEY"),
            anthropic_api_key: opt("ANTHROPIC_API_KEY"),
            google_api_key: opt("GOOGLE_API_KEY").or_else(|| opt("GEMINI_API_KEY")),
            openrouter_api_key: opt("OPENROUTER_API_KEY"),
            tavily_api_key: opt("TAVILY_API_KEY"),
            hal_api_key: opt("HAL_API_KEY"),
            openai_base_url: env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com".into()),
            anthropic_base_url: env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com".into()),
            google_base_url: env::var("GOOGLE_BASE_URL")
                .unwrap_or_else(|_| "https://generativelanguage.googleapis.com".into()),
            openrouter_base_url: env::var("OPENROUTER_BASE_URL")
                .unwrap_or_else(|_| "https://openrouter.ai".into()),
            tavily_base_url: env::var("TAVILY_BASE_URL")
                .unwrap_or_else(|_| "https://api.tavily.com".into()),
            hal_base_url: env::var("HAL_BASE_URL")
                .unwrap_or_else(|_| "https://hal.thaigpt.com".into()),
        })
    }
}

fn opt(name: &str) -> Option<String> {
    env::var(name).ok().filter(|s| !s.is_empty())
}
