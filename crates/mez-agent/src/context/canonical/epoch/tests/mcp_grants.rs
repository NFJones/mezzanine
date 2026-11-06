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

/// Equal retrieval bytes have independent real admission identities. Appending
/// through the owned body also exercises the live continuation round trip.
fn retrieve(owner: &mut ReplayEpochContext) -> ConversationEvent {
    let mut body = owner.clone().into_owned_context();
    body.append_reference_event(
        ContextSourceKind::McpRetrievedManifest,
        "retrieved MCP manifest synthetic",
        serde_json::json!({"version":"mez-mcp-retrieved-manifest/v1", "server_id":"synthetic",
            "tools":[{"name":"echo", "description":"synthetic echo",
                "input_schema":{"type":"object", "properties":{"value":{"type":"string"}}}}]})
        .to_string(),
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
    assert_eq!(first.block(), fresh.block());
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
