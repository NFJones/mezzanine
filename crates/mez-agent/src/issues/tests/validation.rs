//! Model-authored issue field validation tests.

use crate::issues::{
    IssueQueryValidation, IssueUpdateValidation, validate_issue_query, validate_issue_update,
};

/// Verifies valid model-authored issue updates and queries are accepted.
///
/// This scenario keeps MAAP field validation aligned with the canonical issue
/// domain without depending on product persistence types.
#[test]
fn issue_action_validation_accepts_valid_fields() {
    let dependencies = vec!["issue-1".to_string()];
    validate_issue_update(IssueUpdateValidation {
        kind: Some("task"),
        state: Some("open"),
        title: Some("Implement agent contract"),
        body: None,
        clear_body: false,
        notes: Some("in progress"),
        clear_notes: false,
        depends_on: Some(&dependencies),
        clear_depends_on: false,
    })
    .unwrap();
    validate_issue_query(IssueQueryValidation {
        kind: Some("defect"),
        state: Some("resolved"),
        text: Some("agent"),
        limit: Some(200),
    })
    .unwrap();
}

/// Verifies model-authored issue validation rejects malformed fields.
///
/// Conflicting updates, duplicate dependencies, invalid grammar, NUL text,
/// and out-of-range queries must fail before product execution.
#[test]
fn issue_action_validation_rejects_invalid_fields() {
    let duplicate_dependencies = vec!["issue-1".to_string(), "issue-1".to_string()];
    let error = validate_issue_update(IssueUpdateValidation {
        kind: None,
        state: None,
        title: None,
        body: Some("replacement"),
        clear_body: true,
        notes: None,
        clear_notes: false,
        depends_on: Some(&duplicate_dependencies),
        clear_depends_on: false,
    })
    .unwrap_err();
    assert!(error.to_string().contains("set and clear body"), "{error}");

    for query in [
        IssueQueryValidation {
            kind: Some("bug"),
            state: None,
            text: None,
            limit: None,
        },
        IssueQueryValidation {
            kind: None,
            state: Some("closed"),
            text: None,
            limit: None,
        },
        IssueQueryValidation {
            kind: None,
            state: None,
            text: Some("bad\0query"),
            limit: None,
        },
        IssueQueryValidation {
            kind: None,
            state: None,
            text: None,
            limit: Some(201),
        },
    ] {
        assert!(validate_issue_query(query).is_err());
    }
}
