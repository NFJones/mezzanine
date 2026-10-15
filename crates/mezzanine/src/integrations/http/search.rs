//! Authenticated-search HTTP foundation, not a search backend selector.
//!
//! Adapters supply already approved, credential-free payloads and a credential
//! resolved only after authorization. Endpoints and auth placement are owned
//! here, never by model/config base URLs. A single shared exchange enforces
//! strict routing, identity encoding, complete JSON and the caller deadline.
//! No progress is published before JSON validation and canary suppression.
//! Query authentication necessarily reaches the destination's URL logs; this
//! boundary cannot control remote retention or recognize arbitrary transformed
//! echoes. It rejects known raw, percent/form-encoded and JSON-decoded echoes.

use std::collections::BTreeMap;

use mez_agent::{ProviderHttpError, ProviderHttpRequest, ProviderHttpResult, ProviderHttpTimeouts};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

use super::{HttpCompletion, HttpExecution, HttpResponseMode, HttpRoutingPolicy, ReqwestProviderHttpTransport};

#[cfg(test)]
mod tests;

/// Code-owned destinations; there is no production base-URL override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), expect(dead_code, reason = "Fixed search endpoints await separately authorized backend consumers; no paid dispatch is activated here."))]
pub(crate) enum SearchEndpoint {
    /// Brave Web Search API.
    Brave,
    /// Tavily search API.
    Tavily,
    /// Exa search API.
    Exa,
    /// SerpApi search API (its credential is a query parameter).
    SerpApi,
}

#[cfg_attr(not(test), expect(dead_code, reason = "Endpoint identity and auth placement are a dormant search adapter boundary."))]
impl SearchEndpoint {
    /// Returns a credential-free code-owned endpoint for approval descriptions.
    pub(crate) const fn url(self) -> &'static str {
        match self {
            Self::Brave => "https://api.search.brave.com/res/v1/web/search",
            Self::Tavily => "https://api.tavily.com/search",
            Self::Exa => "https://api.exa.ai/search",
            Self::SerpApi => "https://serpapi.com/search.json",
        }
    }

    /// Returns this destination's documented credential placement.
    fn auth(self, key: &SecretString) -> WireAuth<'_> {
        match self {
            Self::Brave => WireAuth::Header("x-subscription-token", key),
            Self::Tavily => WireAuth::Bearer(key),
            Self::Exa => WireAuth::Header("x-api-key", key),
            Self::SerpApi => WireAuth::Query("api_key", key),
        }
    }
}

/// Approved payload, intentionally separate from the late credential.
///
/// This is transport input, not an approval decision or model-facing result.
/// Backend adapters own query/options contracts and authorization. Debug omits
/// the complete payload, including user query and body.
#[cfg_attr(not(test), expect(dead_code, reason = "Credential-free search input awaits separately authorized runtime adapters."))]
pub(crate) struct SearchHttpRequest {
    endpoint: SearchEndpoint,
    request: ProviderHttpRequest,
}

impl std::fmt::Debug for SearchHttpRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("SearchHttpRequest")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

#[cfg_attr(not(test), expect(dead_code, reason = "Search execution is qualified offline but not wired to paid backend selection."))]
impl SearchHttpRequest {
    /// Builds credential-free wire inputs for one fixed endpoint. A JSON body
    /// selects POST; otherwise GET. No custom headers, host or URL are accepted.
    pub(crate) fn new(
        endpoint: SearchEndpoint,
        query: &[(String, String)],
        json: Option<&Value>,
        timeouts: ProviderHttpTimeouts,
        max_response_bytes: usize,
    ) -> ProviderHttpResult<Self> {
        timeouts.validate()?;
        let mut url = reqwest::Url::parse(endpoint.url())
            .map_err(|_| ProviderHttpError::invalid_state("search endpoint is invalid"))?;
        if query.iter().any(|(name, _)| name.eq_ignore_ascii_case("api_key")) {
            return Err(ProviderHttpError::invalid_args("search credentials must be supplied separately"));
        }
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().map(|(key, value)| (key, value)));
        }
        Ok(Self {
            endpoint,
            request: ProviderHttpRequest {
                method: if json.is_some() { "POST" } else { "GET" }.into(),
                url: url.into(),
                headers: BTreeMap::from([
                    ("accept".into(), "application/json".into()),
                    ("content-type".into(), "application/json".into()),
                ]),
                body: json.map(Value::to_string).unwrap_or_default(),
                timeouts,
                max_response_bytes: Some(max_response_bytes),
            },
        })
    }

    /// Safe destination identity, distinct from authenticated wire material.
    /// The caller's approved action remains the authority for query/options.
    pub(crate) fn description(&self) -> &'static str {
        self.endpoint.url()
    }

    /// Executes once, without retry or fallback, under the caller's absolute
    /// deadline. Dropping this future cancels the scoped request/body future.
    /// Only complete validated JSON can leave this boundary; raw headers/error
    /// bodies/request IDs are never published. Status diagnostics are numeric.
    pub(crate) async fn send_json(
        &self,
        credential: &SecretString,
        deadline: tokio::time::Instant,
    ) -> ProviderHttpResult<Value> {
        let deadline = deadline.min(tokio::time::Instant::now() + std::time::Duration::from_millis(self.request.timeouts.total_timeout_ms));
        let auth = self.endpoint.auth(credential);
        let exchange = ReqwestProviderHttpTransport.send_async_with_observers(
            &self.request,
            HttpExecution {
                mode: HttpResponseMode::SearchJson,
                routing: HttpRoutingPolicy::AuthenticatedSearch,
                deadline: Some(deadline),
                auth,
            },
            None,
            None,
        ).await?;
        check_deadline(deadline, self.request.timeouts)?;
        let result = checked_json(exchange, credential);
        check_deadline(deadline, self.request.timeouts)?;
        result
    }
}

/// Authentication exists only in the concrete product send boundary. No Debug,
/// serialization, user-defined header names, or model-facing representation.
pub(super) enum WireAuth<'a> {
    /// Legacy unauthenticated exchange; no wire changes.
    None,
    /// Endpoint-owned bearer placement.
    Bearer(&'a SecretString),
    /// Endpoint-owned named header placement.
    Header(&'static str, &'a SecretString),
    /// Endpoint-owned query placement, percent encoded as one value.
    Query(&'static str, &'a SecretString),
}

impl WireAuth<'_> {
    /// Applies credentials immediately before sending, after endpoint validation.
    pub(super) fn apply(&self, url: &mut reqwest::Url, headers: &mut reqwest::header::HeaderMap) -> ProviderHttpResult<()> {
        let (name, value) = match self {
            Self::None => return Ok(()),
            Self::Query(name, key) => {
                validate_credential(key)?;
                url.query_pairs_mut().append_pair(name, key.expose_secret());
                return Ok(());
            }
            Self::Bearer(key) => {
                validate_credential(key)?;
                ("authorization", format!("Bearer {}", key.expose_secret()))
            }
            Self::Header(name, key) => {
                validate_credential(key)?;
                (*name, key.expose_secret().to_string())
            }
        };
        let mut value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| ProviderHttpError::invalid_args("search credential is not a valid HTTP header"))?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::HeaderName::from_static(name), value);
        Ok(())
    }
}

/// Empty/control-bearing keys fail without rendering the supplied material.
fn validate_credential(key: &SecretString) -> ProviderHttpResult<()> {
    if key.expose_secret().is_empty() || key.expose_secret().chars().any(char::is_control) {
        return Err(ProviderHttpError::invalid_args("search credential is invalid"));
    }
    Ok(())
}

/// Enforces the private search transport URL invariant. Production callers can
/// only construct fixed endpoints; plaintext is available solely to unit tests.
pub(super) fn validate_wire_url(url: &reqwest::Url) -> ProviderHttpResult<()> {
    let secure = url.scheme() == "https";
    #[cfg(test)]
    let secure = secure || (url.scheme() == "http" && url.host_str().is_some_and(|host| host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())));
    if !secure || !url.username().is_empty() || url.password().is_some()
        || url.fragment().is_some() || url.as_str().split('/').nth(2).is_some_and(|authority| authority.contains('@')) {
        return Err(ProviderHttpError::invalid_args("search endpoint requires verified HTTPS without userinfo or fragment"));
    }
    Ok(())
}

/// Validates before returning anything that may enter model/transcript/audit or
/// subsequent fetch calls. Status/error paths never return remote payloads.
fn checked_json(exchange: super::HttpExchangeResponse, credential: &SecretString) -> ProviderHttpResult<Value> {
    if exchange.completion != HttpCompletion::Complete {
        return Err(ProviderHttpError::invalid_state("search JSON response is incomplete"));
    }
    let response = exchange.response;
    if !(200..300).contains(&response.status_code) {
        return Err(ProviderHttpError::invalid_state(format!("search HTTP status {}", response.status_code)));
    }
    let value: Value = serde_json::from_str(&response.body)
        .map_err(|_| ProviderHttpError::invalid_state("search response is not complete JSON"))?;
    if contains_credential(&response.body, credential) || json_contains_credential(&value, credential) {
        return Err(ProviderHttpError::invalid_state("search response suppressed: credential echo"));
    }
    Ok(value)
}

/// Detects exact raw and percent/form-encoded canaries, not heuristic key names.
fn contains_credential(value: &str, credential: &SecretString) -> bool {
    let secret = credential.expose_secret();
    if value.contains(secret) { return true; }
    [false, true].into_iter().any(|form| {
        let bytes = decode_canary_bytes(value.as_bytes(), form);
        bytes.windows(secret.len()).any(|window| window == secret.as_bytes())
    })
}

/// Percent decoding for canary comparison is byte-based: unrelated invalid
/// UTF-8 escapes cannot hide a valid encoded secret elsewhere in the field.
fn decode_canary_bytes(value: &[u8], form: bool) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        if value[index] == b'%' && index + 2 < value.len()
            && let (Some(high), Some(low)) = (hex_digit(value[index + 1]), hex_digit(value[index + 2])) {
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(if form && value[index] == b'+' { b' ' } else { value[index] });
            index += 1;
        }
    }
    decoded
}

/// Converts one ASCII percent-encoding digit without Unicode or payload errors.
fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Synchronous JSON/echo work is byte/depth bounded and cannot be preempted;
/// never publish a result if that work exhausted the absolute response budget.
fn check_deadline(deadline: tokio::time::Instant, timeouts: ProviderHttpTimeouts) -> ProviderHttpResult<()> {
    if tokio::time::Instant::now() >= deadline {
        return Err(ProviderHttpError::timeout(mez_agent::ProviderHttpTimeoutPhase::Total, timeouts.total_timeout_ms, "validating the search response"));
    }
    Ok(())
}

/// JSON decoding exposes escaped strings/keys before canary inspection.
fn json_contains_credential(value: &Value, credential: &SecretString) -> bool {
    match value {
        Value::String(value) => contains_credential(value, credential),
        Value::Array(values) => values.iter().any(|value| json_contains_credential(value, credential)),
        Value::Object(values) => values.iter().any(|(key, value)| contains_credential(key, credential) || json_contains_credential(value, credential)),
        _ => false,
    }
}
