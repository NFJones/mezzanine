//! Connected supervised provider/writer survival after native local refusal.

use super::*;
use crate::host::async_runtime::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Use the established actor/provider fixture stack, not a changed runtime
/// default. The production supervisor retains actual writer/driver joins.
#[test]
fn runtime_native_refusal_supervised_writer_and_safe_pane_complete() {
    std::thread::Builder::new()
        .name("native-refusal-supervised".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(qualify());
        })
        .unwrap()
        .join()
        .unwrap_or_else(|p| std::panic::resume_unwind(p));
}

async fn qualify() {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};
    use std::os::unix::fs::DirBuilderExt;
    let root = std::env::temp_dir().join(format!(
        "mez-supervised-native-refusal-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let (mut service, primary) = admit_siblings(Discovery::Absent, AgentLogLevel::Verbose, None);
    let original = service.agent_turn_executions()["turn-1"].clone();
    let dispatch = service
        .claim_native_shell_action("turn-1", "shell-a")
        .unwrap()
        .unwrap();
    let cause =
        crate::error::MezError::invalid_state("original native owner refuses provider reporting");
    service
        .fail_configured_agent_provider_task("turn-1", &cause)
        .unwrap();
    let outcome = crate::runtime::execute_native_shell_dispatch(dispatch);
    let key = (
        outcome.turn_id.clone(),
        outcome.action_id.clone(),
        outcome.marker.clone(),
    );
    assert!(service.ensure_completion_admission_open().is_ok());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "offline-safe-sibling".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                r#"[permissions]
sandbox = "policy-only"
[agents]
routing = false
default_provider = "runtime-batch"
default_model_profile = "default"
shell_mode = "native"
[providers.runtime-batch]
kind = "openai-compatible"
base_url = "{endpoint}"
models = ["test"]
default_model = "test"
[providers.runtime-batch.options]
maap_output = "structured_json"
structured_output = "json_schema"
[model_profiles.default]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
"#
            ),
        }])
        .unwrap();
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root.join("auth")),
    ));
    let pane = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, None)
        .unwrap()
        .pane_id;
    wait_until_primary_shell_foreground(&mut service, &pane);
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane)
        .unwrap();
    let safe = service
        .start_agent_prompt_turn(&pane, "complete safe independent work")
        .unwrap();
    let store = crate::storage::transcript::AgentTranscriptStore::new(root.join("transcripts"));
    let rows = vec![TranscriptEntry {
        conversation_id: "supervised-ack".into(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "independent-write".into(),
        agent_id: "agent-independent".into(),
        pane_id: pane.clone(),
        content: "exact acknowledged source".into(),
    }];
    let batch =
        crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows.clone()).unwrap();
    let path = store.transcript_path("supervised-ack").unwrap();
    service.queue_transcript_for_tests(
        crate::runtime::RuntimeSideEffect::PersistTranscriptEntries {
            store: store.clone(),
            path: path.clone(),
            batch: batch.clone(),
        },
    );
    let (handle, actor) = AsyncRuntimeSessionActor::new(service, Default::default()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            bytes.push(byte[0]);
            assert!(bytes.len() < 65536);
        }
        let head = bytes.len();
        let length: usize = String::from_utf8_lossy(&bytes)
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse().unwrap())
            })
            .unwrap();
        assert!(length < 1_048_576);
        bytes.resize(head + length, 0);
        socket.read_exact(&mut bytes[head..]).await.unwrap();
        let request: serde_json::Value = serde_json::from_slice(&bytes[head..]).unwrap();
        seen.fetch_add(1, Ordering::SeqCst);
        let content = serde_json::json!({"rationale":"finish independent work","actions":[{
            "type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"safe sibling completed"
        }]}).to_string();
        let (kind, body) = if request["stream"] == true {
            let chunk = serde_json::json!({"id":"safe","model":"test","choices":[{"index":0,"delta":{"role":"assistant","content":content},"finish_reason":null}]}).to_string();
            let end = serde_json::json!({"id":"safe","model":"test","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}).to_string();
            (
                "text/event-stream",
                format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"),
            )
        } else {
            ("application/json", serde_json::json!({"id":"safe","model":"test","choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}]}).to_string())
        };
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    let (finish, finished) = tokio::sync::oneshot::channel();
    let effects = AsyncRuntimeSideEffectServiceConfig {
        max_polls: u64::MAX,
        drain_limit: 32,
        idle_interval: std::time::Duration::from_millis(1),
    };
    let services = vec![
        build_async_persistence_side_effect_service("persistence", handle.clone(), effects)
            .unwrap(),
        build_async_agent_provider_service(
            "providers",
            handle.clone(),
            AsyncAgentProviderServiceConfig::new(1).unwrap(),
        )
        .unwrap(),
        build_async_runtime_timer_side_effect_service(
            "timers",
            handle.clone(),
            effects,
            crate::runtime::current_unix_millis(),
        )
        .unwrap(),
        AsyncRuntimeService::new("control-lifetime", async move {
            let _ = finished.await;
            Ok(AsyncRuntimeServiceExit::completed(0))
        }),
    ];
    let supervisor_handle = handle.clone();
    let supervision = async move {
        let result = supervise_async_runtime_services(services, std::future::pending()).await;
        // Match SessionRuntime: service failure closes actor ingress too.
        let shutdown = supervisor_handle.shutdown().await;
        (result, shutdown)
    };
    let client = async {
        // The native effect already happened, but its first actor result is
        // deliberately delayed until the real supervised services are live.
        // Ignore the first reply's semantic result and redeliver the SAME
        // occurrence to model a lost completion ACK, never another execution.
        let mut delivered = crate::runtime::RuntimeEventBatch::new();
        delivered.push(crate::runtime::RuntimeEvent::NativeShell(outcome.clone()));
        let first = handle.submit_runtime_events(delivered).await;
        let mut repeated = crate::runtime::RuntimeEventBatch::new();
        repeated.push(crate::runtime::RuntimeEvent::NativeShell(outcome));
        let second = handle.submit_runtime_events(repeated).await;
        let progress = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let live = handle.lifecycle_state().await?;
                if live != crate::runtime::RuntimeLifecycleState::Running {
                    return Err(crate::error::MezError::invalid_state(
                        "runtime stopped before safe sibling",
                    ));
                }
                if calls.load(Ordering::SeqCst) == 1
                    && !handle.agent_turn_is_running(&safe.turn_id).await?
                    && store.pending_append_receipts().unwrap().is_empty()
                    && !handle
                        .owns_transcript_claim_for_tests(batch.clone(), path.clone())
                        .await?
                {
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await;
        let mut duplicate_ack = crate::runtime::RuntimeEventBatch::new();
        duplicate_ack.push(crate::runtime::RuntimeEvent::Persistence(
            crate::runtime::PersistenceEvent::TranscriptCompleted {
                conversation_id: "supervised-ack".into(),
                first_sequence: 1,
                batch,
                path,
                bytes: 1,
            },
        ));
        let duplicate_ack = handle.submit_runtime_events(duplicate_ack).await;
        let protocol = async {
            let initialized = handle.handle_control_input_for_connection(
                crate::control::encode_control_body(r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":2,"client_name":"native-refusal-control","client":{"name":"native-refusal-control","interactive":true,"terminal":{"columns":100,"rows":30,"term":"xterm"}}}}"#),
                1024 * 1024, crate::control::ControlConnectionState::new(true, true),
            ).await?;
            let shown = handle.handle_control_input_for_connection(
                crate::control::encode_control_body(&format!(r#"{{"jsonrpc":"2.0","id":"show","method":"agent/shell/show","params":{{"target":{{"pane_id":"{pane}"}},"idempotency_key":"safe-live-show"}}}}"#)),
                1024 * 1024, initialized.connection,
            ).await?;
            let (body, _) = crate::control::decode_control_frame(&shown.output, 1024 * 1024)?;
            Ok::<_, crate::error::MezError>(body.contains(r#""visible":true"#))
        }.await;
        let control = handle.lifecycle_state().await;
        let _ = finish.send(());
        (progress, control, first, second, duplicate_ack, protocol)
    };
    let (
        (supervision, shutdown),
        (progress, control, first, second, duplicate_ack, protocol),
        mut exit,
    ) = tokio::join!(supervision, client, actor.run());
    if !server.is_finished() {
        server.abort();
    }
    let server_result = server.await;
    let retained = exit
        .service
        .retained_native_outcome_for_tests(&key)
        .is_some();
    let source = exit
        .service
        .provider_failure_source_for_tests("turn-1")
        .and_then(|(_, execution)| execution)
        .cloned();
    let safe_state = exit
        .service
        .agent_turn_ledger()
        .turn(&safe.turn_id)
        .unwrap()
        .state;
    exit.service.terminate_all_pane_processes().unwrap();
    let actual = store.inspect("supervised-ack").unwrap();
    drop((exit, handle, store));
    fs::remove_dir_all(&root).unwrap();
    assert!(!root.exists());
    assert!(supervision.is_ok(), "{supervision:?}");
    assert!(shutdown.is_ok(), "{shutdown:?}");
    assert!(
        first.is_ok() && second.is_ok(),
        "late original result lost custody"
    );
    assert_eq!(
        duplicate_ack.unwrap().applied,
        0,
        "retired ACK cannot reapply"
    );
    assert!(
        protocol.unwrap(),
        "real primary control request must succeed"
    );
    assert!(
        progress.unwrap().is_ok() && control.is_ok(),
        "safe pane/control failed"
    );
    assert!(server_result.is_ok(), "{server_result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(safe_state, AgentTurnState::Completed);
    assert!(retained);
    assert_eq!(source.unwrap().action_results, original.action_results);
    assert_eq!(actual, rows);
}
