//! Actual agent composer Tab/render discovery must not execute selection.

use super::*;

#[test]
fn runtime_compaction_mode_composer_completion_and_shadow_are_non_mutating() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary(
            "compaction-discovery",
            true,
            Size::new(120, 40).unwrap(),
            120,
        )
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    let config = service.integration.config_layers().to_vec();
    let mode = service.effective_agent_compaction_mode("%1");
    let override_before = service.agent_compaction_mode_override("%1");
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"/compaction-mode ")
        .unwrap();
    let frame = service.terminal_frame_context();
    let prompt = frame.panes["%1"].agent_prompt.as_ref().unwrap();
    let draft = prompt.buffer.draft_snapshot();
    assert!(
        prompt
            .render_with_shadow_hint()
            .contains("[status|summary|raw_tail|reset]")
    );
    assert_eq!(prompt.buffer.draft_snapshot(), draft);
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"\t")
        .unwrap();
    let first = service.agent_prompt_inputs_for_tests()["%1"]
        .prompt
        .buffer
        .line()
        .to_string();
    assert!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .selector
            .is_some()
    );
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"\t")
        .unwrap();
    assert_ne!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        first
    );
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"\x1b[Z")
        .unwrap();
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        first
    );
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"\x03")
        .unwrap();
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"/compaction-mode summary ")
        .unwrap();
    let frame = service.terminal_frame_context();
    assert!(
        frame.panes["%1"]
            .agent_prompt
            .as_ref()
            .unwrap()
            .render_with_shadow_hint()
            .contains("[--global]")
    );
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"--g\t")
        .unwrap();
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        "/compaction-mode summary --global "
    );
    assert_eq!(service.integration.config_layers(), config);
    assert_eq!(service.effective_agent_compaction_mode("%1"), mode);
    assert_eq!(
        service.agent_compaction_mode_override("%1"),
        override_before
    );
    assert!(!service.agent_is_compacting("%1"));
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(service.agent_turn_ledger().turns().is_empty());
}
