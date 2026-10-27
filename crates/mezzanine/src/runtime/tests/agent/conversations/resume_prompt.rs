//! Agent conversation resume prompt tests.

use super::*;

/// Diagnoses a saved-session resume using only a disposable copy of its files.
/// The explicit source directory is read-only evidence; the worker renders the
/// copied presentation without provider I/O or touching the original catalog.
#[test]
#[ignore = "requires an explicit local saved-session evidence directory"]
fn diagnostic_saved_session_resume_from_disposable_copy() {
    let source = std::path::PathBuf::from(
        std::env::var_os("MEZ_RESUME_DIAGNOSTIC_SOURCE").expect("explicit session directory"),
    );
    let conversation_id = source.file_name().unwrap().to_str().unwrap().to_string();
    let root = temp_root("runtime-resume-crash-diagnostic");
    let destination = root.join(&conversation_id);
    std::fs::create_dir_all(&destination).unwrap();
    for entry in std::fs::read_dir(&source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), destination.join(entry.file_name())).unwrap();
        }
    }
    let transcript_store = AgentTranscriptStore::new(root);
    let mut service = test_runtime_service();
    std::fs::copy(
        source.parent().unwrap().join("active-agent-sessions.tsv"),
        transcript_store.agent_session_metadata_path_for_tests(),
    )
    .unwrap();
    let mut metadata = transcript_store
        .load_agent_session_metadata("$1790712711571452756")
        .unwrap()
        .into_iter()
        .find(|metadata| metadata.conversation_id == conversation_id)
        .unwrap();
    let runtime_id = service.session().id.to_string();
    metadata.mezzanine_session_id = runtime_id.clone();
    metadata.pane_id = "%1".to_string();
    transcript_store
        .save_agent_session_metadata(&runtime_id, &[metadata])
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(140, 42).unwrap(), 120)
        .unwrap();
    if std::env::var_os("MEZ_RESUME_DIAGNOSTIC_RESTORE").is_some() {
        eprintln!("diagnostic: restore original running-turn metadata");
        assert_eq!(
            service
                .restore_agent_sessions_from_transcript_store()
                .unwrap(),
            1
        );
        assert_eq!(
            service.agent_shell_store().get("%1").unwrap().session_id,
            conversation_id,
        );
        eprintln!("diagnostic: metadata restoration completed");
        return;
    }
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    eprintln!("diagnostic: dispatch resume");
    service
        .execute_agent_shell_command(&primary, &format!("/resume {conversation_id}"))
        .unwrap();
    let dispatch = service
        .take_pending_deferred_agent_commands()
        .pop()
        .unwrap();
    let work = service
        .claim_agent_command_work(
            &dispatch.primary_client_id,
            &dispatch.pane_id,
            &dispatch.command,
            &dispatch.input,
            dispatch.claim_generation,
            &dispatch.conversation_id,
        )
        .unwrap()
        .unwrap();
    eprintln!("diagnostic: prepare direct resume projection");
    let outcome = RuntimeSessionService::execute_deferred_agent_command(&work);
    assert!(matches!(
        outcome,
        crate::runtime::RuntimeAgentCommandAsyncOutcome::DirectResume { .. }
    ));
    eprintln!("diagnostic: install direct resume projection");
    assert!(service.complete_agent_command_work(&work, outcome).unwrap());
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        conversation_id,
    );
    eprintln!("diagnostic: resume completed");
}

/// Verifies `/resume` completion includes saved conversation ids supplied by
/// the runtime transcript store.
#[test]
fn runtime_agent_prompt_resume_autocompletes_saved_session_uuid() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-complete"));
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "018f6b3a-1b2c-7000-9000-cafebabefeed".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-saved".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.request_agent_prompt_selector_extra_candidates_refresh("%1");
    service.complete_agent_prompt_selector_refresh_for_tests("%1");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/resume 018f".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\t".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(
        service
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/resume 018f6b3a-1b2c-7000-9000-cafebabefeed "
    );
}

/// Verifies resume completion excludes delegated child UUIDs even though
/// direct `/resume <uuid>` remains supported by command execution.
///
/// Completion is a discovery surface and must follow the same root-only
/// default as the picker and `--latest`.
#[test]
fn runtime_agent_prompt_resume_completion_excludes_subagent_uuid() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-child"));
    let child_id = "018f6b3a-1b2c-7000-9000-delegated001";
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: child_id.to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-child".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "delegated prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .save_subagent_conversation_contract(
            child_id,
            mez_agent::SubagentSessionLineage {
                parent_agent_id: "agent-%1".to_string(),
                root_agent_id: "agent-%1".to_string(),
                depth: 1,
                display_name: "delegated child".to_string(),
                terminal: false,
            },
            mez_agent::AllowedActionSet::say_only(),
        )
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.request_agent_prompt_selector_extra_candidates_refresh("%1");
    service.complete_agent_prompt_selector_refresh_for_tests("%1");

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/resume 018f".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\t".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(
        service
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/resume 018f"
    );
}

/// Verifies later prompt input retains selector candidates loaded by the first
/// keystroke instead of rereading saved transcripts on every input batch.
#[test]
fn runtime_agent_prompt_resume_completion_uses_cached_saved_sessions() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-agent-resume-complete-cache");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "018f6b3a-1b2c-7000-9000-cafebabefeed".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-saved".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.request_agent_prompt_selector_extra_candidates_refresh("%1");
    service.complete_agent_prompt_selector_refresh_for_tests("%1");

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"/resume 018f".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    std::fs::remove_dir_all(&transcript_root).unwrap();

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\t".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(
        service
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/resume 018f6b3a-1b2c-7000-9000-cafebabefeed "
    );
}

/// Verifies `/resume <session>` replays saved transcript context into the pane
/// buffer after rebinding the pane-local agent shell. A resumed task should
/// show enough prior conversation content for the user to continue without
/// opening a separate transcript file.
#[test]
fn runtime_agent_prompt_resume_displays_saved_transcript_context() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-display"));
    let conversation_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
    for (sequence, role, content) in [
        (1, mez_agent::transcript::TranscriptRole::User, "aGVsbG8K"),
        (
            2,
            mez_agent::transcript::TranscriptRole::Assistant,
            "I inspected the repo and started the change",
        ),
        (
            3,
            mez_agent::transcript::TranscriptRole::Tool,
            r#"action_id=action-1 action_type=say status=succeeded content: ignored structured_content: {"kind":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"Implemented the change"}"#,
        ),
    ] {
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%9".to_string(),
                pane_id: "%9".to_string(),
                content: content.to_string(),
            })
            .unwrap();
    }
    transcript_store
        .save_compaction_epoch(conversation_id, 1, "CROSS_PANE_DURABLE_SUMMARY")
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, &format!("/resume {conversation_id}"))
        .unwrap();

    assert!(
        response.contains(r#""body":null"#),
        "the direct resume accepts before durable replay: {response}"
    );
    let settled = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .expect("the deferred direct resume settles");
    assert!(settled.contains("resumed=true"), "{settled}");
    let history = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(
        history
            .blocks()
            .iter()
            .any(|block| block.content == "CROSS_PANE_DURABLE_SUMMARY")
    );
    assert!(
        !history
            .blocks()
            .iter()
            .any(|block| block.content == "aGVsbG8K")
    );
    assert!(
        history
            .blocks()
            .iter()
            .any(|block| block.content.contains("I inspected the repo"))
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("Resumed Agent Session"), "{pane_text}");
    assert!(
        pane_text.contains(&format!("Conversation ID: {conversation_id}")),
        "{pane_text}"
    );
    assert!(pane_text.contains("Entries: 3"), "{pane_text}");
    assert!(pane_text.contains("Resumed:\n│ yes"), "{pane_text}");
    assert!(pane_text.contains("user> hello"), "{pane_text}");
    assert!(
        pane_text.contains("mez> I inspected the repo and started the change"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent: Implemented the change"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("aGVsbG8K"), "{pane_text}");
    assert!(!pane_text.contains("structured_content"), "{pane_text}");
    assert!(!pane_text.contains("[1 turn=turn-1]"), "{pane_text}");
}

/// Verifies the direct-resume worker lease keeps a concurrent durable
/// presentation append behind actor installation of the prepared projection.
#[test]
fn runtime_agent_prompt_resume_installs_before_concurrent_presentation_append() {
    let mut service = test_runtime_service();
    let transcript_store =
        AgentTranscriptStore::new(temp_root("runtime-agent-resume-stale-presentation"));
    let conversation_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-saved".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    service
        .execute_agent_shell_command(&primary, &format!("/resume {conversation_id}"))
        .unwrap();
    let dispatch = service
        .take_pending_deferred_agent_commands()
        .pop()
        .expect("direct resume should dispatch worker preparation");
    let work = service
        .claim_agent_command_work(
            &dispatch.primary_client_id,
            &dispatch.pane_id,
            &dispatch.command,
            &dispatch.input,
            dispatch.claim_generation,
            &dispatch.conversation_id,
        )
        .unwrap()
        .expect("direct resume should claim worker preparation");
    let crate::runtime::RuntimeAgentCommandAsyncOutcome::DirectResume { store, read } =
        RuntimeSessionService::execute_deferred_agent_command(&work)
    else {
        panic!("direct resume worker should return prepared state");
    };
    let (writer_started, writer_started_rx) = std::sync::mpsc::channel();
    let (writer_finished, writer_finished_rx) = std::sync::mpsc::channel();
    let writer_store = transcript_store.clone();
    let writer_conversation_id = conversation_id.to_string();
    let writer = std::thread::spawn(move || {
        writer_started.send(()).unwrap();
        writer_store
            .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
                presentation: None,
                conversation_id: writer_conversation_id,
                sequence: 1,
                created_at_unix_seconds: 2,
                pane_id: "%9".to_string(),
                turn_id: None,
                terminal_width: 80,
                style_names: vec!["assistant".to_string()],
                display_lines: vec!["late presentation".to_string()],
                copy_lines: vec!["late presentation".to_string()],
                ansi_text: None,
                source_text: Some("late presentation".to_string()),
                source_content_type: Some("text/plain; charset=utf-8".to_string()),
            })
            .unwrap();
        writer_finished.send(()).unwrap();
    });
    writer_started_rx.recv().unwrap();
    assert!(matches!(
        writer_finished_rx.recv_timeout(std::time::Duration::from_millis(20)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));

    assert!(
        service
            .complete_agent_command_work(
                &work,
                crate::runtime::RuntimeAgentCommandAsyncOutcome::DirectResume { store, read },
            )
            .unwrap(),
        "the current command claim should install its prepared resume"
    );
    writer_finished_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("presentation append should proceed after actor installation");
    writer.join().unwrap();
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text.contains("late presentation"),
        "the prepared projection must install before the later durable append: {pane_text}"
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some(conversation_id)
    );
}

/// Resumes a real three-row archive with a prefix epoch through row two, then
/// captures fork history through the ordinary runtime boundary. Provider replay
/// is a control: it must contain the prior summary and only the last raw row.
/// Fork capture must use archive high-water three, not prefix two plus the
/// resumed archive row count three. No counter normalization or provider call
/// may conceal the mismatch between those separate sequence/count domains.
#[test]
fn runtime_resumed_compacted_archive_fork_keeps_actual_high_water() {
    let root = temp_root("runtime-resumed-compacted-fork-count");
    let transcript_store = AgentTranscriptStore::new(root.clone());
    let conversation_id = "018f6b3a-1b2c-7000-9000-cafebabe0007";
    for (sequence, content) in [
        (1, "covered row one"),
        (2, "covered row two"),
        (3, "retained row three"),
    ] {
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: conversation_id.into(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("closed-turn-{sequence}"),
                agent_id: "agent-%9".into(),
                pane_id: "%9".into(),
                content: content.into(),
            })
            .unwrap();
    }
    transcript_store
        .save_compaction_epoch(conversation_id, 2, "PREFIX_SUMMARY")
        .unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, &format!("/resume {conversation_id}"))
        .unwrap();
    let settled = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .expect("real resume must settle");
    assert!(settled.contains("resumed=true"), "{settled}");
    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content == "PREFIX_SUMMARY")
    );
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains("retained row three"))
    );
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.content.contains("covered row"))
    );
    let work = service
        .prepare_subagent_fork_read_work("agent-%1")
        .unwrap()
        .expect("resumed raw suffix must have fork work");
    let snapshot = work
        .execute()
        .expect("resumed fork must not invent a missing high-water row");
    assert_eq!(
        snapshot
            .entries
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        [3]
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// Resumes a prefix-compacted archive before issuing the actual manual command.
/// A fitting retention budget must force only uncovered closed work into the
/// model task. The old archive count incorrectly starts again at row one,
/// behind the already committed row-two epoch. No provider call is needed to
/// inspect the frozen selected boundary and raw source handed to the task.
#[test]
fn runtime_resumed_compacted_archive_manual_selection_stays_after_prefix() {
    let root = temp_root("runtime-resumed-compacted-manual-count");
    let store = AgentTranscriptStore::new(root.clone());
    let conversation = "018f6b3a-1b2c-7000-9000-cafebabe0008";
    for sequence in 1..=3 {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: conversation.into(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("closed-{sequence}"),
                agent_id: "agent-%9".into(),
                pane_id: "%9".into(),
                content: format!("row-{sequence} {}", "small closed group ".repeat(20)),
            })
            .unwrap();
    }
    store
        .save_compaction_epoch(conversation, 2, "PRIOR_PREFIX")
        .unwrap();
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "resumed-count-manual".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"count-test\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"count-test\"]\ndefault_model = \"count-test\"\n[model_profiles.count-test]\nprovider = \"openai\"\nmodel = \"count-test\"\ncontext_window_tokens = 128000\n".into(),
    }]).unwrap();
    service.set_agent_transcript_store(store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, &format!("/resume {conversation}"))
        .unwrap();
    let resumed = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(resumed.contains("resumed=true"), "{resumed}");
    let accepted = service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    let settled = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap_or(accepted);
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap_or_else(|| panic!("manual compaction must queue uncovered work: {settled}"));
    assert_eq!(
        task.compacted_through_sequence,
        Some(3),
        "covered prefix must never be selected again"
    );
    assert_eq!(
        task.frozen_compaction_rows
            .iter()
            .map(|row| row.sequence)
            .collect::<Vec<_>>(),
        [3]
    );
    std::fs::remove_dir_all(root).unwrap();
}
