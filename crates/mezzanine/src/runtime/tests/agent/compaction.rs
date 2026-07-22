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

/// Verifies large bracketed-paste agent prompt input is displayed compactly in
/// the pane transcript while the agent turn receives the exact pasted payload.
#[test]
fn runtime_agent_prompt_displays_large_paste_as_compact_block() {
    let mut service = test_runtime_service();
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
        RuntimeSideEffect::DispatchAgentCompaction { pane_id } if pane_id == "%1"
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
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let pane_text_unwrapped = pane_text.replace('\n', "").replace("▐ ", "");
    assert!(
        pane_text_unwrapped.contains(
            "provider rejected context as too large; requesting model-backed context compaction"
        ),
        "{pane_text}"
    );
}

/// Verifies a compactor context-limit failure retries with one newer complete
/// execution group moved unchanged from model input into the exact raw tail.
///
/// The rejected compaction request must not mutate active-turn context or
/// dispatch the original provider turn. A successful retry then summarizes
/// only the older group and leaves the excluded newer group byte-for-byte.
#[test]
fn runtime_model_compaction_context_limit_retries_with_exact_newer_group() {
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
        RuntimeSideEffect::DispatchAgentCompaction { pane_id } if pane_id == "%1"
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
    assert!(retry_source.contains("older-backoff-marker"));
    assert!(!retry_source.contains("newer-exact-marker"));
    let crate::runtime::agent_state::RuntimeAgentCompactionTarget::ActiveTurn {
        compaction_backoff_attempt,
        plan,
        ..
    } = &retry_task.target
    else {
        panic!("expected active-turn compaction target");
    };
    assert_eq!(*compaction_backoff_attempt, 1);
    assert!(
        plan.retained_tail()
            .iter()
            .any(|block| block.content.contains("newer-exact-marker"))
    );

    complete_runtime_test_compaction(&mut service, "%1", "model-authored older-group summary");
    let compacted = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(compacted.contains("model-authored older-group summary"));
    assert!(compacted.contains("newer-exact-marker"));
    assert!(!compacted.contains("older-backoff-marker"));
    assert!(service.agent_provider_task_is_pending("turn-1"));
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
    assert!(!service.agent_provider_task_is_pending("turn-1"));
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
    let pane_text_unwrapped = pane_text.replace('\n', "").replace("▐ ", "");
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
    let pane_text_unwrapped = pane_text.replace('\n', "").replace("▐ ", "");
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
    assert_eq!(requests[0].max_output_tokens, Some(4096));
    assert_eq!(requests[1].max_output_tokens, Some(4096));
    assert_eq!(requests[2].max_output_tokens, Some(16_384));
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
            .contains("provider response hit output limit; retrying with shorter-response"),
        "{pane_text}"
    );
    assert!(
        pane_text_unwrapped
            .contains("provider response hit output limit again; retrying compactly"),
        "{pane_text}"
    );
    assert!(
        pane_text_unwrapped.contains("max_output_tokens=16384"),
        "{pane_text}"
    );
}

/// Verifies automatic output-limit compaction refreshes a running turn and
/// queues provider continuation.
///
/// The first recovery stage for `response.incomplete/max_output_tokens` keeps
/// the active-turn context intact and asks for a compact MAAP retry. If that
/// bounded retry budget is exhausted, the runtime queues model-backed
/// conversation compaction while the turn remains running. Completion must
/// replace raw transcript replay with compact memory, retain the recent raw
/// tail, preserve the running turn, and queue provider work for the same task.
#[test]
fn runtime_output_limit_auto_compaction_completion_requeues_running_turn() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "output-limit-auto-compaction".to_string(),
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
context_window_tokens = 128000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-runtime-output-limit-auto-compact-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&transcript_root).unwrap();
    let transcript_store = AgentTranscriptStore::new(transcript_root);
    for sequence in 1..=4 {
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "output-limit-auto".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("durable prior output-limit entry {sequence}"),
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
        .bind_conversation("%1", "output-limit-auto", 4)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-output-limit-auto-compact","method":"agent/shell/command","params":{"idempotency_key":"agent-output-limit-auto-compact","input":"continue with the current implementation"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let context = service.agent_turn_contexts_mut().get_mut("turn-1").unwrap();
    insert_test_context_block(
        context,
        ContextBlock::reference_event(
            ContextSourceKind::LocalMessage,
            "local message compaction barrier",
            "local message must remain after the active prompt",
        ),
    );
    insert_test_context_block(
        context,
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "synthetic same-turn result".to_string(),
            content: "same-turn result must survive compaction refresh".to_string(),
        },
    );
    let retained_labels = [
        "user prompt",
        "local message compaction barrier",
        "synthetic test assistant action",
        "synthetic same-turn result",
    ];
    let retained_before = canonical_event_oracle(context)
        .into_iter()
        .filter(|event| retained_labels.contains(&event.label.as_str()))
        .collect::<Vec<_>>();
    let error =
        MezError::invalid_state("OpenAI stream returned an incomplete response: max_output_tokens")
            .with_provider_failure_json(r#"{"incomplete_details":{"reason":"max_output_tokens"}}"#);

    let queued = service
        .queue_agent_output_limit_recovery_compaction(
            &AgentId::opaque("agent-%1").unwrap(),
            "turn-1",
            &error,
        )
        .unwrap();

    assert!(queued);
    assert!(
        service
            .pending_agent_compaction_task_for_tests("%1")
            .and_then(|task| task.resume_turn_id.as_deref())
            == Some("turn-1")
    );
    assert!(!service.agent_provider_task_is_pending("turn-1"));

    complete_runtime_test_compaction(&mut service, "%1", "summary after output-limit exhaustion");

    let retained_after =
        canonical_event_oracle(service.agent_turn_contexts().get("turn-1").unwrap())
            .into_iter()
            .filter(|event| retained_labels.contains(&event.label.as_str()))
            .collect::<Vec<_>>();
    assert_eq!(retained_after, retained_before);

    assert!(service.agent_provider_task_is_pending("turn-1"));
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        Some("turn-1")
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        3
    );
    let stored_context = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        stored_context.contains("summary after output-limit exhaustion"),
        "{stored_context}"
    );
    assert!(
        stored_context
            .contains("Older durable transcript entries were summarized into this compact memory"),
        "{stored_context}"
    );
    assert!(
        stored_context.contains("durable prior output-limit entry 4"),
        "{stored_context}"
    );
    assert!(
        !stored_context.contains("durable prior output-limit entry 1"),
        "{stored_context}"
    );
    assert!(
        stored_context.contains("same-turn result must survive compaction refresh"),
        "{stored_context}"
    );
    let blocks = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks();
    let summary_index = blocks
        .iter()
        .position(|block| {
            block
                .content
                .contains("summary after output-limit exhaustion")
        })
        .unwrap();
    let retained_raw_index = blocks
        .iter()
        .position(|block| block.content.contains("durable prior output-limit entry 4"))
        .unwrap();
    let prompt_index = blocks
        .iter()
        .position(|block| block.label == "user prompt")
        .unwrap();
    let local_message_index = blocks
        .iter()
        .position(|block| block.label == "local message compaction barrier")
        .unwrap();
    let same_turn_result_index = blocks
        .iter()
        .position(|block| block.label == "synthetic same-turn result")
        .unwrap();
    assert!(summary_index < retained_raw_index);
    assert!(retained_raw_index < prompt_index);
    assert!(prompt_index < local_message_index);
    assert!(local_message_index < same_turn_result_index);
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
    let pane_text_unwrapped = pane_text.replace('\n', "").replace("▐ ", "");
    assert!(
        pane_text_unwrapped.contains(
            "provider rejected context as too large; requesting model-backed context compaction"
        ),
        "{pane_text}"
    );
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

    assert!(response.contains("already compacting"), "{response}");
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
[providers.openai]
kind = "openai"
models = ["gpt-compact-tail-test"]
default_model = "gpt-compact-tail-test"
[model_profiles.compact-tail-test]
provider = "openai"
model = "gpt-compact-tail-test"
context_window_tokens = 5000
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
            8 => (
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
    service.set_agent_transcript_store(transcript_store);
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
    assert!(compact.contains("summarized_entries=5"), "{compact}");
    complete_runtime_test_compaction(&mut service, "%1", "old raw marker should be summary only");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        7
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
        .find(|block| block.label.contains("compact-as-tail"))
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
        !transcript_context.contains("old raw marker should be summary only"),
        "{transcript_context}"
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
        .find(|record| record.id == "compact-as-forced")
        .expect("compacted memory record");
    assert!(
        compacted.content.contains("forced compact marker 1"),
        "{}",
        compacted.content
    );
}
