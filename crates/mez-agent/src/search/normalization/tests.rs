//! Pure normalization checks do not substitute a parser for real URL tests.
//! The product suite separately exercises its existing structural URL parser
//! and typed local HTTP-completion conversion through these same public APIs.

use super::*;
use crate::search::{SearchHit, SearchRequest};

/// Builds one known-valid local citation with no invented optional facts.
fn result() -> SearchResult {
    SearchResult::legacy_ddg(
        &SearchRequest {
            query: "fixture".into(),
            domains: vec![],
            recency_days: None,
            max_results: None,
        },
        vec![SearchHit {
            title: "é".repeat(3000),
            url: "https://example.test/a".into(),
            excerpt: None,
            source_date: None,
        }],
        false,
    )
}

#[test]
/// Complete JSON/text budgets account for UTF-8, metadata and dropped-all
/// disclosure. Tiny budgets fail rather than returning malformed output.
fn pure_normalization_enforces_complete_projection_budgets() {
    for bytes in [1, 180, 300, 1024, 32 * 1024, 256 * 1024] {
        let limits = SearchLimits::strict(None, 3, Some(bytes)).unwrap();
        match normalize_strict_with_url_validator(result(), limits, |_| Ok(())) {
            Ok(normalized) => {
                assert!(normalized.to_json().unwrap().len() <= bytes);
                assert!(normalized.to_text().len() <= bytes);
                assert!(normalized.to_text().contains("\"effective_count\":3"));
                assert!(normalized.result().truncated);
                if normalized.result().hits.is_empty() {
                    assert!(normalized.to_text().contains("No retained search hits."));
                }
            }
            Err(error) => assert_eq!(error, SearchError::InvalidBounds),
        }
    }
}

#[test]
/// A trusted parser's rejection propagates and lower lexical guards cannot be
/// bypassed by an accepting parser callback. This is not an I/O extension.
fn pure_normalization_requires_the_structural_validator() {
    let limits = SearchLimits::strict(None, 50, None).unwrap();
    assert!(matches!(
        normalize_strict_with_url_validator(result(), limits, |_| Err(SearchError::InvalidUrl)),
        Err(SearchError::InvalidUrl)
    ));
    for url in [
        "javascript:bad",
        "https://user@example.test",
        "https:///repair.test",
        "https://example.test/\n",
    ] {
        let mut value = result();
        value.hits[0].url = url.into();
        assert!(matches!(
            normalize_strict_with_url_validator(value, limits, |_| Ok(())),
            Err(SearchError::InvalidUrl)
        ));
    }
}

#[test]
/// Locally observed completion gates only successful body decoding; known
/// pending/auth/payment/throttle statuses remain distinct even without a body.
fn pure_json_decoder_keeps_status_and_completion_distinct() {
    let limits = SearchLimits::strict(None, 50, None).unwrap();
    let mut response = ProviderHttpResponse {
        status_code: 200,
        headers: Default::default(),
        body: serde_json::to_string(&result()).unwrap(),
    };
    assert!(decode_complete_json(&response, true, limits).is_ok());
    assert!(matches!(
        decode_complete_json(&response, false, limits),
        Err(SearchError::Incomplete)
    ));
    for (status, error) in [
        (202, SearchError::Pending),
        (401, SearchError::Authentication),
        (402, SearchError::PaymentRequired),
        (403, SearchError::Forbidden),
        (429, SearchError::RateLimited),
    ] {
        response.status_code = status;
        assert!(
            matches!(decode_complete_json(&response, false, limits), Err(found) if found == error)
        );
    }
    response.status_code = 200;
    response.body = "{}".into();
    assert!(matches!(
        decode_complete_json(&response, true, limits),
        Err(SearchError::MalformedJson)
    ));
}
