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

impl ProviderErrorKind {
    /// Returns the stable snake-case identifier used across provider worker
    /// event channels.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgs => "invalid_args",
            Self::InvalidState => "invalid_state",
            Self::Config => "config",
            Self::Io => "io",
            Self::Conflict => "conflict",
            Self::NotFound => "not_found",
            Self::Forbidden => "forbidden",
            Self::NotImplemented => "not_implemented",
        }
    }

    /// Parses a stable provider worker event identifier.
    ///
    /// Both canonical snake-case identifiers and legacy Rust variant names
    /// are accepted. Unknown identifiers return `None` so the product adapter
    /// can apply its fail-closed error policy.
    pub fn from_event_name(name: &str) -> Option<Self> {
        match name {
            "invalid_args" | "InvalidArgs" => Some(Self::InvalidArgs),
            "invalid_state" | "InvalidState" => Some(Self::InvalidState),
            "config" | "Config" => Some(Self::Config),
            "io" | "Io" => Some(Self::Io),
            "conflict" | "Conflict" => Some(Self::Conflict),
            "not_found" | "NotFound" => Some(Self::NotFound),
            "forbidden" | "Forbidden" => Some(Self::Forbidden),
            "not_implemented" | "NotImplemented" => Some(Self::NotImplemented),
            _ => None,
        }
    }
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

/// Provider retry budget and bounded exponential-backoff policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRetryPolicy {
    /// Maximum retries accepted after the initial provider failure.
    pub max_attempts: u32,
    /// Whether retryable failures bypass the finite attempt budget.
    pub unlimited: bool,
    /// Initial delay used for the first accepted retry.
    pub initial_delay_ms: u64,
    /// Maximum delay applied after exponential growth.
    pub max_delay_ms: u64,
}

impl ProviderRetryPolicy {
    /// Returns whether a failure class remains eligible under the recorded
    /// retry-attempt count.
    pub const fn should_retry(
        self,
        recorded_attempts: u64,
        retry_class: ProviderErrorRetryClass,
    ) -> bool {
        let retryable = matches!(
            retry_class,
            ProviderErrorRetryClass::ContextLimit
                | ProviderErrorRetryClass::OutputLimit
                | ProviderErrorRetryClass::RetryableTransport
        );
        let budget_available = recorded_attempts < self.max_attempts as u64
            || (self.unlimited
                && matches!(retry_class, ProviderErrorRetryClass::RetryableTransport));
        retryable && budget_available
    }

    /// Returns one bounded, jittered delay for a one-based retry attempt.
    ///
    /// `jitter_sample` is injected so callers can use runtime randomness while
    /// tests remain deterministic. Provider advice is treated as a minimum
    /// after both local backoff and advice are capped by `max_delay_ms`.
    pub fn delay_ms(
        self,
        attempt: u64,
        advised_delay_ms: Option<u64>,
        jitter_sample: Option<u64>,
    ) -> u64 {
        let exponent = attempt.saturating_sub(1).min(10) as u32;
        let exponential_delay = self
            .initial_delay_ms
            .saturating_mul(2u64.saturating_pow(exponent))
            .min(self.max_delay_ms);
        let local_delay = if exponential_delay == self.max_delay_ms {
            exponential_delay
        } else {
            jitter_sample.map_or(exponential_delay, |jitter_sample| {
                let jitter_floor = exponential_delay / 2;
                let jitter_span = exponential_delay.saturating_sub(jitter_floor);
                jitter_floor.saturating_add(jitter_sample % jitter_span.saturating_add(1))
            })
        };
        local_delay.max(advised_delay_ms.unwrap_or(0).min(self.max_delay_ms))
    }
}

/// Canonical runtime provider retry budget and backoff settings.
pub const DEFAULT_PROVIDER_RETRY_POLICY: ProviderRetryPolicy = ProviderRetryPolicy {
    max_attempts: 5,
    unlimited: false,
    initial_delay_ms: 1_000,
    max_delay_ms: 15 * 60 * 1_000,
};

/// Parses provider `Retry-After` advice from a sanitized failure payload.
///
/// Delta-seconds and HTTP-date forms are supported. `now_unix_ms` is injected
/// to make date handling deterministic. Malformed values return `None`; past
/// dates normalize to an immediate zero-millisecond advisory delay.
pub fn provider_retry_after_delay_ms(
    provider_failure_json: Option<&str>,
    now_unix_ms: u64,
) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(provider_failure_json?).ok()?;
    let retry_after = value.get("retry_after")?;
    if let Some(seconds) = retry_after.as_u64() {
        return Some(seconds.saturating_mul(1_000));
    }
    let text = retry_after.as_str()?.trim();
    if let Ok(seconds) = text.parse::<u64>() {
        return Some(seconds.saturating_mul(1_000));
    }
    let advised = httpdate::parse_http_date(text)
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some(
        u64::try_from(advised)
            .unwrap_or(u64::MAX)
            .saturating_sub(now_unix_ms),
    )
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
        || message.contains("provider HTTP timeout phase=")
        || message.contains("provider stream response did not contain SSE data events")
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
    use super::{
        DEFAULT_PROVIDER_RETRY_POLICY, ProviderErrorKind, ProviderErrorRetryClass,
        ProviderRetryPolicy, classify_provider_error_retry, provider_retry_after_delay_ms,
    };

    /// Verifies retry eligibility accepts recoverable classes only while the
    /// canonical budget remains available.
    #[test]
    fn provider_retry_policy_bounds_eligible_failures() {
        assert!(
            DEFAULT_PROVIDER_RETRY_POLICY
                .should_retry(0, ProviderErrorRetryClass::RetryableTransport)
        );
        assert!(
            DEFAULT_PROVIDER_RETRY_POLICY.should_retry(4, ProviderErrorRetryClass::ContextLimit)
        );
        assert!(
            !DEFAULT_PROVIDER_RETRY_POLICY.should_retry(5, ProviderErrorRetryClass::OutputLimit)
        );
        assert!(
            !DEFAULT_PROVIDER_RETRY_POLICY.should_retry(0, ProviderErrorRetryClass::NonRetryable)
        );
    }

    /// Verifies unlimited retry mode bypasses only the finite attempt budget
    /// while non-retryable provider failures remain terminal.
    #[test]
    fn provider_retry_policy_supports_explicit_unlimited_mode() {
        let policy = ProviderRetryPolicy {
            max_attempts: 2,
            unlimited: true,
            initial_delay_ms: 1_000,
            max_delay_ms: 900_000,
        };

        assert!(policy.should_retry(u64::MAX, ProviderErrorRetryClass::RetryableTransport));
        assert!(!policy.should_retry(u64::MAX, ProviderErrorRetryClass::ContextLimit));
        assert!(!policy.should_retry(u64::MAX, ProviderErrorRetryClass::OutputLimit));
        assert!(!policy.should_retry(u64::MAX, ProviderErrorRetryClass::NonRetryable));
    }

    /// Verifies exponential delays are one-based, jittered deterministically,
    /// honor bounded provider advice, and saturate at the canonical cap.
    #[test]
    fn provider_retry_policy_bounds_exponential_delay() {
        assert_eq!(
            DEFAULT_PROVIDER_RETRY_POLICY.delay_ms(0, None, Some(0)),
            500
        );
        assert_eq!(DEFAULT_PROVIDER_RETRY_POLICY.delay_ms(1, None, None), 1_000);
        assert_eq!(
            DEFAULT_PROVIDER_RETRY_POLICY.delay_ms(2, None, Some(0)),
            1_000
        );
        assert_eq!(
            DEFAULT_PROVIDER_RETRY_POLICY.delay_ms(2, Some(1_750), Some(0)),
            1_750
        );
        assert_eq!(
            DEFAULT_PROVIDER_RETRY_POLICY.delay_ms(1, Some(u64::MAX), Some(0)),
            900_000
        );
        assert_eq!(
            DEFAULT_PROVIDER_RETRY_POLICY.delay_ms(u64::MAX, None, Some(0)),
            900_000
        );
    }

    /// Verifies retry advice accepts delta-seconds and HTTP dates while
    /// malformed and past values remain safe and deterministic.
    #[test]
    fn provider_retry_after_parses_supported_bounded_inputs() {
        assert_eq!(
            provider_retry_after_delay_ms(Some(r#"{"retry_after":"12"}"#), 0),
            Some(12_000)
        );
        assert_eq!(
            provider_retry_after_delay_ms(
                Some(r#"{"retry_after":"Thu, 01 Jan 1970 00:00:20 GMT"}"#),
                5_000,
            ),
            Some(15_000)
        );
        assert_eq!(
            provider_retry_after_delay_ms(
                Some(r#"{"retry_after":"Thu, 01 Jan 1970 00:00:01 GMT"}"#),
                5_000,
            ),
            Some(0)
        );
        assert_eq!(
            provider_retry_after_delay_ms(Some(r#"{"retry_after":"later"}"#), 0),
            None
        );
    }

    /// Verifies provider worker event identifiers remain stable while legacy
    /// variant names continue to decode during rolling runtime transitions.
    #[test]
    fn provider_error_kinds_have_stable_event_names() {
        let cases = [
            (
                ProviderErrorKind::InvalidArgs,
                "invalid_args",
                "InvalidArgs",
            ),
            (
                ProviderErrorKind::InvalidState,
                "invalid_state",
                "InvalidState",
            ),
            (ProviderErrorKind::Config, "config", "Config"),
            (ProviderErrorKind::Io, "io", "Io"),
            (ProviderErrorKind::Conflict, "conflict", "Conflict"),
            (ProviderErrorKind::NotFound, "not_found", "NotFound"),
            (ProviderErrorKind::Forbidden, "forbidden", "Forbidden"),
            (
                ProviderErrorKind::NotImplemented,
                "not_implemented",
                "NotImplemented",
            ),
        ];
        for (kind, canonical, legacy) in cases {
            assert_eq!(kind.as_str(), canonical);
            assert_eq!(ProviderErrorKind::from_event_name(canonical), Some(kind));
            assert_eq!(ProviderErrorKind::from_event_name(legacy), Some(kind));
        }
        assert_eq!(ProviderErrorKind::from_event_name("unknown"), None);
    }

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
        assert_eq!(
            classify_provider_error_retry(
                ProviderErrorKind::InvalidState,
                "provider HTTP timeout phase=total limit_ms=100 while waiting for provider response body progress",
                None,
            ),
            ProviderErrorRetryClass::RetryableTransport
        );
    }

    /// Verifies an otherwise successful streaming response with no SSE data
    /// events is retried through the bounded provider transport recovery path.
    #[test]
    fn empty_sse_streams_are_retryable_transport_failures() {
        assert_eq!(
            classify_provider_error_retry(
                ProviderErrorKind::InvalidState,
                "provider stream response did not contain SSE data events",
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
