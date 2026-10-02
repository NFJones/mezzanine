//! Native basic-action admission for executable hooks and MCP preparation.
//!
//! These fixtures drive production preparation/admission without optional
//! tracing utilities. Queue and registry evidence is separate from filesystem
//! dispatch qualification, which remains a dependent issue.

use super::*;

/// Native preparation retains configured stdio metadata without starting it;
/// explicit discovery still returns its independently admitted startup plan.
#[test]
fn native_provider_preparation_does_not_start_stdio_mcp() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    service
        .mcp_registry_mut()
        .add_server(mez_agent::mcp::McpServerConfig::stdio(
            "fixture",
            "Fixture",
            "/missing/absolute/mez-helper",
            Vec::new(),
        ))
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect native preparation")
        .unwrap();
    let work = service
        .prepare_agent_provider_work(&started.turn_id)
        .unwrap();
    assert!(!work.allow_stdio);
    assert!(work.mcp_plans.is_empty());
    assert_eq!(work.attempted_mcp_servers, 0);
    assert_eq!(
        service.mcp_registry().list_servers()[0].status,
        mez_agent::mcp::McpServerStatus::Configured
    );
    let explicit = service.prepare_runtime_mcp_discovery_work().unwrap();
    assert!(explicit.allow_stdio);
    assert_eq!(explicit.mcp_plans.len(), 1);
}

/// Native preparation may retain direct HTTP discovery work without admitting
/// executable stdio startup; no synthetic callable schema is invented.
#[test]
fn native_provider_preparation_preserves_http_discovery() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    service
        .mcp_registry_mut()
        .add_server(mez_agent::mcp::McpServerConfig::streamable_http(
            "http",
            "HTTP",
            "https://example.invalid/mcp",
        ))
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect HTTP preparation")
        .unwrap();
    let work = service
        .prepare_agent_provider_work(&started.turn_id)
        .unwrap();
    assert!(!work.allow_stdio);
    assert_eq!(work.mcp_plans.len(), 1);
    assert!(matches!(
        work.mcp_plans[0].transport,
        mez_agent::mcp::McpStartupTransportPlan::StreamableHttp { .. }
    ));
}

/// The external worker independently rejects an incompatible stdio plan before
/// attempting even an absolute missing executable, rather than relying only
/// on the actor-side server filter to preserve the no-implicit-process rule.
#[tokio::test]
async fn native_preparation_worker_rejects_stdio_before_spawn() {
    let mut service = test_runtime_service();
    service
        .mcp_registry_mut()
        .add_server(mez_agent::mcp::McpServerConfig::stdio(
            "fixture",
            "Fixture",
            "/missing/absolute/mez-helper",
            Vec::new(),
        ))
        .unwrap();
    let mut work = service.prepare_runtime_mcp_discovery_work().unwrap();
    work.allow_stdio = false;
    let result = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    assert_eq!(result.mcp.len(), 1);
    assert!(
        result.mcp[0]
            .result
            .as_ref()
            .err()
            .unwrap()
            .message()
            .contains("cannot start stdio")
    );
}

/// Required executable guards block native semantic admission even when their
/// failure policy says warn. Optional handlers are diagnosed without execution,
/// including completion handlers that must not erase already committed effects.
#[test]
fn native_hook_admission_blocks_required_and_never_queues_optional_handlers() {
    let mut service = test_runtime_service();
    service.use_hook_effect_adapter();
    service.replace_config_layers(vec![ConfigLayer {
        name: "primary".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[hooks.required]\nevent = \"pre_shell_command\"\nprogram = \"/missing/absolute/guard\"\nrequired = true\non_failure = \"warn\"\n[hooks.optional]\nevent = \"permission_request\"\ncommand = \"printf optional\"\non_failure = \"warn\"\n[hooks.post]\nevent = \"post_shell_command\"\nprogram = \"/missing/absolute/post\"\n".into(),
    }]).unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    let payload = r#"{"pane_id":"%1","action_type":"apply_patch"}"#;
    assert!(
        service
            .run_configured_pre_action_hooks(HookEvent::PreShellCommand, payload)
            .unwrap()
            .is_some()
    );
    assert!(
        service
            .run_configured_pre_action_hooks(HookEvent::PermissionRequest, payload)
            .unwrap()
            .is_none()
    );
    service
        .run_configured_completed_hooks(HookEvent::PostShellCommand, payload)
        .unwrap();
    assert!(
        service
            .drain_program_hook_transition()
            .side_effects
            .is_empty()
    );
    assert_eq!(service.focused_shell_hook_queue_len(), 0);
}

/// Legacy shell-shaped patch payloads still identify semantic ownership before
/// dispatch; prompt/start/stop handlers cannot become hidden process prerequisites.
#[test]
fn native_semantic_and_lifecycle_hooks_do_not_launch_or_queue() {
    let mut service = test_runtime_service();
    service.use_hook_effect_adapter();
    service.replace_config_layers(vec![ConfigLayer {
        name: "primary".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[hooks.patch]\nevent = \"pre_shell_command\"\nprogram = \"/missing/guard\"\non_failure = \"block\"\n[hooks.prompt]\nevent = \"user_prompt_submit\"\nprogram = \"/missing/prompt\"\non_failure = \"warn\"\n[hooks.start]\nevent = \"agent_turn_start\"\ncommand = \"printf start\"\non_failure = \"warn\"\n[hooks.stop]\nevent = \"agent_turn_stop\"\nprogram = \"/missing/stop\"\n".into(),
    }]).unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "inspect semantic hooks")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    let action = mez_agent::AgentAction {
        id: "patch".into(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Add File: file\n+x\n*** End Patch".into(),
            strip: None,
        },
    };
    let payload = crate::runtime::runtime_pre_shell_hook_payload(
        &turn,
        &action,
        "generated inert patch phase",
    );
    assert!(
        service
            .run_configured_pre_action_hooks(HookEvent::PreShellCommand, &payload)
            .unwrap()
            .is_some()
    );
    let lifecycle = serde_json::json!({"turn_id": turn.turn_id}).to_string();
    assert!(
        service
            .run_configured_pre_action_hooks(HookEvent::AgentTurnStart, &lifecycle)
            .unwrap()
            .is_none()
    );
    service
        .run_configured_completed_hooks(HookEvent::AgentTurnStop, &lifecycle)
        .unwrap();
    assert!(
        service
            .drain_program_hook_transition()
            .side_effects
            .is_empty()
    );
    assert_eq!(service.focused_shell_hook_queue_len(), 0);
}
