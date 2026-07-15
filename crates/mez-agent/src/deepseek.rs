//! Provider-independent DeepSeek endpoint and protocol policy.
//!
//! This module owns deterministic DeepSeek API behavior that does not require
//! credentials, HTTP metadata, transport, quota attachment, or product error
//! projection. Request and response semantics will move here incrementally as
//! the root transport adapter is reduced.

use crate::{ProviderEndpointError, ProviderEndpointResult};

/// Derives the DeepSeek Chat Completions endpoint from a configured base URL.
pub fn deepseek_chat_completions_endpoint_for_base_url(
    base_url: &str,
) -> ProviderEndpointResult<String> {
    let base_url = deepseek_base_url(base_url)?;
    if base_url.ends_with("/chat/completions") {
        return Ok(base_url);
    }
    if let Some(prefix) = base_url.strip_suffix("/models") {
        return Ok(format!("{prefix}/chat/completions"));
    }
    Ok(format!("{base_url}/chat/completions"))
}

/// Derives the DeepSeek Models endpoint from a configured base URL or Chat
/// Completions endpoint.
pub fn deepseek_models_endpoint_for_base_url(base_url: &str) -> ProviderEndpointResult<String> {
    let chat_endpoint = deepseek_chat_completions_endpoint_for_base_url(base_url)?;
    Ok(chat_endpoint.replace("/chat/completions", "/models"))
}

fn deepseek_base_url(base_url: &str) -> ProviderEndpointResult<String> {
    if base_url.trim().is_empty() {
        return Err(ProviderEndpointError::invalid_args(
            "DeepSeek provider base URL must not be empty",
        ));
    }
    Ok(base_url.trim().trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies configured DeepSeek base URLs expand to the current documented
    /// Chat Completions and Models endpoints.
    ///
    /// User-facing configuration names this setting `base_url`, so callers must
    /// be able to provide `https://api.deepseek.com` exactly as shown in the
    /// DeepSeek SDK examples. Existing endpoint URLs remain accepted so tests
    /// and advanced users can still target a proxy or explicit route.
    #[test]
    fn deepseek_base_url_derives_documented_chat_and_models_endpoints() {
        assert_eq!(
            deepseek_chat_completions_endpoint_for_base_url("https://api.deepseek.com").unwrap(),
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(
            deepseek_chat_completions_endpoint_for_base_url("https://api.deepseek.com/").unwrap(),
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(
            deepseek_chat_completions_endpoint_for_base_url(
                "https://api.deepseek.com/chat/completions"
            )
            .unwrap(),
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(
            deepseek_chat_completions_endpoint_for_base_url("https://proxy.example/models")
                .unwrap(),
            "https://proxy.example/chat/completions"
        );
        assert_eq!(
            deepseek_models_endpoint_for_base_url("https://api.deepseek.com").unwrap(),
            "https://api.deepseek.com/models"
        );
    }
}
