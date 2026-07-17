//! Agent tests for openai cache behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies current-turn action results remain after the latest user request
/// while historical tool transcript entries stay reusable stable prefix
/// context.
///
/// Execution evidence for the active instruction must stay in the volatile
/// suffix so the provider sees it after the latest user request and does not
/// reuse it as immutable prefix material.
fn openai_current_action_results_remain_volatile_suffix() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let request = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "historical tool result".to_string(),
                content: "action_id=action-3\noutput: cached evidence".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "use the prior output".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "action result".to_string(),
                content: "action_id=action-4\noutput: fresh evidence".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();
    let body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&request).unwrap()).unwrap();
    let input = body["input"].as_array().unwrap();
    let user_index = input
        .iter()
        .position(|message| {
            message["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("use the prior output"))
        })
        .expect("user instruction should be rendered into input");
    let action_index = input
        .iter()
        .position(|message| {
            message["content"][0]["text"].as_str().is_some_and(|text| {
                text.contains("[executed result transcript entry]")
                    && text.contains("fresh evidence")
            })
        })
        .expect("current action result should be rendered into input");
    assert!(action_index > user_index);
    let prefix = openai_stable_projection_material_for_request(&request).unwrap();
    assert!(prefix.contains("[executed result transcript entry]"));
    assert!(prefix.contains("cached evidence"));
    assert!(!prefix.contains("fresh evidence"));
    assert!(input.iter().any(|message| {
        message["content"][0]["text"].as_str().is_some_and(|text| {
            text.contains("[executed result transcript entry]") && text.contains("cached evidence")
        })
    }));
    assert!(input.iter().any(|message| {
        message["content"][0]["text"].as_str().is_some_and(|text| {
            text.contains("[executed result transcript entry]") && text.contains("fresh evidence")
        })
    }));
    let diagnostics = openai_prompt_cache_diagnostics_for_request(&request).unwrap();
    assert!(diagnostics.stable_input_bytes > 2);
    assert!(diagnostics.volatile_input_bytes > 2);
}

#[test]
/// Verifies user prompts and action results keep their exact OpenAI wire
/// representation when current-turn context is promoted into transcript history.
///
/// OpenAI can reuse the conversational prefix only when promotion changes the
/// local cache classification without changing provider-visible role, wrapper,
/// content, or ordering. The late action-surface message remains an intentional
/// volatile suffix, so the next request should first diverge after the promoted
/// user and action-result entries rather than at either promoted entry.
fn openai_promoted_conversation_entries_keep_complete_input_bytes() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let first = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "inspect cache continuity".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "action result".to_string(),
                content: "action_id=action-1\noutput: continuity evidence".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();
    let second = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::TranscriptUser,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "transcript user".to_string(),
                content: "inspect cache continuity".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "transcript tool".to_string(),
                content: "action_id=action-1\noutput: continuity evidence".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "transcript assistant".to_string(),
                content: "the prior evidence is complete".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "continue".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let first_body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&first).unwrap()).unwrap();
    let second_body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&second).unwrap()).unwrap();
    let first_input = first_body["input"].as_array().unwrap();
    let second_input = second_body["input"].as_array().unwrap();

    assert_eq!(first_input[0], second_input[0]);
    assert_eq!(first_input[1], second_input[1]);

    let first_diagnostics = openai_prompt_cache_diagnostics_for_request(&first).unwrap();
    let second_diagnostics = openai_prompt_cache_diagnostics_for_request(&second).unwrap();
    let continuity = mez_agent::compare_openai_request_continuity(
        &first_diagnostics.continuity_snapshot,
        &second_diagnostics.continuity_snapshot,
    );
    assert_eq!(continuity.category, "messages");
    assert_eq!(continuity.message_index, Some(2));
    assert_eq!(continuity.common_message_prefix, 2);
    assert!(!continuity.messages_append_only);
}

#[test]
/// Verifies volatile controller state remains out of OpenAI `instructions` and
/// out of the stable input prefix.
///
/// Dynamic capability decisions are authoritative controller context, but
/// rendering them at the front of the prompt would invalidate cache reuse for
/// otherwise identical follow-up requests. They should stay model-visible as
/// late developer input.
fn openai_dynamic_controller_state_is_late_developer_input() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let mut request = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "user".to_string(),
            content: "inspect the repo".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    request.messages.push(super::ModelMessage {
        role: ModelMessageRole::Developer,
        source: ContextSourceKind::DeveloperInstruction,
        placement: mez_agent::ContextPlacement::StablePrefix,
        content: "[capability shell]\ncapability=shell\nallowed_actions=say,shell_command"
            .to_string(),
    });

    let body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&request).unwrap()).unwrap();
    let instructions = body["instructions"].as_str().unwrap();
    assert!(!instructions.contains("[capability shell]"));
    let input = body["input"].as_array().unwrap();
    assert!(input.iter().any(|message| {
        message["role"] == "developer"
            && message["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("[capability shell]"))
    }));

    let diagnostics = openai_prompt_cache_diagnostics_for_request(&request).unwrap();
    assert!(diagnostics.volatile_input_bytes > 2);
    assert_eq!(diagnostics.stable_input_bytes, 2);
}

#[test]
/// Verifies historical tool transcript entries replay as ordinary provider
/// input outside the reusable stable prefix.
///
/// Historical tool output should stay available as regular context so later
/// turns can reference exact prior command evidence without routing through a
/// generated summary layer.
fn openai_historical_tool_results_replay_outside_stable_prefix() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let first = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::TranscriptUser,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "transcript user".to_string(),
                content: "previous request".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "transcript assistant".to_string(),
                content: "previous answer".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "historical tool result".to_string(),
                content: "action_id=action-7\ncommand: rg cache\noutput: stable evidence"
                    .to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "first follow-up".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();
    let second = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::TranscriptUser,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "transcript user".to_string(),
                content: "previous request".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "transcript assistant".to_string(),
                content: "previous answer".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "historical tool result".to_string(),
                content: "action_id=action-7\ncommand: rg cache\noutput: stable evidence"
                    .to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "second follow-up".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();
    let first_body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&first).unwrap()).unwrap();
    let first_input = first_body["input"].as_array().unwrap();
    let historical_tool_text = first_input
        .iter()
        .find_map(|message| {
            let text = message["content"][0]["text"].as_str()?;
            text.contains("[executed result transcript entry]")
                .then_some(text)
        })
        .expect("historical tool result should replay as ordinary input");
    assert!(historical_tool_text.contains("stable evidence"));
    assert!(historical_tool_text.contains("command: rg cache"));
    assert!(first_input.iter().any(|message| {
        message["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("[executed result transcript entry]"))
    }));
    let first_prefix = openai_stable_projection_material_for_request(&first).unwrap();
    let second_prefix = openai_stable_projection_material_for_request(&second).unwrap();
    assert!(first_prefix.contains("[executed result transcript entry]"));
    assert!(first_prefix.contains("stable evidence"));
    assert_eq!(first_prefix, second_prefix);
    let first_diagnostics = openai_prompt_cache_diagnostics_for_request(&first).unwrap();
    let second_diagnostics = openai_prompt_cache_diagnostics_for_request(&second).unwrap();
    assert!(first_diagnostics.stable_input_bytes > 2);
    assert_eq!(
        first_diagnostics.stable_input_sha256,
        second_diagnostics.stable_input_sha256
    );
    assert_ne!(
        first_diagnostics.volatile_input_sha256,
        second_diagnostics.volatile_input_sha256
    );
}

#[test]
/// Verifies a long OpenAI session keeps already-observed action results raw
/// instead of rewriting them into committed summaries during ordinary request
/// assembly.
///
/// Ordinary continuation should preserve already-observed evidence byte for
/// byte. Compaction remains the only path that may rewrite old history.
fn openai_long_session_keeps_observed_action_results_raw_without_committed_evidence() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let mut blocks = Vec::new();
    for index in 0..12 {
        let stable_seed = format!("provider-doc-{index}-title");
        blocks.push(ContextBlock {
            source: ContextSourceKind::TranscriptUser,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: format!("historical user {index}"),
            content: format!("Read provider document {index}."),
        });
        blocks.push(ContextBlock {
            source: ContextSourceKind::TranscriptAssistant,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: format!("historical assistant plan {index}"),
            content: format!("I will fetch provider document {index}."),
        });
        blocks.push(ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: format!("fetch result {index}"),
            content: format!(
                "[action_result fetch-{index} fetch_url succeeded]\n\
                 content:\n\
                 summary_seed={stable_seed}; provider document {index} confirms tool-calling support. {}\n\
                 RAW_DETAIL_SHOULD_NOT_BE_REPLAYED_{index}",
                "stable filler ".repeat(30)
            ),
        });
        blocks.push(ContextBlock {
            source: ContextSourceKind::TranscriptAssistant,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: format!("historical assistant observed {index}"),
            content: format!("I observed fetch-{index} and can use {stable_seed}."),
        });
    }
    blocks.push(ContextBlock {
        source: ContextSourceKind::UserInstruction,
        placement: mez_agent::ContextPlacement::EphemeralTail,
        label: "user".to_string(),
        content: "Compare the provider evidence and continue from the current fetch.".to_string(),
    });
    blocks.push(ContextBlock {
        source: ContextSourceKind::ActionResult,
        placement: mez_agent::ContextPlacement::EphemeralTail,
        label: "current fetch result".to_string(),
        content: "[action_result fetch-current fetch_url succeeded]\ncontent:\nCURRENT_RAW_RESULT_MUST_REMAIN_VOLATILE".to_string(),
    });

    let request =
        assemble_model_request(&profile, &turn(), &AgentContext::new(blocks).unwrap()).unwrap();

    assert!(
        !request
            .messages
            .iter()
            .any(|message| message.source == ContextSourceKind::CommittedEvidence)
    );
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::ActionResult && message.content.contains("fetch-0")
    }));
    assert_eq!(
        request
            .messages
            .iter()
            .filter(|message| message.source == ContextSourceKind::ActionResult)
            .count(),
        13
    );
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::ActionResult
            && message
                .content
                .contains("CURRENT_RAW_RESULT_MUST_REMAIN_VOLATILE")
    }));

    let prefix = openai_stable_projection_material_for_request(&request).unwrap();
    assert!(!prefix.contains("[committed_evidence]"));
    assert!(!prefix.contains("RAW_DETAIL_SHOULD_NOT_BE_REPLAYED_0"));

    let body_text = openai_responses_request_body(&request).unwrap();
    assert!(body_text.contains("CURRENT_RAW_RESULT_MUST_REMAIN_VOLATILE"));
    assert!(body_text.contains("RAW_DETAIL_SHOULD_NOT_BE_REPLAYED_0"));
    let body: serde_json::Value = serde_json::from_str(&body_text).unwrap();
    let input = body["input"].as_array().unwrap();
    let user_index = input
        .iter()
        .position(|message| {
            message["content"][0]["text"].as_str().is_some_and(|text| {
                text.contains("Compare the provider evidence and continue from the current fetch.")
            })
        })
        .expect("current user instruction should be rendered into input");
    let current_result_index = input
        .iter()
        .position(|message| {
            message["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("CURRENT_RAW_RESULT_MUST_REMAIN_VOLATILE"))
        })
        .expect("current action result should be rendered into input");
    assert!(current_result_index > user_index);

    let diagnostics = openai_prompt_cache_diagnostics_for_request(&request).unwrap();
    assert!(diagnostics.stable_input_bytes > 2);
    assert!(diagnostics.volatile_input_bytes > 2);
}

#[test]
/// Verifies long OpenAI sessions preserve append-only stable-prefix continuity
/// until an explicit compaction changes the sequence.
///
/// Prompt caching can only reuse the prior request when the next request keeps
/// the already-rendered stable context byte-identical and appends newly durable
/// transcript material after it. This regression simulates many user/model
/// turns, rebuilds each provider request from the growing transcript, and
/// asserts that every stable input item from the previous request remains the
/// byte-for-byte leading sequence of the next request.
///
/// The current user instruction is intentionally not part of the reusable
/// prefix for its own turn. It should become durable transcript context only
/// on the following request, alongside the assistant output it produced.
fn openai_long_session_stable_prefix_is_append_only_until_compaction() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let mut transcript = Vec::new();
    let mut previous_instructions: Option<String> = None;
    let mut previous_stable_input: Option<Vec<serde_json::Value>> = None;
    let mut previous_diagnostics: Option<mez_agent::OpenAiPromptCacheDiagnostics> = None;
    let mut initial_stable_input_len = None;

    for turn_index in 0..32 {
        let mut turn = turn();
        turn.turn_id = format!("turn-{turn_index}");
        let current_user = format!(
            "current-user-turn-{turn_index}: investigate cache continuity {}",
            "with stable transcript replay ".repeat(8)
        );
        let mut blocks = vec![
            ContextBlock {
                source: ContextSourceKind::Configuration,
                placement: mez_agent::ContextPlacement::StablePrefix,
                label: "session identity".to_string(),
                content: "session_id=session-cache-continuity session_name=cache-test".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                placement: mez_agent::ContextPlacement::StablePrefix,
                label: "project guidance".to_string(),
                content: "keep provider request prefixes byte stable".to_string(),
            },
        ];
        blocks.extend(transcript.clone());
        blocks.push(ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "user".to_string(),
            content: current_user.clone(),
        });

        let request =
            assemble_model_request(&profile, &turn, &AgentContext::new(blocks).unwrap()).unwrap();
        let (instructions, stable_input) = openai_test_stable_prefix_parts(&request);
        let diagnostics = openai_prompt_cache_diagnostics_for_request(&request).unwrap();
        let initial_len = *initial_stable_input_len.get_or_insert(stable_input.len());
        assert_eq!(
            stable_input.len(),
            initial_len + turn_index * 2,
            "turn {turn_index} should contain the stable seed plus one durable user/assistant pair per completed turn"
        );

        if let Some(previous_instructions) = &previous_instructions {
            assert_eq!(
                previous_instructions, &instructions,
                "turn {turn_index} changed stable instructions"
            );
        }
        if let Some(previous_stable_input) = &previous_stable_input {
            assert_eq!(
                stable_input.len(),
                previous_stable_input.len() + 2,
                "turn {turn_index} should append one durable user/assistant pair"
            );
            for (message_index, previous_message) in previous_stable_input.iter().enumerate() {
                let previous_bytes = serde_json::to_vec(previous_message).unwrap();
                let current_bytes = serde_json::to_vec(&stable_input[message_index]).unwrap();
                assert_eq!(
                    previous_bytes, current_bytes,
                    "turn {turn_index} changed stable input message {message_index}"
                );
            }
        }
        if let Some(previous_diagnostics) = &previous_diagnostics {
            assert_eq!(
                previous_diagnostics.prompt_cache_key, diagnostics.prompt_cache_key,
                "turn {turn_index} changed prompt cache routing key"
            );
            assert_eq!(
                previous_diagnostics.instructions_sha256, diagnostics.instructions_sha256,
                "turn {turn_index} changed cached instructions"
            );
            assert_eq!(
                previous_diagnostics.tools_sha256, diagnostics.tools_sha256,
                "turn {turn_index} changed cached tool schema"
            );
            assert_eq!(
                previous_diagnostics.tool_choice_sha256, diagnostics.tool_choice_sha256,
                "turn {turn_index} changed tool choice"
            );
            assert!(
                diagnostics.stable_input_bytes >= previous_diagnostics.stable_input_bytes,
                "turn {turn_index} shrank stable input without compaction"
            );
        }

        let stable_material = openai_stable_projection_material_for_request(&request).unwrap();
        assert!(
            !stable_material.contains(&current_user),
            "turn {turn_index} leaked current user input into its own stable prefix"
        );
        if turn_index > 0 {
            assert!(
                stable_material.contains(&format!("current-user-turn-{}", turn_index - 1)),
                "turn {turn_index} did not append the previous user input"
            );
            assert!(
                stable_material.contains(&format!("assistant-output-turn-{}", turn_index - 1)),
                "turn {turn_index} did not append the previous assistant output"
            );
        }

        previous_instructions = Some(instructions);
        previous_stable_input = Some(stable_input);
        previous_diagnostics = Some(diagnostics);
        transcript.push(ContextBlock {
            source: ContextSourceKind::TranscriptUser,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "transcript user".to_string(),
            content: current_user,
        });
        transcript.push(ContextBlock {
            source: ContextSourceKind::TranscriptAssistant,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "transcript assistant".to_string(),
            content: format!(
                "assistant-output-turn-{turn_index}: cache continuity finding {}",
                "previous bytes remain immutable ".repeat(16)
            ),
        });
    }
}

#[test]
/// Verifies OpenAI prompt-cache diagnostics expose request fingerprints without
/// adding any diagnostic text to model-visible context.
///
/// Trace and status surfaces can use these hashes to explain cache misses while
/// preserving the exact provider prompt shape sent for inference.
fn openai_prompt_cache_diagnostics_fingerprint_provider_prefix_parts() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let request = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                placement: mez_agent::ContextPlacement::StablePrefix,
                label: "active repository instructions".to_string(),
                content: "run just test".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "fix cache hits".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&request).unwrap()).unwrap();
    let diagnostics = openai_prompt_cache_diagnostics_for_request(&request).unwrap();

    assert!(
        body["instructions"]
            .as_str()
            .unwrap()
            .contains("run just test")
    );
    assert!(!body["input"].as_array().unwrap().iter().any(|message| {
        message["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("run just test"))
    }));
    assert!(diagnostics.prompt_cache_key.starts_with("mez-"));
    assert_eq!(diagnostics.prompt_cache_key.len(), "mez-".len() + 32);
    assert!(diagnostics.instructions_bytes > 1024);
    assert_eq!(diagnostics.instructions_sha256.len(), 64);
    assert!(diagnostics.response_format_bytes > 0);
    assert_eq!(diagnostics.response_format_sha256.len(), 64);
    assert!(diagnostics.tools_bytes > 2);
    assert_eq!(diagnostics.tools_sha256.len(), 64);
    assert_eq!(diagnostics.stable_input_bytes, 2);
    assert_eq!(diagnostics.stable_input_sha256.len(), 64);
    assert!(diagnostics.volatile_input_bytes > 2);
    assert_eq!(diagnostics.volatile_input_sha256.len(), 64);
    assert!(diagnostics.stable_projection_bytes > diagnostics.instructions_bytes);
    assert_eq!(diagnostics.stable_projection_sha256.len(), 64);
    assert!(diagnostics.provider_request_shape_bytes > diagnostics.tools_bytes);
    assert_eq!(diagnostics.provider_request_shape_sha256.len(), 64);
}

#[test]
/// Verifies OpenAI prompt-cache diagnostics ignore retention profile options.
///
/// Diagnostics fingerprint the provider-visible request shape used for cache
/// analysis. Because OpenAI does not accept `prompt_cache_retention`, changing a
/// stale local option must not perturb the canonical request-shape digest.
fn openai_prompt_cache_diagnostics_ignore_prompt_cache_retention_option() {
    let implicit = openai_prompt_cache_retention_test_request("gpt-5.4");
    let mut explicit = openai_prompt_cache_retention_test_request("gpt-5.4");
    explicit.prompt_cache_retention = Some("24h".to_string());

    let implicit_body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&implicit).unwrap()).unwrap();
    let explicit_body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&explicit).unwrap()).unwrap();
    assert!(implicit_body.get("prompt_cache_retention").is_none());
    assert_eq!(implicit_body, explicit_body);

    let implicit_diagnostics = openai_prompt_cache_diagnostics_for_request(&implicit).unwrap();
    let explicit_diagnostics = openai_prompt_cache_diagnostics_for_request(&explicit).unwrap();
    assert_eq!(
        implicit_diagnostics.provider_request_shape_bytes,
        explicit_diagnostics.provider_request_shape_bytes
    );
    assert_eq!(
        implicit_diagnostics.provider_request_shape_sha256,
        explicit_diagnostics.provider_request_shape_sha256
    );
}

#[test]
/// Verifies OpenAI prompt-cache routing keys include lineage and provider identity.
///
/// The local routing namespace should follow explicit lineage ids and survive
/// resume-like session-id changes when provider and lineage stay the same.
/// Same-provider OpenAI model switches should reuse one routing key so
/// auto-sizing does not fragment provider prompt-cache affinity, while different
/// provider compatibility targets must not share one routing key.
fn openai_prompt_cache_key_uses_lineage_provider_and_model_namespace() {
    let context_for_session = |session_id: &str, lineage_id: Option<&str>| {
        let mut blocks = vec![
            ContextBlock {
                source: ContextSourceKind::Configuration,
                placement: mez_agent::ContextPlacement::StablePrefix,
                label: "session identity".to_string(),
                content: format!("session_id={session_id} session_name=default"),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "inspect the repo".to_string(),
            },
        ];
        if let Some(lineage_id) = lineage_id {
            blocks.insert(
                1,
                ContextBlock {
                    source: ContextSourceKind::Configuration,
                    placement: mez_agent::ContextPlacement::StablePrefix,
                    label: "prompt cache lineage".to_string(),
                    content: lineage_id.to_string(),
                },
            );
        }
        AgentContext::new(blocks).unwrap()
    };
    let profile = |provider: &str, model: &str| ModelProfile {
        provider: provider.to_string(),
        model: model.to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let inherited_lineage_openai = assemble_model_request(
        &profile("openai", "gpt-a"),
        &turn(),
        &context_for_session("session-a", Some("lineage-parent")),
    )
    .unwrap();
    let inherited_lineage_same_provider_other_model = assemble_model_request(
        &profile("openai", "gpt-b"),
        &turn(),
        &context_for_session("session-a", Some("lineage-parent")),
    )
    .unwrap();
    let inherited_lineage_other_provider = assemble_model_request(
        &profile("deepseek", "deepseek-b"),
        &turn(),
        &context_for_session("session-a", Some("lineage-parent")),
    )
    .unwrap();
    let resumed_session_same_lineage = assemble_model_request(
        &profile("openai", "gpt-a"),
        &turn(),
        &context_for_session("session-b", Some("lineage-parent")),
    )
    .unwrap();
    let fresh_lineage = assemble_model_request(
        &profile("openai", "gpt-a"),
        &turn(),
        &context_for_session("session-b", Some("lineage-fresh")),
    )
    .unwrap();
    let lineage_fallback_session_a = assemble_model_request(
        &profile("openai", "gpt-a"),
        &turn(),
        &context_for_session("session-a", None),
    )
    .unwrap();
    let lineage_fallback_session_b = assemble_model_request(
        &profile("openai", "gpt-a"),
        &turn(),
        &context_for_session("session-b", None),
    )
    .unwrap();

    let inherited_lineage_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&inherited_lineage_openai).unwrap())
            .unwrap();
    let inherited_lineage_same_provider_other_model_value: serde_json::Value =
        serde_json::from_str(
            &openai_responses_request_body(&inherited_lineage_same_provider_other_model).unwrap(),
        )
        .unwrap();
    let inherited_lineage_other_provider_value: serde_json::Value = serde_json::from_str(
        &openai_responses_request_body(&inherited_lineage_other_provider).unwrap(),
    )
    .unwrap();
    let resumed_session_value: serde_json::Value = serde_json::from_str(
        &openai_responses_request_body(&resumed_session_same_lineage).unwrap(),
    )
    .unwrap();
    let fresh_lineage_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&fresh_lineage).unwrap()).unwrap();
    let fallback_a_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&lineage_fallback_session_a).unwrap())
            .unwrap();
    let fallback_b_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&lineage_fallback_session_b).unwrap())
            .unwrap();

    assert_eq!(
        inherited_lineage_value["prompt_cache_key"],
        inherited_lineage_same_provider_other_model_value["prompt_cache_key"]
    );
    assert_ne!(
        inherited_lineage_value["prompt_cache_key"],
        inherited_lineage_other_provider_value["prompt_cache_key"]
    );
    assert_eq!(
        inherited_lineage_value["prompt_cache_key"],
        resumed_session_value["prompt_cache_key"]
    );
    assert_ne!(
        inherited_lineage_value["prompt_cache_key"],
        fresh_lineage_value["prompt_cache_key"]
    );
    assert_eq!(
        fallback_a_value["prompt_cache_key"],
        fallback_b_value["prompt_cache_key"]
    );
}

#[test]
/// Verifies stable-prefix material changes when repo-scoped guidance changes,
/// while the OpenAI prompt-cache key remains a coarse routing namespace.
///
/// OpenAI already hashes the exact prompt prefix for correctness. Mezzanine's
/// explicit key should keep requests with related stable startup context routed
/// together rather than fragmenting on every prompt-prefix text change.
fn openai_prompt_cache_key_uses_stable_namespace_not_rendered_prefix_hash() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let stable_a = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                placement: mez_agent::ContextPlacement::StablePrefix,
                label: "project guidance".to_string(),
                content: "use style a".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "first prompt".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();
    let stable_a_different_user = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                placement: mez_agent::ContextPlacement::StablePrefix,
                label: "project guidance".to_string(),
                content: "use style a".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "second prompt".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();
    let stable_b = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                placement: mez_agent::ContextPlacement::StablePrefix,
                label: "project guidance".to_string(),
                content: "use style b".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "first prompt".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let stable_a_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&stable_a).unwrap()).unwrap();
    let stable_a_user_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&stable_a_different_user).unwrap())
            .unwrap();
    let stable_b_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&stable_b).unwrap()).unwrap();
    let stable_a_diagnostics = openai_prompt_cache_diagnostics_for_request(&stable_a).unwrap();
    let stable_b_diagnostics = openai_prompt_cache_diagnostics_for_request(&stable_b).unwrap();

    assert_eq!(
        openai_stable_projection_material_for_request(&stable_a).unwrap(),
        openai_stable_projection_material_for_request(&stable_a_different_user).unwrap()
    );
    assert_ne!(
        openai_stable_projection_material_for_request(&stable_a).unwrap(),
        openai_stable_projection_material_for_request(&stable_b).unwrap()
    );
    assert_eq!(
        stable_a_value["prompt_cache_key"],
        stable_a_user_value["prompt_cache_key"]
    );
    assert_eq!(
        stable_a_value["prompt_cache_key"],
        stable_b_value["prompt_cache_key"]
    );
    assert_ne!(
        stable_a_diagnostics.stable_projection_sha256,
        stable_b_diagnostics.stable_projection_sha256
    );
}

#[test]
/// Verifies OpenAI prompt-cache routing keys do not use live session fallback.
///
/// When no explicit lineage id is present, the key should use the stable unknown
/// lineage namespace plus provider identity instead of volatile session ids.
fn openai_prompt_cache_key_uses_unknown_lineage_without_session_identity() {
    let context_for_session = |session_id: &str| {
        AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::Configuration,
                placement: mez_agent::ContextPlacement::StablePrefix,
                label: "session identity".to_string(),
                content: format!("session_id={session_id} session_name=default"),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "inspect the repo".to_string(),
            },
        ])
        .unwrap()
    };
    let profile = |provider: &str, model: &str| ModelProfile {
        provider: provider.to_string(),
        model: model.to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let session_a_openai = assemble_model_request(
        &profile("openai", "gpt-a"),
        &turn(),
        &context_for_session("session-a"),
    )
    .unwrap();
    let session_a_other_model = assemble_model_request(
        &profile("deepseek", "deepseek-b"),
        &turn(),
        &context_for_session("session-a"),
    )
    .unwrap();
    let session_b_openai = assemble_model_request(
        &profile("openai", "gpt-a"),
        &turn(),
        &context_for_session("session-b"),
    )
    .unwrap();

    let session_a_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&session_a_openai).unwrap()).unwrap();
    let session_a_other_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&session_a_other_model).unwrap())
            .unwrap();
    let session_b_value: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&session_b_openai).unwrap()).unwrap();

    assert_ne!(
        session_a_value["prompt_cache_key"],
        session_a_other_value["prompt_cache_key"]
    );
    assert_eq!(
        session_a_value["prompt_cache_key"],
        session_b_value["prompt_cache_key"]
    );
}

#[test]
/// Verifies active-turn read/search action results replay directly into the
/// provider request instead of being replaced with a synthetic read-ledger
/// block.
fn openai_replays_current_turn_read_results_without_synthetic_ledger() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let request = assemble_model_request(
        &profile,
        &turn(),
        &AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "Patch the overlay style helper.".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "action result read-1".to_string(),
                content: "[action_result read-1 shell_command succeeded]\ncommand: sed -n '300,420p' src/runtime/render/overlay.rs\noutput:\nowner body".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "action result read-2".to_string(),
                content: "[action_result read-2 shell_command succeeded]\ncommand: sed -n '1148,1238p' src/runtime/render/overlay.rs\noutput:\nhelper body".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "action result read-3".to_string(),
                content: "[action_result read-3 shell_command succeeded]\ncommand: rg -n \"overlay style\" \"docs/reference/issue backlog.md\"\nread_observation_json: {\"kind\":\"search\",\"target\":\"docs/reference/issue backlog.md\",\"ranges\":[],\"query\":\"overlay style\"}\noutput:\n120: overlay style".to_string(),
            },
        ])
        .unwrap(),
    )
    .unwrap();

    let body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&request).unwrap()).unwrap();
    let input = body["input"].as_array().unwrap();
    let raw_results = input
        .iter()
        .filter_map(|message| message["content"][0]["text"].as_str())
        .filter(|text| {
            text.contains("[executed result transcript entry]")
                && text.contains("[action_result read-")
        })
        .collect::<Vec<_>>();

    assert_eq!(raw_results.len(), 3, "{raw_results:#?}");
    assert!(raw_results.iter().any(|text| {
        text.contains("sed -n '300,420p' src/runtime/render/overlay.rs")
            && text.contains("owner body")
    }));
    assert!(raw_results.iter().any(|text| {
        text.contains("sed -n '1148,1238p' src/runtime/render/overlay.rs")
            && text.contains("helper body")
    }));
    assert!(raw_results.iter().any(|text| {
        text.contains("rg -n \"overlay style\" \"docs/reference/issue backlog.md\"")
            && text.contains("120: overlay style")
    }));
    let synthetic_summary = input
        .iter()
        .filter_map(|message| message["content"][0]["text"].as_str())
        .any(|text| text.contains("Recent successful read/search coverage for this active turn."));
    assert!(!synthetic_summary);
}

#[test]
/// Verifies injected MCP integration context stays out of OpenAI instructions
/// and the stable input prefix.
///
/// Explicit `@server` MCP metadata is turn-volatile prompt context. Keeping it
/// in deterministic late developer input prevents one injected server catalog
/// from invalidating invariant instructions or influencing later turns that did
/// not invoke that server.
fn openai_stable_prefix_excludes_injected_mcp_integration_context() {
    let profile = ModelProfile {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };
    let context = AgentContext::new(vec![
        ContextBlock {
            source: ContextSourceKind::ProjectGuidance,
            placement: mez_agent::ContextPlacement::StablePrefix,
            label: "project guidance".to_string(),
            content: "use stable project style".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::RuntimeHint,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "mcp integrations".to_string(),
            content: "available_servers=1 available_tools=1 unavailable_servers=0".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::TranscriptAssistant,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "assistant".to_string(),
            content: "durable assistant context after mcp".to_string(),
        },
        ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "user".to_string(),
            content: "inspect cache reuse".to_string(),
        },
    ])
    .unwrap();

    let ordinary_context = AgentContext::new(
        context
            .blocks
            .iter()
            .filter(|block| block.label != "mcp integrations")
            .cloned()
            .collect(),
    )
    .unwrap();
    let ordinary_request = assemble_model_request(&profile, &turn(), &ordinary_context).unwrap();
    let request = assemble_model_request(&profile, &turn(), &context).unwrap();
    let post_mcp_request = assemble_model_request(&profile, &turn(), &ordinary_context).unwrap();
    let (ordinary_instructions, ordinary_stable_input) =
        openai_test_stable_prefix_parts(&ordinary_request);
    let (instructions, stable_input) = openai_test_stable_prefix_parts(&request);
    let (post_mcp_instructions, post_mcp_stable_input) =
        openai_test_stable_prefix_parts(&post_mcp_request);
    let diagnostics = openai_prompt_cache_diagnostics_for_request(&request).unwrap();
    let stable_input_text = serde_json::to_string(&stable_input).unwrap();
    let body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&request).unwrap()).unwrap();
    let ordinary_body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&ordinary_request).unwrap()).unwrap();
    let post_mcp_body: serde_json::Value =
        serde_json::from_str(&openai_responses_request_body(&post_mcp_request).unwrap()).unwrap();

    assert_eq!(ordinary_instructions, instructions);
    assert_eq!(instructions, post_mcp_instructions);
    assert_eq!(ordinary_stable_input, stable_input);
    assert_eq!(stable_input, post_mcp_stable_input);
    assert!(
        !stable_input_text.contains("[mcp integrations]"),
        "{stable_input_text}"
    );
    assert!(stable_input_text.contains("durable assistant context after mcp"));
    assert!(!stable_input.is_empty());
    assert!(diagnostics.stable_input_bytes > 2);
    assert!(diagnostics.volatile_input_bytes > 2);
    assert!(body["input"].as_array().unwrap().iter().any(|message| {
        message["role"] == "developer"
            && message["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("[mcp integrations]"))
    }));
    assert!(
        !ordinary_body["input"]
            .to_string()
            .contains("[mcp integrations]")
    );
    assert_eq!(ordinary_body["input"], post_mcp_body["input"]);
}
