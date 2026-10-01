//! Objective refresh throttling, explicit clearing, and atomic refusal coverage.

use super::*;

/// An unchanged objective publishes nothing and does not churn presence, so
/// discovery rows and resume views stay identical.
#[test]
fn unchanged_objective_update_is_throttled() {
    let mut service = MessageService::default();
    let identity = service
        .register_agent_with_objective(None, None, "agent", Vec::new(), Some("Inspect the backlog"))
        .unwrap();
    assert_eq!(service.presence()[0].updated_at_ms, 0);
    assert!(
        !service
            .update_agent_objective(&identity.agent_id, Some("Inspect the backlog"), 500)
            .unwrap()
    );
    assert_eq!(service.presence()[0].updated_at_ms, 0);
    assert!(
        service
            .update_agent_objective(&identity.agent_id, Some("Review the backlog"), 900)
            .unwrap()
    );
    assert_eq!(service.presence()[0].updated_at_ms, 900);
    assert_eq!(
        service
            .registered_identity(&identity.agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Review the backlog")
    );
    assert_eq!(
        service.presence()[0].identity.objective.as_deref(),
        Some("Review the backlog")
    );
}

/// Verifies explicit clearing removes discovery state while a protocol-like
/// absent refresh remains a no-op and cannot accidentally clear it.
#[test]
fn explicit_objective_clear_is_distinct_from_an_absent_refresh() {
    let mut service = MessageService::default();
    let identity = service
        .register_agent_with_objective(None, None, "agent", Vec::new(), Some("Inspect the backlog"))
        .unwrap();
    assert!(
        !service
            .update_agent_objective(&identity.agent_id, None, 10)
            .unwrap()
    );
    assert!(
        service
            .clear_agent_objective(&identity.agent_id, 20)
            .unwrap()
    );
    assert!(
        service
            .registered_identity(&identity.agent_id)
            .and_then(|identity| identity.objective.as_deref())
            .is_none()
    );
    assert!(
        !service
            .clear_agent_objective(&identity.agent_id, 30)
            .unwrap()
    );
}

/// A failed objective refresh keeps the previous published objective and
/// leaves the presence timestamp untouched.
#[test]
fn failed_objective_update_keeps_previous_objective() {
    let mut service = MessageService::default();
    let identity = service
        .register_agent_with_objective(None, None, "agent", Vec::new(), Some("Inspect the backlog"))
        .unwrap();
    assert_eq!(
        service
            .update_agent_objective(&identity.agent_id, Some("inspect\u{7}the pane"), 700)
            .unwrap_err()
            .message(),
        "MMP objective must not contain control characters"
    );
    assert!(
        service
            .update_agent_objective(&identity.agent_id, Some(String::new().as_str()), 700)
            .is_err()
    );
    assert_eq!(
        service
            .registered_identity(&identity.agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Inspect the backlog")
    );
    assert_eq!(service.presence()[0].updated_at_ms, 0);
}

/// An absent objective refresh is a no-op that keeps the previous published
/// objective and leaves the presence timestamp untouched.
#[test]
fn absent_objective_refresh_keeps_previous_objective_and_timestamp() {
    let mut service = MessageService::default();
    let identity = service
        .register_agent_with_objective(None, None, "agent", Vec::new(), Some("Inspect the backlog"))
        .unwrap();
    let published_at_ms = service.presence()[0].updated_at_ms;
    assert!(
        !service
            .update_agent_objective(&identity.agent_id, None, published_at_ms + 900)
            .unwrap()
    );
    assert_eq!(
        service
            .registered_identity(&identity.agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Inspect the backlog")
    );
    assert_eq!(
        service.presence()[0].identity.objective.as_deref(),
        Some("Inspect the backlog")
    );
    assert_eq!(service.presence()[0].updated_at_ms, published_at_ms);
}
