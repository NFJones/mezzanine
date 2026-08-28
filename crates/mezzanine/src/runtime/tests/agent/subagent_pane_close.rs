//! Runtime tests for deferred terminal subagent pane closure.

use super::*;

/// Verifies automatic subagent pane closure resizes and repaints the surviving
/// adapter-owned pane at the geometry produced by layout reflow.
///
/// Adapter-owned workers do not observe in-memory session geometry directly.
/// The close path must preserve the mux resize effects and emit one generation-
/// scoped resize request for the sibling that expands into the removed pane.
/// It must also invalidate clients by affected window rather than by the
/// removed pane id so the expanded sibling is rendered immediately.
#[test]
fn runtime_automatic_subagent_close_resizes_surviving_adapter_owned_pane() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let closing = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap();
    let mut handed_off = service
        .take_running_pane_process_instances_for_adapter(2)
        .unwrap();
    let surviving_instance = handed_off
        .iter()
        .find_map(|(instance, _)| (instance.pane_id == "%1").then_some(instance.clone()))
        .unwrap();
    let original_surviving_size = service
        .find_pane_descriptor("%1")
        .map(|descriptor| descriptor.size)
        .unwrap();
    assert!(service.drain_pane_io_transition().side_effects.is_empty());

    let turn = mez_agent::AgentTurnRecord {
        turn_id: "automatic-subagent-close".to_string(),
        conversation_id: "conversation-subagent-close".to_string(),
        agent_id: format!("agent-{}", closing.pane_id),
        pane_id: closing.pane_id.clone(),
        trigger: mez_agent::AgentTurnTrigger::SubagentEvent,
        started_at_unix_seconds: 200,
        deadline_at_unix_millis: 0,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: Some("parent-turn".to_string()),
        cooperation_mode: None,
        state: mez_agent::AgentTurnState::Completed,
        initial_capability: None,
    };
    service.mark_terminal_subagent_pane_close_for_tests(&closing.pane_id);

    service
        .close_terminal_subagent_pane_if_pending(&turn)
        .unwrap();

    let surviving_size = service
        .find_pane_descriptor("%1")
        .map(|descriptor| descriptor.size)
        .unwrap();
    assert!(surviving_size.columns > original_surviving_size.columns);
    assert_eq!(surviving_size.rows, original_surviving_size.rows);
    let effects = service.drain_deferred_effects_transition().side_effects;
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            RuntimeSideEffect::PaneProcessIo {
                instance,
                effect: crate::runtime::PaneProcessIoEffect::Resize { size },
            } if instance == &surviving_instance && size == &surviving_size
        )),
        "{effects:#?}"
    );
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            RuntimeSideEffect::RenderClient {
                client_id,
                reason: crate::runtime::RenderInvalidationReason::Layout,
            } if client_id == &primary
        )),
        "{effects:#?}"
    );

    for (_, process) in &mut handed_off {
        process
            .terminate(std::time::Duration::from_millis(100))
            .unwrap();
    }
}
