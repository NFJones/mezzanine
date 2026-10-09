//! Manual compaction through actor ingress, real provider worker and durable epoch.
//!
//! The loopback provider returns fixed plain summary text behind a response gate.
//! No external provider, billing or credentials are involved. The fixture never
//! supplies a synthetic completion event, so publication proves worker execution.
//! This qualifies queued/executing visibility, not early off-actor preparation.

use super::*;

mod transport_retries;

/// Under-budget eligible history still runs actual model compaction. Both direct
/// and attached input routes must remain visibly compacting while the provider
/// response is withheld, publish its summary, preserve the append-only archive,
/// and clear state without another keystroke or a synthetic ordinary turn.
#[tokio::test(flavor = "current_thread")]
async fn async_manual_compaction_executes_provider_and_publishes_durable_epoch() {
    for attached in [false, true] {
        Box::pin(qualify(attached, false)).await;
    }
}

/// Typed assistant/result history must survive the real source worker, context
/// capture and request worker before one actual provider dispatch. The selected
/// prefix appears once; retained evidence never leaks into compactor input, and
/// publication reopens valid owned tail history without changing the archive.
#[tokio::test(flavor = "current_thread")]
async fn async_manual_compaction_typed_replay_preserves_execution_ownership() {
    Box::pin(qualify(false, true)).await;
}

/// Directly owns server/provider/actor/client futures under an external fixture
/// deadline. Response release is explicit; no retry or artificial paint delay is
/// added to the product. Archive/summary assertions use independent store reads.
async fn qualify(attached: bool, typed: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let started = StdArc::new(tokio::sync::Notify::new());
    let release = StdArc::new(tokio::sync::Notify::new());
    let server_started = started.clone();
    let server_release = release.clone();
    let server = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = async_provider_concurrency_read_http_request(&mut stream).await;
        assert!(request.contains("eligible-source-1"));
        assert!(request.contains("local-chat-model"));
        if typed {
            assert_eq!(request.matches("typed-result-1").count(), 1, "{request}");
            assert!(!request.contains("typed-result-2"), "{request}");
            assert!(!request.contains("typed-result-3"), "{request}");
        }
        server_started.notify_one();
        server_release.notified().await;
        async_provider_concurrency_write_chat_content_response(
            &mut stream,
            "local-chat-model",
            "model-authored-compaction-marker",
        )
        .await;
    };
    let root =
        std::env::temp_dir().join(format!("mez-compact-exec-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    let store = AgentTranscriptStore::new(root.join("transcripts"));
    let conversation = "manual-compact-exec";
    for sequence in 1..=3 {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: conversation.into(),
                sequence: if typed {
                    (sequence - 1) * 3 + 1
                } else {
                    sequence
                },
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: format!(
                    "eligible-source-{sequence} {}",
                    "completed detail ".repeat(60)
                ),
            })
            .unwrap();
        if typed {
            let group =
                mez_agent::ContextExecutionGroupId::new(format!("typed-{sequence}")).unwrap();
            for (ordinal, source, content) in [
                (
                    1,
                    mez_agent::ContextSourceKind::TranscriptAssistant,
                    format!("typed-assistant-{sequence}"),
                ),
                (
                    2,
                    mez_agent::ContextSourceKind::ActionResult,
                    format!("typed-result-{sequence}"),
                ),
            ] {
                store
                    .append(&mez_agent::transcript::TranscriptEntry {
                        conversation_id: conversation.into(),
                        sequence: (sequence - 1) * 3 + ordinal + 1,
                        created_at_unix_seconds: sequence,
                        role: mez_agent::transcript::TranscriptRole::System,
                        turn_id: format!("turn-{sequence}"),
                        agent_id: "agent-%1".into(),
                        pane_id: "%1".into(),
                        content: mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                            source,
                            "typed replay",
                            content,
                            group.clone(),
                            ordinal,
                            None,
                        )
                        .unwrap()
                        .to_transcript_content(),
                    })
                    .unwrap();
            }
        }
    }
    let original = store.inspect(conversation).unwrap();
    let mut service = test_service();
    service.set_agent_transcript_store(store.clone());
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root),
    ));
    service.replace_config_layers(vec![ConfigLayer {
        name:"local-compaction".into(),path:None,format:ConfigFormat::Toml,scope:ConfigScope::Primary,trusted:true,
        text:format!("[agents]\ndefault_provider=\"local-chat\"\ndefault_model_profile=\"default\"\n[providers.local-chat]\nkind=\"openai-compatible\"\nbase_url=\"http://{address}/v1\"\nmodels=[\"local-chat-model\"]\ndefault_model=\"local-chat-model\"\n[model_profiles.default]\nprovider=\"local-chat\"\nmodel=\"local-chat-model\"\ncontext_window_tokens=128000\n"),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", conversation, if typed { 9 } else { 3 })
        .unwrap();
    let source_started = StdArc::new(tokio::sync::Notify::new());
    let source_release = StdArc::new(tokio::sync::Notify::new());
    let request_started = StdArc::new(tokio::sync::Notify::new());
    let request_release = StdArc::new(tokio::sync::Notify::new());
    if typed {
        service.set_manual_compaction_preparation_probe_for_tests(
            source_started.clone(),
            source_release.clone(),
            StdArc::new(tokio::sync::Notify::new()),
        );
        service.set_manual_compaction_request_probe_for_tests(
            request_started.clone(),
            request_release.clone(),
            StdArc::new(tokio::sync::Notify::new()),
        );
    }
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
        if attached {
            let result = handle
                .apply_attached_terminal_step_plan(
                    primary.clone(),
                    AttachedTerminalClientStepPlan {
                        actions: vec![TerminalClientLoopAction::ForwardToPane(
                            b"/compact\r".to_vec(),
                        )],
                        output_lines: vec![],
                        output_line_style_spans: vec![],
                        input_hangup: false,
                        output_hangup: false,
                        error_roles: vec![],
                    },
                )
                .await
                .unwrap();
            assert_eq!(result.agent_prompt_inputs_applied, 1);
        } else {
            let response = handle
                .execute_agent_shell_command(primary.clone(), "/compact".into())
                .await
                .unwrap();
            assert!(response.contains("state=preparing"));
        }
        if typed {
            for (phase_started, phase_release) in [
                (&source_started, &source_release),
                (&request_started, &request_release),
            ] {
                tokio::time::timeout(Duration::from_secs(5), phase_started.notified())
                    .await
                    .unwrap();
                assert!(store.compaction_epoch(conversation).unwrap().is_none());
                let config = handle
                    .terminal_client_loop_config(TerminalClientLoopConfig::default())
                    .await
                    .unwrap();
                assert_eq!(
                    config
                        .frame_context
                        .panes
                        .get("%1")
                        .and_then(|pane| pane.agent_status.as_deref()),
                    Some("compacting")
                );
                assert!(
                    !handle
                        .drain_agent_provider_dispatch_side_effects(16)
                        .await
                        .unwrap()
                        .iter()
                        .any(|effect| matches!(
                            effect,
                            RuntimeSideEffect::DispatchAgentCompaction { .. }
                        ))
                );
                phase_release.notify_one();
            }
        }
        tokio::time::timeout(Duration::from_secs(5), started.notified())
            .await
            .unwrap();
        assert!(store.compaction_epoch(conversation).unwrap().is_none());
        let view = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            view.lines.iter().any(|line| line.contains("compacting")),
            "real queued/executing compactor must be visible"
        );
        let config = handle
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .await
            .unwrap();
        assert_eq!(
            config
                .frame_context
                .panes
                .get("%1")
                .and_then(|pane| pane.agent_status.as_deref()),
            Some("compacting")
        );
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let view = handle
                    .render_client_view(
                        ClientViewRole::Primary,
                        Size::new(80, 24).unwrap(),
                        TerminalClientLoopConfig::default(),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                assert!(
                    !view
                        .lines
                        .iter()
                        .any(|line| line.contains("compact failed")),
                    "fixture compactor failure: {}",
                    view.lines.join("\n")
                );
                if store.compaction_epoch(conversation).unwrap().is_some() {
                    let config = handle
                        .terminal_client_loop_config(TerminalClientLoopConfig::default())
                        .await
                        .unwrap();
                    if config
                        .frame_context
                        .panes
                        .get("%1")
                        .and_then(|pane| pane.agent_status.as_deref())
                        != Some("compacting")
                    {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("model summary publication and state clearing must settle without more input");
        stop.store(true, Ordering::SeqCst);
        stopped.notified().await;
        handle.shutdown().await.unwrap();
    };
    let ((), report, (), mut exit) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(
            Box::pin(client),
            Box::pin(provider),
            Box::pin(server),
            actor.run()
        )
    })
    .await
    .unwrap();
    assert_eq!(report.executions, 1);
    assert!(!exit.service.agent_is_compacting("%1"));
    assert!(exit.service.agent_turn_ledger().turns().is_empty());
    let epoch = store.compaction_epoch(conversation).unwrap().unwrap();
    assert!(epoch.summary.contains("model-authored-compaction-marker"));
    assert_eq!(epoch.through_sequence, if typed { 3 } else { 1 });
    let archive = store.inspect(conversation).unwrap();
    assert_eq!(&archive[..original.len()], original.as_slice());
    let reopened = AgentTranscriptStore::new(root.join("transcripts"));
    assert_eq!(
        reopened
            .compaction_epoch(conversation)
            .unwrap()
            .unwrap()
            .summary,
        epoch.summary
    );
    let context = exit
        .service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(format!("{context:?}").contains("model-authored-compaction-marker"));
    context.validate_durable().unwrap();
    if typed {
        assert!(
            !context
                .blocks()
                .iter()
                .any(|block| block.content == "typed-result-1")
        );
        for sequence in [2, 3] {
            let group =
                mez_agent::ContextExecutionGroupId::new(format!("typed-{sequence}")).unwrap();
            assert_eq!(
                context
                    .chronology()
                    .iter()
                    .filter(|event| event.execution_group_id() == Some(&group))
                    .count(),
                2
            );
        }
    }
    exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
