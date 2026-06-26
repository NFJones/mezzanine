//! Anthropic Messages provider shell and endpoint helpers.
//!
//! This module owns the Anthropic-specific provider boundary built on top of
//! the shared Chat Completions transport shell. It defines the Anthropic
//! endpoint derivation rules, provider identity defaults, and current
//! first-pass skeleton behavior for request, response, and model-catalog
//! handling while keeping the parent module responsible for facade exports and
//! auth-store construction.

use super::chat_completions::ChatCompletionsDialect;
use super::{
    ANTHROPIC_MESSAGES_ENDPOINT, MezError, ModelRequest, ModelResponse, ProviderHttpRequest,
    ProviderHttpResponse, Result, validate_non_empty,
};

/// Chat Completions transport dialect implementation for Anthropic Messages.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnthropicMessagesDialect;

impl ChatCompletionsDialect for AnthropicMessagesDialect {
    /// Returns the default provider id used before configuration overrides are applied.
    fn default_provider_id(&self) -> &'static str {
        "anthropic"
    }

    /// Returns the default Anthropic Messages endpoint.
    fn default_chat_endpoint(&self) -> &'static str {
        ANTHROPIC_MESSAGES_ENDPOINT
    }

    /// Returns the human-readable provider label used in diagnostics.
    fn provider_label(&self) -> &'static str {
        "Anthropic"
    }

    /// Returns the diagnostic label used when validating a credential.
    fn credential_label(&self) -> &'static str {
        "Anthropic API key"
    }

    /// Derives the Anthropic Messages endpoint from a configured base URL.
    fn chat_endpoint_for_base_url(&self, base_url: &str) -> Result<String> {
        anthropic_messages_endpoint_for_base_url(base_url)
    }

    /// Builds one provider-specific Messages API request.
    fn build_chat_request(
        &self,
        _request: &ModelRequest,
        _api_key: Option<&str>,
        _endpoint: &str,
        _stream: bool,
        _timeout_ms: u64,
    ) -> Result<ProviderHttpRequest> {
        Err(MezError::invalid_state(
            "Anthropic provider request construction is not implemented yet",
        ))
    }

    /// Parses one successful provider-specific Messages API response.
    fn parse_chat_response(
        &self,
        _response: ProviderHttpResponse,
        _request: &ModelRequest,
        _provider_id: &str,
        _stream: bool,
    ) -> Result<ModelResponse> {
        Err(MezError::invalid_state(
            "Anthropic provider response parsing is not implemented yet",
        ))
    }

    /// Builds the provider-specific model catalog HTTP request.
    fn build_models_request(
        &self,
        _api_key: Option<&str>,
        _chat_endpoint: &str,
        _timeout_ms: u64,
    ) -> Result<ProviderHttpRequest> {
        Err(MezError::invalid_state(
            "Anthropic provider model listing is not implemented yet",
        ))
    }
}

/// Derives the Anthropic Messages endpoint from a configured base URL.
pub(super) fn anthropic_messages_endpoint_for_base_url(base_url: &str) -> Result<String> {
    validate_non_empty("Anthropic provider base URL", base_url)?;
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.ends_with("/v1/messages") || base_url.ends_with("/messages") {
        return Ok(base_url.to_string());
    }
    if base_url.ends_with("/v1") {
        return Ok(format!("{base_url}/messages"));
    }
    Ok(format!("{base_url}/v1/messages"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        AnthropicMessagesProvider, DEFAULT_PROVIDER_TIMEOUT_MS, ReqwestProviderHttpTransport,
    };
    use crate::auth::{AuthPaths, AuthStore};
    use crate::error::MezErrorKind;
    use std::fs;

    /// Verifies Anthropic base URL normalization accepts the documented root,
    /// versioned root, and full Messages endpoint forms without producing an
    /// OpenAI-compatible path.
    #[test]
    fn anthropic_base_url_derives_documented_messages_endpoints() {
        assert_eq!(
            anthropic_messages_endpoint_for_base_url("https://api.anthropic.com").unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            anthropic_messages_endpoint_for_base_url("https://api.anthropic.com/v1").unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            anthropic_messages_endpoint_for_base_url("https://api.anthropic.com/messages").unwrap(),
            "https://api.anthropic.com/messages"
        );
        assert_eq!(
            anthropic_messages_endpoint_for_base_url("https://api.anthropic.com/v1/messages")
                .unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
    }

    /// Verifies Anthropic provider construction scopes credentials to the
    /// configured provider id rather than only the literal `anthropic` name.
    #[test]
    fn anthropic_provider_from_auth_store_uses_configured_provider_id() {
        let root = std::env::temp_dir().join(format!(
            "mez-auth-anthropic-provider-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let store = AuthStore::new(AuthPaths::under_config_root(&root));
        let credential_store = store.file_credential_store("claude-prod").unwrap();
        store
            .login_with_api_key(
                "claude-prod",
                "default",
                "anthropic-test-key",
                &credential_store,
            )
            .unwrap();

        let provider = super::super::anthropic_provider_from_auth_store_with_provider_options(
            &store,
            "claude-prod",
            Some("https://api.anthropic.com/v1"),
            DEFAULT_PROVIDER_TIMEOUT_MS,
            ReqwestProviderHttpTransport,
        )
        .unwrap();

        assert_eq!(provider.provider_id(), "claude-prod");
        assert_eq!(provider.endpoint, "https://api.anthropic.com/v1/messages");

        let _ = fs::remove_dir_all(root);
    }

    /// Verifies direct Anthropic provider construction fails clearly when the
    /// configured provider id has no stored credential.
    #[test]
    fn anthropic_provider_from_auth_store_reports_missing_provider_credential() {
        let root = std::env::temp_dir().join(format!(
            "mez-auth-anthropic-missing-provider-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let store = AuthStore::new(AuthPaths::under_config_root(&root));

        let error = super::super::anthropic_provider_from_auth_store_with_provider_options(
            &store,
            "claude-prod",
            None,
            DEFAULT_PROVIDER_TIMEOUT_MS,
            ReqwestProviderHttpTransport,
        )
        .unwrap_err();

        assert_eq!(error.kind(), MezErrorKind::InvalidState);
        assert_eq!(
            error.message(),
            "Anthropic provider `claude-prod` requires an authenticated API key"
        );

        let _ = fs::remove_dir_all(root);
    }

    /// Verifies Anthropic providers can still be constructed without auth for
    /// test or proxy scenarios when callers explicitly use the shell type.
    #[test]
    fn anthropic_provider_without_auth_uses_default_endpoint() {
        let provider =
            AnthropicMessagesProvider::without_auth(ReqwestProviderHttpTransport).unwrap();

        assert_eq!(provider.provider_id(), "anthropic");
        assert_eq!(provider.endpoint, ANTHROPIC_MESSAGES_ENDPOINT);
    }
}
