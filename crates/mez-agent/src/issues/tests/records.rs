//! Canonical issue record tests.

use crate::issues::{IssueKind, IssueRecord};

/// Verifies invalid issue kinds and required text fields are rejected.
///
/// This regression protects the canonical record boundary before any product
/// persistence adapter is allowed to write malformed values.
#[test]
fn issue_validation_rejects_invalid_kind_and_empty_title() {
    assert!(IssueKind::parse("bug").is_err());
    assert!(
        IssueRecord::new(
            "id".to_string(),
            "/repo".to_string(),
            IssueKind::Task,
            "".to_string(),
            None,
            None,
            10,
        )
        .is_err()
    );
}
