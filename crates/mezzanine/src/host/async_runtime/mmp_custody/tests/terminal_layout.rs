//! Real terminal commands suspend at layout restore without losing MMP custody.
//!
//! A real native interval, accepted raw source, private snapshot repository and
//! physical end-journal gate exercise direct, framed and attached/prefix paths.
//! Later commands/actions must run against the restored layout, exact RPC retries
//! must not replay them, and selector errors must stop the original sequence.

use super::*;
use crate::control::{ControlConnectionState, decode_control_frame, encode_control_body};
use crate::host::async_runtime::{AsyncRuntimeActorExit, AsyncRuntimeSessionHandle};
use crate::host::terminal::{AttachedTerminalClientStepPlan, TerminalClientLoopAction};
use crate::storage::snapshot::SnapshotRepository;
use mez_core::ids::ClientId;

/// One actual actor and same-daemon raw lease with source accepted before load.
struct Scene {
    handle: AsyncRuntimeSessionHandle,
    runner: tokio::task::JoinHandle<AsyncRuntimeActorExit>,
    primary: ClientId,
    snapshots: SnapshotRepository,
    raw: mez_agent::messaging::MessageConnection,
    owner: tokio::sync::watch::Sender<Option<mez_core::ids::AgentId>>,
    _lease: tokio::sync::watch::Receiver<Option<mez_core::ids::AgentId>>,
}
impl Scene {
    /// Creates actual source and namespace evidence before closing a worker gate.
    async fn new(fixture: &Fixture) -> Self {
        Self::with_blocking_hook(fixture, false).await
    }

    /// Keeps existing native configuration while adding a real pre-action guard.
    async fn with_blocking_hook(fixture: &Fixture, blocked: bool) -> Self {
        let (handle, mut actor, _, _) = native_receive::scene(fixture).await;
        if blocked {
            let mut layers = actor.service.config_layers().to_vec();
            layers.push(crate::config::ConfigLayer {
                name: "terminal-layout-guard".into(), path: None,
                format: crate::config::ConfigFormat::Toml, scope: crate::config::ConfigScope::Primary,
                trusted: true, text: "[hooks.guard]\nevent = \"layout_load\"\nprogram = \"/bin/sh\"\nargs = [\"-c\", \"exit 1\"]\non_failure = \"block\"\n".into(),
            });
            actor.service.replace_config_layers(layers).unwrap();
        }
        let primary = actor
            .service
            .session()
            .attached_primaries()
            .next()
            .unwrap()
            .id
            .clone();
        actor
            .service
            .execute_terminal_command(&primary, "rename-window saved-layout")
            .unwrap();
        let snapshots = SnapshotRepository::new(fixture.path.join("snapshots"));
        snapshots
            .create_from_session(
                "terminal-checkpoint",
                Some("checkpoint".into()),
                actor.service.session(),
            )
            .unwrap();
        actor.service.set_snapshot_repository(snapshots.clone());
        let runner = tokio::spawn(actor.run());
        let (raw, owner, lease) = raw_actor::hello(&handle).await;
        let input = raw_frame(&raw, "before-terminal-load");
        let accepted = handle
            .handle_message_input_with_owner(input, 4096, raw, 100, Some(owner.clone()))
            .await
            .unwrap();
        assert_eq!(raw_actor::body(&accepted.output)["type"], "ack");
        fixture.probe.started.notified().await;
        Self {
            handle,
            runner,
            primary,
            snapshots,
            raw: accepted.connection,
            owner,
            _lease: lease,
        }
    }

    /// Closes the real actor only after its root's original storage work settles.
    async fn finish(self) -> AsyncRuntimeActorExit {
        self.handle.shutdown().await.unwrap();
        let mut exit = self.runner.await.unwrap();
        exit.service.terminate_all_pane_processes().unwrap();
        exit
    }

    /// Issues framed terminal control through the production per-frame dispatcher.
    async fn rpc(&self, id: &str, key: &str, input: &str) -> serde_json::Value {
        let body = serde_json::json!({"jsonrpc":"2.0","id":id,"method":"terminal/command","params":{"input":input,"idempotency_key":key}}).to_string();
        let result = self
            .handle
            .handle_control_input_for_connection_with_snapshots(
                encode_control_body(&body),
                1024 * 1024,
                ControlConnectionState::trusted_existing_client(self.primary.clone()),
                self.snapshots.clone(),
            )
            .await
            .unwrap();
        let (body, consumed) = decode_control_frame(&result.output, 1024 * 1024).unwrap();
        assert_eq!(consumed, result.output.len());
        serde_json::from_str(&body).unwrap()
    }
}

/// Immutable real raw frame; the same original connection survives layout loads.
fn raw_frame(connection: &mez_agent::messaging::MessageConnection, id: &str) -> Vec<u8> {
    let id_agent = connection.agent_id.as_ref().unwrap().as_str();
    crate::protocol::message::encode_mmp_body(&serde_json::json!({
        "protocol":"mmp/1","type":"send","id":id,"time":"original","scope":"session",
        "sender":{"agent_id":id_agent},"recipient":{"agent_id":id_agent},"ttl_ms":null,
        "correlation_id":null,"content_type":"text/plain; charset=utf-8","payload":"raw original across terminal load"
    }).to_string())
}

/// A panic or assertion cannot leave a generated worker waiting on its gate.
struct Gate(Arc<Probe>);
impl Drop for Gate {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Observes the physical original end start without relying on an idle label.
async fn wait_end(fixture: &Fixture) {
    tokio::time::timeout(Duration::from_secs(15), fixture.probe.started.notified())
        .await
        .unwrap();
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT count(*) FROM records WHERE key LIKE 'o1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

/// Prefix status is captured/rendered before its native owner retires. The
/// later rename must run after load, or the saved name would overwrite it.
/// The raw source/connection survives both restore and a subsequent real send.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_layout_sequence_waits_for_end_and_preserves_command_order() {
    let fixture = Fixture::new(true);
    let scene = Scene::new(&fixture).await;
    let _gate = Gate(fixture.probe.clone());
    *fixture.probe.gate.0.lock().unwrap() = false;
    let handle = scene.handle.clone();
    let primary = scene.primary.clone();
    let task = tokio::spawn(async move {
        handle.execute_terminal_command(primary, "rename-window prefix-before; agent-status --extended; load-layout --name checkpoint; rename-window after; list-windows".into()).await
    });
    wait_end(&fixture).await;
    assert!(!task.is_finished());
    assert!(scene.handle.lifecycle_state().await.is_ok());
    fixture.probe.release();
    let output = tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["executed"], 5);
    let commands = value["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["command"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        commands,
        [
            "rename-window",
            "agent-status",
            "load-layout",
            "rename-window",
            "list-windows"
        ]
    );
    let raw_id = scene.raw.agent_id.as_ref().unwrap().clone();
    let batch = scene
        .handle
        .message_fanout_ready_for(raw_id, 200, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        batch.batch.batch.messages[0].envelope.id,
        "before-terminal-load"
    );
    let next = scene
        .handle
        .handle_message_input_with_owner(
            raw_frame(&scene.raw, "after-terminal-load"),
            4096,
            scene.raw.clone(),
            201,
            Some(scene.owner.clone()),
        )
        .await
        .unwrap();
    assert_eq!(raw_actor::body(&next.output)["type"], "ack");
    let primary = scene.primary.clone();
    let exit = scene.finish().await;
    assert_eq!(
        exit.service
            .session()
            .active_window_for(&primary)
            .unwrap()
            .name,
        "after"
    );
    assert_eq!(exit.service.message_service().next_message_sequence(), 3);
}

/// A failed selector stops later commands and releases the original lease/key.
/// A successful framed sequence and its exact retry preserve final outcomes;
/// neither retry of success nor retry of failure can replay the original prefix.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_layout_rpc_preserves_failures_and_final_idempotency() {
    let fixture = Fixture::new(true);
    let scene = Scene::new(&fixture).await;
    let bad_input =
        "rename-window prefix-before; load-layout --name absent; rename-window forbidden";
    let failed = scene
        .rpc("failed-layout", "failed-layout-key", bad_input)
        .await;
    assert!(
        failed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no stored layout named"),
        "{failed}"
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT count(*) FROM records WHERE key LIKE 'o1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let input = "load-layout --name checkpoint; rename-window after; list-windows";
    let first_body = serde_json::json!({"jsonrpc":"2.0","id":"successful-layout","method":"terminal/command","params":{"input":input,"idempotency_key":"successful-layout-key"}}).to_string();
    let next_body = serde_json::json!({"jsonrpc":"2.0","id":"following-frame","method":"terminal/command","params":{"input":"rename-window frame-tail; list-windows","idempotency_key":"following-frame-key"}}).to_string();
    let mut frames = encode_control_body(&first_body);
    frames.extend_from_slice(&encode_control_body(&next_body));
    let first = scene
        .handle
        .handle_control_input_for_connection_with_snapshots(
            frames.clone(),
            1024 * 1024,
            ControlConnectionState::trusted_existing_client(scene.primary.clone()),
            scene.snapshots.clone(),
        )
        .await
        .unwrap();
    assert_eq!(first.consumed, frames.len());
    let (body, first_bytes) = decode_control_frame(&first.output, 1024 * 1024).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["result"]["executed"], 3, "{value}");
    let (body, next_bytes) =
        decode_control_frame(&first.output[first_bytes..], 1024 * 1024).unwrap();
    let next: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(next["result"]["executed"], 2, "{next}");
    assert_eq!(first_bytes + next_bytes, first.output.len());
    let attempts = fixture.probe.attempts.load(Ordering::SeqCst);
    let again = scene
        .handle
        .handle_control_input_for_connection_with_snapshots(
            frames,
            1024 * 1024,
            first.connection,
            scene.snapshots.clone(),
        )
        .await
        .unwrap();
    assert_eq!(again.output, first.output);
    let failed_again = scene
        .rpc("failed-layout", "failed-layout-key", bad_input)
        .await;
    assert_eq!(failed_again, failed);
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), attempts);
    let primary = scene.primary.clone();
    let exit = scene.finish().await;
    assert_eq!(
        exit.service
            .session()
            .active_window_for(&primary)
            .unwrap()
            .name,
        "frame-tail"
    );
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&crate::protocol::event::EventAudience::AllPrimaries);
    assert_eq!(
        events
            .iter()
            .filter(
                |event| event.payload.contains("\"method\":\"snapshot/resume\"")
                    && event.payload.contains("\"resumed\":true")
            )
            .count(),
        1
    );
}

/// Builds identical real prefix/key-bound paths with one following action.
fn terminal_command_step(prefix: bool, command: &str) -> AttachedTerminalClientStepPlan {
    let mut actions = if prefix {
        vec![
            TerminalClientLoopAction::EnterPrefixKeyMode,
            TerminalClientLoopAction::ExecuteMux(mez_mux::input::MuxAction::EnterCommandPrompt),
            TerminalClientLoopAction::ForwardToPane(format!("{command}\r").into_bytes()),
        ]
    } else {
        vec![TerminalClientLoopAction::ExecuteCommand(command.into())]
    };
    actions.push(TerminalClientLoopAction::ExecuteCommand(
        "rename-window tail".into(),
    ));
    AttachedTerminalClientStepPlan {
        actions,
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    }
}

/// Direct key-bound commands and the colon prompt share the same AST suspension.
/// A later action in the same already-planned step must target the restored
/// layout, and its actual applied report is merged without replaying the prefix.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attached_and_prefix_layout_load_resume_following_actions() {
    for prefix in [false, true] {
        // UI interception can account for opening a command prompt differently
        // from a direct command. Compare the same real path without a load.
        let reference_fixture = Fixture::new(true);
        let reference = Scene::new(&reference_fixture).await;
        let reference_step = terminal_command_step(prefix, "rename-window after");
        let expected = reference
            .handle
            .apply_attached_terminal_step_plan(reference.primary.clone(), reference_step)
            .await
            .unwrap();
        reference.finish().await;
        let fixture = Fixture::new(true);
        let scene = Scene::new(&fixture).await;
        let _gate = Gate(fixture.probe.clone());
        *fixture.probe.gate.0.lock().unwrap() = false;
        let command = "load-layout --name checkpoint; rename-window after";
        let step = terminal_command_step(prefix, command);
        let handle = scene.handle.clone();
        let primary = scene.primary.clone();
        let task = tokio::spawn(async move {
            handle
                .apply_attached_terminal_step_plan(primary, step)
                .await
        });
        wait_end(&fixture).await;
        assert!(!task.is_finished());
        assert!(scene.handle.lifecycle_state().await.is_ok());
        fixture.probe.release();
        let applied = tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            applied.mux_actions_applied, expected.mux_actions_applied,
            "prefix={prefix}, applied={applied:?}, reference={expected:?}"
        );
        assert!(applied.unsupported_actions.is_empty());
        let primary = scene.primary.clone();
        let exit = scene.finish().await;
        assert_eq!(
            exit.service
                .session()
                .active_window_for(&primary)
                .unwrap()
                .name,
            "tail",
            "prefix={prefix}"
        );
        for event in exit
            .service
            .event_log()
            .unwrap()
            .replay_for(&crate::protocol::event::EventAudience::AllPrimaries)
        {
            let value: serde_json::Value =
                serde_json::from_str(&event.payload).unwrap_or(serde_json::Value::Null);
            if value.get("key_binding_command").is_some()
                && let Some(output) = value.get("output").and_then(serde_json::Value::as_str)
            {
                let result: serde_json::Value = serde_json::from_str(output).unwrap();
                assert_ne!(result["executed"], 0, "premature deferred output: {value}");
            }
        }
    }
}

/// The real terminal/step RPC retains its application/view schema, consumed
/// bytes and idempotency result while colon-prompt load waits on the old end.
/// Empty-input view refresh must not resubmit the command a second time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_step_layout_load_keeps_step_schema_and_does_not_replay_input() {
    let fixture = Fixture::new(true);
    let scene = Scene::new(&fixture).await;
    let connection = ControlConnectionState::trusted_existing_client(scene.primary.clone());
    let open = serde_json::json!({"jsonrpc":"2.0","id":"open-prompt","method":"terminal/step","params":{"input_bytes":[1,58],"render":false,"idempotency_key":"open-layout-prompt"}}).to_string();
    let opened = scene
        .handle
        .handle_control_input_for_connection_with_snapshots(
            encode_control_body(&open),
            1024 * 1024,
            connection,
            scene.snapshots.clone(),
        )
        .await
        .unwrap();
    let bytes = b"load-layout --name checkpoint; rename-window rpc-after\r";
    let body = serde_json::json!({"jsonrpc":"2.0","id":"load-prompt","method":"terminal/step","params":{"input_bytes":bytes.as_slice(),"render":true,"idempotency_key":"step-layout-original"}}).to_string();
    let _gate = Gate(fixture.probe.clone());
    *fixture.probe.gate.0.lock().unwrap() = false;
    let handle = scene.handle.clone();
    let snapshots = scene.snapshots.clone();
    let frame = encode_control_body(&body);
    let task = tokio::spawn(async move {
        handle
            .handle_control_input_for_connection_with_snapshots(
                frame,
                1024 * 1024,
                opened.connection,
                snapshots,
            )
            .await
    });
    wait_end(&fixture).await;
    assert!(!task.is_finished());
    fixture.probe.release();
    let result = tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let (text, _) = decode_control_frame(&result.output, 1024 * 1024).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["result"]["input_bytes"], bytes.len(), "{value}");
    assert!(value["result"]["application"].is_object(), "{value}");
    assert!(value["result"].get("outcomes").is_none());
    assert!(!value["result"]["view"].is_null());
    let retry = scene
        .handle
        .handle_control_input_for_connection_with_snapshots(
            encode_control_body(&body),
            1024 * 1024,
            result.connection,
            scene.snapshots.clone(),
        )
        .await
        .unwrap();
    assert_eq!(retry.output, result.output);
    let primary = scene.primary.clone();
    let exit = scene.finish().await;
    assert_eq!(
        exit.service
            .session()
            .active_window_for(&primary)
            .unwrap()
            .name,
        "rpc-after"
    );
}

/// Cancelling only the terminal response observer cannot detach a real O1
/// write or abandon accepted remaining commands. The same actor remains live,
/// completes one original load and releases its sequence slot for later input.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_layout_keeps_original_sequence_after_observer_cancellation() {
    let fixture = Fixture::new(true);
    let scene = Scene::new(&fixture).await;
    let _gate = Gate(fixture.probe.clone());
    *fixture.probe.gate.0.lock().unwrap() = false;
    let handle = scene.handle.clone();
    let primary = scene.primary.clone();
    let task = tokio::spawn(async move {
        handle
            .execute_terminal_command(
                primary,
                "load-layout --name checkpoint; rename-window after-cancel".into(),
            )
            .await
    });
    wait_end(&fixture).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    fixture.probe.release();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match scene
                .handle
                .execute_terminal_command(scene.primary.clone(), "list-windows".into())
                .await
            {
                Ok(output) => {
                    assert!(output.contains("after-cancel"), "{output}");
                    break;
                }
                Err(error) => {
                    assert!(error.message().contains("pending"), "{error}");
                    tokio::task::yield_now().await;
                }
            }
        }
    })
    .await
    .unwrap();
    let primary = scene.primary.clone();
    let exit = scene.finish().await;
    assert_eq!(
        exit.service
            .session()
            .active_window_for(&primary)
            .unwrap()
            .name,
        "after-cancel"
    );
    assert!(exit.service.message_service().payload_observer().is_some());
}

/// A buffered action may itself contain another load. Each original sequence
/// keeps its reply through the next actual attached-step continuation, and the
/// final action runs only after both restores instead of being dropped/replayed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_layout_nested_following_load_keeps_original_reply() {
    let fixture = Fixture::new(true);
    let scene = Scene::new(&fixture).await;
    let step = AttachedTerminalClientStepPlan {
        actions: vec![
            TerminalClientLoopAction::ExecuteCommand("load-layout --name checkpoint".into()),
            TerminalClientLoopAction::ExecuteCommand("load-layout --name checkpoint".into()),
            TerminalClientLoopAction::ExecuteCommand("rename-window nested-tail".into()),
        ],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    let report = tokio::time::timeout(
        Duration::from_secs(20),
        scene
            .handle
            .apply_attached_terminal_step_plan(scene.primary.clone(), step),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(report.mux_actions_applied, 3, "{report:?}");
    let primary = scene.primary.clone();
    let exit = scene.finish().await;
    assert_eq!(
        exit.service
            .session()
            .active_window_for(&primary)
            .unwrap()
            .name,
        "nested-tail"
    );
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&crate::protocol::event::EventAudience::AllPrimaries);
    assert_eq!(
        events
            .iter()
            .filter(
                |event| event.payload.contains("\"method\":\"snapshot/resume\"")
                    && event.payload.contains("\"resumed\":true")
            )
            .count(),
        2
    );
}

/// A failed attached load is an ordinary contained action error. It cannot
/// report a completed mux command, execute later commands/actions, or leave
/// its private sequence slot pinned so future input is permanently rejected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attached_failed_layout_load_stops_later_work_and_releases_owner() {
    let fixture = Fixture::new(true);
    let scene = Scene::new(&fixture).await;
    let step = terminal_command_step(false, "load-layout --name absent; rename-window forbidden");
    let report = scene
        .handle
        .apply_attached_terminal_step_plan(scene.primary.clone(), step)
        .await
        .unwrap();
    assert_eq!(report.mux_actions_applied, 0, "{report:?}");
    assert!(report.full_redraw_required);
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT count(*) FROM records WHERE key LIKE 'o1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let current = scene
        .handle
        .execute_terminal_command(scene.primary.clone(), "list-windows".into())
        .await
        .unwrap();
    assert!(current.contains("saved-layout"), "{current}");
    assert!(!current.contains("forbidden"));
    assert!(!current.contains("tail"));
    let primary = scene.primary.clone();
    let exit = scene.finish().await;
    assert_eq!(
        exit.service
            .session()
            .active_window_for(&primary)
            .unwrap()
            .name,
        "saved-layout"
    );
}

/// A real snapshot/resume pre-action refusal propagates through the internal
/// actor control response. No end or topology mutation is allowed before that
/// refusal, later commands stay unexecuted, and the original terminal slot/key
/// is released instead of returning a successful Noop or pinning future input.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_layout_surfaces_actual_snapshot_resume_refusal() {
    let fixture = Fixture::new(true);
    let scene = Scene::with_blocking_hook(&fixture, true).await;
    let failed = scene
        .rpc(
            "blocked-layout",
            "blocked-layout-key",
            "load-layout --name checkpoint; rename-window forbidden",
        )
        .await;
    assert!(failed["error"].is_object(), "{failed}");
    assert!(
        failed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("layout_load"),
        "{failed}"
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT count(*) FROM records WHERE key LIKE 'o1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let current = scene
        .handle
        .execute_terminal_command(scene.primary.clone(), "list-windows".into())
        .await
        .unwrap();
    assert!(current.contains("saved-layout"));
    assert!(!current.contains("forbidden"));
    let primary = scene.primary.clone();
    let exit = scene.finish().await;
    assert_eq!(
        exit.service
            .session()
            .active_window_for(&primary)
            .unwrap()
            .name,
        "saved-layout"
    );
}
