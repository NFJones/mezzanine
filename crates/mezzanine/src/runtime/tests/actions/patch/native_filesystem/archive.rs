//! Fresh archive projections from actual moved original filesystem backing.
//!
//! Real descriptor effects arrive after forced retirement has staged an older
//! candidate. Archive restaging must use the late original facts, never merely
//! change a witness around old rows or adopt replacement live-map values.

use super::canonical::execute_and_retain;
use super::*;

/// Real positive filesystem facts arrive after forced retirement moved the
/// original context/execution. Restaging must include their canonical result,
/// preserve the same archive target, and remain stable on a duplicate callback.
#[test]
fn native_filesystem_archive_reprojects_late_original_failed_backing() {
    let mut fixture = Fixture::new();
    fixture
        .service
        .set_agent_transcript_store(AgentTranscriptStore::new(fixture.root.join("archive")));
    fixture.service.persistence.enable_transcript_adapter();
    let (attempt, _) = execute_and_retain(&mut fixture, false);
    assert!(
        fixture
            .service
            .prepare_completion_forced_retirement(&fixture.turn)
            .is_err()
    );
    let before = fixture
        .service
        .retained_completion_snapshot_for_tests(&fixture.turn.turn_id)
        .unwrap()
        .unwrap();
    assert!(
        !String::from_utf8(before.clone())
            .unwrap()
            .contains("Native patch completed; 1 confirmed file endpoint(s).")
    );
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    let after = fixture
        .service
        .publish_and_stage_completion_sources_for_tests(&fixture.turn.turn_id)
        .unwrap();
    assert!(
        String::from_utf8(after.clone())
            .unwrap()
            .contains("Native patch completed; 1 confirmed file endpoint(s)."),
        "restaged archive omitted actual late filesystem result"
    );
    let before: serde_json::Value = serde_json::from_slice(&before).unwrap();
    let candidate: serde_json::Value = serde_json::from_slice(&after).unwrap();
    assert_eq!(candidate["path"], before["path"]);
    assert_eq!(candidate["conversation"], before["conversation"]);
    assert_eq!(
        fixture
            .service
            .publish_and_stage_completion_sources_for_tests(&fixture.turn.turn_id)
            .unwrap(),
        after
    );
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}

/// Original moved source outranks a later same-key execution/context and its
/// imported-history metadata. The fixture boundary marks its own seeded typed
/// group as imported; this tests projection isolation, not a historical reader.
#[test]
fn native_filesystem_archive_ignores_replacement_maps_and_keeps_import_boundary() {
    let mut fixture = Fixture::with_context(
        false,
        |root| {
            vec![mez_agent::AgentAction {
                id: "native-file".into(),
                payload: mez_agent::AgentActionPayload::ApplyPatch {
                    patch: format!(
                        "*** Begin Patch\n*** Add File: {}\n+native data\n*** End Patch",
                        root.join("file").display()
                    ),
                    strip: None,
                },
            }]
        },
        |service, turn| {
            let context = service
                .agent_turn_contexts_mut()
                .get_mut(&turn.turn_id)
                .unwrap();
            let imported =
                mez_agent::ContextExecutionGroupId::new("fixture-imported-original").unwrap();
            context
                .append_assistant_event(
                    "imported decision",
                    "IMPORTED_DO_NOT_REAPPEND",
                    imported.clone(),
                )
                .unwrap();
            context
                .append_evidence_event(
                    mez_agent::ContextSourceKind::ActionResult,
                    "imported evidence",
                    "IMPORTED_RESULT_DO_NOT_REAPPEND",
                    imported,
                    None,
                    true,
                )
                .unwrap();
            let high_water = context.event_sequence_high_water_mark();
            service.set_agent_turn_imported_history_sequence_high_water(
                turn.turn_id.clone(),
                high_water,
            );
        },
    );
    fixture
        .service
        .set_agent_transcript_store(AgentTranscriptStore::new(
            fixture.root.join("original-archive"),
        ));
    fixture.service.persistence.enable_transcript_adapter();
    let (attempt, _) = execute_and_retain(&mut fixture, false);
    let replacement_context = fixture.service.agent_turn_contexts()[&fixture.turn.turn_id].clone();
    assert!(
        fixture
            .service
            .prepare_completion_forced_retirement(&fixture.turn)
            .is_err()
    );
    let mut replacement = fixture.execution.clone();
    replacement.response.raw_text = "REPLACEMENT_EXECUTION_MUST_NOT_ARCHIVE".into();
    fixture
        .service
        .agent_turn_executions_mut()
        .insert(fixture.turn.turn_id.clone(), replacement.clone());
    let mut replacement_context = replacement_context;
    replacement_context
        .append_user_event("replacement", "REPLACEMENT_CONTEXT_MUST_NOT_ARCHIVE")
        .unwrap();
    fixture
        .service
        .agent_turn_contexts_mut()
        .insert(fixture.turn.turn_id.clone(), replacement_context.clone());
    fixture
        .service
        .set_agent_turn_imported_history_sequence_high_water(fixture.turn.turn_id.clone(), 0);
    fixture
        .service
        .set_agent_transcript_store(AgentTranscriptStore::new(
            fixture.root.join("replacement-archive"),
        ));
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    let bytes = fixture
        .service
        .publish_and_stage_completion_sources_for_tests(&fixture.turn.turn_id)
        .unwrap();
    let candidate: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let rows = candidate["entries"].as_array().unwrap();
    assert!(rows.iter().any(|entry| {
        entry["content"]
            .as_str()
            .unwrap()
            .contains("Native patch completed; 1 confirmed file endpoint(s).")
    }));
    for marker in [
        "REPLACEMENT_EXECUTION_MUST_NOT_ARCHIVE",
        "REPLACEMENT_CONTEXT_MUST_NOT_ARCHIVE",
        "IMPORTED_DO_NOT_REAPPEND",
        "IMPORTED_RESULT_DO_NOT_REAPPEND",
    ] {
        assert!(
            !rows
                .iter()
                .any(|entry| entry["content"].as_str().unwrap().contains(marker)),
            "unexpected source: {marker}"
        );
    }
    assert_eq!(
        fixture.service.agent_turn_executions()[&fixture.turn.turn_id],
        replacement
    );
    assert_eq!(
        fixture.service.agent_turn_contexts()[&fixture.turn.turn_id],
        replacement_context
    );
    // Duplicate cleanup captures neither replacement context nor its watermark.
    assert!(
        fixture
            .service
            .prepare_completion_forced_retirement(&fixture.turn)
            .is_err()
    );
    let repeated = fixture
        .service
        .publish_and_stage_completion_sources_for_tests(&fixture.turn.turn_id)
        .unwrap();
    assert_eq!(repeated, bytes);
}

/// Seeds settled canonical source and runs the actual compaction completion
/// producer before the pending filesystem assistant is installed. Offline test
/// provider output is deterministic; no model or filesystem effect is replayed.
fn compacted_fixture() -> Fixture {
    Fixture::with_context(
        false,
        |root| {
            vec![mez_agent::AgentAction {
                id: "native-file".into(),
                payload: mez_agent::AgentActionPayload::ApplyPatch {
                    patch: format!(
                        "*** Begin Patch\n*** Add File: {}\n+native data\n*** End Patch",
                        root.join("file").display()
                    ),
                    strip: None,
                },
            }]
        },
        |service, turn| {
            let context = service
                .agent_turn_contexts_mut()
                .get_mut(&turn.turn_id)
                .unwrap();
            for index in 0..2 {
                let group =
                    mez_agent::ContextExecutionGroupId::new(format!("original-compacted-{index}"))
                        .unwrap();
                context
                    .append_assistant_event(
                        "original raw source",
                        "ORIGINAL_COMPACTION_RAW ".repeat(3000),
                        group.clone(),
                    )
                    .unwrap();
                context
                    .append_evidence_event(
                        mez_agent::ContextSourceKind::ActionResult,
                        "original settled result",
                        "ORIGINAL_COMPACTION_RESULT",
                        group,
                        None,
                        true,
                    )
                    .unwrap();
                context
                    .append_user_event("steering", format!("ORIGINAL_COMPACTION_BARRIER_{index}"))
                    .unwrap();
            }
            let plan = mez_agent::plan_model_context_compaction_for_provider_tokens(
                context,
                2000,
                0,
                context.event_sequence_high_water_mark(),
                mez_agent::ProviderBudgetProjection::new(
                    mez_agent::ProviderApiCompatibility::OpenAiResponses,
                    "runtime-batch",
                ),
            )
            .unwrap();
            assert!(plan.changes_context());
            let profile = service
                .agent_turn_model_profile(&turn.turn_id)
                .unwrap()
                .clone();
            assert!(service.queue_agent_active_turn_compaction(&turn.turn_id, "archive-source".into(), profile,
            crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ProviderContextLimit {attempt:1}, plan).unwrap());
            for index in 0..8 {
                if service
                    .pending_agent_compaction_task_for_tests("%1")
                    .is_none()
                {
                    break;
                }
                complete_runtime_test_compaction(
                    service,
                    "%1",
                    &format!("ORIGINAL_ARCHIVE_SUMMARY_{index}"),
                );
            }
            assert!(
                service
                    .pending_agent_compaction_task_for_tests("%1")
                    .is_none()
            );
            assert!(
                !service.agent.agent_turn_compaction_handoffs[&turn.turn_id]
                    .witnesses
                    .is_empty()
            );
        },
    )
}

/// Actual accepted compaction witnesses move with original context and survive
/// removal/replacement of the live metadata maps. Restaging restores their raw
/// occurrences and terminal certificate alongside later native result facts.
#[test]
fn native_filesystem_archive_keeps_actual_compaction_witnesses_after_detach() {
    let mut fixture = compacted_fixture();
    fixture
        .service
        .set_agent_transcript_store(AgentTranscriptStore::new(fixture.root.join("archive")));
    fixture.service.persistence.enable_transcript_adapter();
    let (attempt, _) = execute_and_retain(&mut fixture, false);
    let handoff =
        fixture.service.agent.agent_turn_compaction_handoffs[&fixture.turn.turn_id].clone();
    assert!(
        fixture
            .service
            .prepare_completion_forced_retirement(&fixture.turn)
            .is_err()
    );
    assert!(
        !fixture
            .service
            .agent
            .agent_turn_compaction_handoffs
            .contains_key(&fixture.turn.turn_id)
    );
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    let bytes = fixture
        .service
        .publish_and_stage_completion_sources_for_tests(&fixture.turn.turn_id)
        .unwrap();
    let candidate: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let rows = candidate["entries"].as_array().unwrap();
    let certificates = rows
        .iter()
        .filter_map(|entry| entry["content"].as_str())
        .filter(|content| content.starts_with("[mez-terminal-compaction-handoff/"))
        .collect::<Vec<_>>();
    assert_eq!(certificates.len(), 1);
    for witness in &handoff.witnesses {
        assert!(certificates[0].contains(&witness.summary));
    }
    assert!(rows.iter().any(|entry| {
        entry["content"]
            .as_str()
            .unwrap()
            .contains("ORIGINAL_COMPACTION_RAW")
    }));
    assert!(rows.iter().any(|entry| {
        entry["content"]
            .as_str()
            .unwrap()
            .contains("Native patch completed; 1 confirmed file endpoint(s).")
    }));
    assert_eq!(
        fixture
            .service
            .publish_and_stage_completion_sources_for_tests(&fixture.turn.turn_id)
            .unwrap(),
        bytes
    );
}

/// Captured absence is final even when an older candidate and later same-key
/// context exist. A real late effect remains retained; restaging must fail
/// rather than publish old rows or borrow the replacement as its original owner.
#[test]
fn native_filesystem_archive_missing_captured_context_never_uses_replacement() {
    let mut fixture = Fixture::new();
    fixture
        .service
        .set_agent_transcript_store(AgentTranscriptStore::new(fixture.root.join("archive")));
    fixture.service.persistence.enable_transcript_adapter();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .publish_and_stage_completion_sources_for_tests(&fixture.turn.turn_id)
        .unwrap();
    let mut replacement = fixture
        .service
        .agent_turn_contexts_mut()
        .remove(&fixture.turn.turn_id)
        .unwrap();
    assert!(
        fixture
            .service
            .prepare_completion_forced_retirement(&fixture.turn)
            .is_err()
    );
    let retained = fixture
        .service
        .retained_completion_snapshot_for_tests(&fixture.turn.turn_id)
        .unwrap()
        .unwrap();
    replacement
        .append_user_event("replacement", "MISSING_ORIGINAL_MUST_NOT_ADOPT")
        .unwrap();
    fixture
        .service
        .agent_turn_contexts_mut()
        .insert(fixture.turn.turn_id.clone(), replacement.clone());
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .is_err()
    );
    assert!(
        fixture
            .service
            .publish_and_stage_completion_sources_for_tests(&fixture.turn.turn_id)
            .is_err()
    );
    assert_eq!(
        fixture
            .service
            .retained_completion_snapshot_for_tests(&fixture.turn.turn_id)
            .unwrap()
            .unwrap(),
        retained
    );
    assert_eq!(
        fixture.service.agent_turn_contexts()[&fixture.turn.turn_id],
        replacement
    );
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
}

/// A genuine compaction witness with a changed accepted anchor is invalidated
/// before capture. Moved metadata must keep that failure even if a clean-looking
/// handoff is later inserted in the live map; stale candidate bytes stay owned
/// without being relabelled as freshly projected accepted source.
#[test]
fn native_filesystem_archive_rejects_changed_original_compaction_witness() {
    let mut fixture = compacted_fixture();
    fixture
        .service
        .set_agent_transcript_store(AgentTranscriptStore::new(fixture.root.join("archive")));
    fixture.service.persistence.enable_transcript_adapter();
    let (attempt, _) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .publish_and_stage_completion_sources_for_tests(&fixture.turn.turn_id)
        .unwrap();
    let clean = fixture.service.agent.agent_turn_compaction_handoffs[&fixture.turn.turn_id].clone();
    fixture
        .service
        .agent
        .agent_turn_compaction_handoffs
        .get_mut(&fixture.turn.turn_id)
        .unwrap()
        .witnesses[0]
        .summary
        .push_str("CHANGED_ANCHOR");
    assert!(
        fixture
            .service
            .prepare_completion_forced_retirement(&fixture.turn)
            .is_err()
    );
    let retained = fixture
        .service
        .retained_completion_snapshot_for_tests(&fixture.turn.turn_id)
        .unwrap()
        .unwrap();
    fixture
        .service
        .agent
        .agent_turn_compaction_handoffs
        .insert(fixture.turn.turn_id.clone(), clean);
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert!(
        fixture
            .service
            .publish_and_stage_completion_sources_for_tests(&fixture.turn.turn_id)
            .is_err()
    );
    assert_eq!(
        fixture
            .service
            .retained_completion_snapshot_for_tests(&fixture.turn.turn_id)
            .unwrap()
            .unwrap(),
        retained
    );
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
}
