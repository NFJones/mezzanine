//! Source continuation controls with real native codecs and original live
//! replay handles. No model, tool process, receipt settlement or effect replay
//! runs here; imported rows remain descriptive pending source.

use super::*;
use crate::{
    ContextConversationAppend, ContextSourceKind, ModelContextMetadata,
    ProviderApiCompatibility as Api, ProviderContinuityOwner, ReplayConversationId,
    ReplayEpochContext,
};

/// Actual pending declaration and the exact later native result to import.
struct NativeFixture {
    body: AgentContext,
    group: ContextExecutionGroupId,
    provider: ProviderContinuityOwner,
    result: ContextBlock,
}

impl NativeFixture {
    fn new(api: Api) -> Self {
        let calls = vec![
            serde_json::json!({"id":"call", "type":"function", "function":{
                "name":crate::MAAP_ACTION_BATCH_TOOL_NAME, "arguments":"{}"
            }}),
        ];
        let (call, result) = match api {
            Api::OpenAiResponses => (
                ProviderTranscriptEvent::OpenAiResponseOutput {
                    items: vec![serde_json::json!({"type":"function_call", "id":"item",
                        "call_id":"call", "name":crate::MAAP_ACTION_BATCH_TOOL_NAME,
                        "arguments":"{}"})],
                },
                ProviderTranscriptEvent::OpenAiFunctionCallOutput {
                    call_id: "call".into(),
                    output: "actual result".into(),
                },
            ),
            Api::OpenAiChatCompletions => (
                ProviderTranscriptEvent::validated_openai_chat_completions_assistant_tool_call(
                    "offline".into(),
                    "actual declaration".into(),
                    calls,
                )
                .unwrap(),
                ProviderTranscriptEvent::OpenAiChatCompletionsToolResult {
                    provider_id: "offline".into(),
                    tool_call_id: "call".into(),
                    content: "actual result".into(),
                },
            ),
            Api::DeepSeekChatCompletions => (
                ProviderTranscriptEvent::DeepSeekAssistantToolCall {
                    content: "actual declaration".into(),
                    reasoning_content: None,
                    tool_calls: calls,
                },
                ProviderTranscriptEvent::DeepSeekToolResult {
                    tool_call_id: "call".into(),
                    content: "actual result".into(),
                },
            ),
            _ => panic!("fixture requires one supported native codec"),
        };
        let context = AgentContext::new(vec![ContextBlock::stable_instruction(
            ContextSourceKind::System,
            "policy",
            "current actual policy",
        )])
        .unwrap()
        .with_metadata(ModelContextMetadata::new(Some("session"), Some("cache")));
        let mut owner =
            ReplayEpochContext::new(ReplayConversationId::new("native-suffix").unwrap(), context)
                .unwrap();
        let group = ContextExecutionGroupId::new("original-group").unwrap();
        let provider = ProviderContinuityOwner::new(api, "offline").unwrap();
        owner
            .append_execution_events([ContextConversationAppend::provider_assistant(
                "actual call",
                call.to_transcript_content(),
                group.clone(),
                provider.clone(),
            )])
            .unwrap();
        Self {
            body: owner.into_owned_context(),
            group,
            provider,
            result: ContextBlock::reference_event(
                ContextSourceKind::TranscriptTool,
                "actual later result",
                result.to_transcript_content(),
            ),
        }
    }

    fn imported(
        &self,
        block: &ContextBlock,
        record: ContextBlockRecord,
        ordinal: u64,
    ) -> ImportedExecutionEvent {
        ImportedExecutionEvent::new(
            block.clone(),
            self.group.clone(),
            ordinal,
            Some(self.provider.clone()),
        )
        .unwrap()
        .with_record(record)
        .unwrap()
    }
}

/// All native families can continue a real pending group without restarting
/// its ordinal, changing frozen source or replaying its call. Only successful
/// admission advances the append fence; the old captured frontier cannot close
/// the new source. Duplicate imported occurrence IDs remain atomic conflicts.
#[test]
fn imported_native_suffix_preserves_source_and_advances_original_append_owner() {
    for api in [
        Api::OpenAiResponses,
        Api::OpenAiChatCompletions,
        Api::DeepSeekChatCompletions,
    ] {
        let mut fixture = NativeFixture::new(api);
        let original = fixture.body.clone();
        let before = original
            .replay_execution_append_owner(&fixture.group)
            .unwrap()
            .clone();
        let record = ContextBlockRecord::allocate(&fixture.result).unwrap();
        let imported = fixture.imported(&fixture.result, record, 2);
        let blocks = vec![
            ContextBlock::reference_event(ContextSourceKind::PeerMessage, "peer", "untrusted data"),
            fixture.result.clone(),
        ];
        assert_eq!(
            fixture
                .body
                .append_imported_history_with_execution(
                    blocks,
                    &[(1, record)],
                    std::slice::from_ref(&imported),
                )
                .unwrap(),
            2
        );
        assert_eq!(fixture.body.chronology()[0], original.chronology()[0]);
        assert_eq!(fixture.body.stable_slots(), original.stable_slots());
        assert_eq!(fixture.body.metadata(), original.metadata());
        let result = &fixture.body.chronology()[2];
        assert_eq!(result.block(), &fixture.result);
        assert_eq!(result.record(), record);
        assert_eq!(result.execution_group_id(), Some(&fixture.group));
        assert_eq!(result.provider_owner(), Some(&fixture.provider));
        assert!(
            fixture
                .body
                .peer_message_is_pending(fixture.body.chronology()[1].sequence())
        );
        let actual = fixture
            .body
            .replay_execution_append_owner(&fixture.group)
            .unwrap()
            .clone();
        assert_eq!(actual.frontier(), result.sequence());
        assert_ne!(before, actual);
        let unchanged = fixture.body.clone();
        assert!(
            fixture
                .body
                .append_imported_history_with_execution(
                    vec![fixture.result.clone()],
                    &[(0, record)],
                    &[imported],
                )
                .is_err()
        );
        assert_eq!(fixture.body, unchanged);
        fixture
            .body
            .update_replay_owner(|owner| {
                assert!(owner.finish_execution_group(&before).is_err());
                owner.finish_execution_group(&actual)
            })
            .unwrap();
        assert!(!fixture.body.execution_group_is_pending(&fixture.group));
    }
}

/// A matching numeric group or equal text cannot repair missing/skipped ordinals,
/// forged call/provider attribution, duplicate results or missing native metadata.
/// Every refusal preserves the body and its original private append fence.
#[test]
fn imported_native_suffix_refuses_unqualified_source_atomically() {
    for alteration in 0..7 {
        let mut fixture = NativeFixture::new(Api::OpenAiChatCompletions);
        let mut result = fixture.result.clone();
        if alteration == 2 || alteration == 3 {
            result.content = ProviderTranscriptEvent::OpenAiChatCompletionsToolResult {
                provider_id: if alteration == 3 {
                    "foreign"
                } else {
                    "offline"
                }
                .into(),
                tool_call_id: if alteration == 2 {
                    "missing-call"
                } else {
                    "call"
                }
                .into(),
                content: "actual result".into(),
            }
            .to_transcript_content();
        }
        let record = ContextBlockRecord::allocate(&result).unwrap();
        let provider = ProviderContinuityOwner::new(
            Api::OpenAiChatCompletions,
            if alteration == 3 {
                "foreign"
            } else {
                "offline"
            },
        )
        .unwrap();
        let ordinal = match alteration {
            0 => 1,
            1 => 3,
            _ => 2,
        };
        let execution = ImportedExecutionEvent::new(
            result.clone(),
            fixture.group.clone(),
            ordinal,
            Some(provider),
        )
        .unwrap()
        .with_record(record)
        .unwrap();
        let mut blocks = vec![result];
        let mut records = vec![(0, record)];
        let mut imported = vec![execution];
        if alteration == 4 {
            imported.clear();
        } else if alteration == 5 {
            blocks.push(blocks[0].clone());
            let duplicate_result = ContextBlockRecord::allocate(&blocks[1]).unwrap();
            records.push((1, duplicate_result));
            imported.push(fixture.imported(&blocks[1], duplicate_result, 3));
        } else if alteration == 6 {
            imported = vec![
                ImportedExecutionEvent::new(
                    fixture.body.chronology()[0].block().clone(),
                    fixture.group.clone(),
                    2,
                    Some(fixture.provider.clone()),
                )
                .unwrap()
                .with_record(fixture.body.chronology()[0].record())
                .unwrap(),
            ];
        }
        let before = fixture.body.clone();
        assert!(
            fixture
                .body
                .append_imported_history_with_execution(blocks, &records, &imported)
                .is_err(),
            "alteration={alteration}"
        );
        assert_eq!(fixture.body, before, "alteration={alteration}");
    }
}

/// A new group starts at one, remains pending with an unanswered call and can
/// later accept its exact result. Import cannot treat a stable slot as an arrival
/// or drop protection from an unrelated original pending execution group.
#[test]
fn imported_new_native_group_stays_pending_until_actual_result_and_retirement() {
    let mut fixture = NativeFixture::new(Api::OpenAiChatCompletions);
    let group = ContextExecutionGroupId::new("new-group").unwrap();
    let call = fixture.body.chronology()[0].block().clone();
    let record = ContextBlockRecord::allocate(&call).unwrap();
    let imported = ImportedExecutionEvent::new(
        call.clone(),
        group.clone(),
        1,
        Some(fixture.provider.clone()),
    )
    .unwrap()
    .with_record(record)
    .unwrap();
    fixture
        .body
        .append_imported_history_with_execution(vec![call], &[(0, record)], &[imported])
        .unwrap();
    assert!(fixture.body.execution_group_is_pending(&fixture.group));
    assert!(fixture.body.execution_group_is_pending(&group));
    let before = fixture.body.clone();
    let pending = fixture
        .body
        .replay_execution_append_owner(&group)
        .unwrap()
        .clone();
    assert!(
        fixture
            .body
            .update_replay_owner(|owner| owner.finish_execution_group(&pending))
            .is_err()
    );
    assert_eq!(fixture.body, before);
    assert!(
        fixture
            .body
            .append_imported_history_with_execution(
                vec![ContextBlock::stable_instruction(
                    ContextSourceKind::System,
                    "saved policy",
                    "must not become an arrival",
                )],
                &[],
                &[]
            )
            .is_err()
    );
    assert_eq!(fixture.body, before);
    let record = ContextBlockRecord::allocate(&fixture.result).unwrap();
    let imported = ImportedExecutionEvent::new(
        fixture.result.clone(),
        group.clone(),
        2,
        Some(fixture.provider.clone()),
    )
    .unwrap()
    .with_record(record)
    .unwrap();
    fixture
        .body
        .append_imported_history_with_execution(
            vec![fixture.result.clone()],
            &[(0, record)],
            &[imported],
        )
        .unwrap();
    let actual = fixture
        .body
        .replay_execution_append_owner(&group)
        .unwrap()
        .clone();
    fixture
        .body
        .update_replay_owner(|owner| owner.finish_execution_group(&actual))
        .unwrap();
    assert!(!fixture.body.execution_group_is_pending(&group));
    assert!(fixture.body.execution_group_is_pending(&fixture.group));
}

/// An explicit native source import stays protected even before a live epoch
/// continuation is installed. Importing descriptive bytes is not proof that the
/// unanswered declaration has completed or may be summarized.
#[test]
fn imported_native_source_protects_plain_context_without_minting_a_live_owner() {
    let fixture = NativeFixture::new(Api::OpenAiChatCompletions);
    let call = fixture.body.chronology()[0].block().clone();
    let record = ContextBlockRecord::allocate(&call).unwrap();
    let imported = fixture.imported(&call, record, 1);
    let mut plain = AgentContext::empty();
    plain
        .append_imported_history_with_execution(vec![call], &[(0, record)], &[imported])
        .unwrap();
    assert!(plain.execution_group_is_pending(&fixture.group));
    assert!(!plain.has_replay_owner());
}
