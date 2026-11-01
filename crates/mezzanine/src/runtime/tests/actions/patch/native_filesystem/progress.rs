//! Actual producer confirmations through actor ingress and final deduplication.
//!
//! Real descriptor writes and actor leases supply receipts; held installs model
//! lost optional notifications without replaying filesystem or canonical facts.

use super::async_producer::ActorFixture;
use super::canonical::result_events;
use super::*;
use crate::host::async_runtime::native_filesystem::{
    claim_and_start_native_filesystem_producer, retain_joined_native_filesystem_outcome,
};
use crate::runtime::agent::native_filesystem::NativeFilesystemAttempt;
use crate::runtime::processes::native_filesystem_producer::NativeFilesystemProducer;

/// Runs and joins the actual registered producer; final canonical projection is
/// deliberately left pending so each test controls the display boundary.
async fn run(fixture: Fixture) -> (Fixture, NativeFilesystemAttempt, NativeFilesystemProducer) {
    run_with_certainty(fixture, NativeEffectCertainty::Confirmed).await
}

/// Allows genuine partial execution without changing the actor/worker pipeline.
async fn run_with_certainty(
    mut fixture: Fixture,
    certainty: NativeEffectCertainty,
) -> (Fixture, NativeFilesystemAttempt, NativeFilesystemProducer) {
    let attempt = fixture.queue();
    let producer = NativeFilesystemProducer::new(attempt.clone());
    let actor = ActorFixture::new(fixture);
    assert!(
        claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer)
            .await
            .unwrap()
    );
    let raw = retain_joined_native_filesystem_outcome(&actor.handle, &producer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(raw.result.certainty, certainty);
    (actor.finish().await, attempt, producer)
}

/// Reads original installed final RESULT sources, separately from confirmed
/// progress rows, so duplicate final diffs cannot hide behind screen wrapping.
fn final_results(fixture: &Fixture) -> Vec<String> {
    fixture
        .service
        .presentation
        .agent_published_provider_results
        .iter()
        .filter(|((_, turn, _, _, _), _)| turn == &fixture.turn.turn_id)
        .map(|(_, source)| source.clone())
        .collect()
}

/// Actual post-commit ingress promotes once before final canonical projection.
/// Final publication keeps its native outcome and full counts but omits the
/// already acknowledged diff; a late duplicate cannot recreate live progress.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_progress_live_and_final_do_not_duplicate_diff() {
    let (mut fixture, attempt, producer) = run(Fixture::new()).await;
    let delivery = producer.progress_snapshot().unwrap();
    let confirmed = delivery.confirmed().unwrap();
    assert_eq!(confirmed.len(), 1);
    assert!(
        fixture
            .service
            .confirmed_progress_source_is_promoted(&confirmed[0])
    );
    assert!(final_results(&fixture).is_empty());
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert!(final_results(&fixture).is_empty());
    assert_eq!(result_events(&fixture), 1);
    assert!(
        !fixture
            .service
            .apply_native_filesystem_progress(&delivery)
            .unwrap()
    );
    assert_eq!(fixture.service.retry_completion_publications(), 0);
    let path = fixture.root.join("file").to_str().unwrap().to_owned();
    assert_eq!(
        fixture.service.retained_agent_modified_files("%1").unwrap()[&path].added,
        1
    );
    assert_eq!(fs::read(&path).unwrap(), b"native data\n");
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}

/// Every optional live install is held while two real endpoints commit. Final
/// projection reconciles the retained cumulative prefix after recovery, rather
/// than relying on delivery of either earlier actor notification.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_progress_final_flush_recovers_uninstalled_prefix() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Add File: {}\n+one\n*** Add File: {}\n+two\n*** End Patch",
            root.join("one").display(),
            root.join("two").display()
        )
    });
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    let (mut fixture, attempt, producer) = run(fixture).await;
    let confirmed = producer.progress_snapshot().unwrap().confirmed().unwrap();
    assert_eq!(confirmed.len(), 2);
    assert!(
        confirmed
            .iter()
            .all(|p| !fixture.service.confirmed_progress_source_is_promoted(p))
    );
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert!(
        confirmed
            .iter()
            .all(|p| fixture.service.confirmed_progress_source_is_promoted(p))
    );
    assert!(final_results(&fixture).is_empty());
    assert_eq!(result_events(&fixture), 1);
    assert_eq!(fs::read(fixture.root.join("one")).unwrap(), b"one\n");
    assert_eq!(fs::read(fixture.root.join("two")).unwrap(), b"two\n");
}

/// A permanently held live/final install leaves immutable sources available.
/// Once installations recover, the final diff appears once and late delivery
/// cannot add an already terminal action's confirmation on top of that source.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_progress_held_final_retry_stays_exactly_once() {
    let mut fixture = Fixture::new();
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    let (mut fixture, attempt, producer) = run(fixture).await;
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_publication_held"
    );
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    assert!(fixture.service.retry_completion_publications() > 0);
    let installed = final_results(&fixture);
    assert_eq!(installed.len(), 1);
    assert!(
        !fixture
            .service
            .apply_native_filesystem_progress(&producer.progress_snapshot().unwrap())
            .unwrap()
    );
    assert_eq!(final_results(&fixture), installed);
    assert_eq!(result_events(&fixture), 1);
}

/// Actor authorization checks the actual producer allocation and exact retained
/// data. Equal attempt labels, changed bytes and revisions do not authorize
/// paint, while the original confirmations survive a replacement conversation.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_progress_rejects_forged_sources_and_stale_conversation() {
    let (mut fixture, attempt, producer) = run(Fixture::new()).await;
    let delivery = producer.progress_snapshot().unwrap();
    let mut forged = delivery.confirmed().unwrap()[0].clone();
    forged.revision += 1;
    assert!(
        !fixture
            .service
            .native_filesystem_progress_is_registered(&forged)
    );
    forged.revision = 1;
    forged.source.push_str("forged");
    assert!(
        !fixture
            .service
            .native_filesystem_progress_is_registered(&forged)
    );
    let wrong = NativeFilesystemProducer::new(attempt.clone());
    assert!(
        !fixture
            .service
            .apply_native_filesystem_progress(&wrong.progress_snapshot().unwrap())
            .unwrap()
    );
    fixture
        .service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .session_id = "replacement".into();
    let screen = fixture.service.agent_pane_screen("%1").unwrap().clone();
    assert!(
        !fixture
            .service
            .apply_native_filesystem_progress(&delivery)
            .unwrap()
    );
    assert_eq!(fixture.service.agent_pane_screen("%1").unwrap(), &screen);
    assert_eq!(
        producer
            .progress_snapshot()
            .unwrap()
            .confirmed()
            .unwrap()
            .len(),
        1
    );
}

/// Live display stops at the shared 64-component limit; a real 65th committed
/// endpoint remains in the bounded confirmation journal, full original outcome
/// and final remaining diff. The display boundary never truncates effect truth.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_progress_capacity_keeps_final_overflow_endpoint() {
    let fixture = Fixture::with_patch(false, |root| {
        let mut patch = "*** Begin Patch\n".to_owned();
        for index in 0..65 {
            patch.push_str(&format!(
                "*** Add File: {}\n+entry-{index}\n",
                root.join(format!("f{index}")).display()
            ));
        }
        patch.push_str("*** End Patch");
        patch
    });
    let (mut fixture, attempt, producer) = run(fixture).await;
    assert_eq!(
        producer
            .progress_snapshot()
            .unwrap()
            .confirmed()
            .unwrap()
            .len(),
        64
    );
    let raw = fixture
        .service
        .retained_native_filesystem_outcome(&attempt)
        .unwrap();
    assert_eq!(raw.receipts.len(), 65);
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    let sources = final_results(&fixture);
    assert_eq!(sources.len(), 1);
    assert!(sources[0].contains("+entry-64"));
    assert!(!sources[0].contains("+entry-0\n"));
    assert_eq!(result_events(&fixture), 1);
    assert_eq!(fs::read(fixture.root.join("f64")).unwrap(), b"entry-64\n");
}

/// An actual retained endpoint cannot be revised, rebound to an unauthored
/// path/kind or recorded under another operation. Exact duplicate callbacks
/// reuse the original allocation rather than accumulating new components.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_progress_receipt_ledger_rejects_changed_evidence() {
    let (fixture, attempt, producer) = run(Fixture::new()).await;
    let raw = fixture
        .service
        .retained_native_filesystem_outcome(&attempt)
        .unwrap();
    let receipt = &raw.receipts[0];
    assert_eq!(
        producer
            .record_confirmed_progress(receipt)
            .unwrap()
            .unwrap()
            .confirmed()
            .unwrap()
            .len(),
        1
    );
    let mut changed = receipt.clone();
    changed.diff.push_str("changed");
    assert!(producer.record_confirmed_progress(&changed).is_err());
    changed = receipt.clone();
    changed.path.push_str("unapproved");
    assert!(producer.record_confirmed_progress(&changed).is_err());
    changed = receipt.clone();
    changed.operation_ordinal = 1;
    assert!(producer.record_confirmed_progress(&changed).is_err());
    producer.close().unwrap();
    assert!(
        !producer.matches_progress(&producer.progress_snapshot().unwrap().confirmed().unwrap()[0])
    );
    assert_eq!(
        producer
            .progress_snapshot()
            .unwrap()
            .confirmed()
            .unwrap()
            .len(),
        1
    );
}

/// A first real create remains confirmed when a later create conflicts. Live
/// promotion and the failed final result agree on the positive prefix without
/// inventing the second effect, rolling back the first, or duplicating its diff.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_progress_partial_failure_keeps_positive_prefix() {
    let fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Add File: {}\n+committed\n*** Add File: {}\n+forbidden overwrite\n*** End Patch",
            root.join("first").display(),
            root.join("second").display()
        )
    });
    fs::write(fixture.root.join("second"), b"original\n").unwrap();
    let (mut fixture, attempt, producer) =
        run_with_certainty(fixture, NativeEffectCertainty::Partial).await;
    let confirmed = producer.progress_snapshot().unwrap().confirmed().unwrap();
    assert_eq!(confirmed.len(), 1);
    assert!(
        fixture
            .service
            .confirmed_progress_source_is_promoted(&confirmed[0])
    );
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert!(final_results(&fixture).is_empty());
    assert_eq!(
        fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0].status,
        mez_agent::ActionStatus::Failed
    );
    assert_eq!(
        fs::read(fixture.root.join("first")).unwrap(),
        b"committed\n"
    );
    assert_eq!(
        fs::read(fixture.root.join("second")).unwrap(),
        b"original\n"
    );
    assert_eq!(result_events(&fixture), 1);
}

/// Live confirmations cannot cross an earlier still-running ordinal. They stay
/// in the actual producer and complete canonical facts without painting until
/// the predecessor's ordered result is accepted by the existing publisher.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_progress_waits_for_predecessor_before_paint() {
    let fixture = Fixture::with_actions(false, |root| {
        vec![
            mez_agent::AgentAction {
                id: "prefix".into(),
                payload: mez_agent::AgentActionPayload::ListAgents {
                    agent_type: None,
                    scope: None,
                },
            },
            mez_agent::AgentAction {
                id: "native-file".into(),
                payload: mez_agent::AgentActionPayload::ApplyPatch {
                    patch: format!(
                        "*** Begin Patch\n*** Add File: {}\n+after-prefix\n*** End Patch",
                        root.join("file").display()
                    ),
                    strip: None,
                },
            },
        ]
    });
    let (mut fixture, attempt, producer) = run(fixture).await;
    let progress = producer.progress_snapshot().unwrap().confirmed().unwrap();
    assert_eq!(progress.len(), 1);
    assert!(
        !fixture
            .service
            .confirmed_progress_source_is_promoted(&progress[0])
    );
    assert!(
        fixture
            .service
            .action_presentation_progress_entries_for_tests("%1")
            .is_empty()
    );
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert!(final_results(&fixture).is_empty());
    let mut execution = fixture.service.agent_turn_executions()[&fixture.turn.turn_id].clone();
    let prior = execution.response.action_batch.as_ref().unwrap().actions[0].clone();
    execution.action_results[0] = mez_agent::ActionResult::succeeded(
        &fixture.turn,
        &prior,
        vec!["PREFIX RESULT".into()],
        None,
    );
    fixture
        .service
        .agent_turn_executions_mut()
        .insert(fixture.turn.turn_id.clone(), execution.clone());
    fixture
        .service
        .queue_ordered_provider_header("%1", &execution, &prior)
        .unwrap();
    fixture
        .service
        .queue_ordered_provider_result("%1", &execution, &prior, &execution.action_results[0])
        .unwrap();
    assert!(fixture.service.retry_completion_publications() > 0);
    let text = fixture
        .service
        .agent_pane_screen("%1")
        .unwrap()
        .visible_lines()
        .join("\n");
    assert!(text.find("PREFIX RESULT").unwrap() < text.find("+after-prefix").unwrap());
    assert_eq!(result_events(&fixture), 1);
}

/// A confirmed transient exists before promotion is held. Final canonical facts
/// still commit, but final publication waits instead of rendering the same diff
/// twice. Source-only retry promotes the exact retained component after repair.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_progress_final_waits_for_retained_transient_promotion() {
    let mut fixture = Fixture::new();
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    let (mut fixture, attempt, producer) = run(fixture).await;
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    fixture
        .service
        .queue_ordered_provider_header("%1", &fixture.execution, &fixture.action)
        .unwrap();
    fixture
        .service
        .flush_ordered_provider_headers("%1", &fixture.execution)
        .unwrap();
    let progress = producer.progress_snapshot().unwrap().confirmed().unwrap()[0].clone();
    assert!(
        fixture
            .service
            .apply_action_presentation_progress(progress.clone())
            .unwrap()
    );
    assert!(
        fixture
            .service
            .confirmed_progress_source_is_retained(&progress)
    );
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert_eq!(result_events(&fixture), 1);
    assert!(final_results(&fixture).is_empty());
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_publication_held"
    );
    assert!(
        !fixture
            .service
            .confirmed_progress_source_is_promoted(&progress)
    );
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    assert!(fixture.service.retry_completion_publications() > 0);
    assert!(
        fixture
            .service
            .confirmed_progress_source_is_promoted(&progress)
    );
    assert!(final_results(&fixture).is_empty());
    assert_eq!(result_events(&fixture), 1);
    assert_eq!(fixture.service.retry_completion_publications(), 0);
}
