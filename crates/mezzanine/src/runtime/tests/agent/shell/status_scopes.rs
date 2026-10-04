//! Scoped status snapshots, partition conservation and exact-client settlement.
//!
//! Fixtures qualify project mappings once, then exercise report reads without
//! reconstructing attribution from current cwd or issuing provider requests.

use super::*;
use crate::storage::token_usage::{AccountingOrigin, TokenUsageStore};
use mez_agent::slash::{StatusOptions, StatusScope};
use mez_agent::{ModelTokenUsage, ModelTokenUsageKey};

/// Missing or explicitly withheld project evidence never selects ancestor or
/// overall totals. A failed acceptance leaves no command claim behind.
#[test]
fn runtime_status_scopes_missing_and_nested_denial_fail_without_claim() {
    let (mut service, client, root) = fixture();
    let nested = root.join("a/nested");
    fs::create_dir_all(&nested).unwrap();
    let mut trust = service.integration.project_trust_store().unwrap().clone();
    trust
        .decide(nested.clone(), TrustDecision::Rejected, None)
        .unwrap();
    service.set_project_trust_store(trust, None);
    service.set_pane_current_working_directory("%1", nested);
    assert!(service.cached_accounting_project_for_pane("%1").is_none());
    let error = service
        .dispatch_deferred_agent_shell_command(
            &client,
            "%1",
            "status",
            "/status --project --extended",
        )
        .unwrap_err();
    assert!(error.message().contains("active-project-unavailable"));
    assert!(!service.agent_command_is_active("%1"));
    service.remove_pane_current_working_directory("%1");
    assert!(
        service
            .prepare_status_report(
                &client,
                "%1",
                StatusOptions {
                    scope: StatusScope::Project,
                    extended: false
                }
            )
            .is_err()
    );
    fs::remove_dir_all(root).unwrap();
}

/// A framed control report queries history outside actor ownership, retains
/// following frames in order, and replays the completed response without a new
/// worker lease. This path must not install an interactive overlay.
#[tokio::test(flavor = "current_thread")]
async fn runtime_status_scopes_control_history_preserves_framing_and_replay() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let (mut service, owner, root) = fixture();
    let started = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    service.set_deferred_agent_command_probe_for_tests(started.clone(), release.clone());
    let mut connection = ControlConnectionState::new(true, true);
    connection.rebind_caller_client(owner);
    let request = r#"{"jsonrpc":"2.0","id":"report","method":"agent/shell/command","params":{"idempotency_key":"report","input":"/status --all-projects --extended"}}"#;
    let mut input = crate::control::encode_control_body(
        r#"{"jsonrpc":"2.0","id":"before","method":"session/get","params":{}}"#,
    );
    input.extend_from_slice(&crate::control::encode_control_body(request));
    input.extend_from_slice(&crate::control::encode_control_body(
        r#"{"jsonrpc":"2.0","id":"after","method":"session/get","params":{}}"#,
    ));
    let expected_consumed = input.len();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let client = async {
        let query_handle = handle.clone();
        let query_connection = connection.clone();
        let query = tokio::spawn(async move {
            query_handle
                .handle_control_input_for_connection(input, 1_000_000, query_connection)
                .await
                .unwrap()
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), started.notified())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), handle.lifecycle_state())
            .await
            .unwrap()
            .unwrap();
        release.notify_one();
        let response = query.await.unwrap();
        assert_eq!(response.consumed, expected_consumed);
        let (before, prefix_len) =
            crate::control::decode_control_frame(&response.output, 1_000_000).unwrap();
        assert!(before.contains("\"id\":\"before\""), "{before}");
        let (first, consumed) =
            crate::control::decode_control_frame(&response.output[prefix_len..], 1_000_000)
                .unwrap();
        assert!(first.contains("Accounting Snapshot"), "{first}");
        let (next, _) = crate::control::decode_control_frame(
            &response.output[prefix_len + consumed..],
            1_000_000,
        )
        .unwrap();
        assert!(next.contains("\"id\":\"after\""), "{next}");
        let replay = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            handle.handle_control_input_for_connection(
                crate::control::encode_control_body(request),
                1_000_000,
                connection,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let (body, _) = crate::control::decode_control_frame(&replay.output, 1_000_000).unwrap();
        assert_eq!(body, first);
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    fs::remove_dir_all(root).unwrap();
}

/// Hold the production history worker after claim while another primary uses
/// the actor. Releasing it installs only the invoking client's static report,
/// regardless of which client most recently projected presentation state.
#[tokio::test(flavor = "current_thread")]
async fn runtime_status_scopes_worker_is_responsive_and_client_local() {
    use crate::host::async_runtime::{
        AsyncAgentProviderServiceConfig, AsyncRuntimeActorConfig, AsyncRuntimeSessionActor,
        run_async_agent_command_service,
    };
    let (mut service, owner, root) = fixture();
    let other = service
        .attach_primary(
            "other-status-client",
            true,
            Size::new(100, 30).unwrap(),
            121,
        )
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let started = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    service.set_deferred_agent_command_probe_for_tests(started.clone(), release.clone());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client = async {
        let worker_handle = handle.clone();
        let worker_stop = stop.clone();
        let worker = tokio::spawn(async move {
            run_async_agent_command_service(
                &worker_handle,
                AsyncAgentProviderServiceConfig::new(1)
                    .unwrap()
                    .with_idle_interval(std::time::Duration::from_millis(5))
                    .unwrap(),
                move |_, _| worker_stop.load(std::sync::atomic::Ordering::SeqCst),
            )
            .await
            .unwrap()
        });
        let response = handle
            .execute_agent_shell_command(owner.clone(), "/status --all-projects --extended".into())
            .await
            .unwrap();
        assert!(
            response.contains("\"command\":\"status\"") && response.contains("\"body\":null"),
            "{response}"
        );
        tokio::time::timeout(std::time::Duration::from_secs(3), started.notified())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), handle.lifecycle_state())
            .await
            .unwrap()
            .unwrap();
        let other_frame = handle
            .render_client_frame(
                other.clone(),
                ClientViewRole::Primary,
                Size::new(100, 30).unwrap(),
                TerminalClientLoopConfig::default(),
                true,
            )
            .await
            .unwrap();
        assert!(
            !other_frame
                .view
                .unwrap()
                .lines
                .join("\n")
                .contains("Agent Status")
        );
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let frame = handle
                    .render_client_frame(
                        owner.clone(),
                        ClientViewRole::Primary,
                        Size::new(100, 30).unwrap(),
                        TerminalClientLoopConfig::default(),
                        true,
                    )
                    .await
                    .unwrap();
                if frame
                    .view
                    .unwrap()
                    .lines
                    .join("\n")
                    .contains("Agent Status")
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let frame = handle
            .render_client_frame(
                other.clone(),
                ClientViewRole::Primary,
                Size::new(100, 30).unwrap(),
                TerminalClientLoopConfig::default(),
                true,
            )
            .await
            .unwrap();
        assert!(
            !frame
                .view
                .unwrap()
                .lines
                .join("\n")
                .contains("Agent Status")
        );
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(worker.await.unwrap(), 1);
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Creates qualified inventory with two populated and one zero-use project.
fn fixture() -> (RuntimeSessionService, mez_core::ids::ClientId, PathBuf) {
    let root = temp_root("status-scopes");
    let mut service = test_runtime_service();
    let client = service
        .attach_primary("status-owner", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let mut trust = ProjectTrustStore::default();
    for name in ["a", "b", "zero"] {
        fs::create_dir_all(root.join(name)).unwrap();
        trust
            .decide(root.join(name), TrustDecision::Trusted, None)
            .unwrap();
    }
    let store = TokenUsageStore::new(root.join("usage.sqlite"));
    let inventory = store
        .prepare_accounting_projects(&trust.records().cloned().collect::<Vec<_>>())
        .unwrap();
    service.set_project_trust_store(trust, None);
    service.set_token_usage_store(store.clone());
    service
        .persistence
        .install_accounting_projects(&store, Some(inventory.clone()));
    service.set_pane_current_working_directory("%1", root.join("a"));
    for (name, input) in [("a", 7), ("b", 11)] {
        let id = inventory
            .iter()
            .find(|row| row.root == root.join(name))
            .unwrap()
            .id
            .clone()
            .unwrap();
        service.record_native_usage_observation(
            &conversation,
            Some("%1"),
            &AccountingOrigin::Project(id),
            &ModelTokenUsageKey::new("provider", "shared-model"),
            ModelTokenUsage {
                input_tokens: input,
                output_tokens: 2,
                ..Default::default()
            },
            format!("scope-{name}"),
        );
    }
    service.record_native_usage_observation(
        &conversation,
        Some("%1"),
        &AccountingOrigin::Unattributed,
        &ModelTokenUsageKey::new("provider", "legacy-model"),
        ModelTokenUsage {
            input_tokens: 13,
            ..Default::default()
        },
        "scope-legacy".into(),
    );
    (service, client, root)
}

/// Scope flags select frozen partitions, not overall counters. Zero-use registry
/// headings and unattributed remainder remain visible, and ordinary status keeps
/// its overall view with no history reads.
#[test]
fn runtime_status_scopes_report_partitions_zero_use_and_remainder() {
    let (service, client, root) = fixture();
    let project = service
        .prepare_status_report(
            &client,
            "%1",
            StatusOptions {
                scope: StatusScope::Project,
                extended: false,
            },
        )
        .unwrap()
        .render()
        .unwrap();
    assert!(project.contains("Accounting Snapshot (STATIC"));
    assert!(project.contains("shared-model | 7 |"), "{project}");
    assert!(!project.contains("shared-model | 18 |"), "{project}");
    assert!(!project.contains("legacy-model"), "{project}");
    let all = service
        .prepare_status_report(
            &client,
            "%1",
            StatusOptions {
                scope: StatusScope::AllProjects,
                extended: true,
            },
        )
        .unwrap()
        .render()
        .unwrap();
    assert!(all.contains("/zero"), "{all}");
    assert!(all.contains("Unattributed Remainder"), "{all}");
    assert!(all.contains("shared-model | 11 |"), "{all}");
    assert!(all.contains("legacy-model | 13 |"), "{all}");
    assert!(all.contains("1-Day Token Usage"), "{all}");
    let overall = service.runtime_agent_status_display("%1").unwrap();
    assert!(overall.contains("shared-model | 18 |"), "{overall}");
    assert!(!overall.contains("1-Day Token Usage"));
    fs::remove_dir_all(root).unwrap();
}

/// Extended admission freezes inputs before worker claim and rejects a changed
/// project or detached owner rather than publishing another client's report.
#[test]
fn runtime_status_scopes_deferred_snapshot_rejects_changed_owner() {
    let (mut service, client, root) = fixture();
    service
        .dispatch_deferred_agent_shell_command(
            &client,
            "%1",
            "status",
            "/status --extended --project",
        )
        .unwrap();
    let dispatch = service
        .take_pending_deferred_agent_commands()
        .pop()
        .unwrap();
    let work = service
        .claim_agent_command_work(
            &client,
            "%1",
            "status",
            &dispatch.input,
            dispatch.claim_generation,
            &dispatch.conversation_id,
        )
        .unwrap()
        .unwrap();
    let outcome = RuntimeSessionService::execute_deferred_agent_command(&work);
    service.set_pane_current_working_directory("%1", root.join("b"));
    assert!(!service.complete_agent_command_work(&work, outcome).unwrap());
    service.set_pane_current_working_directory("%1", root.join("a"));
    service
        .dispatch_deferred_agent_shell_command(
            &client,
            "%1",
            "status",
            "/status --project --extended",
        )
        .unwrap();
    let dispatch = service
        .take_pending_deferred_agent_commands()
        .pop()
        .unwrap();
    let work = service
        .claim_agent_command_work(
            &client,
            "%1",
            "status",
            &dispatch.input,
            dispatch.claim_generation,
            &dispatch.conversation_id,
        )
        .unwrap()
        .unwrap();
    let outcome = RuntimeSessionService::execute_deferred_agent_command(&work);
    service.session.detach_primary(&client).unwrap();
    assert!(!service.complete_agent_command_work(&work, outcome).unwrap());
    fs::remove_dir_all(root).unwrap();
}
