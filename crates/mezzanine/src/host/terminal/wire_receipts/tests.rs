//! Snapshot receipt identity bounds without presentation or execution authority.
use super::*;

/// Empty receipt lists are valid snapshots. Positive distinct producer order is
/// retained, while invalid shapes reject before any acknowledgement can be sent.
#[test]
fn wire_receipts_preserve_bounded_distinct_occurrences() {
    assert!(parse_receipts(&serde_json::json!([])).unwrap().is_empty());
    assert_eq!(
        parse_receipts(&serde_json::json!([8, 7, 9])).unwrap(),
        vec![8, 7, 9]
    );
    for invalid in [
        serde_json::json!([0]),
        serde_json::json!([7, 7]),
        serde_json::json!([1, 2, 3, 4]),
        serde_json::json!(["7"]),
        serde_json::json!([-1]),
        serde_json::json!({}),
    ] {
        assert!(parse_receipts(&invalid).is_err());
    }
}
