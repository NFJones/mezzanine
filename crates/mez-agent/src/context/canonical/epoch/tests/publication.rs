//! Prepared-head adoption across a real live-owner worker boundary. These
//! controls preserve source identities and exercise subsequent publication;
//! no checkpoint can mint the private evidence for an in-flight arrival.

use super::*;

/// Source admitted while a prepared head is being persisted keeps its actual
/// birth revision. Adoption, duplicate ACK and the next publisher must all
/// accept that genuine carry without allowing a cold copy to forge its proof.
#[test]
fn prepared_publication_carries_arrivals_through_ack_and_next_publication() {
    let mut live = owner();
    live.append_fact("covered", "old source").unwrap();
    let prepared = live
        .prepare_compaction_publication(
            plan(&live),
            ReplayHeadPublication::Summary("exact prepared head".into()),
        )
        .unwrap();
    live.append_fact("arrival", "source received during storage write")
        .unwrap();
    let arrival = live.members()[1].clone();
    let event = live.context().chronology()[1].clone();
    live.adopt_compaction_publication(&prepared).unwrap();
    assert_eq!(live.head(), prepared.candidate().head());
    assert_eq!(
        live.context().chronology()[0],
        prepared.candidate().context().chronology()[0]
    );
    assert_eq!(live.members(), &[arrival]);
    assert_eq!(live.members()[0].born_in().get(), 0);
    assert_eq!(live.context().chronology()[1], event);
    live.validate_source_extension(prepared.candidate())
        .unwrap();
    let unchanged = live.clone();
    live.adopt_compaction_publication(&prepared).unwrap();
    assert_eq!(live, unchanged);
    let cold = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        live.checkpoint().unwrap(),
        owner().context().clone(),
    )
    .unwrap();
    assert!(
        cold.validate_source_extension(prepared.candidate())
            .is_err()
    );
    let next = live
        .prepare_compaction_publication(
            plan(&live),
            ReplayHeadPublication::Summary("second exact head".into()),
        )
        .unwrap();
    live.adopt_compaction_publication(&next).unwrap();
    assert_eq!(live.revision().get(), 2);
    assert!(live.members().is_empty());
    assert!(
        live.context()
            .chronology()
            .iter()
            .all(|entry| entry != &event)
    );
}

/// Completion/handoff while storage is running must not be rolled back by the
/// prepared snapshot. Still-pending handles and task ownership remain usable;
/// completed handles still verify their exact original protection interval.
#[test]
fn prepared_publication_preserves_live_lifecycle_and_request_inputs() {
    for finish_before_ack in [false, true] {
        let mut live = owner();
        live.append_fact("covered", "old source").unwrap();
        let group = ContextExecutionGroupId::new("retained-group").unwrap();
        let sequence = live
            .append_execution_events([ContextConversationAppend::assistant(
                "assistant",
                "actual output",
                group.clone(),
            )])
            .unwrap()[0];
        let execution = live.capture_execution_owner(&group, sequence).unwrap();
        let task = live.begin_instruction_task().unwrap();
        live.append_instruction(
            &task,
            ContextBlock::user_event("user prompt", "active task"),
        )
        .unwrap();
        let peer = live
            .append_pending_peer("peer", "untrusted reference")
            .unwrap();
        let prepared = live
            .prepare_compaction_publication(
                plan(&live),
                ReplayHeadPublication::Summary("prepared summary".into()),
            )
            .unwrap();
        if finish_before_ack {
            live.finish_execution_group(&execution).unwrap();
            live.complete_instruction_task(&task).unwrap();
            assert!(live.release_peer(peer).unwrap());
        }
        let current = AgentContext::new(vec![ContextBlock::stable_instruction(
            ContextSourceKind::System,
            "policy",
            "current policy during write",
        )])
        .unwrap();
        live.replace_stable_slots(current.stable_slots().to_vec())
            .unwrap();
        let current_slots = live.context().stable_slots().to_vec();
        live.select_provider_projection(
            ReplayProviderProjection::new(
                crate::ProviderApiCompatibility::OpenAiResponses,
                "current-provider",
                "current-model",
            )
            .unwrap(),
        )
        .unwrap();
        let mut body = live
            .into_owned_context()
            .with_metadata(crate::ModelContextMetadata::new(
                Some("current-session"),
                Some("current-cache"),
            ));
        body.update_replay_owner(|live| {
            live.adopt_compaction_publication(&prepared)?;
            Ok(())
        })
        .unwrap();
        let mut live = ReplayEpochContext::from_owned_context(
            ReplayConversationId::new("conversation-a").unwrap(),
            body,
        )
        .unwrap();
        assert_eq!(live.context().stable_slots(), current_slots);
        assert_eq!(
            live.context().metadata().prompt_cache_session_id.as_deref(),
            Some("current-session")
        );
        assert_eq!(
            live.provider_projection().unwrap().provider_id(),
            "current-provider"
        );
        assert_eq!(
            live.context().peer_message_is_pending(peer),
            !finish_before_ack
        );
        assert_eq!(
            live.context().execution_group_is_pending(&group),
            !finish_before_ack
        );
        if !finish_before_ack {
            live.finish_execution_group(&execution).unwrap();
            live.complete_instruction_task(&task).unwrap();
            assert!(live.release_peer(peer).unwrap());
        }
        assert!(live.execution_group_finished(&execution));
        live.validate_source_extension(prepared.candidate())
            .unwrap();
        let unchanged = live.clone();
        live.adopt_compaction_publication(&prepared).unwrap();
        assert_eq!(live, unchanged);
    }
}

/// Equal descriptive bytes cannot adopt another incarnation's head. Renewing
/// selected protection or advancing to another head must also refuse without
/// replacing the live body or erasing pending work.
#[test]
fn prepared_publication_rejects_foreign_protected_and_superseded_source() {
    let mut live = owner();
    let group = ContextExecutionGroupId::new("selected-group").unwrap();
    live.append_execution_events([ContextConversationAppend::assistant(
        "assistant",
        "closed output",
        group.clone(),
    )])
    .unwrap();
    finish_group(&mut live, &group).unwrap();
    let prepared = live
        .prepare_compaction_publication(
            plan(&live),
            ReplayHeadPublication::Summary("prepared head".into()),
        )
        .unwrap();
    let mut cold = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        live.checkpoint().unwrap(),
        owner().context().clone(),
    )
    .unwrap();
    let before = cold.clone();
    assert!(cold.adopt_compaction_publication(&prepared).is_err());
    assert_eq!(cold, before);
    let mut body = live.clone().into_owned_context();
    body.retain_pending_execution_group(&group).unwrap();
    let before = body.clone();
    assert!(
        body.update_replay_owner(|live| {
            live.adopt_compaction_publication(&prepared)?;
            Ok(())
        })
        .is_err()
    );
    assert_eq!(body, before);
    live.adopt_compaction_publication(&prepared).unwrap();
    live.append_fact("new", "new epoch source").unwrap();
    live.publish_compaction(
        plan(&live),
        ReplayHeadPublication::Summary("newer head".into()),
    )
    .unwrap();
    let before = live.clone();
    assert!(live.adopt_compaction_publication(&prepared).is_err());
    assert_eq!(live, before);
}
