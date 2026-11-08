//! Manual compaction through actor ingress, real provider worker and durable epoch.
//!
//! The loopback provider returns fixed plain summary text behind a response gate.
//! No external provider, billing or credentials are involved. The fixture never
//! supplies a synthetic completion event, so publication proves worker execution.
//! This qualifies queued/executing visibility, not early off-actor preparation.

use super::*;

mod canonical_convergence;
mod summary_correction;
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
pub(super) async fn qualify(attached: bool, typed: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let started = StdArc::new(tokio::sync::Notify::new());
    let release = StdArc::new(tokio::sync::Notify::new());
    let server_started = started.clone();
    let server_release = release.clone();
    let root =
        std::env::temp_dir().join(format!("mez-compact-exec-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    let store = AgentTranscriptStore::new(root.join("transcripts"));
    let conversation = "manual-compact-exec";
    let mut service = test_service();
    service.set_agent_transcript_store(store.clone());
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root),
    ));
    service.replace_config_layers(vec![ConfigLayer {
        name:"local-compaction".into(),path:None,format:ConfigFormat::Toml,scope:ConfigScope::Primary,trusted:true,
        text:format!("[agents]\ndefault_provider=\"local-chat\"\ndefault_model_profile=\"default\"\nsession_title_policy=\"objective\"\n[providers.local-chat]\nkind=\"openai-compatible\"\nbase_url=\"http://{address}/v1\"\nmodels=[\"local-chat-model\"]\ndefault_model=\"local-chat-model\"\n[model_profiles.default]\nprovider=\"local-chat\"\nmodel=\"local-chat-model\"\ncontext_window_tokens=128000\n"),
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
        .bind_conversation("%1", conversation, 0)
        .unwrap();
    // Real admitted tasks establish closure. The large first task is outside
    // the optional tail; the two short later tasks and their original groups
    // remain raw. Neither a display role nor a fixture flag proves completion.
    let mut groups = Vec::new();
    for sequence in 1..=3 {
        let prompt = format!(
            "eligible-source-{sequence} {}",
            "completed detail ".repeat(if sequence == 1 { 5_000 } else { 1 }),
        );
        let admitted = service
            .execute_agent_shell_command(&primary, &prompt)
            .unwrap();
        assert!(admitted.contains(r#""state":"running""#), "{admitted}");
        let turn_id = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id
            .clone()
            .unwrap();
        crate::test_support::compaction_source::complete_task(
            &mut service,
            &turn_id,
            &format!(
                "typed-result-{sequence} {}",
                if sequence == 1 {
                    "result detail ".repeat(5_000)
                } else {
                    String::new()
                }
            ),
            mez_agent::ProviderApiCompatibility::OpenAiChatCompletions,
        );
        let group = store
            .inspect(conversation)
            .unwrap()
            .into_iter()
            .rev()
            .find_map(|row| {
                if row.turn_id != turn_id {
                    return None;
                }
                match mez_agent::TranscriptContextEvent::from_transcript_content(&row.content) {
                    Some(mez_agent::TranscriptContextEvent::ExecutionBlock {
                        execution_group_id,
                        ..
                    }) => execution_group_id,
                    _ => None,
                }
            })
            .unwrap();
        groups.push(group);
    }
    let original = store.inspect(conversation).unwrap();
    let turns_before = service.agent_turn_ledger().turns().to_vec();
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
    let selected_records = original
        .iter()
        .filter_map(|row| {
            match mez_agent::TranscriptContextEvent::from_transcript_content(&row.content) {
                Some(mez_agent::TranscriptContextEvent::ExecutionBlock {
                    execution_group_id: Some(group),
                    content,
                    ..
                }) if group == groups[0] => Some(content),
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    // The completed task also owns a canonical committed-evidence member.
    // Qualify its actual framing rather than assuming only assistant/result.
    let selected_kinds = original
        .iter()
        .filter_map(|row| {
            match mez_agent::TranscriptContextEvent::from_transcript_content(&row.content) {
                Some(mez_agent::TranscriptContextEvent::ExecutionBlock {
                    source,
                    execution_group_id: Some(group),
                    ..
                }) if group == groups[0] => Some(source),
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selected_kinds,
        [
            mez_agent::ContextSourceKind::CommittedEvidence,
            mez_agent::ContextSourceKind::TranscriptAssistant,
            mez_agent::ContextSourceKind::ActionResult,
        ]
    );
    let expected_markers = selected_records
        .iter()
        .map(|content| content.matches("typed-result-1").count())
        .sum::<usize>();
    assert!(expected_markers > 0);
    let server = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = async_provider_concurrency_read_http_request(&mut stream).await;
        let body: serde_json::Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["model"], "local-chat-model");
        let source = body["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(source.contains("eligible-source-1"));
        if typed {
            // Compare each original canonical occurrence, including its own
            // internal repetitions, with the actual decoded HTTP source.
            for content in &selected_records {
                assert_eq!(source.matches(content.as_str()).count(), 1);
            }
            assert_eq!(source.matches("typed-result-1").count(), expected_markers);
            assert!(!source.contains("typed-result-2"));
            assert!(!source.contains("typed-result-3"));
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
                if store
                    .load_replay_epoch(conversation, mez_agent::AgentContext::empty())
                    .unwrap()
                    .is_some()
                {
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
    assert_eq!(exit.service.agent_turn_ledger().turns(), turns_before);
    let (_, epoch) = store
        .load_replay_epoch(conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(
        epoch.context().chronology()[0].block().content,
        "model-authored-compaction-marker"
    );
    assert_eq!(epoch.revision().get(), 1);
    let archive = store.inspect(conversation).unwrap();
    assert_eq!(&archive[..original.len()], original.as_slice());
    let reopened = AgentTranscriptStore::new(root.join("transcripts"));
    let (_, cold) = reopened
        .load_replay_epoch(conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    // Each load gets fresh runtime ownership, not a cloned producer capability.
    // Durable head, raw occurrences, roles, bytes and cached costs remain exact.
    assert_eq!(cold.head(), epoch.head());
    assert_eq!(cold.revision(), epoch.revision());
    assert_eq!(cold.context().chronology(), epoch.context().chronology());
    assert_eq!(cold.input_estimates(), epoch.input_estimates());
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
                .any(|block| block.content.contains("typed-result-1"))
        );
        assert!(
            context
                .chronology()
                .iter()
                .all(|event| { event.execution_group_id() != Some(&groups[0]) }),
            "covered execution occurrences must not return after publication"
        );
        for group in &groups[1..] {
            let expected = original
                .iter()
                .filter_map(
                    |row| match mez_agent::TranscriptContextEvent::from_transcript_content(
                        &row.content,
                    ) {
                        Some(mez_agent::TranscriptContextEvent::ExecutionBlock {
                            source,
                            execution_group_id: Some(original_group),
                            content,
                            ..
                        }) if &original_group == group => Some((source, content)),
                        _ => None,
                    },
                )
                .collect::<Vec<_>>();
            assert!(!expected.is_empty());
            assert_eq!(
                context
                    .chronology()
                    .iter()
                    .filter(|event| event.execution_group_id() == Some(group))
                    .map(|event| (event.block().source, event.block().content.clone()))
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }
    exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
