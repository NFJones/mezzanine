//! Completed-output correction through production actor leases and local HTTP workers.

use super::*;

/// A complete invalid response must admit a fresh auxiliary claim, never an
/// ordinary retry. The second HTTP response is gated to prove Running/deferred
/// ownership and no publication; valid output then produces one epoch and one
/// ordinary continuation. All requests are offline loopback traffic.
#[tokio::test(flavor = "current_thread")]
async fn async_compaction_invalid_completed_output_repairs_before_ordinary_dispatch() {
    for kind in ["budget", "empty", "bytes"] {
        Box::pin(qualify_completed_output(kind)).await;
    }
}

/// Owns fake server, real worker and actor futures with explicit response gates.
async fn qualify_completed_output(kind: &str) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let root = std::env::temp_dir().join(format!(
        "mez-summary-repair-{:032x}",
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
    for sequence in 1..=3 {
        let group =
            mez_agent::ContextExecutionGroupId::new(format!("summary-source-{sequence}")).unwrap();
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: conversation.clone(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::System,
                turn_id: format!("old-{sequence}"),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                    mez_agent::ContextSourceKind::TranscriptAssistant,
                    "closed evidence",
                    format!("FROZEN_SOURCE_{sequence} {}", "evidence ".repeat(1000)),
                    group,
                    1,
                    None,
                )
                .unwrap()
                .to_transcript_content(),
            })
            .unwrap();
    }
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", &conversation, 3)
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "continue after observed compaction")
        .unwrap();
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    let plan = mez_agent::plan_model_context_compaction_for_provider_tokens(
        context,
        20000,
        10,
        context.event_sequence_high_water_mark(),
        mez_agent::ProviderBudgetProjection::new(
            mez_agent::ProviderApiCompatibility::OpenAiChatCompletions,
            "local-chat",
        ),
    )
    .unwrap();
    let budget = plan.summary_budget_words();
    assert!(
        plan.changes_context(),
        "fixture must contain closed history: {context:#?}"
    );
    let profile = service.agent_turn_model_profile("turn-1").unwrap().clone();
    assert!(
        service
            .queue_agent_active_turn_compaction(
                "turn-1",
                "default".into(),
                profile,
                crate::runtime::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                    observed_input_tokens: 20000,
                    max_input_tokens: 20000,
                },
                plan
            )
            .unwrap()
    );
    let invalid = match kind {
        "budget" => "INVALID_SUMMARY ".repeat(budget + 1),
        "empty" => " \n\t".into(),
        _ => "界".repeat(128 * 1024 / 3 + 1),
    };
    let started = StdArc::new(tokio::sync::Notify::new());
    let release = StdArc::new(tokio::sync::Notify::new());
    let generation = service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    let server = async {
        let mut requests = Vec::new();
        for step in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            requests.push(async_provider_concurrency_read_http_request(&mut stream).await);
            match step {
                0 => {
                    async_provider_concurrency_write_chat_content_response(
                        &mut stream,
                        "local-chat-model",
                        &invalid,
                    )
                    .await
                }
                1 => {
                    assert!(requests[1].contains("FROZEN_SOURCE_1"));
                    assert!(!requests[1].contains("INVALID_SUMMARY"));
                    started.notify_one();
                    release.notified().await;
                    async_provider_concurrency_write_chat_content_response(
                        &mut stream,
                        "local-chat-model",
                        "VALID_REPAIRED_SUMMARY",
                    )
                    .await;
                }
                _ => {
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
        started.notified().await;
        assert!(handle.agent_turn_is_running("turn-1").await.unwrap());
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty()
        );
        assert!(store.compaction_epoch(&conversation).unwrap().is_none());
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
            if view
                .lines
                .iter()
                .any(|line| line.contains("ordinary repaired continuation completed"))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        stop.store(true, Ordering::SeqCst);
        stopped.notified().await;
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
    .unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(report.executions, 3);
    let epoch = store.compaction_epoch(&conversation).unwrap().unwrap();
    assert_eq!(epoch.ranges.len(), 1);
    assert_eq!(epoch.ranges[0].summary, "VALID_REPAIRED_SUMMARY");
    assert!(!exit.service.agent_is_compacting("%1"));
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn("turn-1")
            .unwrap()
            .state,
        mez_agent::AgentTurnState::Completed
    );
    exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
