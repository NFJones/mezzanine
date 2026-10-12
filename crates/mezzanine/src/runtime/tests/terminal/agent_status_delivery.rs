//! Production delivery, binding and replay fences for terminal status reports.
//!
//! Worker gates prove actor responsiveness and in-flight replay behavior without
//! sleeps. Adapter handoff makes queued input observable at the host boundary,
//! while the shared retained-conversation fixture keeps status data deterministic.

use super::agent_status::fixture;
use super::*;

/// A gated control retry must conflict before rerunning its preceding mutation,
/// and the original completed reply must subsequently replay without a worker.
#[tokio::test(flavor = "current_thread")]
async fn terminal_agent_status_control_in_flight_retry_does_not_repeat_mutation() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let (mut service, owner) = fixture();
    let started = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    service.set_deferred_agent_command_probe_for_tests(started.clone(), release.clone());
    let mut connection = ControlConnectionState::new(true, true);
    connection.rebind_caller_client(owner);
    let request = r#"{"jsonrpc":"2.0","id":"report","method":"terminal/command","params":{"idempotency_key":"report","input":"zen toggle; agent-status --extended"}}"#;
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let client = async {
        let query_handle = handle.clone();
        let query_connection = connection.clone();
        let query = tokio::spawn(async move {
            query_handle
                .handle_control_input_for_connection(
                    crate::control::encode_control_body(request),
                    1_000_000,
                    query_connection,
                )
                .await
                .unwrap()
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), started.notified())
            .await
            .unwrap();
        let retry = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            handle.handle_control_input_for_connection(
                crate::control::encode_control_body(request),
                1_000_000,
                connection.clone(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let (body, _) = crate::control::decode_control_frame(&retry.output, 1_000_000).unwrap();
        assert!(body.contains("still in progress"), "{body}");
        release.notify_one();
        let original = query.await.unwrap();
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
        assert_eq!(replay.output, original.output);
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::join!(client, actor.run());
    assert!(
        exit.service.terminal_zen_mode(),
        "duplicate retry toggled zen twice"
    );
    assert!(!exit.service.agent_command_is_active("%1"));
    assert!(exit.service.primary_display_overlay().is_none());
}

/// Configured prefix commands and sequences enter the same production terminal
/// dispatcher. A final snapshot owns its waiting pager; earlier queued snapshots
/// are cancelled, so a stale dispatch cannot replace the final command's display.
#[test]
fn terminal_agent_status_configured_binding_and_sequence_handoff() {
    let (mut service, primary) = fixture();
    let mut config = TerminalClientLoopConfig::default();
    config.command_bindings.insert(
        KeyChord::new(KeyCode::Char('x')),
        "list-panes; agent-status --all-projects".into(),
    );
    let action = crate::host::terminal::route_client_input(b"\x01x", &config).unwrap();
    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![action],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(report.forwarded_bytes, 0);
    let dispatch = service.next_pending_deferred_agent_command().unwrap();
    service.retire_pending_deferred_agent_command();
    let work = service
        .claim_agent_command_work(
            &dispatch.primary_client_id,
            &dispatch.pane_id,
            &dispatch.command,
            &dispatch.input,
            dispatch.claim_generation,
            &dispatch.conversation_id,
        )
        .unwrap()
        .unwrap();
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
    assert!(!service.agent_command_is_active("%1"));
    service
        .execute_attached_display_command(&primary, "agent-status --extended; list-panes")
        .unwrap();
    let dispatch = service.next_pending_deferred_agent_command().unwrap();
    service.retire_pending_deferred_agent_command();
    assert!(
        service
            .claim_agent_command_work(
                &dispatch.primary_client_id,
                &dispatch.pane_id,
                &dispatch.command,
                &dispatch.input,
                dispatch.claim_generation,
                &dispatch.conversation_id
            )
            .unwrap()
            .is_none()
    );
    assert!(!service.agent_command_is_active("%1"));
}

/// Terminal and slash status render identical Markdown rows, styles and copy
/// sources. Refresh/resize keep the existing pager scroll and search state rather
/// than restarting a command or rebuilding an agent prompt.
#[test]
fn terminal_agent_status_pager_layout_and_interaction_parity() {
    let (mut service, owner) = fixture();
    let slash = service
        .execute_agent_shell_command(&owner, "/status")
        .unwrap();
    service
        .set_agent_prompt_response_display_output_for_tests("%1", &slash)
        .unwrap();
    let expected = service.primary_display_overlay().unwrap().clone();
    service
        .execute_attached_display_command(&owner, "agent-status")
        .unwrap();
    let actual = service.primary_display_overlay().unwrap();
    assert_eq!(actual.lines, expected.lines);
    assert_eq!(actual.line_style_spans, expected.line_style_spans);
    assert_eq!(actual.line_copy_texts, expected.line_copy_texts);
    service.apply_primary_display_overlay_scroll(2).unwrap();
    {
        let overlay = service.primary_display_overlay_mut().unwrap();
        overlay.search_query = Some("Pane".into());
    }
    let before = service.primary_display_overlay().unwrap().scroll_offset;
    service
        .refresh_live_overlay_for_client(&owner, u64::MAX)
        .unwrap();
    service
        .resize_attached_primary_terminal(&owner, Size::new(50, 20).unwrap())
        .unwrap();
    let overlay = service.primary_display_overlay().unwrap();
    assert_eq!(overlay.search_query.as_deref(), Some("Pane"));
    assert_eq!(overlay.scroll_offset, before);
    assert!(overlay.live_source.is_some());
}

/// Mixed sequence commands retain ordinary adapter-side pane input admission in
/// both textual and framed-control paths. The status worker gate is held while
/// the exact queued prefix input is drained, proving it is not parked in service
/// state until an unrelated later input happens to trigger a drain.
#[tokio::test(flavor = "current_thread")]
async fn terminal_agent_status_mixed_sequence_drains_adapter_input() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    use crate::runtime::PaneProcessIoEffect;
    for control in [false, true] {
        let (mut service, owner) = fixture();
        service.agent_shell_store_mut().request_exit("%1").unwrap();
        service.start_initial_pane_process(None).unwrap();
        let (instance, mut process) = service
            .take_running_pane_process_instances_for_adapter(1)
            .unwrap()
            .remove(0);
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        service.set_deferred_agent_command_probe_for_tests(started.clone(), release.clone());
        let mut connection = ControlConnectionState::new(true, true);
        connection.rebind_caller_client(owner.clone());
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let client = async {
            let query_handle = handle.clone();
            let query = tokio::spawn(async move {
                if control {
                    let request = r#"{"jsonrpc":"2.0","id":"mixed","method":"terminal/command","params":{"idempotency_key":"mixed","input":"send-prefix; agent-status --extended"}}"#;
                    let response = query_handle
                        .handle_control_input_for_connection(
                            crate::control::encode_control_body(request),
                            1_000_000,
                            connection,
                        )
                        .await
                        .unwrap();
                    crate::control::decode_control_frame(&response.output, 1_000_000)
                        .unwrap()
                        .0
                } else {
                    query_handle
                        .execute_terminal_command(
                            owner,
                            "send-prefix; agent-status --extended".into(),
                        )
                        .await
                        .unwrap()
                }
            });
            tokio::time::timeout(std::time::Duration::from_secs(3), started.notified())
                .await
                .unwrap();
            let effects = handle
                .drain_pane_process_io_side_effects(instance.clone(), 8)
                .await
                .unwrap();
            assert!(effects.iter().any(|effect| matches!(effect, RuntimeSideEffect::PaneProcessIo { instance: target, effect: PaneProcessIoEffect::WriteInput { bytes } } if target == &instance && bytes == b"\x01")), "{effects:?}");
            release.notify_one();
            assert!(query.await.unwrap().contains("Accounting Snapshot"));
            handle.shutdown().await.unwrap();
        };
        let ((), exit) = tokio::join!(client, actor.run());
        assert!(!exit.service.agent_command_is_active("%1"));
        process
            .terminate(std::time::Duration::from_millis(100))
            .unwrap();
    }
}

/// Changes to cwd, native conversation, visibility or client attachment between
/// acceptance and worker settlement must discard the captured report and release
/// its lease rather than relabel counters or replace the surviving client pager.
#[test]
fn terminal_agent_status_snapshot_rejects_changed_capture() {
    for change in ["cwd", "conversation", "visibility"] {
        let (mut service, owner) = fixture();
        service
            .execute_attached_display_command(&owner, "agent-status --extended")
            .unwrap();
        let dispatch = service.next_pending_deferred_agent_command().unwrap();
        service.retire_pending_deferred_agent_command();
        let work = service
            .claim_agent_command_work(
                &dispatch.primary_client_id,
                &dispatch.pane_id,
                &dispatch.command,
                &dispatch.input,
                dispatch.claim_generation,
                &dispatch.conversation_id,
            )
            .unwrap()
            .unwrap();
        match change {
            "cwd" => service.set_pane_current_working_directory("%1", PathBuf::from("/tmp")),
            "conversation" => {
                service.agent_shell_store_mut().remove_session("%1");
                service
                    .agent_shell_store_mut()
                    .enter_or_resume("%1")
                    .unwrap();
            }
            _ => {
                service.agent_shell_store_mut().request_exit("%1").unwrap();
            }
        }
        let before = service.primary_display_overlay().cloned();
        let outcome = RuntimeSessionService::execute_deferred_agent_command(&work);
        assert!(
            !service.complete_agent_command_work(&work, outcome).unwrap(),
            "{change}"
        );
        assert_eq!(service.primary_display_overlay(), before.as_ref());
        assert!(!service.agent_command_is_active("%1"));
    }
}

/// Last-pane closure must publish its lifecycle only after the control response
/// flush, even when report preparation fails or another frame is pipelined. The
/// consumed prefix stops at the terminal mutation and never executes its successor.
#[tokio::test(flavor = "current_thread")]
async fn terminal_agent_status_control_last_pane_close_retains_flush_fence() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    for input in [
        "agent-status --extended; kill-pane --force",
        "kill-pane --force; agent-status --extended",
    ] {
        let (service, owner) = fixture();
        let mut connection = ControlConnectionState::new(true, true);
        connection.rebind_caller_client(owner);
        let request = serde_json::json!({"jsonrpc":"2.0", "id":"close", "method":"terminal/command", "params":{"idempotency_key":"close", "input":input}}).to_string();
        let mut bytes = crate::control::encode_control_body(&request);
        let expected_consumed = bytes.len();
        bytes.extend_from_slice(&crate::control::encode_control_body(
            r#"{"jsonrpc":"2.0","id":"after","method":"session/get","params":{}}"#,
        ));
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let mut lifecycle = handle.lifecycle_state_watcher();
        let initial = *lifecycle.borrow_and_update();
        let client = async {
            let response = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                handle.handle_control_input_for_connection(bytes, 1_000_000, connection),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(response.consumed, expected_consumed, "{input}");
            let (body, consumed) =
                crate::control::decode_control_frame(&response.output, 1_000_000).unwrap();
            assert!(body.contains("\"id\":\"close\""), "{body}");
            assert_eq!(consumed, response.output.len());
            assert_eq!(
                *lifecycle.borrow(),
                initial,
                "shutdown published before response flush"
            );
            // Dropping the transport-owned response exercises the guard's
            // cancellation fallback without borrowing private host internals.
            drop(response);
            tokio::time::timeout(std::time::Duration::from_secs(1), lifecycle.changed())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(*lifecycle.borrow(), RuntimeLifecycleState::Killed);
            handle.shutdown().await.unwrap();
        };
        let ((), exit) = tokio::join!(client, actor.run());
        assert_eq!(
            exit.service.lifecycle_state(),
            RuntimeLifecycleState::Killed
        );
    }
}
