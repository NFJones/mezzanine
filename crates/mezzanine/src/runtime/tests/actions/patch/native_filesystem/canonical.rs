//! Canonical filesystem facts under actual original completion-group ownership.
//!
//! Real descriptor workers produce the positive and partial receipts. Tests
//! verify canonical/context deduplication, full modification counts, failed
//! required gates and moved original backing. They do not acknowledge remaining
//! audit/publication/archive gates or certify dispatcher/supervisor cutover.

use super::*;
use crate::runtime::agent::native_filesystem::{
    NativeFilesystemAttempt, authority::RuntimeNativeFilesystemAuthority,
};
use crate::runtime::processes::native_patch::worker::{
    NativePatchWorkerRequest, execute_native_patch,
};

/// Executes actual descriptor work before the producer delivers its evidence.
/// Optional post-first-commit cancellation exercises partial move attribution.
#[cfg(target_os = "linux")]
fn execute_raw(
    fixture: &mut Fixture,
    cancel_after_first: bool,
) -> (NativeFilesystemAttempt, NativePatchWorkerOutcome) {
    let attempt = fixture.queue();
    let dispatch = fixture
        .service
        .claim_native_filesystem_action(&attempt)
        .unwrap()
        .unwrap();
    let fence = dispatch.fence.clone();
    let request = NativePatchWorkerRequest {
        identity: dispatch.work.identity.clone(),
        root: dispatch.work.source.capture_root().unwrap(),
        scopes: dispatch.scopes,
        fence: dispatch.fence,
    };
    let outcome = execute_native_patch(
        request,
        RuntimeNativeFilesystemAuthority::new(&mut fixture.service, &attempt),
        |_| {
            if cancel_after_first {
                fence.cancel();
            }
        },
    );
    (attempt, outcome)
}

/// Retains the original actual worker Arc after execution, without projection.
pub(super) fn execute_and_retain(
    fixture: &mut Fixture,
    cancel_after_first: bool,
) -> (NativeFilesystemAttempt, Arc<NativePatchWorkerOutcome>) {
    let (attempt, outcome) = execute_raw(fixture, cancel_after_first);
    let outcome = Arc::new(outcome);
    fixture
        .service
        .retain_native_filesystem_outcome(&attempt, Arc::clone(&outcome))
        .unwrap();
    (attempt, outcome)
}

/// Counts actual immutable chronology entries for the original action only.
pub(super) fn result_events(fixture: &Fixture) -> usize {
    fixture
        .service
        .agent_turn_contexts()
        .get(&fixture.turn.turn_id)
        .unwrap()
        .chronology()
        .iter()
        .filter(|event| {
            event.block().source == mez_agent::ContextSourceKind::ActionResult
                && event.block().label == format!("action result {}", fixture.action.id)
        })
        .count()
}

/// Successful original effects become one typed canonical result and chronology
/// entry, with actual modification counts. Duplicate projection cannot repeat
/// counts, mutate files, release issued custody or bypass remaining group gates.
#[cfg(target_os = "linux")]
#[test]
fn canonical_filesystem_success_commits_typed_facts_once_without_acknowledging_settlement() {
    let mut fixture = Fixture::new();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    let result = &fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0];
    assert_eq!(result.status, mez_agent::ActionStatus::Succeeded);
    let metadata: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_ref().unwrap()).unwrap();
    assert_eq!(metadata["execution_target"], "native_runtime");
    assert_eq!(metadata["sent_to_pane"], false);
    assert_eq!(metadata["effect_certainty"], "confirmed");
    assert_eq!(metadata["confirmed_endpoints"][0]["added_lines"], 1);
    for fabricated in ["stdout", "stderr", "exit_code", "signal", "shell_command"] {
        assert!(metadata.get(fabricated).is_none());
    }
    assert_eq!(result_events(&fixture), 1);
    assert!(
        !fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    assert_eq!(result_events(&fixture), 1);
    let path = fixture.root.join("file").to_str().unwrap().to_owned();
    let modified = fixture.service.retained_agent_modified_files("%1").unwrap();
    assert_eq!(modified[&path].added, 1);
    assert_eq!(modified[&path].removed, 0);
    assert_eq!(fs::read(&path).unwrap(), b"native data\n");
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
    assert!(
        fixture
            .service
            .take_unissued_native_filesystem_owner(&attempt)
            .is_err()
    );
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_publication_accepted"
    );
}

/// An independently committed earlier file survives a later planning error. Its
/// endpoint and diff enter the failed canonical result exactly once; failure is
/// not reported as an unexecuted or atomically rolled-back patch.
#[cfg(target_os = "linux")]
#[test]
fn canonical_filesystem_partial_keeps_confirmed_file_when_later_operation_fails() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Add File: {}\n+first\n*** Update File: {}\n@@\n-old\n+new\n*** End Patch",
            root.join("one").display(),
            root.join("missing").display()
        )
    });
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Partial);
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    let result = &fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0];
    assert_eq!(result.status, mez_agent::ActionStatus::Failed);
    let metadata: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_ref().unwrap()).unwrap();
    assert_eq!(metadata["effect_certainty"], "partial");
    assert_eq!(metadata["confirmed_endpoints"].as_array().unwrap().len(), 1);
    assert_eq!(metadata["confirmed_effects"].as_array().unwrap().len(), 1);
    assert_eq!(fs::read(fixture.root.join("one")).unwrap(), b"first\n");
    assert!(!fixture.root.join("missing").exists());
    assert_eq!(result_events(&fixture), 1);
}

/// Destination publication followed by cancellation is a positive partial move
/// endpoint, never a completed move. Canonical facts retain the new destination
/// and original source while whole-move confirmation remains empty.
#[cfg(target_os = "linux")]
#[test]
fn canonical_filesystem_partial_move_preserves_endpoint_without_whole_move_success() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n-old\n+new\n*** End Patch",
            root.join("a").display(),
            root.join("z").display()
        )
    });
    fs::write(fixture.root.join("a"), b"old\n").unwrap();
    let (attempt, raw) = execute_and_retain(&mut fixture, true);
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Partial);
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    let result = &fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0];
    let metadata: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_ref().unwrap()).unwrap();
    assert!(metadata["confirmed_effects"].as_array().unwrap().is_empty());
    assert_eq!(metadata["confirmed_endpoints"].as_array().unwrap().len(), 1);
    assert_eq!(metadata["confirmed_endpoints"][0]["endpoint_ordinal"], 0);
    assert_eq!(fs::read(fixture.root.join("a")).unwrap(), b"old\n");
    assert_eq!(fs::read(fixture.root.join("z")).unwrap(), b"new\n");
}

/// Losing the actual context at its required boundary retains canonical facts,
/// original raw Arc and first error. Restoring context cannot replay that entered
/// gate or falsely convert a failed projection acknowledgement into success.
#[cfg(target_os = "linux")]
#[test]
fn canonical_filesystem_context_failure_retains_original_evidence_and_first_failure() {
    let mut fixture = Fixture::new();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    let context = fixture
        .service
        .agent_turn_contexts_mut()
        .remove(&fixture.turn.turn_id)
        .unwrap();
    let first = fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap_err();
    assert_eq!(
        fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0].status,
        mez_agent::ActionStatus::Succeeded
    );
    fixture
        .service
        .agent_turn_contexts_mut()
        .insert(fixture.turn.turn_id.clone(), context);
    let repeated = fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap_err();
    assert_eq!(first.kind(), repeated.kind());
    assert_eq!(first.message(), repeated.message());
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_context_entered"
    );
    assert_eq!(result_events(&fixture), 0);
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
    assert!(
        fixture
            .service
            .ensure_completion_required_gates_ready(&fixture.turn.turn_id)
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
}

/// A forced retirement moves actual original execution/context into failed
/// backing. Later confirmed facts update that backing without replacing a newer
/// current response and without resetting the original required failure.
#[cfg(target_os = "linux")]
#[test]
fn canonical_filesystem_late_facts_update_original_failed_backing_not_latest_execution() {
    let mut fixture = Fixture::new();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    let failure = fixture
        .service
        .prepare_completion_forced_retirement(&fixture.turn)
        .unwrap_err();
    assert!(
        !fixture
            .service
            .agent_turn_executions()
            .contains_key(&fixture.turn.turn_id)
    );
    let mut replacement = fixture.execution.clone();
    replacement.response.raw_text = "later current response".into();
    fixture
        .service
        .agent_turn_executions_mut()
        .insert(fixture.turn.turn_id.clone(), replacement.clone());
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    assert_eq!(
        fixture.service.agent_turn_executions()[&fixture.turn.turn_id],
        replacement
    );
    assert!(
        fixture
            .service
            .ensure_completion_required_gates_ready(&fixture.turn.turn_id)
            .is_err()
    );
    assert_eq!(
        fixture
            .service
            .ensure_completion_required_gates_ready(&fixture.turn.turn_id)
            .unwrap_err()
            .message(),
        failure.message()
    );
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_publication_held"
    );
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
}

/// Display truncation must not truncate full per-file modification counts. The
/// actual canonical result still names both independently committed endpoints
/// and records the second file even after the aggregate native diff budget ends.
#[cfg(target_os = "linux")]
#[test]
fn canonical_filesystem_truncated_diff_keeps_full_modification_counts() {
    let mut fixture = Fixture::with_patch(false, |root| {
        let text = "line\n"
            .repeat(30_000)
            .lines()
            .map(|line| format!("+{line}\n"))
            .collect::<String>();
        format!(
            "*** Begin Patch\n*** Add File: {}\n{text}*** Add File: {}\n{text}*** End Patch",
            root.join("a").display(),
            root.join("b").display()
        )
    });
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Confirmed);
    assert!(raw.receipts.iter().any(|receipt| receipt.diff_truncated));
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    let modified = fixture.service.retained_agent_modified_files("%1").unwrap();
    for name in ["a", "b"] {
        assert_eq!(
            modified[fixture.root.join(name).to_str().unwrap()].added,
            30_000
        );
    }
}

/// Contradictory certainty cannot erase actual positive receipts or manufacture
/// an unexecuted canonical result. This is deliberate outcome fault injection
/// after a real commit; the unchanged raw Arc remains available for diagnosis.
#[test]
fn canonical_filesystem_rejects_unexecuted_certainty_with_real_positive_receipt() {
    let mut fixture = Fixture::new();
    let (attempt, mut raw) = execute_raw(&mut fixture, false);
    raw.result.certainty = NativeEffectCertainty::Unexecuted;
    let raw = Arc::new(raw);
    fixture
        .service
        .retain_native_filesystem_outcome(&attempt, Arc::clone(&raw))
        .unwrap();
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .is_err()
    );
    assert_eq!(
        fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0].status,
        mez_agent::ActionStatus::Running
    );
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
    assert_eq!(result_events(&fixture), 0);
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
}

/// A producer that incorrectly labels a destination-only move completed cannot
/// enter whole-move success into chronology. Real destination bytes and the
/// original source survive; malformed original evidence stays retained.
#[test]
fn canonical_filesystem_rejects_whole_move_success_without_source_delete_receipt() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n-old\n+new\n*** End Patch",
            root.join("a").display(),
            root.join("z").display()
        )
    });
    fs::write(fixture.root.join("a"), b"old\n").unwrap();
    let (attempt, mut raw) = execute_raw(&mut fixture, true);
    assert_eq!(raw.receipts.len(), 1);
    raw.completed_operations.push(0);
    raw.result.confirmed_effects = raw.result.identity.operation.effects.clone();
    raw.result.certainty = NativeEffectCertainty::Confirmed;
    raw.result.diagnostic = None;
    let raw = Arc::new(raw);
    fixture
        .service
        .retain_native_filesystem_outcome(&attempt, Arc::clone(&raw))
        .unwrap();
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .is_err()
    );
    assert_eq!(result_events(&fixture), 0);
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
    assert_eq!(fs::read(fixture.root.join("a")).unwrap(), b"old\n");
    assert_eq!(fs::read(fixture.root.join("z")).unwrap(), b"new\n");
}

/// A projection requested before any original outcome is observed must refuse
/// without poisoning queued admission. The same actual owner can subsequently
/// claim, execute and project once its real worker evidence arrives.
#[test]
fn canonical_filesystem_early_projection_refusal_does_not_poison_queued_claim() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .is_err()
    );
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_queued"
    );
    assert!(
        fixture
            .service
            .ensure_completion_required_gates_ready(&fixture.turn.turn_id)
            .is_ok()
    );
    let dispatch = fixture
        .service
        .claim_native_filesystem_action(&attempt)
        .unwrap()
        .unwrap();
    let request = NativePatchWorkerRequest {
        identity: dispatch.work.identity.clone(),
        root: dispatch.work.source.capture_root().unwrap(),
        scopes: dispatch.scopes,
        fence: dispatch.fence,
    };
    let raw = Arc::new(execute_native_patch(
        request,
        RuntimeNativeFilesystemAuthority::new(&mut fixture.service, &attempt),
        |_| {},
    ));
    fixture
        .service
        .retain_native_filesystem_outcome(&attempt, raw)
        .unwrap();
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    assert_eq!(result_events(&fixture), 1);
}

/// Fault-injects missing mandatory receipts after actual successful mutation.
/// Completed operation labels alone must not manufacture canonical success.
fn reject_missing_mandatory_receipts(kind: &str) {
    let mut fixture = Fixture::with_patch(false, |root| match kind {
        "create" => format!(
            "*** Begin Patch\n*** Add File: {}\n+new\n*** End Patch",
            root.join("file").display()
        ),
        "delete" => format!(
            "*** Begin Patch\n*** Delete File: {}\n*** End Patch",
            root.join("file").display()
        ),
        "move" => format!(
            "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n-old\n+new\n*** End Patch",
            root.join("file").display(),
            root.join("moved").display()
        ),
        _ => panic!("unknown fixture operation"),
    });
    if kind != "create" {
        fs::write(fixture.root.join("file"), b"old\n").unwrap();
    }
    let (attempt, mut raw) = execute_raw(&mut fixture, false);
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Confirmed);
    assert!(!raw.receipts.is_empty());
    assert_eq!(raw.completed_operations, vec![0]);
    raw.receipts.clear();
    raw.result.confirmed_effects.clear();
    let raw = Arc::new(raw);
    fixture
        .service
        .retain_native_filesystem_outcome(&attempt, Arc::clone(&raw))
        .unwrap();
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .is_err()
    );
    assert_eq!(result_events(&fixture), 0);
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
}

/// A genuinely committed create whose returned receipts were erased must be
/// rejected instead of being mislabeled a successful semantic no-op.
#[test]
fn canonical_filesystem_rejects_completed_create_without_any_endpoint_receipt() {
    reject_missing_mandatory_receipts("create");
}

/// A genuinely committed deletion requires its positive endpoint receipt even
/// when the returned completed-operation list still names the original action.
#[test]
fn canonical_filesystem_rejects_completed_delete_without_any_endpoint_receipt() {
    reject_missing_mandatory_receipts("delete");
}

/// Removing both move receipts must not evade the source-deletion dependency
/// check: completed move intent requires both positively confirmed endpoints.
#[test]
fn canonical_filesystem_rejects_completed_move_without_any_endpoint_receipt() {
    reject_missing_mandatory_receipts("move");
}

/// Partial mutation certainty requires a positively confirmed endpoint. This
/// deliberately erases genuine earlier receipts before first delivery and must
/// remain a validation failure, with original malformed evidence retained.
#[test]
fn canonical_filesystem_rejects_partial_certainty_without_any_endpoint_receipt() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Add File: {}\n+first\n*** Update File: {}\n@@\n-old\n+new\n*** End Patch",
            root.join("one").display(),
            root.join("missing").display()
        )
    });
    let (attempt, mut raw) = execute_raw(&mut fixture, false);
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Partial);
    assert!(!raw.receipts.is_empty());
    raw.receipts.clear();
    raw.result.confirmed_effects.clear();
    let raw = Arc::new(raw);
    fixture
        .service
        .retain_native_filesystem_outcome(&attempt, Arc::clone(&raw))
        .unwrap();
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .is_err()
    );
    assert_eq!(result_events(&fixture), 0);
    assert_eq!(fs::read(fixture.root.join("one")).unwrap(), b"first\n");
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
}

/// A real update that leaves bytes unchanged is the legitimate completed no-op.
/// It requires no endpoint receipt, creates one canonical success observation,
/// and records no fabricated modified-file entry.
#[test]
fn canonical_filesystem_accepts_actual_update_noop_without_endpoint_receipts() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Update File: {}\n@@\n-same\n+same\n*** End Patch",
            root.join("file").display()
        )
    });
    fs::write(fixture.root.join("file"), b"same\n").unwrap();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Confirmed);
    assert!(raw.receipts.is_empty());
    assert_eq!(raw.completed_operations, vec![0]);
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    assert_eq!(result_events(&fixture), 1);
    assert_eq!(
        fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0].status,
        mez_agent::ActionStatus::Succeeded
    );
    assert!(
        fixture
            .service
            .retained_agent_modified_files("%1")
            .is_none()
    );
    assert_eq!(fs::read(fixture.root.join("file")).unwrap(), b"same\n");
}

/// The actual shared planner requires a missing move destination. Actor commit
/// admission must reject a typed proposal captured from an existing destination,
/// independently of the worker's normal earlier planner rejection.
#[test]
fn canonical_filesystem_rejects_invalid_move_destination_lease() {
    use crate::runtime::processes::native_patch::lease::NativePatchCommitProposal;
    use crate::security::filesystem::capability::NativeFileCapability;
    use mez_agent::semantic_patch_planning::{ApplyPatchFileChange, ApplyPatchOriginalState};
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n-old\n+new\n*** End Patch",
            root.join("a").display(),
            root.join("z").display()
        )
    });
    fs::write(fixture.root.join("a"), b"old\n").unwrap();
    fs::write(fixture.root.join("z"), b"existing\n").unwrap();
    let attempt = fixture.queue();
    let dispatch = fixture
        .service
        .claim_native_filesystem_action(&attempt)
        .unwrap()
        .unwrap();
    let root = dispatch.work.source.capture_root().unwrap();
    let path = fixture.root.join("z").to_str().unwrap().to_owned();
    let capability = NativeFileCapability::capture(&root, &dispatch.scopes, &path, true).unwrap();
    let change = ApplyPatchFileChange {
        path: path.clone(),
        resolved_path: path,
        original: ApplyPatchOriginalState::Regular(b"existing\n".to_vec()),
        final_bytes: Some(b"new\n".to_vec()),
    };
    let proposal = NativePatchCommitProposal::capture(&capability, &change, 0, 0).unwrap();
    assert!(
        fixture
            .service
            .authorize_native_filesystem_commit(&attempt, &dispatch.work.identity, &proposal)
            .is_err()
    );
    assert_eq!(fs::read(fixture.root.join("z")).unwrap(), b"existing\n");
    assert_eq!(fs::read(fixture.root.join("a")).unwrap(), b"old\n");
}

/// A move's real destination create cannot be reclassified as replacement in a
/// returned receipt. This faults only the typed kind after genuine worker output
/// and must preserve the malformed raw evidence without canonical success.
#[test]
fn canonical_filesystem_rejects_invalid_move_destination_receipt() {
    use crate::runtime::processes::native_patch::worker::NativePatchEndpointKind;
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n-old\n+new\n*** End Patch",
            root.join("a").display(),
            root.join("z").display()
        )
    });
    fs::write(fixture.root.join("a"), b"old\n").unwrap();
    let (attempt, mut raw) = execute_raw(&mut fixture, false);
    assert_eq!(raw.receipts.len(), 2);
    assert_eq!(raw.receipts[0].kind, NativePatchEndpointKind::Created);
    raw.receipts[0].kind = NativePatchEndpointKind::Updated;
    let raw = Arc::new(raw);
    fixture
        .service
        .retain_native_filesystem_outcome(&attempt, Arc::clone(&raw))
        .unwrap();
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .is_err()
    );
    assert_eq!(result_events(&fixture), 0);
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
    assert_eq!(fs::read(fixture.root.join("z")).unwrap(), b"new\n");
    assert!(!fixture.root.join("a").exists());
}
