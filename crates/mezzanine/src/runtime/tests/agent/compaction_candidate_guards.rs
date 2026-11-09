//! Manual candidate admission preserves original custody and exact source.
//! These controls exercise real command/source workers and storage receipts;
//! matching pane counters or preview success cannot authorize a foreign write.

use super::compaction_candidate::CanonicalManualFixture;
use super::*;
use crate::MezErrorKind;
use mez_agent::AgentContext;

#[test]
fn runtime_manual_compaction_candidate_rejects_foreign_source() {
    for headless in [false, true] {
        for worker in [false, true] {
            let fixture = || {
                if headless {
                    CanonicalManualFixture::headless(true)
                } else {
                    CanonicalManualFixture::new(None, 90, true)
                }
            };
            let mut original = fixture();
            let task = original.queue(worker);
            let mut foreign = fixture();
            if headless {
                // Match the installed store too: only the actual frozen
                // NoStore capture distinguishes these otherwise equal binds.
                foreign
                    .service
                    .set_agent_transcript_store(original.store.clone());
            }
            let other = foreign.queue(worker);
            assert_eq!(task.conversation_id, other.conversation_id);
            assert_eq!(task.task_generation, other.task_generation);
            assert_eq!(task.model_profile, other.model_profile);
            let before = original
                .service
                .manual_compaction_candidate_context(&task, "original preview")
                .unwrap();
            for both_owners in [false, true] {
                let mut substituted = task.clone();
                substituted.replay_source = other.replay_source.clone();
                if both_owners {
                    substituted.source_owner = other.source_owner.clone();
                }
                let error = original
                    .service
                    .manual_compaction_candidate_context(&substituted, "foreign preview")
                    .unwrap_err();
                assert_eq!(error.kind(), MezErrorKind::Conflict);
            }
            let after = original
                .service
                .manual_compaction_candidate_context(&task, "original preview")
                .unwrap();
            assert_eq!(before.blocks(), after.blocks());
            assert!(
                original
                    .service
                    .take_replay_epoch_attempt_for_tests()
                    .is_none()
            );
            assert!(original.service.agent_turn_ledger().turns().is_empty());
            assert!(original.service.pending_agent_provider_tasks().is_empty());
            original.publish("original accepted head");
        }
    }
}

/// A protected leading reference survives while later closed facts compact.
/// Exercise inline and prepared commands through real NoStore publication;
/// neither candidate preview nor acceptance may alter the retained reference.
#[test]
fn runtime_manual_compaction_candidate_preserves_leading_exact_reference() {
    for (worker, repeats) in [(false, 1), (true, 1), (false, 500), (true, 500)] {
        let mut fixture = CanonicalManualFixture::headless(false);
        let conversation = "leading-exact-reference";
        let id = mez_agent::ReplayConversationId::new(conversation).unwrap();
        let mut body = mez_agent::ReplayEpochContext::new(id.clone(), AgentContext::empty())
            .unwrap()
            .into_owned_context();
        body.append_reference_event(
            ContextSourceKind::PersistedContextDocument,
            "exact leading reference",
            "preserve the original reference",
        )
        .unwrap();
        let mut owner = mez_agent::ReplayEpochContext::from_owned_context(id, body).unwrap();
        owner
            .append_fact("later fact", "later source ".repeat(repeats))
            .unwrap();
        let original = owner.context().chronology().to_vec();
        fixture
            .service
            .agent_shell_store_mut()
            .bind_ephemeral_conversation_with_lineage("%1", conversation, 0, None)
            .unwrap();
        fixture
            .service
            .retain_ephemeral_replay_body(
                "%1",
                conversation,
                &owner.into_owned_context(),
                None,
                Some((
                    crate::storage::transcript::replay_epoch::ReplaySourceOrigin::new(
                        "original-reference-source",
                        "agent-%1",
                        "%1",
                    )
                    .unwrap(),
                    0,
                )),
                None,
            )
            .unwrap();
        let task = fixture.queue(worker);
        let candidate = fixture
            .service
            .manual_compaction_candidate_context(&task, "closed fact summary")
            .unwrap();
        // Candidate sizing includes the next request prompt, not just history.
        assert_eq!(candidate.chronology().len(), 3);
        assert_eq!(candidate.chronology()[1], original[0]);
        assert!(!candidate.chronology().contains(&original[1]));
        assert_eq!(
            candidate.chronology()[2].block().content,
            "Continue after conversation compaction."
        );
        assert_eq!(
            candidate.chronology()[0].block().content,
            "closed fact summary"
        );
        let unchanged = fixture
            .service
            .runtime_agent_history_epoch_context("%1")
            .unwrap()
            .replay_owner()
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.context().chronology(), original);
        assert!(fixture.service.pending_agent_provider_tasks().is_empty());
        fixture.publish("closed fact summary");
        assert!(fixture.service.pending_agent_compaction_tasks().is_empty());
        assert!(fixture.service.pending_agent_provider_tasks().is_empty());
        assert!(
            fixture
                .service
                .take_replay_epoch_attempt_for_tests()
                .is_none()
        );
        assert!(fixture.service.agent_turn_ledger().turns().is_empty());
        let context = fixture
            .service
            .agent_context_for_pane_prompt("%1", "Continue after conversation compaction.", 100)
            .unwrap();
        assert_eq!(context.chronology()[1], original[0]);
        assert_eq!(context.blocks(), candidate.blocks());
        assert!(
            fixture
                .store
                .load_replay_epoch(conversation, AgentContext::empty())
                .unwrap()
                .is_none()
        );
    }
}

/// Preview remains pure while the actual worker owns expected-parent CAS.
/// Another legitimate publisher wins after preparation; the stale command
/// must settle without overwriting that head or starting ordinary execution.
#[test]
fn runtime_manual_compaction_candidate_cannot_overwrite_newer_head() {
    for worker in [false, true] {
        let mut fixture = CanonicalManualFixture::new(None, 90, true);
        let task = fixture.queue(worker);
        let (parent, source) = fixture
            .store
            .load_replay_epoch(&fixture.conversation, AgentContext::empty())
            .unwrap()
            .unwrap();
        let plan = match source
            .prepare_compaction(mez_agent::ReplayTailPolicy::new(128_000, 90).unwrap())
            .unwrap()
        {
            mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
            other => panic!("expected original eligible source: {other:?}"),
        };
        let proposal = fixture
            .store
            .prepare_replay_epoch(
                crate::storage::transcript::replay_epoch::ReplaySourceOrigin::new(
                    "other-compactor",
                    "other-agent",
                    "%other",
                )
                .unwrap()
                .capture(None),
                &fixture.conversation,
                Some(parent),
                &source,
                plan,
                mez_agent::ReplayHeadPublication::Summary("newer authoritative head".into()),
            )
            .unwrap();
        fixture.store.publish_replay_epoch(&proposal).unwrap();
        let (newer, _) = fixture
            .store
            .load_replay_epoch(&fixture.conversation, AgentContext::empty())
            .unwrap()
            .unwrap();
        // The actor preview does not reread storage or claim publication.
        fixture
            .service
            .manual_compaction_candidate_context(&task, "stale preview")
            .unwrap();
        complete_runtime_test_compaction(&mut fixture.service, "%1", "stale summary");
        let attempt = fixture
            .service
            .take_replay_epoch_attempt_for_tests()
            .unwrap();
        let completion = attempt.execute();
        assert!(
            completion.error().is_some(),
            "expected actual stale-parent refusal"
        );
        assert!(
            fixture
                .service
                .apply_replay_epoch_completion(completion)
                .unwrap()
                .applied
        );
        assert!(!fixture.service.agent_is_compacting("%1"));
        assert!(fixture.service.pending_agent_compaction_tasks().is_empty());
        assert!(fixture.service.pending_agent_provider_tasks().is_empty());
        assert!(
            fixture
                .service
                .take_replay_epoch_attempt_for_tests()
                .is_none()
        );
        assert!(fixture.service.agent_turn_ledger().turns().is_empty());
        let (after, current) = fixture
            .store
            .load_replay_epoch(&fixture.conversation, AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(after, newer);
        assert_eq!(
            current.context().chronology()[0].block().content,
            "newer authoritative head"
        );
    }
}
