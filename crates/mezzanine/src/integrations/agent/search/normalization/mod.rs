//! Strict future output validation, independent of vendor wire schemas and the
//! legacy DDG compatibility renderer. All projections derive from one immutable
//! normalized value. Whole hits are removed to fit complete JSON and text; URLs
//! and serialized JSON are never sliced. No output here authorizes publication,
//! resolves credentials or establishes a vendor-specific response schema.

use mez_agent::search::{SearchError, SearchLimits, SearchResult};
use crate::integrations::http::{HttpCompletion, HttpExchangeResponse};

#[cfg(test)]
mod tests;

/// Immutable validated result. Debug omits the untrusted corpus and remote IDs.
pub struct NormalizedSearchResult(SearchResult);

impl std::fmt::Debug for NormalizedSearchResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("NormalizedSearchResult([redacted])") }
}

impl NormalizedSearchResult {
    /// Read-only facts shared by text and JSON, including final retained order.
    pub fn result(&self) -> &SearchResult { &self.0 }
    /// Complete bounded JSON, never truncated JSON or a duplicate text corpus.
    pub fn to_json(&self) -> Result<String, SearchError> { serde_json::to_string(&self.0).map_err(|_| SearchError::MalformedJson) }
    /// Complete bounded text with whole citation URLs and provenance labels.
    pub fn to_text(&self) -> String { strict_text(&self.0) }
}

/// Decodes a complete bounded JSON representation of the *shared contract*.
/// This is NOT a vendor schema decoder, credential scrubber, approval check or
/// publishable output: callers must normalize and apply their trust boundary.
/// Truncated JSON is always failure, even when its retained prefix is valid.
pub fn decode_complete_json(exchange: &HttpExchangeResponse, limits: SearchLimits) -> Result<SearchResult, SearchError> {
    let response = &exchange.response;
    match response.status_code {
        202 => return Err(SearchError::Pending),
        401 => return Err(SearchError::Authentication),
        403 => return Err(SearchError::Forbidden),
        402 => return Err(SearchError::PaymentRequired),
        429 => return Err(SearchError::RateLimited),
        200..=299 => {},
        status => return Err(SearchError::Http(status)),
    }
    if exchange.completion != HttpCompletion::Complete { return Err(SearchError::Incomplete); }
    if response.body.len() > limits.transport_bytes() { return Err(SearchError::Oversize); }
    serde_json::from_str(&response.body).map_err(|_| SearchError::MalformedJson)
}

/// Strictly validates ordered hits and optional facts with UTF-8 safe per-field,
/// list and final budgets. Invalid URLs fail, never become empty search success.
/// Titles/excerpts/warnings may be clipped; citations are rejected if oversized.
/// If metadata alone cannot fit, returns InvalidBounds instead of looping or
/// emitting partial JSON. This function is NOT used by the legacy DDG path.
pub fn normalize_strict(mut result: SearchResult, limits: SearchLimits) -> Result<NormalizedSearchResult, SearchError> {
    result.effective_count = limits.results();
    if result.hits.len() > limits.results() { result.hits.truncate(limits.results()); result.truncated = true; }
    for hit in &mut result.hits {
        validate_citation(&hit.url)?;
        result.truncated |= bound_text(&mut hit.title, 4096);
        if let Some(excerpt) = &mut hit.excerpt { result.truncated |= bound_text(&mut excerpt.text, 8192); }
    }
    if result.warnings.len() > 16 { result.warnings.truncate(16); result.truncated = true; }
    for warning in &mut result.warnings { result.truncated |= bound_text(warning, 512); }
    if let Some(id) = &result.request_id {
        if id.is_empty() || id.len() > 128 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b)) { return Err(SearchError::InvalidRequest); }
    }
    if let Some(usage) = &result.usage {
        if usage.unit.is_empty() || usage.unit.len() > 64 || !usage.unit.bytes().all(|b| b.is_ascii_alphanumeric() || b" _-".contains(&b)) { return Err(SearchError::InvalidRequest); }
    }
    loop {
        let json = serde_json::to_vec(&result).map_err(|_| SearchError::MalformedJson)?;
        if json.len() <= limits.normalized_bytes() && strict_text(&result).len() <= limits.normalized_bytes() {
            return Ok(NormalizedSearchResult(result));
        }
        if result.hits.pop().is_none() { return Err(SearchError::InvalidBounds); }
        result.truncated = true;
    }
}

/// Includes non-corpus facts in the selected text projection. The metadata line
/// never duplicates titles/excerpts/URLs, but retains source-date interpretation
/// by one-based hit index. Unknown optional facts are omitted, not invented.
fn strict_text(result: &SearchResult) -> String {
    let mut metadata = serde_json::json!({
        "requested_count": result.requested_count,
        "effective_count": result.effective_count,
        "returned_count": result.hits.len(),
        "domains": result.domains, "dates": result.dates,
        "warnings": result.warnings, "truncated": result.truncated,
    });
    if let Some(id) = &result.request_id { metadata["request_id"] = serde_json::json!(id); }
    if let Some(usage) = &result.usage { metadata["usage"] = serde_json::json!(usage); }
    let dates: Vec<_> = result.hits.iter().enumerate().filter_map(|(index, hit)| hit.source_date.as_ref().map(|date| (index + 1, date))).collect();
    if !dates.is_empty() { metadata["source_dates"] = serde_json::json!(dates); }
    let hits = result.render_text();
    format!("[mez: search {metadata}]\n{}", if hits.is_empty() { "No retained search hits." } else { &hits })
}

/// Uses the existing product URL parser, rejecting parser repair of whitespace,
/// control bytes, backslashes, missing authorities and any userinfo. Keep the
/// original complete citation rather than silently canonicalizing/clipping it.
fn validate_citation(value: &str) -> Result<(), SearchError> {
    if value.is_empty() || value.len() > 8192 || value.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\\') { return Err(SearchError::InvalidUrl); }
    let url = reqwest::Url::parse(value).map_err(|_| SearchError::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() { return Err(SearchError::InvalidUrl); }
    let Some((_, rest)) = value.split_once("://") else { return Err(SearchError::InvalidUrl); };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') { return Err(SearchError::InvalidUrl); }
    Ok(())
}

/// Bounds one field without splitting UTF-8. Control characters are rendered as
/// spaces so remote fields cannot forge new citation/provenance lines.
fn bound_text(value: &mut String, max: usize) -> bool {
    *value = value.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if value.len() <= max { return false; }
    let mut end = max;
    while !value.is_char_boundary(end) { end -= 1; }
    value.truncate(end);
    true
}
