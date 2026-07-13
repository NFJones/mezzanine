//! Provider failure retry and recovery classification.
//!
//! This module owns provider-domain interpretation of sanitized failure
//! diagnostics. Product error envelopes and async transport channels adapt
//! their error kinds into these dependency-neutral contracts.

/// Stable provider error categories needed by retry classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// Invalid model- or caller-authored arguments.
    InvalidArgs,
    /// Invalid provider or turn state.
    InvalidState,
    /// Invalid product configuration.
    Config,
    /// A transport or operating-system I/O failure.
    Io,
    /// A conflicting operation.
    Conflict,
    /// A missing provider-side resource.
    NotFound,
    /// A forbidden provider operation.
    Forbidden,
    /// An unsupported provider operation.
    NotImplemented,
}

/// Shared retry/recovery class for provider failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorRetryClass {
    /// The request exceeded the provider input context window.
    ContextLimit,
    /// The response exhausted the provider output-token budget.
    OutputLimit,
    /// The same request may be retried without a terminal failure summary.
    RetryableTransport,
    /// The provider failure should terminate the current recovery attempt.
    NonRetryable,
}

/// Classifies sanitized provider failure fields for recovery and retry policy.
///
/// Context and output limits take precedence over generic transport retries.
/// Unsupported status-400 failures remain terminal, while rate limits,
/// server errors, transient provider types, and explicit retry invitations are
/// retryable.
pub fn classify_provider_error_retry(
    kind: ProviderErrorKind,
    message: &str,
    provider_failure_json: Option<&str>,
) -> ProviderErrorRetryClass {
    if provider_error_is_context_limit_exceeded(message, provider_failure_json) {
        return ProviderErrorRetryClass::ContextLimit;
    }
    if provider_error_is_output_limit_exceeded(message, provider_failure_json) {
        return ProviderErrorRetryClass::OutputLimit;
    }
    if provider_error_is_transient_overload_or_unavailable(message, provider_failure_json) {
        return ProviderErrorRetryClass::RetryableTransport;
    }
    if let Some(status_code) = provider_failure_status_code(provider_failure_json) {
        if status_code == 400
            && (message.contains("Unsupported") || message.contains("unsupported"))
        {
            return ProviderErrorRetryClass::NonRetryable;
        }
        if status_code == 429 || (500..=599).contains(&status_code) {
            return ProviderErrorRetryClass::RetryableTransport;
        }
        if provider_error_invites_retry(message, provider_failure_json) {
            return ProviderErrorRetryClass::RetryableTransport;
        }
        return ProviderErrorRetryClass::NonRetryable;
    }
    if kind == ProviderErrorKind::Io {
        return ProviderErrorRetryClass::RetryableTransport;
    }
    if kind != ProviderErrorKind::InvalidState {
        return ProviderErrorRetryClass::NonRetryable;
    }
    if message.contains("provider HTTP request failed")
        || message.contains("provider HTTP response read failed")
        || message.contains("provider HTTP response read stalled")
        || provider_error_invites_retry(message, provider_failure_json)
    {
        ProviderErrorRetryClass::RetryableTransport
    } else {
        ProviderErrorRetryClass::NonRetryable
    }
}

fn provider_failure_status_code(provider_failure_json: Option<&str>) -> Option<u16> {
    let value: serde_json::Value = serde_json::from_str(provider_failure_json?).ok()?;
    let status_code = value.get("status_code")?.as_u64()?;
    u16::try_from(status_code).ok()
}

fn provider_error_invites_retry(message: &str, provider_failure_json: Option<&str>) -> bool {
    provider_error_fields(
        message,
        provider_failure_json,
        &[
            "/error/message",
            "/message",
            "/body/error/message",
            "/body/message",
            "/response/error/message",
        ],
    )
    .any(|text| provider_error_text_invites_retry(&text))
}

fn provider_error_is_transient_overload_or_unavailable(
    message: &str,
    provider_failure_json: Option<&str>,
) -> bool {
    provider_error_fields(
        message,
        provider_failure_json,
        &[
            "/error/type",
            "/error/message",
            "/message",
            "/body/error/type",
            "/body/error/message",
            "/body/message",
            "/response/error/type",
            "/response/error/message",
        ],
    )
    .any(|text| provider_error_text_is_transient_overload_or_unavailable(&text))
}

fn provider_error_is_context_limit_exceeded(
    message: &str,
    provider_failure_json: Option<&str>,
) -> bool {
    provider_error_fields(
        message,
        provider_failure_json,
        &[
            "/error/code",
            "/error/type",
            "/error/message",
            "/message",
            "/body/error/code",
            "/body/error/type",
            "/body/error/message",
            "/body/message",
            "/response/error/code",
            "/response/error/type",
            "/response/error/message",
            "/response/incomplete_details/reason",
        ],
    )
    .any(|text| provider_error_text_is_context_limit_exceeded(&text))
}

fn provider_error_is_output_limit_exceeded(
    message: &str,
    provider_failure_json: Option<&str>,
) -> bool {
    provider_error_fields(
        message,
        provider_failure_json,
        &[
            "/incomplete_details/reason",
            "/response/incomplete_details/reason",
            "/body/incomplete_details/reason",
            "/body/response/incomplete_details/reason",
            "/error/code",
            "/error/message",
            "/message",
            "/body/error/code",
            "/body/error/message",
            "/body/message",
            "/response/error/code",
            "/response/error/message",
        ],
    )
    .any(|text| provider_error_text_is_output_limit_exceeded(&text))
}

fn provider_error_fields(
    message: &str,
    provider_failure_json: Option<&str>,
    pointers: &'static [&'static str],
) -> impl Iterator<Item = String> {
    let parsed = provider_failure_json
        .and_then(|failure| serde_json::from_str::<serde_json::Value>(failure).ok());
    std::iter::once(message.to_string()).chain(pointers.iter().filter_map(move |pointer| {
        parsed
            .as_ref()?
            .pointer(pointer)?
            .as_str()
            .map(str::to_string)
    }))
}

fn provider_error_text_invites_retry(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("you can retry your request")
        || lower.contains("you can retry the request")
        || (lower.contains("an error occurred while processing your request")
            && lower.contains("retry"))
}

fn provider_error_text_is_transient_overload_or_unavailable(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("api_error")
        || lower.contains("timeout_error")
        || lower.contains("rate_limit_error")
        || lower.contains("overloaded_error")
        || lower.contains("api overloaded")
        || lower.contains("server overloaded")
        || lower.contains("server is overloaded")
        || lower.contains("temporarily unavailable")
        || lower.contains("service unavailable")
        || (lower.contains("overloaded") && lower.contains("try again"))
}

fn provider_error_text_is_context_limit_exceeded(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("context_length_exceeded")
        || lower.contains("context length exceeded")
        || lower.contains("context_window_exceeded")
        || lower.contains("model_context_window_exceeded")
        || lower.contains("request_too_large")
        || lower.contains("exceeds the context window")
        || lower.contains("maximum context length")
        || lower.contains("max context length")
        || lower.contains("context window")
        || lower.contains("prompt is too long")
        || lower.contains("input is too large")
        || lower.contains("input too large")
        || lower.contains("too many input tokens")
        || lower.contains("too many tokens")
        || lower.contains("reduce the length of the messages")
        || lower.contains("reduce the length of your input")
        || lower.contains("request too large for the model")
}

fn provider_error_text_is_output_limit_exceeded(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("max_output_tokens")
        || lower.contains("max_tokens")
        || lower.contains("maximum output tokens")
        || lower.contains("output token limit")
        || lower.contains("output tokens limit")
        || lower.contains("response output limit")
}

#[cfg(test)]
mod tests {
    use super::{ProviderErrorKind, ProviderErrorRetryClass, classify_provider_error_retry};

    /// Verifies transport stalls remain retryable after classification moves
    /// below the product error-envelope adapter.
    #[test]
    fn response_read_stalls_are_retryable_transport_failures() {
        assert_eq!(
            classify_provider_error_retry(
                ProviderErrorKind::InvalidState,
                "provider HTTP response read stalled for 50ms while waiting for body chunk",
                None,
            ),
            ProviderErrorRetryClass::RetryableTransport
        );
    }

    /// Verifies explicit retry invitations override otherwise terminal status
    /// codes without weakening unsupported status-400 handling.
    #[test]
    fn retry_invitations_with_status_codes_are_honored() {
        assert_eq!(
            classify_provider_error_retry(
                ProviderErrorKind::InvalidState,
                "Chat Completions API returned status 409: you can retry your request",
                Some(r#"{"status_code":409,"error":{"message":"you can retry your request"}}"#),
            ),
            ProviderErrorRetryClass::RetryableTransport
        );
        assert_eq!(
            classify_provider_error_retry(
                ProviderErrorKind::InvalidState,
                "Chat Completions API returned status 400: Unsupported parameter",
                Some(r#"{"status_code":400,"error":{"message":"Unsupported parameter"}}"#),
            ),
            ProviderErrorRetryClass::NonRetryable
        );
    }

    /// Verifies structured provider status and error types map to the shared
    /// context, transport, and terminal recovery classes.
    #[test]
    fn structured_provider_failures_map_to_recovery_classes() {
        let cases = [
            (
                401,
                "authentication_error",
                ProviderErrorRetryClass::NonRetryable,
            ),
            (
                408,
                "timeout_error",
                ProviderErrorRetryClass::RetryableTransport,
            ),
            (
                413,
                "request_too_large",
                ProviderErrorRetryClass::ContextLimit,
            ),
            (
                429,
                "rate_limit_error",
                ProviderErrorRetryClass::RetryableTransport,
            ),
            (
                529,
                "overloaded_error",
                ProviderErrorRetryClass::RetryableTransport,
            ),
        ];
        for (status, error_type, expected) in cases {
            let failure = format!(
                r#"{{"status_code":{status},"error":{{"type":"{error_type}","message":"{error_type}"}}}}"#
            );
            assert_eq!(
                classify_provider_error_retry(
                    ProviderErrorKind::InvalidState,
                    error_type,
                    Some(&failure),
                ),
                expected,
                "{status} {error_type}"
            );
        }
    }
}
