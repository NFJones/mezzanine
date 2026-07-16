//! Runtime tests for agent skills behavior.

use super::*;

/// Verifies skill catalog lookup logs a compact normal-mode action line.
///
/// Non-effecting skill discovery still needs the same execution visibility as
/// other runtime actions so the pane shows that the agent performed a catalog
/// lookup instead of silently continuing provider turns.
#[test]
fn runtime_skill_lookup_logs_styled_action_line_in_normal_mode() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "skill-catalog-1".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::RequestSkills,
    };

    let emitted = service
        .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
        .unwrap();
    assert!(emitted);

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let action_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent: skill lookup:"))
        .unwrap();
    assert!(
        action_line
            .text
            .contains("agent: skill lookup: available skills"),
        "{action_line:?}"
    );
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let prefix_column = display_column_for_fragment(&action_line.text, "agent:");
    let action_column = display_column_for_fragment(&action_line.text, "skill lookup");
    let prefix_rendition = styled_line_rendition_at(action_line, prefix_column);
    let action_rendition = styled_line_rendition_at(action_line, action_column);
    assert_eq!(
        prefix_rendition.foreground,
        Some(theme.colors.agent_transcript_status.foreground)
    );
    assert!(prefix_rendition.dim);
    assert_eq!(
        action_rendition.foreground,
        Some(theme.colors.agent_transcript_command.foreground)
    );
    assert!(action_rendition.bold);
}

/// Verifies skill loading logs the selected skill name and appended task
/// context in a compact normal-mode action line.
///
/// Loaded skills can materially change the next provider step, so the pane
/// should expose both the invoked skill and the extra context that shaped the
/// load request.
#[test]
fn runtime_skill_load_logs_styled_action_line_in_normal_mode() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "skill-load-1".to_string(),
        rationale: String::new(),
        payload: mez_agent::AgentActionPayload::CallSkill {
            name: "review".to_string(),
            additional_context: Some("focus on context replay churn".to_string()),
        },
    };

    let emitted = service
        .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
        .unwrap();
    assert!(emitted);

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let pane_text = styled_lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(pane_text.contains("agent: skill load: review"));
    assert!(pane_text.contains("context=focus on context replay churn"));
    let action_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent: skill load:"))
        .unwrap();
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let prefix_column = display_column_for_fragment(&action_line.text, "agent:");
    let action_column = display_column_for_fragment(&action_line.text, "skill load");
    let argument_column = display_column_for_fragment(&action_line.text, "review");
    let prefix_rendition = styled_line_rendition_at(action_line, prefix_column);
    let action_rendition = styled_line_rendition_at(action_line, action_column);
    let argument_rendition = styled_line_rendition_at(action_line, argument_column);
    assert_eq!(
        prefix_rendition.foreground,
        Some(theme.colors.agent_transcript_status.foreground)
    );
    assert!(prefix_rendition.dim);
    assert_eq!(
        action_rendition.foreground,
        Some(theme.colors.agent_transcript_command.foreground)
    );
    assert!(action_rendition.bold);
    assert_ne!(
        argument_rendition.foreground,
        Some(theme.colors.agent_transcript_command.foreground),
        "{action_line:?}"
    );
}

/// Verifies agent work refreshes project overlays from the active pane's cwd.
///
/// The daemon may start outside the repository. Before an agent prompt runs,
/// the runtime should discover `.mezzanine/config.*` under the pane project,
/// block for trust, apply the trusted overlay, and expose trusted project
/// skills through the same catalog used by `/list-skills` and `$skill`.
#[test]
fn runtime_agent_prompt_refreshes_project_overlay_and_project_skills_from_pane_cwd() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-project-refresh");
    let config_root = root.join("config-root");
    let project_root = root.join("repo");
    let nested = project_root.join("src");
    let overlay_dir = project_root.join(".mezzanine");
    let skill_dir = overlay_dir.join("skills/review");
    fs::create_dir_all(project_root.join(".git")).unwrap();
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir_all(&skill_dir).unwrap();
    let overlay_path = overlay_dir.join("config.toml");
    fs::write(&overlay_path, "version = 19\n[history]\nlines = 11\n").unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: review\ndescription: Project review workflow\n---\n\nReview this repository.\n",
    )
    .unwrap();
    service.set_config_root(config_root.clone());
    service.set_project_trust_store(
        ProjectTrustStore::default(),
        Some(config_root.join("project-trust.tsv")),
    );
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 3\n".to_string(),
        }])
        .unwrap();
    service
        .pane_current_working_directories
        .insert("%1".to_string(), nested.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let blocked_prompt = service
        .execute_agent_shell_command(&primary, "summarize this project")
        .unwrap();
    assert!(
        blocked_prompt.contains("project trust decision pending"),
        "{blocked_prompt}"
    );
    assert!(service.agent_turn_ledger.turns().is_empty());
    assert_eq!(service.terminal_history_limit(), 3);
    assert!(
        service
            .config_layers()
            .iter()
            .any(|layer| layer.path.as_ref() == Some(&overlay_path) && !layer.trusted)
    );

    let trust = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"trust-refresh","method":"project/trust/decide","params":{{"project_root":"{}","decision":"trust","idempotency_key":"trust-refresh"}}}}"#,
            json_escape(&project_root.to_string_lossy())
        ),
        &primary,
    );
    assert!(trust.contains(r#""state":"trusted""#), "{trust}");
    assert_eq!(service.terminal_history_limit(), 11);

    let skills = service
        .execute_agent_shell_command(&primary, "/list-skills")
        .unwrap();
    assert!(skills.contains("Project review workflow"), "{skills}");
    assert!(
        skills.contains("| `$review` | project | Project review workflow |"),
        "{skills}"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies explicit `$skill` prompt syntax loads the selected skill into the
/// next turn context and appends trailing prompt text as skill-specific
/// semantic context. The raw prompt remains present so the user's latest input
/// is still the visible turn instruction.
#[test]
fn runtime_agent_context_explicit_skill_prompt_loads_skill_context() {
    let config_root = temp_root("runtime-skill-context");
    let skill_dir = config_root.join("skills/review");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: review\ndescription: Review workflow\n---\n\nCheck tests and risks.\n",
    )
    .unwrap();
    let mut service = test_runtime_service();
    service.set_config_root(config_root);

    let context = service
        .agent_context_for_pane_prompt("%1", "$review focus src/lib.rs", 0)
        .unwrap();
    let skill_block = context
        .blocks
        .iter()
        .find(|block| block.label == "explicit skill review")
        .expect("missing explicit skill context block");
    let prompt_block = context
        .blocks
        .iter()
        .find(|block| block.label == "user prompt")
        .expect("missing raw user prompt block");

    assert_eq!(skill_block.source, ContextSourceKind::SkillInstruction);
    assert!(skill_block.content.contains("name: review"));
    assert!(skill_block.content.contains("Check tests and risks."));
    assert!(
        skill_block
            .content
            .contains("## Additional context\n\nfocus src/lib.rs")
    );
    assert_eq!(prompt_block.content, "$review focus src/lib.rs");
}

/// Verifies explicit `$create-skill` prompt syntax loads the built-in skill
/// authoring workflow even when no user or project skills have been installed.
/// This keeps the built-in workflow available as normal skill context instead
/// of requiring a separate command or bootstrap file.
#[test]
fn runtime_agent_context_builtin_create_skill_prompt_loads_builtin_context() {
    let mut service = test_runtime_service();

    let context = service
        .agent_context_for_pane_prompt(
            "%1",
            "$create-skill create a project skill for release notes",
            0,
        )
        .unwrap();
    let skill_block = context
        .blocks
        .iter()
        .find(|block| block.label == "explicit skill create-skill")
        .expect("missing explicit built-in skill context block");

    assert_eq!(skill_block.source, ContextSourceKind::SkillInstruction);
    assert!(skill_block.content.contains("Source: builtin"));
    assert!(skill_block.content.contains("name: create-skill"));
    assert!(skill_block.content.contains("Project scope:"));
    assert!(
        skill_block
            .content
            .contains("Invocation state: this skill is already loaded"),
        "{}",
        skill_block.content
    );
    assert!(
        skill_block
            .content
            .contains("## Additional context\n\ncreate a project skill for release notes")
    );
    let invocation_block = context
        .blocks
        .iter()
        .find(|block| block.label == "explicit skill invocation create-skill")
        .expect("missing explicit skill invocation block");
    assert_eq!(invocation_block.source, ContextSourceKind::RuntimeHint);
    assert!(
        invocation_block
            .content
            .contains("The selected skill context has already been loaded above"),
        "{}",
        invocation_block.content
    );
}

/// Verifies persisted skill payloads are not replayed into later model context.
///
/// This covers both newly compact skill-action transcripts and legacy
/// transcripts that may already contain an expanded `SKILL.md` body from an
/// earlier build. The next ordinary prompt should see the raw user request and
/// assistant/tool evidence, not stale skill workflow instructions.
#[test]
fn runtime_agent_context_omits_persisted_skill_payloads_from_replay() {
    let transcript_root = temp_root("runtime-skill-transcript-replay");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    let mut service = test_runtime_service();
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
    for (sequence, role, content) in [
        (
            1,
            mez_agent::transcript::TranscriptRole::User,
            "# Skill: review\n\nSource: project\nPath: skills/review/SKILL.md\n\nInvocation state: this skill is already loaded for the current turn.\n\nReview workflow body.",
        ),
        (
            2,
            mez_agent::transcript::TranscriptRole::Tool,
            "action_id=skill-1 action_type=call_skill status=Succeeded\ncontent:\n# Skill: review\n\nReview workflow body.",
        ),
        (
            3,
            mez_agent::transcript::TranscriptRole::Tool,
            "action_id=catalog-1 action_type=request_skills status=Succeeded\ncontent:\nAvailable skills:\n- review (project) - Review workflow body.",
        ),
        (
            4,
            mez_agent::transcript::TranscriptRole::User,
            "$review focus src/lib.rs",
        ),
        (
            5,
            mez_agent::transcript::TranscriptRole::Assistant,
            "I reviewed the requested area.",
        ),
    ] {
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: conversation_id.clone(),
                sequence,
                created_at_unix_seconds: 100,
                role,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: content.to_string(),
            })
            .unwrap();
    }
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 5)
        .unwrap();

    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    let replayed = context
        .blocks
        .iter()
        .filter(|block| {
            matches!(
                block.source,
                ContextSourceKind::TranscriptUser
                    | ContextSourceKind::TranscriptAssistant
                    | ContextSourceKind::TranscriptTool
            )
        })
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    assert!(replayed.contains("$review focus src/lib.rs"), "{replayed}");
    assert!(
        replayed.contains("I reviewed the requested area."),
        "{replayed}"
    );
    assert!(!replayed.contains("# Skill:"), "{replayed}");
    assert!(!replayed.contains("Review workflow body"), "{replayed}");
    assert!(!replayed.contains("Available skills:"), "{replayed}");
    let _ = fs::remove_dir_all(transcript_root);
}

/// Verifies explicit `$skill` prompts do not allow a model to loop by loading
/// the same skill again.
///
/// A `$create-skill ...` prompt has already loaded the built-in skill into the
/// turn context. If the model responds with `call_skill(create-skill)` instead
/// of requesting a concrete execution capability, the strict request surface
/// should reject the action before runtime skill execution can start another
/// successful continuation.
#[test]
fn runtime_explicit_skill_prompt_rejects_redundant_call_skill_loop() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "$create-skill create a review skill")
        .unwrap();
    service
        .pending_agent_provider_tasks
        .remove(&started.turn_id);
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "load create skill again".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "load skill authoring context".to_string(),
                thought: None,
                turn_id: started.turn_id.clone(),
                agent_id: started.agent_id.clone(),
                actions: vec![mez_agent::AgentAction {
                    id: "skill-loop".to_string(),
                    rationale: "load the create-skill workflow".to_string(),
                    payload: mez_agent::AgentActionPayload::CallSkill {
                        name: "create-skill".to_string(),
                        additional_context: Some("create a review skill".to_string()),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            &started.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(
        !service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == started.turn_id)
    );
    assert!(
        execution
            .response
            .raw_text
            .contains("maap action type call_skill is not allowed"),
        "{}",
        execution.response.raw_text
    );
    assert!(
        execution
            .request
            .allowed_actions
            .action_type_names()
            .contains(&"request_capability")
    );
    assert!(
        !execution
            .request
            .allowed_actions
            .action_type_names()
            .contains(&"call_skill")
    );
    assert!(execution.action_results.is_empty());
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("maap_validation_error"), "{pane_text}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies explicit `$skill` prompts do not need an additional skill catalog
/// lookup before acting on the already-loaded workflow.
///
/// The model-facing action surface suppresses `request_skills` once a full
/// skill body is in context. A provider that still emits the forbidden lookup
/// is rejected at MAAP validation rather than handed to the runtime skill
/// executor as another recoverable lookup.
#[test]
fn runtime_explicit_skill_prompt_rejects_redundant_skill_catalog_lookup() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "$create-skill create a review skill")
        .unwrap();
    service
        .pending_agent_provider_tasks
        .remove(&started.turn_id);
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "request skills again".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "check available skill workflows".to_string(),
                thought: None,
                turn_id: started.turn_id.clone(),
                agent_id: started.agent_id.clone(),
                actions: vec![mez_agent::AgentAction {
                    id: "skill-catalog-loop".to_string(),
                    rationale: "check available skill workflows".to_string(),
                    payload: mez_agent::AgentActionPayload::RequestSkills,
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            &started.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(
        execution
            .response
            .raw_text
            .contains("maap action type request_skills is not allowed"),
        "{}",
        execution.response.raw_text
    );
    assert!(
        !execution
            .request
            .allowed_actions
            .action_type_names()
            .contains(&"request_skills")
    );
    assert!(execution.action_results.is_empty());
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies `/list-skills` displays the effective pane skill catalog with the
/// same `$skill` invocation syntax accepted by explicit skill prompts. This
/// gives users a discoverable way to see and select available workflows before
/// submitting a prompt.
#[test]
fn runtime_agent_shell_list_skills_displays_effective_catalog() {
    let config_root = temp_root("runtime-list-skills");
    let skill_dir = config_root.join("skills/review");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: review\ndescription: Review workflow\n---\n\nCheck tests and risks.\n",
    )
    .unwrap();
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(config_root);

    let response = service
        .execute_agent_shell_command(&primary, "/list-skills")
        .unwrap();

    assert!(response.contains("## Skills"), "{response}");
    assert!(response.contains("Start a prompt with `$`"), "{response}");
    assert!(
        response.contains("`$<skill-name> [additional context]`"),
        "{response}"
    );
    assert!(
        response.contains("| `$create-skill` | user | Create or modify concise Mezzanine skills"),
        "{response}"
    );
    assert!(
        response.contains(
            "| `$add-issues` | user | Use when recent findings should be turned into mez issue tracker entries. |"
        ),
        "{response}"
    );
    assert!(
        response.contains(
            "| `$fix-issues` | user | Use when you need to query the current project's mez issue tracker, fix open issues, keep per-issue plans and progress notes updated, and mark verified fixes resolved. |"
        ),
        "{response}"
    );
    assert!(
        response.contains("| `$review` | user | Review workflow |"),
        "{response}"
    );
}

/// Verifies `/list-skills` shows the built-in skill-authoring workflow when the
/// current pane has no user or trusted-project skills. This makes skill
/// creation discoverable before any external skill directories exist.
#[test]
fn runtime_agent_shell_list_skills_reports_builtin_catalog_without_external_skills() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(temp_root("runtime-list-skills-empty"));

    let response = service
        .execute_agent_shell_command(&primary, "/list-skills")
        .unwrap();

    assert!(
        response.contains("| `$create-skill` | user | Create or modify concise Mezzanine skills"),
        "{response}"
    );
    assert!(
        response.contains(
            "| `$add-issues` | user | Use when recent findings should be turned into mez issue tracker entries. |"
        ),
        "{response}"
    );
    assert!(
        response.contains(
            "| `$fix-issues` | user | Use when you need to query the current project's mez issue tracker, fix open issues, keep per-issue plans and progress notes updated, and mark verified fixes resolved. |"
        ),
        "{response}"
    );
    assert!(
        !response.contains("No skills are currently available."),
        "{response}"
    );
    assert!(response.contains("Start a prompt with `$`"), "{response}");
}

/// Verifies `/sync-builtin-skills` reports managed built-in skill sync results
/// and preserves user overrides in the user configuration root.
#[test]
fn runtime_agent_shell_sync_builtin_skills_reports_user_scope_results() {
    let config_root = temp_root("runtime-sync-builtin-skills");
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_config_root(config_root.clone());
    fs::write(
        config_root.join("skills/create-skill/SKILL.md"),
        "---
name: create-skill
description: Custom skill workflow
---

Keep this override.
",
    )
    .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "/sync-builtin-skills")
        .unwrap();

    assert!(response.contains("## Built-in skill sync"), "{response}");
    assert!(
        response.contains("7 built-in skills checked; 0 changed."),
        "{response}"
    );
    assert!(
        response.contains("| `$create-skill` | preserved-override |"),
        "{response}"
    );
    let override_text =
        fs::read_to_string(config_root.join("skills/create-skill/SKILL.md")).unwrap();
    assert!(override_text.contains("Keep this override."));
}
