//! Model Context tests for assembly behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;
use crate::{
    AgentPromptResult, AgentRequestAssemblyErrorKind, McpPromptServer, McpPromptSummary,
    McpPromptTool, ModelRequest, append_mcp_context,
};

/// Synthetic root-turn identity retained by moved exact-name tests.
struct TestTurnIdentity;

/// Returns the stable synthetic turn identity used by assembly policy tests.
fn turn() -> TestTurnIdentity {
    TestTurnIdentity
}

/// Synthetic prompt source for provider-independent assembly policy tests.
struct TestPromptAssets;

impl AgentPromptAssetSource for TestPromptAssets {
    fn system_fragment<'a>(&'a self, path: &str) -> AgentPromptResult<&'a str> {
        Ok(match path {
            "identity.md" => "profile {profile_name} version {profile_version}; pane shell",
            "repository_instructions.md" => "3. Repository Instructions\n{repository_instructions}",
            "actions.md" => {
                "6. Actions\nAfter action results, inspect the result content first. Use shell_command for local inspection; semantic actions do not replace validation."
            }
            "mcp.md" => {
                "Concrete MCP server and tool metadata is not globally exposed. Use `@<mcp-server-name>` in a submitted prompt or loaded skill."
            }
            "subagents.md" => "subagent contract",
            _ => "generic contract",
        })
    }

    fn provider_fragment<'a>(&'a self, _path: &str) -> AgentPromptResult<&'a str> {
        Ok("provider contract")
    }
}

/// Adapts the former root helper signature to lower request assembly.
fn assemble_model_request(
    profile: &ModelProfile,
    _turn: &TestTurnIdentity,
    context: &AgentContext,
) -> AgentRequestAssemblyResult<ModelRequest> {
    assemble_model_request_from_context(
        profile,
        ModelRequestIdentity {
            turn_id: "turn-1",
            agent_id: "agent-1",
            pane_id: "%1",
        },
        context,
        &TestPromptAssets,
    )
}

#[test]
/// Verifies DeepSeek system prompts point to fixed-position repository guidance.
///
/// DeepSeek receives repository instructions in a dedicated user message after
/// the system prompt, while ordinary user transcript content remains unchanged.
/// Section 3 still explains where those active instructions live so the model
/// does not treat the empty section as permission to reread instruction files.
fn assemble_model_request_points_deepseek_system_prompt_to_neutral_repository_instructions() {
    let request = assemble_model_request(
        &ModelProfile {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            reasoning_profile: Some("high".to_string()),
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                placement: crate::ContextPlacement::StablePrefix,
                label: "active repository instructions".to_string(),
                content: "Run just test before handoff.".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "fix the prompt".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let system = &request.messages[0].content;
    let repository_guidance = &request.messages[1];
    let user_prompt = &request.messages[2];

    assert!(system.contains("3. Repository Instructions"));
    assert!(system.contains("DeepSeek provider note"));
    assert!(system.contains("dedicated neutral-context message"));
    assert!(!system.contains("Run just test before handoff."));
    assert!(
        repository_guidance
            .content
            .starts_with("Active repository instructions:")
    );
    assert_eq!(
        repository_guidance.source,
        ContextSourceKind::ProjectGuidance
    );
    assert!(
        repository_guidance
            .content
            .contains("Run just test before handoff.")
    );
    assert_eq!(user_prompt.source, ContextSourceKind::UserInstruction);
    assert_eq!(user_prompt.content, "[user]\nfix the prompt");
}

#[test]
/// Verifies DeepSeek repository guidance remains provider-visible without an ordinary user prompt.
///
/// Dedicated placement must not depend on finding a mutable user transcript
/// entry, because compaction and internal request modes can omit one.
fn assemble_model_request_keeps_deepseek_repository_guidance_without_user_prompt() {
    let request = assemble_model_request(
        &ModelProfile {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            reasoning_profile: Some("high".to_string()),
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::ProjectGuidance,
            placement: crate::ContextPlacement::StablePrefix,
            label: "active repository instructions".to_string(),
            content: "Run just test before handoff.".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();

    assert_eq!(request.messages.len(), 2);
    assert_eq!(request.messages[1].role, ModelMessageRole::Context);
    assert_eq!(
        request.messages[1].source,
        ContextSourceKind::ProjectGuidance
    );
    assert!(
        request.messages[1]
            .content
            .contains("Run just test before handoff.")
    );
}

#[test]
/// Verifies request assembly carries hidden provider-native transcript events
/// without wrapping them in normal context labels.
///
/// Provider replay markers are not natural-language prompt context. They must
/// remain byte-stable hidden payloads so DeepSeek can decode and render them as
/// native Chat Completions messages, while other provider renderers can omit
/// them safely.
fn assemble_model_request_preserves_hidden_provider_transcript_events_without_labeling() {
    let turn = turn();
    let event_content = ProviderTranscriptEvent::DeepSeekToolResult {
        tool_call_id: "call_1".to_string(),
        content: "action_id=a1 status=success".to_string(),
    }
    .to_transcript_content();
    let request = assemble_model_request(
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
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "previous provider-native event".to_string(),
                content: event_content.clone(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "continue".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let event_message = request
        .messages
        .iter()
        .find(|message| {
            ProviderTranscriptEvent::from_transcript_content(&message.content).is_some()
        })
        .unwrap();

    assert_eq!(event_message.role, ModelMessageRole::System);
    assert_eq!(event_message.content, event_content);
    assert!(
        !event_message
            .content
            .contains("previous provider-native event")
    );
}

#[test]
/// Verifies a DeepSeek-owned continuity event is omitted by every non-owner
/// provider without moving or relabeling surrounding canonical events.
///
/// DeepSeek is currently the only adapter with native continuity payloads.
/// The switch matrix therefore projects the same stored chronology through
/// OpenAI, Anthropic, Claude Code, and an OpenAI-compatible provider and proves
/// each receives the neutral user event but never the opaque DeepSeek record.
fn assemble_model_request_omits_deepseek_continuity_for_all_nonowners() {
    let event_content = ProviderTranscriptEvent::DeepSeekAssistantToolCall {
        content: "native assistant call".to_string(),
        reasoning_content: Some("native reasoning".to_string()),
        tool_calls: vec![serde_json::json!({
            "id": "call_1",
            "type": "function",
            "function": {"name": "submit_maap_action_batch", "arguments": "{}"}
        })],
    }
    .to_transcript_content();
    let context = AgentContext::new(vec![
        ContextBlock {
            source: ContextSourceKind::Transcript,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "deepseek native event".to_string(),
            content: event_content.clone(),
        },
        ContextBlock::user_event("user prompt", "continue after the switch"),
    ])
    .unwrap();

    for provider in ["openai", "anthropic", "claude-code", "compatible-chat"] {
        let request = assemble_model_request(
            &ModelProfile {
                provider: provider.to_string(),
                model: "test-model".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
            &turn(),
            &context,
        )
        .unwrap();

        assert!(
            request
                .messages
                .iter()
                .all(|message| message.content != event_content),
            "{provider} received DeepSeek-owned continuity"
        );
        assert!(
            request
                .messages
                .iter()
                .any(|message| message.content.contains("continue after the switch")),
            "{provider} lost the provider-neutral user event"
        );
    }
}

#[test]
/// Verifies runtime MCP availability does not mutate stable system instructions.
///
/// The selected model reads both the system prompt and the `[mcp integrations]`
/// context block. If these disagree, the model can treat MCP as unavailable
/// even though concrete `mcp_call` schemas are exposed later by the runner.
fn assemble_model_request_keeps_mcp_availability_out_of_system_prompt() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "user".to_string(),
        content: "use @gitlab to inspect an issue".to_string(),
    }])
    .unwrap();
    let context = append_mcp_context(
        context,
        &McpPromptSummary {
            available_servers: vec![McpPromptServer {
                server_id: "gitlab".to_string(),
                display_name: "GitLab".to_string(),
                purpose: "GitLab issue and merge request operations".to_string(),
                usage_instructions: "Use for GitLab issue and merge request tasks.".to_string(),
                tool_count: 1,
                approval_required_tool_count: 0,
            }],
            available_tools: vec![McpPromptTool {
                server_id: "gitlab".to_string(),
                tool_name: "get_issue".to_string(),
                description: "Read one GitLab issue".to_string(),
                approval_required: false,
                input_schema_json: r#"{"type":"object"}"#.to_string(),
            }],
            unavailable_servers: vec![crate::McpPromptUnavailableServer {
                server_id: "jira".to_string(),
                purpose: "Jira issue operations".to_string(),
                usage_instructions: "Use for Jira issue tasks.".to_string(),
                reason: "startup failed".to_string(),
                retryable: true,
            }],
        },
    )
    .unwrap();

    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &context,
    )
    .unwrap();
    let system_prompt = &request.messages[0].content;

    assert!(
        system_prompt.contains("Concrete MCP server and tool metadata is not globally exposed"),
        "{system_prompt}"
    );
    assert!(
        system_prompt.contains("Use `@<mcp-server-name>` in a submitted prompt or loaded skill"),
        "{system_prompt}"
    );
    assert!(
        !system_prompt.contains("Current availability:"),
        "{system_prompt}"
    );
}

#[test]
/// Verifies provider request assembly no longer generates a synthetic helper
/// block for prior action history.
fn model_request_does_not_generate_evidence_ledger_block() {
    let mut blocks = vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "user".to_string(),
        content: "Continue from the existing command history.".to_string(),
    }];
    for index in 0..8 {
        blocks.push(ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: crate::ContextPlacement::ConversationAppend,
            label: format!("action result {index}"),
            content: format!(
                "[action_result action-{index} shell_command succeeded]\ncommand: git status --short path-{index}\noutput:\nhistory evidence {index}"
            ),
        });
    }

    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "default".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(blocks).unwrap(),
    )
    .unwrap();

    assert!(
        request
            .messages
            .iter()
            .all(|message| !message.content.contains("[evidence ledger]"))
    );
}

#[test]
/// Verifies model request keeps context sources distinct.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn model_request_keeps_context_sources_distinct() {
    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "default".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::Policy,
                placement: crate::ContextPlacement::StablePrefix,
                label: "policy".to_string(),
                content: "approval_policy=ask".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "history".to_string(),
                content: "previous output".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::LocalMessage,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "local message".to_string(),
                content: "from=agent-%2".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::RuntimeHint,
                placement: crate::ContextPlacement::EphemeralTail,
                label: "runtime hint".to_string(),
                content: "cwd=/repo".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    assert_eq!(request.messages[0].role, ModelMessageRole::System);
    assert!(request.messages[0].content.contains("pane shell"));
    assert!(request.messages[0].content.contains("6. Actions"));
    assert!(
        request.messages[0]
            .content
            .contains("After action results, inspect the result content first"),
        "{}",
        request.messages[0].content
    );
    assert!(
        request.messages[0]
            .content
            .contains("Use shell_command for local inspection"),
        "{}",
        request.messages[0].content
    );
    assert!(
        request.messages[0]
            .content
            .contains("semantic actions do not"),
        "{}",
        request.messages[0].content
    );
    assert_eq!(request.messages[1].role, ModelMessageRole::Developer);
    assert_eq!(request.messages[2].source, ContextSourceKind::Transcript);
    assert_eq!(request.messages[3].source, ContextSourceKind::LocalMessage);
    assert_eq!(request.messages[4].source, ContextSourceKind::RuntimeHint);
    assert_eq!(request.messages[4].role, ModelMessageRole::Context);
}

#[test]
/// Verifies returned skill catalogs do not re-enable model-selected skill actions.
///
/// Model-authored skill discovery and loading are currently disabled. Historical
/// skill-catalog action results may still appear in transcript context, but
/// they must not make `call_skill` available again.
fn model_request_keeps_skill_actions_disabled_after_skill_catalog_result() {
    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "default".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "action result skill-catalog".to_string(),
            content: "[action_result skill-catalog request_skills succeeded]\n- create-skill"
                .to_string(),
        }])
        .unwrap(),
    )
    .unwrap();

    assert_eq!(
        request.allowed_actions.action_type_names(),
        vec!["say", "request_capability"]
    );
}

#[test]
/// Verifies request assembly does not compact older context merely because a
/// local estimate crosses a threshold.
///
/// Provider-reported response usage and provider context-limit failures are the
/// source of truth for context-size decisions, so normal request assembly should
/// preserve recoverable action details and the newest task direction.
fn model_request_preserves_action_results_before_provider_feedback() {
    let mut blocks = Vec::new();
    for index in 0..6 {
        blocks.push(ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: crate::ContextPlacement::ConversationAppend,
            label: format!("action result {index}"),
            content: format!("result-{index} {}", "action-result-word ".repeat(12_000)),
        });
    }
    blocks.push(ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "user".to_string(),
        content: "recent instruction must remain exact".to_string(),
    });

    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "default".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(blocks).unwrap(),
    )
    .unwrap();

    let user_message = request
        .messages
        .iter()
        .find(|message| message.source == ContextSourceKind::UserInstruction)
        .expect("user instruction context should remain present");

    assert!(
        request
            .messages
            .iter()
            .any(|message| message.content.contains("result-0")),
        "older action result should remain until provider feedback requires compaction"
    );
    assert!(
        user_message
            .content
            .contains("recent instruction must remain exact")
    );
    assert!(!user_message.content.contains("[context compacted]"));
    assert!(
        request
            .messages
            .iter()
            .all(|message| !message.content.contains("[context compacted]"))
    );
}

#[test]
/// Verifies request assembly rejects every cache-lifecycle regression.
///
/// Reordering malformed context would change transcript and tool-event
/// semantics, so the canonical provider boundary must fail before projection.
fn model_request_rejects_context_lifecycle_regressions() {
    let regressions = [
        (
            crate::ContextPlacement::EphemeralTail,
            crate::ContextPlacement::ConversationAppend,
        ),
        (
            crate::ContextPlacement::ConversationAppend,
            crate::ContextPlacement::StablePrefix,
        ),
        (
            crate::ContextPlacement::EphemeralTail,
            crate::ContextPlacement::StablePrefix,
        ),
    ];

    for (first, second) in regressions {
        let error = assemble_model_request(
            &ModelProfile {
                provider: "openai".to_string(),
                model: "default".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
            &turn(),
            &AgentContext::new(vec![
                ContextBlock {
                    source: ContextSourceKind::RuntimeHint,
                    placement: first,
                    label: "first block".to_string(),
                    content: "first".to_string(),
                },
                ContextBlock {
                    source: ContextSourceKind::UserInstruction,
                    placement: second,
                    label: "regressing block".to_string(),
                    content: "second".to_string(),
                },
            ])
            .unwrap(),
        )
        .unwrap_err();

        assert_eq!(error.kind(), AgentRequestAssemblyErrorKind::InvalidArgs);
        assert!(error.message().contains("block index 1"));
        assert!(error.message().contains("regressing block"));
        assert!(error.message().contains("UserInstruction"));
        assert!(error.message().contains(&format!("placement={second:?}")));
        assert!(
            error
                .message()
                .contains(&format!("entered_phase={first:?}"))
        );
    }
}

#[test]
/// Verifies provider request assembly preserves observed context order while
/// still embedding project guidance into the system prompt.
///
/// Action results appended after a user instruction are execution evidence for
/// that instruction, so request assembly must not move the user instruction
/// behind the action result and make the completed work look stale.
fn model_request_preserves_context_observation_order() {
    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "default".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                placement: crate::ContextPlacement::StablePrefix,
                label: "project guidance".to_string(),
                content: "stable guidance".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "action result".to_string(),
                content: "volatile result".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "latest request".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let sources = request
        .messages
        .iter()
        .map(|message| message.source)
        .collect::<Vec<_>>();
    assert_eq!(
        sources,
        vec![
            ContextSourceKind::System,
            ContextSourceKind::ActionResult,
            ContextSourceKind::UserInstruction,
        ]
    );
    assert!(request.messages[0].content.contains("stable guidance"));
    assert!(
        request
            .messages
            .iter()
            .all(|message| !message.content.contains("[evidence ledger]"))
    );

    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "default".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                placement: crate::ContextPlacement::StablePrefix,
                label: "project guidance".to_string(),
                content: "stable guidance".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "verify the file exists".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "action result".to_string(),
                content: "test -s file && git status succeeded".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let sources = request
        .messages
        .iter()
        .map(|message| message.source)
        .collect::<Vec<_>>();
    assert_eq!(
        sources,
        vec![
            ContextSourceKind::System,
            ContextSourceKind::UserInstruction,
            ContextSourceKind::ActionResult,
        ]
    );
    assert!(request.messages[0].content.contains("stable guidance"));
    assert!(
        request
            .messages
            .iter()
            .all(|message| !message.content.contains("[evidence ledger]"))
    );
}

#[test]
/// Verifies provider request assembly preserves context until provider feedback
/// proves that compaction is required.
///
/// Local fallback accounting is intentionally not used as a preflight gate. An
/// oversized provider request should be sent as assembled, and provider
/// context-limit recovery is responsible for compacting before a retry.
fn model_request_preserves_oversized_context_until_provider_feedback() {
    let huge_content = format!("provider-visible-marker {}", "x".repeat(256 * 1024));
    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "default".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "action result".to_string(),
            content: huge_content,
        }])
        .unwrap(),
    )
    .unwrap();

    let action_message = request
        .messages
        .iter()
        .find(|message| message.source == ContextSourceKind::ActionResult)
        .expect("action result should be sent without request-local compaction");
    assert!(action_message.content.contains("provider-visible-marker"));
    assert!(
        request
            .messages
            .iter()
            .all(|message| !message.content.contains("[context compacted]"))
    );
}

#[test]
/// Verifies loaded skill bodies narrow the model's concrete action surface.
///
/// Explicit `$skill` prompt expansion has already placed the workflow in
/// context. The next provider request should guide the model toward using the
/// loaded instructions or requesting an execution capability, not toward
/// rediscovering or reloading the same skill.
fn model_request_suppresses_skill_actions_when_skill_context_loaded() {
    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "default".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "explicit skill create-skill".to_string(),
            content: "# Skill: create-skill\n\nCreate or update skills.".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();

    assert_eq!(
        request.allowed_actions.action_type_names(),
        vec!["say", "request_capability"]
    );
}
