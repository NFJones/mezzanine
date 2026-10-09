//! End-to-end auxiliary transport interruption qualification using loopback
//! HTTP, production provider workers, exact actor leases and retry timers.

use super::*;
use tokio::io::AsyncWriteExt;

/// A real ordinary context rejection followed by HTTP 200/chunked EOF or
/// partial SSE EOF must retry only the auxiliary request. Complete summary
/// validation then resumes the ordinary turn exactly once, with no action replay.
#[tokio::test(flavor = "current_thread")]
async fn async_compaction_transport_http_eof_retries_before_ordinary_continuation() {
    for sse in [false, true] {
        Box::pin(qualify_transport_retry(sse)).await;
    }
}

/// Drives timers explicitly and gates the successful compactor response so the
/// test can inspect authoritative source and waiting-turn state during recovery.
async fn qualify_transport_retry(sse: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let summary_started = StdArc::new(tokio::sync::Notify::new());
    let summary_release = StdArc::new(tokio::sync::Notify::new());
    let server = async {
        let mut requests = Vec::new();
        for step in 0..4 {
            let (mut stream, _) = listener.accept().await.unwrap();
            requests.push(async_provider_concurrency_read_http_request(&mut stream).await);
            match step {
                0 => async_provider_concurrency_write_chat_error_response(
                    &mut stream, 400,
                    r#"{"error":{"message":"context length exceeded","code":"context_length_exceeded"}}"#,
                ).await,
                1 => {
                    let content_type = if sse { "text/event-stream" } else { "application/json" };
                    let fragment = if sse {
                        "data: {\"choices\":[{\"delta\":{\"content\":\"PROVISIONAL_NOT_A_SUMMARY\"},\"finish_reason\":null}]}\n\n"
                    } else { "{\"choices\":[{\"message\":{\"content\":\"PROVISIONAL_NOT_A_SUMMARY" };
                    let tail = if sse { "0\r\n\r\n" } else { "" };
                    stream.write_all(format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{fragment}\r\n{tail}", fragment.len()
                    ).as_bytes()).await.unwrap();
                    stream.shutdown().await.unwrap();
                }
                2 => {
                    let body = |request: &String| request.split_once("\r\n\r\n").unwrap().1.to_string();
                    assert!(body(&requests[1]) == body(&requests[2]), "auxiliary wire request must be frozen");
                    summary_started.notify_one();
                    summary_release.notified().await;
                    async_provider_concurrency_write_chat_content_response(
                        &mut stream, "local-chat-model", "COMPLETE_TRANSPORT_RETRY_SUMMARY",
                    ).await;
                }
                _ => {
                    assert!(requests[3].contains("COMPLETE_TRANSPORT_RETRY_SUMMARY"));
                    assert!(!requests[3].contains("PROVISIONAL_NOT_A_SUMMARY"));
                    async_provider_concurrency_write_chat_response(&mut stream, "ordinary resumed exactly once").await;
                }
            }
        }
        requests
    };
    let root = std::env::temp_dir().join(format!(
        "mez-compact-transport-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    let store = AgentTranscriptStore::new(root.join("transcripts"));
    let mut service = test_service();
    service.set_agent_transcript_store(store.clone());
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root),
    ));
    service.replace_config_layers(vec![ConfigLayer {
        name: "transport-compaction".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: format!("[agents]\ndefault_provider=\"local-chat\"\ndefault_model_profile=\"default\"\nsession_title_policy=\"objective\"\n[providers.local-chat]\nkind=\"openai-compatible\"\nbase_url=\"http://{address}/v1\"\nmodels=[\"local-chat-model\"]\ndefault_model=\"local-chat-model\"\n[providers.local-chat.options]\nmaap_output=\"structured_json\"\nstructured_output=\"json_schema\"\n[model_profiles.default]\nprovider=\"local-chat\"\nmodel=\"local-chat-model\"\ncontext_window_tokens=40000\n"),
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
    service
        .execute_agent_shell_command(&primary, "continue after oversized observations")
        .unwrap();
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    for label in ["older", "newer"] {
        let group = mez_agent::ContextExecutionGroupId::new(format!("transport-{label}")).unwrap();
        context
            .append_assistant_event("settled inspection", "already executed", group.clone())
            .unwrap();
        context
            .append_evidence_event(
                mez_agent::ContextSourceKind::ActionResult,
                label,
                "settled observation ".repeat(3000),
                group,
                None,
                true,
            )
            .unwrap();
    }
    let original = store.inspect(&conversation).unwrap_or_default();
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
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: AgentId::opaque("agent-%1").unwrap(),
                turn_id: "turn-1".into(),
            }])
            .await
            .unwrap();
        let retry_key = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                for effect in handle.drain_timer_side_effects(32).await.unwrap() {
                    if let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = effect
                        && key.kind == RuntimeTimerKind::CompactionRetry
                    {
                        assert!(delay_ms > 0);
                        return key;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("interrupted auxiliary request must arm a retry timer");
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty()
        );
        assert!(store.compaction_epoch(&conversation).unwrap().is_none());
        let mut timer = RuntimeEventBatch::new();
        timer.push(RuntimeEvent::Timer(TimerEvent {
            key: retry_key.clone(),
            now_ms: 1,
        }));
        assert_eq!(
            handle.submit_runtime_events(timer).await.unwrap().applied,
            1
        );
        let mut duplicate = RuntimeEventBatch::new();
        duplicate.push(RuntimeEvent::Timer(TimerEvent {
            key: retry_key,
            now_ms: 2,
        }));
        assert_eq!(
            handle
                .submit_runtime_events(duplicate)
                .await
                .unwrap()
                .applied,
            0
        );
        summary_started.notified().await;
        assert!(store.compaction_epoch(&conversation).unwrap().is_none());
        let archive = store.inspect(&conversation).unwrap_or_default();
        assert_eq!(&archive[..original.len()], original.as_slice());
        assert!(
            !archive
                .iter()
                .any(|row| row.content.contains("PROVISIONAL_NOT_A_SUMMARY"))
        );
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty()
        );
        summary_release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
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
                if view
                    .lines
                    .iter()
                    .any(|line| line.contains("ordinary resumed exactly once"))
                {
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
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("ordinary turn must resume after a complete compactor response");
        stop.store(true, Ordering::SeqCst);
        stopped.notified().await;
        handle.shutdown().await.unwrap();
    };
    let ((), report, requests, mut exit) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(
            Box::pin(client),
            Box::pin(provider),
            Box::pin(server),
            actor.run()
        )
    })
    .await
    .unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        report.executions, 2,
        "one accepted compactor and one ordinary completion"
    );
    assert!(!exit.service.agent_is_compacting("%1"));
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn("turn-1")
            .unwrap()
            .state,
        mez_agent::AgentTurnState::Completed
    );
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert!(exit.service.pending_agent_compaction_tasks().is_empty());
    exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
