//! Model Context tests for assembly behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;
use crate::{AgentPromptResult, ModelRequest, append_mcp_context};

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
/// Verifies DeepSeek system prompts point to repository guidance reinforced in user context.
///
/// DeepSeek receives repository instructions at the front of the first user
/// message for provider adherence, but section 3 must still contain prompt
/// material explaining where those active instructions live so the model does
/// not treat the empty section as permission to reread instruction files.
fn assemble_model_request_points_deepseek_system_prompt_to_user_repository_instructions() {
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
                label: "active repository instructions".to_string(),
                content: "Run just test before handoff.".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "fix the prompt".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let system = &request.messages[0].content;
    let first_user = request
        .messages
        .iter()
        .find(|message| message.role == ModelMessageRole::User)
        .unwrap();

    assert!(system.contains("3. Repository Instructions"));
    assert!(system.contains("DeepSeek provider note"));
    assert!(system.contains("first user message"));
    assert!(!system.contains("Run just test before handoff."));
    assert!(
        first_user
            .content
            .starts_with("Active repository instructions:")
    );
    assert!(first_user.content.contains("Run just test before handoff."));
    assert!(first_user.content.contains("fix the prompt"));
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
                label: "previous provider-native event".to_string(),
                content: event_content.clone(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
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
/// Verifies provider request assembly carries runtime MCP availability into
/// the system prompt instead of leaving the prompt profile at its empty
/// defaults.
///
/// The selected model reads both the system prompt and the `[mcp integrations]`
/// context block. If these disagree, the model can treat MCP as unavailable
/// even though concrete `mcp_call` schemas are exposed later by the runner.
fn assemble_model_request_system_prompt_uses_mcp_context_availability() {
    let context = AgentContext::new(vec![ContextBlock {
        source: ContextSourceKind::UserInstruction,
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
        label: "user".to_string(),
        content: "Continue from the existing command history.".to_string(),
    }];
    for index in 0..8 {
        blocks.push(ContextBlock {
            source: ContextSourceKind::ActionResult,
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
            .all(|message| message.source != ContextSourceKind::EvidenceLedger)
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
                label: "policy".to_string(),
                content: "approval_policy=ask".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::LocalMessage,
                label: "local message".to_string(),
                content: "from=agent-%2".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::RuntimeHint,
                label: "runtime hint".to_string(),
                content: "[action pressure]\nPrefer validation now.".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::Transcript,
                label: "history".to_string(),
                content: "previous output".to_string(),
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
    assert_eq!(request.messages[2].source, ContextSourceKind::LocalMessage);
    assert_eq!(request.messages[3].source, ContextSourceKind::RuntimeHint);
    assert_eq!(request.messages[3].role, ModelMessageRole::Developer);
    assert_eq!(request.messages[4].source, ContextSourceKind::Transcript);
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
            label: format!("action result {index}"),
            content: format!("result-{index} {}", "action-result-word ".repeat(12_000)),
        });
    }
    blocks.push(ContextBlock {
        source: ContextSourceKind::UserInstruction,
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
                source: ContextSourceKind::ActionResult,
                label: "action result".to_string(),
                content: "volatile result".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                label: "project guidance".to_string(),
                content: "stable guidance".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
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
            .all(|message| message.source != ContextSourceKind::EvidenceLedger)
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
                label: "project guidance".to_string(),
                content: "stable guidance".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: "user".to_string(),
                content: "verify the file exists".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
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
            .all(|message| message.source != ContextSourceKind::EvidenceLedger)
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
