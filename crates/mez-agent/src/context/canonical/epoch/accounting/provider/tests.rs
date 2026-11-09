//! Compare incremental provider views to the existing whole-context projector.
//! Real typed native declarations/results exercise incomplete, foreign and
//! interleaved groups without launching native work or contacting a provider.

use super::*;
use crate::{
    ProviderTranscriptEvent as Native, context_compaction::projected_context_block_input_tokens,
};

type Api = ProviderApiCompatibility;

/// Completed native pairs on either side of a pending native island use the
/// same sparse selection and incremental accounting as ordinary source. Cold
/// restoration preserves pending protection and exact wire records, not rights.
#[test]
fn protected_selection_native_islands_keep_projection_and_cold_source() {
    for api in [
        Api::OpenAiResponses,
        Api::OpenAiChatCompletions,
        Api::DeepSeekChatCompletions,
    ] {
        let mut source = owner();
        let task = source.begin_instruction_task().unwrap();
        source
            .append_instruction(&task, ContextBlock::user_event("task", "exact native root"))
            .unwrap();
        let first = ContextExecutionGroupId::new("first native closed").unwrap();
        source
            .append_execution_events(pair(api, "provider", &first, "first"))
            .unwrap();
        close(&mut source, &first);
        let pending = ContextExecutionGroupId::new("pending native island").unwrap();
        let mut incomplete = pair(api, "provider", &pending, "pending");
        incomplete.pop();
        source.append_execution_events(incomplete).unwrap();
        let second = ContextExecutionGroupId::new("second native closed").unwrap();
        source
            .append_execution_events(pair(api, "provider", &second, "second"))
            .unwrap();
        close(&mut source, &second);
        source
            .select_provider_projection(projection(api, "provider", "model"))
            .unwrap();
        assert_projection(&source);
        let ReplayCompactionPreparation::Ready(plan) = source
            .prepare_compaction(ReplayTailPolicy::new(1000, 0).unwrap())
            .unwrap()
        else {
            panic!("completed native pairs must be selectable");
        };
        assert_eq!(plan.removed_raw_members().count(), 8);
        let retained = plan.retained_raw_events().cloned().collect::<Vec<_>>();
        let expected = plan.input_estimates().retained_raw_input_tokens();
        source
            .publish_compaction(
                plan,
                ReplayHeadPublication::Summary("closed native reference".into()),
            )
            .unwrap();
        assert_eq!(&source.context().chronology()[1..], retained);
        assert_eq!(
            source
                .provider_input_estimates()
                .unwrap()
                .raw_input_tokens(),
            expected
        );
        assert_projection(&source);
        let mut cold = ReplayEpochContext::from_checkpoint(
            source.conversation_id().clone(),
            source.checkpoint().unwrap(),
            owner().context().clone(),
        )
        .unwrap();
        cold.select_provider_projection(projection(api, "provider", "model"))
            .unwrap();
        assert_projection(&cold);
        assert_eq!(
            cold.provider_input_estimates(),
            source.provider_input_estimates()
        );
        assert_eq!(cold.context().chronology(), source.context().chronology());
        assert!(cold.context().execution_group_is_pending(&pending));
        let before = cold.clone();
        let frontier = cold.context().chronology().last().unwrap().sequence();
        let fence = cold.capture_execution_owner(&pending, frontier).unwrap();
        assert!(
            cold.finish_execution_group(&fence).is_err(),
            "unanswered native call cannot close"
        );
        assert_eq!(cold, before);
    }
}

/// Creates only canonical fixture source with one exact stable policy slot.
fn owner() -> ReplayEpochContext {
    ReplayEpochContext::new(
        ReplayConversationId::new("provider-estimates").unwrap(),
        AgentContext::new_durable(vec![ContextBlock::stable_instruction(
            ContextSourceKind::System,
            "policy",
            "stable exact instructions",
        )])
        .unwrap(),
    )
    .unwrap()
}
/// Captures a checked configured descriptor, without provider or execution work.
fn projection(api: Api, provider: &str, model: &str) -> ReplayProviderProjection {
    ReplayProviderProjection::new(api, provider, model).unwrap()
}

/// Constructs actual native wire records and their independent neutral evidence.
/// The group is supplied structurally, with no pairing inferred from text.
fn pair(
    api: Api,
    provider: &str,
    group: &ContextExecutionGroupId,
    tag: &str,
) -> Vec<ContextConversationAppend> {
    let call_id = format!("call-{tag}");
    let tools = vec![
        serde_json::json!({"id":call_id, "type":"function", "function":{"name":crate::MAAP_ACTION_BATCH_TOOL_NAME,"arguments":"{}"}}),
    ];
    let (call, result) = match api {
        Api::OpenAiResponses => (
            Native::OpenAiResponseOutput {
                items: vec![
                    serde_json::json!({"type":"function_call","id":format!("item-{tag}"),"call_id":call_id,"name":crate::MAAP_ACTION_BATCH_TOOL_NAME,"arguments":"{}"}),
                ],
            },
            Native::OpenAiFunctionCallOutput {
                call_id,
                output: "exact é😀 native result".into(),
            },
        ),
        Api::OpenAiChatCompletions => (
            Native::validated_openai_chat_completions_assistant_tool_call(
                provider.into(),
                "native declaration".into(),
                tools,
            )
            .unwrap(),
            Native::OpenAiChatCompletionsToolResult {
                provider_id: provider.into(),
                tool_call_id: call_id,
                content: "exact é😀 native result".into(),
            },
        ),
        Api::DeepSeekChatCompletions => (
            Native::DeepSeekAssistantToolCall {
                content: "native declaration".into(),
                reasoning_content: Some("retained reasoning".into()),
                tool_calls: tools,
            },
            Native::DeepSeekToolResult {
                tool_call_id: call_id,
                content: "exact é😀 native result".into(),
            },
        ),
        Api::AnthropicMessages => panic!("no opaque Anthropic codec in this fixture"),
    };
    let native_owner = ProviderContinuityOwner::new(api, provider).unwrap();
    vec![
        ContextConversationAppend::assistant(
            "neutral assistant",
            "neutral rationale é😀",
            group.clone(),
        ),
        ContextConversationAppend::provider_assistant(
            "native call",
            call.to_transcript_content(),
            group.clone(),
            native_owner.clone(),
        ),
        ContextConversationAppend::evidence(
            ContextSourceKind::ActionResult,
            "neutral action",
            "{\"dense\":[1,2,3],\"text\":\"é😀\"}",
            group.clone(),
            None,
            true,
        ),
        ContextConversationAppend::evidence(
            ContextSourceKind::TranscriptTool,
            "native result",
            result.to_transcript_content(),
            group.clone(),
            Some(native_owner),
            true,
        ),
    ]
}
/// Retires a fixture group through its exact actually admitted frontier.
fn close(owner: &mut ReplayEpochContext, group: &ContextExecutionGroupId) {
    let last = owner
        .context()
        .chronology()
        .iter()
        .rev()
        .find(|event| event.execution_group_id() == Some(group))
        .unwrap()
        .sequence();
    let fence = owner.capture_execution_owner(group, last).unwrap();
    owner.finish_execution_group(&fence).unwrap();
}
/// Counts index group qualifications on this test thread only.
fn qualifications() -> usize {
    GROUP_QUALIFICATIONS.with(std::cell::Cell::get)
}
/// Reads actual estimate construction count, excluding cache/metadata access.
fn computations() -> usize {
    crate::context::canonical::block_record::ESTIMATE_COMPUTATIONS.with(std::cell::Cell::get)
}

/// The oracle uses the existing whole-history traversal, independent of the
/// occurrence/group index and its incremental deltas. Costs include zero entries
/// so native invisibility cannot disappear from the association comparison.
fn assert_projection(owner: &ReplayEpochContext) -> Vec<usize> {
    let key = owner.provider_projection().unwrap();
    let costs =
        projected_context_block_input_tokens(owner.context(), Some(key.budget_projection()));
    let mut expected = ReplayEpochInputEstimates::default();
    let prefix = owner.context().stable_slots().len();
    for (index, cost) in costs.iter().copied().enumerate() {
        if index < prefix {
            expected.stable += cost;
        } else if index == prefix && owner.head.is_some() {
            expected.summary += cost;
        } else {
            expected.raw += cost;
            if owner.event_is_replaceable(&owner.context().chronology()[index - prefix]) {
                expected.eligible += cost;
            }
        }
        let record = owner.context().block_metadata()[index].record();
        assert_eq!(
            owner.accounting.provider.as_ref().unwrap().entries[&record.occurrence_id()].cost,
            cost
        );
    }
    assert_eq!(owner.provider_input_estimates(), Some(expected));
    expected.total_input_tokens().unwrap();
    owner.validate_membership().unwrap();
    costs
}

/// For each supported native API, adding its final result replaces the earlier
/// neutral charge with native cost, including prior members. Closure changes
/// eligibility only. API/provider/model switches keep every source record and
/// use valid separate projections without rerunning text estimation.
#[test]
fn native_completion_and_provider_api_model_switches_match_request_projection() {
    for api in [
        Api::OpenAiResponses,
        Api::OpenAiChatCompletions,
        Api::DeepSeekChatCompletions,
    ] {
        let mut owner = owner();
        let group = ContextExecutionGroupId::new("native-group").unwrap();
        let mut events = pair(api, "configured-provider", &group, "original");
        let result = events.pop().unwrap();
        owner.append_execution_events(events).unwrap();
        owner
            .select_provider_projection(projection(api, "configured-provider", "model-a"))
            .unwrap();
        let incomplete = assert_projection(&owner);
        assert!(incomplete[1] > 0 && incomplete[3] > 0);
        assert_eq!(incomplete[2], 0);
        owner.append_execution_events([result]).unwrap();
        let complete = assert_projection(&owner);
        assert_eq!(complete[1], 0);
        assert_eq!(complete[3], 0);
        assert!(complete[2] > 0 && complete[4] > 0);
        assert_eq!(
            owner
                .provider_input_estimates()
                .unwrap()
                .eligible_raw_input_tokens(),
            0
        );
        let count = qualifications();
        close(&mut owner, &group);
        assert_eq!(
            qualifications(),
            count,
            "retirement cannot reparse native groups"
        );
        assert_projection(&owner);
        let totals = owner.provider_input_estimates().unwrap();
        assert_eq!(
            totals.raw_input_tokens(),
            totals.eligible_raw_input_tokens()
        );
        let records = owner.context().block_records();
        let source_buffers = owner
            .context()
            .chronology()
            .iter()
            .map(|event| event.block().content.as_ptr())
            .collect::<Vec<_>>();
        let text_count = computations();
        let before_model_qualification = qualifications();
        owner
            .select_provider_projection(projection(api, "configured-provider", "model-b"))
            .unwrap();
        assert_eq!(computations(), text_count);
        assert_eq!(qualifications(), before_model_qualification);
        assert_eq!(owner.provider_input_estimates(), Some(totals));
        assert_eq!(owner.context().block_records(), records);
        owner
            .select_provider_projection(projection(api, "foreign-provider", "model-b"))
            .unwrap();
        let foreign = assert_projection(&owner);
        assert!(foreign[1] > 0 && foreign[3] > 0);
        assert_eq!(foreign[2], 0);
        assert_eq!(foreign[4], 0);
        owner
            .select_provider_projection(projection(
                Api::AnthropicMessages,
                "configured-provider",
                "model-b",
            ))
            .unwrap();
        assert_eq!(assert_projection(&owner), foreign);
        assert_eq!(computations(), text_count);
        assert_eq!(owner.context().block_records(), records);
        assert_eq!(
            owner
                .context()
                .chronology()
                .iter()
                .map(|event| event.block().content.as_ptr())
                .collect::<Vec<_>>(),
            source_buffers,
            "changing an estimate projection must not clone/replace canonical source buffers"
        );
    }
}

/// A late same-group occurrence after unrelated source makes only that group
/// interleaved, withdrawing its native projection. Unrelated groups and source
/// cache records remain intact. Same-key and lifecycle-only updates do no group
/// qualification; an ungrouped arrival does none either.
#[test]
fn interleaved_arrival_refreshes_only_its_own_group() {
    let mut owner = owner();
    let (a, b) = (
        ContextExecutionGroupId::new("a").unwrap(),
        ContextExecutionGroupId::new("b").unwrap(),
    );
    for (group, tag) in [(&a, "a"), (&b, "b")] {
        owner
            .append_execution_events(pair(Api::OpenAiChatCompletions, "provider", group, tag))
            .unwrap();
        close(&mut owner, group);
    }
    let key = projection(Api::OpenAiChatCompletions, "provider", "model");
    owner.select_provider_projection(key.clone()).unwrap();
    assert_projection(&owner);
    let unaffected = owner
        .context()
        .chronology()
        .iter()
        .filter(|event| event.execution_group_id() == Some(&b))
        .map(|event| {
            let id = event.record().occurrence_id();
            (id, owner.accounting.provider.as_ref().unwrap().entries[&id])
        })
        .collect::<Vec<_>>();
    let before = qualifications();
    owner.select_provider_projection(key).unwrap();
    owner
        .append_fact("unrelated", "actual unrelated source")
        .unwrap();
    let task = owner.begin_instruction_task().unwrap();
    owner
        .append_instruction(
            &task,
            ContextBlock::user_event("user", "completed exact task"),
        )
        .unwrap();
    owner.complete_instruction_task(&task).unwrap();
    assert_eq!(qualifications(), before);
    owner
        .append_execution_events([ContextConversationAppend::evidence(
            ContextSourceKind::ActionResult,
            "late action",
            "later actual result",
            a.clone(),
            None,
            true,
        )])
        .unwrap();
    assert_eq!(qualifications(), before + 1);
    assert_projection(&owner);
    let view = owner.accounting.provider.as_ref().unwrap();
    assert!(!view.complete_groups.contains(&a));
    assert!(view.complete_groups.contains(&b));
    for (id, entry) in unaffected {
        assert_eq!(view.entries[&id], entry);
    }
    let before = qualifications();
    close(&mut owner, &a);
    assert_eq!(qualifications(), before);
    assert_projection(&owner);
}

/// A result owned by another provider cannot complete the original declaration
/// for either provider, even when its visible wire call id is identical. Such a
/// mixed group stays neutral and cannot acquire a completion witness.
#[test]
fn mixed_foreign_native_result_keeps_neutral_accounting() {
    let mut owner = owner();
    let group = ContextExecutionGroupId::new("mixed ownership").unwrap();
    let mut events = pair(Api::OpenAiChatCompletions, "original", &group, "same");
    events.pop();
    owner.append_execution_events(events).unwrap();
    let foreign = pair(Api::OpenAiChatCompletions, "foreign", &group, "same")
        .pop()
        .unwrap();
    owner.append_execution_events([foreign]).unwrap();
    for provider in ["original", "foreign"] {
        owner
            .select_provider_projection(projection(Api::OpenAiChatCompletions, provider, "model"))
            .unwrap();
        let costs = assert_projection(&owner);
        assert!(costs[1] > 0 && costs[3] > 0);
        assert_eq!(costs[2], 0);
        assert_eq!(costs[4], 0);
    }
    let frontier = owner.context().chronology().last().unwrap().sequence();
    let fence = owner.capture_execution_owner(&group, frontier).unwrap();
    let before = owner.clone();
    assert!(owner.finish_execution_group(&fence).is_err());
    assert_eq!(owner, before);
}

/// Provider-aware plans partition only the current projection, keep entire
/// native groups, and preserve cached retained members across three summaries
/// plus post-frontier arrivals. Publication never requalifies unrelated tails.
#[test]
fn three_provider_epochs_freeze_projected_costs_and_preserve_native_tail() {
    let mut owner = owner();
    for tag in ["a", "b"] {
        let group = ContextExecutionGroupId::new(tag).unwrap();
        owner
            .append_execution_events(pair(Api::OpenAiResponses, "provider", &group, tag))
            .unwrap();
        close(&mut owner, &group);
    }
    let key = projection(Api::OpenAiResponses, "provider", "model");
    owner.select_provider_projection(key.clone()).unwrap();
    let first_costs = assert_projection(&owner);
    let retained_budget = first_costs[5..].iter().sum::<usize>();
    let mut head_ids = BTreeSet::new();
    for round in 0..3 {
        let costs = assert_projection(&owner);
        let before = computations();
        let ReplayCompactionPreparation::Ready(plan) = owner
            .prepare_compaction(
                ReplayTailPolicy::new(
                    if round == 0 { retained_budget } else { 1000 },
                    if round == 0 { 100 } else { 0 },
                )
                .unwrap(),
            )
            .unwrap()
        else {
            panic!("closed source required");
        };
        assert_eq!(computations(), before);
        assert_eq!(plan.provider_projection(), Some(&key));
        let prefix = owner.context().stable_slots().len();
        let selected = plan.summary_input().len();
        assert_eq!(
            plan.input_estimates().source_input_tokens().unwrap(),
            costs[prefix..prefix + selected].iter().sum::<usize>()
        );
        assert_eq!(
            plan.input_estimates().retained_raw_input_tokens(),
            costs[prefix + selected..].iter().sum::<usize>()
        );
        if round == 0 {
            assert_eq!(plan.retained_tail().len(), 4);
        }
        let retained = owner.members[plan.removed_raw_members().count()..].to_vec();
        owner
            .append_fact("arrival", "post-frontier actual source")
            .unwrap();
        let arrival = owner.members.last().unwrap().clone();
        let group_count = qualifications();
        owner
            .publish_compaction(
                plan,
                ReplayHeadPublication::Summary(format!("summary {round}")),
            )
            .unwrap();
        assert_eq!(qualifications(), group_count);
        assert!(head_ids.insert(owner.head().unwrap().occurrence_id()));
        assert_eq!(&owner.members[..retained.len()], retained);
        assert_eq!(owner.members.last(), Some(&arrival));
        assert_projection(&owner);
    }
    assert_eq!(owner.revision().get(), 3);
}

/// Projection identity is frozen independently of source bytes. A model/provider
/// switch preserves canonical records but invalidates an older plan; invalid
/// descriptors and exhausted private aggregates cannot partially adopt source.
#[test]
fn projection_change_and_exhaustion_reject_atomically() {
    for (provider, model) in [
        ("", "model"),
        ("provider", ""),
        (" provider", "model"),
        ("provider", "bad\nmodel"),
    ] {
        assert!(ReplayProviderProjection::new(Api::OpenAiResponses, provider, model).is_err());
    }
    let mut owner = owner();
    owner.append_fact("raw", "original source").unwrap();
    owner
        .select_provider_projection(projection(Api::OpenAiResponses, "provider", "model-a"))
        .unwrap();
    let ReplayCompactionPreparation::Ready(plan) = owner
        .prepare_compaction(ReplayTailPolicy::new(1000, 0).unwrap())
        .unwrap()
    else {
        panic!("source required");
    };
    let records = owner.context().block_records();
    let before_count = computations();
    owner
        .select_provider_projection(projection(Api::OpenAiResponses, "provider", "model-b"))
        .unwrap();
    assert_eq!(owner.context().block_records(), records);
    assert_eq!(computations(), before_count);
    let before = owner.clone();
    assert!(
        owner
            .publish_compaction(
                plan,
                ReplayHeadPublication::Summary("stale model result".into())
            )
            .is_err()
    );
    assert_eq!(owner, before);
    owner.accounting.provider.as_mut().unwrap().totals.raw = usize::MAX;
    let before = owner.clone();
    assert!(owner.append_fact("raw", "new source").is_err());
    assert_eq!(owner, before);
}

/// Explicit typed reimport preserves occurrence/cache records and exact native
/// group ownership. A fresh epoch owner still requires its own genuine closure;
/// imported metadata cannot restore lifecycle authority. Valid serialized costs,
/// provider switches and unchanged stable refresh never recount source bytes.
#[test]
fn typed_native_reimport_reuses_records_without_restoring_closure_authority() {
    let mut original = owner();
    let group = ContextExecutionGroupId::new("restored native source").unwrap();
    original
        .append_execution_events(pair(
            Api::DeepSeekChatCompletions,
            "provider",
            &group,
            "restore",
        ))
        .unwrap();
    original
        .select_provider_projection(projection(
            Api::DeepSeekChatCompletions,
            "provider",
            "model",
        ))
        .unwrap();
    let records = serde_json::from_str::<Vec<(usize, crate::ContextBlockRecord)>>(
        &serde_json::to_string(&original.context().block_records()).unwrap(),
    )
    .unwrap();
    let imported = original
        .context()
        .chronology()
        .iter()
        .enumerate()
        .map(|(ordinal, event)| {
            crate::ImportedExecutionEvent::new(
                event.block().clone(),
                group.clone(),
                ordinal as u64 + 1,
                event.provider_owner().cloned(),
            )
            .unwrap()
            .with_record(event.record())
            .unwrap()
        })
        .collect::<Vec<_>>();
    let before = computations();
    let mut context = AgentContext::import_durable_blocks_with_records(
        original.context().blocks().to_vec(),
        &records,
        &[],
    )
    .unwrap();
    context
        .restore_imported_execution_events(&imported)
        .unwrap();
    assert_eq!(context.block_records(), records);
    let mut restored = ReplayEpochContext::new(
        ReplayConversationId::new("restored-owner").unwrap(),
        context,
    )
    .unwrap();
    restored
        .select_provider_projection(projection(
            Api::DeepSeekChatCompletions,
            "provider",
            "model",
        ))
        .unwrap();
    assert_eq!(computations(), before);
    assert_eq!(
        restored.provider_input_estimates(),
        original.provider_input_estimates()
    );
    assert_projection(&restored);
    let first_fence = original
        .capture_execution_owner(
            &group,
            original.context().chronology().last().unwrap().sequence(),
        )
        .unwrap();
    assert!(restored.finish_execution_group(&first_fence).is_err());
    close(&mut restored, &group);
    close(&mut original, &group);
    assert_eq!(
        restored.provider_input_estimates(),
        original.provider_input_estimates()
    );
    let slots = restored.context().stable_slots().to_vec();
    let checks = qualifications();
    restored.replace_stable_slots(slots).unwrap();
    assert_eq!(qualifications(), checks);
    assert_eq!(computations(), before);
    assert_projection(&restored);
}

/// A retained pending group can become complete after the frozen frontier. Its
/// earlier native/neutral costs change in the live view, while the frozen plan
/// still describes its original source. Publishing an unrelated selected prefix
/// must retain all four exact group records and their current projected costs.
#[test]
fn retained_group_completion_after_frontier_keeps_current_costs_on_publication() {
    let mut owner = owner();
    owner
        .append_fact("closed prefix", "old replaceable fact")
        .unwrap();
    let group = ContextExecutionGroupId::new("retained pending group").unwrap();
    let mut events = pair(Api::OpenAiResponses, "provider", &group, "retained");
    let result = events.pop().unwrap();
    owner.append_execution_events(events).unwrap();
    owner
        .select_provider_projection(projection(Api::OpenAiResponses, "provider", "model"))
        .unwrap();
    let before = assert_projection(&owner);
    let retained_ids = owner.members()[1..]
        .iter()
        .map(ReplayEpochMember::occurrence_id)
        .collect::<Vec<_>>();
    let ReplayCompactionPreparation::Ready(plan) = owner
        .prepare_compaction(ReplayTailPolicy::new(1000, 0).unwrap())
        .unwrap()
    else {
        panic!("closed prefix required");
    };
    assert_eq!(plan.summary_input().len(), 1);
    assert_eq!(plan.retained_tail().len(), 3);
    let frozen_cost = plan.input_estimates().retained_raw_input_tokens();
    assert_eq!(frozen_cost, before[2..].iter().sum::<usize>());
    owner.append_execution_events([result]).unwrap();
    let completed = assert_projection(&owner);
    let current_raw = completed[2..].iter().sum::<usize>();
    assert_eq!(
        plan.input_estimates().retained_raw_input_tokens(),
        frozen_cost
    );
    let checks = qualifications();
    owner
        .publish_compaction(
            plan,
            ReplayHeadPublication::Summary("old prefix summarized".into()),
        )
        .unwrap();
    assert_eq!(qualifications(), checks);
    assert_eq!(
        owner.provider_input_estimates().unwrap().raw_input_tokens(),
        current_raw
    );
    assert_eq!(
        owner
            .provider_input_estimates()
            .unwrap()
            .eligible_raw_input_tokens(),
        0
    );
    assert_eq!(
        owner.members()[..3]
            .iter()
            .map(ReplayEpochMember::occurrence_id)
            .collect::<Vec<_>>(),
        retained_ids
    );
    assert_eq!(owner.context().chronology().len(), 5);
    assert_projection(&owner);
    close(&mut owner, &group);
    assert_eq!(qualifications(), checks);
    assert_projection(&owner);
    assert_eq!(
        owner
            .provider_input_estimates()
            .unwrap()
            .eligible_raw_input_tokens(),
        current_raw
    );
}

/// Non-prefix omission must update the existing provider visibility index, not
/// rebuild neutral-only costs or retain a complete-native duplicate charge.
#[test]
fn raw_tail_omission_preserves_provider_native_accounting() {
    for api in [
        Api::OpenAiResponses,
        Api::OpenAiChatCompletions,
        Api::DeepSeekChatCompletions,
    ] {
        for pending in [false, true] {
            let mut live = owner();
            live.append_pending_peer("peer", "exact pending peer source")
                .unwrap();
            let group = ContextExecutionGroupId::new("raw-native-group").unwrap();
            live.append_execution_events(pair(api, "provider", &group, "raw"))
                .unwrap();
            let required = live.context().chronology().to_vec();
            if !pending {
                close(&mut live, &group);
            }
            live.append_fact("optional", "discard after the native group")
                .unwrap();
            live.select_provider_projection(projection(api, "provider", "model"))
                .unwrap();
            assert_projection(&live);
            let policy = ReplayTailPolicy::new(10_000, 0)
                .unwrap()
                .with_raw_tail_limits(19, Some(0));
            let plan = match live.prepare_compaction(policy).unwrap() {
                ReplayCompactionPreparation::Ready(plan) => plan,
                other => panic!("expected optional raw work: {other:?}"),
            };
            let expected = if pending {
                required.as_slice()
            } else {
                &required[..1]
            };
            assert_eq!(
                plan.retained_raw_events().cloned().collect::<Vec<_>>(),
                expected
            );
            live.publish_compaction(plan, ReplayHeadPublication::IntentionalOmission)
                .unwrap();
            assert_eq!(live.context().chronology(), expected);
            assert_projection(&live);
            assert_eq!(
                live.provider_input_estimates()
                    .unwrap()
                    .summary_input_tokens(),
                0
            );
            if pending {
                close(&mut live, &group);
                assert_projection(&live);
            }
        }
    }
}
