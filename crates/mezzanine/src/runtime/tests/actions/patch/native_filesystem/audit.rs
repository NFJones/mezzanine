//! Original audit registration and append custody after real filesystem work.
//!
//! Tests distinguish absent policy, synchronous writes, deferred acceptance,
//! replaced writers and failed/lost acknowledgements. Actual committed effects
//! and original raw evidence survive every later failure. Deferred acceptance
//! never stands in for physical persistence, archive or producer transfer.

use super::canonical::{execute_and_retain, result_events};
use super::*;
use crate::security::audit::{AuditActor, AuditConfig, AuditLog, AuditRecord};

/// Attaches a private original writer without changing root or policy epochs.
fn attach(fixture: &mut Fixture, enabled: bool, required: bool) -> PathBuf {
    let path = fixture.root.join("audit/events.jsonl");
    fixture.service.set_audit_log(AuditLog::new(AuditConfig {
        enabled,
        required,
        path: path.clone(),
        hash_chain: true,
    }));
    path
}

/// Reads actual writer bytes rather than inferring append success from phases.
fn records(path: &std::path::Path) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// A real confirmed effect produces one original filesystem audit record with
/// typed semantic outcome and actual enforcement, without fabricated shell
/// evidence. Duplicate projection cannot append again or release raw custody.
#[test]
fn native_filesystem_audit_appends_original_record_once() {
    let mut fixture = Fixture::new();
    let path = attach(&mut fixture, true, true);
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .session_id = "later-audit-cid".into();
    fixture
        .service
        .set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Pane));
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    let rows = records(&path);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["event_type"], "native_filesystem");
    assert_eq!(rows[0]["action"], "apply_patch");
    assert_eq!(rows[0]["outcome"], "succeeded");
    assert_eq!(
        rows[0]["metadata"]["conversation_id"],
        fixture.turn.conversation_id
    );
    assert_eq!(rows[0]["metadata"]["execution_target"], "native_runtime");
    assert_eq!(rows[0]["metadata"]["sent_to_pane"], "false");
    let semantic: serde_json::Value =
        serde_json::from_str(rows[0]["metadata"]["semantic_outcome"].as_str().unwrap()).unwrap();
    assert_eq!(semantic["effect_certainty"], "confirmed");
    assert_eq!(semantic["confirmed_endpoint_count"], 1);
    for fabricated in ["command", "stdout", "stderr", "exit_code", "signal"] {
        assert!(semantic.get(fabricated).is_none());
        assert!(rows[0]["metadata"].get(fabricated).is_none());
    }
    let (write, deferred) = fixture
        .service
        .filesystem_audit_append_for_tests(&attempt)
        .unwrap()
        .unwrap();
    assert_eq!(write.event_id, 1);
    assert!(write.hash.is_some());
    assert!(!deferred);
    assert!(
        !fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    assert_eq!(records(&path), rows);
    assert_eq!(result_events(&fixture), 1);
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
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
        "filesystem_audit_append_accepted"
    );
}

/// An adapter-owned accepted record retains its event/hash identity while the
/// actual JSONL bytes remain queued. Neither absence of a disk file nor draining
/// that queue may be mistaken for physical persistence or producer release.
#[test]
fn native_filesystem_audit_deferred_acceptance_is_not_physical_settlement() {
    let mut fixture = Fixture::new();
    fixture.service.use_audit_effect_adapter();
    let path = attach(&mut fixture, true, true);
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert!(!path.exists());
    let (write, deferred) = fixture
        .service
        .filesystem_audit_append_for_tests(&attempt)
        .unwrap()
        .unwrap();
    assert_eq!(write.event_id, 1);
    assert!(deferred);
    assert!(
        !fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    let transition = fixture.service.drain_audit_persistence_transition();
    assert_eq!(transition.side_effects.len(), 1);
    let RuntimeSideEffect::PersistAuditLog {
        path: actual,
        bytes,
        ..
    } = &transition.side_effects[0]
    else {
        panic!("expected actual deferred audit write");
    };
    assert_eq!(actual, &path);
    let row: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(row["event_id"], write.event_id);
    assert_eq!(row["event_type"], "native_filesystem");
    assert!(
        fixture
            .service
            .drain_audit_persistence_transition()
            .side_effects
            .is_empty()
    );
    assert!(!path.exists());
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}

/// Lost acknowledgement after a genuine synced append retains returned evidence
/// and the entered gate. Repeated callbacks preserve the first error and cannot
/// append again, erase actual effects or invent an accepted settlement.
#[test]
fn native_filesystem_audit_lost_ack_retains_evidence_without_replay() {
    let mut fixture = Fixture::new();
    let path = attach(&mut fixture, true, true);
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .reject_filesystem_audit_ack_for_tests(&attempt)
        .unwrap();
    let first = fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap_err();
    assert!(first.message().contains("after audit append"));
    assert_eq!(records(&path).len(), 1);
    assert!(
        fixture
            .service
            .filesystem_audit_append_for_tests(&attempt)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_audit_entered"
    );
    let repeat = fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap_err();
    assert_eq!(first.message(), repeat.message());
    assert_eq!(records(&path).len(), 1);
    assert_eq!(result_events(&fixture), 1);
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}

/// A genuine append failure after a confirmed mutation cannot be healed by
/// repairing the filesystem and replaying the entered audit operation. Original
/// typed success remains evidence of the mutation, separate from gate failure.
#[test]
fn native_filesystem_audit_io_failure_preserves_effects_and_first_error() {
    let mut fixture = Fixture::new();
    let path = attach(&mut fixture, true, true);
    fs::write(path.parent().unwrap(), b"not a directory").unwrap();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    let first = fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap_err();
    assert!(first.message().contains("native filesystem audit append"));
    fs::remove_file(path.parent().unwrap()).unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let repeat = fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap_err();
    assert_eq!(first.message(), repeat.message());
    assert!(!path.exists());
    assert!(
        fixture
            .service
            .filesystem_audit_append_for_tests(&attempt)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0].status,
        mez_agent::ActionStatus::Succeeded
    );
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
}

/// Equal settings/path, removal and a disabled replacement are all different
/// writer allocations. None may waive or satisfy the original record after
/// committed effects; installing another equal writer cannot reset that error.
#[test]
fn native_filesystem_audit_replacement_cannot_adopt_original_obligation() {
    for replacement in 0..3 {
        let mut fixture = Fixture::new();
        let path = attach(&mut fixture, true, true);
        let (attempt, _) = execute_and_retain(&mut fixture, false);
        if replacement == 0 {
            fixture.service.persistence.clear_audit_log();
        } else {
            fixture.service.set_audit_log(AuditLog::new(AuditConfig {
                enabled: replacement == 1,
                required: false,
                path: path.clone(),
                hash_chain: true,
            }));
        }
        let first = fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap_err();
        assert!(first.message().contains("original audit writer changed"));
        attach(&mut fixture, true, true);
        let repeat = fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap_err();
        assert_eq!(first.message(), repeat.message());
        assert!(!path.exists());
        assert_eq!(
            fs::read(fixture.root.join("file")).unwrap(),
            b"native data\n"
        );
        assert_eq!(
            fixture
                .service
                .filesystem_completion_phase_for_tests(&attempt)
                .unwrap(),
            "filesystem_audit_entered"
        );
    }
}

/// Required disabled logging refuses before producer issuance, retaining positive
/// unissued custody; an optional disabled writer records a distinct no-obligation
/// milestone without fabricating an append or blocking a permitted mutation.
#[test]
fn native_filesystem_audit_required_disabled_blocks_but_optional_is_explicit() {
    for required in [true, false] {
        let mut fixture = Fixture::new();
        let path = attach(&mut fixture, false, required);
        if required {
            let attempt = fixture.queue();
            let error = fixture
                .service
                .claim_native_filesystem_action(&attempt)
                .err()
                .unwrap();
            assert!(
                error
                    .message()
                    .contains("required audit writer is disabled")
            );
            fixture
                .service
                .take_unissued_native_filesystem_owner(&attempt)
                .unwrap();
            assert!(!fixture.root.join("file").exists());
        } else {
            let (attempt, _) = execute_and_retain(&mut fixture, false);
            fixture
                .service
                .project_native_filesystem_completion(&attempt)
                .unwrap();
            assert_eq!(
                fixture
                    .service
                    .filesystem_completion_phase_for_tests(&attempt)
                    .unwrap(),
                "filesystem_audit_not_required"
            );
            assert!(
                fixture
                    .service
                    .filesystem_audit_append_for_tests(&attempt)
                    .unwrap()
                    .is_none()
            );
            assert!(fixture.root.join("file").exists());
        }
        assert!(!path.exists());
    }
}

/// Writer removal after issuance is rechecked by the actual endpoint lease,
/// rather than only by queue/claim. The real worker cannot publish its prepared
/// file and reports no confirmed endpoint; staged cleanup owns only its stage.
#[test]
fn native_filesystem_audit_source_is_rechecked_before_actual_commit() {
    use crate::runtime::agent::native_filesystem::authority::RuntimeNativeFilesystemAuthority;
    let mut fixture = Fixture::new();
    attach(&mut fixture, true, true);
    let attempt = fixture.queue();
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
    fixture.service.persistence.clear_audit_log();
    let outcome = execute_native_patch(
        request,
        RuntimeNativeFilesystemAuthority::new(&mut fixture.service, &attempt),
        |_| {},
    );
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Unexecuted);
    assert!(outcome.receipts.is_empty());
    assert!(!fixture.root.join("file").exists());
}

/// A legitimate exclusively staged append continues the same registered writer.
/// A queued native obligation survives it and advances the actual event/hash
/// state instead of forking or resetting the original stream at settlement.
#[test]
fn native_filesystem_audit_staged_continuation_preserves_registration_and_sequence() {
    let mut fixture = Fixture::new();
    let path = attach(&mut fixture, true, true);
    let (attempt, _) = execute_and_retain(&mut fixture, false);
    let identity = fixture.service.persistence.audit_writer_identity().unwrap();
    fixture
        .service
        .persistence
        .with_staged_audit_log(|writer| {
            writer.append(AuditRecord::new(
                "original-session",
                AuditActor {
                    kind: "runtime".into(),
                    id: "staged".into(),
                },
                "client",
                "detach",
            ))?;
            Ok(())
        })
        .unwrap();
    assert!(
        fixture
            .service
            .persistence
            .audit_writer_is_current(&identity)
    );
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    let rows = records(&path);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["event_id"], 1);
    assert_eq!(rows[1]["event_id"], 2);
    assert_eq!(rows[1]["event_type"], "native_filesystem");
    assert_eq!(
        fixture
            .service
            .filesystem_audit_append_for_tests(&attempt)
            .unwrap()
            .unwrap()
            .0
            .event_id,
        2
    );
}

/// A failed staged continuation after a real append cannot discard its accepted
/// deferred bytes or present the original registration as healthy. The pending
/// native obligation retains canonical effects and fails closed without replay,
/// for both actual synced writes and adapter-owned acceptance.
#[test]
fn native_filesystem_audit_failed_staged_append_is_retained_and_unproven() {
    for deferred in [false, true] {
        let mut fixture = Fixture::new();
        if deferred {
            fixture.service.use_audit_effect_adapter();
        }
        let path = attach(&mut fixture, true, true);
        let (attempt, raw) = execute_and_retain(&mut fixture, false);
        let error = fixture
            .service
            .persistence
            .with_staged_audit_log(|writer| {
                writer.append(AuditRecord::new(
                    "original-session",
                    AuditActor {
                        kind: "runtime".into(),
                        id: "staged-failure".into(),
                    },
                    "client",
                    "detach",
                ))?;
                Err::<(), _>(MezError::conflict("failure after actual staged append"))
            })
            .unwrap_err();
        assert!(error.message().contains("actual staged append"));
        assert!(fixture.service.persistence.audit_writer_is_unproven());
        let first = fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap_err();
        assert!(first.message().contains("unproven staged append"));
        let repeat = fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap_err();
        assert_eq!(first.message(), repeat.message());
        if deferred {
            assert!(!path.exists());
            assert_eq!(
                fixture
                    .service
                    .drain_audit_persistence_transition()
                    .side_effects
                    .len(),
                1
            );
        } else {
            assert_eq!(records(&path).len(), 1);
        }
        assert!(Arc::ptr_eq(
            &raw,
            &fixture
                .service
                .retained_native_filesystem_outcome(&attempt)
                .unwrap()
        ));
        assert_eq!(result_events(&fixture), 1);
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
}

/// A panic after a genuine staged append must retain the same custody as a
/// returned failure. Unwinding cannot discard queued bytes or leave an old
/// event/hash state apparently healthy for the original native obligation.
#[test]
fn native_filesystem_audit_staged_unwind_keeps_append_custody() {
    for deferred in [false, true] {
        let mut fixture = Fixture::new();
        if deferred {
            fixture.service.use_audit_effect_adapter();
        }
        let path = attach(&mut fixture, true, true);
        let (attempt, raw) = execute_and_retain(&mut fixture, false);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = fixture.service.persistence.with_staged_audit_log(
                |writer| -> crate::error::Result<()> {
                    writer.append(AuditRecord::new(
                        "original-session",
                        AuditActor {
                            kind: "runtime".into(),
                            id: "staged-unwind".into(),
                        },
                        "client",
                        "detach",
                    ))?;
                    panic!("fixture unwind after actual staged append");
                },
            );
        }));
        assert!(panic.is_err());
        if !deferred {
            assert_eq!(records(&path).len(), 1);
        }
        assert!(fixture.service.persistence.audit_writer_is_unproven());
        if deferred {
            assert!(!path.exists());
            assert_eq!(
                fixture
                    .service
                    .drain_audit_persistence_transition()
                    .side_effects
                    .len(),
                1
            );
        }
        let error = fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap_err();
        assert!(error.message().contains("unproven staged append"));
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
        assert!(
            !fixture
                .service
                .completion_provider_continuation_ready(&fixture.turn.turn_id)
        );
    }
}
