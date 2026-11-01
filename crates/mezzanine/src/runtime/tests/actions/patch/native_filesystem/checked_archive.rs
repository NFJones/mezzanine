//! Actual checked archive admission, distinct from retention and physical I/O.
//!
//! Real native descriptor effects feed the original canonical source. Actual
//! candidate claims, coherent history checks and append ownership mint receipts;
//! stale work, substituted receipt allocations and failed reads cannot do so.
//! These tests release no producer and do not claim durable append or root cutover.

use super::canonical::execute_and_retain;
use super::*;
use crate::runtime::agent::RuntimeCompletionCheckedSource;
use crate::runtime::agent::native_filesystem::NativeFilesystemAttempt;

/// Keeps the original store before admission and enables off-actor work.
fn saved_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture
        .service
        .set_agent_transcript_store(AgentTranscriptStore::new(fixture.root.join("archive")));
    fixture.service.persistence.enable_transcript_adapter();
    fixture
}

/// Commits one real effect and canonical facts; archive ownership stays pending.
fn project(fixture: &mut Fixture) -> NativeFilesystemAttempt {
    let (attempt, _) = execute_and_retain(fixture, false);
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    attempt
}

/// Retains and checks one actual original candidate through its normal owner.
/// No boolean or synthetic checked-source token is supplied to the actor.
fn check(fixture: &mut Fixture, attempt: &NativeFilesystemAttempt) {
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(attempt)
            .unwrap()
            .is_none()
    );
    let mut work = fixture.service.claim_bookkeeping_candidates();
    assert_eq!(work.len(), 1);
    let work = work.pop().unwrap();
    let history = work.execute();
    assert!(
        fixture
            .service
            .complete_bookkeeping_candidate(work, history)
            .unwrap()
    );
}

/// Retention, a genuine checked read, and append admission are separate facts.
/// The accepted gate is opaque and original, while physical archive rows are
/// still queued and all raw/group/producer retirement obligations remain held.
#[test]
fn native_filesystem_checked_archive_requires_actual_checked_admission() {
    let mut fixture = saved_fixture();
    let store = fixture
        .service
        .persistence
        .cloned_transcript_store()
        .unwrap();
    let attempt = project(&mut fixture);
    let raw = fixture
        .service
        .retained_native_filesystem_outcome(&attempt)
        .unwrap();
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(&attempt)
            .unwrap()
            .is_none()
    );
    let mut work = fixture.service.claim_bookkeeping_candidates();
    assert_eq!(work.len(), 1);
    let work = work.pop().unwrap();
    let source = work.candidate.checked_source.clone().unwrap();
    assert!(!source.accepted());
    let history = work.execute();
    assert!(!source.accepted());
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(&attempt)
            .unwrap()
            .is_none()
    );
    let stale = work.clone();
    assert!(
        fixture
            .service
            .complete_bookkeeping_candidate(work, history)
            .unwrap()
    );
    assert!(source.accepted());
    let accepted = fixture
        .service
        .prepare_native_filesystem_archive(&attempt)
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .service
            .native_filesystem_archive_acceptance_is_current(&accepted)
    );
    assert!(
        !fixture
            .service
            .complete_bookkeeping_candidate(stale, Ok((Vec::new(), 1)))
            .unwrap()
    );
    assert!(source.accepted());
    match store.inspect(&fixture.turn.conversation_id) {
        Ok(rows) => assert!(rows.is_empty()),
        Err(error) => assert_eq!(error.kind(), crate::error::MezErrorKind::NotFound),
    }
    assert!(fixture.service.claim_bookkeeping_candidates().is_empty());
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
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
}

/// A changed history result cannot reuse an actual read's private observation.
/// First error stays in its exact receipt, the candidate remains blocked, and
/// restoring a later callback cannot convert it into checked acceptance.
#[test]
fn native_filesystem_checked_archive_rejects_changed_checked_history() {
    let mut fixture = saved_fixture();
    let attempt = project(&mut fixture);
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(&attempt)
            .unwrap()
            .is_none()
    );
    let work = fixture
        .service
        .claim_bookkeeping_candidates()
        .pop()
        .unwrap();
    let source = work.candidate.checked_source.clone().unwrap();
    let (mut history, sequence) = work.execute().unwrap();
    // Empty checked history still has an exact digest; injecting a real-source
    // projection row must not be accepted as that worker's observed archive.
    history.push(work.candidate.entries[0].clone());
    let first = fixture
        .service
        .complete_bookkeeping_candidate(work.clone(), Ok((history, sequence)))
        .unwrap_err();
    assert!(!source.accepted());
    assert_eq!(source.failure().unwrap().message(), first.message());
    assert!(
        !fixture
            .service
            .complete_bookkeeping_candidate(work, Ok((Vec::new(), 1)))
            .unwrap()
    );
    let repeated = fixture
        .service
        .prepare_native_filesystem_archive(&attempt)
        .unwrap_err();
    assert_eq!(repeated.message(), first.message());
    assert!(fixture.service.claim_bookkeeping_candidates().is_empty());
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
}

/// Correct generation, snapshot bytes and real read data cannot replace the
/// receipt allocation retained by the live claim. A substituted pending token
/// cannot mint acceptance; the original owner records the failed check.
#[test]
fn native_filesystem_checked_archive_rejects_substituted_receipt_allocation() {
    let mut fixture = saved_fixture();
    let attempt = project(&mut fixture);
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(&attempt)
            .unwrap()
            .is_none()
    );
    let mut work = fixture
        .service
        .claim_bookkeeping_candidates()
        .pop()
        .unwrap();
    let original = work.candidate.checked_source.clone().unwrap();
    let substituted = RuntimeCompletionCheckedSource::pending();
    work.candidate.checked_source = Some(substituted.clone());
    let history = work.execute();
    assert!(
        fixture
            .service
            .complete_bookkeeping_candidate(work, history)
            .is_err()
    );
    assert!(!original.accepted());
    assert!(original.failure().is_some());
    assert!(!substituted.accepted());
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(&attempt)
            .is_err()
    );
}

/// Newly appended canonical steering changes the exact snapshot. An older
/// accepted source cannot attest the new candidate; the normal owner must check
/// it anew, even though previous physical append work is still merely queued.
#[test]
fn native_filesystem_checked_archive_invalidates_old_snapshot_acceptance() {
    let mut fixture = saved_fixture();
    let attempt = project(&mut fixture);
    check(&mut fixture, &attempt);
    let old = fixture
        .service
        .prepare_native_filesystem_archive(&attempt)
        .unwrap()
        .unwrap();
    fixture
        .service
        .agent_turn_contexts_mut()
        .get_mut(&fixture.turn.turn_id)
        .unwrap()
        .append_user_event("steering", "NEW_ORIGINAL_SOURCE_NEEDS_CHECK")
        .unwrap();
    assert!(
        !fixture
            .service
            .native_filesystem_archive_acceptance_is_current(&old),
        "saved acceptance did not recheck changed canonical source"
    );
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(&attempt)
            .unwrap()
            .is_none()
    );
    assert!(
        !fixture
            .service
            .native_filesystem_archive_acceptance_is_current(&old)
    );
    let work = fixture
        .service
        .claim_bookkeeping_candidates()
        .pop()
        .unwrap();
    let history = work.execute();
    assert!(
        fixture
            .service
            .complete_bookkeeping_candidate(work, history)
            .unwrap()
    );
    let new = fixture
        .service
        .prepare_native_filesystem_archive(&attempt)
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .service
            .native_filesystem_archive_acceptance_is_current(&new)
    );
    assert!(
        !fixture
            .service
            .native_filesystem_archive_acceptance_is_current(&old)
    );
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
}

/// Original no-save policy remains explicit after a repository is attached.
/// It creates no fake candidate or physical write receipt and releases no raw
/// outcome. Matching policy metadata in another actual actor is insufficient.
#[test]
fn native_filesystem_checked_archive_keeps_original_runtime_only_policy() {
    let mut fixture = Fixture::new();
    let attempt = project(&mut fixture);
    let accepted = fixture
        .service
        .prepare_native_filesystem_archive(&attempt)
        .unwrap()
        .unwrap();
    fixture
        .service
        .set_agent_transcript_store(AgentTranscriptStore::new(
            fixture.root.join("later-archive"),
        ));
    fixture.service.persistence.enable_transcript_adapter();
    assert!(
        fixture
            .service
            .native_filesystem_archive_acceptance_is_current(&accepted)
    );
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(&attempt)
            .unwrap()
            .is_some()
    );
    assert!(fixture.service.claim_bookkeeping_candidates().is_empty());
    let mut other = Fixture::new();
    let _other_attempt = project(&mut other);
    assert!(
        !other
            .service
            .native_filesystem_archive_acceptance_is_current(&accepted)
    );
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}

/// Native archive preparation must not select the legacy inline I/O fallback.
/// Enabling the real off-actor adapter afterward leaves the original source
/// eligible for actual checked work; this refusal was not an entered append.
#[test]
fn native_filesystem_checked_archive_refuses_inline_actor_persistence() {
    let mut fixture = Fixture::new();
    fixture
        .service
        .set_agent_transcript_store(AgentTranscriptStore::new(fixture.root.join("archive")));
    let attempt = project(&mut fixture);
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(&attempt)
            .is_err()
    );
    assert!(fixture.service.claim_bookkeeping_candidates().is_empty());
    fixture.service.persistence.enable_transcript_adapter();
    check(&mut fixture, &attempt);
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(&attempt)
            .unwrap()
            .is_some()
    );
}

/// Real actor RPC and the actual joined descriptor producer preserve source
/// custody across a lost archive reply. A later opaque gate remains attributable
/// to the same original actor and does not remove its retained raw outcome.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_checked_archive_actor_reply_loss_keeps_custody() {
    use super::async_producer::ActorFixture;
    use crate::host::async_runtime::native_filesystem::{
        claim_and_start_native_filesystem_producer, prepare_joined_native_filesystem_archive,
    };
    use crate::runtime::processes::native_filesystem_producer::NativeFilesystemProducer;
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt.clone());
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    let first = prepare_joined_native_filesystem_archive(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    let mut lost = Box::pin(
        actor
            .handle
            .prepare_native_filesystem_archive(attempt.clone()),
    );
    assert!(futures_util::poll!(lost.as_mut()).is_pending());
    drop(lost);
    let accepted = actor
        .handle
        .prepare_native_filesystem_archive(attempt.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(
        actor
            .handle
            .validate_native_filesystem_archive(accepted.clone())
            .await
            .unwrap()
    );
    let mut fixture = actor.finish().await;
    assert!(
        fixture
            .service
            .native_filesystem_archive_acceptance_is_current(&first)
    );
    assert!(
        fixture
            .service
            .native_filesystem_archive_acceptance_is_current(&accepted)
    );
    assert!(Arc::ptr_eq(
        &producer.observe().await.unwrap().unwrap(),
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

/// A valid original-path archive row may share a turn/content identity while
/// carrying another actor or pane. Checked source acceptance must reject that
/// attribution collision instead of treating foreign metadata as the original
/// native result. Both controls use an actual coherent store read and real I/O.
#[test]
fn native_filesystem_checked_archive_rejects_foreign_result_attribution() {
    for changed in ["agent", "pane"] {
        let mut fixture = saved_fixture();
        let store = fixture
            .service
            .persistence
            .cloned_transcript_store()
            .unwrap();
        let attempt = project(&mut fixture);
        assert!(
            fixture
                .service
                .prepare_native_filesystem_archive(&attempt)
                .unwrap()
                .is_none()
        );
        let work = fixture
            .service
            .claim_bookkeeping_candidates()
            .pop()
            .unwrap();
        let source = work.candidate.checked_source.clone().unwrap();
        let mut foreign = work
            .candidate
            .entries
            .iter()
            .find(|entry| {
                matches!(
                    mez_agent::TranscriptContextEvent::from_transcript_content(&entry.content),
                    Some(mez_agent::TranscriptContextEvent::ExecutionBlock {
                        source: mez_agent::ContextSourceKind::ActionResult,
                        ..
                    })
                )
            })
            .unwrap()
            .clone();
        foreign.sequence = 1;
        match changed {
            "agent" => foreign.agent_id = "different-actor".into(),
            _ => foreign.pane_id = "%9".into(),
        }
        store.append_many(&[foreign]).unwrap();
        let history = work.execute();
        let result = fixture
            .service
            .complete_bookkeeping_candidate(work, history);
        assert!(
            result.is_err(),
            "foreign {changed} metadata was accepted as original result source"
        );
        assert!(!source.accepted());
        assert!(source.failure().is_some());
        assert!(
            fixture
                .service
                .prepare_native_filesystem_archive(&attempt)
                .is_err()
        );
        assert_eq!(
            fs::read(fixture.root.join("file")).unwrap(),
            b"native data\n"
        );
    }
}

/// Late confirmed facts after forced retirement still own the original explicit
/// no-save policy. Archive acceptance may acknowledge that policy while keeping
/// the group's first failure, raw outcome and continuation/retirement fence.
#[test]
fn native_filesystem_checked_archive_late_runtime_only_keeps_original_policy() {
    let mut fixture = Fixture::new();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    let first = fixture
        .service
        .prepare_completion_forced_retirement(&fixture.turn)
        .unwrap_err();
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    let accepted = fixture
        .service
        .prepare_native_filesystem_archive(&attempt)
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .service
            .native_filesystem_archive_acceptance_is_current(&accepted)
    );
    assert_eq!(
        fixture
            .service
            .ensure_completion_required_gates_ready(&fixture.turn.turn_id)
            .unwrap_err()
            .message(),
        first.message()
    );
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
    assert!(fixture.service.claim_bookkeeping_candidates().is_empty());
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
}

/// A newly retained generation of identical source still needs its own actual
/// checked read, even when all rows have an existing logical append owner. The
/// empty-delta path accepts that exact receipt without adding duplicate writes.
#[test]
fn native_filesystem_checked_archive_empty_delta_still_checks_exact_receipt() {
    let mut fixture = saved_fixture();
    let attempt = project(&mut fixture);
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(&attempt)
            .unwrap()
            .is_none()
    );
    let work = fixture
        .service
        .claim_bookkeeping_candidates()
        .pop()
        .unwrap();
    let candidate = work.candidate.clone();
    let original = candidate.checked_source.clone().unwrap();
    let history = work.execute();
    assert!(
        fixture
            .service
            .complete_bookkeeping_candidate(work, history)
            .unwrap()
    );
    let accepted = fixture
        .service
        .persistence
        .retain_completion_bookkeeping_candidate(candidate)
        .unwrap();
    let source = accepted.witness().checked_source().clone();
    assert_ne!(source, original);
    assert!(!source.accepted());
    let work = fixture
        .service
        .claim_bookkeeping_candidates()
        .pop()
        .unwrap();
    let history = work.execute();
    assert!(!source.accepted());
    assert!(
        fixture
            .service
            .complete_bookkeeping_candidate(work, history)
            .unwrap()
    );
    assert!(source.accepted());
    assert!(original.accepted());
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(&attempt)
            .unwrap()
            .is_some()
    );
}

/// An older candidate may already own forced-retirement source when real late
/// descriptor facts arrive. Checking that older generation cannot attest the
/// newer rows; the ordinary ordered candidate lane must check the fresh exact
/// snapshot, without replacing first failure or replaying its file effect.
#[test]
fn native_filesystem_checked_archive_late_saved_source_requires_fresh_candidate() {
    let mut fixture = saved_fixture();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    let first = fixture
        .service
        .prepare_completion_forced_retirement(&fixture.turn)
        .unwrap_err();
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(&attempt)
            .unwrap()
            .is_none()
    );
    let old = fixture
        .service
        .claim_bookkeeping_candidates()
        .pop()
        .unwrap();
    let old_source = old.candidate.checked_source.clone().unwrap();
    assert!(!old.candidate.entries.iter().any(|row| {
        row.content
            .contains("Native patch completed; 1 confirmed file endpoint(s).")
    }));
    let history = old.execute();
    assert!(
        fixture
            .service
            .complete_bookkeeping_candidate(old, history)
            .unwrap()
    );
    assert!(old_source.accepted());
    assert!(
        fixture
            .service
            .prepare_native_filesystem_archive(&attempt)
            .unwrap()
            .is_none()
    );
    let fresh = fixture
        .service
        .claim_bookkeeping_candidates()
        .pop()
        .unwrap();
    let fresh_source = fresh.candidate.checked_source.clone().unwrap();
    assert_ne!(fresh_source, old_source);
    assert!(fresh.candidate.entries.iter().any(|row| {
        row.content
            .contains("Native patch completed; 1 confirmed file endpoint(s).")
    }));
    let history = fresh.execute();
    assert!(
        fixture
            .service
            .complete_bookkeeping_candidate(fresh, history)
            .unwrap()
    );
    let accepted = fixture
        .service
        .prepare_native_filesystem_archive(&attempt)
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .service
            .native_filesystem_archive_acceptance_is_current(&accepted)
    );
    assert_eq!(
        fixture
            .service
            .ensure_completion_required_gates_ready(&fixture.turn.turn_id)
            .unwrap_err()
            .message(),
        first.message()
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
