//! Exact physical audit acknowledgements under original filesystem ownership.
//!
//! Real descriptor effects precede real ordered persistence workers. Tests
//! distinguish opaque receipts from path/byte notifications, retain failures
//! and lost acknowledgements, and forbid replay or premature producer release.

use super::async_producer::ActorFixture;
use super::canonical::execute_and_retain;
use super::*;
use crate::host::async_runtime::{
    AsyncRuntimeSideEffectServiceConfig, run_async_persistence_side_effect_service,
};
use crate::runtime::{PersistenceEvent, PersistenceTarget};
use crate::security::audit::{AuditActor, AuditAppendStatus, AuditConfig, AuditLog, AuditRecord};
use std::time::Duration;

/// Attaches an actual deferred original writer while preserving source epochs.
fn attach(fixture: &mut Fixture) -> PathBuf {
    let path = fixture.root.join("physical-audit/events.jsonl");
    fixture.service.use_audit_effect_adapter();
    fixture.service.set_audit_log(AuditLog::new(AuditConfig {
        enabled: true,
        required: true,
        path: path.clone(),
        hash_chain: true,
    }));
    path
}

/// Executes the actual ordered persistence service, including actor event ingress.
async fn persist(actor: &ActorFixture, effects: Vec<RuntimeSideEffect>) -> (u64, u64, u64, usize) {
    actor
        .handle
        .queue_runtime_side_effects(effects)
        .await
        .unwrap();
    let report = run_async_persistence_side_effect_service(
        &actor.handle,
        AsyncRuntimeSideEffectServiceConfig {
            max_polls: 2,
            drain_limit: 32,
            idle_interval: Duration::from_millis(1),
        },
        |polls, _| polls >= 2,
    )
    .await
    .unwrap();
    (
        report.completed,
        report.failed,
        report.audit_batches,
        report.bytes_written,
    )
}

/// Ordinary records before/after the tracked native record remain in the same
/// actor-owned ordered persistence lane. Exact duplicates observe the joined
/// result without another row, write count, context result or producer release.
#[tokio::test]
async fn native_filesystem_audit_actual_worker_preserves_order_and_deduplicates_physical_io() {
    let mut fixture = Fixture::new();
    let path = attach(&mut fixture);
    let ordinary = |name| {
        AuditRecord::new(
            "session",
            AuditActor {
                kind: "runtime".into(),
                id: "ordered".into(),
            },
            "ordinary",
            name,
        )
    };
    fixture
        .service
        .persistence
        .audit_log_mut()
        .unwrap()
        .append(ordinary("before"))
        .unwrap();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    fixture
        .service
        .persistence
        .audit_log_mut()
        .unwrap()
        .append(ordinary("after"))
        .unwrap();
    let effects = fixture
        .service
        .drain_audit_persistence_transition()
        .side_effects;
    assert_eq!(effects.len(), 3);
    let tracked = effects[1].clone();
    assert!(matches!(
        tracked,
        RuntimeSideEffect::PersistTrackedAuditLog { .. }
    ));
    let actor = ActorFixture::new(fixture);
    let (completed, failed, batches, _) = persist(&actor, effects).await;
    assert_eq!(completed, 3);
    assert_eq!(failed, 0);
    assert_eq!(batches, 3);
    let (completed, _, batches, bytes) = persist(&actor, vec![tracked]).await;
    assert_eq!(completed, 0);
    assert_eq!(bytes, 0);
    assert_eq!(batches, 0);
    assert!(
        !actor
            .handle
            .project_native_filesystem_completion(attempt.clone())
            .await
            .unwrap()
    );
    let fixture = actor.finish().await;
    let rows: Vec<serde_json::Value> = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["action"], "before");
    assert_eq!(rows[1]["event_type"], "native_filesystem");
    assert_eq!(rows[2]["action"], "after");
    assert_eq!(
        rows.iter()
            .map(|row| row["event_id"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_publication_accepted"
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
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}

/// Actual deferred I/O failure is delivered through its original receipt. Repair
/// and duplicate side effects cannot replay an uncertain operation or erase the
/// original committed filesystem effect and first required-gate failure.
#[tokio::test]
async fn native_filesystem_audit_actual_worker_failure_cannot_replay_after_repair() {
    let mut fixture = Fixture::new();
    let path = attach(&mut fixture);
    fs::write(path.parent().unwrap(), b"not a directory").unwrap();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    let effects = fixture
        .service
        .drain_audit_persistence_transition()
        .side_effects;
    let actor = ActorFixture::new(fixture);
    let (_, failed, _, _) = persist(&actor, effects.clone()).await;
    assert_eq!(failed, 1);
    let first = actor
        .handle
        .project_native_filesystem_completion(attempt.clone())
        .await
        .unwrap_err();
    assert!(first.message().contains("physical audit settlement"));
    fs::remove_file(path.parent().unwrap()).unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let (completed, _, _, bytes) = persist(&actor, effects).await;
    assert_eq!(bytes, 0);
    assert_eq!(completed, 0);
    assert!(!path.exists());
    let repeated = actor
        .handle
        .project_native_filesystem_completion(attempt.clone())
        .await
        .unwrap_err();
    assert_eq!(first.message(), repeated.message());
    let fixture = actor.finish().await;
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

/// Path/byte success notifications and observations of a still-queued receipt
/// cannot acknowledge physical work. A genuinely completed/joined write can be
/// reconciled after its notification is lost, without reexecuting either effect.
#[tokio::test]
async fn native_filesystem_audit_lost_notification_requires_actual_original_physical_proof() {
    let mut fixture = Fixture::new();
    let path = attach(&mut fixture);
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    let effects = fixture
        .service
        .drain_audit_persistence_transition()
        .side_effects;
    let RuntimeSideEffect::PersistTrackedAuditLog { work } = &effects[0] else {
        panic!("expected original tracked work");
    };
    let receipt = work.receipt().unwrap();
    fixture
        .service
        .apply_persistence_transition(PersistenceEvent::Completed {
            target: PersistenceTarget::AuditLog,
            path: path.clone(),
            bytes: work.byte_count(),
        })
        .unwrap();
    fixture
        .service
        .apply_persistence_transition(PersistenceEvent::AuditAppendObserved {
            receipt: receipt.clone(),
        })
        .unwrap();
    assert!(!path.exists());
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_audit_append_accepted"
    );
    assert!(matches!(
        work.execute_owned().await.unwrap(),
        (AuditAppendStatus::Accepted, true)
    ));
    assert!(receipt.worker_joined());
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_audit_append_accepted"
    );
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_publication_accepted"
    );
    assert!(
        !fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 1);
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

/// Late positive physical evidence remains attached to the original owner after
/// append acknowledgement loss. It cannot clear that first error, replay an
/// audit record or convert the failed append milestone into healthy settlement.
#[tokio::test]
async fn native_filesystem_audit_late_physical_evidence_preserves_append_ack_failure() {
    let mut fixture = Fixture::new();
    let path = attach(&mut fixture);
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .reject_filesystem_audit_ack_for_tests(&attempt)
        .unwrap();
    let first = fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap_err();
    let effects = fixture
        .service
        .drain_audit_persistence_transition()
        .side_effects;
    let RuntimeSideEffect::PersistTrackedAuditLog { work } = &effects[0] else {
        panic!("expected original tracked work");
    };
    work.execute_owned().await.unwrap();
    let physical = work.receipt().unwrap();
    assert!(physical.worker_joined());
    assert!(matches!(physical.status(), AuditAppendStatus::Accepted));
    let observed = fixture
        .service
        .observe_native_filesystem_audit(&physical)
        .unwrap_err();
    assert_eq!(observed.message(), first.message());
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_audit_entered"
    );
    assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 1);
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
