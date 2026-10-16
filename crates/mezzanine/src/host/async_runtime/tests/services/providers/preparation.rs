//! Provider preparation admission and turn-local error containment regressions.
//!
//! Captured dispatches deliberately arrive after lifecycle changes. Only local
//! actor/service work runs; synthetic credentials permit a healthy claim but
//! no test executes its provider request.

use super::*;
use crate::security::auth::{AuthPaths, AuthStore};

/// Builds one isolated provider-ready session and returns its credential root
/// for cleanup. No ambient credentials or live provider endpoint are used.
fn preparation_service(label: &str) -> (RuntimeSessionService, ClientId, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "mez-provider-preparation-{label}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth = AuthStore::new(AuthPaths::under_config_root(&root));
    let store = auth.file_credential_store("openai").unwrap();
    auth.login_with_api_key("openai", "openai", "synthetic-key", &store)
        .unwrap();
    let mut service = test_service();
    service.set_auth_store(auth);
    service.replace_config_layers(vec![ConfigLayer {
        name: "provider-preparation".into(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\nshell_mode = \"pane\"\n[permissions]\nsandbox = \"policy-only\"\n[providers.openai]\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:9\"\nmodels = [\"fixture\"]\ndefault_model = \"fixture\"\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"fixture\"\n".into(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    (service, primary, root)
}

/// Delayed dispatch after `/stop`, an absent turn, and a no-longer-pending live
/// turn must be no-ops. The same actor must still claim a new healthy turn,
/// proving that obsolete work neither fails the provider service nor poisons
/// subsequent work. Removing the live non-pending turn's profile also proves
/// that the no-work decision precedes the profile lookup.
#[tokio::test(flavor = "current_thread")]
async fn async_provider_preparation_skips_stale_dispatches_and_claims_next_turn() {
    let (mut service, primary, root) = preparation_service("stale");
    service
        .execute_agent_shell_command(&primary, "stopped task")
        .unwrap();
    let stopped = service.pending_agent_provider_tasks()[0].clone();
    assert!(service.agent_turn_model_profile(&stopped.turn_id).is_some());
    service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(service.agent_turn_model_profile(&stopped.turn_id).is_none());
    assert!(service.agent_turn_ledger().turn(&stopped.turn_id).is_some());
    service
        .execute_agent_shell_command(&primary, "non-pending task")
        .unwrap();
    let non_pending = service.pending_agent_provider_tasks()[0].clone();
    service.remove_pending_agent_provider_task(&non_pending.turn_id);
    service.remove_agent_turn_model_profile(&non_pending.turn_id);
    let non_pending_turn = non_pending.turn_id.clone();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        let mut dispatches = vec![stopped, non_pending];
        let mut missing = dispatches[0].clone();
        missing.turn_id = "turn-missing-preparation".into();
        dispatches.push(missing);
        for task in dispatches {
            handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::DispatchAgentProvider {
                    agent_id: AgentId::opaque(task.agent_id).unwrap(),
                    turn_id: task.turn_id,
                }])
                .await
                .unwrap();
            let report = run_async_agent_provider_service(
                &handle,
                AsyncAgentProviderServiceConfig::new(1).unwrap(),
                |polls, _| polls >= 1,
            )
            .await
            .expect("stale preparation must not fail the provider service");
            assert_eq!(report.executions, 0);
        }
        assert!(
            handle
                .agent_turn_is_running(&non_pending_turn)
                .await
                .unwrap()
        );
        handle
            .execute_agent_shell_command(primary.clone(), "/stop".into())
            .await
            .unwrap();
        handle
            .execute_agent_shell_command(primary.clone(), "healthy task".into())
            .await
            .unwrap();
        let next = handle
            .pending_agent_provider_tasks()
            .await
            .unwrap()
            .remove(0);
        assert!(
            handle
                .claim_configured_agent_provider_task(
                    AgentId::opaque(next.agent_id).unwrap(),
                    next.turn_id,
                )
                .await
                .unwrap()
                .is_some()
        );
        handle
            .execute_agent_shell_command(primary, "/stop".into())
            .await
            .unwrap();
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// A genuinely pending/running turn with missing profile state is a fault,
/// not stale work. Preparation must visibly fail and clean up that turn while
/// leaving the actor and provider service available for a new healthy claim.
#[tokio::test(flavor = "current_thread")]
async fn async_provider_preparation_missing_live_profile_fails_only_its_turn() {
    exercise_live_preparation_failure(true).await;
}

/// A retained profile whose provider is unavailable must take the normal
/// provider-failure path, not the missing-profile fallback or daemon failure.
/// A subsequent valid turn can still claim without replaying the failed one.
#[tokio::test(flavor = "current_thread")]
async fn async_provider_preparation_missing_provider_fails_only_its_turn() {
    exercise_live_preparation_failure(false).await;
}

/// Drives early snapshot faults through the provider loop and verifies both
/// visible diagnosis and successful fresh work in the same live actor.
async fn exercise_live_preparation_failure(missing_profile: bool) {
    let label = if missing_profile {
        "missing-profile"
    } else {
        "missing-provider"
    };
    let (mut service, primary, root) = preparation_service(label);
    service
        .execute_agent_shell_command(&primary, "broken task")
        .unwrap();
    let task = service.pending_agent_provider_tasks()[0].clone();
    let turn_id = task.turn_id.clone();
    if missing_profile {
        service.remove_agent_turn_model_profile(&turn_id);
    } else {
        let mut profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
        profile.provider = "unconfigured-provider".into();
        service.set_agent_turn_model_profile(&turn_id, profile);
    }
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: AgentId::opaque(task.agent_id).unwrap(),
                turn_id: task.turn_id,
            }])
            .await
            .unwrap();
        let report = run_async_agent_provider_service(
            &handle,
            AsyncAgentProviderServiceConfig::new(1).unwrap(),
            |polls, _| polls >= 1,
        )
        .await
        .expect("live preparation faults must be contained to their turn");
        assert_eq!(report.executions, 0);
        assert!(!handle.agent_turn_is_running(&turn_id).await.unwrap());
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty()
        );
        let view = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        let text = view.lines.join("");
        let diagnostic = if missing_profile {
            "preparation model profile is unavailable"
        } else {
            "is not configured"
        };
        assert!(text.contains(diagnostic), "{text}");
        handle
            .execute_agent_shell_command(primary.clone(), "healthy task".into())
            .await
            .unwrap();
        let next = handle
            .pending_agent_provider_tasks()
            .await
            .unwrap()
            .remove(0);
        assert!(
            handle
                .claim_configured_agent_provider_task(
                    AgentId::opaque(next.agent_id).unwrap(),
                    next.turn_id,
                )
                .await
                .unwrap()
                .is_some()
        );
        handle
            .execute_agent_shell_command(primary, "/stop".into())
            .await
            .unwrap();
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn(&turn_id)
            .unwrap()
            .state,
        mez_agent::AgentTurnState::Failed
    );
    exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Human pause must inhibit preparation before any MCP registry mutation, even
/// when a previously captured provider dispatch is presented directly. The
/// shared post-I/O claim gate must preserve the same pause without retiring it.
#[test]
fn async_provider_preparation_human_pause_prevents_external_work() {
    let (mut service, primary, root) = preparation_service("paused");
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "paused task")
        .unwrap();
    let task = service.pending_agent_provider_tasks()[0].clone();
    service
        .mcp_registry_mut()
        .add_server(mez_agent::mcp::McpServerConfig::stdio(
            "unstarted",
            "Unstarted",
            "/missing/mez-preparation-helper",
            Vec::new(),
        ))
        .unwrap();
    let target = service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    service
        .pause_agent_lifecycle_target(&primary, &target)
        .unwrap();
    let agent = AgentId::opaque(task.agent_id).unwrap();
    assert!(
        service
            .prepare_pending_agent_provider_work(&agent, &task.turn_id)
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .claim_configured_agent_provider_task(&agent, &task.turn_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        service.mcp_registry().list_servers()[0].status,
        mez_agent::mcp::McpServerStatus::Configured
    );
    assert!(service.agent_turn_is_human_paused(&task.turn_id));
    service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
