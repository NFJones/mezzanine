//! Strict-contract failure tests use local canonical fixtures, not invented
//! provider wire APIs or external requests. Legacy DDG bypass is tested by the
//! unchanged network-action characterization suite.

use super::*;
use mez_agent::search::{ExcerptProvenance, SearchExcerpt, SearchHit, SearchRequest, ReportedSearchUsage};
use mez_agent::ProviderHttpResponse;

/// Makes honest optional-free hits in a stable order.
fn result() -> SearchResult {
    SearchResult::legacy_ddg(&SearchRequest { query: "fixture".into(), domains: vec![], recency_days: None, max_results: None }, vec![SearchHit { title: "First".into(), url: "https://example.test/one".into(), excerpt: None, source_date: None }, SearchHit { title: "Second".into(), url: "http://example.test/two".into(), excerpt: None, source_date: None }], false)
}

#[test]
/// Invalid citations fail rather than becoming an empty success; credentials,
/// parser repairs and clipped URLs are never accepted by the strict contract.
fn strict_urls_reject_nonweb_userinfo_and_malformed_values() {
    let limits = SearchLimits::strict(None, 50, None).unwrap();
    for invalid in ["file:///a", "javascript:alert(1)", "/relative", "https://user:pass@example.test", "https://@example.test", "https:///example.test", "https://exa mple.test", "https://example.test/\n", "https://example.test\\a", "https://", "https://[invalid]"] {
        let mut value = result(); value.hits[0].url = invalid.into();
        assert!(matches!(normalize_strict(value, limits), Err(SearchError::InvalidUrl)), "{invalid}");
    }
    let mut value = result(); value.hits[0].url = format!("https://example.test/{}", "a".repeat(8192));
    assert!(matches!(normalize_strict(value, limits), Err(SearchError::InvalidUrl)));
}

#[test]
/// Both projections share normalized Unicode fields, original stable order and
/// explicit snippet/extractive/generated labels; missing facts stay absent.
fn strict_provenance_unicode_and_optional_facts_share_one_value() {
    for provenance in [ExcerptProvenance::Snippet, ExcerptProvenance::Extractive, ExcerptProvenance::Generated] {
        let mut value = result();
        value.hits[0].title = "é".repeat(3000);
        value.hits[0].excerpt = Some(SearchExcerpt { text: "🦀".repeat(3000), provenance });
        let normalized = normalize_strict(value, SearchLimits::strict(None, 50, None).unwrap()).unwrap();
        assert!(normalized.result().truncated);
        assert_eq!(normalized.result().hits[0].title.len(), 4096);
        assert_eq!(normalized.result().hits[0].excerpt.as_ref().unwrap().text.len(), 8192);
        assert_eq!(normalized.result().hits[1].title, "Second");
        let json = normalized.to_json().unwrap();
        let round: SearchResult = serde_json::from_str(&json).unwrap();
        assert!(round == *normalized.result());
        assert!(normalized.to_text().contains(&format!("[{provenance:?}]")));
        assert!(!json.contains("source_date"));
        assert!(!json.contains("usage"));
    }
}

#[test]
/// Escaping overhead, metadata and text are included in every final budget;
/// whole trailing hits are dropped and no citation or JSON token is clipped.
fn strict_complete_serialized_budgets_account_for_escaping_and_metadata() {
    for bytes in [1, 180, 256, 512, 1024, 32 * 1024, 256 * 1024] {
        let limits = SearchLimits::strict(None, 50, Some(bytes)).unwrap();
        let mut value = result();
        value.hits[0].title = "\"\\".repeat(3000);
        value.warnings = vec!["warning".repeat(100); 20];
        match normalize_strict(value, limits) {
            Ok(normalized) => {
                let json = normalized.to_json().unwrap();
                assert!(json.len() <= bytes);
                assert!(normalized.to_text().len() <= bytes);
                assert!(serde_json::from_str::<SearchResult>(&json).is_ok());
                assert!(normalized.result().warnings.len() <= 16);
                for hit in &normalized.result().hits { assert!(hit.url == "https://example.test/one" || hit.url == "http://example.test/two"); }
            }
            Err(error) => assert_eq!(error, SearchError::InvalidBounds),
        }
    }
    let mut value = result(); value.hits = vec![value.hits[0].clone(); 60];
    let normalized = normalize_strict(value, SearchLimits::strict(None, 3, None).unwrap()).unwrap();
    assert_eq!(normalized.result().hits.len(), 3);
    assert_eq!(normalized.result().effective_count, 3);
    assert!(normalized.result().truncated);
}

#[test]
/// Remote metadata is optional and bounded, with no fabricated charges or
/// arbitrary untrusted request identifiers escaping into diagnostics.
fn strict_metadata_fields_have_independent_failure_bounds() {
    let limits = SearchLimits::strict(None, 50, None).unwrap();
    for id in ["bad\nid".to_string(), "x".repeat(129), String::new()] {
        let mut value = result(); value.request_id = Some(id);
        assert!(matches!(normalize_strict(value, limits), Err(SearchError::InvalidRequest)));
    }
    let mut value = result(); value.usage = Some(ReportedSearchUsage { units: 3, unit: "x".repeat(65) });
    assert!(matches!(normalize_strict(value, limits), Err(SearchError::InvalidRequest)));
    let mut value = result(); value.request_id = Some("request-123".into()); value.usage = Some(ReportedSearchUsage { units: 3, unit: "credits".into() });
    let normalized = normalize_strict(value, limits).unwrap();
    assert!(normalized.to_json().unwrap().contains("credits"));
    assert!(!format!("{normalized:?}").contains("request-123"));
}

#[test]
/// Complete local JSON is syntax/contract validation only. Truncated, oversized,
/// malformed, pending and paid HTTP errors never masquerade as empty success.
fn strict_complete_json_rejects_incomplete_and_http_error_cases() {
    let limits = SearchLimits::strict(None, 50, None).unwrap();
    let mut exchange = HttpExchangeResponse { response: ProviderHttpResponse { status_code: 200, headers: Default::default(), body: serde_json::to_string(&result()).unwrap() }, completion: HttpCompletion::Complete };
    assert!(decode_complete_json(&exchange, limits).is_ok());
    exchange.completion = HttpCompletion::Truncated;
    assert!(matches!(decode_complete_json(&exchange, limits), Err(SearchError::Incomplete)));
    exchange.completion = HttpCompletion::Complete;
    for body in ["", "{", "{}", "[]", "null", "{\"hits\":[]}"] { exchange.response.body = body.into(); assert!(matches!(decode_complete_json(&exchange, limits), Err(SearchError::MalformedJson))); }
    exchange.response.body = "x".repeat(1024 * 1024 + 1);
    assert!(matches!(decode_complete_json(&exchange, limits), Err(SearchError::Oversize)));
    exchange.response.body = "{}".into();
    for (status, expected) in [(202, SearchError::Pending), (401, SearchError::Authentication), (403, SearchError::Forbidden), (402, SearchError::PaymentRequired), (429, SearchError::RateLimited), (503, SearchError::Http(503))] {
        exchange.response.status_code = status;
        for completion in [HttpCompletion::Complete, HttpCompletion::HeadersOnly, HttpCompletion::Truncated] {
            exchange.completion = completion;
            for body in ["{}".into(), "x".repeat(1024 * 1024 + 1)] {
                exchange.response.body = body;
                assert!(matches!(decode_complete_json(&exchange, limits), Err(error) if error == expected));
            }
        }
    }
    exchange.response.status_code = 200;
    exchange.completion = HttpCompletion::HeadersOnly;
    assert!(matches!(decode_complete_json(&exchange, limits), Err(SearchError::Incomplete)));
}

#[test]
/// Selected text must disclose effective caps, filter strength, warnings, actual
/// source-date basis and dropped-all truncation without duplicating the corpus.
fn strict_text_discloses_material_facts_and_empty_retention() {
    let mut value = result();
    value.hits[0].source_date = Some(mez_agent::search::SearchSourceDate { date: mez_agent::search::UtcDate::parse("2025-01-01").unwrap(), basis: mez_agent::search::SourceDateBasis::Published });
    value.warnings.push("local fixture warning".into());
    let normalized = normalize_strict(value, SearchLimits::strict(None, 3, None).unwrap()).unwrap();
    let text = normalized.to_text();
    assert!(text.contains("\"effective_count\":3"));
    assert!(text.contains("\"returned_count\":2"));
    assert!(text.contains("not_requested"));
    assert!(text.contains("local fixture warning"));
    assert!(text.contains("published"));
    assert!(text.contains("2025-01-01"));
    assert_eq!(text.matches("https://example.test/one").count(), 1);
    let mut value = result(); value.hits[0].title = "x".repeat(4096); value.hits.truncate(1);
    let normalized = normalize_strict(value, SearchLimits::strict(None, 3, Some(300)).unwrap()).unwrap();
    assert!(normalized.result().hits.is_empty());
    assert!(normalized.to_text().contains("No retained search hits."));
    assert!(normalized.to_text().contains("\"truncated\":true"));
    assert!(normalized.to_text().len() <= 300);
}
