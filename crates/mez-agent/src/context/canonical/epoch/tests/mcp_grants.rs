//! Retrieval evidence stays exact while accepted rewrites revoke availability.
//! These controls call the production tool selector and actual epoch owner;
//! they execute no MCP action and mint no native execution/transport receipt.

use super::*;

fn tools(context: &AgentContext) -> Vec<crate::McpPromptTool> {
    crate::invoked_mcp_tools_for_context(
        context,
        &crate::McpPromptSummary {
            available_servers: Vec::new(),
            available_tools: Vec::new(),
            unavailable_servers: Vec::new(),
        },
    )
}

/// Copying revokes even revision-zero evidence in the same namespace; a fresh
/// retrieval for the copied scope grants normally and survives later restore.
#[test]
fn mcp_grants_conversation_copy_revokes_inherited_evidence_once() {
    let mut source = owner();
    let original = retrieve(&mut source);
    assert_eq!(tools(source.context()).len(), 1);
    for target in ["conversation-a", "conversation-copy"] {
        let target = ReplayConversationId::new(target).unwrap();
        let checkpoint = source
            .checkpoint()
            .unwrap()
            .for_conversation_copy(target.clone())
            .unwrap();
        let copied =
            ReplayEpochContext::from_checkpoint(target.clone(), checkpoint, AgentContext::empty())
                .unwrap();
        assert_eq!(copied.context().chronology().last(), Some(&original));
        assert!(tools(copied.context()).is_empty());
        let mut body = copied.into_owned_context();
        let (_, fresh) =
            crate::mcp_retrieved_manifest_for_action_result(&retrieval_result("copy-get"), &body)
                .unwrap();
        body.append_reference_event(
            ContextSourceKind::McpRetrievedManifest,
            "fresh copy retrieval",
            fresh,
        )
        .unwrap();
        assert_eq!(tools(&body).len(), 1);
        let live = ReplayEpochContext::from_owned_context(target.clone(), body).unwrap();
        let restored = ReplayEpochContext::from_checkpoint(
            target,
            live.checkpoint().unwrap(),
            AgentContext::empty(),
        )
        .unwrap();
        assert_eq!(tools(restored.context()).len(), 1);
        assert_eq!(tools(source.context()).len(), 1);
    }
}

fn retrieval_result(action_id: &str) -> crate::ActionResult {
    crate::ActionResult {
        protocol: "maap/1".into(),
        turn_id: "synthetic-turn".into(),
        agent_id: "synthetic-agent".into(),
        action_id: action_id.into(),
        action_type: "mcp_server_get",
        status: crate::ActionStatus::Succeeded,
        content: Vec::new(),
        structured_content_json: Some(
            serde_json::json!({"server": {
                "server_id":"synthetic", "state":"available", "display_name":"Synthetic",
                "purpose":"Synthetic coverage", "usage_instructions":"Use echo",
                "tools":[{"name":"echo", "state":"available", "description":"synthetic echo",
                    "input_schema":{"type":"object", "properties":{"value":{"type":"string"}}}}]
            }})
            .to_string(),
        ),
        permission_evaluation: None,
        is_error: false,
        error: None,
    }
}

/// Runtime admission deduplicates the returned manifest bytes. A new successful
/// retrieval must remain distinct from the old evidence whose grant was revoked.
#[test]
fn mcp_grants_fresh_action_result_is_distinct_from_revoked_retrieval() {
    let context = owner().into_owned_context();
    let first =
        crate::mcp_retrieved_manifest_for_action_result(&retrieval_result("first-get"), &context)
            .unwrap();
    let fresh =
        crate::mcp_retrieved_manifest_for_action_result(&retrieval_result("fresh-get"), &context)
            .unwrap();
    assert_eq!(first.0, fresh.0);
    assert_ne!(first.1, fresh.1);
    assert_eq!(
        first,
        crate::mcp_retrieved_manifest_for_action_result(&retrieval_result("first-get"), &context)
            .unwrap()
    );
}

/// A physical append after the head swap is not proof that retrieval occurred
/// in the new epoch. Original payload framing must survive current-born import.
#[test]
fn mcp_grants_delayed_old_scope_does_not_revive_on_current_born_import() {
    let mut source = owner();
    source.append_fact("source", "closed source").unwrap();
    let prepared = source
        .prepare_compaction_publication(
            plan(&source),
            ReplayHeadPublication::Summary("head".into()),
        )
        .unwrap();
    let late = retrieve(&mut source);
    assert_eq!(tools(source.context()).len(), 1);
    let cold = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        prepared.candidate().checkpoint().unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    let mut body = cold.into_owned_context();
    body.append_reference_event(
        late.block().source,
        &late.block().label,
        &late.block().content,
    )
    .unwrap();
    let mut cold = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        body,
    )
    .unwrap();
    assert_eq!(cold.members().last().unwrap().born_in(), cold.revision());
    assert_eq!(
        cold.context().chronology().last().unwrap().block(),
        late.block()
    );
    assert!(tools(cold.context()).is_empty());
    retrieve(&mut cold);
    assert_eq!(tools(cold.context()).len(), 1);
}

/// A delayed duplicate result is not a fresh retrieval. Rebuilding its grant
/// against the new live context must retain its original pre-acceptance scope.
#[test]
fn mcp_grants_duplicate_result_after_rewrite_cannot_acquire_new_scope() {
    let mut source = owner();
    source.append_fact("source", "closed source").unwrap();
    let frozen = plan(&source);
    let result = retrieval_result("original-get");
    let (_, content) =
        crate::mcp_retrieved_manifest_for_action_result(&result, source.context()).unwrap();
    let mut body = source.into_owned_context();
    body.append_reference_event(
        ContextSourceKind::McpRetrievedManifest,
        "retrieval",
        &content,
    )
    .unwrap();
    source = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        body,
    )
    .unwrap();
    source
        .publish_compaction(frozen, ReplayHeadPublication::Summary("head".into()))
        .unwrap();
    let repeated =
        crate::mcp_retrieved_manifest_for_action_result(&result, source.context()).unwrap();
    assert_eq!(repeated.1, content);
    let mut body = source.into_owned_context();
    body.append_reference_event(
        ContextSourceKind::McpRetrievedManifest,
        "duplicate",
        repeated.1,
    )
    .unwrap();
    assert!(tools(&body).is_empty());
}

#[test]
fn mcp_grants_malformed_foreign_future_and_unscoped_records_cannot_claim_freshness() {
    let mut source = owner();
    source.append_fact("source", "closed source").unwrap();
    source
        .publish_compaction(plan(&source), ReplayHeadPublication::Summary("head".into()))
        .unwrap();
    let (_, content) =
        crate::mcp_retrieved_manifest_for_action_result(&retrieval_result("get"), source.context())
            .unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
    let mut cases = Vec::new();
    for (field, replacement) in [
        ("conversation", serde_json::json!("foreign")),
        ("revision", serde_json::json!(2)),
        ("unexpected", serde_json::json!(true)),
    ] {
        let mut invalid = value.clone();
        invalid["replay_scope"][field] = replacement;
        cases.push(invalid);
    }
    let mut unscoped = value.clone();
    unscoped["replay_scope"] = serde_json::Value::Null;
    cases.push(unscoped);
    let mut missing = value.clone();
    missing.as_object_mut().unwrap().remove("replay_scope");
    cases.push(missing);
    let mut legacy = value.clone();
    legacy["version"] = serde_json::json!("mez-mcp-retrieved-manifest/v1");
    cases.push(legacy);
    let mut missing_action = value;
    missing_action["retrieval"]["action_id"] = serde_json::json!("");
    cases.push(missing_action);
    for invalid in cases {
        let mut body = source.clone().into_owned_context();
        body.append_reference_event(
            ContextSourceKind::McpRetrievedManifest,
            "invalid freshness",
            invalid.to_string(),
        )
        .unwrap();
        assert!(tools(&body).is_empty());
    }
}

/// Equal tool schemas have independent real retrieval identities. Appending
/// through the owned body also exercises the live continuation round trip.
fn retrieve(owner: &mut ReplayEpochContext) -> ConversationEvent {
    let mut body = owner.clone().into_owned_context();
    let action_id = String::from(ContextBlockOccurrenceId::allocate());
    let (_, content) =
        crate::mcp_retrieved_manifest_for_action_result(&retrieval_result(&action_id), &body)
            .unwrap();
    body.append_reference_event(
        ContextSourceKind::McpRetrievedManifest,
        "retrieved MCP manifest synthetic",
        content,
    )
    .unwrap();
    let event = body.chronology().last().unwrap().clone();
    *owner = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new("conversation-a").unwrap(),
        body,
    )
    .unwrap();
    event
}

/// Preparation is not acceptance. Actual adoption revokes retained and
/// during-write retrievals once; duplicate adoption cannot revoke newer grants.
#[test]
fn mcp_grants_revoke_only_at_acceptance_and_preserve_fresh_retrieval_on_duplicate() {
    let mut source = owner();
    source.append_fact("old", "actual closed source").unwrap();
    let frozen = plan(&source);
    let first = retrieve(&mut source);
    let prepared = source
        .prepare_compaction_publication(
            frozen,
            ReplayHeadPublication::Summary("accepted head".into()),
        )
        .unwrap();
    assert_eq!(tools(source.context()).len(), 1);
    let during = retrieve(&mut source);
    source.adopt_compaction_publication(&prepared).unwrap();
    assert!(tools(source.context()).is_empty());
    assert_eq!(
        source.context().chronology()[1..],
        [first.clone(), during.clone()]
    );
    let fresh = retrieve(&mut source);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&first.block().content).unwrap()["tools"],
        serde_json::from_str::<serde_json::Value>(&fresh.block().content).unwrap()["tools"]
    );
    assert_ne!(
        first.record().occurrence_id(),
        fresh.record().occurrence_id()
    );
    assert_eq!(tools(source.context()).len(), 1);
    let before = source.clone();
    source.adopt_compaction_publication(&prepared).unwrap();
    assert_eq!(source, before);
    assert_eq!(tools(source.context()).len(), 1);
    let checkpoint = source.checkpoint().unwrap();
    let cold = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        checkpoint,
        AgentContext::empty(),
    )
    .unwrap();
    assert_eq!(cold.context().chronology(), source.context().chronology());
    assert_eq!(tools(cold.context()).len(), 1);
}

#[test]
fn mcp_grants_failed_summary_keeps_original_availability() {
    let mut source = owner();
    source.append_fact("old", "actual source").unwrap();
    let frozen = plan(&source);
    retrieve(&mut source);
    let before = source.clone();
    assert!(
        source
            .publish_compaction(frozen, ReplayHeadPublication::Summary(String::new()))
            .is_err()
    );
    assert_eq!(source, before);
    assert_eq!(tools(source.context()).len(), 1);
}

/// Actual old v1 JSON keeps its original framing. It needs no provider call or
/// action replay: original births determine minimum old-epoch grant revocation.
#[test]
fn mcp_grants_checkpoint_v1_import_and_v2_required_denials_are_checked() {
    let mut source = owner();
    source.append_fact("old", "actual source").unwrap();
    let frozen = plan(&source);
    let evidence = retrieve(&mut source);
    source
        .publish_compaction(frozen, ReplayHeadPublication::Summary("head".into()))
        .unwrap();
    let value = serde_json::to_value(source.checkpoint().unwrap()).unwrap();
    assert_eq!(value["version"], 2);
    assert_eq!(value["retired_mcp_retrievals"].as_array().unwrap().len(), 1);
    let restore = |value| {
        ReplayEpochContext::from_checkpoint(
            ReplayConversationId::new("conversation-a").unwrap(),
            serde_json::from_value(value).unwrap(),
            AgentContext::empty(),
        )
    };
    let mut old = value.clone();
    old["version"] = serde_json::json!(1);
    old.as_object_mut()
        .unwrap()
        .remove("retired_mcp_retrievals");
    let old_dto: ReplayEpochCheckpoint = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(serde_json::to_value(&old_dto).unwrap(), old);
    let cold = restore(old).unwrap();
    assert!(tools(cold.context()).is_empty());
    assert_eq!(cold.context().chronology().last(), Some(&evidence));
    let mut missing = value.clone();
    missing
        .as_object_mut()
        .unwrap()
        .remove("retired_mcp_retrievals");
    assert!(restore(missing).is_err());
    let mut revived = value.clone();
    revived["retired_mcp_retrievals"] = serde_json::json!([]);
    assert!(restore(revived).is_err());
    let mut duplicate = value.clone();
    duplicate["retired_mcp_retrievals"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(evidence.record().occurrence_id()).unwrap());
    assert!(restore(duplicate).is_err());
    let mut unknown = value.clone();
    unknown["retired_mcp_retrievals"] = serde_json::json!([ContextBlockOccurrenceId::allocate()]);
    assert!(restore(unknown).is_err());
    let mut head = value.clone();
    head["retired_mcp_retrievals"] = serde_json::json!([source.head().unwrap().occurrence_id()]);
    assert!(restore(head).is_err());
    let mut wrong_version = value;
    wrong_version["version"] = serde_json::json!(1);
    assert!(restore(wrong_version).is_err());
}

/// Restrictive source-copy policy must persist even without a prior summary or
/// older birth revision. Source extension cannot downgrade that explicit denial.
#[test]
fn mcp_grants_explicit_revision_zero_denials_survive_checkpoint_and_source_checks() {
    let mut source = owner();
    let evidence = retrieve(&mut source);
    assert_eq!(tools(source.context()).len(), 1);
    source.revoke_retrieved_mcp_grants();
    assert!(tools(source.context()).is_empty());
    let value = serde_json::to_value(source.checkpoint().unwrap()).unwrap();
    let cold = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        serde_json::from_value(value.clone()).unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert!(tools(cold.context()).is_empty());
    assert_eq!(cold.context().chronology().last(), Some(&evidence));
    let mut downgraded = value;
    downgraded["version"] = serde_json::json!(1);
    downgraded
        .as_object_mut()
        .unwrap()
        .remove("retired_mcp_retrievals");
    let restored = ReplayEpochContext::from_checkpoint(
        ReplayConversationId::new("conversation-a").unwrap(),
        serde_json::from_value(downgraded).unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    assert!(restored.validate_source_extension(&source).is_err());
}

#[test]
fn mcp_grants_explicit_omission_revokes_retained_retrievals() {
    let mut source = ReplayEpochContext::new_for_head_policy(
        ReplayConversationId::new("conversation-a").unwrap(),
        AgentContext::empty(),
        ReplayHeadPolicy::IntentionalOmission,
    )
    .unwrap();
    source.append_fact("old", "source").unwrap();
    let frozen = plan(&source);
    let evidence = retrieve(&mut source);
    source
        .publish_compaction(frozen, ReplayHeadPublication::IntentionalOmission)
        .unwrap();
    assert!(source.head().is_none());
    assert_eq!(source.context().chronology(), [evidence]);
    assert!(tools(source.context()).is_empty());
}
