//! Runtime tests for agent prompt lifecycle behavior.

use super::*;

/// Verifies shorthand prompt words still resolve to the read-only subagent mode.
///
/// Provider prompts describe cooperation mode as a safety/scope concept, and
/// some models may echo those words back even when the compact spawn schema
/// omits the explicit field. Accepting these shorthands keeps runtime subagent
/// spawns compatible with that model behavior instead of failing validation.
#[test]
fn runtime_cooperation_mode_accepts_prompt_shorthand_scope_words() {
    assert_eq!(
        runtime_cooperation_mode("safety").unwrap(),
        CooperationMode::ExploreOnly
    );
    assert_eq!(
        runtime_cooperation_mode("scope").unwrap(),
        CooperationMode::ExploreOnly
    );
    assert_eq!(
        runtime_cooperation_mode("scoped").unwrap(),
        CooperationMode::ExploreOnly
    );
}

/// Verifies a spawned subagent pane records the exact parent prompt before the
/// child turn starts.
///
/// Parent-authored task text is the child agent's effective user instruction.
/// Showing it as a `parent>` log entry lets users inspect the child pane
/// without reconstructing the prompt from parent-pane status messages.
#[test]
fn runtime_subagent_spawn_logs_parent_prompt_in_child_pane() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "explorer".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::ExploreOnly,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: false,
        write_scopes: Vec::new(),
        write_scopes_defaulted: false,
        task_prompt: "inspect the renderer issue".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: false,
    };

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            spawn,
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    assert!(spawned.contains(r#""id":"turn-1""#), "{spawned}");
    let child_pane_id = serde_json::from_str::<serde_json::Value>(&spawned)
        .unwrap()
        .get("pane")
        .and_then(|pane| pane.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .expect("spawned pane id")
        .to_string();
    let child_text = service
        .pane_screen(&child_pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");

    assert!(
        child_text.contains("parent> inspect the renderer issue"),
        "{child_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies runtime agent shell prompt starts live turn lifecycle.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_agent_shell_prompt_starts_live_turn_lifecycle() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt","input":"summarize the pane"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"turn_started""#), "{response}");
    assert!(response.contains(r#""command":null"#), "{response}");
    assert!(response.contains(r#""body":null"#), "{response}");
    assert!(response.contains(r#""state":"running""#), "{response}");
    let response_json: serde_json::Value = serde_json::from_str(&response).unwrap();
    let turn = &response_json["result"]["turn"];
    assert_eq!(turn["id"], "turn-1", "{response}");
    assert_eq!(turn["version"], serde_json::json!(1), "{response}");
    assert_eq!(turn["agent_id"], "agent-%1", "{response}");
    assert_eq!(turn["state"], "running", "{response}");
    assert!(turn["created_at"].as_str().is_some(), "{response}");
    assert!(turn["started_at"].as_str().is_some(), "{response}");
    assert_eq!(turn["finished_at"], serde_json::Value::Null, "{response}");
    assert_eq!(turn["prompt_preview"], "summarize the pane", "{response}");
    assert_eq!(turn["approval_ids"], serde_json::json!([]), "{response}");
    assert_eq!(
        turn["result_summary"],
        serde_json::Value::Null,
        "{response}"
    );
    assert!(
        turn["extensions"]["context_blocks"].as_u64().is_some(),
        "{response}"
    );
    let tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"tasks","method":"agent/task/list","params":{"target":{"pane_id":"%1"}}}"#,
        &primary,
    );
    assert!(tasks.contains(r#""id":"turn-1""#), "{tasks}");
    assert!(tasks.contains(r#""state":"running""#), "{tasks}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].turn_id, "turn-1");
    assert_eq!(pending[0].model_profile.provider, "openai");
    assert_eq!(pending[0].model_profile.model, "gpt-5.6-sol");
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("agent: working on"), "{pane_text}");
}

/// Verifies that a user prompt and a non-command agent response are written
/// into the pane's normal terminal buffer instead of a transient prompt
/// overlay. This preserves the Codex-like interaction transcript as copyable
/// terminal text while still retaining terminal style spans for user-facing
/// color. Each injected line keeps the same Mezzanine UI prefix used by the
/// pane-local prompt so message boundaries are visible in the terminal buffer.
#[test]
fn runtime_agent_prompt_and_say_response_are_interleaved_in_pane_buffer() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-visible-say","input":"summarize visible output"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap say response".to_string(),
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
                    id: "say-1".to_string(),
                    rationale: "answer in the pane".to_string(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: "The pane is ready.".to_string(),
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
    assert!(
        pane_text.contains("user> summarize visible output"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("▐ user> summarize visible output"),
        "{pane_text}"
    );
    assert!(pane_text.contains("mez> The pane is ready."), "{pane_text}");
    assert!(
        pane_text.contains("▐ mez> The pane is ready."),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("mez> answer in the pane"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("agent: turn turn-1"), "{pane_text}");
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let assistant_line = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("mez> The pane is ready."))
        .unwrap();
    assert!(assistant_line.text.starts_with("▐ "));
    assert!(!assistant_line.style_spans.is_empty());
    let assistant_body_start = "▐ mez> ".chars().count();
    assert!(
        assistant_line
            .style_spans
            .iter()
            .all(|span| span.start.saturating_add(span.length) <= assistant_body_start),
        "assistant body text should use default terminal color: {:?}",
        assistant_line.style_spans
    );
    assert!(
        assistant_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground
                    == Some(theme.colors.agent_transcript_assistant.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "assistant gutter and label should use themed foreground without a background: {:?}",
        assistant_line.style_spans
    );
    let user_line = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("user> summarize visible output"))
        .unwrap();
    let user_body_start = "▐ user> ".chars().count();
    assert!(
        user_line
            .style_spans
            .iter()
            .all(|span| span.start.saturating_add(span.length) <= user_body_start),
        "user prompt body text should use default terminal color: {:?}",
        user_line.style_spans
    );
    assert!(
        user_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground == Some(theme.colors.agent_transcript_user.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "user gutter and label should use themed foreground without a background: {:?}",
        user_line.style_spans
    );
    service
        .append_agent_error_text_to_terminal_buffer("%1", "agent error: failed")
        .unwrap();
    service
        .append_agent_command_preview_to_terminal_buffer("%1", "ls -la")
        .unwrap();
    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let error_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent error: failed"))
        .unwrap();
    assert!(
        error_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground == Some(theme.colors.agent_transcript_error.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "error transcript lines should use themed error foreground without a background: {:?}",
        error_line.style_spans
    );
    let command_line = styled_lines
        .iter()
        .find(|line| line.text.contains("$ ls -la"))
        .unwrap();
    assert!(
        command_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground
                    == Some(theme.colors.agent_transcript_command.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "command transcript lines should use themed command foreground without a background: {:?}",
        command_line.style_spans
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies visible-pane user prompt transcript lines wrap to the bounded pane
/// width with a sixth-column hanging indent for continuation rows.
///
/// Long user-entered transcript lines should use the same bounded renderer as
/// other visible pane logs so they stay within the pane width or the 120-column
/// cap. Wrapped continuation rows align with the `mez> ` continuation column
/// instead of repeating the `user> ` label so the copied transcript remains
/// readable.
#[test]
fn runtime_user_prompt_logs_wrap_with_sixth_column_hanging_indent() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(24, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(24, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    service
        .append_agent_user_prompt_to_terminal_buffer("%1", "alpha beta gamma delta epsilon")
        .unwrap();

    let user_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .into_iter()
        .filter(|line| line.starts_with("▐ "))
        .collect::<Vec<_>>();
    assert!(
        user_lines.iter().any(|line| line == "▐ user> alpha beta"),
        "{user_lines:#?}"
    );
    assert!(
        user_lines.iter().any(|line| line == "▐      gamma delta"),
        "{user_lines:#?}"
    );
    assert!(
        user_lines.iter().any(|line| line == "▐      epsilon"),
        "{user_lines:#?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies pasted provider diagnostics remain normal prompt text.
///
/// Users often paste the previous terminal failure back into the agent shell for
/// diagnosis. That text can contain JSON error payloads, wrapped words, and the
/// provider_error marker, but it is still user-authored prompt content. The
/// runtime should render it through the agent transcript presentation path
/// without surfacing a secondary terminal presentation failure.
#[test]
fn runtime_agent_user_prompt_renders_pasted_provider_error_without_terminal_failure() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 12).unwrap(), 120).unwrap(),
    );
    let prompt = "provider_error: InvalidState: OpenAI Responses-compatible provider `lmstudio` is not authenticated\nInvalidState: terminal step failed: {\"code\":-32004,\n\"data\":{\"mezzanine_code\":\"invalid_state\"},\"message\":\"agent terminal presentation feed panicked while appending styled agent\n lines\"}";

    service
        .append_agent_user_prompt_to_terminal_buffer("%1", prompt)
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("provider_error: InvalidState"),
        "{pane_text}"
    );
    assert!(pane_text.contains("terminal step failed"), "{pane_text}");
}
