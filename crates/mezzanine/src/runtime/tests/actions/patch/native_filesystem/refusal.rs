//! Positive original no-dispatch proof and required canonical settlement.
//!
//! Actual queued canonical slots qualify refusal without a worker result. Wrong
//! producers/actors and issued work cannot mint proof. Canonical retries, failed
//! original context and lost transfer replies preserve immutable source custody.

use super::async_producer::ActorFixture;
use super::*;
use crate::runtime::native_filesystem::NativeFilesystemProducer;

/// Open admission, equal-label replacement producers and foreign actors cannot
/// certify a refusal. The exact closed source can, without a native join; a
/// separately issued source remains exclusively on the joined-outcome path.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_refusal_requires_positive_original_unissued_custody() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let producer = attempt.root_producer().unwrap();
    assert!(
        fixture
            .service
            .take_unissued_native_filesystem_owner(&attempt)
            .is_err()
    );
    assert!(
        fixture
            .service
            .refuse_native_filesystem_producer(&producer)
            .is_err()
    );
    let alias = NativeFilesystemProducer::new(attempt.clone());
    alias.close().unwrap();
    assert!(
        fixture
            .service
            .refuse_native_filesystem_producer(&alias)
            .is_err()
    );
    producer.close().unwrap();
    let mut foreign = Fixture::new();
    assert!(
        foreign
            .service
            .refuse_native_filesystem_producer(&producer)
            .is_err()
    );
    let proof = fixture
        .service
        .refuse_native_filesystem_producer(&producer)
        .unwrap()
        .unwrap();
    assert!(proof.cancelled());
    assert!(proof.belongs_to(&producer));
    assert!(!proof.belongs_to(&alias));
    assert!(producer.joined_outcome().is_none());
    assert!(producer.observe().await.unwrap().is_none());
    assert!(
        fixture
            .service
            .take_unissued_native_filesystem_owner(&attempt)
            .is_err()
    );
    let mut issued = Fixture::new();
    let producer = issued.queue().root_producer().unwrap();
    assert!(
        issued
            .service
            .claim_native_filesystem_producer(&producer)
            .unwrap()
    );
    producer.close().unwrap();
    assert!(
        issued
            .service
            .refuse_native_filesystem_producer(&producer)
            .is_err()
    );
    assert!(producer.retire().await.unwrap().is_some());
    assert!(producer.refusal().unwrap().is_none());
    assert!(!fixture.root.join("file").exists());
    assert!(!issued.root.join("file").exists());
}

/// A real actor-recorded non-executable guard rejection retains its original
/// failure instead of masquerading as cancellation. Repeated proof/projection
/// calls preserve one canonical result and chronology entry without dispatch.
#[test]
fn native_filesystem_refusal_keeps_rejection_and_canonical_facts_once() {
    let mut fixture = Fixture::new();
    super::hooks::install_guard(&mut fixture, true, false);
    let attempt = fixture.queue();
    let producer = attempt.root_producer().unwrap();
    let cause = fixture
        .service
        .claim_native_filesystem_producer(&producer)
        .unwrap_err();
    producer.close().unwrap();
    let proof = fixture
        .service
        .refuse_native_filesystem_producer(&producer)
        .unwrap()
        .unwrap();
    assert!(!proof.cancelled());
    assert_eq!(proof.cause().kind(), cause.kind());
    assert_eq!(proof.cause().message(), cause.message());
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert!(
        fixture
            .service
            .refuse_native_filesystem_producer(&producer)
            .unwrap()
            .unwrap()
            .same(&proof)
    );
    assert!(
        !fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    let result = &fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0];
    assert_eq!(result.status, mez_agent::ActionStatus::Failed);
    let metadata: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_ref().unwrap()).unwrap();
    assert_eq!(metadata["dispatch_state"], "not_dispatched");
    assert_eq!(metadata["effect_certainty"], "unexecuted");
    assert!(
        metadata["confirmed_endpoints"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for fabricated in ["stdout", "stderr", "exit_code", "signal", "shell_command"] {
        assert!(metadata.get(fabricated).is_none());
    }
    assert_eq!(super::canonical::result_events(&fixture), 1);
    assert!(producer.joined_outcome().is_none());
    assert!(
        fixture
            .service
            .drain_program_hook_transition()
            .side_effects
            .is_empty()
    );
    assert!(!fixture.root.join("file").exists());
}

/// A missing original context is a required settlement failure, never a native
/// execution failure. Restoring context cannot re-enter that boundary; both the
/// group failure and exact refusal proof survive with no accepted transfer.
#[test]
fn native_filesystem_refusal_context_failure_retains_first_cause_and_source() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let producer = attempt.root_producer().unwrap();
    producer.close().unwrap();
    let proof = fixture
        .service
        .refuse_native_filesystem_producer(&producer)
        .unwrap()
        .unwrap();
    let context = fixture
        .service
        .agent_turn_contexts_mut()
        .remove(&fixture.turn.turn_id)
        .unwrap();
    let first = fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap_err();
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
            .completion_required_failures_for_tests(&fixture.turn.turn_id)[0]
            .1
            .message(),
        first.message()
    );
    assert!(
        fixture
            .service
            .refuse_native_filesystem_producer(&producer)
            .is_err()
    );
    assert!(producer.refusal().unwrap().unwrap().same(&proof));
    assert!(producer.refusal_acknowledgement().unwrap().is_none());
    assert!(producer.joined_outcome().is_none());
    assert_eq!(super::canonical::result_events(&fixture), 0);
    assert_eq!(
        fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0].status,
        mez_agent::ActionStatus::Cancelled
    );
}

/// Historical refusal acknowledgement is stored before the actor replies. Lost
/// receivers retry the same exact storage/proof, while aliases and foreign actors
/// remain rejected. No worker observation or native joined transfer is invented.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_refusal_lost_reply_keeps_distinct_historical_acceptance() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let producer = attempt.root_producer().unwrap();
    producer.close().unwrap();
    fixture
        .service
        .refuse_native_filesystem_producer(&producer)
        .unwrap()
        .unwrap();
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    let source = fixture
        .service
        .prepare_native_filesystem_archive(&attempt)
        .unwrap()
        .unwrap();
    let storage = fixture
        .service
        .observe_native_filesystem_archive_storage(&source)
        .unwrap()
        .unwrap();
    let actor = ActorFixture::new(fixture);
    let mut lost = Box::pin(
        actor
            .handle
            .transfer_refused_native_filesystem_producer(producer.clone(), storage.clone()),
    );
    assert!(futures_util::poll!(lost.as_mut()).is_pending());
    drop(lost);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while producer.refusal_acknowledgement().unwrap().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let ack = actor
        .handle
        .transfer_refused_native_filesystem_producer(producer.clone(), storage.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(ack.acknowledges(&producer));
    let alias = NativeFilesystemProducer::new(attempt.clone());
    assert!(!ack.acknowledges(&alias));
    assert!(
        actor
            .handle
            .transfer_refused_native_filesystem_producer(alias, storage.clone())
            .await
            .is_err()
    );
    assert!(
        actor
            .handle
            .transfer_native_filesystem_producer(producer.clone(), storage.clone())
            .await
            .is_err()
    );
    let foreign = ActorFixture::new(Fixture::new());
    assert!(
        foreign
            .handle
            .transfer_refused_native_filesystem_producer(producer.clone(), storage)
            .await
            .is_err()
    );
    drop(foreign.finish().await);
    let fixture = actor.finish().await;
    assert!(
        fixture
            .service
            .retained_native_filesystem_producer(&attempt)
            .is_none()
    );
    assert!(producer.joined_outcome().is_none());
    assert!(producer.transfer_acknowledgement().unwrap().is_none());
    assert_eq!(super::canonical::result_events(&fixture), 1);
}

/// Forced retirement moves original backing out of the current execution map.
/// A later refusal must update that retained source and leave a replacement
/// projection intact, with no native join and no renewal of failed group gates.
#[test]
fn native_filesystem_refusal_after_retirement_preserves_original_backing() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let producer = attempt.root_producer().unwrap();
    producer.close().unwrap();
    fixture
        .service
        .refuse_native_filesystem_producer(&producer)
        .unwrap()
        .unwrap();
    let failure = fixture
        .service
        .prepare_completion_forced_retirement(&fixture.turn)
        .unwrap_err();
    let mut replacement = fixture.execution.clone();
    replacement.response.raw_text = "replacement remains untouched".into();
    fixture
        .service
        .agent_turn_executions_mut()
        .insert(fixture.turn.turn_id.clone(), replacement.clone());
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert_eq!(
        fixture.service.agent_turn_executions()[&fixture.turn.turn_id],
        replacement
    );
    assert_eq!(
        fixture
            .service
            .ensure_completion_required_gates_ready(&fixture.turn.turn_id)
            .unwrap_err()
            .message(),
        failure.message()
    );
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_publication_held"
    );
    assert!(producer.joined_outcome().is_none());
    assert!(producer.refusal_acknowledgement().unwrap().is_none());
    assert!(!fixture.root.join("file").exists());
}
