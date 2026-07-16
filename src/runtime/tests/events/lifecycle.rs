//! Runtime tests for events lifecycle behavior.

use super::*;

/// Verifies mixed `say` plus semantic file-mutation batches present the file
/// diff before the assistant summary.
///
/// Providers can emit a convenient final message in the same batch as a file
/// action. Normal mode should not show that prose before the runtime has
/// actually applied the file action and displayed its diff, otherwise users see
/// a completion claim followed by unrelated-looking edit logs.
#[test]
fn runtime_mixed_say_and_file_mutation_defers_say_until_after_diff() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(160, 60).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let target_rel = format!(
        "target/mez-semantic-mutation-deferred-say-{}-{unique}/note.txt",
        std::process::id()
    );
    let target = PathBuf::from(&target_rel);
    fs::create_dir_all(target.parent().unwrap()).unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-semantic-deferred-say","input":"create a note"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap semantic response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![
                    mez_agent::AgentAction {
                        id: "say-1".to_string(),
                        rationale: String::new(),
                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Final,
                            text: "Created `note.txt`.".to_string(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                    mez_agent::AgentAction {
                        id: "patch-1".to_string(),
                        rationale: "write a file".to_string(),
                        payload: mez_agent::AgentActionPayload::ApplyPatch {
                            patch: format!(
                                "*** Begin Patch\n*** Add File: {target_rel}\n+alpha\n+beta\n*** End Patch"
                            ),
                            strip: None,
                        },
                    },
                ],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert_eq!(execution.action_results[1].status, ActionStatus::Running);
    assert!(
        service
            .running_shell_transactions
            .values()
            .any(|transaction| matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "patch-1"
            )),
        "file actions should dispatch through pane shell transactions"
    );
    let marker = service
        .running_shell_transactions
        .keys()
        .next()
        .cloned()
        .expect("apply_patch transaction should be running");
    let transaction = service.running_shell_transactions.get_mut(&marker).unwrap();
    transaction.command = "# __MEZ_APPLY_PATCH_WRITE_PHASE__".to_string();
    transaction.observed_output_preview = format!(
        "diff -- apply patch\n--- /dev/null\n+++ b/{target_rel}\n@@ -0,0 +1,2 @@\n+alpha\n+beta\n"
    );
    transaction.observed_output_bytes = transaction.observed_output_preview.len();
    service
        .observe_agent_shell_transaction_start("%1", &marker, "turn-1", "agent-%1", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, "turn-1", "agent-%1", "%1", 0)
        .unwrap();
    poll_until_turn_state(&mut service, "turn-1", AgentTurnState::Completed);

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let diff_index = pane_text.find("@@ -0,0 +1,2 @@").unwrap_or(usize::MAX);
    let say_index = pane_text.find("Created note.txt.").unwrap_or(usize::MAX);
    assert!(diff_index < say_index, "{pane_text}");
    assert!(pane_text.contains("Worked for"), "{pane_text}");
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(target.parent().unwrap());
}

/// Verifies that invalid runtime-backed slash command arguments are converted
/// into pane-local display responses rather than JSON-RPC errors. This keeps
/// the agent prompt alive for commands whose validation happens in runtime
/// handlers instead of the slash-command registry.
#[test]
fn runtime_control_reports_invalid_runtime_slash_args_as_agent_display() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-command-invalid","method":"agent/shell/command","params":{"idempotency_key":"agent-command-invalid","input":"/model one two three"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(
        response.contains(
            "agent command error: /model accepts at most a model name and optional reasoning level"
        ),
        "{response}"
    );
    assert!(!response.contains(r#""error""#), "{response}");
}

/// Verifies model-authored diff output uses the diff content renderer.
///
/// Diffs are a structured text media type rather than prose. The runtime should
/// parse the unified diff, omit raw diff scaffolding from the visible pane log,
/// and apply file-aware token colors to changed source lines when the file path
/// identifies a supported syntax.
#[test]
fn runtime_agent_diff_say_renders_file_aware_syntax_spans() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-diff","method":"agent/shell/command","params":{"idempotency_key":"agent-diff-say","input":"show diff"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let diff = "diff -- update file\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@\n-fn old() {}\n+fn new() {}";
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "diff say response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "say-diff".to_string(),
                    rationale: String::new(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: diff.to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_DIFF_CONTENT_TYPE.to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let pane_text = styled_lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(pane_text.contains("--- src/main.rs"), "{pane_text}");
    assert!(pane_text.contains("+++ src/main.rs"), "{pane_text}");
    assert!(pane_text.contains("@@ -1,1 +1,1 @@"), "{pane_text}");
    assert!(
        pane_text.contains("            1 +fn new() {}"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("diff -- update file"), "{pane_text}");
    let addition_line = styled_lines
        .iter()
        .find(|line| line.text.contains("            1 +fn new() {}"))
        .unwrap();
    let syntax_start = "▐ ".chars().count() + 15;
    assert!(
        addition_line
            .style_spans
            .iter()
            .any(|span| span.start >= syntax_start && span.rendition.foreground.is_some()),
        "{addition_line:?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that a provider response containing only a final completion marker
/// still leaves an explicit pane-buffer status. This prevents the default
/// non-verbose view from looking silent when the model forgets to include a
/// user-facing `say` action.
#[test]
fn runtime_agent_complete_without_say_reports_visible_completion_status() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-visible-complete","input":"finish silently"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap complete response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "the task is complete".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: String::new(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: "Done.".to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("mez> Done."), "{pane_text}");
    assert!(
        pane_text.contains("thinking: the task is complete"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that a completed but unparseable bootstrap attempt is still
/// one-shot. Retrying the same hidden wrapper on every tick floods the pane
/// with Mezzanine-owned shell boilerplate without improving context.
#[test]
fn runtime_bootstrap_unparsed_output_does_not_retry_forever() {
    let mut service = test_runtime_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_pane_readiness("%1", PaneReadinessState::Busy);
    let marker = "bootstrap-unparsed-marker";
    let turn_id = "bootstrap-%1-unparsed";
    service.running_shell_transactions.insert(
        marker.to_string(),
        RunningShellTransactionRef {
            turn_id: turn_id.to_string(),
            kind: RunningShellTransactionKind::Bootstrap,
            pane_id: "%1".to_string(),
            command: "bootstrap".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let observed = service
        .observe_agent_shell_transaction_end("%1", marker, turn_id, "agent-%1", "%1", 0)
        .unwrap();

    assert_eq!(observed, 1);
    assert!(!service.pane_bootstrap_is_pending_for_tests("%1"));
    assert!(service.pane_environment_signature("%1").is_none());
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Ready
    );
    service.maybe_bootstrap_ready_panes().unwrap();
    assert!(
        service
            .running_shell_transactions
            .values()
            .all(|transaction| transaction.kind != RunningShellTransactionKind::Bootstrap)
    );
    let events = service
        .event_log
        .as_ref()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.payload.contains(r#""bootstrap":"unparsed""#)),
        "{events:?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that generated runtime model profiles produce distinct identities
/// when latency preference differs so pane-local overrides for the same
/// provider/model/reasoning tuple do not collapse together.
#[test]
fn runtime_generated_profile_identity_differs_by_latency_preference() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"high\"\nlatency_preference = \"default\"\n"
                .to_string(),
        }])
        .unwrap();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![mez_agent::ProviderModelInfo {
            id: "gpt-5.5".to_string(),
            display_name: None,
            reasoning_levels: vec!["high".to_string()],
            context_window_tokens: Some(1_050_000),
            capabilities: Vec::new(),
        }],
        vec!["high".to_string()],
    );

    let default_outcome = service
        .execute_agent_shell_latency_command("%1", "/latency default")
        .unwrap();
    let default_text = match default_outcome {
        super::AgentShellCommandOutcome::Mutated { body, .. } => body,
        other => panic!("expected Mutated outcome, got {other:?}"),
    };
    assert!(default_text.contains("latency_preference=default"));

    let slow_outcome = service
        .execute_agent_shell_latency_command("%1", "/latency slow")
        .unwrap();
    let slow_text = match slow_outcome {
        super::AgentShellCommandOutcome::Mutated { body, .. } => body,
        other => panic!("expected Mutated outcome, got {other:?}"),
    };
    assert!(slow_text.contains("latency_preference=slow"));

    let (_name, profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(
        profile.latency_preference.as_deref(),
        Some("slow"),
        "last applied latency should be slow"
    );
}

/// Verifies active agent shell metadata survives a daemon-style restart for the
/// same Mezzanine session without replaying a prompt or requiring a snapshot.
#[test]
fn runtime_restores_active_agent_session_metadata_for_same_session() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-active-restore"));
    let cwd = temp_root("runtime-agent-active-restore-cwd");
    fs::create_dir_all(&cwd).unwrap();
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "saved restart context".to_string(),
        })
        .unwrap();
    transcript_store
        .append_prompt_history("saved", "remember this")
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_current_working_directory("%1".to_string(), cwd.clone());

    let resumed = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-resume","method":"agent/shell/command","params":{"idempotency_key":"restore-resume","input":"/resume saved"}}"#,
        &primary,
    );
    assert!(resumed.contains("conversation_id=saved"), "{resumed}");
    let saved_token_usage_key = mez_agent::ModelTokenUsageKey::new("openai", "gpt-5.6-sol");
    let saved_token_usage = mez_agent::ModelTokenUsage {
        input_tokens: 321,
        output_tokens: 45,
        reasoning_tokens: 12,
        cached_input_tokens: Some(123),
        cache_write_input_tokens: None,
    };
    service.record_agent_provider_token_usage("%1", saved_token_usage);
    let routing = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-routing","method":"agent/shell/command","params":{"idempotency_key":"restore-routing","input":"/routing on"}}"#,
        &primary,
    );
    assert!(routing.contains("enabled=true"), "{routing}");
    let approval = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-approval","method":"agent/shell/command","params":{"idempotency_key":"restore-approval","input":"/approval full-access"}}"#,
        &primary,
    );
    assert!(approval.contains("requested=full-access"), "{approval}");
    let personality = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-personality","method":"agent/shell/command","params":{"idempotency_key":"restore-personality","input":"/personality concise"}}"#,
        &primary,
    );
    assert!(personality.contains("style=concise"), "{personality}");
    let log_level = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-log-level","method":"agent/shell/command","params":{"idempotency_key":"restore-log-level","input":"/log-level trace"}}"#,
        &primary,
    );
    assert!(log_level.contains("now trace"), "{log_level}");
    let directive = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore-directive","method":"agent/shell/command","params":{"idempotency_key":"restore-directive","input":"/directive Prefer focused tests."}}"#,
        &primary,
    );
    assert!(directive.contains("Prefer focused tests."), "{directive}");
    let saved_metadata = transcript_store
        .load_agent_session_metadata(service.session().id.as_str())
        .unwrap();
    assert_eq!(saved_metadata.len(), 1);
    assert_eq!(
        saved_metadata[0].working_directory.as_deref(),
        Some(cwd.to_string_lossy().as_ref())
    );
    assert_eq!(saved_metadata[0].token_usage, saved_token_usage);
    assert_eq!(
        saved_metadata[0]
            .token_usage_by_model
            .get(&saved_token_usage_key),
        Some(&saved_token_usage)
    );
    assert_eq!(saved_metadata[0].routing_enabled, Some(true));
    assert_eq!(
        saved_metadata[0].approval_policy.as_deref(),
        Some("full-access")
    );

    let mut restored = test_runtime_service();
    restored.session.id = service.session().id.clone();
    restored.set_agent_transcript_store(transcript_store.clone());
    let restored_count = restored
        .restore_agent_sessions_from_transcript_store()
        .unwrap();

    let restored_session = restored.agent_shell_store().get("%1").unwrap();
    assert_eq!(restored_count, 1);
    assert_eq!(restored_session.session_id, "saved");
    assert_eq!(restored_session.visibility, AgentShellVisibility::Visible);
    assert_eq!(restored_session.transcript_entries, 1);
    assert_eq!(restored_session.log_level, AgentLogLevel::Trace);
    assert_eq!(
        restored
            .agent_token_usage_by_conversation
            .get("saved")
            .and_then(|usage_by_model| usage_by_model.get(&saved_token_usage_key))
            .copied(),
        Some(saved_token_usage)
    );
    assert_eq!(
        restored.agent_routing_overrides.get("%1").copied(),
        Some(true)
    );
    assert_eq!(
        restored.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    assert_eq!(
        restored.pane_current_working_directory("%1").as_deref(),
        Some(cwd.as_path())
    );
    assert_eq!(
        restored.agent_response_styles.get("%1").map(String::as_str),
        Some("concise")
    );
    assert_eq!(
        restored_session.directive.as_deref(),
        Some("Prefer focused tests.")
    );
    assert_eq!(
        restored
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .history(),
        &[
            String::from("remember this"),
            String::from("/resume saved"),
            String::from("/routing on"),
            String::from("/approval full-access"),
            String::from("/personality concise"),
            String::from("/log-level trace"),
            String::from("/directive Prefer focused tests."),
        ]
    );
    let context = restored
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == mez_agent::ContextSourceKind::TranscriptUser
            && block.content.contains("saved restart context")
    }));
    let _ = fs::remove_dir_all(cwd);
}

/// Verifies active agent metadata from a different Mezzanine session id does
/// not auto-bind a fresh runtime pane to a stale conversation.
#[test]
fn runtime_does_not_restore_agent_metadata_for_other_sessions() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-other-session"));
    transcript_store
        .save_agent_session_metadata(
            "$foreign",
            &[mez_agent::transcript::AgentSessionMetadata {
                mezzanine_session_id: "$foreign".to_string(),
                pane_id: "%1".to_string(),
                conversation_id: "foreign".to_string(),
                prompt_cache_lineage_id: "lineage-foreign".to_string(),
                visibility: "visible".to_string(),
                running_turn_id: None,
                transcript_entries: 1,
                log_level: "normal".to_string(),
                pane_model_profile: None,
                planning_enabled: false,
                response_style: None,
                directive: None,
                routing_enabled: None,
                approval_policy: None,
                working_directory: None,
                project_root: None,
                context_usage: None,
                context_usage_snapshot: None,
                token_usage: Default::default(),
                token_usage_by_model: Default::default(),
            }],
        )
        .unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store);

    let restored = service
        .restore_agent_sessions_from_transcript_store()
        .unwrap();

    assert_eq!(restored, 0);
    assert!(service.agent_shell_store().get("%1").is_none());
}

/// Verifies crash-recovered active metadata never resumes a previously running
/// turn automatically; it restores the conversation and records the turn as
/// interrupted so retry requires a fresh user action.
#[test]
fn runtime_restored_agent_metadata_marks_running_turn_interrupted() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-active-interrupted"));
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    service
        .start_agent_turn(mez_agent::AgentTurnRecord {
            turn_id: "turn-running-restore".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 200,
            policy_profile: "runtime".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: AgentTurnState::Queued,

            initial_capability: None,
        })
        .unwrap();
    assert_eq!(
        transcript_store
            .load_agent_session_metadata(service.session().id.as_str())
            .unwrap()[0]
            .running_turn_id
            .as_deref(),
        Some("turn-running-restore")
    );

    let mut restored = test_runtime_service();
    restored.session.id = service.session().id.clone();
    restored.set_agent_transcript_store(transcript_store);
    let restored_count = restored
        .restore_agent_sessions_from_transcript_store()
        .unwrap();

    let restored_session = restored.agent_shell_store().get("%1").unwrap();
    let restored_turn = restored
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-running-restore")
        .unwrap();
    assert_eq!(restored_count, 1);
    assert_eq!(restored_session.session_id, conversation_id);
    assert_eq!(restored_session.running_turn_id, None);
    assert_eq!(restored_turn.state, AgentTurnState::Interrupted);
}

/// Verifies runtime event fanout batches all ready event notifications for one
/// connection into a single sink write while still advancing the per-connection
/// delivery cursor for each visible event.
///
/// High-volume event streams can otherwise perform one write per event per
/// connection, so this regression protects the fanout path from regressing to
/// O(events × clients) write calls when a bounded replay batch is ready.
#[test]
fn runtime_event_fanout_batches_frames_per_connection() {
    struct RecordingEventSink {
        frames: Vec<(String, Vec<u8>)>,
    }

    impl crate::runtime::RuntimeEventFanoutSink for RecordingEventSink {
        fn send_frame(&mut self, connection_id: &str, frame: &[u8]) -> crate::Result<()> {
            self.frames
                .push((connection_id.to_string(), frame.to_vec()));
            Ok(())
        }
    }

    let mut event_log = crate::event::EventLog::new(8, 1024).unwrap();
    event_log
        .append(
            crate::event::EventKind::Diagnostic,
            Some("session".to_string()),
            crate::event::EventVisibility::SessionView,
            r#"{"message":"first"}"#,
        )
        .unwrap();
    event_log
        .append(
            crate::event::EventKind::Diagnostic,
            Some("session".to_string()),
            crate::event::EventVisibility::SessionView,
            r#"{"message":"second"}"#,
        )
        .unwrap();

    let mut connections = crate::runtime::RuntimeEventConnectionTable::default();
    connections
        .attach(
            "event-connection",
            crate::event::EventAudience::Primary,
            true,
            0,
        )
        .unwrap();
    let wakeups = connections.wakeups(Some(&event_log), 10);

    let mut sink = RecordingEventSink { frames: Vec::new() };
    let delivered =
        crate::runtime::flush_runtime_event_wakeups(&mut connections, &wakeups, &mut sink).unwrap();

    assert_eq!(delivered, 2);
    assert_eq!(sink.frames.len(), 1);
    assert_eq!(sink.frames[0].0, "event-connection");
    let batched = String::from_utf8(sink.frames[0].1.clone()).unwrap();
    assert!(batched.contains(r#""message":"first""#), "{batched}");
    assert!(batched.contains(r#""message":"second""#), "{batched}");
    assert!(connections.wakeups(Some(&event_log), 10).is_empty());
}
