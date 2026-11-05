//! Provider implementations

#![allow(dead_code)]

pub mod abacus;
pub mod alibaba;
pub mod alibabatokenplan;
pub mod amp;
pub mod antigravity;
pub mod augment;
pub mod azureopenai;
pub mod bedrock;
pub mod chutes;
pub mod claude;
pub mod codebuff;
pub mod codex;
pub mod commandcode;
pub mod copilot;
pub mod crof;
pub mod crossmodel;
pub mod cursor;
pub mod deepgram;
pub mod deepseek;
pub mod deepseek_harness;
pub mod devin;
pub mod doubao;
pub mod elevenlabs;
pub mod factory;
pub mod gemini;
pub mod grok;
pub mod groq;
pub mod infini;
pub mod jetbrains;
pub mod kilo;
pub mod kimi;
pub mod kimik2;
pub mod kiro;
pub mod litellm;
pub mod llmproxy;
pub mod manus;
pub mod mimo;
pub mod minimax;
pub mod mistral;
pub mod nanogpt;
pub mod ollama;
pub mod openai;
pub mod openaiapi;
pub mod opencode;
pub mod opencodego;
pub mod openrouter;
pub mod perplexity;
pub mod poe;
pub mod qoder;
pub mod sakana;
pub mod stepfun;
pub mod t3chat;
pub mod venice;
pub mod vertexai;
pub mod warp;
pub mod wayfinder;
pub mod windsurf;
pub mod zai;
pub mod zed;

// Re-export provider implementations
pub use abacus::AbacusProvider;
pub use alibaba::{AlibabaProvider, AlibabaRegion};
pub use alibabatokenplan::AlibabaTokenPlanProvider;
pub use amp::AmpProvider;
pub use antigravity::AntigravityProvider;
pub use augment::AugmentProvider;
pub use azureopenai::AzureOpenAIProvider;
pub use bedrock::BedrockProvider;
pub use chutes::ChutesProvider;
pub use claude::ClaudeProvider;
pub use codebuff::CodebuffProvider;
pub use codex::CodexProvider;
pub use commandcode::CommandCodeProvider;
pub use copilot::CopilotProvider;
pub use crof::CrofProvider;
pub use crossmodel::CrossModelProvider;
pub use cursor::CursorProvider;
pub use deepgram::DeepgramProvider;
pub use deepseek::DeepSeekProvider;
pub use deepseek_harness::DeepSeekHarnessProvider;
pub use devin::DevinProvider;
pub use doubao::DoubaoProvider;
pub use elevenlabs::ElevenLabsProvider;
pub use factory::FactoryProvider;
pub use gemini::GeminiProvider;
pub use grok::GrokProvider;
pub use groq::GroqProvider;
pub use infini::InfiniProvider;
pub use jetbrains::JetBrainsProvider;
pub use kilo::KiloProvider;
pub use kimi::KimiProvider;
pub use kimik2::KimiK2Provider;
pub use kiro::KiroProvider;
pub use litellm::LiteLLMProvider;
pub use llmproxy::LLMProxyProvider;
pub use manus::ManusProvider;
pub use mimo::MiMoProvider;
pub use minimax::{MiniMaxProvider, MiniMaxRegion};
pub use mistral::MistralProvider;
pub use nanogpt::NanoGPTProvider;
pub use ollama::OllamaProvider;
pub use openaiapi::OpenAIApiProvider;
pub use opencode::OpenCodeProvider;
pub use opencodego::OpenCodeGoProvider;
pub use openrouter::OpenRouterProvider;
pub use perplexity::PerplexityProvider;
pub use poe::PoeProvider;
pub use qoder::QoderProvider;
pub use sakana::SakanaProvider;
pub use stepfun::StepFunProvider;
pub use t3chat::T3ChatProvider;
pub use venice::VeniceProvider;
pub use vertexai::VertexAIProvider;
pub use warp::WarpProvider;
pub use wayfinder::WayfinderProvider;
pub use windsurf::WindsurfProvider;
pub use zai::ZaiProvider;
pub use zed::ZedProvider;

pub(crate) fn browser_cookie_header(
    _domains: &[&str],
) -> Result<String, crate::core::ProviderError> {
    // Browser database extraction is intentionally disabled. Chromium's
    // App-Bound Encryption makes it unreliable, and silently probing browser
    // profiles conflicts with the explicit manual-cookie UX.
    Err(crate::core::ProviderError::NoCookies)
}

pub(crate) fn resolve_api_key(
    explicit: Option<&str>,
    credential_target: &str,
    env_names: &[&str],
) -> Result<String, crate::core::ProviderError> {
    if let Some(key) = explicit
        && !key.trim().is_empty()
    {
        return Ok(key.trim().to_string());
    }
    if let Some(key) =
        crate::keychain::get_secret(crate::keychain::Scope::Any, credential_target, "api_key")
    {
        return Ok(key);
    }
    for env in env_names {
        if let Ok(key) = std::env::var(env)
            && !key.trim().is_empty()
        {
            return Ok(key);
        }
    }
    Err(crate::core::ProviderError::NotInstalled(format!(
        "API key not found. Set {} in Preferences or environment.",
        env_names.join(" / ")
    )))
}

/// Delete provider-owned copies of live tokens that survive the shared
/// credential files. `revoke_managed_credentials` calls this so settings does
/// not hardcode a vendor. Today only StepFun writes a refreshed token to the
/// OS keyring (SBS-920). Other providers only *read* keyring entries the user
/// or another app placed there; those are left alone.
pub(crate) fn clear_persisted_credentials(provider: crate::core::ProviderId) -> anyhow::Result<()> {
    match provider {
        crate::core::ProviderId::StepFun => stepfun::clear_persisted_credentials(),
        _ => Ok(()),
    }
}

/// Whether a parsed URL points at the local machine.
///
/// Host-based, never prefix-based: `http://localhost@evil.example` and
/// `http://127.0.0.1.evil.example` both *start with* a loopback literal while
/// resolving somewhere else entirely.
pub(crate) fn is_loopback_url(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_matches(['[', ']']);
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

pub(crate) fn validated_https_url(
    raw: &str,
    label: &str,
) -> Result<reqwest::Url, crate::core::ProviderError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(crate::core::ProviderError::Other(format!(
            "{label} URL is empty"
        )));
    }
    let lower = trimmed.to_ascii_lowercase();
    if ["%2f", "%5c", "%3f", "%23", "%40", "%3a"]
        .iter()
        .any(|encoded| lower.contains(encoded))
    {
        return Err(crate::core::ProviderError::Other(format!(
            "{label} URL must not contain encoded host delimiters"
        )));
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let url = reqwest::Url::parse(&candidate)
        .map_err(|e| crate::core::ProviderError::Other(format!("Invalid {label} URL: {e}")))?;
    let host = url.host_str().ok_or_else(|| {
        crate::core::ProviderError::Other(format!("{label} URL must include a host"))
    })?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || host.contains('%')
        || host.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(crate::core::ProviderError::Other(format!(
            "{label} URL must use HTTPS without user info or encoded host tricks"
        )));
    }
    Ok(url)
}

#[cfg(test)]
mod browser_cookie_policy_tests {
    use super::*;

    #[test]
    fn provider_browser_cookie_fallback_is_disabled() {
        assert!(matches!(
            browser_cookie_header(&["example.com"]),
            Err(crate::core::ProviderError::NoCookies)
        ));
    }

    #[test]
    fn clear_persisted_credentials_is_noop_for_providers_without_a_shadow_store() {
        assert!(clear_persisted_credentials(crate::core::ProviderId::Claude).is_ok());
        assert!(clear_persisted_credentials(crate::core::ProviderId::Codex).is_ok());
    }

    /// SBS-1023: a keyring-resident API key must not authenticate when the
    /// user disabled keychain access. Without the gate this returns the mock
    /// secret.
    #[test]
    fn resolve_api_key_skips_keyring_when_disabled() {
        let _guard = crate::keychain::with_mock_store(
            false,
            &[("ceiling-sbs-1023", "api_key", "from-keyring")],
        );
        let error = resolve_api_key(None, "ceiling-sbs-1023", &[]).expect_err("skip");
        assert!(matches!(error, crate::core::ProviderError::NotInstalled(_)));
    }

    /// SBS-1023: the same mock secret is used when access is allowed, so the
    /// skip test above is not a missing-entry false pass.
    #[test]
    fn resolve_api_key_uses_keyring_when_enabled() {
        let _guard = crate::keychain::with_mock_store(
            true,
            &[("ceiling-sbs-1023", "api_key", "from-keyring")],
        );
        assert_eq!(
            resolve_api_key(None, "ceiling-sbs-1023", &[]).expect("keyring"),
            "from-keyring"
        );
    }
}
