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
    let mut fixture = Fixture::with_transcript_store("archive");
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
    let mut fixture = Fixture::with_context_and_store(
        Some("original-archive"),
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

/// Completes a real earlier task with ordinary append settlement still pending.
/// Its successor makes that source eligible, then actual epoch publication makes
/// the head durable without claiming the pending raw archive has been appended.
/// The native assistant is installed only after the actual receipt is adopted.
fn compacted_fixture() -> Fixture {
    Fixture::with_history_and_context(
        Some("archive"),
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
        |service| {
            service
                .replace_config_layers(vec![ConfigLayer {
                    name: "native-archive-compaction".into(),
                    path: None,
                    format: ConfigFormat::Toml,
                    scope: ConfigScope::Primary,
                    trusted: true,
                    text: r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "native-archive"
shell_mode = "native"
compaction_raw_retention_percent = 10
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.native-archive]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
max_input_tokens = 20000
"#
                    .into(),
                }])
                .unwrap();
            service.persistence.enable_transcript_adapter();
            let start = service
                .start_agent_prompt_turn(
                    "%1",
                    &format!("ORIGINAL_COMPACTION_RAW {}", "source ".repeat(5_000)),
                )
                .unwrap();
            crate::test_support::compaction_source::complete_task(
                service,
                &start.turn_id,
                "ORIGINAL_COMPACTION_RESULT",
                mez_agent::ProviderApiCompatibility::OpenAiResponses,
            );
            // Admit the real checked bookkeeping proposal, leaving its ordinary
            // append with the external writer. Unchecked bookkeeping correctly
            // fences the next task; logical acceptance is still no disk ACK.
            let work = service.claim_bookkeeping_candidates().pop().unwrap();
            let history = work.execute();
            assert!(
                service
                    .complete_bookkeeping_candidate(work, history)
                    .unwrap()
            );
        },
        |service, turn| {
            let store = service.persistence.transcript_store().unwrap().clone();
            assert!(
                !store
                    .transcript_path(&turn.conversation_id)
                    .unwrap()
                    .exists()
            );
            let pending = service
                .persistence
                .pending_transcript_entries_at_path(
                    &store.transcript_path(&turn.conversation_id).unwrap(),
                    &turn.conversation_id,
                )
                .unwrap();
            assert!(
                pending
                    .iter()
                    .any(|row| row.content.contains("ORIGINAL_COMPACTION_RAW"))
            );
            let profile = service
                .agent_turn_model_profile(&turn.turn_id)
                .unwrap()
                .clone();
            assert!(service.queue_agent_active_turn_compaction(
                &turn.turn_id, "native-archive".into(), profile,
                crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                    observed_input_tokens: 40_000, max_input_tokens: 40_000,
                },
            ).unwrap());
            assert!(
                service
                    .pending_agent_compaction_task_for_tests("%1")
                    .unwrap()
                    .replay_source
                    .is_some()
            );
            complete_runtime_test_compaction(service, "%1", "ORIGINAL_ARCHIVE_SUMMARY");
            let receipt =
                crate::test_support::compaction_publication::execute_pending_publication(service);
            assert!(
                service
                    .apply_replay_epoch_completion(receipt)
                    .unwrap()
                    .applied
            );
            assert!(
                service
                    .pending_agent_compaction_task_for_tests("%1")
                    .is_none()
            );
            // Publication primes the archive/index with an empty file, while
            // these raw rows remain owned by their original pending append.
            assert!(store.inspect(&turn.conversation_id).unwrap().is_empty());
            assert_eq!(
                service
                    .persistence
                    .pending_transcript_entries_at_path(
                        &store.transcript_path(&turn.conversation_id).unwrap(),
                        &turn.conversation_id,
                    )
                    .unwrap(),
                pending
            );
            let (_, cold) = store
                .load_replay_epoch(&turn.conversation_id, mez_agent::AgentContext::empty())
                .unwrap()
                .unwrap();
            assert_eq!(
                cold.context().chronology()[0].block().content,
                "ORIGINAL_ARCHIVE_SUMMARY"
            );
        },
    )
}

/// The accepted head and source survive actual context detachment while ordinary
/// source append is unsettled. Late native facts use the original moved owner;
/// no second terminal rewrite/certificate is manufactured during restaging.
#[test]
fn native_filesystem_archive_keeps_actual_compaction_witnesses_after_detach() {
    let mut fixture = compacted_fixture();
    let store = fixture
        .service
        .persistence
        .transcript_store()
        .unwrap()
        .clone();
    let (_, accepted) = store
        .load_replay_epoch(
            &fixture.turn.conversation_id,
            mez_agent::AgentContext::empty(),
        )
        .unwrap()
        .unwrap();
    let original = fixture.service.agent_turn_contexts()[&fixture.turn.turn_id].clone();
    let source = fixture
        .service
        .agent_turn_history_source(&fixture.turn.turn_id)
        .unwrap()
        .clone();
    let (attempt, _) = execute_and_retain(&mut fixture, false);
    assert!(
        fixture
            .service
            .prepare_completion_forced_retirement(&fixture.turn)
            .is_err()
    );
    assert!(
        !fixture
            .service
            .agent_turn_contexts()
            .contains_key(&fixture.turn.turn_id)
    );
    assert!(
        fixture
            .service
            .agent_turn_history_source(&fixture.turn.turn_id)
            .is_none()
    );
    // A later same-key live body/source must not replace the captured originals.
    let mut replacement = original;
    replacement
        .append_user_event("replacement", "REPLACEMENT_CONTEXT_MUST_NOT_ARCHIVE")
        .unwrap();
    fixture
        .service
        .agent_turn_contexts_mut()
        .insert(fixture.turn.turn_id.clone(), replacement);
    fixture
        .service
        .set_agent_turn_history_source(fixture.turn.turn_id.clone(), Some(source));
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
    assert!(!rows.iter().any(|entry| {
        entry["content"]
            .as_str()
            .unwrap()
            .starts_with("[mez-terminal-compaction-handoff/")
    }));
    assert!(
        !String::from_utf8(bytes.clone())
            .unwrap()
            .contains("REPLACEMENT_CONTEXT_MUST_NOT_ARCHIVE")
    );
    assert!(rows.iter().any(|entry| {
        entry["content"]
            .as_str()
            .unwrap()
            .contains("Native patch completed; 1 confirmed file endpoint(s).")
    }));
    let (_, cold) = AgentTranscriptStore::new(store.root())
        .load_replay_epoch(
            &fixture.turn.conversation_id,
            mez_agent::AgentContext::empty(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(cold.head(), accepted.head());
    assert_eq!(cold.context().chronology(), accepted.context().chronology());
    assert_eq!(
        fixture
            .service
            .publish_and_stage_completion_sources_for_tests(&fixture.turn.turn_id)
            .unwrap(),
        bytes
    );
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
}

/// Captured absence is final even when an older candidate and later same-key
/// context exist. A real late effect remains retained; restaging must fail
/// rather than publish old rows or borrow the replacement as its original owner.
#[test]
fn native_filesystem_archive_missing_captured_context_never_uses_replacement() {
    let mut fixture = Fixture::with_transcript_store("archive");
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

/// An unpublished replacement head cannot claim the accepted source's archive
/// authority. Capture keeps that mismatch after clean live context/source maps
/// are restored, while the real late filesystem effect remains retained.
#[test]
fn native_filesystem_archive_rejects_changed_original_compaction_witness() {
    let mut fixture = compacted_fixture();
    let (attempt, _) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .publish_and_stage_completion_sources_for_tests(&fixture.turn.turn_id)
        .unwrap();
    let clean = fixture.service.agent_turn_contexts()[&fixture.turn.turn_id].clone();
    let source = fixture
        .service
        .agent_turn_history_source(&fixture.turn.turn_id)
        .unwrap()
        .clone();
    fixture
        .service
        .agent_turn_contexts_mut()
        .get_mut(&fixture.turn.turn_id)
        .unwrap()
        .update_replay_owner(|owner| {
            let mez_agent::ReplayCompactionPreparation::Ready(plan) =
                owner.prepare_compaction(mez_agent::ReplayTailPolicy::new(20_000, 10)?)?
            else {
                panic!("accepted head must remain eligible for a replacement rewrite");
            };
            owner.publish_compaction(
                plan,
                mez_agent::ReplayHeadPublication::Summary("CHANGED_ANCHOR".into()),
            )?;
            Ok(())
        })
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
    fixture
        .service
        .agent_turn_contexts_mut()
        .insert(fixture.turn.turn_id.clone(), clean);
    fixture
        .service
        .set_agent_turn_history_source(fixture.turn.turn_id.clone(), Some(source));
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    let error = fixture
        .service
        .publish_and_stage_completion_sources_for_tests(&fixture.turn.turn_id)
        .unwrap_err();
    assert!(
        error.message().contains("original accepted replay head"),
        "{error}"
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
