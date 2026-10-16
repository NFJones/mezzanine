//! Fixed-allowance convergence through real actor leases and canonical OpenAI
//! Responses HTTP construction. All traffic and synthetic auth are local to
//! this fixture; no user configuration, credentials or live provider is used.

use super::*;
use tokio::io::AsyncWriteExt;

/// Actual provider boundary interrupted before completed-output validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryPath {
    /// Completed output alone requires correction.
    Direct,
    /// Incomplete HTTP body, followed by the exact frozen request.
    Transport,
    /// Typed provider generation cutoff, handled by auxiliary helper once.
    OutputLimit,
    /// Compactor context rejection requiring smaller private source input.
    ContextSplit,
}

/// An ordinary context rejection must freeze auxiliary work, reject oversized
/// completed output, accept a summary above halved guidance but within ORIGINAL
/// authority, pass full fit and dispatch exactly one ordinary continuation.
/// Every captured canonical OpenAI body omits max_output_tokens, with and
/// without an internal allocation and in streaming and nonstreaming modes.
#[tokio::test(flavor = "current_thread")]
async fn async_openai_compaction_converges_against_original_allowance() {
    for stream in [false, true] {
        for allocation in [false, true] {
            Box::pin(qualify(false, stream, allocation, RetryPath::Direct)).await;
        }
    }
}

/// Manual preparation freezes words rather than borrowing provider output
/// tokens. The same correction/wire owner must publish one complete epoch
/// without dispatching an ordinary turn, in both stream/allocation modes.
#[tokio::test(flavor = "current_thread")]
async fn async_openai_manual_compaction_keeps_frozen_word_allowance() {
    for stream in [false, true] {
        for allocation in [false, true] {
            Box::pin(qualify(true, stream, allocation, RetryPath::Direct)).await;
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
            Box::pin(qualify(false, streaming, false, retry)).await;
            Box::pin(qualify(true, streaming, false, retry)).await;
        }
    }
}

/// Owns the actor, worker, HTTP endpoint and response gate under one deadline.
/// Publication and archived source are observed independently of worker output.
async fn qualify(manual: bool, streaming: bool, allocation: bool, retry: RetryPath) {
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
    // Automatic rejection must reach the wire before compaction, and the
    // repaired word summary must also be smaller in BYTES than selected source.
    // This fixture models a large, sparse-word source under a real larger
    // configured input window; it never changes limits during recovery.
    let input = if manual { 20000 } else { 100000 };
    let window = if manual { 40000 } else { 200000 };
    service.replace_config_layers(vec![ConfigLayer {
        name: "canonical-convergence".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: format!("[agents]\ndefault_provider=\"openai\"\ndefault_model_profile=\"default\"\nsession_title_policy=\"objective\"\n[providers.openai]\nkind=\"openai\"\nbase_url=\"http://{address}/v1/responses\"\nmodels=[\"gpt-test\"]\ndefault_model=\"gpt-test\"\n[model_profiles.default]\nprovider=\"openai\"\nmodel=\"gpt-test\"\ncontext_window_tokens={window}\nmax_input_tokens={input}\n{cap}"),
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
    for sequence in 1..=3 {
        // One immutable execution group disables whole-group walkback for the
        // split fixture, so a real rejection must traverse chunks/synthesis.
        let group = mez_agent::ContextExecutionGroupId::new(if retry == RetryPath::ContextSplit {
            "canonical-source-single-group".to_string()
        } else {
            format!("canonical-source-{sequence}")
        })
        .unwrap();
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: conversation.clone(),
                sequence,
                created_at_unix_seconds: sequence,
                role: if manual {
                    mez_agent::transcript::TranscriptRole::Assistant
                } else {
                    mez_agent::transcript::TranscriptRole::System
                },
                turn_id: format!("old-{sequence}"),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: if manual {
                    format!(
                        "CANONICAL_FROZEN_SOURCE_{sequence} {}",
                        "evidence ".repeat(1000)
                    )
                } else {
                    mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                        mez_agent::ContextSourceKind::TranscriptAssistant,
                        "closed evidence",
                        format!(
                            "CANONICAL_FROZEN_SOURCE_{sequence} {}",
                            "evidence ".repeat(8000)
                        ),
                        group,
                        sequence,
                        None,
                    )
                    .unwrap()
                    .to_transcript_content()
                },
            })
            .unwrap();
    }
    let archived = store.inspect(&conversation).unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", &conversation, 3)
        .unwrap();
    if !manual {
        service
            .execute_agent_shell_command(&primary, "continue with frozen evidence")
            .unwrap();
    }
    let initial_effect = if manual {
        None
    } else {
        Some(RuntimeSideEffect::DispatchAgentProvider {
            agent_id: AgentId::opaque("agent-%1").unwrap(),
            turn_id: "turn-1".into(),
        })
    };
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
        assert!(body.to_string().contains("CANONICAL_FROZEN_SOURCE_1"));
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
                        .contains("[ephemeral compaction output-limit retry]")
                );
            }
            body = next;
        }
        let text = body.to_string();
        let target = text.split_once("[compaction summary target]").unwrap().1;
        let budget: usize = target
            .split_once("within ")
            .unwrap()
            .1
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(budget > 8);
        let token_mode = target
            .chars()
            .take(200)
            .collect::<String>()
            .contains("estimated input tokens (approximately");
        let invalid = if token_mode {
            "abcd".repeat(budget + 1)
        } else {
            "INVALID ".repeat(budget + 1)
        };
        requests.push(body);
        respond(&mut socket, &invalid, streaming).await;
        let corrected = if !token_mode {
            // One-byte words keep the independent byte/full-request fit small;
            // the word cost still exceeds the halved advisory target.
            "v ".repeat(budget / 2 + 1).trim().to_string()
        } else {
            "abcd".repeat(budget / 2 + 1)
        };
        mez_agent::context_compaction::validate_model_compaction_summary(
            &corrected, budget, token_mode,
        )
        .unwrap();
        let corrected_cost = if !token_mode {
            mez_agent::model_context_text_word_count(&corrected)
        } else {
            mez_agent::provider_text_input_token_estimate(&corrected)
        };
        assert!(corrected_cost > budget / 2);
        let (mut socket, _) = listener.accept().await.unwrap();
        let body = read_body(&mut socket, streaming).await;
        assert!(body.to_string().contains("[private summary correction]"));
        assert!(!body.to_string().contains("INVALID INVALID"));
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
                    assert!(!next.contains("INVALID INVALID"));
                    requests.push(body);
                    final_summary = "CANONICAL_ACCEPTED_SYNTHESIS".into();
                    respond(&mut socket, &final_summary, streaming).await;
                    continue;
                }
                let next_target = next
                    .split_once("[compaction summary target]")
                    .and_then(|(_, value)| value.split_once("within "))
                    .and_then(|(_, value)| value.split_whitespace().next())
                    .and_then(|value| value.parse::<usize>().ok());
                assert!(
                    next.contains(&final_summary),
                    "next request: compactor={} correction={} synthesis={} budget={budget} token_mode={token_mode} corrected_cost={corrected_cost} next_target={next_target:?}",
                    next.contains("conversation compactor"),
                    next.contains("[private summary correction]"),
                    next.contains("Chunk 1 summary")
                );
                phase.store(4, Ordering::SeqCst);
                requests.push(body);
                respond(
                    &mut socket,
                    "ordinary canonical continuation completed",
                    streaming,
                )
                .await;
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
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty()
        );
        release.notify_one();
        loop {
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
                    "compacted conversation summary"
                } else {
                    "ordinary canonical continuation completed"
                })
            }) {
                break;
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
        (if manual { 2 } else { 4 }) + usize::from(retry != RetryPath::Direct) + extra_summaries
    );
    assert_eq!(
        report.executions,
        (if manual { 2 } else { 3 }) + extra_summaries as u64
    );
    if retry == RetryPath::ContextSplit {
        assert!(
            extra_summaries >= 2,
            "later chunk and synthesis must execute: manual={manual} streaming={streaming} extra={extra_summaries}"
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
    assert_eq!(input, report.executions * 100 + cutoff * 10);
    assert_eq!(output, report.executions * 10 + cutoff);
    let epoch = store.compaction_epoch(&conversation).unwrap().unwrap();
    if manual {
        assert!(epoch.summary.contains(&corrected));
    } else {
        assert_eq!(epoch.ranges.len(), 1);
        assert_eq!(epoch.ranges[0].summary, corrected);
    }
    assert_eq!(
        &store.inspect(&conversation).unwrap()[..archived.len()],
        archived.as_slice()
    );
    if !manual {
        assert_eq!(
            exit.service
                .agent_turn_ledger()
                .turn("turn-1")
                .unwrap()
                .state,
            mez_agent::AgentTurnState::Completed
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
