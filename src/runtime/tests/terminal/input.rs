//! Runtime tests for terminal input behavior.

use super::*;

/// Verifies command-start metadata revokes stale readiness while a turn waits.
///
/// A user command may start after the agent shell is opened while the active
/// turn is still waiting for provider output or shell dispatch. The runtime
/// should suppress queueing/repaint side effects for that command-start event,
/// but it must still record the pane as busy and revoke any manual readiness
/// override so the next agent shell action cannot write into a non-idle pane.
#[test]
fn runtime_osc_command_start_while_turn_waiting_marks_pane_busy() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect the pane")
        .unwrap();
    assert_eq!(started.state, AgentTurnState::Running);
    service.set_pane_readiness("%1", PaneReadinessState::Ready);
    service
        .pane_readiness_overrides
        .mark_ready_for_epoch("%1", 7, "test override", true)
        .unwrap();

    let observed = service
        .observe_passive_shell_busy("%1", "osc133-command-start")
        .unwrap();

    assert_eq!(observed, 1);
    assert_eq!(service.pane_readiness_state("%1"), PaneReadinessState::Busy);
    assert!(!service.pane_readiness_overrides.allows_epoch("%1", 7));
}

/// Verifies that the async terminal command path refreshes provider metadata
/// through the live-pane runtime entrypoint instead of relying on a nested
/// sync-to-async bridge inside command dispatch.
#[tokio::test(flavor = "multi_thread")]
async fn runtime_terminal_refresh_provider_info_async_command_refreshes_provider_metadata() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let output = service
        .execute_terminal_command_async(&primary, "refresh-provider-info")
        .await
        .unwrap();
    assert!(
        output.contains(r#""command":"refresh-provider-info""#),
        "{output}"
    );
    assert!(
        output.contains("providers=1 refreshed=1 failed=0"),
        "{output}"
    );
    assert!(output.contains("openai source=config"), "{output}");
    assert!(output.contains("provider_error=none"), "{output}");
    assert!(service.provider_model_catalog_cache.contains_key("openai"));
}
