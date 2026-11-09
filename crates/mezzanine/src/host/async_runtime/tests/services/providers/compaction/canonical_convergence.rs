//! Compaction convergence through real actor leases and canonical OpenAI
//! Responses HTTP construction. All traffic and synthetic auth are local to
//! this fixture; no user configuration, credentials or live provider is used.

use super::*;
use tokio::io::AsyncWriteExt;

/// Actual provider boundary interrupted before completed-output validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryPath {
    /// A usable completed summary needs no extra model work.
    Direct,
    /// Incomplete HTTP body, followed by the exact frozen request.
    Transport,
    /// Typed provider generation cutoff, handled by the common output repair.
    OutputLimit,
    /// Compactor context rejection requiring smaller private source input.
    ContextSplit,
}

/// An ordinary context rejection freezes one fitting auxiliary input. A usable
/// long final publishes once and admits one ordinary continuation without size
/// chasing or a prospective ordinary-request fit gate.
/// Every captured canonical OpenAI body omits max_output_tokens, with and
/// without an internal allocation and in streaming and nonstreaming modes.
#[tokio::test(flavor = "current_thread")]
async fn async_openai_compaction_accepts_usable_long_final_once() {
    for stream in [false, true] {
        for allocation in [false, true] {
            Box::pin(qualify(false, stream, allocation, RetryPath::Direct, false)).await;
        }
    }
}

/// Manual preparation freezes canonical source and profile controls. The
/// common worker publishes one usable long final in one inference,
/// without dispatching an ordinary turn, in both stream/allocation modes.
#[tokio::test(flavor = "current_thread")]
async fn async_openai_manual_compaction_accepts_usable_long_final_once() {
    for stream in [false, true] {
        for allocation in [false, true] {
            Box::pin(qualify(true, stream, allocation, RetryPath::Direct, false)).await;
        }
    }
}

/// Actual auxiliary retries preserve canonical omission. Transport retains
/// exact input, generation cutoffs add bounded guidance, and context rejection
/// splits private source before chunk/synthesis work, in both streaming routes.
#[tokio::test(flavor = "current_thread")]
async fn async_openai_compaction_retry_wires_omit_max_output_tokens() {
    for streaming in [false, true] {
        for retry in [
            RetryPath::Transport,
            RetryPath::OutputLimit,
            RetryPath::ContextSplit,
        ] {
            for allocation in [false, true] {
                Box::pin(qualify(false, streaming, allocation, retry, false)).await;
                Box::pin(qualify(true, streaming, allocation, retry, false)).await;
            }
        }
    }
}

/// A second authoritative rejection of the accepted head must terminalize as
/// context unavailable, with one auxiliary call and no source/retention walkdown.
#[tokio::test(flavor = "current_thread")]
async fn async_openai_compaction_unchanged_head_rejection_never_reenters_model_work() {
    for streaming in [false, true] {
        Box::pin(qualify(false, streaming, true, RetryPath::Direct, true)).await;
    }
}

/// Owns the actor, worker, HTTP endpoint and response gate under one deadline.
/// Publication and archived source are observed independently of worker output.
async fn qualify(
    manual: bool,
    streaming: bool,
    allocation: bool,
    retry: RetryPath,
    reject_continuation: bool,
) {
    assert!(!reject_continuation || !manual);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let root = std::env::temp_dir().join(format!(
        "mez-openai-convergence-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    let store = AgentTranscriptStore::new(root.join("transcripts"));
    let auth = crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root),
    );
    if streaming {
        // Existing ChatGPT route selects streaming. Synthetic non-expiring
        // credentials are fixture-owned and never leave the loopback endpoint.
        auth.login_openai_provider_credential(
            "default",
            crate::security::auth::OpenAiProviderCredential {
                api_key: "synthetic-loopback-only".into(),
                refresh_token: None,
                account_id: Some("synthetic-account".into()),
                organization_id: None,
                token_expires_at: None,
            },
            &auth.file_credential_store("openai").unwrap(),
        )
        .unwrap();
    }
    let mut service = test_service();
    service.set_agent_transcript_store(store.clone());
    service.set_auth_store(auth);
    service.configure_provider_retry_policy(mez_agent::ProviderRetryPolicy {
        max_attempts: 3,
        unlimited: false,
        initial_delay_ms: 20,
        max_delay_ms: 40,
    });
    let cap = if allocation {
        "max_output_tokens=12000\n"
    } else {
        ""
    };
    // Ordinary input reaches the wire before compaction. The repeated-rejection
    // case leaves genuine closed source in the raw tail, so only the new fence
    // can prevent another compactor; no limit changes during recovery.
    let input = if manual { 20000 } else { 100000 };
    let window = if manual { 40000 } else { 200000 };
    let retention = if reject_continuation { 100 } else { 10 };
    service.replace_config_layers(vec![ConfigLayer {
        name: "canonical-convergence".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: format!("[agents]\ndefault_provider=\"openai\"\ndefault_model_profile=\"default\"\nsession_title_policy=\"objective\"\ncompaction_raw_retention_percent={retention}\n[providers.openai]\nkind=\"openai\"\nbase_url=\"http://{address}/v1/responses\"\nmodels=[\"gpt-test\"]\ndefault_model=\"gpt-test\"\n[model_profiles.default]\nprovider=\"openai\"\nmodel=\"gpt-test\"\ncontext_window_tokens={window}\nmax_input_tokens={input}\n{cap}"),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let mut exact_manual_root = None;
    let turn = if manual {
        let original = (1..=3)
            .map(|sequence| {
                format!(
                    "CANONICAL_FROZEN_SOURCE_{sequence} {}",
                    "evidence ".repeat(1000),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let current = crate::test_support::compaction_source::complete_task_and_admit_successor(
            &mut service,
            &primary,
            &original,
            "original manual source task completed",
            "preserve this exact manual task",
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
        );
        exact_manual_root = Some(
            service.agent_turn_contexts()[&current]
                .chronology()
                .iter()
                .rev()
                .find(|event| event.block().source == mez_agent::ContextSourceKind::UserInstruction)
                .unwrap()
                .clone(),
        );
        crate::test_support::compaction_source::complete_task(
            &mut service,
            &current,
            "current manual task completed",
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
        );
        None
    } else {
        // One original instruction is an indivisible canonical source unit.
        // A provider context rejection must use private chunks and synthesis,
        // without inventing execution closure for old unaccounted wire rows.
        let original = (1..=3)
            .map(|sequence| {
                format!(
                    "CANONICAL_FROZEN_SOURCE_{sequence} {}",
                    "evidence ".repeat(8000),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(
            crate::test_support::compaction_source::complete_task_and_admit_successor(
                &mut service,
                &primary,
                &original,
                "original source task completed",
                "continue with frozen evidence",
                mez_agent::ProviderApiCompatibility::OpenAiResponses,
            ),
        )
    };
    let archived = store.inspect(&conversation).unwrap();
    let initial_effect = turn
        .as_ref()
        .map(|turn| RuntimeSideEffect::DispatchAgentProvider {
            agent_id: AgentId::opaque("agent-%1").unwrap(),
            turn_id: turn.clone(),
        });
    let started = StdArc::new(tokio::sync::Notify::new());
    let release = StdArc::new(tokio::sync::Notify::new());
    let published = StdArc::new(tokio::sync::Notify::new());
    let phase = std::sync::atomic::AtomicUsize::new(0);
    let server = async {
        let mut requests = Vec::new();
        if !manual {
            let (mut socket, _) = listener.accept().await.unwrap();
            requests.push(read_body(&mut socket, streaming).await);
            phase.store(1, Ordering::SeqCst);
            async_provider_concurrency_write_chat_error_response(&mut socket, 400,
                r#"{"error":{"message":"context length exceeded","code":"context_length_exceeded"}}"#).await;
        }
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut body = read_body(&mut socket, streaming).await;
        if !reject_continuation {
            assert!(body.to_string().contains("CANONICAL_FROZEN_SOURCE_1"));
        }
        assert!(!body.to_string().contains("continue with frozen evidence"));
        phase.store(2, Ordering::SeqCst);
        if retry != RetryPath::Direct {
            requests.push(body.clone());
            interrupt(&mut socket, retry, streaming).await;
            drop(socket);
            (socket, _) = listener.accept().await.unwrap();
            let next = read_body(&mut socket, streaming).await;
            if retry == RetryPath::Transport {
                assert_eq!(body, next, "transport retries preserve exact frozen body");
            } else if retry == RetryPath::ContextSplit {
                assert!(next.to_string().len() < body.to_string().len());
            } else {
                assert!(
                    next.to_string()
                        .contains("[private epoch compaction output repair]")
                );
            }
            body = next;
        }
        // A fitting source with usable output needs one auxiliary call. Long
        // output is accepted without word targets or prospective fit correction.
        let corrected = if retry == RetryPath::Direct {
            format!("{}END", "v ".repeat(50_000))
        } else {
            "CANONICAL_ACCEPTED_PRIVATE".to_owned()
        };
        phase.store(3, Ordering::SeqCst);
        requests.push(body);
        started.notify_one();
        release.notified().await;
        respond(&mut socket, &corrected, streaming).await;
        let mut final_summary = corrected.clone();
        let mut extra_summaries = 0;
        if !manual || retry == RetryPath::ContextSplit {
            loop {
                let (mut socket, _) = if manual {
                    tokio::select! {
                        _ = published.notified() => break,
                        connection = listener.accept() => connection.unwrap(),
                    }
                } else {
                    listener.accept().await.unwrap()
                };
                let body = read_body(&mut socket, streaming).await;
                let next = body.to_string();
                if retry == RetryPath::ContextSplit && next.contains("conversation compactor") {
                    extra_summaries += 1;
                    assert!(extra_summaries <= 8, "finite frozen chunk/synthesis work");
                    requests.push(body);
                    final_summary = "CANONICAL_ACCEPTED_SYNTHESIS".into();
                    respond(&mut socket, &final_summary, streaming).await;
                    continue;
                }
                assert!(
                    next.contains(&final_summary),
                    "ordinary request must use the accepted head"
                );
                phase.store(4, Ordering::SeqCst);
                requests.push(body);
                if reject_continuation {
                    let (_, mut current) = store
                        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
                        .unwrap()
                        .unwrap();
                    current
                        .select_provider_projection(
                            mez_agent::ReplayProviderProjection::new(
                                mez_agent::ProviderApiCompatibility::OpenAiResponses,
                                "openai",
                                "gpt-test",
                            )
                            .unwrap(),
                        )
                        .unwrap();
                    let policy = mez_agent::ReplayTailPolicy::new(input, retention).unwrap();
                    assert!(
                        matches!(
                            current.prepare_compaction(policy).unwrap(),
                            mez_agent::ReplayCompactionPreparation::Ready(_)
                        ),
                        "old retained source is selectable; reentry must be blocked by identity"
                    );
                    assert!(!current.automatic_compaction_is_fresh(policy).unwrap());
                    async_provider_concurrency_write_chat_error_response(&mut socket, 400,
                        r#"{"error":{"message":"context length exceeded","code":"context_length_exceeded"}}"#).await;
                } else {
                    respond(
                        &mut socket,
                        "ordinary canonical continuation completed",
                        streaming,
                    )
                    .await;
                }
                break;
            }
        }
        (requests, final_summary, extra_summaries)
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let stop = StdArc::new(AtomicBool::new(false));
    let stopped = StdArc::new(tokio::sync::Notify::new());
    let provider = async {
        let report = run_async_agent_provider_service(
            &handle,
            AsyncAgentProviderServiceConfig::new(1)
                .unwrap()
                .with_idle_interval(Duration::from_millis(5))
                .unwrap(),
            |_, state| {
                stop.load(Ordering::SeqCst) || matches!(state, RuntimeLifecycleState::Stopping)
            },
        )
        .await
        .unwrap();
        stopped.notify_one();
        report
    };
    let client = async {
        if let Some(effect) = initial_effect {
            handle
                .queue_runtime_side_effects(vec![effect])
                .await
                .unwrap();
        } else {
            let outcome = handle
                .execute_agent_shell_command(primary.clone(), "/compact".into())
                .await
                .unwrap();
            assert!(
                outcome.contains("state=preparing"),
                "manual preparation must start"
            );
        }
        loop {
            tokio::select! {
                _ = started.notified() => break,
                _ = tokio::time::sleep(Duration::from_millis(5)) => {
                    for effect in handle.drain_timer_side_effects(32).await.unwrap() {
                        if let RuntimeSideEffect::ScheduleTimer { key, .. } = effect
                            && key.kind == RuntimeTimerKind::CompactionRetry {
                            let mut events = RuntimeEventBatch::new();
                            events.push(RuntimeEvent::Timer(TimerEvent { key, now_ms: 1 }));
                            assert_eq!(handle.submit_runtime_events(events).await.unwrap().applied, 1);
                        }
                    }
                    let view = handle.render_client_view(ClientViewRole::Primary, Size::new(100, 30).unwrap(),
                        TerminalClientLoopConfig::default()).await.unwrap().unwrap();
                    if let Some(cause) = view.lines.iter().find(|line| line.contains("agent: failure:") || line.contains("compact failed")) {
                        panic!("fixture admission failed before summary gate: {cause}");
                    }
                }
            }
        }
        assert!(store.compaction_epoch(&conversation).unwrap().is_none());
        assert!(
            store
                .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
                .unwrap()
                .is_none()
        );
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty()
        );
        release.notify_one();
        loop {
            run_async_persistence_side_effect_service(
                &handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: 4,
                    drain_limit: 64,
                    idle_interval: Duration::from_millis(1),
                },
                |_, _| false,
            )
            .await
            .unwrap();
            let view = handle
                .render_client_view(
                    ClientViewRole::Primary,
                    Size::new(100, 30).unwrap(),
                    TerminalClientLoopConfig::default(),
                )
                .await
                .unwrap()
                .unwrap();
            if view.lines.iter().any(|line| {
                line.contains(if manual {
                    "canonical compaction head accepted"
                } else if reject_continuation {
                    "context unavailable"
                } else {
                    "ordinary canonical continuation completed"
                })
            }) {
                if !reject_continuation
                    || !handle
                        .agent_turn_is_running(turn.as_deref().unwrap())
                        .await
                        .unwrap()
                {
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
        published.notify_one();
        stop.store(true, Ordering::SeqCst);
        stopped.notified().await;
        run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 8,
                drain_limit: 64,
                idle_interval: Duration::from_millis(1),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        handle.shutdown().await.unwrap();
    };
    let ((), report, (requests, corrected, extra_summaries), mut exit) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(Box::pin(client), Box::pin(provider), Box::pin(server), actor.run())
    }).await.unwrap_or_else(|_| panic!("canonical fixture deadline: manual={manual} streaming={streaming} allocation={allocation} phase={}", phase.load(Ordering::SeqCst)));
    assert_eq!(
        requests.len(),
        (if manual { 1 } else { 3 }) + usize::from(retry != RetryPath::Direct) + extra_summaries
    );
    assert_eq!(
        report.executions,
        (if manual || reject_continuation { 1 } else { 2 })
            + extra_summaries as u64
            + u64::from(matches!(
                retry,
                RetryPath::OutputLimit | RetryPath::ContextSplit
            ))
    );
    if retry == RetryPath::ContextSplit {
        assert!(
            extra_summaries >= 1,
            "rejected source must complete its remaining private work: manual={manual} streaming={streaming} extra={extra_summaries}"
        );
    }
    assert!(!exit.service.agent_is_compacting("%1"));
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert!(exit.service.pending_agent_compaction_tasks().is_empty());
    let usage = exit
        .service
        .agent_token_usage_for_conversation(&conversation);
    let input: u64 = usage.values().map(|value| value.input_tokens).sum();
    let output: u64 = usage.values().map(|value| value.output_tokens).sum();
    let cutoff = u64::from(retry == RetryPath::OutputLimit);
    let complete_responses =
        (if manual || reject_continuation { 1 } else { 2 }) + extra_summaries as u64;
    assert_eq!(input, complete_responses * 100 + cutoff * 10);
    assert_eq!(output, complete_responses * 10 + cutoff);
    let (_, cold) = AgentTranscriptStore::new(store.root())
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(cold.context().chronology()[0].block().content, corrected);
    if reject_continuation {
        assert_eq!(cold.revision().get(), 1);
    }
    if let Some(exact) = exact_manual_root {
        assert_eq!(
            cold.context()
                .chronology()
                .iter()
                .find(|event| event.record().occurrence_id() == exact.record().occurrence_id()),
            Some(&exact)
        );
    }
    assert_eq!(
        &store.inspect(&conversation).unwrap()[..archived.len()],
        archived.as_slice()
    );
    if !manual {
        assert_eq!(
            exit.service
                .agent_turn_ledger()
                .turn(turn.as_deref().unwrap())
                .unwrap()
                .state,
            if reject_continuation {
                mez_agent::AgentTurnState::Failed
            } else {
                mez_agent::AgentTurnState::Completed
            }
        );
    }
    exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Captures and checks the actual canonical HTTP body, not ModelRequest fields.
async fn read_body(socket: &mut tokio::net::TcpStream, streaming: bool) -> serde_json::Value {
    let request = async_provider_concurrency_read_http_request(socket).await;
    let body: serde_json::Value =
        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert!(body.get("max_output_tokens").is_none());
    assert_eq!(body["stream"], streaming);
    body
}

/// Emits real incomplete/cutoff/context-limit wire responses. The production
/// retry owner, not incomplete response content, decides subsequent work.
async fn interrupt(socket: &mut tokio::net::TcpStream, retry: RetryPath, streaming: bool) {
    match retry {
        RetryPath::ContextSplit => async_provider_concurrency_write_chat_error_response(
            socket,
            400,
            r#"{"error":{"message":"context length exceeded","code":"context_length_exceeded"}}"#,
        )
        .await,
        RetryPath::Transport => {
            let fragment = "{\"status\":\"completed\",\"output\":[";
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{fragment}", fragment.len() + 100).as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        }
        RetryPath::OutputLimit => {
            let response = serde_json::json!({"id":"fixture-incomplete", "model":"gpt-test", "status":"incomplete",
                "incomplete_details":{"reason":"max_output_tokens"}, "output":[], "usage":{"input_tokens":10,"output_tokens":1}});
            let (content_type, body) = if streaming {
                (
                    "text/event-stream",
                    format!(
                        "event: response.incomplete\ndata: {}\n\n",
                        serde_json::json!({"type":"response.incomplete", "response":response})
                    ),
                )
            } else {
                ("application/json", response.to_string())
            };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        }
        RetryPath::Direct => unreachable!("direct path injects no failure"),
    }
}

/// Supplies complete framed MAAP through real Responses JSON/SSE parsing.
async fn respond(socket: &mut tokio::net::TcpStream, text: &str, streaming: bool) {
    let batch = serde_json::json!({"rationale":"complete fixture output", "actions":[{
        "type":"say", "status":"final", "content_type":"text/plain; charset=utf-8", "text":text,
    }]})
    .to_string();
    let response = serde_json::json!({"id":"fixture-response", "model":"gpt-test", "status":"completed",
        "output":[{"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":batch}]}],
        "usage":{"input_tokens":100,"output_tokens":10}});
    let (content_type, body) = if streaming {
        (
            "text/event-stream",
            format!(
                "event: response.output_item.done\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
                serde_json::json!({"type":"response.output_item.done", "item":response["output"][0]}),
                serde_json::json!({"type":"response.completed", "response":response})
            ),
        )
    } else {
        ("application/json", response.to_string())
    };
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    socket.shutdown().await.unwrap();
}
