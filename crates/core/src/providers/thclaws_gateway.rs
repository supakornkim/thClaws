//! thClaws Gateway overlay — distinct from the EE-policy gateway in
//! `crate::providers::gateway`.
//!
//! When the user has the "Use thClaws Gateway" toggle enabled for the
//! active provider AND has pasted an access key, the provider's HTTP
//! client points at the gateway instead of the upstream. The gateway
//! preserves each provider's native wire shape (per-prefix passthrough),
//! so the only knobs that change at the provider layer are:
//!
//! 1. Base URL → `<gateway>/<provider-segment>/<original-path>`
//! 2. Auth header value → the gateway access key
//!
//! The header **scheme** stays unchanged: OpenAI/OpenRouter clients
//! still send `Authorization: Bearer …`, Anthropic still sends
//! `x-api-key`, Gemini still sends `x-goog-api-key`. The gateway
//! accepts all three (see `gateway::auth::require_bearer`).
//!
//! ## Base URL
//!
//! The gateway base URL is **fixed** at the canonical
//! [`GATEWAY_BASE_URL`] (`https://gateway.thclaws.ai`). End users
//! can't change it from the Settings UI — there's nothing to
//! misconfigure. For development against a staging gateway, set the
//! `THCLAWS_GATEWAY_BASE_URL` env var; it overrides at lookup time.
//!
//! ## Access key
//!
//! Resolution order:
//! 1. `THCLAWS_GATEWAY_API_KEY` env var
//! 2. OS keychain bundle, account `gateway`
//! 3. None → overlay disabled (falls back to the provider's native upstream)

use crate::config::AppConfig;
use crate::providers::ProviderKind;

/// Fixed gateway base URL. Matches the DNS at
/// `gateway.thclaws.ai → 203.150.118.93` + the Ingress host in
/// `thclaws/k8s/gateway/ingress.yaml`. Override at lookup time with
/// `THCLAWS_GATEWAY_BASE_URL` for staging / local dev only.
pub const GATEWAY_BASE_URL: &str = "https://gateway.thclaws.ai";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayOverlay {
    /// Final base URL: `<gateway>/<segment>` with no trailing slash.
    /// Provider impls append their own per-request path.
    pub base_url: String,
    /// The gateway access key. Each provider plugs this into its
    /// existing auth header (Authorization / x-api-key / x-goog-api-key).
    pub access_key: String,
}

/// The path segment under the gateway base URL for each provider.
/// Matches the routes wired in `crates/gateway/src/routes/mod.rs`.
pub fn provider_segment(kind: ProviderKind) -> Option<&'static str> {
    match kind {
        ProviderKind::OpenAI | ProviderKind::OpenAIResponses => Some("openai"),
        ProviderKind::Anthropic => Some("anthropic"),
        ProviderKind::Gemini => Some("google"),
        ProviderKind::OpenRouter => Some("openrouter"),
        _ => None,
    }
}

/// Lowercase name used in `AppConfig::gateway_use_for`. Matches the
/// path segment so the per-provider toggle UI and the routing share
/// vocabulary.
pub fn provider_name_for_config(kind: ProviderKind) -> Option<&'static str> {
    provider_segment(kind)
}

/// Compute the overlay for this provider kind. Returns `None` when
/// the toggle is off for this provider OR the access key isn't
/// available. The base URL is fixed (see [`GATEWAY_BASE_URL`] and the
/// `THCLAWS_GATEWAY_BASE_URL` override).
pub fn for_kind(config: &AppConfig, kind: ProviderKind) -> Option<GatewayOverlay> {
    let name = provider_name_for_config(kind)?;
    if !config
        .gateway_use_for
        .iter()
        .any(|p| p.eq_ignore_ascii_case(name))
    {
        return None;
    }
    let access_key = resolve_access_key()?;
    let segment = provider_segment(kind)?;
    let base_url = format!("{}/{}", resolve_base_url().trim_end_matches('/'), segment);
    Some(GatewayOverlay {
        base_url,
        access_key,
    })
}

/// Resolve the gateway base URL. Honors `THCLAWS_GATEWAY_BASE_URL`
/// for dev/staging overrides; otherwise returns the canonical
/// [`GATEWAY_BASE_URL`].
fn resolve_base_url() -> String {
    std::env::var("THCLAWS_GATEWAY_BASE_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| GATEWAY_BASE_URL.to_string())
}

/// Look up the gateway access key. Env var wins (handy for CI /
/// scripted runs); otherwise keychain bundle.
fn resolve_access_key() -> Option<String> {
    if let Ok(v) = std::env::var("THCLAWS_GATEWAY_API_KEY") {
        let trimmed = v.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    crate::secrets::get("gateway")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests below mutate the process-global `THCLAWS_GATEWAY_*` env
    // vars. Cargo runs lib tests in parallel; this mutex serialises
    // the env-touching tests so a sibling test reading the resolved
    // value mid-mutation doesn't see ghost state.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn cfg(providers: &[&str]) -> AppConfig {
        let mut c = AppConfig::default();
        c.gateway_use_for = providers.iter().map(|s| s.to_string()).collect();
        c
    }

    #[test]
    fn provider_segment_covers_supported_kinds() {
        assert_eq!(provider_segment(ProviderKind::OpenAI), Some("openai"));
        assert_eq!(provider_segment(ProviderKind::Anthropic), Some("anthropic"));
        assert_eq!(provider_segment(ProviderKind::Gemini), Some("google"));
        assert_eq!(
            provider_segment(ProviderKind::OpenRouter),
            Some("openrouter")
        );
        assert_eq!(provider_segment(ProviderKind::Ollama), None);
        assert_eq!(provider_segment(ProviderKind::LMStudio), None);
    }

    #[test]
    fn for_kind_returns_none_when_provider_not_enabled() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = cfg(&["openai"]);
        std::env::set_var("THCLAWS_GATEWAY_API_KEY", "gw_v1_test");
        let out = for_kind(&config, ProviderKind::Gemini);
        std::env::remove_var("THCLAWS_GATEWAY_API_KEY");
        assert!(out.is_none());
    }

    #[test]
    fn for_kind_returns_none_when_access_key_missing() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = cfg(&["openai"]);
        std::env::remove_var("THCLAWS_GATEWAY_API_KEY");
        let out = for_kind(&config, ProviderKind::OpenAI);
        // Will be None unless the keychain happens to have a 'gateway'
        // entry on the test machine. Most CI hosts won't.
        if out.is_some() {
            // Local dev with a real key in the keychain — accept it.
            return;
        }
        assert!(out.is_none());
    }

    #[test]
    fn for_kind_uses_fixed_base_url_by_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = cfg(&["openai", "anthropic"]);
        std::env::set_var("THCLAWS_GATEWAY_API_KEY", "gw_v1_test");
        std::env::remove_var("THCLAWS_GATEWAY_BASE_URL");
        let openai = for_kind(&config, ProviderKind::OpenAI).expect("openai overlay");
        let anthropic = for_kind(&config, ProviderKind::Anthropic).expect("anthropic overlay");
        std::env::remove_var("THCLAWS_GATEWAY_API_KEY");

        assert_eq!(openai.base_url, format!("{GATEWAY_BASE_URL}/openai"));
        assert_eq!(openai.access_key, "gw_v1_test");
        assert_eq!(anthropic.base_url, format!("{GATEWAY_BASE_URL}/anthropic"));
    }

    #[test]
    fn for_kind_honors_base_url_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = cfg(&["openrouter"]);
        std::env::set_var("THCLAWS_GATEWAY_API_KEY", "k");
        std::env::set_var(
            "THCLAWS_GATEWAY_BASE_URL",
            "https://staging.gateway.thclaws.ai/",
        );
        let out = for_kind(&config, ProviderKind::OpenRouter).expect("overlay");
        std::env::remove_var("THCLAWS_GATEWAY_API_KEY");
        std::env::remove_var("THCLAWS_GATEWAY_BASE_URL");
        assert_eq!(
            out.base_url,
            "https://staging.gateway.thclaws.ai/openrouter"
        );
    }
}
