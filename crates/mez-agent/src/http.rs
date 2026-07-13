//! Provider-neutral HTTP request and response contracts.
//!
//! This module owns the data exchanged between provider request builders and
//! product-owned HTTP transports. Concrete clients, credentials, retries, and
//! transport error conversion remain in the Mezzanine composition crate.

use std::collections::BTreeMap;

/// Maximum provider response body retained by the shared transport.
pub const DEFAULT_PROVIDER_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Default per-read stall timeout for long-running provider responses.
pub const DEFAULT_PROVIDER_TIMEOUT_MS: u64 = 30 * 60 * 1000;

/// Provider-neutral HTTP request assembled by a model-provider adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHttpRequest {
    /// HTTP method.
    pub method: String,
    /// Absolute provider endpoint.
    pub url: String,
    /// Request headers, including any credentials supplied by the product.
    pub headers: BTreeMap<String, String>,
    /// UTF-8 request body.
    pub body: String,
    /// Per-read stall timeout in milliseconds.
    pub timeout_ms: u64,
    /// Optional response-body bound requested by the caller.
    pub max_response_bytes: Option<usize>,
}

/// Provider-neutral HTTP response returned by a product-owned transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHttpResponse {
    /// HTTP status code.
    pub status_code: u16,
    /// Non-secret response headers retained for provider parsing.
    pub headers: BTreeMap<String, String>,
    /// UTF-8 response body, possibly bounded by the request limit.
    pub body: String,
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_PROVIDER_MAX_RESPONSE_BYTES, DEFAULT_PROVIDER_TIMEOUT_MS, ProviderHttpRequest,
        ProviderHttpResponse,
    };
    use std::collections::BTreeMap;

    #[test]
    /// Verifies provider HTTP values preserve the complete dependency-neutral
    /// transport envelope and stable response bounds.
    fn provider_http_contracts_preserve_transport_values() {
        let request = ProviderHttpRequest {
            method: "POST".to_string(),
            url: "https://provider.invalid/v1/responses".to_string(),
            headers: BTreeMap::from([("content-type".to_string(), "application/json".to_string())]),
            body: "{}".to_string(),
            timeout_ms: DEFAULT_PROVIDER_TIMEOUT_MS,
            max_response_bytes: Some(DEFAULT_PROVIDER_MAX_RESPONSE_BYTES),
        };
        assert_eq!(request.timeout_ms, 30 * 60 * 1000);
        assert_eq!(request.max_response_bytes, Some(16 * 1024 * 1024));

        let response = ProviderHttpResponse {
            status_code: 200,
            headers: BTreeMap::new(),
            body: "ok".to_string(),
        };
        assert_eq!(response.status_code, 200);
        assert_eq!(response.body, "ok");
    }
}
