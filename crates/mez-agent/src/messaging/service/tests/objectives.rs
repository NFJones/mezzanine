//! Objective publication validation and discovery projection coverage.

use super::*;

/// Registering with an objective normalizes it and publishes it through the
/// same registry that discovery reads.
#[test]
fn registered_objective_is_published_normalized_to_discovery() {
    let mut service = MessageService::default();
    let identity = service
        .register_agent_with_objective(
            None,
            None,
            "agent",
            vec!["agent-harness".to_string()],
            Some("  Review   the discovery contract "),
        )
        .unwrap();
    assert_eq!(
        identity.objective.as_deref(),
        Some("Review the discovery contract")
    );
    let discovered =
        service.discover_agents_filtered_session_wide(None, None, None, None, None, &[]);
    assert_eq!(discovered.len(), 1);
    assert_eq!(
        discovered[0].objective.as_deref(),
        Some("Review the discovery contract")
    );
    assert!(
        service
            .register_agent_with_objective(None, None, "agent", Vec::new(), Some("  "))
            .is_err()
    );
}
