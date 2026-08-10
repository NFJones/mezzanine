//! End-to-end async-runtime coverage for semantic patch shell transport.
//!
//! These tests retain the full actor, adapter-owned pane worker, interactive
//! shell, generated transaction, and semantic patch pipeline so regressions in
//! delivery classification or PTY pacing cannot hide behind unit fixtures.

use super::super::*;

/// Verifies a semantic patch against a generated one-megabyte file completes
/// through an adapter-owned interactive zsh pane on macOS.
///
/// The read phase snapshots the large file and the write phase streams the
/// resulting generated payload through receiver-acknowledged PTY delivery.
/// The bounded settlement deadline accommodates macOS pacing while requiring
/// the final acknowledgement to settle the action before ordinary pane input
/// can resume, with no payload tail escaping into the interactive shell.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "current_thread")]
async fn async_zsh_large_semantic_patch_completes_and_releases_input() {
    let zsh = Path::new("/bin/zsh");
    if !zsh.is_file() {
        return;
    }

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let target_rel = format!(
        "target/mez-async-zsh-large-patch-{}-{unique}/large.txt",
        std::process::id()
    );
    let target = PathBuf::from(&target_rel);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    let repeated_line = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n";
    let initial = format!("{}MEZ_LARGE_PATCH_OLD\n", repeated_line.repeat(16_384));
    assert!(initial.len() >= 1024 * 1024);
    std::fs::write(&target, initial.as_bytes()).unwrap();
    let expected = initial.replace("MEZ_LARGE_PATCH_OLD", "MEZ_LARGE_PATCH_NEW");

    let mut service = test_service_with_shell(zsh.to_str().unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 10)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", mez_agent::AgentLogLevel::Verbose)
        .unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service.set_pane_readiness("%1", mez_agent::PaneReadinessState::Ready);

    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let workers_done = StdArc::new(AtomicBool::new(false));
    let pane_worker_handle = handle.clone();
    let pane_worker_stop = StdArc::clone(&workers_done);
    let (pane_worker_stopped_tx, pane_worker_stopped_rx) = tokio::sync::oneshot::channel();
    let pane_worker = async move {
        let report = run_async_pane_process_supervisor_service(
            pane_worker_handle,
            AsyncPaneProcessSupervisorServiceConfig {
                max_polls: u64::MAX,
                take_limit: 8,
                idle_interval: Duration::from_millis(1),
                pane_service: AsyncPaneProcessServiceConfig {
                    max_polls: u64::MAX,
                    output_drain_limit: 8,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                    foreground_metadata_interval: Duration::from_secs(60),
                },
            },
            move |_, state| {
                pane_worker_stop.load(Ordering::SeqCst)
                    || matches!(state, RuntimeLifecycleState::Stopping)
            },
        )
        .await
        .unwrap();
        let _ = pane_worker_stopped_tx.send(());
        report
    };

    let client_handle = handle.clone();
    let client_target = target.clone();
    let client_expected = expected.clone();
    let client = async move {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        let start = client_handle
            .execute_agent_shell_command(primary.clone(), "patch the large fixture".to_string())
            .await
            .unwrap();
        assert!(start.contains(r#""state":"running""#), "{start}");
        let task = client_handle
            .pending_agent_provider_tasks()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.turn_id == "turn-1")
            .expect("agent prompt should queue turn-1 provider task");
        assert_eq!(
            client_handle
                .drain_agent_provider_dispatch_side_effects(8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: AgentId::opaque(task.agent_id.clone()).unwrap(),
                turn_id: task.turn_id.clone(),
            }]
        );

        let turn = mez_agent::AgentTurnRecord {
            turn_id: task.turn_id.clone(),
            conversation_id: "conversation-large-patch".to_string(),
            agent_id: task.agent_id.clone(),
            pane_id: task.pane_id.clone(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            state: mez_agent::AgentTurnState::Running,
            cooperation_mode: None,
            initial_capability: None,
        };
        let action = mez_agent::AgentAction {
            id: "patch-large".to_string(),
            rationale: "replace the unique trailing marker".to_string(),
            payload: mez_agent::AgentActionPayload::ApplyPatch {
                patch: format!(
                    "*** Begin Patch\n*** Update File: {target_rel}\n@@ MEZ_LARGE_PATCH_OLD\n-MEZ_LARGE_PATCH_OLD\n+MEZ_LARGE_PATCH_NEW\n*** End Patch"
                ),
                strip: None,
            },
        };
        let batch = mez_agent::MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "exercise large acknowledged semantic patch delivery".to_string(),
            thought: None,
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            actions: vec![action.clone()],
            final_turn: false,
        };
        let execution = mez_agent::AgentTurnExecution {
            request: mez_agent::ModelRequest {
                provider: task.model_profile.provider.clone(),
                model: task.model_profile.model.clone(),
                reasoning_effort: task.model_profile.reasoning_profile.clone(),
                thinking_enabled: task.model_profile.thinking_enabled(),
                latency_preference: task.model_profile.latency_preference.clone(),
                prompt_cache_retention: None,
                max_output_tokens: task.model_profile.max_output_tokens(),
                temperature: None,
                stop: None,
                prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
                turn_id: task.turn_id.clone(),
                agent_id: task.agent_id.clone(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: true,
                interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
                allowed_actions: mez_agent::AllowedActionSet::for_capability(
                    mez_agent::AgentCapability::Shell,
                ),
                recovery_input: None,
                messages: vec![mez_agent::ModelMessage {
                    role: mez_agent::ModelMessageRole::User,
                    source: mez_agent::ContextSourceKind::UserInstruction,
                    placement: mez_agent::ContextPlacement::ConversationAppend,
                    content: "patch the large fixture".to_string(),
                }],
            },
            response: mez_agent::ModelResponse {
                provider: task.model_profile.provider.clone(),
                model: task.model_profile.model.clone(),
                raw_text: "large semantic patch response".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(batch),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
                &turn,
                &action,
                vec!["apply_patch accepted for pane execution".to_string()],
                Some(r#"{"state":"pending_dispatch"}"#.to_string()),
            )],
            final_turn: false,
            terminal_state: mez_agent::AgentTurnState::Running,
        };
        let mut provider_batch = RuntimeEventBatch::new();
        provider_batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id.clone(),
            execution: Box::new(execution),
        }));
        let provider_report = client_handle
            .submit_runtime_events(provider_batch)
            .await
            .unwrap();
        assert_eq!(provider_report.accepted, 1);
        assert_eq!(provider_report.applied, 1);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        let expected_tail = b"MEZ_LARGE_PATCH_NEW\n";
        let settled = loop {
            let mut tail = vec![0; expected_tail.len()];
            let mut target_file = std::fs::File::open(&client_target).unwrap();
            let target_len = target_file.metadata().unwrap().len();
            std::io::Seek::seek(
                &mut target_file,
                std::io::SeekFrom::Start(target_len - expected_tail.len() as u64),
            )
            .unwrap();
            std::io::Read::read_exact(&mut target_file, &mut tail).unwrap();
            let continuation_queued = client_handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .into_iter()
                .any(|pending| pending.turn_id == "turn-1");
            if tail == expected_tail && continuation_queued {
                break true;
            }
            if tokio::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        if settled {
            assert_eq!(
                std::fs::read_to_string(&client_target).unwrap(),
                client_expected
            );

            let hidden = client_handle
                .execute_terminal_command(primary.clone(), "agent-shell".to_string())
                .await
                .unwrap();
            assert!(hidden.contains("visibility=hidden"), "{hidden}");
            let post_input = b"printf 'MEZ_POST_PATCH_INPUT\\n'\n".to_vec();
            let written = client_handle
                .write_input_to_pane(primary, "%1", post_input.clone())
                .await
                .unwrap();
            assert_eq!(written.bytes_written, post_input.len());
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        workers_done.store(true, Ordering::SeqCst);
        pane_worker_stopped_rx
            .await
            .expect("pane worker should stop after large semantic patch settlement");
        assert_eq!(
            client_handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        settled
    };

    let (settled, supervisor_report, mut actor_exit) =
        tokio::time::timeout(Duration::from_secs(180), async {
            tokio::join!(client, pane_worker, actor.run())
        })
        .await
        .expect("large async zsh semantic patch should not hang");
    assert!(supervisor_report.spawned_workers >= 1);
    let pane_text = actor_exit
        .service
        .pane_screen("%1")
        .map(|screen| screen.normal_content_lines().join("\n"))
        .unwrap_or_default();
    let transactions = actor_exit
        .service
        .running_shell_transactions_for_tests()
        .iter()
        .map(|(marker, transaction)| {
            let phase = if transaction
                .command
                .contains("__MEZ_APPLY_PATCH_WRITE_PHASE__")
            {
                "write"
            } else if transaction
                .command
                .contains("__MEZ_APPLY_PATCH_READ_PHASE__")
            {
                "read"
            } else {
                "other"
            };
            format!(
                "{marker}: phase={phase} pending_payload={} observed_bytes={}",
                transaction.pending_input_payload.is_some(),
                transaction.observed_output_bytes
            )
        })
        .collect::<Vec<_>>();
    assert!(
        settled,
        "large semantic patch did not settle within 120 seconds; readiness={:?} transactions={transactions:#?} pane={pane_text}",
        actor_exit.service.pane_readiness_state("%1")
    );
    assert!(
        !pane_text.contains("record progress timed out"),
        "{pane_text}"
    );
    actor_exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(target.parent().unwrap()).unwrap();
}
