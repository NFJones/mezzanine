//! Contract regressions: identities/bounds/filter semantics are pure and cannot
//! activate providers or touch configuration, credentials or the network.

use super::*;

/// Creates a deterministic request whose ordered filter/presence facts matter.
fn request() -> SearchRequest {
    SearchRequest { query: "private query".into(), domains: vec!["b.test".into(), "a.test".into()], recency_days: None, max_results: None }
}

#[test]
/// Pins legacy limits independently of strict future presets, including zero,
/// backend-lower caps and rejection of enlarged or impossible budget requests.
fn limits_preserve_legacy_and_bound_future_modes() {
    assert_eq!(SearchLimits::legacy_ddg(None).results(), 10);
    assert_eq!(SearchLimits::legacy_ddg(Some(0)).results(), 0);
    assert_eq!(SearchLimits::legacy_ddg(Some(99)).results(), 50);
    let limits = SearchLimits::strict(None, 7, None).unwrap();
    assert_eq!(limits.results(), 7);
    assert_eq!(limits.normalized_bytes(), 32 * 1024);
    assert_eq!(limits.transport_bytes(), 1024 * 1024);
    assert_eq!(limits.deadline_ms(), 30_000);
    assert!(SearchLimits::strict(Some(0), 7, None).is_err());
    assert!(SearchLimits::strict(None, 0, None).is_err());
    assert!(SearchLimits::strict(None, 7, Some(0)).is_err());
    assert!(SearchLimits::strict(None, 7, Some(256 * 1024 + 1)).is_err());
}

#[test]
/// Hashes preserve order/presence and bind all effect-relevant plan fields;
/// implicit DDG has no invented config epoch or credential-slot identity.
fn digests_bind_effects_without_diagnostic_corpus() {
    let original = request();
    let mut changed = original.clone();
    changed.domains.reverse();
    assert_ne!(original.digest().unwrap(), changed.digest().unwrap());
    changed = original.clone(); changed.max_results = Some(10);
    assert_ne!(original.digest().unwrap(), changed.digest().unwrap());
    changed = original.clone(); changed.recency_days = Some(0);
    assert_ne!(original.digest().unwrap(), changed.digest().unwrap());
    let plan = SearchExecutionPlan::legacy_ddg(&original).unwrap();
    assert_eq!(plan.binding, SearchPlanBinding::default());
    let mut modified = plan.clone(); modified.binding.config_revision = Some(SearchIdentity::new("rev1").unwrap());
    assert_ne!(plan.digest().unwrap(), modified.digest().unwrap());
    modified = plan.clone(); modified.binding.credential_binding = Some(SearchIdentity::new("slot1").unwrap());
    assert_ne!(plan.digest().unwrap(), modified.digest().unwrap());
    modified = plan.clone(); modified.limits = SearchLimits::legacy_ddg(Some(1));
    assert_ne!(plan.digest().unwrap(), modified.digest().unwrap());
    assert!(!format!("{original:?}{plan:?}").contains("private query"));
    assert!(SearchIdentity::new("https://secret.test/key").is_err());
    assert!(SearchIdentity::new(&"x".repeat(129)).is_err());
    assert_eq!(original.legacy_ddg_url(), "https://duckduckgo.com/html/?q=private%20query%20site%3Ab.test%20site%3Aa.test");
}

#[test]
/// Strict hostname unions include subdomains without suffix collisions and
/// never reinterpret the legacy arbitrary site-term syntax as this contract.
fn hostname_union_validates_and_matches_precisely() {
    let filter = HostnameUnion::new(&["EXAMPLE.com".into(), "other.test".into(), "example.com".into()]).unwrap();
    assert!(filter.matches("sub.example.com"));
    assert!(filter.matches("OTHER.TEST"));
    assert!(!filter.matches("notexample.com"));
    for bad in ["https://example.com", "*.test", "a.test/path", "a.test:80", "user@a.test", "127.0.0.1", "a..test", "-a.test", "a.test.", "é.test"] {
        assert!(HostnameUnion::new(&[bad.into()]).is_err(), "{bad}");
    }
    assert!(HostnameUnion::new(&vec!["a.test".into(); 51]).is_err());
}

#[test]
/// Checked UTC dates reject malformed and impossible dates, including century
/// leap-year exceptions. Deserialization cannot bypass the constructor.
fn checked_utc_dates_reject_calendar_errors() {
    for valid in ["2000-02-29", "2024-02-29", "2025-12-31"] { assert!(UtcDate::parse(valid).is_ok()); }
    for bad in ["1900-02-29", "2025-02-29", "0000-01-01", "2025-13-01", "2025-04-31", "2025-01-00", "2025-1-01", "2025-01-01Z", "é025-01-01"] {
        assert!(UtcDate::parse(bad).is_err(), "{bad}");
        assert!(serde_json::from_value::<UtcDate>(serde_json::json!(bad)).is_err());
    }
}

#[test]
/// Missing optional facts remain absent; no snippet/date/request-ID/usage/score
/// or charges can be manufactured by the default DDG constructor.
fn legacy_result_does_not_fabricate_optional_facts() {
    let result = SearchResult::legacy_ddg(&request(), vec![SearchHit { title: "Title".into(), url: "https://a.test".into(), excerpt: None, source_date: None }], false);
    let json = serde_json::to_string(&result).unwrap();
    for field in ["excerpt", "source_date", "request_id", "usage", "score", "charge"] { assert!(!json.contains(field)); }
    assert_eq!(result.render_text(), "1. Title\n   https://a.test");
    assert_eq!(result.domains, FilterDisposition::Query);
    assert_eq!(result.dates, FilterDisposition::NotRequested);
}
