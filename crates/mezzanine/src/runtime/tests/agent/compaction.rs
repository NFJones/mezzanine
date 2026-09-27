//! Runtime tests for agent compaction behavior.

use super::*;

/// Verifies the runtime applies raw-retention config for compaction recovery.
///
/// Provider context-limit recovery and manual compaction both use the
/// raw-retention percentage to decide how much exact recent context remains
/// after compaction.
#[test]
fn runtime_config_reload_applies_compaction_raw_retention() {
    let mut service = test_runtime_service();

    assert_eq!(service.agent_compaction_raw_retention_percent(), 10);

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ncompaction_raw_retention_percent = 25\n".to_string(),
        }])
        .unwrap();

    assert_eq!(service.agent_compaction_raw_retention_percent(), 25);
}

/// A provider context rejection must relax optional raw-tail retention before
/// declaring that no closed, consumed history can be compacted.
#[test]
fn runtime_context_limit_recovery_replans_retained_tail_noop() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "retained-tail-recovery".to_string(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"tail-recovery\"\ncompaction_raw_retention_percent = 10\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.tail-recovery]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 40000\n".to_string(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"tail","method":"agent/shell/command","params":{"idempotency_key":"retained-tail-recovery","input":"continue"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    insert_test_context_block(
        service.agent_turn_contexts_mut().get_mut("turn-1").unwrap(),
        ContextBlock::evidence_event(
            ContextSourceKind::ActionResult,
            "synthetic compactable result",
            "result ".repeat(500),
        ),
    );
    let high_water = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .event_sequence_high_water_mark();
    service
        .record_claimed_agent_provider_context_for_tests("turn-1", high_water)
        .unwrap();
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    let budget = 30_000;
    let ordinary = mez_agent::plan_model_context_compaction_at_consumed_sequence(
        context, budget, 10, high_water,
    )
    .unwrap();
    let minimum = mez_agent::plan_model_context_compaction_at_consumed_sequence(
        context, budget, 1, high_water,
    )
    .unwrap();
    assert!(!ordinary.changes_context());
    assert!(minimum.changes_context());
    let before = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks()
        .to_vec();
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    assert!(
        service
            .recover_agent_provider_context_limit_failure(
                &AgentId::opaque("agent-%1").unwrap(),
                "turn-1",
                &error,
                1,
            )
            .unwrap()
    );
    assert_eq!(
        service
            .agent_turn_contexts()
            .get("turn-1")
            .unwrap()
            .blocks(),
        before
    );
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some()
    );
}

/// A previously summarized segment must not mask later closed work behind an
/// exact steering barrier during a subsequent provider context rejection.
#[test]
fn runtime_context_limit_recovery_skips_earlier_summary_only_segment() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "summary-segment-recovery".to_string(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"segment-recovery\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.segment-recovery]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 40000\n".to_string(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"segment","method":"agent/shell/command","params":{"idempotency_key":"summary-segment-recovery","input":"continue"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service
        .agent_turn_contexts_mut()
        .get_mut("turn-1")
        .unwrap()
        .replace_after_compaction(vec![
            ContextBlock::reference_event(
                ContextSourceKind::Memory,
                "context compaction summary",
                "[context compacted]\nEarlier decision summarized.",
            ),
            ContextBlock::user_event("steering", "preserve this instruction exactly"),
            ContextBlock::assistant_event("later decision", "later decision ".repeat(15_000)),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "later result",
                "later result ".repeat(15_000),
            ),
        ])
        .unwrap();
    let high_water = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .event_sequence_high_water_mark();
    service
        .record_claimed_agent_provider_context_for_tests("turn-1", high_water)
        .unwrap();
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    assert!(
        service
            .recover_agent_provider_context_limit_failure(
                &AgentId::opaque("agent-%1").unwrap(),
                "turn-1",
                &error,
                1,
            )
            .unwrap()
    );
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn { plan, .. } =
        &task.target
    else {
        panic!("expected active-turn compaction target");
    };
    assert_eq!(plan.replacement_blocks().len(), 2);
    assert!(
        plan.replacement_blocks()
            .iter()
            .all(|block| block.content.contains("later"))
    );
    complete_runtime_test_compaction(&mut service, "%1", "Later events summarized.");
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert_eq!(
        context.chronology()[0].block().content,
        "[context compacted]\nEarlier decision summarized."
    );
    assert_eq!(
        context.chronology()[1].block().content,
        "preserve this instruction exactly"
    );
    assert_eq!(
        context.chronology()[2].block().content,
        "Later events summarized."
    );
    assert!(service.agent_provider_task_is_pending("turn-1"));
}

/// A pre-summary zero budget must stage separate anchored summaries instead
/// of publishing an earlier summary ahead of exact steering. The first
/// response cannot resume the turn; the second must retain original order.
#[test]
fn runtime_zero_budget_recovery_stages_barrier_separated_segments() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "zero-budget-recovery".to_string(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"zero-budget\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.zero-budget]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 40000\n".to_string(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"zero-budget","method":"agent/shell/command","params":{"idempotency_key":"zero-budget-recovery","input":"continue"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    context
        .replace_after_compaction(vec![
            ContextBlock::assistant_event("first decision", "first ".repeat(200)),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "first outcome",
                "outcome ".repeat(200),
            ),
            ContextBlock::user_event("steering", "preserve this exact instruction"),
            ContextBlock::assistant_event("second decision", "second ".repeat(200)),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "second outcome",
                "result ".repeat(200),
            ),
        ])
        .unwrap();
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    let plan = mez_agent::plan_model_context_compaction_at_consumed_sequence(
        context,
        100,
        1,
        context.event_sequence_high_water_mark(),
    )
    .unwrap();
    assert!(plan.requires_additional_segments());
    let first_summary = "word ".repeat(plan.summary_budget_words());
    let profile = service.agent_turn_model_profile("turn-1").unwrap().clone();
    assert!(
        service
            .queue_agent_context_limit_recovery_compaction(
                "turn-1",
                "zero-budget".to_string(),
                profile,
                1,
                plan,
            )
            .unwrap()
    );
    complete_runtime_test_compaction(&mut service, "%1", &first_summary);
    assert!(!service.agent_provider_task_is_pending("turn-1"));
    let queued = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
        staged, plan, ..
    } = &queued.target
    else {
        panic!("expected staged active-turn compaction")
    };
    assert_eq!(staged.as_ref().unwrap().attempts, 1);
    assert!(
        plan.replacement_blocks()
            .iter()
            .all(|block| block.content.contains("second") || block.content.contains("result"))
    );
    complete_runtime_test_compaction(&mut service, "%1", "Second events summarized.");
    let chronology = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .chronology();
    assert_eq!(chronology[0].block().content, first_summary.trim_end());
    assert_eq!(
        chronology[1].block().content,
        "preserve this exact instruction"
    );
    assert_eq!(chronology[2].block().content, "Second events summarized.");
    assert!(service.agent_provider_task_is_pending("turn-1"));
}

/// Verifies large bracketed-paste agent prompt input is displayed compactly in
/// the pane transcript while the agent turn receives the exact pasted payload.
#[test]
fn runtime_agent_prompt_displays_large_paste_as_compact_block() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-paste-history"));
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );

    let payload = "z".repeat(1229);
    let mut input = Vec::new();
    input.extend_from_slice(b"prefix ");
    input.extend_from_slice(b"\x1b[200~");
    input.extend_from_slice(payload.as_bytes());
    input.extend_from_slice(b"\x1b[201~ suffix\r");
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ForwardToPane(input)],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(
        prompt_state.prompt.buffer.history(),
        &[format!("prefix {payload} suffix")]
    );
    let persisted_history = transcript_store
        .structured_prompt_history("conversation-1")
        .unwrap();
    assert_eq!(persisted_history.len(), 1);
    assert_eq!(
        persisted_history[0].text,
        format!("prefix {payload} suffix")
    );
    assert_eq!(
        persisted_history[0].rendered(),
        "prefix [Pasted 1.2 KiB] suffix"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("user> prefix [Pasted 1.2 KiB] suffix"),
        "{pane_text}"
    );
    assert!(!pane_text.contains(&payload), "{pane_text}");
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains(&format!("prefix {payload} suffix")))
    );
}

/// Verifies a mixed prompt keeps its typed prefix and suffix around one large
/// paste after durable history reload and Up-arrow recall.
///
/// Real terminals deliver typing and bracketed paste as separate input batches.
/// Reloading that submission in a later agent session must reconstruct only the
/// pasted payload as an opaque block; it must not hide or absorb adjacent text.
#[test]
fn runtime_agent_prompt_recalls_mixed_paste_display_after_reload() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-mixed-paste-recall"));
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );

    let payload = "z".repeat(70 * 1024);
    let expected = format!("typed before {payload} typed after");
    let pasted = format!("\x1b[200~{payload}\x1b[201~");
    for input in [
        b"typed before ".as_slice(),
        pasted.as_bytes(),
        b" typed after\r".as_slice(),
    ] {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(input.to_vec())],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    }

    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[A".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    let prompt = &service
        .agent_prompt_inputs_for_tests()
        .get("%1")
        .unwrap()
        .prompt;
    assert_eq!(prompt.buffer.expanded_line(), expected);
    assert_eq!(
        prompt.buffer.rendered_line(),
        "typed before [Pasted 70.0 KiB] typed after"
    );
    assert_eq!(
        prompt.render(),
        "❱ typed before [Pasted 70.0 KiB] typed after"
    );
}

/// Verifies compact pasted placeholders are used for bracketed paste payloads
/// that exceed the visible agent prompt height even when the byte size is small.
///
/// Agent prompt rendering only shows up to six input rows. A seven-line
/// bracketed paste must collapse to the same inline placeholder form as a
/// large byte paste so surrounding prompt text remains editable and readable.
#[test]
fn runtime_agent_prompt_displays_over_height_paste_as_compact_block() {
    let mut service = test_runtime_service_with_size(Size::new(50, 8).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(50, 8).unwrap(), 10).unwrap(),
    );

    let payload = (1..=7)
        .map(|index| format!("tiny-line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut input = Vec::new();
    input.extend_from_slice(b"prefix ");
    input.extend_from_slice(b"\x1b[200~");
    input.extend_from_slice(payload.as_bytes());
    input.extend_from_slice(b"\x1b[201~ suffix\r");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(input)],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("user> prefix [Pasted "), "{pane_text}");
    assert!(pane_text.contains(" suffix"), "{pane_text}");
    assert!(!pane_text.contains("tiny-line-7"), "{pane_text}");
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains(&format!("prefix {payload} suffix")))
    );
}

/// Verifies `/list-modified-files` renders compact modified-file rows.
///
/// Agent mutation previews already show `edited path (+N -M)` style summaries;
/// the slash command should expose the tracked aggregate in the same compact
/// form instead of a verbose nested object list.
#[test]
fn runtime_agent_shell_list_modified_files_reports_compact_rows() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.record_agent_modified_file_delta("%1", "src/lib.rs".to_string(), 12, 3);

    let response = service
        .execute_agent_shell_command(&primary, "/list-modified-files")
        .unwrap();
    assert!(
        response.contains(r#""body":null"#),
        "the deferred lane acknowledges /list-modified-files: {response}"
    );
    let response = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .expect("the deferred /list-modified-files applies its page");

    assert!(response.contains("## modified files"), "{response}");
    assert!(response.contains("edited `src/lib.rs`"), "{response}");
    assert!(
        response.contains(r#"<span class=\"mez-diff-addition\">+12</span>"#),
        "{response}"
    );
    assert!(
        response.contains(r#"<span class=\"mez-diff-deletion\">-3</span>"#),
        "{response}"
    );
    assert!(!response.contains("Added:"), "{response}");
    assert!(!response.contains("Removed:"), "{response}");
    assert!(!response.contains("`summary`"), "{response}");
    assert_eq!(
        crate::runtime::commands::lists::runtime_agent_modified_files_body(None),
        "## modified files\n\nno modified files tracked for this agent conversation."
    );
    assert_eq!(
        crate::runtime::commands::lists::runtime_agent_modified_files_body(Some(
            &std::collections::BTreeMap::new()
        )),
        "## modified files\n\nno modified files tracked for this agent conversation.",
        "an empty tracked map renders the same empty state as no map at all"
    );
}

/// Verifies prompt submission does not run fallback context accounting before
/// appending prompt-derived state.
///
/// Provider responses and provider context-limit errors are the source of truth
/// for context-size handling, so prompt submission must start the turn even when
/// a local estimate would have crossed the model window.
#[test]
fn runtime_agent_prompt_does_not_preflight_compact_before_context_append() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-preflight-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-preflight-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-preflight-test"]
default_model = "gpt-compact-preflight-test"
[model_profiles.compact-preflight-test]
provider = "openai"
model = "gpt-compact-preflight-test"
context_window_tokens = 1024
max_input_tokens = 1
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact-preflight"));
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "as-preflight".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "turn-previous".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: format!("large prior context {}", "context-pressure ".repeat(900)),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 8).unwrap(), 80).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as-preflight", 1)
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, "continue with the next item")
        .unwrap();

    assert!(response.contains(r#""state":"running""#), "{response}");
    assert!(
        !response.contains(r#""kind":"requires_runtime""#),
        "{response}"
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), 1);
    assert_eq!(
        transcript_store.prompt_history("as-preflight").unwrap(),
        vec!["continue with the next item".to_string()]
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("continue with the next item"),
        "{pane_text}"
    );
}

/// Verifies provider context-limit API errors trigger active-turn compaction
/// and retry before the turn is failed.
///
/// The proactive threshold path can miss provider-specific tokenization or
/// hidden request overhead. When the provider rejects the request anyway, the
/// runtime must compact the stored active-turn context before retrying so the
/// same oversized payload is not sent again.
#[test]
fn runtime_provider_context_limit_error_compacts_context_and_retries() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "provider-context-limit-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "provider-context-limit-test"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.provider-context-limit-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-context-limit-recovery","input":"continue with the large observation"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    insert_test_context_block(
        context,
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "synthetic provider-rejected action result".to_string(),
            content: format!("provider-context-limit- {}", "cp ".repeat(10_000)),
        },
    );
    let before_context = context.blocks().to_vec();
    let error = MezError::invalid_state("OpenAI Responses API returned status 400: context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"message":"maximum context length exceeded","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
        );
    let transition = service
        .schedule_agent_provider_retry_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            "turn-1",
            mez_agent::ProviderErrorRetryClass::ContextLimit,
            &error,
        )
        .unwrap()
        .expect("context-limit recovery transition");
    assert!(transition.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::DispatchAgentCompaction { pane_id, .. } if pane_id == "%1"
    )));
    assert_eq!(
        service
            .agent_turn_contexts()
            .get("turn-1")
            .unwrap()
            .blocks(),
        before_context.as_slice()
    );
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some()
    );
    assert!(!service.agent_provider_task_is_pending("turn-1"));
    assert_eq!(service.provider_retry_scheduler_mut().attempt("turn-1"), 1);
    insert_test_context_block(
        service.agent_turn_contexts_mut().get_mut("turn-1").unwrap(),
        ContextBlock::user_event(
            "post-boundary steering",
            "preserve this post-boundary steering exactly",
        ),
    );

    complete_runtime_test_compaction(&mut service, "%1", "model-authored context summary");
    let compacted_context = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(compacted_context.contains("model-authored context summary"));
    assert!(compacted_context.contains("preserve this post-boundary steering exactly"));
    assert!(service.agent_provider_task_is_pending("turn-1"));
    assert_eq!(service.provider_retry_scheduler_mut().attempt("turn-1"), 1);
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries);
    assert!(events.iter().any(|event| {
        event.kind == EventKind::AgentStatus
            && event
                .payload
                .contains(r#""recovery":"provider_context_limit_compaction""#)
    }));
    let trace = service.agent_pane_trace_log_text("%1").unwrap_or_default();
    assert!(
        trace.contains(
            "provider request recovery resuming, reason: provider context limit compaction completed",
        ),
        "{trace}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let pane_text_unwrapped = normalized_pane_log_text(&pane_text);
    assert!(
        pane_text_unwrapped.contains(
            "provider rejected context as too large; requesting model-backed context compaction"
        ),
        "{pane_text}"
    );
}

/// Creates an observed-input compaction task from durable mixed-role history.
///
/// The seeded user instructions are exact history barriers and the current
/// prompt is appended after them. The synthetic response reaches the ordinary
/// continuation boundary so completion exercises the same active-turn refresh
/// and replay-retention path as proactive compaction.
fn queue_observed_input_compaction_with_exact_history() -> (
    crate::runtime::RuntimeSessionService,
    AgentTranscriptStore,
    String,
) {
    queue_observed_input_compaction_with_second_group(None)
}

/// Builds the same exact-barrier fixture with an optional second typed range.
fn queue_observed_input_compaction_with_second_group(
    second_group: Option<String>,
) -> (
    crate::runtime::RuntimeSessionService,
    AgentTranscriptStore,
    String,
) {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "observed-input-exact-history".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "observed-input-exact-history"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.observed-input-exact-history]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
max_input_tokens = 20000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("observed-input-exact-history"));
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let session = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .clone();
    let historical = [
        (
            mez_agent::transcript::TranscriptRole::User,
            "EXACT_OLDER_USER_INSTRUCTION Keep the deployment rollback plan available.".to_string(),
            "historical-turn-1",
        ),
        (
            mez_agent::transcript::TranscriptRole::System,
            mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                ContextSourceKind::TranscriptAssistant,
                "historical answer",
                "TYPED_OLD_WORK ".repeat(80),
                mez_agent::ContextExecutionGroupId::new("historical-group-1").unwrap(),
                1,
                None,
            )
            .unwrap()
            .to_transcript_content(),
            "historical-turn-1",
        ),
        (
            mez_agent::transcript::TranscriptRole::User,
            "EXACT_SECOND_USER_INSTRUCTION Preserve the migration ordering.".to_string(),
            "historical-turn-2",
        ),
        (
            if second_group.is_some() {
                mez_agent::transcript::TranscriptRole::System
            } else {
                mez_agent::transcript::TranscriptRole::Assistant
            },
            second_group.unwrap_or_else(|| "Historical answer two. ".repeat(80)),
            "historical-turn-2",
        ),
        (
            mez_agent::transcript::TranscriptRole::User,
            "EXACT_THIRD_USER_INSTRUCTION Do not remove the compatibility check.".to_string(),
            "historical-turn-3",
        ),
        (
            mez_agent::transcript::TranscriptRole::Assistant,
            "Historical answer three. ".repeat(80),
            "historical-turn-3",
        ),
    ];
    let historical_count = historical.len();
    for (index, (role, content, turn_id)) in historical.into_iter().enumerate() {
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: session.session_id.clone(),
                sequence: u64::try_from(index + 1).unwrap(),
                created_at_unix_seconds: 1,
                role,
                turn_id: turn_id.to_string(),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: content.to_string(),
            })
            .unwrap();
    }
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", historical_count)
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"observed-input-exact-history","method":"agent/shell/command","params":{"idempotency_key":"observed-input-exact-history","input":"CURRENT_USER_PROMPT Continue with the collected evidence."}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service.pending_agent_provider_tasks().remove(0);
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == task.turn_id)
        .cloned()
        .expect("pending provider task owns a running turn");
    insert_test_context_block(
        service
            .agent_turn_contexts_mut()
            .get_mut(&task.turn_id)
            .unwrap(),
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "observed input evidence".to_string(),
            content: "observed-input-exact-history-evidence ".repeat(40),
        },
    );
    let response = runtime_say_response(&task.turn_id, "continue", false);
    let action = response
        .action_batch
        .as_ref()
        .and_then(|batch| batch.actions.first())
        .cloned()
        .expect("continuation response contains a say action");
    service
        .apply_agent_provider_execution(
            &turn,
            &task.model_profile,
            "runtime-batch",
            mez_agent::AgentTurnExecution {
                request: runtime_model_request_fixture_for_agent(&task.turn_id, &task.agent_id),
                response,
                latest_response_usage: mez_agent::ModelTokenUsage {
                    input_tokens: 20000,
                    output_tokens: 1,
                    reasoning_tokens: 0,
                    cached_input_tokens: Some(20),
                    cache_write_input_tokens: None,
                },
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                action_results: vec![mez_agent::ActionResult::succeeded(
                    &turn,
                    &action,
                    vec!["continue".to_string()],
                    None,
                )],
                final_turn: false,
                terminal_state: AgentTurnState::Running,
            },
        )
        .unwrap();
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some(),
        "observed input should queue proactive compaction"
    );
    (service, transcript_store, turn.turn_id)
}

/// Verifies active-turn compaction refresh preserves the exact current prompt
/// when compaction shrinks the imported durable-history prefix.
///
/// The imported event count is captured before compaction. Reusing that count
/// after selected historical groups collapse to one summary can make the
/// refresh predicate claim the current prompt as imported history and replace
/// it with the shortened durable transcript.
#[test]
fn runtime_observed_compaction_refresh_preserves_current_user_prompt() {
    let (mut service, transcript_store, turn_id) =
        queue_observed_input_compaction_with_exact_history();
    let queued = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn { plan, .. } =
        &queued.target
    else {
        panic!("expected active-turn compaction");
    };
    assert!(
        plan.replacement_blocks()
            .iter()
            .any(|block| block.content.contains("TYPED_OLD_WORK")),
        "selected={:?}",
        plan.replacement_blocks()
    );
    assert_eq!(
        plan.replacement_blocks().len(),
        1,
        "selected={:?}",
        plan.replacement_blocks()
    );
    let session = service
        .agent_shell_store()
        .get("%1")
        .expect("active agent shell session");
    let conversation_id = session.session_id.clone();
    let next_sequence = transcript_store
        .inspect(&conversation_id)
        .unwrap()
        .last()
        .expect("seeded and prompt transcript entries")
        .sequence
        .saturating_add(1);
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: conversation_id.clone(),
            sequence: next_sequence,
            created_at_unix_seconds: 2,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "late-active-turn-transcript".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "LATE_ACTIVE_TURN_TRANSCRIPT_ENTRY".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();
    complete_runtime_test_compaction(
        &mut service,
        "%1",
        "observed input summary without exact user instructions",
    );
    let epoch = transcript_store
        .compaction_epoch(&conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        epoch.version,
        2,
        "epoch={epoch:?}; pane={}",
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
    );
    assert_eq!(epoch.ranges.len(), 1);
    let context = service
        .agent_turn_contexts()
        .get(&turn_id)
        .unwrap_or_else(|| {
            panic!(
                "the running turn retains its refreshed context; pane status: {}",
                service
                    .pane_screen("%1")
                    .unwrap()
                    .normal_content_lines()
                    .join("\\n")
            )
        });
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content == "observed input summary without exact user instructions")
    );
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.content.contains("TYPED_OLD_WORK"))
    );
    assert!(
        context.blocks().iter().any(|block| {
            block.label == "user prompt"
                && block.content == "CURRENT_USER_PROMPT Continue with the collected evidence."
        }),
        "the active prompt must remain exact after compaction refresh: {:#?}",
        context.blocks()
    );
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains("LATE_ACTIVE_TURN_TRANSCRIPT_ENTRY")),
        "post-plan transcript entries must remain in the refreshed live context: {:#?}",
        context.blocks()
    );
}

/// Verifies proactive compaction keeps exact user instructions that were not
/// included in the model-authored summary available to the next turn.
///
/// Durable replay must represent the same selected ranges as the live
/// compacted context. Retaining only the newest transcript count is not valid
/// when exact barriers inside the selected history were intentionally omitted
/// from summary input.
#[test]
fn runtime_observed_compaction_retains_unsummarized_exact_history_for_replay() {
    let (mut service, _transcript_store, _turn_id) =
        queue_observed_input_compaction_with_exact_history();
    complete_runtime_test_compaction(
        &mut service,
        "%1",
        "observed input summary without exact user instructions",
    );
    let next_context = service
        .agent_context_for_pane_prompt("%1", "NEXT_USER_PROMPT Continue.", 0)
        .unwrap();
    assert!(
        next_context
            .blocks()
            .iter()
            .any(|block| block.content == "observed input summary without exact user instructions")
    );
    assert!(
        !next_context
            .blocks()
            .iter()
            .any(|block| block.content.contains("TYPED_OLD_WORK"))
    );
    assert!(
        [
            "EXACT_OLDER_USER_INSTRUCTION",
            "EXACT_SECOND_USER_INSTRUCTION",
            "EXACT_THIRD_USER_INSTRUCTION",
        ]
        .iter()
        .all(|marker| {
            next_context
                .blocks()
                .iter()
                .any(|block| block.content.contains(marker))
        }),
        "every exact historical instruction omitted from summary input must remain replayable: {:#?}",
        next_context.blocks()
    );
}

/// A still-oversized refreshed request retries a smaller model-authored
/// summary without publishing the first candidate or repeating settled work.
#[test]
fn runtime_observed_compaction_retries_oversized_final_request() {
    let (mut service, store, turn_id) = queue_observed_input_compaction_with_exact_history();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let initial_budget = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .request
        .max_output_tokens;
    let mut profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
    profile
        .provider_options
        .insert("max_input_tokens".to_string(), "17000".to_string());
    service.set_agent_turn_model_profile(turn_id.clone(), profile);
    complete_runtime_test_compaction(&mut service, "%1", &"large-summary ".repeat(900));
    assert!(store.compaction_epoch(&conversation_id).unwrap().is_none());
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| { turn.turn_id == turn_id && turn.state == AgentTurnState::Running }),
        "budget={initial_budget:?} pane={}",
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
    );
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .expect("oversized final request must queue another model summary");
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
        final_request_retry,
        ..
    } = &retry.target
    else {
        panic!("expected active turn retry")
    };
    assert_eq!(final_request_retry.attempts, 1);
    assert!(retry.request.max_output_tokens.unwrap() < initial_budget.unwrap());
    complete_runtime_test_compaction(&mut service, "%1", "shorter final summary");
    let epoch = store.compaction_epoch(&conversation_id).unwrap().unwrap();
    assert_eq!(epoch.ranges.len(), 1);
    assert_eq!(epoch.ranges[0].summary, "shorter final summary");
    assert!(service.agent_provider_task_is_pending(&turn_id));
    let next = service
        .agent_context_for_pane_prompt("%1", "next prompt", 0)
        .unwrap();
    assert!(
        !next
            .blocks()
            .iter()
            .any(|block| block.content.contains("TYPED_OLD_WORK"))
    );
}

/// A second closed segment remains provisional until both anchored summaries
/// fit the complete refreshed request and can publish as one epoch.
#[test]
fn runtime_observed_compaction_stages_second_range_before_publication() {
    let second = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
        ContextSourceKind::TranscriptAssistant,
        "second answer",
        "SECOND_RANGE_SOURCE ".repeat(1_200),
        mez_agent::ContextExecutionGroupId::new("historical-group-2").unwrap(),
        1,
        None,
    )
    .unwrap()
    .to_transcript_content();
    let (mut service, store, turn_id) =
        queue_observed_input_compaction_with_second_group(Some(second));
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let mut profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
    profile
        .provider_options
        .insert("max_input_tokens".to_string(), "17000".to_string());
    service.set_agent_turn_model_profile(turn_id.clone(), profile);
    complete_runtime_test_compaction(&mut service, "%1", "first summary");
    assert!(store.compaction_epoch(&conversation_id).unwrap().is_none());
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .expect("a second closed range should be queued");
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
        staged, plan, ..
    } = &retry.target
    else {
        panic!("expected staged active-turn compaction")
    };
    assert_eq!(staged.as_ref().unwrap().attempts, 1);
    assert!(
        plan.replacement_blocks()
            .iter()
            .any(|block| block.content.contains("SECOND_RANGE_SOURCE"))
    );
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn_id)
        .unwrap()
        .append_user_event("user steering 1", "EXACT_STAGED_STEERING")
        .unwrap();
    complete_runtime_test_compaction(&mut service, "%1", "second summary");
    let epoch = store.compaction_epoch(&conversation_id).unwrap().unwrap();
    assert_eq!(epoch.ranges.len(), 2);
    assert_eq!(epoch.ranges[0].summary, "first summary");
    assert_eq!(epoch.ranges[1].summary, "second summary");
    assert!(service.agent_provider_task_is_pending(&turn_id));
    assert!(
        service
            .agent_turn_contexts()
            .get(&turn_id)
            .unwrap()
            .blocks()
            .iter()
            .any(|block| { block.content == "EXACT_STAGED_STEERING" })
    );
    let next = service
        .agent_context_for_pane_prompt("%1", "next", 0)
        .unwrap();
    assert!(
        !next
            .blocks()
            .iter()
            .any(|block| block.content.contains("SECOND_RANGE_SOURCE"))
    );
}

/// A second independently queued observed-input compaction extends a prior
/// selective epoch without requiring the previous task's frozen source rows.
/// Each completion publishes only its newly selected durable range.
#[test]
fn runtime_observed_compaction_extends_previous_selective_epoch() {
    let second = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
        ContextSourceKind::TranscriptAssistant,
        "second answer",
        "SECOND_RANGE_SOURCE ".repeat(80),
        mez_agent::ContextExecutionGroupId::new("historical-group-2").unwrap(),
        1,
        None,
    )
    .unwrap()
    .to_transcript_content();
    let (mut service, store, turn_id) =
        queue_observed_input_compaction_with_second_group(Some(second));
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    complete_runtime_test_compaction(&mut service, "%1", "first summary");
    let first = store.compaction_epoch(&conversation_id).unwrap().unwrap();
    assert_eq!(first.ranges.len(), 1);
    let context = service.agent_turn_contexts().get(&turn_id).unwrap();
    let plan = mez_agent::plan_model_context_compaction_for_provider_tokens(
        context,
        10_000,
        1,
        context.event_sequence_high_water_mark(),
        mez_agent::ProviderBudgetProjection::new(
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
            "runtime-batch",
        ),
    )
    .unwrap();
    assert!(
        plan.replacement_blocks()
            .iter()
            .any(|block| block.content.contains("SECOND_RANGE_SOURCE"))
    );
    let profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
    assert!(service.queue_agent_active_turn_compaction(
        &turn_id,
        "observed-input-exact-history".to_string(),
        profile,
        crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
            observed_input_tokens: 20_000,
            max_input_tokens: 20_000,
        },
        plan,
    ).unwrap());
    complete_runtime_test_compaction(&mut service, "%1", "second summary");
    let final_epoch = store.compaction_epoch(&conversation_id).unwrap().unwrap();
    assert_eq!(final_epoch.ranges.len(), 2);
    assert_eq!(final_epoch.ranges[0], first.ranges[0]);
    assert_eq!(final_epoch.ranges[1].summary, "second summary");
    assert!(service.agent_provider_task_is_pending(&turn_id));
}

/// A zero first-segment allowance must stage both durable ranges before the
/// observed-input continuation, retaining exact steering in replay order.
#[test]
fn runtime_observed_zero_budget_stages_durable_ranges() {
    let second = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
        ContextSourceKind::TranscriptAssistant,
        "second answer",
        "SECOND_RANGE_SOURCE ".repeat(1_200),
        mez_agent::ContextExecutionGroupId::new("historical-group-2").unwrap(),
        1,
        None,
    )
    .unwrap()
    .to_transcript_content();
    let (mut service, store, turn_id) =
        queue_observed_input_compaction_with_second_group(Some(second));
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let queued = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap()
        .clone();
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn { trigger, .. } =
        queued.target
    else {
        panic!("expected observed-input active turn")
    };
    let context = service.agent_turn_contexts().get(&turn_id).unwrap();
    let projection = mez_agent::ProviderBudgetProjection::new(
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        "runtime-batch",
    );
    let plan = (1..20_000)
        .rev()
        .find_map(|budget| {
            mez_agent::plan_model_context_compaction_for_provider_tokens(
                context,
                budget,
                1,
                context.event_sequence_high_water_mark(),
                projection,
            )
            .ok()
            .filter(|plan| plan.requires_additional_segments())
        })
        .expect("two eligible durable segments must recover a zero first allowance");
    let original = context.clone();
    service.fail_current_agent_compaction_task("%1");
    assert!(
        service
            .queue_agent_active_turn_compaction(
                &turn_id,
                queued.model_profile_name,
                queued.model_profile,
                trigger,
                plan,
            )
            .unwrap()
    );
    complete_runtime_test_compaction(&mut service, "%1", "x");
    assert!(store.compaction_epoch(&conversation_id).unwrap().is_none());
    assert_eq!(service.agent_turn_contexts().get(&turn_id), Some(&original));
    assert!(!service.agent_provider_task_is_pending(&turn_id));
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .expect("later durable segment queued");
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
        staged: Some(staged),
        ..
    } = &retry.target
    else {
        panic!("first summary must remain provisional")
    };
    assert_eq!(staged.attempts, 1);
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn_id)
        .unwrap()
        .append_user_event("late steering", "EXACT_LATE_STEERING")
        .unwrap();
    complete_runtime_test_compaction(&mut service, "%1", "x");
    let epoch = store.compaction_epoch(&conversation_id).unwrap().unwrap();
    assert_eq!(epoch.ranges.len(), 2);
    assert_eq!(epoch.ranges[0].summary, "x");
    assert_eq!(epoch.ranges[1].summary, "x");
    assert!(service.agent_provider_task_is_pending(&turn_id));
    assert!(
        service
            .agent_turn_contexts()
            .get(&turn_id)
            .unwrap()
            .blocks()
            .iter()
            .any(|block| block.content == "EXACT_LATE_STEERING")
    );
    let next = service
        .agent_context_for_pane_prompt("%1", "next", 0)
        .unwrap();
    assert!(
        next.blocks()
            .iter()
            .any(|block| block.content.contains("EXACT_SECOND_USER_INSTRUCTION"))
    );
    assert!(
        !next
            .blocks()
            .iter()
            .any(|block| block.content.contains("SECOND_RANGE_SOURCE"))
    );
}

/// An oversized second range cannot publish the earlier staged summary or
/// leave a shortened raw replay suffix after recovery fails.
#[test]
fn runtime_observed_compaction_second_range_failure_keeps_original_epoch() {
    let second = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
        ContextSourceKind::TranscriptAssistant,
        "second answer",
        "SECOND_RANGE_SOURCE ".repeat(1_200),
        mez_agent::ContextExecutionGroupId::new("historical-group-2").unwrap(),
        1,
        None,
    )
    .unwrap()
    .to_transcript_content();
    let (mut service, store, turn_id) =
        queue_observed_input_compaction_with_second_group(Some(second));
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let mut profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
    profile
        .provider_options
        .insert("max_input_tokens".to_string(), "17000".to_string());
    service.set_agent_turn_model_profile(turn_id.clone(), profile);
    complete_runtime_test_compaction(&mut service, "%1", "first summary");
    assert!(store.compaction_epoch(&conversation_id).unwrap().is_none());
    let source = "second oversized summary ".repeat(850);
    let steering = "EXACT_OVERSIZED_STAGED_STEERING ".repeat(2_000);
    service
        .agent_turn_contexts_mut()
        .get_mut(&turn_id)
        .unwrap()
        .append_user_event("user steering 1", steering.clone())
        .unwrap();
    assert!(
        service
            .agent_turn_contexts()
            .get(&turn_id)
            .unwrap()
            .blocks()
            .iter()
            .any(|block| block.content == steering)
    );
    complete_runtime_test_compaction(&mut service, "%1", &source);
    assert!(store.compaction_epoch(&conversation_id).unwrap().is_none());
    if service
        .pending_agent_compaction_task_for_tests("%1")
        .is_some()
    {
        complete_runtime_test_compaction(&mut service, "%1", &source);
    }
    assert!(store.compaction_epoch(&conversation_id).unwrap().is_none());
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| { turn.turn_id == turn_id && turn.state == AgentTurnState::Failed })
    );
    assert!(
        store
            .inspect(&conversation_id)
            .unwrap()
            .iter()
            .any(|row| { row.content.contains("SECOND_RANGE_SOURCE") })
    );
    assert!(service.agent_turn_contexts().get(&turn_id).is_none());
}

/// A second summary that makes no progress cannot cause an unbounded loop or
/// publish either oversized candidate to durable replay.
#[test]
fn runtime_observed_compaction_rejects_non_reducing_final_retry() {
    let (mut service, store, turn_id) = queue_observed_input_compaction_with_exact_history();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let mut profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
    profile
        .provider_options
        .insert("max_input_tokens".to_string(), "17000".to_string());
    service.set_agent_turn_model_profile(turn_id.clone(), profile);
    let oversized = "large-summary ".repeat(900);
    complete_runtime_test_compaction(&mut service, "%1", &oversized);
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some()
    );
    complete_runtime_test_compaction(&mut service, "%1", &oversized);
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_none()
    );
    assert!(store.compaction_epoch(&conversation_id).unwrap().is_none());
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| { turn.turn_id == turn_id && turn.state == AgentTurnState::Failed })
    );
}

/// Verifies transcript rows appended after compaction planning stay in raw
/// replay without displacing exact history that the plan deliberately retained.
#[test]
fn runtime_observed_compaction_preserves_history_when_transcript_arrives_after_queue() {
    let (mut service, transcript_store, _) = queue_observed_input_compaction_with_exact_history();
    let session = service
        .agent_shell_store()
        .get("%1")
        .expect("active agent shell session");
    let conversation_id = session.session_id.clone();
    let next_sequence = transcript_store
        .inspect(&conversation_id)
        .unwrap()
        .last()
        .expect("seeded and prompt transcript entries")
        .sequence
        .saturating_add(1);
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id,
            sequence: next_sequence,
            created_at_unix_seconds: 2,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "late-transcript-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "LATE_POST_PLAN_TRANSCRIPT_ENTRY".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();

    complete_runtime_test_compaction(
        &mut service,
        "%1",
        "observed input summary without exact user instructions",
    );
    let next_context = service
        .agent_context_for_pane_prompt("%1", "NEXT_USER_PROMPT Continue.", 0)
        .unwrap();
    let replay = next_context
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        replay.contains("LATE_POST_PLAN_TRANSCRIPT_ENTRY"),
        "{replay}"
    );
    assert!(replay.contains("EXACT_OLDER_USER_INSTRUCTION"), "{replay}");
    assert!(replay.contains("EXACT_SECOND_USER_INSTRUCTION"), "{replay}");
    assert!(replay.contains("EXACT_THIRD_USER_INSTRUCTION"), "{replay}");
}

/// A late raw transcript row can overfill only the refreshed projection; the
/// first summary must not commit while a shorter summary can still recover.
#[test]
fn runtime_observed_compaction_retries_refreshed_only_overflow() {
    let (mut service, store, turn_id) = queue_observed_input_compaction_with_exact_history();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let sequence = store
        .inspect(&conversation_id)
        .unwrap()
        .last()
        .unwrap()
        .sequence
        + 1;
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: conversation_id.clone(),
            sequence,
            created_at_unix_seconds: sequence,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "late-raw".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "late raw evidence ".repeat(200),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();
    let mut profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
    profile
        .provider_options
        .insert("max_input_tokens".to_string(), "17000".to_string());
    service.set_agent_turn_model_profile(turn_id.clone(), profile);
    complete_runtime_test_compaction(&mut service, "%1", &"large-summary ".repeat(600));
    assert!(store.compaction_epoch(&conversation_id).unwrap().is_none());
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some(),
        "pane={}",
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
    );
    complete_runtime_test_compaction(&mut service, "%1", "shorter summary");
    assert!(service.agent_provider_task_is_pending(&turn_id));
    let next = service
        .agent_context_for_pane_prompt("%1", "next", 0)
        .unwrap();
    assert!(
        next.blocks()
            .iter()
            .any(|block| block.content.contains("late raw evidence"))
    );
    assert!(
        !next
            .blocks()
            .iter()
            .any(|block| block.content.contains("TYPED_OLD_WORK"))
    );
}

/// Verifies oversized transcript history arriving after observed-input
/// compaction was queued is included in the final request check before commit.
#[test]
fn runtime_observed_input_limit_rejects_late_oversized_transcript_without_commit() {
    let (mut service, transcript_store, turn_id) =
        queue_observed_input_compaction_with_exact_history();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .expect("active agent shell session")
        .session_id
        .clone();
    let sequence = transcript_store
        .inspect(&conversation_id)
        .unwrap()
        .last()
        .expect("seeded transcript history")
        .sequence
        .saturating_add(1);
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: conversation_id.clone(),
            sequence,
            created_at_unix_seconds: 2,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "late-oversized-transcript-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "late-oversized-transcript-token ".repeat(21_000),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();

    complete_runtime_test_compaction(&mut service, "%1", "small observed input summary");

    assert_eq!(
        transcript_store.compaction_epoch(&conversation_id).unwrap(),
        None
    );
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Failed)
    );
}

/// Verifies observed-input compaction rejects a candidate that fits the cap
/// but does not strictly shrink the triggering request estimate.
#[test]
fn runtime_observed_input_limit_rejects_non_reducing_candidate_without_commit() {
    let (mut service, transcript_store, turn_id) =
        queue_observed_input_compaction_with_exact_history();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .expect("active agent shell session")
        .session_id
        .clone();
    let queued = service
        .pending_agent_compaction_task_mut_for_tests("%1")
        .expect("observed-input compaction task");
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
        trigger:
            crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                observed_input_tokens,
                ..
            },
        ..
    } = &mut queued.target
    else {
        panic!("expected observed-input-limit active-turn compaction");
    };
    *observed_input_tokens = 1;

    complete_runtime_test_compaction(&mut service, "%1", "small observed input summary");

    assert_eq!(
        transcript_store.compaction_epoch(&conversation_id).unwrap(),
        None
    );
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Failed)
    );
    assert!(service.agent_turn_contexts().get(&turn_id).is_none());
}

/// Verifies observed-input compaction refuses an oversized exact next request.
///
/// A provider-reported threshold queues compaction, but the summary and retained
/// context must still fit the configured cap before either the live turn context
/// or durable transcript epoch is committed.
#[test]
fn runtime_observed_input_limit_rejects_over_cap_candidate_without_commit() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("observed-input-deferred-first-turn"));
    service.set_agent_transcript_store(store.clone());
    service.use_transcript_effect_adapter();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "observed-input-limit".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "observed-input-limit"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.observed-input-limit]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
max_input_tokens = 100
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"observed-input-limit","method":"agent/shell/command","params":{"idempotency_key":"observed-input-limit","input":"continue with the collected evidence"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service.pending_agent_provider_tasks().remove(0);
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == task.turn_id)
        .cloned()
        .expect("pending provider task owns a running turn");
    insert_test_context_block(
        service
            .agent_turn_contexts_mut()
            .get_mut(&task.turn_id)
            .unwrap(),
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "observed input evidence".to_string(),
            content: "observed-input-marker ".repeat(500),
        },
    );
    let response = runtime_say_response(&task.turn_id, "continue", false);
    let action = response
        .action_batch
        .as_ref()
        .and_then(|batch| batch.actions.first())
        .cloned()
        .expect("continuation response contains a say action");
    service
        .apply_agent_provider_execution(
            &turn,
            &task.model_profile,
            "runtime-batch",
            mez_agent::AgentTurnExecution {
                request: runtime_model_request_fixture_for_agent(&task.turn_id, &task.agent_id),
                response,
                latest_response_usage: mez_agent::ModelTokenUsage {
                    input_tokens: 100,
                    output_tokens: 1,
                    reasoning_tokens: 0,
                    cached_input_tokens: Some(20),
                    cache_write_input_tokens: None,
                },
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                action_results: vec![mez_agent::ActionResult::succeeded(
                    &turn,
                    &action,
                    vec!["continue".to_string()],
                    None,
                )],
                final_turn: false,
                terminal_state: AgentTurnState::Running,
            },
        )
        .unwrap();
    assert!(!service.agent_provider_task_is_pending(&task.turn_id));
    let queued = service
        .pending_agent_compaction_task_for_tests("%1")
        .expect("observed threshold queues compaction at continuation boundary");
    assert_eq!(queued.source, "observed-input-limit");
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
        trigger:
            crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                observed_input_tokens,
                max_input_tokens,
            },
        ..
    } = &queued.target
    else {
        panic!("expected observed-input-limit active-turn compaction");
    };
    assert_eq!(*observed_input_tokens, 100);
    assert_eq!(*max_input_tokens, 100);

    complete_runtime_test_compaction(&mut service, "%1", "observed input summary");
    assert!(!service.agent_provider_task_is_pending(&task.turn_id));
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| { turn.turn_id == task.turn_id && turn.state == AgentTurnState::Failed })
    );
    let conversation_id = &turn.conversation_id;
    assert_eq!(store.compaction_epoch(conversation_id).unwrap(), None);
    let pending_transcript = service
        .persistence
        .pending_transcript_entries(conversation_id);
    assert!(
        !pending_transcript
            .iter()
            .any(|entry| entry.content.contains("observed input summary"))
    );
    assert!(
        !pending_transcript
            .iter()
            .any(|entry| entry.content.contains("mcp_compaction_epoch"))
    );
}

/// An observed first-turn input overflow can summarize closed live work even
/// before the transcript worker commits its first archive. Recovery queues one
/// continuation but leaves the original append-only history authoritative.
#[test]
fn runtime_observed_first_turn_without_archive_recovers_turn_locally() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("observed-first-turn-local"));
    service.set_agent_transcript_store(store.clone());
    service.use_transcript_effect_adapter();
    service.replace_config_layers(vec![ConfigLayer {
        name: "observed-first-turn-local".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"first-turn-local\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.first-turn-local]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 40000\nmax_input_tokens = 20000\n".to_string(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"first-local","method":"agent/shell/command","params":{"idempotency_key":"first-local","input":"continue with the collected evidence"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service.pending_agent_provider_tasks().remove(0);
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == task.turn_id)
        .unwrap()
        .clone();
    insert_test_context_block(
        service
            .agent_turn_contexts_mut()
            .get_mut(&task.turn_id)
            .unwrap(),
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "first-turn evidence".to_string(),
            content: "first-turn-evidence ".repeat(500),
        },
    );
    let response = runtime_say_response(&task.turn_id, "continue", false);
    let action = response
        .action_batch
        .as_ref()
        .unwrap()
        .actions
        .first()
        .unwrap()
        .clone();
    service
        .apply_agent_provider_execution(
            &turn,
            &task.model_profile,
            "runtime-batch",
            mez_agent::AgentTurnExecution {
                request: runtime_model_request_fixture_for_agent(&task.turn_id, &task.agent_id),
                response,
                latest_response_usage: mez_agent::ModelTokenUsage {
                    input_tokens: 20_000,
                    output_tokens: 1,
                    reasoning_tokens: 0,
                    cached_input_tokens: Some(20),
                    cache_write_input_tokens: None,
                },
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                action_results: vec![mez_agent::ActionResult::succeeded(
                    &turn,
                    &action,
                    vec!["continue".to_string()],
                    None,
                )],
                final_turn: false,
                terminal_state: AgentTurnState::Running,
            },
        )
        .unwrap();
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some()
    );
    assert!(
        !store
            .transcript_path(&turn.conversation_id)
            .unwrap()
            .exists()
    );
    complete_runtime_test_compaction(&mut service, "%1", "closed evidence summarized");
    assert_eq!(store.compaction_epoch(&turn.conversation_id).unwrap(), None);
    assert!(
        !store
            .transcript_path(&turn.conversation_id)
            .unwrap()
            .exists()
    );
    assert!(service.agent_provider_task_is_pending(&turn.turn_id));
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    assert!(
        service
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .unwrap()
            .blocks()
            .iter()
            .any(|block| block.content == "closed evidence summarized")
    );
    assert!(
        service
            .persistence
            .pending_transcript_entries(&turn.conversation_id)
            .iter()
            .all(|entry| !entry.content.contains("closed evidence summarized"))
    );
}

/// A first-turn zero-budget plan stages closed segments across exact steering
/// without inventing durable ranges; only the final complete request resumes.
#[test]
fn runtime_observed_first_turn_stages_uncommitted_ranges_locally() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("first-turn-staged-local"));
    service.set_agent_transcript_store(store.clone());
    service.use_transcript_effect_adapter();
    service.replace_config_layers(vec![ConfigLayer {
        name: "first-turn-staged-local".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"first-turn-staged\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.first-turn-staged]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 40000\nmax_input_tokens = 20000\n".to_string(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"first-staged","method":"agent/shell/command","params":{"idempotency_key":"first-staged","input":"continue"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    context
        .replace_after_compaction(vec![
            ContextBlock::assistant_event("first decision", "first ".repeat(200)),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "first outcome",
                "outcome ".repeat(200),
            ),
            ContextBlock::user_event("steering", "preserve this exact instruction"),
            ContextBlock::assistant_event("second decision", "second ".repeat(200)),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "second outcome",
                "result ".repeat(200),
            ),
        ])
        .unwrap();
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    let plan = mez_agent::plan_model_context_compaction_for_provider_tokens(
        context,
        100,
        1,
        context.event_sequence_high_water_mark(),
        mez_agent::ProviderBudgetProjection::new(
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
            "runtime-batch",
        ),
    )
    .unwrap();
    assert!(plan.requires_additional_segments());
    let original = context.clone();
    let profile = service.agent_turn_model_profile("turn-1").unwrap().clone();
    assert!(service.queue_agent_active_turn_compaction(
        "turn-1", "first-turn-staged".to_string(), profile,
        crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
            observed_input_tokens: 20_000, max_input_tokens: 20_000,
        }, plan,
    ).unwrap());
    complete_runtime_test_compaction(&mut service, "%1", "x");
    assert!(
        store
            .compaction_epoch(
                service
                    .agent_shell_store()
                    .get("%1")
                    .unwrap()
                    .session_id
                    .as_str()
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(service.agent_turn_contexts().get("turn-1"), Some(&original));
    assert!(!service.agent_provider_task_is_pending("turn-1"));
    let queued = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
        staged: Some(staged),
        ..
    } = &queued.target
    else {
        panic!("second closed segment must be staged");
    };
    assert!(staged.projection.is_none());
    complete_runtime_test_compaction(&mut service, "%1", "x");
    assert!(service.agent_provider_task_is_pending("turn-1"));
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    let chronology = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .chronology();
    assert_eq!(chronology[0].block().content, "x");
    assert_eq!(
        chronology[1].block().content,
        "preserve this exact instruction"
    );
    assert_eq!(chronology[2].block().content, "x");
    assert!(
        store
            .compaction_epoch(
                service
                    .agent_shell_store()
                    .get("%1")
                    .unwrap()
                    .session_id
                    .as_str()
            )
            .unwrap()
            .is_none()
    );
}

/// Verifies an observed-input recovery failure identifies its actual trigger.
///
/// A provider failure while producing the compaction summary must fail the
/// waiting turn without misreporting an output-limit compaction. The observed
/// input count remains the last successful usage sample for the display, while
/// the terminal provider diagnostic names only the content-free trigger.
#[test]
fn runtime_observed_input_limit_compaction_failure_keeps_its_trigger() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "observed-input-limit-failure".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "observed-input-limit-failure"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.observed-input-limit-failure]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
max_input_tokens = 100
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"observed-input-limit-failure","method":"agent/shell/command","params":{"idempotency_key":"observed-input-limit-failure","input":"continue with the collected evidence"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service.pending_agent_provider_tasks().remove(0);
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == task.turn_id)
        .cloned()
        .expect("pending provider task owns a running turn");
    insert_test_context_block(
        service
            .agent_turn_contexts_mut()
            .get_mut(&task.turn_id)
            .unwrap(),
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "observed input evidence".to_string(),
            content: "observed-input-failure-marker ".repeat(500),
        },
    );
    let response = runtime_say_response(&task.turn_id, "continue", false);
    let action = response
        .action_batch
        .as_ref()
        .and_then(|batch| batch.actions.first())
        .cloned()
        .expect("continuation response contains a say action");
    service
        .apply_agent_provider_execution(
            &turn,
            &task.model_profile,
            "runtime-batch",
            mez_agent::AgentTurnExecution {
                request: runtime_model_request_fixture_for_agent(&task.turn_id, &task.agent_id),
                response,
                latest_response_usage: mez_agent::ModelTokenUsage {
                    input_tokens: 100,
                    output_tokens: 1,
                    reasoning_tokens: 0,
                    cached_input_tokens: Some(20),
                    cache_write_input_tokens: None,
                },
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                action_results: vec![mez_agent::ActionResult::succeeded(
                    &turn,
                    &action,
                    vec!["continue".to_string()],
                    None,
                )],
                final_turn: false,
                terminal_state: AgentTurnState::Running,
            },
        )
        .unwrap();
    let task = service
        .take_pending_agent_compaction_task("%1")
        .expect("observed input queues active-turn compaction");
    assert_eq!(task.source, "observed-input-limit");
    service.claim_agent_compaction_task_state("%1", task);
    assert!(
        service
            .apply_agent_compaction_failed_event(
                "%1",
                "forbidden",
                "provider authentication rejected the compaction request",
                Some(r#"{"status_code":401,"error":{"code":"invalid_api_key"}}"#),
            )
            .unwrap()
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let failure = normalized_pane_log_text(&pane_text);
    assert!(
        failure.contains("automatic observed-input-limit compaction failed before provider retry"),
        "{}",
        pane_text
    );
    assert!(
        !failure.contains("automatic output-limit compaction"),
        "{}",
        pane_text
    );
}

/// Verifies a high execution sample still defers continuation when an action
/// settles after the initial provider response.
///
/// Deferred shell, approval, network, and MCP actions enqueue continuation
/// later than response application. The provider-task claim boundary must
/// therefore re-check the stored execution sample before provider I/O.
#[test]
fn runtime_observed_input_limit_compacts_before_deferred_action_continuation_claim() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "observed-input-deferred-continuation".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "observed-input-limit"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.observed-input-limit]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
max_input_tokens = 100
"#
            .to_string(),
        }])
        .unwrap();
    let auth_root = temp_root("observed-input-deferred-continuation-auth");
    service.set_auth_store(AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&auth_root),
    ));
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"observed-input-deferred","method":"agent/shell/command","params":{"idempotency_key":"observed-input-deferred","input":"run the deferred evidence command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service.pending_agent_provider_tasks().remove(0);
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == task.turn_id)
        .cloned()
        .expect("pending provider task owns a running turn");
    insert_test_context_block(
        service
            .agent_turn_contexts_mut()
            .get_mut(&task.turn_id)
            .unwrap(),
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "deferred input evidence".to_string(),
            content: "deferred-observed-input-marker ".repeat(500),
        },
    );
    let action = mez_agent::AgentAction {
        id: "shell-1".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Collect deferred evidence".to_string(),
            command: "printf deferred".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let response = mez_agent::ModelResponse {
        provider: "runtime-batch".to_string(),
        model: "test".to_string(),
        raw_text: "collect deferred evidence".to_string(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Default::default(),
        action_batch: Some(mez_agent::MaapBatch {
            rationale: "test action batch rationale".to_string(),
            actions: vec![action.clone()],
        }),
        provider_transcript_events: Vec::new(),
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&task.turn_id, &task.agent_id),
        response,
        latest_response_usage: mez_agent::ModelTokenUsage {
            input_tokens: 100,
            output_tokens: 1,
            reasoning_tokens: 0,
            cached_input_tokens: Some(20),
            cache_write_input_tokens: None,
        },
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::running(
            &turn,
            &action,
            vec!["shell action accepted for deferred execution".to_string()],
            None,
        )],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    execution.action_results[0] = mez_agent::ActionResult::succeeded(
        &turn,
        &action,
        vec!["deferred evidence collected".to_string()],
        None,
    );
    service.set_agent_turn_model_profile(task.turn_id.clone(), task.model_profile.clone());
    service
        .agent_turn_executions_mut()
        .insert(task.turn_id.clone(), execution);
    service.queue_agent_provider_task(task.turn_id.clone());
    let agent_id = AgentId::opaque(task.agent_id).unwrap();
    assert!(
        service
            .claim_configured_agent_provider_task(&agent_id, &task.turn_id)
            .unwrap()
            .is_none(),
        "the continuation claim must defer into observed-input compaction"
    );
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some(),
        "the settled deferred action must trigger active-turn compaction"
    );
}

/// Verifies an explicit input cap defers an oversized ordinary provider claim
/// before provider I/O and resumes only after model-backed context compaction.
///
/// Proactive compaction is not provider-error recovery, so it must preserve the
/// pending logical turn without consuming a provider retry attempt. The rebuilt
/// request must pass the same complete-wire preflight before becoming claimable.
#[cfg(any())]
#[test]
fn runtime_configured_input_cap_compacts_before_provider_claim() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "configured-input-cap-preflight".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "configured-input-cap-test"
shell_mode = "pane"
[permissions]
sandbox = "policy-only"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.configured-input-cap-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
max_input_tokens = 20000
"#
            .to_string(),
        }])
        .unwrap();
    let auth_root = temp_root("configured-input-cap-auth");
    service.set_auth_store(AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&auth_root),
    ));
    let transcript_store = AgentTranscriptStore::new(temp_root("configured-input-cap-history"));
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "configured-input-cap-history".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "turn-history".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: format!(
                "configured-cap-marker {} secRET-CROSS-BOUNDARY source-tail-sentinel",
                "compactable ".repeat(20_000)
            ),
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
        .agent_shell_store_mut()
        .bind_conversation("%1", "configured-input-cap-history", 1)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"configured-input-cap-preflight","input":"continue after proactive compaction"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service.pending_agent_provider_tasks().remove(0);
    let agent_id = AgentId::opaque(task.agent_id.clone()).unwrap();
    let execution_usage = mez_agent::ModelTokenUsage {
        input_tokens: 1_200,
        output_tokens: 20,
        reasoning_tokens: 5,
        cached_input_tokens: Some(100),
        cache_write_input_tokens: None,
    };
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        execution_usage,
        execution_usage,
        Some(&task.model_profile),
    );

    let same_turn_group =
        mez_agent::ContextExecutionGroupId::new("configured-input-cap-same-turn").unwrap();
    let same_turn_marker = "same-turn-raw-marker ".repeat(20_000);
    let turn_context = service
        .agent_turn_contexts_mut()
        .get_mut(&task.turn_id)
        .expect("the configured-input turn owns an active context");
    turn_context
        .append_assistant_event(
            "same-turn compaction action",
            "inspect same-turn evidence before configured-input compaction",
            same_turn_group.clone(),
        )
        .unwrap();
    turn_context
        .append_evidence_event(
            mez_agent::ContextSourceKind::ActionResult,
            "same-turn large evidence",
            same_turn_marker.clone(),
            same_turn_group,
            None,
            true,
        )
        .unwrap();

    assert!(
        service
            .claim_configured_agent_provider_task(&agent_id, &task.turn_id)
            .unwrap()
            .is_none()
    );
    assert!(!service.agent_provider_task_is_pending(&task.turn_id));
    assert_eq!(
        service
            .provider_retry_scheduler_mut()
            .attempt(&task.turn_id),
        0
    );
    let queued = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap_or_else(|| {
            let turns = service.agent_turn_ledger().turns();
            let pane_text = service
                .pane_screen("%1")
                .unwrap()
                .normal_content_lines()
                .join("\n");
            panic!(
                "configured input cap should queue active-turn compaction; turns={turns:?}; pane={pane_text}"
            )
        });
    assert_eq!(queued.source, "configured-input-limit");
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
        trigger:
            crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ConfiguredInputLimit {
                pass,
                previous_input_tokens,
                max_input_tokens,
            },
        plan,
        ..
    } = &queued.target
    else {
        panic!("expected configured-input-limit active-turn compaction");
    };
    assert_eq!(*pass, 1);
    assert!(*previous_input_tokens > *max_input_tokens);
    assert_eq!(
        queued.request.max_output_tokens,
        Some(plan.summary_budget_words()),
        "the active compactor must not emit a profile-sized summary that exceeds the frozen plan"
    );

    let compactor_estimate = mez_agent::provider_request_input_estimate(
        &queued.request,
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        &std::collections::BTreeMap::new(),
        false,
    )
    .unwrap();
    let compactor_cap = compactor_estimate.input_tokens.saturating_sub(1);
    assert!(compactor_cap > 0);
    service
        .pending_agent_compaction_task_mut_for_tests("%1")
        .unwrap()
        .model_profile
        .provider_options
        .insert("max_input_tokens".to_string(), compactor_cap.to_string());

    let mut compactor_requests = 0usize;
    let mut observed_split = false;
    let mut observed_tail_sentinel = false;
    let mut observed_redaction = false;
    while service
        .pending_agent_compaction_task_for_tests("%1")
        .is_some()
    {
        let dispatch = service
            .claim_agent_compaction_task("%1")
            .unwrap()
            .expect("configured-cap compactor request should become claimable");
        compactor_requests = compactor_requests.saturating_add(1);
        let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
            pending_blocks,
            plan,
            ..
        } = &dispatch.task.target
        else {
            panic!("expected active-turn compactor dispatch");
        };
        observed_tail_sentinel |= dispatch
            .task
            .request
            .messages
            .iter()
            .any(|message| message.content.contains("source-tail-sentinel"));
        observed_redaction |= dispatch
            .task
            .request
            .messages
            .iter()
            .any(|message| message.content.contains("[redacted]"));
        assert!(
            !dispatch
                .task
                .request
                .messages
                .iter()
                .any(|message| message.content.contains("secRET-CROSS-BOUNDARY")),
            "temporary compactor chunks must be redacted before splitting"
        );
        observed_split |= !pending_blocks.is_empty();
        assert_eq!(
            dispatch.task.request.max_output_tokens,
            Some(plan.summary_budget_words()),
            "every post-split compactor dispatch must retain the frozen summary ceiling"
        );
        assert!(
            compactor_requests <= 256,
            "configured-cap compactor recursion did not settle"
        );
        let mut response = runtime_test_compaction_response(&format!(
            "proactively compacted summary {compactor_requests}"
        ));
        response.usage = mez_agent::ModelTokenUsage {
            input_tokens: 40,
            output_tokens: 5,
            reasoning_tokens: 1,
            cached_input_tokens: Some(4),
            cache_write_input_tokens: None,
        };
        assert!(
            service
                .apply_agent_compaction_completed_event("%1", response)
                .unwrap()
        );
    }
    assert!(observed_split, "oversized compactor source was not split");
    assert!(compactor_requests > 1);
    assert!(
        observed_tail_sentinel,
        "the tail of a selected source block must reach a compactor request"
    );
    assert!(
        observed_redaction,
        "selected sensitive source must be redacted"
    );
    assert!(
        service
            .agent_latest_request_usage("configured-input-cap-history")
            .is_none(),
        "context replacement must leave execution usage unknown until execution resumes"
    );
    assert!(
        service
            .agent_context_usage_display("configured-input-cap-history")
            .is_none(),
        "compactor usage must not become the context display"
    );
    assert!(
        service
            .agent_context_usage_snapshot("configured-input-cap-history")
            .is_none(),
        "compactor usage must not become the context snapshot"
    );
    let usage_by_model = service.agent_token_usage_for_conversation("configured-input-cap-history");
    let accumulated = usage_by_model
        .get(&mez_agent::ModelTokenUsageKey::new(
            &task.model_profile.provider,
            &task.model_profile.model,
        ))
        .expect("execution and compactor usage must remain accounted");
    assert_eq!(
        accumulated.input_tokens,
        execution_usage.input_tokens + 40 * compactor_requests as u64
    );
    assert_eq!(
        accumulated.output_tokens,
        execution_usage.output_tokens + 5 * compactor_requests as u64
    );
    assert!(
        !service
            .agent_turn_contexts()
            .get(&task.turn_id)
            .expect("compacted turn context remains active")
            .blocks()
            .iter()
            .any(|block| block.content.contains("same-turn-raw-marker")),
        "configured-input compaction must not resurrect selected same-turn evidence"
    );
    assert!(service.agent_provider_task_is_pending(&task.turn_id));
    assert_eq!(
        service
            .provider_retry_scheduler_mut()
            .attempt(&task.turn_id),
        0
    );
    let dispatch = service
        .claim_configured_agent_provider_task(&agent_id, &task.turn_id)
        .unwrap()
        .expect("smaller rebuilt request should become claimable");
    assert!(
        dispatch
            .context
            .durable()
            .blocks()
            .iter()
            .any(|block| block.content.contains("proactively compacted summary"))
    );

    assert!(service.agent_turn_executions().get(&task.turn_id).is_none());
    service
        .record_claimed_agent_provider_task(&dispatch, 1, 30_000)
        .unwrap();
    let retained_context = service
        .agent_turn_contexts()
        .get(&task.turn_id)
        .cloned()
        .unwrap();
    let (prepared, _) = service
        .prepare_agent_turn_model_context(
            &dispatch.turn,
            retained_context.clone(),
            &service.mcp_registry().prompt_summary(),
            &dispatch.model_profile,
        )
        .unwrap();
    let retained_request = prepared
        .previous_request()
        .expect("claim should retain the exact OpenAI request for the next worker");
    assert_eq!(retained_request.provider, "runtime-batch");
    assert_eq!(retained_request.model, "test");

    let response = runtime_say_response(&task.turn_id, "first turn completed", true);
    let action = response
        .action_batch
        .as_ref()
        .and_then(|batch| batch.actions.first())
        .cloned()
        .expect("completion response should contain a final say action");
    service
        .apply_agent_provider_execution(
            &dispatch.turn,
            &dispatch.model_profile,
            "runtime-batch",
            mez_agent::AgentTurnExecution {
                request: runtime_model_request_fixture_for_agent(&task.turn_id, &task.agent_id),
                response,
                latest_response_usage: Default::default(),
                routing_token_usage_by_model: std::collections::BTreeMap::new(),
                action_results: vec![mez_agent::ActionResult::succeeded(
                    &dispatch.turn,
                    &action,
                    vec!["first turn completed".to_string()],
                    None,
                )],
                final_turn: true,
                terminal_state: AgentTurnState::Completed,
            },
        )
        .unwrap();
    assert!(service.agent_turn_contexts().get(&task.turn_id).is_none());

    let second = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"configured-input-cap-next","method":"agent/shell/command","params":{"idempotency_key":"configured-input-cap-next","input":"small follow-up"}}"#,
        &primary,
    );
    assert!(second.contains(r#""state":"running""#), "{second}");
    let second_task = service.pending_agent_provider_tasks().remove(0);
    let second_agent_id = AgentId::opaque(second_task.agent_id.clone()).unwrap();
    assert!(
        service
            .claim_configured_agent_provider_task(&second_agent_id, &second_task.turn_id)
            .unwrap()
            .is_some(),
        "the next turn should use the persisted compacted epoch"
    );
    assert!(service.pending_agent_compaction_tasks().is_empty());

    service.clear_agent_turn_provider_request_chain(&task.turn_id);
    let (new_epoch, _) = service
        .prepare_agent_turn_model_context(
            &dispatch.turn,
            retained_context,
            &service.mcp_registry().prompt_summary(),
            &dispatch.model_profile,
        )
        .unwrap();
    assert!(new_epoch.previous_request().is_none());
}

/// Verifies an explicit input cap fails before provider dispatch when only
/// protected request material remains and no durable context is compactable.
///
/// The runtime must not queue an ineffective compaction pass or consume a
/// provider retry attempt. The public claim boundary should settle the turn
/// through its normal typed provider-failure path with no provider worker.
#[cfg(any())]
#[test]
fn runtime_configured_input_cap_fails_without_compactable_context() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "configured-input-cap-protected-only".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "configured-input-cap-protected-only"
shell_mode = "pane"
[permissions]
sandbox = "policy-only"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.configured-input-cap-protected-only]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
max_input_tokens = 1
"#
            .to_string(),
        }])
        .unwrap();
    let auth_root = temp_root("configured-input-cap-protected-only-auth");
    service.set_auth_store(AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&auth_root),
    ));
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"configured-input-cap-protected-only","input":"this exact user instruction cannot be compacted"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service.pending_agent_provider_tasks().remove(0);
    let agent_id = AgentId::opaque(task.agent_id).unwrap();

    assert!(
        service
            .claim_configured_agent_provider_task(&agent_id, &task.turn_id)
            .unwrap()
            .is_none()
    );
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(!service.agent_provider_task_is_pending(&task.turn_id));
    assert_eq!(
        service
            .provider_retry_scheduler_mut()
            .attempt(&task.turn_id),
        0
    );
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| { turn.turn_id == task.turn_id && turn.state == AgentTurnState::Failed })
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let pane_text_unwrapped = normalized_pane_log_text(&pane_text);
    assert!(
        pane_text_unwrapped
            .contains("configured input cap is smaller than fixed provider request overhead"),
        "{pane_text}"
    );
}

/// Verifies synchronous provider recovery completes model-backed compaction
/// before rebuilding and retrying a request rejected for context length.
///
/// The compatibility worker must send the oversized context once, send the
/// selected source to the compactor, and only then retry with durable context
/// containing the model-authored summary instead of the rejected source.
#[test]
fn runtime_synchronous_context_limit_recovery_waits_for_compaction() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "synchronous-context-limit-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "synchronous-context-limit-test"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.synchronous-context-limit-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"synchronous-context-limit-recovery","input":"continue with the oversized evidence"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    insert_test_context_block(
        service.agent_turn_contexts_mut().get_mut("turn-1").unwrap(),
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "synchronous rejected context".to_string(),
            content: format!(
                "synchronous-oversized-marker {}",
                "oversized ".repeat(10_000)
            ),
        },
    );
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeContextLimitThenCompactionProvider {
        requests: RefCell::new(Vec::new()),
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            service
                .provider_registry()
                .resolve_profile("synchronous-context-limit-test")
                .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 3, "{requests:#?}");
    let request_text = |index: usize| {
        requests[index]
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let rejected = request_text(0);
    let compaction = request_text(1);
    let retry = request_text(2);
    assert!(rejected.contains("synchronous-oversized-marker"));
    assert!(compaction.contains("synchronous-oversized-marker"));
    assert!(retry.contains("synchronous model-authored context summary"));
    assert!(!retry.contains("synchronous-oversized-marker"));
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(!service.agent_is_compacting("%1"));
}

/// Verifies compactor backoff recursively summarizes smaller temporary chunks.
///
/// The rejected compaction request must not mutate active-turn context or
/// dispatch the original provider turn. Every retry must be smaller than the
/// rejected compactor request, while the original source plan remains atomic
/// until the recursively synthesized final summary is ready.
#[test]
fn runtime_model_compaction_recursively_shrinks_without_exact_tail_fallback() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "model-compaction-context-limit-backoff".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "model-compaction-backoff-test"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.model-compaction-backoff-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
max_input_tokens = 20000
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"model-compaction-context-limit-backoff","input":"continue after compacting the observations"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    insert_test_context_block(
        context,
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "older compaction input".to_string(),
            content: format!("older-backoff-marker {}", "older ".repeat(6_000)),
        },
    );
    insert_test_context_block(
        context,
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "newer exact compaction tail".to_string(),
            content: format!("newer-exact-marker {}", "newer ".repeat(6_000)),
        },
    );
    let original = context.blocks().to_vec();
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    let transition = service
        .schedule_agent_provider_retry_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            "turn-1",
            mez_agent::ProviderErrorRetryClass::ContextLimit,
            &error,
        )
        .unwrap()
        .expect("context-limit recovery transition");
    assert!(transition.side_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::DispatchAgentCompaction { pane_id, .. } if pane_id == "%1"
    )));
    let initial_task = service
        .take_pending_agent_compaction_task("%1")
        .expect("initial compaction task");
    let initial_source = initial_task
        .request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let initial_request_bytes =
        mez_agent::openai_responses_request_body_with_stream(&initial_task.request, false)
            .unwrap()
            .len();
    assert!(initial_source.contains("older-backoff-marker"));
    assert!(initial_source.contains("newer-exact-marker"));
    service.claim_agent_compaction_task_state("%1", initial_task);

    assert!(
        service
            .apply_agent_compaction_failed_event(
                "%1",
                "invalid_state",
                "provider context length exceeded",
                Some(r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#),
            )
            .unwrap()
    );
    assert_eq!(
        service
            .agent_turn_contexts()
            .get("turn-1")
            .unwrap()
            .blocks(),
        original.as_slice()
    );
    assert!(!service.agent_provider_task_is_pending("turn-1"));
    let retry_task = service
        .pending_agent_compaction_task_for_tests("%1")
        .expect("backed-off compaction task");
    let retry_source = retry_task
        .request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let retry_request_bytes =
        mez_agent::openai_responses_request_body_with_stream(&retry_task.request, false)
            .unwrap()
            .len();
    assert!(retry_request_bytes < initial_request_bytes);
    assert!(retry_source.contains("older-backoff-marker"));
    assert!(!retry_source.contains("newer-exact-marker"));
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
        compaction_backoff_attempt,
        pending_blocks,
        plan,
        ..
    } = &retry_task.target
    else {
        panic!("expected active-turn compaction target");
    };
    assert_eq!(*compaction_backoff_attempt, 1);
    assert!(plan.retained_tail().is_empty());
    assert!(
        plan.replacement_blocks()
            .iter()
            .any(|block| block.content.contains("older-backoff-marker"))
    );
    assert!(
        plan.replacement_blocks()
            .iter()
            .any(|block| block.content.contains("newer-exact-marker"))
    );
    assert_eq!(pending_blocks.len(), 1);
    assert!(
        pending_blocks[0]
            .iter()
            .any(|block| block.content.contains("newer-exact-marker"))
    );

    complete_runtime_test_compaction(&mut service, "%1", "model-authored older-group summary");
    assert_eq!(
        service
            .agent_turn_contexts()
            .get("turn-1")
            .unwrap()
            .blocks(),
        original.as_slice()
    );
    assert!(!service.agent_provider_task_is_pending("turn-1"));
    let second_chunk_task = service
        .pending_agent_compaction_task_for_tests("%1")
        .expect("second recursive compaction chunk");
    let second_chunk_source = second_chunk_task
        .request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!second_chunk_source.contains("older-backoff-marker"));
    assert!(second_chunk_source.contains("newer-exact-marker"));

    complete_runtime_test_compaction(&mut service, "%1", "model-authored newer-group summary");
    assert_eq!(
        service
            .agent_turn_contexts()
            .get("turn-1")
            .unwrap()
            .blocks(),
        original.as_slice()
    );
    assert!(!service.agent_provider_task_is_pending("turn-1"));
    let synthesis_task = service
        .pending_agent_compaction_task_for_tests("%1")
        .expect("recursive summary synthesis task");
    let synthesis_source = synthesis_task
        .request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(synthesis_source.contains("model-authored older-group summary"));
    assert!(synthesis_source.contains("model-authored newer-group summary"));

    complete_runtime_test_compaction(&mut service, "%1", "recursive final-fit summary");
    let final_context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert_eq!(
        final_context
            .blocks()
            .iter()
            .filter(|block| block.label == "context compaction summary")
            .count(),
        1
    );
    let final_text = final_context
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(final_text.contains("recursive final-fit summary"));
    assert!(!final_text.contains("older-backoff-marker"));
    assert!(!final_text.contains("newer-exact-marker"));
    assert!(service.agent_provider_task_is_pending("turn-1"));
}

/// A viable whole-group walk-back leaves the newest selected group raw and
/// summarizes only the older source after a compactor context-limit rejection.
#[test]
fn runtime_compactor_walkback_preserves_newest_group() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "walkback".to_string(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"walkback\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.walkback]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 40000\n".to_string(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"walkback","method":"agent/shell/command","params":{"idempotency_key":"walkback","input":"continue"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    for (label, marker) in [("older", "OLDER_WALKBACK"), ("newer", "NEWER_WALKBACK")] {
        insert_test_context_block(
            context,
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: label.to_string(),
                content: format!("{marker} {}", "evidence ".repeat(3_000)),
            },
        );
    }
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    service
        .schedule_agent_provider_retry_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            "turn-1",
            mez_agent::ProviderErrorRetryClass::ContextLimit,
            &error,
        )
        .unwrap()
        .expect("context-limit transition");
    let initial = service.take_pending_agent_compaction_task("%1").unwrap();
    service.claim_agent_compaction_task_state("%1", initial);
    service
        .apply_agent_compaction_failed_event(
            "%1",
            "invalid_state",
            "provider context length exceeded",
            Some(r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#),
        )
        .unwrap();
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
        plan,
        pending_blocks,
        ..
    } = &retry.target
    else {
        panic!("expected active-turn retry");
    };
    assert!(
        plan.replacement_blocks()
            .iter()
            .any(|block| block.content.contains("OLDER_WALKBACK"))
    );
    assert!(
        !plan
            .replacement_blocks()
            .iter()
            .any(|block| block.content.contains("NEWER_WALKBACK"))
    );
    assert!(pending_blocks.is_empty());
    complete_runtime_test_compaction(&mut service, "%1", "older work summarized");
    let blocks = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks();
    assert!(
        blocks
            .iter()
            .any(|block| block.content.contains("NEWER_WALKBACK"))
    );
    assert!(
        !blocks
            .iter()
            .any(|block| block.content.contains("OLDER_WALKBACK"))
    );
}

/// An unmapped first-turn source uses the complete turn-local request when
/// walking back after a compactor context-limit response. It must not require
/// a nonexistent durable archive or publish a selective epoch on completion.
#[test]
fn runtime_observed_first_turn_compactor_walkback_is_turn_local() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("first-turn-walkback"));
    service.set_agent_transcript_store(store.clone());
    service.use_transcript_effect_adapter();
    service.replace_config_layers(vec![ConfigLayer {
        name: "first-turn-walkback".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"first-turn-walkback\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.first-turn-walkback]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 40000\nmax_input_tokens = 20000\n".to_string(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"first-walkback","method":"agent/shell/command","params":{"idempotency_key":"first-walkback","input":"continue"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    for (label, marker) in [
        ("older", "OLDER_FIRST_WALKBACK"),
        ("newer", "NEWER_FIRST_WALKBACK"),
    ] {
        insert_test_context_block(
            context,
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: label.to_string(),
                content: format!("{marker} {}", "evidence ".repeat(1_000)),
            },
        );
    }
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    let plan = mez_agent::plan_model_context_compaction_for_provider_tokens(
        context,
        18_000,
        1,
        context.event_sequence_high_water_mark(),
        mez_agent::ProviderBudgetProjection::new(
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
            "runtime-batch",
        ),
    )
    .unwrap();
    assert!(plan.replacement_blocks().len() >= 2);
    let profile = service.agent_turn_model_profile("turn-1").unwrap().clone();
    assert!(service.queue_agent_active_turn_compaction(
        "turn-1", "first-turn-walkback".to_string(), profile,
        crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
            observed_input_tokens: 20_000, max_input_tokens: 20_000,
        }, plan,
    ).unwrap());
    assert!(
        !store
            .transcript_path(
                service
                    .agent_shell_store()
                    .get("%1")
                    .unwrap()
                    .session_id
                    .as_str()
            )
            .unwrap()
            .exists()
    );
    let initial = service.take_pending_agent_compaction_task("%1").unwrap();
    service.claim_agent_compaction_task_state("%1", initial);
    service
        .apply_agent_compaction_failed_event(
            "%1",
            "invalid_state",
            "provider context length exceeded",
            Some(r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#),
        )
        .unwrap();
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .expect("walk-back queued");
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn { plan, .. } =
        &retry.target
    else {
        panic!("expected active-turn walk-back");
    };
    assert!(
        plan.replacement_blocks()
            .iter()
            .any(|block| block.content.contains("OLDER_FIRST_WALKBACK"))
    );
    assert!(
        !plan
            .replacement_blocks()
            .iter()
            .any(|block| block.content.contains("NEWER_FIRST_WALKBACK"))
    );
    complete_runtime_test_compaction(&mut service, "%1", "older work summarized");
    assert!(service.agent_provider_task_is_pending("turn-1"));
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    let blocks = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks();
    assert!(
        blocks
            .iter()
            .any(|block| block.content.contains("NEWER_FIRST_WALKBACK"))
    );
    assert!(
        !blocks
            .iter()
            .any(|block| block.content.contains("OLDER_FIRST_WALKBACK"))
    );
    assert!(
        store
            .compaction_epoch(
                service
                    .agent_shell_store()
                    .get("%1")
                    .unwrap()
                    .session_id
                    .as_str()
            )
            .unwrap()
            .is_none()
    );
}

/// A walk-back validation error after the compaction claim is retired must
/// fail the waiting turn rather than leave it running without an owner.
#[test]
fn runtime_compactor_walkback_validation_failure_settles_turn() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "walkback-error".to_string(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"walkback\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.walkback]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 40000\n".to_string(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"walkback-error","method":"agent/shell/command","params":{"idempotency_key":"walkback-error","input":"continue"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    for label in ["older", "newer"] {
        insert_test_context_block(
            context,
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: label.to_string(),
                content: format!("{label} {}", "evidence ".repeat(3_000)),
            },
        );
    }
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    service
        .schedule_agent_provider_retry_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            "turn-1",
            mez_agent::ProviderErrorRetryClass::ContextLimit,
            &error,
        )
        .unwrap()
        .expect("context-limit transition");
    let initial = service.take_pending_agent_compaction_task("%1").unwrap();
    service.claim_agent_compaction_task_state("%1", initial);
    let mut profile = service.agent_turn_model_profile("turn-1").unwrap().clone();
    profile.provider = "missing-walkback-provider".to_string();
    service.set_agent_turn_model_profile("turn-1", profile);
    let outcome = service.apply_agent_compaction_failed_event(
        "%1",
        "invalid_state",
        "provider context length exceeded",
        Some(r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#),
    );
    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(!service.agent_is_compacting("%1"));
    assert!(!service.agent_provider_task_is_pending("turn-1"));
    assert_eq!(
        service.agent_turn_ledger().turn("turn-1").unwrap().state,
        AgentTurnState::Failed
    );
}

/// Verifies a rebuilt provider request that is not smaller fails closed.
///
/// The compactor summary must not mutate durable context or automatically
/// replay the rejected provider command when the complete serialized Responses
/// body cannot satisfy the strict monotonic reduction invariant.
#[test]
fn runtime_context_limit_recovery_does_not_resend_non_shrinking_request() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "context-limit-monotonic-gate".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "context-limit-monotonic-test"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.context-limit-monotonic-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"context-limit-monotonic-gate","input":"continue after compacting this oversized evidence"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    insert_test_context_block(
        context,
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "oversized rejected evidence".to_string(),
            content: format!("monotonic-gate-marker {}", "evidence ".repeat(8_000)),
        },
    );
    let original = context.blocks().to_vec();
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    service
        .schedule_agent_provider_retry_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            "turn-1",
            mez_agent::ProviderErrorRetryClass::ContextLimit,
            &error,
        )
        .unwrap()
        .expect("context-limit recovery transition");
    let task = service
        .pending_agent_compaction_task_mut_for_tests("%1")
        .expect("queued active-turn compaction task");
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
        rejected_request_bytes,
        rejected_request_stream,
        ..
    } = &mut task.target
    else {
        panic!("expected active-turn compaction target");
    };
    *rejected_request_bytes = Some(1);
    *rejected_request_stream = Some(false);

    complete_runtime_test_compaction(&mut service, "%1", "bounded model summary");

    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(!service.agent_provider_task_is_pending("turn-1"));
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| { turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed })
    );
    assert!(
        original
            .iter()
            .any(|block| block.content.contains("monotonic-gate-marker"))
    );
}

/// Verifies non-context compactor failures remain terminal and never move a
/// selected execution group into the exact retained tail.
///
/// Progressive backoff is reserved for provider-authoritative context-limit
/// failures. Authentication, transport, malformed output, and other failure
/// classes must preserve the original context and fail the waiting turn.
#[test]
fn runtime_model_compaction_non_context_failure_does_not_back_off() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "model-compaction-terminal-failure".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "model-compaction-terminal-test"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.model-compaction-terminal-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"model-compaction-terminal-failure","input":"continue after compacting the observations"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .expect("running pane retains its agent conversation")
        .session_id
        .clone();
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    for (label, marker) in [
        ("older compaction input", "terminal-older-marker"),
        ("newer compaction input", "terminal-newer-marker"),
    ] {
        insert_test_context_block(
            context,
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: label.to_string(),
                content: format!("{marker} {}", "history ".repeat(6_000)),
            },
        );
    }
    let original = context.blocks().to_vec();
    let (_, profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    let execution_usage = mez_agent::ModelTokenUsage {
        input_tokens: 1_200,
        output_tokens: 20,
        reasoning_tokens: 5,
        cached_input_tokens: Some(100),
        cache_write_input_tokens: None,
    };
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        execution_usage,
        execution_usage,
        Some(&profile),
    );
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    service
        .schedule_agent_provider_retry_transition(
            &AgentId::opaque("agent-%1").unwrap(),
            "turn-1",
            mez_agent::ProviderErrorRetryClass::ContextLimit,
            &error,
        )
        .unwrap()
        .expect("context-limit recovery transition");
    let task = service
        .take_pending_agent_compaction_task("%1")
        .expect("initial compaction task");
    service.claim_agent_compaction_task_state("%1", task);

    assert!(
        service
            .apply_agent_compaction_failed_event(
                "%1",
                "forbidden",
                "provider authentication rejected the compaction request",
                Some(r#"{"status_code":401,"error":{"code":"invalid_api_key"}}"#),
            )
            .unwrap()
    );
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Failed)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let failure = normalized_pane_log_text(&pane_text);
    assert!(
        failure
            .contains("automatic provider-context-limit compaction failed before provider retry"),
        "{}",
        pane_text
    );
    assert!(
        !failure.contains("automatic output-limit compaction"),
        "{}",
        pane_text
    );
    assert!(!service.agent_provider_task_is_pending("turn-1"));
    assert_eq!(
        service
            .agent_latest_request_usage(&conversation_id)
            .expect("failed compaction must retain the execution sample")
            .usage,
        execution_usage
    );
    assert!(
        original
            .iter()
            .any(|block| block.content.contains("terminal-newer-marker"))
    );
}

/// Verifies a second provider context-limit recovery attempt remains deferred
/// until a model-authored summary shrinks stored active-turn context.
///
/// Once one compaction pass has already happened, a second provider rejection
/// must queue another model request without mutating the durable context or
/// prematurely retrying the rejected provider turn.
#[test]
fn runtime_provider_context_limit_error_compacts_context_multiple_times() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "provider-context-limit-multi-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "provider-context-limit-multi-test"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.provider-context-limit-multi-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-context-limit-multi-recovery","input":"continue with the very large observation"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service
        .agent_turn_contexts_mut()
        .get_mut("turn-1")
        .unwrap()
        .replace_after_compaction(vec![
            ContextBlock {
                source: ContextSourceKind::Memory,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "synthetic post-first-pass summary".to_string(),
                content: format!("[context compacted]\n{}", "summary ".repeat(8_000)),
            },
            ContextBlock::assistant_event(
                "synthetic retained action request",
                "synthetic action request owning the retained results",
            ),
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "synthetic retained tail action result one".to_string(),
                content: format!("provider-context-limit-tail-one- {}", "tail ".repeat(5_000)),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "synthetic retained tail action result two".to_string(),
                content: format!("provider-context-limit-tail-two- {}", "tail ".repeat(5_000)),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "synthetic retained tail action result three".to_string(),
                content: format!(
                    "provider-context-limit-tail-three- {}",
                    "tail ".repeat(5_000)
                ),
            },
        ])
        .unwrap();
    let before_context = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let error = MezError::invalid_state(
        "OpenAI Responses API returned status 400: This model's maximum context length is 128000 tokens. However, your messages resulted in 130000 tokens. Please reduce the length of the messages.",
    )
    .with_provider_failure_json(
        r#"{"status_code":400,"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 130000 tokens. Please reduce the length of the messages.","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
    );

    let recovered = service
        .recover_agent_provider_context_limit_failure(
            &AgentId::opaque("agent-%1").unwrap(),
            "turn-1",
            &error,
            2,
        )
        .unwrap();

    assert!(recovered);
    let queued_context = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(queued_context, before_context);
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some()
    );
    assert!(!service.agent_provider_task_is_pending("turn-1"));

    complete_runtime_test_compaction(&mut service, "%1", "second model-authored summary");
    let after_context = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(after_context.contains("second model-authored summary"));
    assert!(service.agent_provider_task_is_pending("turn-1"));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let pane_text_unwrapped = normalized_pane_log_text(&pane_text);
    assert!(
        pane_text_unwrapped.contains(
            "provider rejected context as too large; requesting model-backed context compaction"
        ),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("no compactable active turn context remains"),
        "{pane_text}"
    );
}

/// Verifies repeated provider context-limit recovery can still shrink a stored
/// action-result-only prefix after an earlier retry already narrowed context.
///
/// Later retries may encounter a compacted active-turn context where the next
/// compactable prefix contains only older action results. Recovery must still
/// summarize older exact action-result blocks instead of bailing out with a
/// false "no compactable active turn context remains" message.
#[test]
fn runtime_provider_context_limit_error_recompacts_action_result_prefix() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "provider-context-limit-action-result-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "provider-context-limit-action-result-test"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.provider-context-limit-action-result-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-context-limit-action-result-recovery","input":"continue with the very large observation"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service
        .agent_turn_contexts_mut()
        .get_mut("turn-1")
        .unwrap()
        .replace_after_compaction(vec![
            ContextBlock::assistant_event(
                "synthetic retained action request",
                "synthetic action request owning the retained results",
            ),
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "synthetic retained action result one".to_string(),
                content: format!("provider-context-limit-tail-one- {}", "tail ".repeat(5_000)),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "synthetic retained action result two".to_string(),
                content: format!("provider-context-limit-tail-two- {}", "tail ".repeat(5_000)),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "synthetic retained action result three".to_string(),
                content: format!(
                    "provider-context-limit-tail-three- {}",
                    "tail ".repeat(5_000)
                ),
            },
        ])
        .unwrap();
    let before_context = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let error = MezError::invalid_state(
        "OpenAI Responses API returned status 400: This model's maximum context length is 128000 tokens. However, your messages resulted in 130000 tokens. Please reduce the length of the messages.",
    )
    .with_provider_failure_json(
        r#"{"status_code":400,"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 130000 tokens. Please reduce the length of the messages.","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
    );

    let recovered = service
        .recover_agent_provider_context_limit_failure(
            &AgentId::opaque("agent-%1").unwrap(),
            "turn-1",
            &error,
            2,
        )
        .unwrap();

    assert!(recovered);
    let queued_context = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(queued_context, before_context);
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some()
    );
    assert!(!service.agent_provider_task_is_pending("turn-1"));

    complete_runtime_test_compaction(
        &mut service,
        "%1",
        "model-authored action-result prefix summary",
    );
    let after_context = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        after_context.contains("model-authored action-result prefix summary"),
        "{after_context}"
    );
    assert!(service.agent_provider_task_is_pending("turn-1"));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let pane_text_unwrapped = normalized_pane_log_text(&pane_text);
    assert!(
        pane_text_unwrapped.contains(
            "provider rejected context as too large; requesting model-backed context compaction"
        ),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("no compactable active turn context remains"),
        "{pane_text}"
    );
}

/// Verifies provider output-limit incomplete responses first trigger a compact
/// request-local mode, then max-output escalation, without mutating durable
/// active-turn context.
///
/// Output exhaustion means the provider accepted the input but cut generation
/// off, so the recovery path should first select the stable compact-response
/// behavior before escalating the output budget or discarding chronology.
#[test]
fn runtime_provider_output_limit_error_guides_then_escalates_without_compaction() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "provider-output-limit-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "provider-output-limit-test"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.provider-output-limit-test]
provider = "runtime-batch"
model = "test"
max_output_tokens = 4096
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-output-limit-recovery","method":"agent/shell/command","params":{"idempotency_key":"agent-output-limit-recovery","input":"continue with the current implementation"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    insert_test_context_block(
        context,
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "synthetic retained action result".to_string(),
            content: "output-limit-retained-context".to_string(),
        },
    );
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeOutputLimitThenSuccessProvider {
        requests: RefCell::new(Vec::new()),
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            service
                .provider_registry()
                .resolve_profile("provider-output-limit-test")
                .unwrap(),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 3);
    let conversation_id = &service.agent_shell_store().get("%1").unwrap().session_id;
    let usage = service.agent_token_usage_for_conversation(conversation_id);
    let spent = usage
        .get(&mez_agent::ModelTokenUsageKey::new("runtime-batch", "test"))
        .unwrap();
    assert_eq!(spent.input_tokens, 250);
    assert_eq!(spent.output_tokens, 45);
    assert_eq!(spent.reasoning_tokens, 10);
    assert_eq!(spent.cached_input_tokens, Some(25));
    assert_eq!(requests[0].max_output_tokens, Some(4096));
    assert_eq!(requests[1].max_output_tokens, Some(4096));
    assert_eq!(requests[2].max_output_tokens, Some(4096));
    let second_request_text = requests[1]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second_request_text.contains("output-limit-retained-context"),
        "{second_request_text}"
    );
    assert!(
        second_request_text.contains("Mezzanine interaction mode: output_limit_retry"),
        "{second_request_text}"
    );
    assert!(
        second_request_text.contains("[safe partial assistant text]\npartial safe response"),
        "{second_request_text}"
    );
    assert!(
        second_request_text.contains("Treat the safe assistant text below as already emitted"),
        "{second_request_text}"
    );
    assert!(!second_request_text.contains("output_limit_recovery_attempt="));
    assert!(
        !second_request_text.contains("error_message="),
        "{second_request_text}"
    );
    let third_request_text = requests[2]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        third_request_text.contains("Mezzanine interaction mode: output_limit_retry"),
        "{third_request_text}"
    );
    assert!(!third_request_text.contains("output_limit_recovery_attempt="));
    assert!(
        !second_request_text.contains("[context compacted]"),
        "{second_request_text}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let pane_text_unwrapped = pane_text.replace("\n", "");
    assert!(
        pane_text_unwrapped
            .contains("provider response hit output limit; continuing from safe partial output"),
        "{pane_text}"
    );
    assert!(
        pane_text_unwrapped
            .contains("provider response hit output limit again; starting one fresh compact"),
        "{pane_text}"
    );
}

/// Verifies routing context-limit recovery budgets against the smallest
/// possible main-provider target before a router decision has been stored.
///
/// A turn may start with a large default profile while the router is still able
/// to choose a smaller target profile for the first normal request. Provider
/// context-limit recovery must therefore compact against the minimum target
/// window until the synthesized per-turn profile exists.
#[test]
fn runtime_routing_context_limit_recovery_uses_minimum_target_window() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "routing-context-limit-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"
[agents]
default_provider = "runtime-batch"
default_model_profile = "default"
routing = true

[agents.auto_sizing]
router_model_profile = "router"
small_model_profile = "small"
medium_model_profile = "medium"
large_model_profile = "large"
allowed_reasoning_efforts = ["low", "medium", "high", "xhigh"]
fallback_policy = "use-default-profile"

[providers.runtime-batch]
kind = "openai"
models = ["gpt-router", "gpt-default", "gpt-small", "gpt-medium", "gpt-large"]
default_model = "gpt-default"

[model_profiles.default]
provider = "runtime-batch"
model = "gpt-default"
reasoning_profile = "medium"
context_window_tokens = 100000

[model_profiles.router]
provider = "runtime-batch"
model = "gpt-router"
reasoning_profile = "low"
context_window_tokens = 2000

[model_profiles.small]
provider = "runtime-batch"
model = "gpt-small"
reasoning_profile = "medium"
context_window_tokens = 40000

[model_profiles.medium]
provider = "runtime-batch"
model = "gpt-medium"
reasoning_profile = "medium"
context_window_tokens = 100000

[model_profiles.large]
provider = "runtime-batch"
model = "gpt-large"
reasoning_profile = "high"
context_window_tokens = 100000
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-auto-context-limit","method":"agent/shell/command","params":{"idempotency_key":"agent-auto-context-limit","input":"continue with the current findings"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let default_profile = service
        .provider_registry()
        .resolve_profile("default")
        .unwrap();
    service.set_agent_turn_model_profile("turn-1", default_profile);
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    insert_test_context_block(
        context,
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "synthetic routing action result".to_string(),
            content: format!(
                "routing-context-pressure- {}",
                "context-pressure ".repeat(50_000)
            ),
        },
    );
    let error = MezError::invalid_state(
        "OpenAI Responses API returned status 400: context length exceeded",
    )
    .with_provider_failure_json(
        r#"{"status_code":400,"error":{"message":"maximum context length exceeded","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
    );

    let recovered = service
        .recover_agent_provider_context_limit_failure(
            &AgentId::opaque("agent-%1").unwrap(),
            "turn-1",
            &error,
            1,
        )
        .unwrap();

    assert!(recovered);
    let queued_context = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(queued_context.contains("routing-context-pressure-"));
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some()
    );
    assert!(!service.agent_provider_task_is_pending("turn-1"));

    complete_runtime_test_compaction(&mut service, "%1", "model-authored routing context summary");
    let stored_context = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(stored_context.contains("model-authored routing context summary"));
    assert!(service.agent_provider_task_is_pending("turn-1"));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let pane_text_unwrapped = normalized_pane_log_text(&pane_text);
    assert!(
        pane_text_unwrapped.contains(
            "provider rejected context as too large; requesting model-backed context compaction"
        ),
        "{pane_text}"
    );
}

/// Creates an old claimed compaction and a newer queued compaction owned by a
/// replacement conversation in the same pane.
fn runtime_service_with_replacement_compaction() -> (RuntimeSessionService, String, u64, String, u64)
{
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "stale-compaction-regression".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-stale-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-test"]
default_model = "gpt-compact-test"
[model_profiles.compact-stale-test]
provider = "openai"
model = "gpt-compact-test"
context_window_tokens = 5000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("stale-compaction-regression"));
    for sequence in 1..=12 {
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "stale-compaction-original".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("original-{sequence} {}", "history ".repeat(1_500)),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "stale-compaction-original", 12)
        .unwrap();

    let queued = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"stale-compaction-original","method":"agent/shell/command","params":{"idempotency_key":"stale-compaction-original","input":"/compact"}}"#,
        &primary,
    );
    assert!(queued.contains("state=queued"), "{queued}");
    let old_task = service
        .take_pending_agent_compaction_task("%1")
        .expect("original conversation compaction task");
    let original_conversation_id = old_task.conversation_id.clone();
    let old_generation = old_task.task_generation;
    service.claim_agent_compaction_task_state("%1", old_task.clone());

    service
        .agent_shell_store_mut()
        .start_new_conversation("%1")
        .unwrap();
    let replacement_conversation_id = "stale-compaction-replacement".to_string();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", &replacement_conversation_id, 12)
        .unwrap();
    let mut replacement_task = old_task;
    replacement_task.task_generation = 0;
    replacement_task.compaction_epoch = 0;
    replacement_task.conversation_id = replacement_conversation_id.clone();
    service.queue_agent_compaction_task(replacement_task);
    let replacement_generation = service
        .pending_agent_compaction_task_generation("%1")
        .expect("replacement compaction generation");

    (
        service,
        original_conversation_id,
        old_generation,
        replacement_conversation_id,
        replacement_generation,
    )
}

/// An expired old claim cannot clear a replacement conversation's compaction
/// marker or publish a late summary into that replacement.
#[test]
fn runtime_compaction_claim_expiry_is_generation_and_conversation_fenced() {
    let (mut service, _, old_generation, replacement_conversation, replacement_generation) =
        runtime_service_with_replacement_compaction();
    assert!(
        service
            .expire_claimed_agent_compaction_task("%1", old_generation)
            .unwrap()
    );
    assert!(!service.agent_compaction_task_is_claimed("%1", old_generation));
    assert!(service.agent_is_compacting("%1"));
    assert_eq!(
        service.pending_agent_compaction_task_generation("%1"),
        Some(replacement_generation)
    );
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        replacement_conversation
    );
    assert!(
        !service
            .expire_claimed_agent_compaction_task("%1", old_generation)
            .unwrap()
    );
    let late = service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Completed {
            pane_id: "%1".to_string(),
            task_generation: old_generation,
            response: Box::new(runtime_test_compaction_response(
                "late summary must not publish",
            )),
        })
        .unwrap();
    assert!(!late.applied);
    assert!(service.agent_is_compacting("%1"));
}

/// Verifies a stale completion accounts usage to its original conversation
/// without settling or clearing replacement-conversation compaction work.
#[test]
fn runtime_agent_compaction_stale_completion_preserves_replacement_task() {
    let (
        mut service,
        original_conversation_id,
        old_generation,
        replacement_conversation_id,
        replacement_generation,
    ) = runtime_service_with_replacement_compaction();
    let replacement_task = service
        .take_pending_agent_compaction_task("%1")
        .expect("replacement compaction is queued");
    service.claim_agent_compaction_task_state("%1", replacement_task);
    assert_eq!(
        service.claimed_agent_compaction_task_generation("%1"),
        Some(replacement_generation)
    );
    assert!(
        service
            .claim_agent_compaction_task("%1", old_generation)
            .unwrap()
            .is_none()
    );

    let mut response = runtime_test_compaction_response("stale summary must be ignored");
    response.usage = mez_agent::ModelTokenUsage {
        input_tokens: 17,
        output_tokens: 3,
        reasoning_tokens: 0,
        cached_input_tokens: None,
        cache_write_input_tokens: None,
    };
    response.quota_usage = vec![mez_agent::ProviderQuotaUsage {
        name: "requests".to_string(),
        used_basis_points: 250,
        limit: 100,
        remaining: 98,
        reset: None,
    }];

    service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Completed {
            pane_id: "%1".to_string(),
            task_generation: old_generation,
            response: Box::new(response),
        })
        .unwrap();

    assert_eq!(
        service.claimed_agent_compaction_task_generation("%1"),
        Some(replacement_generation),
        "stale completion must preserve the replacement claim"
    );
    assert!(service.agent_is_compacting("%1"));
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.session_id, replacement_conversation_id);
    assert_eq!(session.transcript_entries, 12);
    let original_usage = service.agent_token_usage_for_conversation(&original_conversation_id);
    assert_eq!(original_usage.values().next().unwrap().input_tokens, 17);
    assert!(
        service
            .agent_token_usage_for_conversation(&replacement_conversation_id)
            .is_empty()
    );
    assert_eq!(
        service.agent_quota_usage_for_conversation(&original_conversation_id),
        &[mez_agent::ProviderQuotaUsage {
            name: "requests".to_string(),
            used_basis_points: 250,
            limit: 100,
            remaining: 98,
            reset: None,
        }]
    );
    assert!(
        service
            .agent_quota_usage_for_conversation(&replacement_conversation_id)
            .is_empty()
    );
}

/// Verifies a late completion remains attributable after its replacement task
/// has already settled, but duplicate delivery is ignored.
#[test]
fn runtime_agent_compaction_late_completion_accounts_after_replacement_settles() {
    let (
        mut service,
        original_conversation_id,
        old_generation,
        replacement_conversation_id,
        replacement_generation,
    ) = runtime_service_with_replacement_compaction();
    let replacement_task = service
        .take_pending_agent_compaction_task("%1")
        .expect("replacement compaction is queued");
    service.claim_agent_compaction_task_state("%1", replacement_task);

    service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Failed {
            pane_id: "%1".to_string(),
            task_generation: replacement_generation,
            kind: "forbidden".to_string(),
            message: "replacement provider failure".to_string(),
            provider_failure_json: None,
            provider_raw_text: None,
        })
        .unwrap();
    assert!(!service.agent_is_compacting("%1"));
    let transcript_entries = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .transcript_entries;

    let mut response = runtime_test_compaction_response("stale summary must be ignored");
    response.usage = mez_agent::ModelTokenUsage {
        input_tokens: 17,
        output_tokens: 3,
        reasoning_tokens: 0,
        cached_input_tokens: None,
        cache_write_input_tokens: None,
    };
    response.quota_usage = vec![mez_agent::ProviderQuotaUsage {
        name: "requests".to_string(),
        used_basis_points: 250,
        limit: 100,
        remaining: 98,
        reset: None,
    }];
    service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Completed {
            pane_id: "%1".to_string(),
            task_generation: old_generation,
            response: Box::new(response.clone()),
        })
        .unwrap();

    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.session_id, replacement_conversation_id);
    assert_eq!(session.transcript_entries, transcript_entries);
    assert!(!service.agent_is_compacting("%1"));
    let original_usage = service.agent_token_usage_for_conversation(&original_conversation_id);
    assert_eq!(original_usage.values().next().unwrap().input_tokens, 17);
    assert!(
        service
            .agent_token_usage_for_conversation(&replacement_conversation_id)
            .is_empty()
    );
    assert_eq!(
        service.agent_quota_usage_for_conversation(&original_conversation_id),
        response.quota_usage
    );
    assert!(
        service
            .agent_quota_usage_for_conversation(&replacement_conversation_id)
            .is_empty()
    );

    service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Completed {
            pane_id: "%1".to_string(),
            task_generation: old_generation,
            response: Box::new(response),
        })
        .unwrap();
    let original_usage = service.agent_token_usage_for_conversation(&original_conversation_id);
    assert_eq!(original_usage.values().next().unwrap().input_tokens, 17);
    assert_eq!(
        service
            .agent_quota_usage_for_conversation(&original_conversation_id)
            .len(),
        1
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        transcript_entries
    );
}

/// Verifies a stale failure does not clear replacement task ownership or its
/// compacting marker after the pane has switched conversations.
#[test]
fn runtime_agent_compaction_stale_failure_preserves_replacement_task() {
    let (mut service, _, old_generation, replacement_conversation_id, replacement_generation) =
        runtime_service_with_replacement_compaction();

    service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Failed {
            pane_id: "%1".to_string(),
            task_generation: old_generation,
            kind: "forbidden".to_string(),
            message: "stale failure must be ignored".to_string(),
            provider_failure_json: None,
            provider_raw_text: None,
        })
        .unwrap();

    let replacement = service
        .pending_agent_compaction_task_for_tests("%1")
        .expect("stale failure must preserve the replacement task");
    assert_eq!(replacement.task_generation, replacement_generation);
    assert_eq!(replacement.conversation_id, replacement_conversation_id);
    assert!(service.agent_is_compacting("%1"));
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.session_id, replacement_conversation_id);
    assert_eq!(session.transcript_entries, 12);
}

/// Verifies overlapping compaction attempts are rejected before they can start
/// another model request for the same pane.
#[test]
fn runtime_agent_shell_compact_rejects_overlapping_pane_compaction() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.mark_agent_compacting_for_tests("%1", 1);

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-overlap","method":"agent/shell/command","params":{"idempotency_key":"compact-overlap","input":"/compact"}}"#,
        &primary,
    );

    assert!(response.contains("cannot mutate pane state"), "{response}");
}

/// Verifies manual compaction owns the pane: plain text is retained for the
/// post-compaction epoch and conversation replacement is rejected meanwhile.
#[test]
fn runtime_manual_compaction_queues_steering_and_blocks_conversation_mutation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.mark_agent_compacting_for_tests("%1", 1);
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();

    let prompt = service
        .execute_agent_shell_command(&primary, "first steering prompt")
        .unwrap();
    assert!(prompt.contains("\"command\":\"compacting\""), "{prompt}");
    assert!(service.pending_agent_provider_tasks().is_empty());
    service
        .execute_agent_shell_command(&primary, "second steering prompt")
        .unwrap();

    let blocked = service
        .execute_agent_shell_command(&primary, "/new")
        .unwrap();
    assert!(blocked.contains("cannot mutate pane state"), "{blocked}");
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        conversation_id
    );
    let stopped = service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(stopped.contains("compaction_cancelled=true"), "{stopped}");
    assert!(!service.agent_is_compacting("%1"));
    let resumed = service.take_pending_agent_prompt_history();
    assert_eq!(resumed.len(), 1);
    assert_eq!(
        resumed[0].prompt,
        "first steering prompt\n\nsecond steering prompt"
    );
    assert!(service.agent_command_is_active("%1"));
    assert!(service.take_agent_compaction_steering("%1").is_empty());
}

/// Verifies cancelling a real queued compaction releases manual steering on
/// the unchanged replay epoch and ignores a late result from that generation.
#[test]
fn runtime_manual_compaction_cancellation_ignores_late_provider_result() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "manual-compaction-cancel".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "manual-cancel-test"
[providers.openai]
kind = "openai"
models = ["gpt-manual-cancel-test"]
default_model = "gpt-manual-cancel-test"
[model_profiles.manual-cancel-test]
provider = "openai"
model = "gpt-manual-cancel-test"
context_window_tokens = 5000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("manual-compaction-cancel"));
    for sequence in 1..=3 {
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "manual-cancel".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("cancel source {sequence}"),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-cancel", 3)
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-cancel","method":"agent/shell/command","params":{"idempotency_key":"manual-cancel","input":"/compact"}}"#,
        &primary,
    );
    assert!(compact.contains("state=queued"), "{compact}");
    let generation = service
        .pending_agent_compaction_task_generation("%1")
        .expect("queued compaction generation");
    let steering = service
        .execute_agent_shell_command(&primary, "continue after cancellation")
        .unwrap();
    assert!(
        steering.contains("\"command\":\"compacting\""),
        "{steering}"
    );

    let stopped = service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(stopped.contains("compaction_cancelled=true"), "{stopped}");
    assert!(!service.agent_is_compacting("%1"));
    let resumed = service.take_pending_agent_prompt_history();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].prompt, "continue after cancellation");

    let late = service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Completed {
            pane_id: "%1".to_string(),
            task_generation: generation,
            response: Box::new(runtime_test_compaction_response("must be ignored")),
        })
        .unwrap();
    assert!(!late.applied);
    assert!(!service.agent_is_compacting("%1"));
    assert_eq!(service.take_pending_agent_prompt_history().len(), 0);
    assert_eq!(service.agent_turn_ledger().turns().len(), 0);
}

/// Verifies prompts retained for one conversation are discarded when the pane
/// is rebound, rather than being resumed in a later conversation's compaction.
#[test]
fn runtime_manual_compaction_steering_does_not_cross_conversation_rebind() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let original_conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    service.queue_agent_compaction_steering(
        "%1",
        primary.clone(),
        original_conversation.clone(),
        service.agent_compaction_epoch("%1"),
        "private old-conversation instruction".to_string(),
    );

    let rebound = service
        .execute_agent_shell_command(&primary, "/new")
        .unwrap();
    assert!(rebound.contains("\"command\":\"new\""), "{rebound}");
    assert_ne!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        original_conversation
    );
    assert!(service.take_agent_compaction_steering("%1").is_empty());

    service.mark_agent_compacting_for_tests("%1", 1);
    service.cancel_current_agent_compaction_task("%1");
    assert!(!service.resume_agent_compaction_steering("%1").unwrap());
    assert!(service.take_pending_agent_prompt_history().is_empty());
}

/// Verifies pane compaction is a dispatch barrier for ordinary model work.
///
/// Action settlement or local messages can queue a continuation while model
/// compaction owns the pane. That continuation must remain queued without
/// becoming visible or claimable until compaction completion has rebuilt the
/// running context and cleared the pane's compacting marker.
#[test]
fn runtime_agent_compaction_blocks_provider_dispatch_until_context_is_ready() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compaction-dispatch-barrier","method":"agent/shell/command","params":{"idempotency_key":"compaction-dispatch-barrier","input":"continue after rebuilding context"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let task = pending[0].clone();
    let agent_id = AgentId::opaque(task.agent_id.clone()).unwrap();
    service.mark_agent_compacting_for_tests("%1", 1);

    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(service.agent_provider_task_is_pending(&task.turn_id));
    assert!(
        service
            .claim_configured_agent_provider_task(&agent_id, &task.turn_id)
            .unwrap()
            .is_none()
    );
    assert!(service.agent_provider_task_is_pending(&task.turn_id));

    let cleared = service.fail_current_agent_compaction_task("%1");
    assert!(cleared.had_task());
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
}

/// Verifies compaction keeps only a bounded raw transcript tail when the active
/// conversation is larger than the exact-reference window.
///
/// The compact memory can summarize older entries, but the next turn needs the
/// recent tail verbatim for prompts like "implement the first item". Older raw
/// messages should not remain in transcript replay after compaction.
#[test]
fn runtime_agent_shell_compact_retains_bounded_recent_transcript_tail() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-tail-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-tail-test"
compaction_raw_retention_percent = 2
[providers.openai]
kind = "openai"
models = ["gpt-compact-tail-test"]
default_model = "gpt-compact-tail-test"
[model_profiles.compact-tail-test]
provider = "openai"
model = "gpt-compact-tail-test"
context_window_tokens = 20000
max_output_tokens = 256
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact-tail"));
    for sequence in 1..=12 {
        let (role, content) = match sequence {
            1 => (
                mez_agent::transcript::TranscriptRole::Assistant,
                format!(
                    "old raw marker should be summary only {}",
                    "old-word ".repeat(28)
                ),
            ),
            11 => (
                mez_agent::transcript::TranscriptRole::Assistant,
                format!(
                    "Recent targets:\n1. Preserve raw tail after compaction.\n2. Keep memory summary. {}",
                    "recent-word ".repeat(28)
                ),
            ),
            _ if sequence % 2 == 0 => (
                mez_agent::transcript::TranscriptRole::Assistant,
                format!("filler user turn {sequence} {}", "tail-user ".repeat(28)),
            ),
            _ => (
                mez_agent::transcript::TranscriptRole::Assistant,
                format!(
                    "filler assistant turn {sequence} {}",
                    "tail-assistant ".repeat(28)
                ),
            ),
        };
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "as-tail".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content,
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as-tail", 12)
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-tail","method":"agent/shell/command","params":{"idempotency_key":"compact-tail","input":"/compact"}}"#,
        &primary,
    );

    assert!(compact.contains("state=queued"), "{compact}");
    assert!(compact.contains("summarized_entries=9"), "{compact}");
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "as-tail".to_string(),
            sequence: 13,
            created_at_unix_seconds: 13,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "turn-13".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "LATE_POST_PLAN_MANUAL_TRANSCRIPT_ENTRY".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();
    complete_runtime_test_compaction(&mut service, "%1", "old raw marker should be summary only");
    let persisted = transcript_store.inspect("as-tail").unwrap();
    assert!(
        persisted.iter().any(|entry| matches!(
            mez_agent::TranscriptContextEvent::from_transcript_content(&entry.content),
            Some(mez_agent::TranscriptContextEvent::McpCompactionEpoch)
        )),
        "{persisted:#?}\n{}",
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
    );
    assert!(
        persisted.iter().any(|entry| matches!(
            mez_agent::TranscriptContextEvent::from_transcript_content(&entry.content),
            Some(mez_agent::TranscriptContextEvent::PromptBoundary { label, .. })
                if label == "MCP compaction re-retrieval guidance"
        )),
        "{persisted:#?}"
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        6
    );

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-tail-prompt","method":"agent/shell/command","params":{"idempotency_key":"compact-tail-prompt","input":"Implement the first item"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    let compact_memory = context
        .blocks()
        .iter()
        .find(|block| {
            block
                .label
                .contains(&mez_agent::memory::canonical_memory_uuid("compact-as-tail"))
        })
        .expect("compact memory should be model-visible after /compact");
    assert!(
        compact_memory
            .content
            .contains("Older durable transcript entries were summarized"),
        "{compact_memory:?}"
    );
    let transcript_context = context
        .blocks()
        .iter()
        .filter(|block| {
            matches!(
                block.source,
                ContextSourceKind::Transcript
                    | ContextSourceKind::TranscriptUser
                    | ContextSourceKind::TranscriptAssistant
                    | ContextSourceKind::TranscriptTool
            )
        })
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        transcript_context.contains("1. Preserve raw tail after compaction."),
        "{transcript_context}"
    );
    assert!(
        transcript_context.contains("LATE_POST_PLAN_MANUAL_TRANSCRIPT_ENTRY"),
        "{transcript_context}"
    );
    assert!(
        !transcript_context.contains("old raw marker should be summary only"),
        "{transcript_context}"
    );
    assert!(
        context.blocks().iter().any(|block| {
            block.label == "MCP compaction re-retrieval guidance"
                && block.content.contains("mcp_server_get")
        }),
        "{:#?}",
        context.blocks()
    );
}

/// Verifies manual compaction reports an oversized unfinished exact tail instead
/// of silently claiming it fits or summarizing a partial execution group.
#[test]
fn runtime_manual_compaction_reports_irreducible_oversized_unfinished_tail() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "manual-unfinished-tail".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"manual-unfinished-tail\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.manual-unfinished-tail]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 10000\nmax_input_tokens = 2000\n".to_string(),
        }])
        .unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-unfinished-tail"));
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "manual-unfinished-tail".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "unfinished-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: format!(
                "EXACT_UNFINISHED_USER_REQUEST {}",
                "protected ".repeat(2_000)
            ),
        })
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-unfinished-tail", 1)
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-unfinished-tail","method":"agent/shell/command","params":{"idempotency_key":"manual-unfinished-tail","input":"/compact"}}"#,
        &primary,
    );

    assert!(
        compact.contains("reason=irreducible-exact-retained-tail"),
        "{compact}"
    );
    assert!(compact.contains("summarized_entries=0"), "{compact}");
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_none()
    );
    assert_eq!(
        store.compaction_epoch("manual-unfinished-tail").unwrap(),
        None
    );
    assert_eq!(store.inspect("manual-unfinished-tail").unwrap().len(), 1);
}

/// Verifies an oversized manual summary is rejected before the transcript
/// replay epoch or retained raw history is mutated.
#[test]
fn runtime_manual_compaction_rejects_oversized_post_summary_request() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-final-fit".to_string(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"manual-final-fit\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.manual-final-fit]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 10000\nmax_input_tokens = 2000\n".to_string(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-final-fit"));
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "manual-final-fit".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "compact this short durable history".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-final-fit", 1)
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-final-fit","method":"agent/shell/command","params":{"idempotency_key":"manual-final-fit","input":"/compact"}}"#,
        &primary,
    );
    assert!(compact.contains("state=queued"), "{compact}");
    let queued = service
        .pending_agent_compaction_task_for_tests("%1")
        .expect("manual compaction task");
    let source_words = queued
        .request
        .messages
        .last()
        .map(|message| mez_agent::model_context_text_word_count(&message.content))
        .unwrap_or_default()
        .max(1);
    assert!(queued.preserve_summary_output_budget);
    assert!(
        queued
            .request
            .max_output_tokens
            .is_some_and(|limit| limit <= source_words)
    );
    complete_runtime_test_compaction(
        &mut service,
        "%1",
        &"oversized summary ".repeat(source_words),
    );

    assert!(
        store
            .compaction_epoch("manual-final-fit")
            .unwrap()
            .is_none()
    );
    assert_eq!(store.inspect("manual-final-fit").unwrap().len(), 1);
    assert!(service.memory_records().iter().all(|record| {
        record.id != mez_agent::memory::canonical_memory_uuid("compact-manual-final-fit")
    }));
}

/// A configured input cap splits the frozen manual source before dispatch,
/// without publishing a partial conversation summary.
/// A manual candidate over its final-request cap retries the frozen source
/// with a smaller output ceiling and publishes only the accepted summary.
#[test]
fn runtime_manual_compaction_retries_oversized_final_request() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-final-retry".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"manual-final-retry\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.manual-final-retry]\nprovider = \"runtime-batch\"\nmodel = \"test\"\ncontext_window_tokens = 40000\nmax_input_tokens = 15000\n".to_string(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-final-retry"));
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "manual-final-retry".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "old-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "old decision ".repeat(800),
        })
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-final-retry", 1)
        .unwrap();
    let queued = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-retry","method":"agent/shell/command","params":{"idempotency_key":"manual-retry","input":"/compact"}}"#,
        &primary,
    );
    assert!(queued.contains("state=queued"), "{queued}");
    complete_runtime_test_compaction(&mut service, "%1", &"long summary ".repeat(700));
    assert!(
        store
            .compaction_epoch("manual-final-retry")
            .unwrap()
            .is_none()
    );
    let retry = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap_or_else(|| {
            panic!(
                "manual retry queued: {}",
                service
                    .pane_screen("%1")
                    .unwrap()
                    .normal_content_lines()
                    .join("\n")
            )
        });
    assert!(retry.manual_final_retry.is_some());
    complete_runtime_test_compaction(&mut service, "%1", "short manual summary");
    assert!(
        store
            .compaction_epoch("manual-final-retry")
            .unwrap()
            .is_some()
    );
}

/// A configured input cap splits the frozen manual source before dispatch,
/// without publishing a partial conversation summary.
#[test]
fn runtime_manual_compaction_splits_configured_input_cap() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-cap".to_string(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"manual-cap\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.manual-cap]\nprovider = \"openai\"\nmodel = \"test\"\ncontext_window_tokens = 128000\n".to_string(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-cap-split"));
    for sequence in 1..=3 {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "manual-cap-split".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!(
                    "SOURCE_{sequence} {} FINAL_SOURCE_SENTINEL",
                    "word ".repeat(if sequence == 3 { 10 } else { 25_000 })
                ),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(store.clone());
    service.set_auth_store(AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&temp_root("manual-cap-auth")),
    ));
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-cap-split", 3)
        .unwrap();
    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-cap","method":"agent/shell/command","params":{"idempotency_key":"manual-cap","input":"/compact"}}"#, &primary,
    );
    assert!(compact.contains("state=queued"), "{compact}");
    let original = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    let original_source = original.request.messages.last().unwrap().content.clone();
    let estimate = mez_agent::provider_request_input_estimate(
        &original.request,
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        &std::collections::BTreeMap::new(),
        false,
    )
    .unwrap();
    let cap = 25_000;
    assert!(estimate.input_tokens > cap, "{estimate:?}");
    service
        .pending_agent_compaction_task_mut_for_tests("%1")
        .unwrap()
        .model_profile
        .provider_options
        .insert("max_input_tokens".to_string(), cap.to_string());
    let generation = service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    let dispatch = service
        .claim_agent_compaction_task("%1", generation)
        .unwrap()
        .unwrap();
    assert!(
        dispatch
            .task
            .conversation_chunks
            .as_ref()
            .is_some_and(|chunks| !chunks.pending.is_empty())
    );
    let reduced = mez_agent::provider_request_input_estimate(
        &dispatch.task.request,
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        &std::collections::BTreeMap::new(),
        false,
    )
    .unwrap();
    assert!(!reduced.exceeds_explicit_cap(cap));
    assert!(
        store
            .compaction_epoch("manual-cap-split")
            .unwrap()
            .is_none()
    );
    let mut fragments = Vec::new();
    let mut current = dispatch.task;
    let logical_epoch = current.compaction_epoch;
    let steering = service
        .execute_agent_shell_command(&primary, "keep steering across chunks")
        .unwrap();
    assert!(
        steering.contains("\"command\":\"compacting\""),
        "{steering}"
    );
    for attempt in 0..32 {
        assert_eq!(current.compaction_epoch, logical_epoch);
        let source = current.request.messages.last().unwrap().content.clone();
        let synthesis = source.contains("Chunk 1 summary:");
        if !synthesis {
            fragments.push(source);
        }
        let summary = if synthesis {
            "FINAL_CAP_SUMMARY"
        } else {
            "bounded chunk summary"
        };
        assert!(
            service
                .apply_agent_compaction_transition(
                    crate::runtime::AgentCompactionEvent::Completed {
                        pane_id: "%1".to_string(),
                        task_generation: current.task_generation,
                        response: Box::new(runtime_test_compaction_response(summary)),
                    }
                )
                .unwrap()
                .applied
        );
        if synthesis {
            break;
        }
        assert!(
            store
                .compaction_epoch("manual-cap-split")
                .unwrap()
                .is_none()
        );
        let generation = service
            .pending_agent_compaction_task_generation("%1")
            .unwrap_or_else(|| panic!("manual synthesis was not queued after {attempt} responses"));
        current = service
            .claim_agent_compaction_task("%1", generation)
            .unwrap()
            .expect("next bounded chunk or synthesis")
            .task;
    }
    assert_eq!(fragments.concat(), original_source);
    assert!(
        store
            .compaction_epoch("manual-cap-split")
            .unwrap()
            .unwrap()
            .summary
            .contains("FINAL_CAP_SUMMARY")
    );
    let resumed = service.take_pending_agent_prompt_history();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].prompt, "keep steering across chunks");
}

/// A rejected manual compactor request must queue smaller temporary input
/// without changing the selected conversation's durable replay boundary.
#[test]
fn runtime_manual_compaction_recovers_provider_context_limit() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-compact-context-limit".to_string(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"manual-limit\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.manual-limit]\nprovider = \"openai\"\nmodel = \"test\"\ncontext_window_tokens = 128000\n".to_string(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-compact-context-limit"));
    for sequence in 1..=3 {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "manual-context-limit".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("MANUAL_SOURCE_{sequence} {}", "history ".repeat(100)),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-context-limit", 3)
        .unwrap();
    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-limit","method":"agent/shell/command","params":{"idempotency_key":"manual-limit","input":"/compact"}}"#,
        &primary,
    );
    assert!(compact.contains("state=queued"), "{compact}");
    let task = service.take_pending_agent_compaction_task("%1").unwrap();
    let logical_epoch = task.compaction_epoch;
    let task_generation = task.task_generation;
    let previous_bytes = mez_agent::openai_responses_request_body_with_stream(&task.request, false)
        .unwrap()
        .len();
    service.claim_agent_compaction_task_state("%1", task);
    let first_steering = service
        .execute_agent_shell_command(&primary, "steer before context retry")
        .unwrap();
    assert!(
        first_steering.contains("\"command\":\"compacting\""),
        "{first_steering}"
    );
    assert!(
        service
            .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Failed {
                pane_id: "%1".to_string(),
                task_generation,
                kind: "invalid_state".to_string(),
                message: "provider context length exceeded".to_string(),
                provider_failure_json: Some(
                    r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#.to_string(),
                ),
                provider_raw_text: None,
            })
            .unwrap()
            .applied
    );
    let retry_bytes = {
        let retry = service
            .pending_agent_compaction_task_for_tests("%1")
            .expect("smaller manual compactor request");
        assert_eq!(retry.compaction_epoch, logical_epoch);
        mez_agent::openai_responses_request_body_with_stream(&retry.request, false)
            .unwrap()
            .len()
    };
    assert!(
        retry_bytes < previous_bytes,
        "{retry_bytes} >= {previous_bytes}"
    );
    let retry_steering = service
        .execute_agent_shell_command(&primary, "steer between compaction chunks")
        .unwrap();
    assert!(
        retry_steering.contains("\"command\":\"compacting\""),
        "{retry_steering}"
    );
    assert_eq!(store.inspect("manual-context-limit").unwrap().len(), 3);
    assert!(
        store
            .compaction_epoch("manual-context-limit")
            .unwrap()
            .is_none()
    );
    let mut chunk_sources = Vec::new();
    let mut synthesized = false;
    for _ in 0..8 {
        let pending = service
            .take_pending_agent_compaction_task("%1")
            .expect("each temporary chunk and final synthesis is queued");
        assert_eq!(pending.compaction_epoch, logical_epoch);
        let task_generation = pending.task_generation;
        let source = pending.request.messages.last().unwrap().content.clone();
        let is_synthesis = source.contains("Chunk 1 summary:");
        chunk_sources.push(source);
        service.claim_agent_compaction_task_state("%1", pending);
        let summary = if is_synthesis {
            "FINAL_MANUAL_SUMMARY"
        } else {
            "temporary chunk summary"
        };
        assert!(
            service
                .apply_agent_compaction_transition(
                    crate::runtime::AgentCompactionEvent::Completed {
                        pane_id: "%1".to_string(),
                        task_generation,
                        response: Box::new(runtime_test_compaction_response(summary)),
                    }
                )
                .unwrap()
                .applied
        );
        if is_synthesis {
            synthesized = true;
            break;
        }
        assert!(
            store
                .compaction_epoch("manual-context-limit")
                .unwrap()
                .is_none()
        );
    }
    assert!(
        synthesized,
        "manual compaction never reached synthesis: {chunk_sources:?}"
    );
    assert!(
        chunk_sources
            .last()
            .unwrap()
            .contains("temporary chunk summary")
    );
    assert!(
        store
            .compaction_epoch("manual-context-limit")
            .unwrap()
            .unwrap()
            .summary
            .contains("FINAL_MANUAL_SUMMARY")
    );
    let resumed = service.take_pending_agent_prompt_history();
    assert_eq!(resumed.len(), 1);
    assert_eq!(
        resumed[0].prompt,
        "steer before context retry\n\nsteer between compaction chunks"
    );
}

/// A failure after one temporary chunk has completed must not publish its
/// partial summary or shorten the exact transcript replay boundary.
#[test]
fn runtime_manual_compaction_partial_chunk_failure_preserves_history() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-partial-failure".to_string(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"manual-limit\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.manual-limit]\nprovider = \"openai\"\nmodel = \"test\"\ncontext_window_tokens = 128000\n".to_string(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-partial-failure"));
    for sequence in 1..=3 {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "manual-partial-failure".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("source {sequence} {}", "word ".repeat(100)),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-partial-failure", 3)
        .unwrap();
    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-partial","method":"agent/shell/command","params":{"idempotency_key":"manual-partial","input":"/compact"}}"#,
        &primary,
    );
    assert!(compact.contains("state=queued"), "{compact}");
    let initial = service.take_pending_agent_compaction_task("%1").unwrap();
    service.claim_agent_compaction_task_state("%1", initial);
    service
        .apply_agent_compaction_failed_event(
            "%1",
            "invalid_state",
            "provider context length exceeded",
            Some(r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#),
        )
        .unwrap();
    complete_runtime_test_compaction(&mut service, "%1", "partial model summary");
    assert!(
        store
            .compaction_epoch("manual-partial-failure")
            .unwrap()
            .is_none()
    );
    let next = service.take_pending_agent_compaction_task("%1").unwrap();
    service.claim_agent_compaction_task_state("%1", next);
    assert!(service.apply_agent_compaction_failed_event(
        "%1", "forbidden", "authentication rejected", None,
    ).unwrap());
    assert!(service.pending_agent_compaction_tasks().is_empty());
    assert!(
        store
            .compaction_epoch("manual-partial-failure")
            .unwrap()
            .is_none()
    );
    assert_eq!(store.inspect("manual-partial-failure").unwrap().len(), 3);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        3
    );
}

/// The configured model token limit, rather than its word-scaled fallback,
/// determines which complete recent turn stays raw during manual compaction.
#[test]
fn runtime_manual_compaction_tail_uses_configured_token_allowance() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-token-tail".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"manual-token-tail\"\ncompaction_raw_retention_percent = 10\n[providers.openai]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.manual-token-tail]\nprovider = \"openai\"\nmodel = \"test\"\ncontext_window_tokens = 20000\n".to_string(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-token-tail"));
    for (sequence, content) in [(1, "old work".to_string()), (2, "x".repeat(6_000))] {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "manual-token-tail".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content,
            })
            .unwrap();
    }
    service.set_agent_transcript_store(store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-token-tail", 2)
        .unwrap();
    let queued = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-token-tail","method":"agent/shell/command","params":{"idempotency_key":"manual-token-tail","input":"/compact"}}"#,
        &primary,
    );
    assert!(queued.contains("summarized_entries=1"), "{queued}");
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert_eq!(task.retained_transcript_entries, 1);
}

/// Manual compaction may absorb a prior selective range only after summarizing
/// a complete prefix that contains it; its later raw tail stays available.
#[test]
fn runtime_manual_compaction_after_selective_epoch() {
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "manual-after-selective".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"selective-manual\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.selective-manual]\nprovider = \"openai\"\nmodel = \"test\"\ncontext_window_tokens = 40000\n".to_string(),
    }]).unwrap();
    let store = AgentTranscriptStore::new(temp_root("manual-after-selective"));
    let group = mez_agent::ContextExecutionGroupId::new("manual-prior-group").unwrap();
    for (sequence, content, turn_id, role) in [
        (
            1,
            "display answer".to_string(),
            "old",
            mez_agent::transcript::TranscriptRole::Assistant,
        ),
        (
            2,
            mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                ContextSourceKind::TranscriptAssistant,
                "answer",
                "old typed work",
                group,
                1,
                None,
            )
            .unwrap()
            .to_transcript_content(),
            "old",
            mez_agent::transcript::TranscriptRole::System,
        ),
        (
            3,
            "recent exact answer".to_string(),
            "recent",
            mez_agent::transcript::TranscriptRole::Assistant,
        ),
    ] {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "manual-selective".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role,
                turn_id: turn_id.to_string(),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content,
            })
            .unwrap();
    }
    store
        .save_compaction_ranges(
            "manual-selective",
            0,
            "",
            vec![crate::storage::transcript::AgentCompactionRange {
                first_sequence: 2,
                through_sequence: 2,
                summary: "prior typed summary".to_string(),
            }],
        )
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "manual-selective", 3)
        .unwrap();
    let queued = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"manual-selective","method":"agent/shell/command","params":{"idempotency_key":"manual-selective","input":"/compact"}}"#,
        &primary,
    );
    assert!(queued.contains("state=queued"), "{queued}");
    complete_runtime_test_compaction(
        &mut service,
        "%1",
        "new manual summary including prior typed summary",
    );
    let epoch = store.compaction_epoch("manual-selective").unwrap().unwrap();
    assert!(epoch.through_sequence >= 2, "{epoch:?}");
    assert!(epoch.ranges.is_empty(), "{epoch:?}");
    let replay = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(
        replay
            .blocks()
            .iter()
            .any(|block| block.content.contains("new manual summary"))
    );
    assert!(
        replay
            .blocks()
            .iter()
            .any(|block| block.content.contains("recent exact answer"))
    );
}

/// Verifies model-generated compacted context survives runtime restoration.
#[test]
fn runtime_agent_compaction_summary_survives_runtime_restore() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-restore-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-restore-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-restore-test"]
default_model = "gpt-compact-restore-test"
[model_profiles.compact-restore-test]
provider = "openai"
model = "gpt-compact-restore-test"
context_window_tokens = 128000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact-restore"));
    for sequence in 1..=3 {
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "as-restore".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("restore compact source {sequence}"),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as-restore", 3)
        .unwrap();
    service.checkpoint_agent_session_metadata().unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-restore","method":"agent/shell/command","params":{"idempotency_key":"compact-restore","input":"/compact"}}"#,
        &primary,
    );
    assert!(compact.contains("state=queued"), "{compact}");
    let steering = service
        .execute_agent_shell_command(&primary, "continue after compaction")
        .unwrap();
    assert!(
        steering.contains("\"command\":\"compacting\""),
        "{steering}"
    );
    let task = service
        .take_pending_agent_compaction_task("%1")
        .expect("manual compaction task");
    service.claim_agent_compaction_task_state("%1", task);
    assert!(
        service
            .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Completed {
                pane_id: "%1".to_string(),
                task_generation: service
                    .claimed_agent_compaction_task_generation("%1")
                    .unwrap(),
                response: Box::new(runtime_test_compaction_response(
                    "COMPACTION_SUMMARY_RESTORED_AFTER_RESTART"
                )),
            })
            .unwrap()
            .applied
    );
    let resumed = service.take_pending_agent_prompt_history();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].prompt, "continue after compaction");
    assert_eq!(resumed[0].transcript_entries, 4);
    let session_snapshot = service.session().clone();

    let mut restored = RuntimeServiceFixture::new().build_with_session(session_snapshot);
    restored.set_agent_transcript_store(transcript_store);
    restored
        .restore_agent_sessions_from_transcript_store()
        .unwrap();
    let context = restored
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(
        context.blocks().iter().any(|block| block
            .content
            .contains("COMPACTION_SUMMARY_RESTORED_AFTER_RESTART")),
        "restored history should include the durable compaction summary: {:#?}",
        context.blocks()
    );
}

/// A lost committed epoch must fail prompt construction rather than replaying
/// the pane checkpoint's shortened suffix without the summarized prefix.
#[test]
fn runtime_compaction_missing_epoch_fails_restored_prompt() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("runtime-missing-compaction-epoch"));
    for sequence in 1..=3 {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "missing-epoch".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("history {sequence}"),
            })
            .unwrap();
    }
    store
        .save_compaction_epoch("missing-epoch", 1, "durable summary")
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "missing-epoch", 2)
        .unwrap();
    service.checkpoint_agent_session_metadata().unwrap();
    let snapshot = service.session().clone();
    let mut restored = RuntimeServiceFixture::new().build_with_session(snapshot);
    restored.set_agent_transcript_store(store.clone());
    restored
        .restore_agent_sessions_from_transcript_store()
        .unwrap();
    let path = store
        .transcript_path("missing-epoch")
        .unwrap()
        .parent()
        .unwrap()
        .join("compaction-epoch.json");
    std::fs::remove_file(path).unwrap();
    assert!(
        restored
            .agent_context_for_pane_prompt("%1", "continue", 0)
            .unwrap_err()
            .message()
            .contains("required compaction epoch")
    );
}

/// A committed summary remains visible with no raw replay rows, and later
/// appends appear exactly once even if the pane's saved count is stale.
#[test]
fn runtime_compaction_epoch_replays_empty_tail_and_later_append() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("runtime-epoch-empty-tail"));
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "epoch-empty".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "OLDER_ARCHIVED_ROW".to_string(),
        })
        .unwrap();
    store
        .save_compaction_epoch("epoch-empty", 1, "DURABLE_EMPTY_TAIL_SUMMARY")
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "epoch-empty", 0)
        .unwrap();
    let before = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(
        before
            .blocks()
            .iter()
            .any(|block| block.content.contains("DURABLE_EMPTY_TAIL_SUMMARY"))
    );
    assert!(
        !before
            .blocks()
            .iter()
            .any(|block| block.content.contains("OLDER_ARCHIVED_ROW"))
    );
    let mut later = store.inspect("epoch-empty").unwrap().pop().unwrap();
    later.sequence = 2;
    later.content = "LATER_EXACT_ROW".to_string();
    store.append(&later).unwrap();
    let after = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert_eq!(
        after
            .blocks()
            .iter()
            .filter(|block| block.content.contains("LATER_EXACT_ROW"))
            .count(),
        1
    );
    assert!(
        after
            .blocks()
            .iter()
            .any(|block| block.content.contains("DURABLE_EMPTY_TAIL_SUMMARY"))
    );
    assert!(
        !after
            .blocks()
            .iter()
            .any(|block| block.content.contains("OLDER_ARCHIVED_ROW"))
    );
}

/// An empty pane replay count cannot conceal loss of the transcript that owns
/// the committed summary and its exact sequence boundary.
#[test]
fn runtime_compaction_epoch_missing_transcript_fails_empty_tail() {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("runtime-epoch-missing-transcript"));
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "missing-transcript".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "archived history".to_string(),
        })
        .unwrap();
    store
        .save_compaction_epoch("missing-transcript", 1, "required summary")
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "missing-transcript", 0)
        .unwrap();
    std::fs::remove_file(store.transcript_path("missing-transcript").unwrap()).unwrap();
    assert!(
        service
            .agent_context_for_pane_prompt("%1", "continue", 0)
            .is_err()
    );
}

/// Verifies explicit `/compact` is forced even when the entire transcript fits
/// inside the normal retained-tail budget.
///
/// The user command is a direct request to compact now, so it must summarize at
/// least one active durable entry instead of returning a budget-based no-op.
#[test]
fn runtime_agent_shell_compact_forces_summary_when_under_context_budget() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "compact-forced-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "compact-forced-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-forced-test"]
default_model = "gpt-compact-forced-test"
[model_profiles.compact-forced-test]
provider = "openai"
model = "gpt-compact-forced-test"
context_window_tokens = 128000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-compact-forced"));
    for sequence in 1..=3 {
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "as-forced".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("forced compact marker {sequence}"),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "as-forced", 3)
        .unwrap();

    let compact = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compact-forced","method":"agent/shell/command","params":{"idempotency_key":"compact-forced","input":"/compact"}}"#,
        &primary,
    );

    assert!(compact.contains(r#""kind":"mutated""#), "{compact}");
    assert!(compact.contains("state=queued"), "{compact}");
    assert!(compact.contains("summarized_entries=1"), "{compact}");
    assert!(
        !compact.contains("within-retained-context-tail"),
        "{compact}"
    );
    complete_runtime_test_compaction(&mut service, "%1", "forced compact marker 1");
    let compacted = service
        .memory_records()
        .into_iter()
        .find(|record| record.id == mez_agent::memory::canonical_memory_uuid("compact-as-forced"))
        .expect("compacted memory record");
    assert!(
        compacted.content.contains("forced compact marker 1"),
        "{}",
        compacted.content
    );
}

/// Verifies a non-reducing configured-cap pass retries instead of failing.
///
/// The cap measures estimated wire bytes while the compaction planner budgets
/// words, so a code-heavy context can leave a pass that replaces fewer bytes than
/// its summary adds. That pass must tighten its budget and retry while the bounded
/// allowance remains, and only the exhausted allowance may end the turn - never an
/// internal "did not reduce" inconsistency error.
#[cfg(any())]
#[test]
fn runtime_configured_input_cap_retries_a_non_reducing_pass() {
    use crate::runtime::RuntimeSessionService;

    let first = RuntimeSessionService::plan_configured_input_cap_pass(0, None, 30_000, 20_000)
        .expect("the first pass of a turn always proceeds");
    assert_eq!(first.pass, 1);
    assert!(!first.non_reducing);

    let reduced =
        RuntimeSessionService::plan_configured_input_cap_pass(1, Some(30_000), 25_000, 20_000)
            .expect("a reducing pass proceeds");
    assert_eq!(reduced.pass, 2);
    assert!(!reduced.non_reducing);

    let non_reducing =
        RuntimeSessionService::plan_configured_input_cap_pass(1, Some(30_000), 31_000, 20_000)
            .expect("a non-reducing pass retries with a tightened budget");
    assert_eq!(non_reducing.pass, 2);
    assert!(non_reducing.non_reducing);

    let exhausted =
        RuntimeSessionService::plan_configured_input_cap_pass(4, Some(30_000), 31_000, 20_000)
            .expect_err("the exhausted allowance must end the turn");
    let message = exhausted.message().to_string();
    assert!(
        message.contains("configured input cap cannot be satisfied"),
        "the terminal outcome must be typed as an unsatisfiable cap: {message}"
    );
    assert!(
        !message.contains("did not reduce"),
        "an internal consistency message must never surface for this condition: {message}"
    );

    assert_eq!(
        RuntimeSessionService::configured_input_cap_pass_budget(2_000, false),
        2_000,
        "an ordinary pass keeps the derived word budget"
    );
    assert_eq!(
        RuntimeSessionService::configured_input_cap_pass_budget(2_000, true),
        1_000,
        "a non-reducing pass retries against half the word budget"
    );
    assert_eq!(
        RuntimeSessionService::configured_input_cap_pass_budget(1, true),
        1,
        "a tightened budget never drops below one word"
    );
}

/// Verifies a durable block owned by another provider does not consume the
/// configured-cap summary budget when the recovery plans.
///
/// The configured-cap path plans through the active provider's rendered
/// projection, and request assembly never renders a block whose provider owner
/// does not match that provider. Without the projection, a foreign-owned native
/// block retained above the consumed boundary could bring the summary budget to
/// zero and fail the deferral with `no budget remains` instead of queueing the
/// compaction that makes the turn fit.
#[cfg(any())]
#[test]
fn runtime_configured_input_cap_plan_ignores_unrendered_provider_blocks() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "configured-input-cap-projection".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "configured-input-cap-test"
shell_mode = "pane"
[permissions]
sandbox = "policy-only"
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
# The cap must clear the fixed provider request overhead this fixture observes -
# about 22.9k tokens of system prompt and tool schemas - or the pass lands in the
# "cap smaller than fixed overhead" terminal before the projection is exercised.
[model_profiles.configured-input-cap-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 200000
max_input_tokens = 60000
"#
            .to_string(),
        }])
        .unwrap();
    let auth_root = temp_root("configured-input-cap-projection-auth");
    service.set_auth_store(AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&auth_root),
    ));
    let transcript_store =
        AgentTranscriptStore::new(temp_root("configured-input-cap-projection-history"));
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "configured-input-cap-projection-history".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::Assistant,
            turn_id: "turn-history".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: format!(
                "configured-cap-projection-marker {}",
                "compactable ".repeat(20_000)
            ),
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
        .agent_shell_store_mut()
        .bind_conversation("%1", "configured-input-cap-projection-history", 1)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"configured-input-cap-projection","input":"continue after proactive compaction"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service.pending_agent_provider_tasks().remove(0);
    let agent_id = AgentId::opaque(task.agent_id.clone()).unwrap();

    let foreign_content = mez_agent::ProviderTranscriptEvent::validated_openai_response_output(
        (0..3000)
            .map(|index| {
                serde_json::json!({
                    "type": "reasoning",
                    "id": format!("foreign-{index}"),
                    "text": "foreign provider continuity replay segment",
                })
            })
            .collect(),
    )
    .unwrap()
    .to_transcript_content();
    let foreign_owner = mez_agent::ProviderContinuityOwner::new(
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        "configured-openai",
    )
    .unwrap();
    let foreign_group =
        mez_agent::ContextExecutionGroupId::new("foreign-native-execution").unwrap();
    let turn_context = service
        .agent_turn_contexts_mut()
        .get_mut(&task.turn_id)
        .expect("the pending turn owns a durable context");
    turn_context
        .append_assistant_event(
            "foreign native turn",
            "foreign call emitted ahead of its transcript",
            foreign_group.clone(),
        )
        .unwrap();
    turn_context
        .append_evidence_event(
            mez_agent::ContextSourceKind::TranscriptTool,
            "foreign native response",
            foreign_content,
            foreign_group,
            Some(foreign_owner),
            false,
        )
        .unwrap();

    assert!(
        service
            .claim_configured_agent_provider_task(&agent_id, &task.turn_id)
            .unwrap()
            .is_none(),
        "the configured cap must still defer into compaction"
    );
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .is_some(),
        "the unrendered provider block must not exhaust the summary budget"
    );
}

/// Verifies the configured-cap word budget is measured rather than assumed.
///
/// The planner budgets words while the cap measures estimated provider tokens, so
/// the budget converts the measured token allowance at the context's own
/// words-per-token ratio: code-heavy text with few words per token gets a smaller
/// word budget than prose with the same allowance, which is what makes the planned
/// reduction match the measured one.
#[cfg(any())]
#[test]
fn runtime_configured_input_cap_budget_uses_the_measured_word_ratio() {
    assert_eq!(
        RuntimeSessionService::configured_input_cap_budget_words(1_000, 250, 1_000),
        250,
        "code-heavy text plans one word per four available tokens"
    );
    assert_eq!(
        RuntimeSessionService::configured_input_cap_budget_words(1_000, 750, 1_000),
        750,
        "prose plans three words per four available tokens"
    );
    assert_eq!(
        RuntimeSessionService::configured_input_cap_budget_words(1_000, 0, 0),
        0,
        "an empty rendered projection leaves no word budget"
    );
}
