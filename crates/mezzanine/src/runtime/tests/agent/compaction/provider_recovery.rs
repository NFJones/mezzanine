//! Recovery over actual completed tasks, retained effects and one typed head.
//!
//! The previous selective fixtures replaced active source with flat blocks.
//! These cases instead use admitted tasks and original provider/action owners;
//! auxiliary results remain private until physical publication and adoption.

use super::*;

/// Owns only isolated fixture state and its physical native pane, if requested.
struct Recovery {
    service: RuntimeSessionService,
    store: AgentTranscriptStore,
    turn: String,
}

impl Drop for Recovery {
    fn drop(&mut self) {
        let _ = self.service.terminate_all_pane_processes();
    }
}

fn config(limit: usize, retention: u8, native: bool) -> String {
    let mode = if native { "native" } else { "pane" };
    format!(
        r#"[agents]
default_provider="runtime-batch"
default_model_profile="main"
shell_mode="{mode}"
compaction_raw_retention_percent={retention}
[permissions]
sandbox="policy-only"
[providers.runtime-batch]
kind="openai"
models=["test"]
default_model="test"
[model_profiles.main]
provider="runtime-batch"
model="test"
context_window_tokens={limit}
max_input_tokens={limit}
[model_profiles.small]
provider="runtime-batch"
model="test"
context_window_tokens=40000
max_input_tokens=30000
"#
    )
}

impl Recovery {
    fn new(config: String, history: &str, native: bool) -> Self {
        let mut service = test_runtime_service();
        let store = AgentTranscriptStore::new(temp_root("canonical-provider-recovery"));
        service.set_agent_transcript_store(store.clone());
        service
            .replace_config_layers(vec![ConfigLayer {
                name: "canonical-provider-recovery".into(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: config,
            }])
            .unwrap();
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        if native {
            service.start_initial_pane_process(None).unwrap();
            wait_until_primary_shell_foreground(&mut service, "%1");
            service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);
            service.permission_policy_mut().set_approval_bypass(true);
        }
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let turn = crate::test_support::compaction_source::complete_task_and_admit_successor(
            &mut service,
            &primary,
            history,
            "the original task completed",
            "continue collecting evidence",
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
        );
        Self {
            service,
            store,
            turn,
        }
    }

    fn conversation(&self) -> String {
        self.service
            .agent_turn_ledger()
            .turn(&self.turn)
            .unwrap()
            .conversation_id
            .clone()
    }

    fn next(&mut self, prompt: &str) {
        let primary = self
            .service
            .session
            .layout_owner_client_id()
            .unwrap()
            .clone();
        let started = self
            .service
            .execute_agent_shell_command(&primary, prompt)
            .unwrap();
        assert!(started.contains(r#""state":"running""#), "{started}");
        self.turn = self
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id
            .clone()
            .unwrap();
    }

    fn finish(&mut self, answer: &str) {
        crate::test_support::compaction_source::complete_task(
            &mut self.service,
            &self.turn,
            answer,
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
        );
    }

    fn reject(&mut self, attempt: u32) {
        let before = self.service.agent_turn_contexts()[&self.turn].clone();
        let error = MezError::invalid_state("provider context length exceeded")
            .with_provider_failure_json(
                r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
            );
        assert!(
            self.service
                .recover_agent_provider_context_limit_failure(
                    &AgentId::opaque("agent-%1").unwrap(),
                    &self.turn,
                    &error,
                    attempt,
                )
                .unwrap()
        );
        assert_eq!(self.service.agent_turn_contexts()[&self.turn], before);
        assert!(!self.service.agent_provider_task_is_pending(&self.turn));
        assert!(
            self.service
                .pending_agent_compaction_task_for_tests("%1")
                .unwrap()
                .replay_source
                .is_some()
        );
    }

    fn input(&self) -> String {
        crate::test_support::compaction_source::rendered_source(
            self.service
                .pending_agent_compaction_task_for_tests("%1")
                .unwrap(),
        )
        .unwrap()
    }

    fn publish(&mut self, summary: &str) {
        let before = self.service.agent_turn_contexts()[&self.turn].clone();
        let mut waves = 0;
        while self
            .service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some()
        {
            waves += 1;
            assert!(waves <= 32, "fixture model work must terminate");
            complete_runtime_test_compaction(&mut self.service, "%1", summary);
            assert_eq!(self.service.agent_turn_contexts()[&self.turn], before);
            assert!(!self.service.agent_provider_task_is_pending(&self.turn));
        }
        replay_observed::adopt(&mut self.service, &self.turn);
        replay_observed::assert_head(&self.service, &self.store, &self.turn, summary);
    }
}

/// A real earlier head cannot hide later completed work. One new head consumes
/// that head exactly once and the later source, preserving the current root.
#[test]
fn runtime_context_limit_recovery_skips_earlier_summary_only_segment() {
    let mut fixture = Recovery::new(
        config(40_000, 1, false),
        &format!("FIRST_COVERED_SOURCE {}", "earlier ".repeat(5_000),),
        false,
    );
    fixture.reject(1);
    fixture.publish("Earlier decision summarized.");
    let (_, first) = fixture
        .store
        .load_replay_epoch(&fixture.conversation(), mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    let original_head = first.head().unwrap().occurrence_id();
    fixture.finish(&format!(
        "LATER_COMPLETED_DECISION {}",
        "later decision ".repeat(800)
    ));
    fixture.next("preserve this instruction exactly");
    let before = fixture.service.agent_turn_contexts()[&fixture.turn].clone();
    let exact = before.chronology().last().unwrap().clone();
    assert_eq!(exact.retention(), mez_agent::ContextRetention::Exact);
    let audit = fixture.store.inspect(&fixture.conversation()).unwrap();
    fixture.reject(1);
    assert_eq!(
        fixture
            .input()
            .matches("Earlier decision summarized.")
            .count(),
        1
    );
    assert!(fixture.input().contains("LATER_COMPLETED_DECISION"));
    assert!(!fixture.input().contains("FIRST_COVERED_SOURCE"));
    assert!(
        !fixture
            .input()
            .contains("preserve this instruction exactly")
    );
    fixture.publish("Earlier and later decisions summarized together.");
    let body = &fixture.service.agent_turn_contexts()[&fixture.turn];
    assert!(body.chronology().contains(&exact));
    assert!(
        !body
            .chronology()
            .iter()
            .any(|event| event.record().occurrence_id() == original_head)
    );
    assert_eq!(
        fixture.store.inspect(&fixture.conversation()).unwrap(),
        audit
    );
}

/// A provider-owned text observation has few words but exceeds the next model's
/// complete byte/token allowance. Select it through actual canonical costs;
/// finite auxiliary rejection/chunking must finish before one head is adopted.
#[test]
fn runtime_provider_compaction_continues_after_word_budget_underestimates_retained_history() {
    let mut fixture = Recovery::new(config(100_000, 1, false), &"first ".repeat(4_000), false);
    let mut response =
        runtime_say_response(&fixture.turn, "byte-heavy observation collected", true);
    response.provider_transcript_events.push(
        mez_agent::ProviderTranscriptEvent::validated_openai_response_output(vec![
            serde_json::json!({"type":"message", "role":"assistant", "content":[
                {"type":"output_text", "text":format!("BYTE_HEAVY_SOURCE {}", "x".repeat(100_000))}
            ]}),
        ])
        .unwrap(),
    );
    assert!(
        fixture
            .service
            .remove_pending_agent_provider_task(&fixture.turn)
    );
    let profile = fixture
        .service
        .agent_turn_model_profile(&fixture.turn)
        .unwrap()
        .clone();
    assert_eq!(
        fixture
            .service
            .execute_agent_turn_with_provider(
                &fixture.turn,
                &RuntimeBatchProvider { response },
                profile,
            )
            .unwrap()
            .terminal_state,
        AgentTurnState::Completed
    );
    // Change the configured selection while idle, before admitting the actual
    // recovery task. No in-flight compactor/profile is rewritten to force fit.
    let primary = fixture
        .service
        .session
        .layout_owner_client_id()
        .unwrap()
        .clone();
    fixture
        .service
        .execute_agent_shell_command(&primary, "/model small")
        .unwrap();
    fixture.next("preserve exact instruction");
    assert_eq!(
        fixture
            .service
            .agent_turn_model_profile(&fixture.turn)
            .unwrap()
            .max_input_tokens(),
        Some(30_000)
    );
    let before = fixture.service.agent_turn_contexts()[&fixture.turn].clone();
    let words = before
        .blocks()
        .iter()
        .map(|block| mez_agent::model_context_text_word_count(&block.content))
        .sum::<usize>();
    assert!(words < 22_500);
    let byte_heavy = before
        .chronology()
        .iter()
        .find(|event| event.block().content.contains("BYTE_HEAVY_SOURCE"))
        .unwrap()
        .clone();
    fixture.reject(1);
    assert!(fixture.input().contains("BYTE_HEAVY_SOURCE"));
    let audit = fixture.store.inspect(&fixture.conversation()).unwrap();
    let mut requests = 0;
    let mut outputs = 0usize;
    let mut rejections = 0usize;
    let mut actual_cap = None;
    let final_summary = "all prior evidence summarized in one final head";
    while let Some(task) = fixture.service.take_pending_agent_compaction_task("%1") {
        let provider = fixture
            .service
            .provider_registry()
            .provider(&task.model_profile.provider)
            .unwrap();
        let api = mez_agent::resolve_provider_api(&provider.kind, provider.api.as_deref()).unwrap();
        let options = crate::runtime::config::runtime_effective_provider_options(
            provider,
            &task.model_profile,
        );
        fixture
            .service
            .claim_agent_compaction_task_state("%1", task.clone());
        let (result, _) = crate::test_support::compaction_source::execute_with_inference(
            &fixture.service,
            &task,
            |request| {
                // Equal text can belong to disjoint original byte intervals.
                // The engine's typed coverage, not body equality, proves work.
                requests += 1;
                assert!(requests <= 64);
                let measured =
                    mez_agent::provider_request_input_estimate(request, api, &options, false)
                        .unwrap()
                        .input_tokens;
                // The first actually packed request meets a lower real provider
                // ceiling. Subsequent input is checked against that same ceiling.
                let limit = *actual_cap.get_or_insert(measured - 1);
                if measured > limit {
                    rejections += 1;
                    Err(MezError::invalid_state("provider context length exceeded")
                        .with_provider_failure_json(
                            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
                        ))
                } else {
                    outputs += 1;
                    Ok(runtime_test_compaction_response(final_summary))
                }
            },
        );
        result
            .as_ref()
            .expect("common engine handles genuine input rejection privately");
        fixture
            .service
            .apply_agent_compaction_transition(
                crate::test_support::compaction_source::model_result_event(
                    "%1",
                    task.task_generation,
                    result,
                ),
            )
            .unwrap();
        assert_eq!(fixture.service.agent_turn_contexts()[&fixture.turn], before);
        assert!(fixture.service.pending_agent_provider_tasks().is_empty());
    }
    assert!(rejections > 0);
    assert!(outputs > 1);
    replay_observed::adopt(&mut fixture.service, &fixture.turn);
    replay_observed::assert_head(
        &fixture.service,
        &fixture.store,
        &fixture.turn,
        final_summary,
    );
    assert!(
        !fixture.service.agent_turn_contexts()[&fixture.turn]
            .chronology()
            .contains(&byte_heavy)
    );
    assert!(
        fixture.service.agent_turn_contexts()[&fixture.turn]
            .blocks()
            .iter()
            .any(|block| block.content == "preserve exact instruction")
    );
    assert_eq!(
        fixture.store.inspect(&fixture.conversation()).unwrap(),
        audit
    );
}

/// A retained completed native group includes the original assistant and its
/// results. Subsequent rejection consumes it with the prior head, never an
/// orphan action-result prefix or a regenerated/replayed action execution.
#[test]
fn runtime_provider_context_limit_error_recompacts_action_result_prefix() {
    std::thread::Builder::new()
        .name("native-prefix-recovery".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(Box::pin(action_result_prefix_recovery()));
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn action_result_prefix_recovery() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    // The one-request oracle covers ordinary continuation, not optional title
    // generation. Select its normal non-provider policy before task admission.
    let source_config = config(40_000, 10, true)
        .replace("shell_mode=\"native\"", "shell_mode=\"native\"\nsession_title_policy=\"first_prompt\"")
        .replace("kind=\"openai\"", &format!("kind=\"openai-compatible\"\nbase_url=\"{endpoint}\""))
        .replace("default_model=\"test\"", "default_model=\"test\"\n[providers.runtime-batch.options]\nmaap_output=\"structured_json\"\nstructured_output=\"json_schema\"");
    let mut fixture = Recovery::new(
        source_config,
        &format!("OLDER_PREFIX_SOURCE {}", "older ".repeat(5_000),),
        true,
    );
    let counter = temp_root("provider-native-prefix").join("executions");
    let command = format!(
        "printf x >> {}; printf 'provider-context-limit-tail-one- provider-context-limit-tail-two- provider-context-limit-tail-three-'",
        shlex::try_quote(counter.to_str().unwrap()).unwrap()
    );
    let mut response = runtime_say_response(&fixture.turn, "collect retained results", false);
    response.action_batch.as_mut().unwrap().actions = vec![mez_agent::AgentAction {
        id: "retained-native-results".into(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Collect retained results once".into(),
            command,
            interactive: false,
            stateful: false,
            timeout_ms: Some(5_000),
        },
    }];
    let original_turn = fixture.turn.clone();
    assert!(
        fixture
            .service
            .remove_pending_agent_provider_task(&fixture.turn)
    );
    let profile = fixture
        .service
        .agent_turn_model_profile(&fixture.turn)
        .unwrap()
        .clone();
    fixture
        .service
        .execute_agent_turn_with_provider(
            &fixture.turn,
            &RuntimeBatchProvider { response },
            profile,
        )
        .unwrap();
    let dispatch = fixture
        .service
        .claim_native_shell_action(&fixture.turn, "retained-native-results")
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .service
            .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(dispatch))
            .unwrap()
    );
    for _ in 0..4 {
        fixture.service.retry_completion_publications();
    }
    finish_native_through_actor(&mut fixture, listener, &counter).await;
    fixture.next("continue with the retained results");
    let result = fixture.service.agent_turn_contexts()[&fixture.turn]
        .chronology()
        .iter()
        .find(|event| {
            event.block().source == ContextSourceKind::ActionResult
                && event
                    .block()
                    .content
                    .contains("provider-context-limit-tail-one-")
        })
        .unwrap()
        .clone();
    let audit = fixture.store.inspect(&fixture.conversation()).unwrap();
    let mut attempt = 1;
    loop {
        let before = fixture.service.agent_turn_contexts()[&fixture.turn].clone();
        let owner = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new(fixture.conversation()).unwrap(),
            before.clone(),
        )
        .unwrap();
        let selected = match owner
            .prepare_compaction(mez_agent::ReplayTailPolicy::new(40_000, 10).unwrap())
            .unwrap()
        {
            mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
            other => panic!("retained result must still have real eligible source: {other:?}"),
        };
        let consumes_result = selected.summary_input().contains(&result);
        // Later epochs require actual completed task source; successive context
        // rejections cannot walk the same retained history down automatically.
        fixture.reject(1);
        if attempt == 1 {
            assert!(fixture.input().contains("OLDER_PREFIX_SOURCE"));
            assert!(!fixture.input().contains("provider-context-limit-tail-one-"));
        }
        if consumes_result {
            assert!(fixture.input().contains("provider-context-limit-tail-one-"));
            assert!(fixture.input().contains("provider-context-limit-tail-two-"));
            assert!(
                fixture
                    .input()
                    .contains("provider-context-limit-tail-three-")
            );
        }
        fixture.publish(&format!(
            "model-authored action-result prefix summary {attempt}"
        ));
        assert_eq!(std::fs::read_to_string(&counter).unwrap(), "x");
        assert!(
            fixture
                .service
                .claim_native_shell_action(&original_turn, "retained-native-results")
                .unwrap()
                .is_none()
        );
        let after = &fixture.service.agent_turn_contexts()[&fixture.turn];
        assert_eq!(after.chronology().contains(&result), !consumes_result);
        assert!(
            after.chronology().len() < before.chronology().len()
                || before
                    .chronology()
                    .iter()
                    .all(|event| event.block().source != ContextSourceKind::CompactionPrefix)
        );
        if consumes_result {
            break;
        }
        assert!(attempt < 8, "fresh-source recovery must terminate");
        fixture.finish("intervening native-source task completed");
        fixture.next("continue with newly eligible native evidence");
        attempt += 1;
    }
    let archive = fixture.store.inspect(&fixture.conversation()).unwrap();
    assert!(archive.starts_with(&audit));
    for original in &audit {
        assert_eq!(archive.iter().filter(|row| *row == original).count(), 1);
    }
}

/// Runs a real lease/provider continuation after native completion and then
/// reopens its saved source through the normal resume command. It fabricates
/// neither positive handoff admission nor an ordinary archive acknowledgment.
async fn finish_native_through_actor(
    fixture: &mut Recovery,
    listener: tokio::net::TcpListener,
    counter: &std::path::Path,
) {
    use crate::host::async_runtime::{
        AsyncAgentProviderServiceConfig, AsyncRuntimeActorConfig, AsyncRuntimeSessionActor,
        AsyncRuntimeSideEffectServiceConfig, run_async_agent_provider_service,
        run_async_persistence_side_effect_service, run_async_runtime_timer_side_effect_service,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let conversation = fixture.conversation();
    let mut service = std::mem::replace(&mut fixture.service, test_runtime_service());
    service.set_auth_store(AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&temp_root("native-prefix-auth")),
    ));
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(AtomicUsize::new(0));
    let server_stop = stop.clone();
    let request_count = requests.clone();
    let server = async move {
        while !server_stop.load(Ordering::SeqCst) {
            let accepted = tokio::time::timeout(Duration::from_millis(20), listener.accept()).await;
            let Ok(Ok((mut socket, _))) = accepted else {
                continue;
            };
            let mut bytes = Vec::new();
            let header_end = loop {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                bytes.push(byte[0]);
                assert!(bytes.len() < 65_536);
                if bytes.ends_with(b"\r\n\r\n") {
                    break bytes.len();
                }
            };
            let length: usize = String::from_utf8_lossy(&bytes)
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
            assert_eq!(std::fs::read_to_string(counter).unwrap(), "x");
            assert_eq!(request_count.fetch_add(1, Ordering::SeqCst), 0);
            let content =
                serde_json::json!({"rationale":"finish the collected native evidence", "actions":[
                    {"type":"say", "status":"final", "text":"the collected evidence is ready",
                     "content_type":mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE}
                ]})
                .to_string();
            mez_agent::parse_maap_action_batch_json(&content).unwrap();
            let (content_type, body) = if request["stream"] == true {
                let chunk = serde_json::json!({"id":"offline-native-prefix", "model":"test", "choices":[
                    {"index":0,"delta":{"role":"assistant","content":content},"finish_reason":null}
                ]});
                let end = serde_json::json!({"id":"offline-native-prefix", "model":"test", "choices":[
                    {"index":0,"delta":{},"finish_reason":"stop"}
                ]});
                (
                    "text/event-stream",
                    format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"),
                )
            } else {
                ("application/json", serde_json::json!({"id":"offline-native-prefix", "model":"test", "choices":[
                    {"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}
                ]}).to_string())
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
                .with_idle_interval(Duration::from_millis(5))
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
                idle_interval: Duration::from_millis(5),
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while handle.agent_turn_is_running(&fixture.turn).await.unwrap()
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        stop.store(true, Ordering::SeqCst);
    };
    let persistence_handle = handle.clone();
    let persistence_stop = stop.clone();
    let persistence = async move {
        run_async_persistence_side_effect_service(
            &persistence_handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: u64::MAX,
                drain_limit: 32,
                idle_interval: Duration::from_millis(5),
            },
            move |_, state| {
                persistence_stop.load(Ordering::SeqCst)
                    || matches!(state, crate::runtime::RuntimeLifecycleState::Stopping)
            },
        )
        .await
        .unwrap()
    };
    let lanes = async {
        let ((), report, (), _, persistence_report) =
            tokio::join!(client, worker, server, timer, persistence);
        assert!(persistence_report.completed > 0, "{persistence_report:?}");
        handle.shutdown().await.unwrap();
        report
    };
    let (report, mut exit) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(lanes, actor.run())
    })
    .await
    .expect("one real native continuation must settle");
    exit.service.terminate_all_pane_processes().unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(report.executions, 1);
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn(&fixture.turn)
            .unwrap()
            .state,
        AgentTurnState::Completed,
        "report={report:?}; queue={:?}; failures={:?}; execution={:?}; pane={:?}",
        exit.service
            .completion_provider_queue_flags_for_tests(&fixture.turn),
        exit.service
            .completion_required_failures_for_tests(&fixture.turn),
        exit.service
            .agent_turn_executions()
            .get(&fixture.turn)
            .map(|execution| (
                execution.terminal_state,
                execution.final_turn,
                execution
                    .action_results
                    .iter()
                    .map(|result| result.status)
                    .collect::<Vec<_>>(),
            )),
        exit.service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .into_iter()
            .rev()
            .take(12)
            .collect::<Vec<_>>(),
    );
    assert_eq!(std::fs::read_to_string(counter).unwrap(), "x");
    assert!(
        fixture
            .store
            .inspect(&conversation)
            .unwrap()
            .iter()
            .any(|row| row.content.contains("the collected evidence is ready"))
    );
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(AgentTranscriptStore::new(fixture.store.root()));
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "reopened-native-prefix".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: config(40_000, 10, false),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let queued = service
        .execute_agent_shell_command(&primary, &format!("/resume {conversation}"))
        .unwrap();
    let resumed = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap_or(queued);
    assert!(resumed.contains("resumed=true"), "{resumed}");
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        conversation
    );
    fixture.service = service;
}

/// The frozen compactor and complete candidate use the smallest routable main
/// target before a decision exists. The router's own small window is excluded;
/// the original default-profile turn and exact root retain their identities.
#[test]
fn runtime_routing_context_limit_recovery_uses_minimum_target_window() {
    let routing = r#"[agents]
default_provider="runtime-batch"
default_model_profile="default"
shell_mode="pane"
compaction_raw_retention_percent=1
routing=true
[agents.auto_sizing]
router_model_profile="router"
small_model_profile="small"
medium_model_profile="medium"
large_model_profile="large"
allowed_reasoning_efforts=["low", "medium", "high", "xhigh"]
fallback_policy="use-default-profile"
[providers.runtime-batch]
kind="openai"
models=["test", "router", "small", "large"]
default_model="test"
[model_profiles.default]
provider="runtime-batch"
model="test"
context_window_tokens=100000
[model_profiles.router]
provider="runtime-batch"
model="router"
context_window_tokens=2000
[model_profiles.small]
provider="runtime-batch"
model="small"
context_window_tokens=40000
[model_profiles.medium]
provider="runtime-batch"
model="large"
context_window_tokens=100000
[model_profiles.large]
provider="runtime-batch"
model="large"
context_window_tokens=100000
"#;
    let mut fixture = Recovery::new(
        routing.into(),
        &format!(
            "routing-context-pressure- {}",
            "context-pressure ".repeat(12_000),
        ),
        false,
    );
    assert_eq!(
        fixture
            .service
            .agent_turn_model_profile(&fixture.turn)
            .unwrap()
            .context_window_tokens(),
        Some(100_000)
    );
    assert!(!fixture.service.agent_turn_routing_applied(&fixture.turn));
    fixture.reject(1);
    let task = fixture
        .service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert_eq!(task.model_profile.context_window_tokens(), Some(40_000));
    assert_eq!(task.model_profile.model, "small");
    assert!(fixture.input().contains("routing-context-pressure-"));
    fixture.publish("model-authored routing context summary");
    assert_eq!(
        fixture
            .service
            .agent_turn_model_profile(&fixture.turn)
            .unwrap()
            .context_window_tokens(),
        Some(100_000)
    );
    assert!(!fixture.service.agent_turn_routing_applied(&fixture.turn));
    assert!(
        fixture
            .service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == fixture.turn)
    );
}
