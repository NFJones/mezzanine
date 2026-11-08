//! Completed-output correction through production actor leases and local HTTP workers.

use super::*;

/// A complete invalid response must admit a fresh auxiliary claim, never an
/// ordinary retry. The second HTTP response is gated to prove Running/deferred
/// ownership and no publication; valid output then produces one epoch and one
/// ordinary continuation. All requests are offline loopback traffic.
#[tokio::test(flavor = "current_thread")]
async fn async_compaction_invalid_completed_output_repairs_before_ordinary_dispatch() {
    for kind in ["empty", "malformed"] {
        Box::pin(qualify_completed_output(kind)).await;
    }
}

/// Exceeding the advisory word target alone is valid. The actual complete
/// provider request still fits; publication must precede one ordinary request
/// without another auxiliary model call or a size-repair generation.
#[tokio::test(flavor = "current_thread")]
async fn async_compaction_word_target_is_advisory_before_ordinary_dispatch() {
    Box::pin(qualify_completed_output("budget")).await;
}

/// A genuine decoded-summary resource overflow is terminal after one request;
/// it cannot start a model-driven shortening loop or publish a partial epoch.
#[tokio::test(flavor = "current_thread")]
async fn async_compaction_summary_byte_overflow_fails_once() {
    Box::pin(qualify_completed_output("bytes")).await;
}

/// Owns fake server, real worker and actor futures with explicit response gates.
async fn qualify_completed_output(kind: &str) {
    let advisory = kind == "budget";
    let resource = kind == "bytes";
    let exhausted = resource || kind.ends_with("-exhaust");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let root = std::env::temp_dir().join(format!(
        "mez-summary-repair-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    let store = AgentTranscriptStore::new(root.join("transcripts"));
    let mut service = test_service();
    service.configure_provider_retry_policy(mez_agent::ProviderRetryPolicy {
        max_attempts: 0,
        unlimited: true,
        initial_delay_ms: 20,
        max_delay_ms: 40,
    });
    service.set_agent_transcript_store(store.clone());
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root),
    ));
    service.replace_config_layers(vec![ConfigLayer {
        name: "summary-correction".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: format!("[agents]\ndefault_provider=\"local-chat\"\ndefault_model_profile=\"default\"\nsession_title_policy=\"objective\"\n[providers.local-chat]\nkind=\"openai-compatible\"\nbase_url=\"http://{address}/v1\"\nmodels=[\"local-chat-model\"]\ndefault_model=\"local-chat-model\"\n[providers.local-chat.options]\nmaap_output=\"structured_json\"\nstructured_output=\"json_schema\"\n[model_profiles.default]\nprovider=\"local-chat\"\nmodel=\"local-chat-model\"\ncontext_window_tokens=40000\nmax_input_tokens=20000\n"),
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
    let turn = crate::test_support::compaction_source::complete_task_and_admit_successor(
        &mut service,
        &primary,
        &format!("FROZEN_SOURCE_1 {}", "evidence ".repeat(3_000)),
        "original source task completed",
        "continue after observed compaction",
        mez_agent::ProviderApiCompatibility::OpenAiChatCompletions,
    );
    let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    assert!(
        service
            .queue_agent_active_turn_compaction(
                &turn,
                "default".into(),
                profile,
                crate::runtime::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                    observed_input_tokens: 20000,
                    max_input_tokens: 20000,
                },
            )
            .unwrap()
    );
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert!(task.replay_source.is_some());
    let budget = 100;
    let invalid = match kind {
        "budget" => "word ".repeat(budget + 1),
        "empty" | "empty-exhaust" => " \n\t".into(),
        "malformed" | "malformed-exhaust" => "```mezzanine-action-json\n{}\n```".into(),
        _ => "界".repeat(128 * 1024 / 3 + 1),
    };
    let started = StdArc::new(tokio::sync::Notify::new());
    let release = StdArc::new(tokio::sync::Notify::new());
    let generation = service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    let phase = std::sync::atomic::AtomicUsize::new(0);
    let server = async {
        let mut requests = Vec::new();
        for step in 0..(if resource {
            1
        } else if advisory {
            2
        } else {
            3
        }) {
            let (mut stream, _) = listener.accept().await.unwrap();
            requests.push(async_provider_concurrency_read_http_request(&mut stream).await);
            phase.store(10 + step, Ordering::SeqCst);
            match step {
                0 => {
                    if advisory {
                        assert!(requests[0].contains("Any token or word target is advisory"));
                        assert!(invalid.split_whitespace().count() > budget);
                    }
                    if resource {
                        started.notify_one();
                        release.notified().await;
                    }
                    async_provider_concurrency_write_chat_content_response(
                        &mut stream,
                        "local-chat-model",
                        &invalid,
                    )
                    .await
                }
                1 => {
                    if advisory {
                        assert!(requests[1].contains(invalid.trim()));
                        assert!(!requests[1].contains("VALID_REPAIRED_SUMMARY"));
                        started.notify_one();
                        release.notified().await;
                        async_provider_concurrency_write_chat_response(
                            &mut stream,
                            "ordinary repaired continuation completed",
                        )
                        .await;
                        continue;
                    }
                    assert!(requests[1].contains("FROZEN_SOURCE_1"));
                    assert!(!requests[1].contains("INVALID_SUMMARY"));
                    started.notify_one();
                    release.notified().await;
                    async_provider_concurrency_write_chat_content_response(
                        &mut stream,
                        "local-chat-model",
                        if exhausted {
                            &invalid
                        } else {
                            "VALID_REPAIRED_SUMMARY"
                        },
                    )
                    .await;
                }
                _ => {
                    if exhausted {
                        assert!(requests[2].contains("FROZEN_SOURCE_1"));
                        async_provider_concurrency_write_chat_content_response(
                            &mut stream,
                            "local-chat-model",
                            &invalid,
                        )
                        .await;
                        continue;
                    }
                    assert!(requests[2].contains("VALID_REPAIRED_SUMMARY"));
                    assert!(!requests[2].contains("INVALID_SUMMARY"));
                    async_provider_concurrency_write_chat_response(
                        &mut stream,
                        "ordinary repaired continuation completed",
                    )
                    .await;
                }
            }
        }
        requests
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
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::DispatchAgentCompaction {
                pane_id: "%1".into(),
                task_generation: generation,
            }])
            .await
            .unwrap();
        // Drive the real publication owner while waiting. A valid first reply
        // must publish before its ordinary HTTP request can reach the gate.
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
                    run_async_persistence_side_effect_service(
                        &handle,
                        AsyncRuntimeSideEffectServiceConfig {
                            max_polls: 4, drain_limit: 64,
                            idle_interval: Duration::from_millis(1),
                        }, |_, _| false,
                    ).await.unwrap();
                }
            }
        }
        assert!(handle.agent_turn_is_running(&turn).await.unwrap());
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty()
        );
        let published = store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap();
        if advisory {
            let (_, owner) =
                published.expect("valid first summary must publish before ordinary dispatch");
            assert_eq!(
                owner.context().chronology()[0].block().content,
                invalid.trim()
            );
        } else {
            assert!(published.is_none());
        }
        release.notify_one();
        loop {
            // Adapter-level completed-output failures can use supervised
            // auxiliary backoff. Drive its actual timer, never a fake reply.
            for effect in handle.drain_timer_side_effects(32).await.unwrap() {
                if let RuntimeSideEffect::ScheduleTimer { key, .. } = effect
                    && key.kind == RuntimeTimerKind::CompactionRetry
                {
                    let mut events = RuntimeEventBatch::new();
                    events.push(RuntimeEvent::Timer(TimerEvent { key, now_ms: 1 }));
                    assert_eq!(
                        handle.submit_runtime_events(events).await.unwrap().applied,
                        1
                    );
                }
            }
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
                line.contains(if exhausted {
                    "compact failed"
                } else {
                    "ordinary repaired continuation completed"
                })
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
        stop.store(true, Ordering::SeqCst);
        stopped.notified().await;
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
        handle.shutdown().await.unwrap();
    };
    let ((), report, requests, mut exit) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(
            Box::pin(client),
            Box::pin(provider),
            Box::pin(server),
            actor.run()
        )
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "summary correction deadline: kind={kind} phase={}",
            phase.load(Ordering::SeqCst)
        )
    });
    assert_eq!(
        requests.len(),
        if resource {
            1
        } else if advisory {
            2
        } else {
            3
        }
    );
    // A wave returning private repair is a completed worker outcome. The final
    // exhausted/resource failure is not, independently of HTTP response count.
    assert_eq!(
        report.executions,
        if resource {
            0
        } else if advisory {
            2
        } else if exhausted {
            2
        } else {
            3
        }
    );
    if exhausted {
        assert!(
            store
                .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
                .unwrap()
                .is_none()
        );
        assert!(
            !requests
                .iter()
                .any(|request| request.contains("VALID_REPAIRED_SUMMARY"))
        );
    } else {
        let (_, cold) = AgentTranscriptStore::new(store.root())
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(
            cold.context().chronology()[0].block().content,
            if advisory {
                invalid.trim()
            } else {
                "VALID_REPAIRED_SUMMARY"
            }
        );
    }
    assert!(!exit.service.agent_is_compacting("%1"));
    assert_eq!(
        exit.service.agent_turn_ledger().turn(&turn).unwrap().state,
        if exhausted {
            mez_agent::AgentTurnState::Failed
        } else {
            mez_agent::AgentTurnState::Completed
        }
    );
    exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Completed HTTP200 blank and malformed replies each admit exactly two
/// corrections under unlimited transport policy, then truthfully fail without
/// an ordinary retry or published epoch. Three HTTP attempts are independently
/// inventoried by the fake server and production actor lease owner.
#[tokio::test(flavor = "current_thread")]
async fn async_compaction_empty_and_malformed_corrections_are_finite() {
    for kind in ["empty-exhaust", "malformed-exhaust"] {
        Box::pin(qualify_completed_output(kind)).await;
    }
}
