//! Focused macro orchestration tests.

/// Verifies that `deregister_macro_managed_subagent` removes an agent
/// from the macro-managed set, preventing stale entries from accumulating
/// and preventing recycled pane ids from hijacking macro bridge routing.
#[test]
fn deregister_macro_managed_removes_agent_from_set() {
    let fixture = crate::test_support::runtime::RuntimeServiceFixture::new();
    let mut service = fixture.build();
    let agent_id = "agent-%99";

    // Initially empty
    assert!(!service.macro_managed_subagent_agents.contains_key(agent_id));

    // Register
    service.register_macro_managed_subagent(agent_id, "turn-99", "agent-%1", "test-macro");
    assert!(service.macro_managed_subagent_agents.contains_key(agent_id));

    // Deregister
    service.deregister_macro_managed_subagent(agent_id);
    assert!(!service.macro_managed_subagent_agents.contains_key(agent_id));

    // Deregistering an already-absent id is a no-op
    service.deregister_macro_managed_subagent(agent_id);
    assert!(!service.macro_managed_subagent_agents.contains_key(agent_id));
}
