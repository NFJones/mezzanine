//! Terminal status parity and exact-owned report regressions.
//!
//! These tests use retained native conversations, deterministic accounting UTC,
//! direct worker claims and explicit replacement gates rather than provider calls
//! or wall-clock comparisons across a running-turn second boundary.

use super::*;
use crate::runtime::{RuntimeAgentCommandAsyncWork, RuntimeAgentCommandPrepared};

/// Creates an idle retained conversation so formatter parity is time invariant.
pub(super) fn fixture() -> (RuntimeSessionService, mez_core::ids::ClientId) {
    let mut service = test_runtime_service();
    let client = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    (service, client)
}

/// Claims exactly the acceptance-owned terminal report, without recapturing cwd,
/// focus, counters or its options from the worker's eventual start time.
fn claim(service: &mut RuntimeSessionService) -> RuntimeAgentCommandAsyncWork {
    let dispatch = service.next_pending_deferred_agent_command().unwrap();
    service.retire_pending_deferred_agent_command();
    service
        .claim_agent_command_work(
            &dispatch.primary_client_id,
            &dispatch.pane_id,
            &dispatch.command,
            &dispatch.input,
            dispatch.claim_generation,
            &dispatch.conversation_id,
        )
        .unwrap()
        .unwrap()
}

/// The terminal uses exactly the slash formatter and preserves hidden native
/// ownership. It must neither enter agent mode nor touch a provider or draft.
#[test]
fn terminal_agent_status_live_matches_slash_and_hidden_native() {
    let (mut service, client) = fixture();
    let slash = service
        .execute_agent_shell_command(&client, "/status")
        .unwrap();
    let terminal = service
        .execute_terminal_command(&client, "agent-status")
        .unwrap();
    let slash: serde_json::Value = serde_json::from_str(&slash).unwrap();
    let terminal: serde_json::Value = serde_json::from_str(&terminal).unwrap();
    assert_eq!(terminal["outcomes"][0]["body"], slash["body"]);
    assert_eq!(terminal["outcomes"][0]["live_source"], slash["live_source"]);
    service.agent_shell_store_mut().request_exit("%1").unwrap();
    let before = service.agent_shell_store().get("%1").unwrap().clone();
    let expected = service.runtime_agent_status_display("%1").unwrap();
    service
        .execute_attached_display_command(&client, "agent-status")
        .unwrap();
    assert_eq!(service.agent_shell_store().get("%1").unwrap(), &before);
    let body: serde_json::Value = serde_json::from_str(
        &service
            .execute_terminal_command(&client, "agent-status")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["outcomes"][0]["body"], expected);
    assert!(
        service
            .agent_turn_ledger()
            .latest_turn_for_pane("%1")
            .is_none()
    );
}

/// Strict token grammar rejects duplicates, contradictory scopes, operands and
/// a single quoted operand containing flags. No failed parse acquires a claim.
#[test]
fn terminal_agent_status_rejects_lossy_and_negative_grammar() {
    let (mut service, client) = fixture();
    for args in [
        "--unknown",
        "--extended --extended",
        "--project --all-projects",
        "--all-projects --all-projects",
        "--project=x",
        "--extended=yes",
        "operand",
        "'--extended --project'",
        "''",
    ] {
        let error = service
            .execute_terminal_command(&client, &format!("agent-status {args}"))
            .unwrap_err();
        assert!(
            error.message().contains("usage: agent-status"),
            "{args}: {error}"
        );
        assert!(!service.agent_command_is_active("%1"));
    }
    let error = service
        .execute_attached_display_command(&client, "agent-status --project")
        .unwrap_err();
    assert!(error.message().contains("active-project-unavailable"));
    assert!(!service.agent_command_is_active("%1"));
}

/// Both option orders share the captured report builder. Hidden-native scoped
/// snapshots settle without allocating prompt state, while replacement and Esc
/// dismissals prevent a stale worker from resurrecting a pager.
#[test]
fn terminal_agent_status_snapshot_orders_and_stale_overlay() {
    for args in [
        "--extended --all-projects",
        "--all-projects --extended",
        "--all-projects",
    ] {
        let (mut service, client) = fixture();
        service.agent_shell_store_mut().request_exit("%1").unwrap();
        let prompts = service.agent_prompt_inputs_for_tests().len();
        service
            .execute_attached_display_command(&client, &format!("agent-status {args}"))
            .unwrap();
        let work = claim(&mut service);
        let RuntimeAgentCommandPrepared::Status(report) = &work.prepared else {
            panic!("missing status capture")
        };
        let expected = report.render().unwrap();
        let outcome = RuntimeSessionService::execute_deferred_agent_command(&work);
        assert!(service.complete_agent_command_work(&work, outcome).unwrap());
        let overlay = service.primary_display_overlay().unwrap();
        assert!(overlay.live_source.is_none());
        assert!(
            overlay
                .lines
                .iter()
                .any(|line| line.contains("Accounting Snapshot")),
            "{expected}"
        );
        assert_eq!(service.agent_prompt_inputs_for_tests().len(), prompts);
        assert!(!service.agent_command_is_active("%1"));
    }
    for replace in [false, true] {
        let (mut service, client) = fixture();
        service
            .execute_attached_display_command(&client, "agent-status --extended")
            .unwrap();
        let work = claim(&mut service);
        if replace {
            service
                .execute_attached_display_command(&client, "list-themes")
                .unwrap();
        } else {
            service.clear_primary_display_overlay();
        }
        let before = service.primary_display_overlay().cloned();
        let outcome = RuntimeSessionService::execute_deferred_agent_command(&work);
        assert!(!service.complete_agent_command_work(&work, outcome).unwrap());
        assert_eq!(service.primary_display_overlay(), before.as_ref());
    }
}

/// Plain status refresh stays pane-bound after focus changes and is torn down
/// if a retained native conversation is hidden or replaced while it is open.
#[test]
fn terminal_agent_status_live_ownership_survives_focus_not_rebind() {
    let (mut service, client) = fixture();
    service
        .execute_attached_display_command(&client, "agent-status")
        .unwrap();
    let source = service
        .primary_display_overlay()
        .unwrap()
        .live_source
        .clone()
        .unwrap();
    service
        .execute_terminal_command(&client, "split-window -h")
        .unwrap();
    service
        .refresh_live_overlay_for_client(&client, u64::MAX)
        .unwrap();
    let current = service
        .primary_display_overlay()
        .unwrap()
        .live_source
        .as_ref()
        .unwrap();
    assert_eq!(current.source, source.source);
    service.agent_shell_store_mut().request_exit("%1").unwrap();
    assert!(
        service
            .refresh_live_overlay_for_client(&client, u64::MAX)
            .unwrap()
    );
    assert!(service.primary_display_overlay().is_none());
}

/// A second primary's pager must not invalidate the first client's waiting
/// report even though both commands allocate action-registry generations. The
/// completed snapshot restores only its caller, leaving the other pager intact.
#[test]
fn terminal_agent_status_snapshot_is_client_local() {
    let (mut service, client) = fixture();
    service
        .execute_attached_display_command(&client, "agent-status --extended")
        .unwrap();
    let work = claim(&mut service);
    let other = service
        .attach_primary("other", true, Size::new(100, 40).unwrap(), 121)
        .unwrap();
    service
        .execute_attached_display_command(&other, "list-themes")
        .unwrap();
    let other_overlay = service.primary_display_overlay().cloned();
    let outcome = RuntimeSessionService::execute_deferred_agent_command(&work);
    assert!(service.complete_agent_command_work(&work, outcome).unwrap());
    assert!(
        service
            .primary_display_overlay()
            .unwrap()
            .lines
            .iter()
            .any(|line| line.contains("Accounting Snapshot"))
    );
    service.presentation.activate_client_state(&other);
    assert_eq!(service.primary_display_overlay(), other_overlay.as_ref());
    let observer = service
        .session
        .attach_observer_with_terminal("observer", None, 1)
        .unwrap();
    assert!(
        service
            .execute_terminal_command(&observer, "agent-status")
            .is_err()
    );
    assert!(
        service
            .execute_attached_display_command(&observer, "agent-status --extended")
            .is_err()
    );
}

/// Production textual actor snapshots must execute outside the actor and return
/// completed sequence outcomes. A deterministic worker gate proves lifecycle
/// requests remain responsive, with no provider service and no interactive UI.
#[tokio::test(flavor = "current_thread")]
async fn terminal_agent_status_textual_actor_worker_is_off_actor() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let (mut service, owner) = fixture();
    service.agent_shell_store_mut().request_exit("%1").unwrap();
    let started = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    service.set_deferred_agent_command_probe_for_tests(started.clone(), release.clone());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let client = async {
        let query_handle = handle.clone();
        let query = tokio::spawn(async move {
            query_handle
                .execute_terminal_command(
                    owner,
                    "agent-status --extended; list-panes; agent-status --all-projects".into(),
                )
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
        let body: serde_json::Value = serde_json::from_str(&query.await.unwrap()).unwrap();
        assert_eq!(body["executed"], 3);
        assert!(
            body["outcomes"][0]["body"]
                .as_str()
                .unwrap()
                .contains("Accounting Snapshot")
        );
        assert_eq!(body["outcomes"][1]["command"], "list-panes");
        assert!(
            body["outcomes"][2]["body"]
                .as_str()
                .unwrap()
                .contains("Unattributed Remainder")
        );
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.service.primary_display_overlay().is_none());
}

/// Framed control uses the same off-actor terminal sequence capture, preserves
/// following frame order and replays completed responses without a new lease.
#[tokio::test(flavor = "current_thread")]
async fn terminal_agent_status_control_worker_framing_and_replay() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let (mut service, owner) = fixture();
    service.agent_shell_store_mut().request_exit("%1").unwrap();
    let started = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    service.set_deferred_agent_command_probe_for_tests(started.clone(), release.clone());
    let mut connection = ControlConnectionState::new(true, true);
    connection.rebind_caller_client(owner);
    let request = r#"{"jsonrpc":"2.0","id":"report","method":"terminal/command","params":{"idempotency_key":"report","input":"list-panes; agent-status --all-projects --extended"}}"#;
    let mut input = crate::control::encode_control_body(request);
    input.extend_from_slice(&crate::control::encode_control_body(
        r#"{"jsonrpc":"2.0","id":"after","method":"session/get","params":{}}"#,
    ));
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
        let (first, consumed) =
            crate::control::decode_control_frame(&response.output, 1_000_000).unwrap();
        assert!(first.contains("Accounting Snapshot"), "{first}");
        let (next, _) =
            crate::control::decode_control_frame(&response.output[consumed..], 1_000_000).unwrap();
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
    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.service.primary_display_overlay().is_none());
}
