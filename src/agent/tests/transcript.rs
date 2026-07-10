//! Agent tests for transcript behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies durable skill action results keep metadata, not skill text.
///
/// `request_skills` and `call_skill` action bodies can contain complete
/// catalogs or full `SKILL.md` documents. Transcript storage should retain a
/// compact audit summary without letting those workflow instructions become
/// future context payload.
fn skill_action_result_transcript_content_summarizes_skill_payloads() {
    let turn = turn();
    let call_action = AgentAction {
        id: "skill-1".to_string(),
        rationale: "load review skill".to_string(),
        payload: AgentActionPayload::CallSkill {
            name: "review".to_string(),
            additional_context: Some("focus src".to_string()),
        },
    };
    let call_result = ActionResult::succeeded(
        &turn,
        &call_action,
        vec!["# Skill: review\n\nDo a deep review.".to_string()],
        Some(
            serde_json::json!({
                "name": "review",
                "source": "project",
                "path": "/repo/.mez/skills/review/SKILL.md",
                "skill_bytes": 1024,
                "additional_context_bytes": 9,
            })
            .to_string(),
        ),
    );
    let call_transcript = action_result_transcript_content(&call_result);

    assert!(call_transcript.contains("action_type=call_skill"));
    assert!(call_transcript.contains("name=review"));
    assert!(call_transcript.contains("skill_bytes=1024"));
    assert!(!call_transcript.contains("# Skill:"), "{call_transcript}");
    assert!(
        !call_transcript.contains("Do a deep review"),
        "{call_transcript}"
    );

    let catalog_action = AgentAction {
        id: "catalog-1".to_string(),
        rationale: "discover skills".to_string(),
        payload: AgentActionPayload::RequestSkills,
    };
    let catalog_result = ActionResult::succeeded(
        &turn,
        &catalog_action,
        vec!["Available skills:\n- review (project) - long description".to_string()],
        Some(
            serde_json::json!({
                "skills": [
                    {
                        "name": "review",
                        "description": "long description that should not persist",
                        "source": "project",
                        "path": "/repo/.mez/skills/review/SKILL.md",
                    }
                ],
                "diagnostics": [],
            })
            .to_string(),
        ),
    );
    let catalog_transcript = action_result_transcript_content(&catalog_result);

    assert!(catalog_transcript.contains("action_type=request_skills"));
    assert!(catalog_transcript.contains("skills=1"));
    assert!(catalog_transcript.contains("names=review"));
    assert!(
        !catalog_transcript.contains("long description"),
        "{catalog_transcript}"
    );
    assert!(!catalog_transcript.contains("Available skills"));
}

#[test]
/// Verifies turn execution persistence appends to durable transcript store.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn turn_execution_persistence_appends_to_durable_transcript_store() {
    let root =
        std::env::temp_dir().join(format!("mez-agent-turn-persistence-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = AgentTranscriptStore::new(root);
    let turn = turn();
    let execution = AgentTurnExecution {
        request: assemble_model_request(
            &ModelProfile {
                provider: "openai".to_string(),
                model: "default".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
            &turn,
            &AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "run pwd".to_string(),
            }])
            .unwrap(),
        )
        .unwrap(),
        response: ModelResponse {
            provider: "openai".to_string(),
            model: "default".to_string(),
            raw_text: "I will inspect the directory.".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![ActionResult::succeeded(
            &turn,
            &shell_action("a1"),
            vec!["/repo\n".to_string()],
            Some(r#"{"exit_code":0}"#.to_string()),
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };

    let first = persist_turn_execution_transcript(&store, "conv1", 200, &turn, &execution).unwrap();
    let second =
        persist_turn_execution_transcript(&store, "conv1", 201, &turn, &execution).unwrap();
    let persisted = store.inspect("conv1").unwrap();

    assert_eq!(first[0].sequence, 1);
    assert_eq!(second[0].sequence, first.len() as u64 + 1);
    assert_eq!(persisted.len(), first.len() + second.len());
    assert!(persisted.iter().any(|entry| {
        entry.role == TranscriptRole::Tool && entry.content.contains("exit_code")
    }));
}

#[test]
/// Verifies expanded skill context is not persisted as user transcript text.
///
/// Skill bodies are execution-time workflow context. Durable transcripts should
/// keep the user's explicit `$skill ...` prompt, but not the expanded `SKILL.md`
/// content that Mezzanine injected into the model request for that turn.
fn turn_execution_transcript_omits_expanded_skill_request_context() {
    let turn = turn();
    let context = AgentContext::new(vec![
        ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "explicit skill review".to_string(),
            content:
                "# Skill: review\n\nSource: project\nPath: skills/review/SKILL.md\n\nReview workflow."
                    .to_string(),
        },
            ContextBlock {
                source: ContextSourceKind::RuntimeHint,
                label: "explicit skill invocation review".to_string(),
                content: "[explicit skill invocation resolved]\nskill=review".to_string(),
            },
        ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user prompt".to_string(),
            content: "$review focus src/lib.rs".to_string(),
        },
    ])
    .unwrap();
    let execution = AgentTurnExecution {
        request: assemble_model_request(
            &ModelProfile {
                provider: "openai".to_string(),
                model: "default".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
            &turn,
            &context,
        )
        .unwrap(),
        response: ModelResponse {
            provider: "openai".to_string(),
            model: "default".to_string(),
            raw_text: "Working on it.".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    let entries = transcript_entries_for_execution("conv1", 1, 200, &turn, &execution).unwrap();
    let user_entries = entries
        .iter()
        .filter(|entry| entry.role == TranscriptRole::User)
        .collect::<Vec<_>>();

    assert_eq!(user_entries.len(), 1, "{entries:?}");
    assert_eq!(user_entries[0].content, "$review focus src/lib.rs");
    assert!(
        entries.iter().all(|entry| {
            !entry.content.contains("# Skill:")
                && !entry.content.contains("explicit skill invocation resolved")
        }),
        "{entries:?}"
    );
}

#[test]
/// Verifies transcript persistence does not recursively store prompt context.
///
/// Request messages can include prior transcript excerpts, legacy passive
/// context blocks, and system/developer scaffolding. Persisting those request
/// messages back to the transcript recursively multiplies prompt context across
/// continuations, so durable storage keeps only the current user instruction
/// plus the execution's assistant and tool records.
fn turn_execution_transcript_omits_recursive_request_context() {
    let turn = turn();
    let recursive_payload = "[recent transcript recursive-payload]\n".repeat(128);
    let context = AgentContext::new(vec![
        ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user prompt".to_string(),
            content: "create test.txt".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::ActionResult,
            label: "legacy passive terminal context".to_string(),
            content: "terminal prompt and previous output".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::Transcript,
            label: "recent transcript for pane %1".to_string(),
            content: recursive_payload.clone(),
        },
    ])
    .unwrap();
    let execution = AgentTurnExecution {
        request: assemble_model_request(
            &ModelProfile {
                provider: "openai".to_string(),
                model: "default".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
            &turn,
            &context,
        )
        .unwrap(),
        response: ModelResponse {
            provider: "openai".to_string(),
            model: "default".to_string(),
            raw_text: "Working on it.".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    let entries = transcript_entries_for_execution("conv1", 1, 200, &turn, &execution).unwrap();

    assert_eq!(entries[0].role, TranscriptRole::User);
    assert_eq!(entries[0].content, "create test.txt");
    assert!(
        entries
            .iter()
            .all(|entry| !entry.content.contains("recursive-payload")),
        "{entries:?}"
    );
    assert!(
        entries
            .iter()
            .all(|entry| !entry.content.contains("visible terminal for pane")),
        "{entries:?}"
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry.role != TranscriptRole::System),
        "{entries:?}"
    );
}

#[test]
/// Verifies conversational `say` output is preserved as assistant history.
///
/// Follow-up prompts often refer to numbered lists or suggested changes the
/// assistant previously printed. Persisting only the compact MAAP action
/// summary loses that referent, so user-visible say text must remain intact in
/// the assistant transcript entry while transient batch/action rationale stays
/// out of durable assistant history.
fn turn_execution_transcript_preserves_visible_say_text() {
    let turn = turn();
    let visible_text = [
        "Suggested changes:",
        "1. Keep conversation history role-aware.",
        "2. Preserve prior assistant lists for follow-up references.",
        "3. Continue summarizing non-conversational action payloads.",
        "4. Add regression coverage for item references.",
    ]
    .join("\n");
    let execution = AgentTurnExecution {
        request: assemble_model_request(
            &ModelProfile {
                provider: "openai".to_string(),
                model: "default".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
            &turn,
            &AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user prompt".to_string(),
                content: "list suggested changes".to_string(),
            }])
            .unwrap(),
        )
        .unwrap(),
        response: ModelResponse {
            provider: "openai".to_string(),
            model: "default".to_string(),
            raw_text: r#"{"rationale":"test action batch rationale","actions":[{"type":"say","status":"final","text":"Suggested changes..."}]}"#.to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![say_action("say-1", &visible_text)],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };

    let entries = transcript_entries_for_execution("conv1", 1, 200, &turn, &execution).unwrap();
    let assistant = entries
        .iter()
        .find(|entry| entry.role == TranscriptRole::Assistant)
        .unwrap();

    assert_eq!(assistant.content, visible_text);
    assert!(
        assistant
            .content
            .contains("2. Preserve prior assistant lists")
    );
    assert!(!assistant.content.contains("thinking: reply to user"));
    assert!(!assistant.content.contains("say text="));
}

#[test]
/// Verifies provider-native replay metadata is durable but not visible.
///
/// DeepSeek thinking-mode tool calls require the original assistant
/// `reasoning_content`, native `tool_calls`, and matching `role: tool` result
/// to be available on later requests. Mezzanine stores those as hidden system
/// transcript entries so visible assistant and tool transcript records remain
/// provider-neutral and do not expose raw provider JSON.
fn turn_execution_transcript_stores_hidden_provider_native_tool_call_events() {
    let turn = turn();
    let action = shell_action("a1");
    let execution = AgentTurnExecution {
        request: assemble_model_request(
            &ModelProfile {
                provider: "deepseek".to_string(),
                model: "deepseek-v4-pro".to_string(),
                reasoning_profile: Some("high".to_string()),
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
            &turn,
            &AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "run pwd".to_string(),
            }])
            .unwrap(),
        )
        .unwrap(),
        response: ModelResponse {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            raw_text: "executing".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: vec![ProviderTranscriptEvent::DeepSeekAssistantToolCall {
                content: "".to_string(),
                reasoning_content: Some("I need command output.".to_string()),
                tool_calls: vec![serde_json::json!({
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                        "arguments": "{}"
                    }
                })],
            }],
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![ActionResult::running(
            &turn,
            &action,
            vec!["shell command accepted for pane execution".to_string()],
            None,
        )],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    let entries = transcript_entries_for_execution("conv1", 1, 200, &turn, &execution).unwrap();
    let hidden_events = entries
        .iter()
        .filter(|entry| entry.role == TranscriptRole::System)
        .map(|entry| ProviderTranscriptEvent::from_transcript_content(&entry.content).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(hidden_events.len(), 2);
    assert!(matches!(
        hidden_events[0],
        ProviderTranscriptEvent::DeepSeekAssistantToolCall { .. }
    ));
    assert!(matches!(
        hidden_events[1],
        ProviderTranscriptEvent::DeepSeekToolResult { .. }
    ));
    let ProviderTranscriptEvent::DeepSeekToolResult {
        tool_call_id,
        content,
    } = &hidden_events[1]
    else {
        panic!("expected DeepSeek tool-result event");
    };
    assert_eq!(tool_call_id, "call_1");
    assert!(content.contains("[action_result a1 shell_command running]"));
    let visible = entries
        .iter()
        .filter(|entry| entry.role != TranscriptRole::System)
        .map(|entry| entry.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!visible.contains("reasoning_content"));
    assert!(!visible.contains("call_1"));
}

#[test]
/// Verifies assistant transcript entries summarize MAAP action batches without
/// retaining inline patch payloads from raw provider JSON.
///
/// File-content actions can carry large generated content in the model
/// response. Durable transcript storage should preserve the action shape and
/// payload size while omitting raw protocol text so later context assembly does
/// not replay or multiply the file bytes.
fn turn_execution_transcript_summarizes_maap_action_batches() {
    let turn = turn();
    let inline_content = "large-inline-file-content\n".repeat(64);
    let patch = format!(
        "*** Begin Patch\n*** Add File: note.txt\n{}*** End Patch",
        inline_content
            .lines()
            .map(|line| format!("+{line}\n"))
            .collect::<String>()
    );
    let action = AgentAction {
        id: "patch-1".to_string(),
        rationale: "write note file".to_string(),
        payload: AgentActionPayload::ApplyPatch {
            patch: patch.clone(),
            strip: None,
        },
    };
    let execution = AgentTurnExecution {
        request: assemble_model_request(
            &ModelProfile {
                provider: "openai".to_string(),
                model: "default".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
            &turn,
            &AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "create note.txt".to_string(),
            }])
            .unwrap(),
        )
        .unwrap(),
        response: ModelResponse {
            provider: "openai".to_string(),
            model: "default".to_string(),
            raw_text: format!(
                r#"{{"rationale":"test action batch rationale","actions":[{{"type":"apply_patch","patch":{}}}]}}"#,
                serde_json::to_string(&patch).unwrap()
            ),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: Some(
                    "The patch summary belongs in future model context.\nDo not show this in normal logs."
                        .to_string(),
                ),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    let entries = transcript_entries_for_execution("conv1", 1, 200, &turn, &execution).unwrap();
    let assistant = entries
        .iter()
        .find(|entry| entry.role == TranscriptRole::Assistant)
        .unwrap();

    assert!(
        assistant
            .content
            .contains("[assistant emitted MAAP actions"),
        "{}",
        assistant.content
    );
    assert!(
        !assistant
            .content
            .contains("thinking: test action batch rationale"),
        "{}",
        assistant.content
    );
    assert!(
        assistant
            .content
            .contains("thinking: The patch summary belongs in future model context."),
        "{}",
        assistant.content
    );
    assert!(
        assistant
            .content
            .contains("thinking: Do not show this in normal logs."),
        "{}",
        assistant.content
    );
    assert!(
        !assistant.content.contains("thinking: write note file"),
        "{}",
        assistant.content
    );
    assert!(assistant.content.contains("apply_patch patch_bytes="));
    assert!(!assistant.content.contains("\"actions\""));
    assert!(!assistant.content.contains("large-inline-file-content"));
}

#[test]
/// Verifies empty provider text still produces a durable assistant transcript entry.
///
/// Some OpenAI-compatible backends can return a response object without visible
/// text when no tool call or MAAP batch was produced. Transcript persistence
/// forbids empty content, so the assistant transcript projection must synthesize
/// a bounded placeholder instead of failing the entire turn cleanup path.
fn turn_execution_transcript_synthesizes_placeholder_for_empty_assistant_response() {
    let turn = turn();
    let execution = AgentTurnExecution {
        request: assemble_model_request(
            &ModelProfile {
                provider: "openai".to_string(),
                model: "default".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
            &turn,
            &AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user prompt".to_string(),
                content: "respond with a MAAP action batch".to_string(),
            }])
            .unwrap(),
        )
        .unwrap(),
        response: ModelResponse {
            provider: "openai".to_string(),
            model: "default".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    let entries = transcript_entries_for_execution("conv1", 1, 200, &turn, &execution).unwrap();
    let assistant = entries
        .iter()
        .find(|entry| entry.role == TranscriptRole::Assistant)
        .unwrap();

    assert_eq!(
        assistant.content,
        "[assistant response contained no visible content]"
    );
    assistant.validate().unwrap();
}
