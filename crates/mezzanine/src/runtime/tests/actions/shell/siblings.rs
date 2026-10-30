//! Completion-driven handoff of already admitted native shell siblings.
//!
//! These fixtures use ordinary provider admission and actual generated local
//! workers. They never manually redispatch before checking the next native
//! claim. A held later discovery header must not block execution progress, while
//! whole-group publication remains a separate provider-continuation obligation.

use super::*;

/// Position of an immediate discovery action relative to the two native shells.
#[derive(Clone, Copy, Debug)]
enum Discovery {
    /// Control with no later immediate action publication.
    Absent,
    /// Control whose discovery output precedes native execution.
    Leading,
    /// Regression: discovery output is held behind unfinished shell B.
    Trailing,
}

/// Admits the same real native pair under one logging/order case.
/// The caller owns the returned service and explicitly retires its private root.
fn admit_siblings(
    discovery: Discovery,
    level: AgentLogLevel,
    marker: Option<&std::path::Path>,
) -> (RuntimeSessionService, mez_core::ids::ClientId) {
    admit_batch(discovery, level, marker, None)
}

/// Admits the ordinary pair or a patch predecessor through the same provider path.
fn admit_batch(
    discovery: Discovery,
    level: AgentLogLevel,
    marker: Option<&std::path::Path>,
    patch: Option<&str>,
) -> (RuntimeSessionService, mez_core::ids::ClientId) {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 200)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", level)
        .unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    service.permission_policy_mut().set_approval_bypass(true);
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"native-pair","method":"agent/shell/command","params":{"idempotency_key":"native-pair","input":"run the native pair"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let mut actions = ["a", "b"]
        .into_iter()
        .map(|name| mez_agent::AgentAction {
            id: format!("shell-{name}"),
            payload: mez_agent::AgentActionPayload::ShellCommand {
                summary: format!("Native sibling {name}"),
                command: match marker {
                    Some(path) => format!(
                        "printf '{name}\\n' >> {}; printf 'native-sibling-{name}\\n'",
                        shlex::try_quote(path.to_str().unwrap()).unwrap()
                    ),
                    None => format!("printf 'native-sibling-{name}\\n'"),
                },
                interactive: false,
                stateful: false,
                timeout_ms: None,
            },
        })
        .collect::<Vec<_>>();
    if let Some(patch) = patch {
        actions[0] = mez_agent::AgentAction {
            id: "patch-a".into(),
            payload: mez_agent::AgentActionPayload::ApplyPatch {
                patch: patch.into(),
                strip: None,
            },
        };
    }
    let listing = mez_agent::AgentAction {
        id: "discovery".into(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: None,
            scope: None,
        },
    };
    match discovery {
        Discovery::Absent => {}
        Discovery::Leading => actions.insert(0, listing),
        Discovery::Trailing => actions.push(listing),
    }
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "native pair".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "exercise automatic native sibling handoff".into(),
                actions,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.remove_pending_agent_provider_task("turn-1");
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    (service, primary)
}

/// Exercises automatic handoff with actual workers and no manual redispatch.
fn exercise_sibling_handoff(discovery: Discovery, level: AgentLogLevel) {
    let (mut service, _) = admit_siblings(discovery, level, None);
    let first = service
        .claim_native_shell_action("turn-1", "shell-a")
        .unwrap()
        .expect("first shell must be admitted");
    assert!(
        service
            .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(first))
            .unwrap()
    );
    // This is deliberately the first possible claim after A completion. No
    // explicit redispatch or unrelated event may make B eligible for the test.
    let second = service
        .claim_native_shell_action("turn-1", "shell-b")
        .unwrap();
    let Some(second) = second else {
        let cleanup = service.terminate_all_pane_processes();
        panic!(
            "completion did not automatically dispatch B: {discovery:?}/{level:?}; cleanup={cleanup:?}"
        );
    };
    assert!(
        service
            .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(second))
            .unwrap()
    );
    for _ in 0..4 {
        service.retry_completion_publications();
    }
    assert!(service.native_shell_progress_turn_ids().is_empty());
    assert!(!service.completion_publication_retry_needed_for_turn("turn-1"));
    service.terminate_all_pane_processes().unwrap();
}

/// Without discovery, completing A must automatically admit B in Normal mode.
/// This control distinguishes the later-publication cycle from all native pairs.
#[test]
fn runtime_native_siblings_without_discovery_advance_normal() {
    exercise_sibling_handoff(Discovery::Absent, AgentLogLevel::Normal);
}

/// The same no-discovery control must hold with Verbose result publication.
/// Logging detail cannot become a scheduler or require a manual redispatch.
#[test]
fn runtime_native_siblings_without_discovery_advance_verbose() {
    exercise_sibling_handoff(Discovery::Absent, AgentLogLevel::Verbose);
}

/// A leading discovery header is already eligible before native shell A runs.
/// Completing A must dispatch B through the normal completion owner in Normal.
#[test]
fn runtime_native_siblings_leading_discovery_advance_normal() {
    exercise_sibling_handoff(Discovery::Leading, AgentLogLevel::Normal);
}

/// Leading discovery remains a working control under Verbose result publication.
/// Both shells run through production completion without manual redispatch.
#[test]
fn runtime_native_siblings_leading_discovery_advance_verbose() {
    exercise_sibling_handoff(Discovery::Leading, AgentLogLevel::Verbose);
}

/// Later immediate discovery is correctly held behind unfinished native B.
/// That display barrier must not prevent A completion dispatching B in Normal.
#[test]
fn runtime_native_siblings_trailing_discovery_advance_normal() {
    exercise_sibling_handoff(Discovery::Trailing, AgentLogLevel::Normal);
}

/// The original circular dependency must also be absent in Verbose mode.
/// Later discovery may hold group acknowledgement while admitted B advances.
#[test]
fn runtime_native_siblings_trailing_discovery_advance_verbose() {
    exercise_sibling_handoff(Discovery::Trailing, AgentLogLevel::Verbose);
}

/// Duplicate and stale A deliveries cannot run B twice. Own-prefix installation
/// failure must retain dispatch custody until healthy publication retry, while a
/// later B/discovery install failure must keep ordinary continuation blocked.
/// File side effects prove each real command executes exactly once; the offline
/// continuation stays pending only after all group gates clear. The async actor
/// test below exercises the actual typed provider claim and worker execution.
#[test]
fn runtime_native_siblings_publication_retry_preserves_effects_and_continuation() {
    for hold_first in [false, true] {
        let root = std::env::temp_dir().join(format!(
            "mez-native-siblings-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        fs::create_dir(&root).unwrap();
        let marker = root.join("effects");
        let (mut service, _) =
            admit_siblings(Discovery::Trailing, AgentLogLevel::Verbose, Some(&marker));
        let first = service
            .claim_native_shell_action("turn-1", "shell-a")
            .unwrap()
            .unwrap();
        let first_outcome = crate::runtime::execute_native_shell_dispatch(first);
        service.hold_agent_presentation_installs_for_tests(hold_first);
        assert!(
            service
                .complete_native_shell_action(first_outcome.clone())
                .unwrap()
        );
        assert!(!service.agent_provider_task_is_pending("turn-1"));
        if hold_first {
            assert!(
                service
                    .claim_native_shell_action("turn-1", "shell-b")
                    .unwrap()
                    .is_none()
            );
            assert_eq!(service.retry_completion_publications(), 0);
            service.hold_agent_presentation_installs_for_tests(false);
            assert!(service.retry_completion_publications() > 0);
        }
        assert_eq!(fs::read_to_string(&marker).unwrap(), "a\n");
        service
            .complete_native_shell_action(first_outcome.clone())
            .unwrap();
        let mut stale = first_outcome.clone();
        stale.marker.push_str("-obsolete");
        assert!(!service.complete_native_shell_action(stale).unwrap());
        let second = service
            .claim_native_shell_action("turn-1", "shell-b")
            .unwrap()
            .expect("B must have exactly one dispatch owner");
        assert!(
            service
                .claim_native_shell_action("turn-1", "shell-b")
                .unwrap()
                .is_none()
        );
        assert_eq!(service.retry_completion_publications(), 0);
        let second_outcome = crate::runtime::execute_native_shell_dispatch(second);
        service.hold_agent_presentation_installs_for_tests(true);
        assert!(
            service
                .complete_native_shell_action(second_outcome.clone())
                .unwrap()
        );
        assert!(!service.agent_provider_task_is_pending("turn-1"));
        assert_eq!(service.retry_completion_publications(), 0);
        service.hold_agent_presentation_installs_for_tests(false);
        for _ in 0..4 {
            service.retry_completion_publications();
        }
        assert!(service.agent_provider_task_is_pending("turn-1"));
        assert!(!service.completion_publication_retry_needed_for_turn("turn-1"));
        let pane = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        // Completed shell output is a transient preview at the pane suffix.
        // Compare the durable command rows rather than that live preview.
        let first_row = pane
            .find("native-sibling-a")
            .expect("A command remains installed");
        let second_row = pane
            .find("native-sibling-b")
            .expect("B command remains installed");
        let discovery_row = pane
            .find("list agents: type=primary scope=project")
            .expect("discovery header installs after B");
        assert!(
            first_row < second_row && second_row < discovery_row,
            "{pane}"
        );
        service.complete_native_shell_action(first_outcome).unwrap();
        service
            .complete_native_shell_action(second_outcome)
            .unwrap();
        for _ in 0..3 {
            assert_eq!(service.retry_completion_publications(), 0);
        }
        assert!(service.agent_provider_task_is_pending("turn-1"));
        assert_eq!(fs::read_to_string(&marker).unwrap(), "a\nb\n");
        service.terminate_all_pane_processes().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}

/// Human pause, Stop, conversation replacement and failed required audit must
/// continue to deny unissued B even after real A has executed. These controls
/// change authority before completion, then retry publication without another
/// execution event. A retained effect is never permission to launch a sibling.
#[test]
fn runtime_native_siblings_held_publication_respects_authority_and_required_gates() {
    for boundary in ["pause", "stop", "rebind", "required-audit"] {
        let root = std::env::temp_dir().join(format!(
            "mez-native-boundary-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        fs::create_dir(&root).unwrap();
        let (mut service, primary) =
            admit_siblings(Discovery::Trailing, AgentLogLevel::Verbose, None);
        let first = service
            .claim_native_shell_action("turn-1", "shell-a")
            .unwrap()
            .unwrap();
        let outcome = crate::runtime::execute_native_shell_dispatch(first);
        match boundary {
            "pause" => {
                let target = service
                    .capture_agent_lifecycle_target(&primary, "%1")
                    .unwrap();
                service
                    .pause_agent_lifecycle_target(&primary, &target)
                    .unwrap();
            }
            "stop" => {
                let result = service.dispatch_runtime_control_body(r#"{"jsonrpc":"2.0","id":"stop","method":"agent/shell/command","params":{"idempotency_key":"stop-native-pair","input":"/stop"}}"#, &primary);
                assert!(!result.contains("\"error\""), "{result}");
            }
            "rebind" => {
                service
                    .agent_shell_store_mut()
                    .restore_conversation_for_running_turn_with_lineage(
                        "%1",
                        "turn-1",
                        "replacement-native-pair",
                        mez_agent::AgentTranscriptPosition::uncompacted(0),
                        None,
                    )
                    .unwrap();
            }
            "required-audit" => {
                service
                    .replace_config_layers(vec![ConfigLayer {
                        name: "primary".into(),
                        path: None,
                        format: ConfigFormat::Toml,
                        scope: ConfigScope::Primary,
                        trusted: true,
                        text: r#"[hooks.post]
event = "post_shell_command"
program = "/missing/native-pair-post-hook"
"#
                        .into(),
                    }])
                    .unwrap();
                service.set_audit_log(crate::security::audit::AuditLog::new(
                    crate::security::audit::AuditConfig {
                        enabled: true,
                        path: root.clone(),
                        hash_chain: false,
                        required: true,
                    },
                ));
            }
            _ => unreachable!(),
        }
        let completion = service.complete_native_shell_action(outcome);
        if boundary == "required-audit" {
            assert!(completion.is_err(), "required completion audit must fail");
        }
        for _ in 0..3 {
            service.retry_completion_publications();
        }
        assert!(
            !matches!(
                service.claim_native_shell_action("turn-1", "shell-b"),
                Ok(Some(_))
            ),
            "{boundary}"
        );
        assert!(
            !service.agent_provider_task_is_pending("turn-1"),
            "{boundary}"
        );
        // Repair only the fixture sink for teardown; its recorded required
        // completion failure remains sticky and grants no action authority.
        service.set_audit_log(crate::security::audit::AuditLog::new(
            crate::security::audit::AuditConfig {
                enabled: false,
                path: root.join("audit.jsonl"),
                hash_chain: false,
                required: false,
            },
        ));
        service.terminate_all_pane_processes().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}

/// An actual async actor and native worker lane must run A then B without any
/// status query, redispatch, message, or second user event driving the handoff.
/// A generated loopback model endpoint receives exactly one ordinary final
/// continuation after both effects; it never contacts an external provider.
/// The combined debug actor/provider/timer chain follows the established native
/// worker fixture convention: use a dedicated 32 MiB stack on both platforms.
#[test]
fn runtime_native_siblings_actor_advances_without_external_event() {
    std::thread::Builder::new()
        .name("native-sibling-handoff".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run_native_siblings_actor());
        })
        .unwrap()
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload));
}

/// Runs the real actor, native/provider lane and timer recovery lane together.
/// See the owning test for automatic-handoff and stack-size invariants.
async fn run_native_siblings_actor() {
    use crate::host::async_runtime::{
        AsyncAgentProviderServiceConfig, AsyncRuntimeActorConfig, AsyncRuntimeSessionActor,
        AsyncRuntimeSideEffectServiceConfig, run_async_agent_provider_service,
        run_async_runtime_timer_side_effect_service,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = std::env::temp_dir().join(format!(
        "mez-native-actor-pair-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    fs::create_dir(&root).unwrap();
    let marker = root.join("effects");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let (mut service, _) =
        admit_siblings(Discovery::Trailing, AgentLogLevel::Verbose, Some(&marker));
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "offline-native-continuation".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                r#"[permissions]
sandbox = "policy-only"
[agents]
default_provider = "runtime-batch"
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
    service.permission_policy_mut().set_approval_bypass(true);
    let first = service
        .pending_native_shell_actions()
        .into_iter()
        .next()
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let requests = std::sync::Arc::new(AtomicUsize::new(0));
    let server_stop = stop.clone();
    let request_count = requests.clone();
    let server_marker = marker.clone();
    let server = async move {
        while !server_stop.load(Ordering::SeqCst) {
            let accepted =
                tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept()).await;
            let Ok(Ok((mut socket, _))) = accepted else {
                continue;
            };
            let mut bytes = Vec::new();
            let header_end = loop {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                bytes.push(byte[0]);
                assert!(bytes.len() < 65536);
                if bytes.ends_with(b"\r\n\r\n") {
                    break bytes.len();
                }
            };
            let headers = String::from_utf8_lossy(&bytes);
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse().unwrap())
                })
                .unwrap();
            assert!(length < 1_048_576);
            bytes.resize(header_end + length, 0);
            socket.read_exact(&mut bytes[header_end..]).await.unwrap();
            let request: serde_json::Value = serde_json::from_slice(&bytes[header_end..]).unwrap();
            assert_eq!(
                fs::read_to_string(&server_marker).unwrap(),
                "a\nb\n",
                "provider continued before both native effects"
            );
            assert_eq!(
                request_count.fetch_add(1, Ordering::SeqCst),
                0,
                "duplicate ordinary continuation"
            );
            let content = serde_json::json!({"rationale":"finish the native pair","actions":[{"type":"say","status":"final","text":"native pair finished","content_type":mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE}]}).to_string();
            mez_agent::parse_maap_action_batch_json(&content).unwrap();
            let (content_type, body) = if request["stream"] == true {
                let chunk = serde_json::json!({"id":"offline-native-pair","model":"test","choices":[{"index":0,"delta":{"role":"assistant","content":content},"finish_reason":null}]}).to_string();
                let end = serde_json::json!({"id":"offline-native-pair","model":"test","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}).to_string();
                (
                    "text/event-stream",
                    format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"),
                )
            } else {
                ("application/json", serde_json::json!({"id":"offline-native-pair","model":"test","choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}]}).to_string())
            };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    };
    let worker_handle = handle.clone();
    let worker_stop = stop.clone();
    let worker = async move {
        run_async_agent_provider_service(
            &worker_handle,
            AsyncAgentProviderServiceConfig::new(1)
                .unwrap()
                .with_idle_interval(std::time::Duration::from_millis(5))
                .unwrap(),
            move |_, state| {
                worker_stop.load(Ordering::SeqCst)
                    || matches!(state, crate::runtime::RuntimeLifecycleState::Stopping)
            },
        )
        .await
        .unwrap()
    };
    let timer_handle = handle.clone();
    let timer_stop = stop.clone();
    let timer = async move {
        run_async_runtime_timer_side_effect_service(
            &timer_handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: u64::MAX,
                drain_limit: 32,
                idle_interval: std::time::Duration::from_millis(5),
            },
            0,
            move |_, state| {
                timer_stop.load(Ordering::SeqCst)
                    || matches!(state, crate::runtime::RuntimeLifecycleState::Stopping)
            },
        )
        .await
        .unwrap()
    };
    let client = async {
        // Initial admission occurred before the actor started. Queue only A's
        // original lane effect; all B and provider work must arise internally.
        assert_eq!(
            handle
                .queue_runtime_side_effects(vec![
                    crate::runtime::RuntimeSideEffect::DispatchNativeShellAction {
                        turn_id: first.0,
                        action_id: first.1
                    }
                ])
                .await
                .unwrap(),
            1
        );
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while requests.load(Ordering::SeqCst) == 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        while requests.load(Ordering::SeqCst) > 0
            && handle.agent_turn_is_running("turn-1").await.unwrap()
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        stop.store(true, Ordering::SeqCst);
    };
    // Retire the service lanes before actor shutdown so their final drain
    // requests retain a live reply owner, including on diagnostic failure.
    let lanes = async {
        let ((), report, (), timer_report) = tokio::join!(client, worker, server, timer);
        handle.shutdown().await.unwrap();
        (report, timer_report)
    };
    let ((report, timer_report), mut exit) =
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            tokio::join!(lanes, actor.run())
        })
        .await
        .expect("native pair and one offline continuation must settle automatically");
    exit.service.terminate_all_pane_processes().unwrap();
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "pending={:?}; report={report:?}; timer={timer_report:?}; turn={:?}; pane={:?}",
        exit.service.pending_agent_provider_tasks(),
        exit.service.agent_turn_ledger().turn("turn-1"),
        exit.service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
    );
    assert_eq!(report.executions, 1);
    assert_eq!(fs::read_to_string(marker).unwrap(), "a\nb\n");
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn("turn-1")
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    assert!(!exit.service.agent_turn_is_running("turn-1"));
    assert!(exit.service.native_shell_progress_turn_ids().is_empty());
    assert!(
        !exit
            .service
            .completion_publication_retry_needed_for_turn("turn-1")
    );
    exit.service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// A native patch's read/probe phase must keep its exact write owner instead of
/// releasing shell B early. After confirmed patch settlement, a later discovery
/// header must not block B's automatic handoff. Only generated target fixtures
/// are mutated; duplicate completion delivery never reexecutes the patch.
#[test]
fn runtime_native_siblings_patch_then_shell_and_discovery_advance() {
    let relative = format!(
        "target/mez-native-sibling-patch-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    );
    let root = std::env::current_dir().unwrap().join(&relative);
    fs::create_dir_all(&root).unwrap();
    let marker = root.join("effects");
    let patch = format!(
        "*** Begin Patch\n*** Add File: {relative}/patched.txt\n+patched-once\n*** End Patch"
    );
    let (mut service, _) = admit_batch(
        Discovery::Trailing,
        AgentLogLevel::Verbose,
        Some(&marker),
        Some(&patch),
    );
    let mut terminal = false;
    let mut last = None;
    for _ in 0..8 {
        let dispatch = service
            .claim_native_shell_action("turn-1", "patch-a")
            .unwrap()
            .expect("patch must retain its actual next-phase owner");
        let outcome = crate::runtime::execute_native_shell_dispatch(dispatch);
        assert!(
            service
                .complete_native_shell_action(outcome.clone())
                .unwrap()
        );
        last = Some(outcome);
        let execution = &service.agent_turn_executions()["turn-1"];
        let result = execution
            .action_results
            .iter()
            .find(|result| result.action_id == "patch-a")
            .unwrap();
        if result.is_terminal() {
            assert_eq!(result.status, ActionStatus::Succeeded);
            terminal = true;
            break;
        }
        assert!(
            service
                .claim_native_shell_action("turn-1", "shell-b")
                .unwrap()
                .is_none(),
            "patch phase must settle before B"
        );
    }
    assert!(
        terminal,
        "patch did not reach confirmed terminal settlement"
    );
    assert_eq!(
        fs::read_to_string(root.join("patched.txt")).unwrap(),
        "patched-once\n"
    );
    service.complete_native_shell_action(last.unwrap()).unwrap();
    assert!(
        service
            .claim_native_shell_action("turn-1", "patch-a")
            .unwrap()
            .is_none()
    );
    let second = service
        .claim_native_shell_action("turn-1", "shell-b")
        .unwrap()
        .expect("final patch completion must automatically dispatch B");
    assert!(
        service
            .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(second))
            .unwrap()
    );
    for _ in 0..4 {
        service.retry_completion_publications();
    }
    assert_eq!(fs::read_to_string(marker).unwrap(), "b\n");
    assert!(service.agent_provider_task_is_pending("turn-1"));
    assert!(service.native_shell_progress_turn_ids().is_empty());
    assert!(!service.completion_publication_retry_needed_for_turn("turn-1"));
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}
