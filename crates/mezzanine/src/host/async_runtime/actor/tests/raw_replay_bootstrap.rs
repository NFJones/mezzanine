//! A new raw conversation acquires canonical ownership before provider actions.
//! Synthetic source uses the real prompt, actor claim and publication boundaries.

use super::super::*;
use super::replay_epoch_publication::setup;

fn fresh(
    ephemeral: bool,
) -> (
    crate::test_support::replay_epoch::Fixture,
    crate::runtime::RuntimeSessionService,
    String,
    String,
    mez_core::ids::ClientId,
) {
    fresh_task(ephemeral, true, "retain exact first user intent")
}

/// Samples only explicitly requested environment input; no canonical member is
/// injected or reclassified to establish compaction eligibility.
fn fresh_task(
    ephemeral: bool,
    environment: bool,
    prompt: &str,
) -> (
    crate::test_support::replay_epoch::Fixture,
    crate::runtime::RuntimeSessionService,
    String,
    String,
    mez_core::ids::ClientId,
) {
    let (fixture, mut service, primary) = setup();
    let conversation = String::from(mez_agent::ContextBlockOccurrenceId::allocate());
    if ephemeral {
        service
            .agent_shell_store_mut()
            .bind_ephemeral_conversation_with_lineage("%1", &conversation, 0, None)
            .unwrap();
    } else {
        service
            .agent_shell_store_mut()
            .bind_conversation("%1", &conversation, 0)
            .unwrap();
    }
    // This is a sampled environment input, not a canonical-context insertion.
    // Its normal prompt projection remains an exact pre-user source barrier.
    if environment {
        service.set_pane_environment_signature_for_tests(
            "%1",
            mez_agent::EnvironmentSignature::new(
                std::env::consts::OS,
                std::env::consts::ARCH,
                None,
                "fixture-host",
                "fixture-user",
                Some(fixture.root.to_string_lossy().into_owned()),
                "/bin/sh",
                mez_agent::ShellClassification::PosixSh,
                None,
                Some("/usr/bin:/bin".into()),
                fixture.root.to_string_lossy(),
                None,
                false,
                None,
                Vec::new(),
            )
            .unwrap(),
        );
    }
    let response = service
        .execute_agent_shell_command(&primary, prompt)
        .unwrap();
    let turn = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .clone()
        .unwrap_or_else(|| panic!("first prompt not admitted: {response:?}"));
    service.remove_pending_agent_provider_task(&turn);
    (fixture, service, conversation, turn, primary)
}

/// No test can bootstrap the owner by mutating the context after admission:
/// the actual first prompt must already carry it before any provider append.
#[tokio::test(flavor = "current_thread")]
async fn raw_replay_bootstrap_owns_first_prompt_before_provider_work() {
    let (fixture, service, conversation, turn, _) = fresh(false);
    assert_eq!(
        fixture.store.inspect(&conversation).unwrap_err().kind(),
        crate::MezErrorKind::NotFound
    );
    assert!(
        fixture
            .store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    let body = &service.agent_turn_contexts()[&turn];
    assert!(
        body.has_replay_owner(),
        "actual first prompt lacks canonical ownership"
    );
    let owner = mez_agent::ReplayEpochContext::from_owned_context(
        mez_agent::ReplayConversationId::new(&conversation).unwrap(),
        body.clone(),
    )
    .unwrap();
    assert_eq!(owner.revision().get(), 0);
    assert!(owner.head().is_none());
    let root = body
        .chronology()
        .iter()
        .find(|event| event.block().source == mez_agent::ContextSourceKind::UserInstruction)
        .unwrap();
    assert_eq!(root.retention(), mez_agent::ContextRetention::Exact);
    assert!(
        body.replay_task_admission_start(root.record().occurrence_id())
            .is_some()
    );
}

/// Exact first-source observations and the active instruction are barriers.
/// Initial ownership must not manufacture eligibility or queue a useless model.
#[tokio::test(flavor = "current_thread")]
async fn raw_replay_bootstrap_protected_first_source_queues_no_compactor() {
    for ephemeral in [false, true] {
        let (fixture, mut service, conversation, turn, _) = fresh(ephemeral);
        let before = service.agent_turn_contexts()[&turn].clone();
        assert!(before.has_replay_owner());
        let legacy =
            mez_agent::context_compaction::plan_model_context_compaction_at_consumed_sequence(
                &before,
                16000,
                1,
                before.event_sequence_high_water_mark(),
            )
            .unwrap();
        let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
        assert!(
            !service
                .queue_agent_active_turn_compaction(
                    &turn,
                    "default".into(),
                    profile,
                    crate::runtime::RuntimeActiveTurnCompactionTrigger::ProviderContextLimit {
                        attempt: 1
                    },
                    legacy
                )
                .unwrap()
        );
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.pending_agent_compaction_task_ids().is_empty());
        assert_eq!(service.agent_turn_contexts()[&turn], before);
        assert_eq!(
            service.agent_turn_ledger().turn(&turn).unwrap().state,
            mez_agent::AgentTurnState::Running
        );
        assert_eq!(
            service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .running_turn_id
                .as_deref(),
            Some(turn.as_str())
        );
        assert_eq!(
            fixture.store.inspect(&conversation).unwrap_err().kind(),
            crate::MezErrorKind::NotFound
        );
        assert!(
            fixture
                .store
                .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
                .unwrap()
                .is_none()
        );
    }
}

struct FinalProvider(std::sync::atomic::AtomicUsize);
impl crate::integrations::agent::provider::ModelProvider for FinalProvider {
    fn provider_id(&self) -> &str {
        "runtime-batch"
    }
    fn send_request(&self, _: &mez_agent::ModelRequest) -> crate::Result<mez_agent::ModelResponse> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "first task complete".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Vec::new(),
            provider_transcript_events: Vec::new(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "offline terminal fixture".into(),
                actions: vec![mez_agent::AgentAction {
                    id: "terminal-response".into(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: "first task complete".into(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
                    },
                }],
            }),
        })
    }
}

/// Saved raw history must reconstruct the same canonical task before a head
/// exists. A new runtime reads original accounted rows from a fresh store handle;
/// no frozen live body or synthetic completion witness crosses that boundary.
#[tokio::test(flavor = "current_thread")]
async fn raw_replay_bootstrap_saved_terminal_reopens_before_first_head() {
    let (fixture, mut service, conversation, turn, _) = fresh(false);
    let original = service.agent_turn_contexts()[&turn].chronology().to_vec();
    let root = original
        .iter()
        .find(|event| event.block().source == mez_agent::ContextSourceKind::UserInstruction)
        .unwrap();
    let task_start = service.agent_turn_contexts()[&turn]
        .replay_task_admission_start(root.record().occurrence_id())
        .unwrap();
    let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    let provider = FinalProvider(std::sync::atomic::AtomicUsize::new(0));
    service
        .execute_agent_turn_with_provider(&turn, &provider, profile)
        .unwrap();
    let entries = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .transcript_entries;
    assert!(!fixture.store.inspect(&conversation).unwrap().is_empty());
    assert!(
        fixture
            .store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );

    let (_other_fixture, mut reopened, primary) = setup();
    reopened.set_agent_transcript_store(crate::storage::transcript::AgentTranscriptStore::new(
        &fixture.root,
    ));
    reopened
        .agent_shell_store_mut()
        .bind_conversation("%1", &conversation, entries)
        .unwrap();
    reopened
        .agent_context_for_pane_prompt("%1", "preview after restart", 0)
        .unwrap();
    let admission = reopened
        .execute_agent_shell_command(&primary, "actual task after restart")
        .unwrap();
    let next = reopened
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .clone()
        .unwrap_or_else(|| panic!("cold raw admission failed: {admission:?}"));
    let body = &reopened.agent_turn_contexts()[&next];
    assert!(
        body.has_replay_owner(),
        "saved raw source lost canonical ownership before its first head"
    );
    let owner = mez_agent::ReplayEpochContext::from_owned_context(
        mez_agent::ReplayConversationId::new(&conversation).unwrap(),
        body.clone(),
    )
    .unwrap();
    assert_eq!(owner.revision().get(), 0);
    assert!(owner.head().is_none());
    for expected in original {
        let actual = body
            .chronology()
            .iter()
            .find(|event| event.record().occurrence_id() == expected.record().occurrence_id())
            .unwrap();
        assert_eq!(actual.block(), expected.block());
        assert_eq!(actual.record(), expected.record());
        assert_eq!(actual.sequence(), expected.sequence());
        if expected.block().source == mez_agent::ContextSourceKind::UserInstruction {
            assert_eq!(
                actual.retention(),
                mez_agent::ContextRetention::Summarizable
            );
            assert_eq!(
                body.replay_task_admission_start(actual.record().occurrence_id()),
                Some(task_start)
            );
        }
    }
    assert_eq!(provider.0.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// Final Say obligations really settle through the runtime before cleanup.
/// Saved replay must retain the same closed execution frontier as NoStore;
/// neither the final response text nor display rows authorize that transition.
async fn assert_completed_group_replay(ephemeral: bool, asynchronous: bool) {
    let (fixture, mut service, conversation, turn, _) = fresh(ephemeral);
    let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    let provider = FinalProvider(std::sync::atomic::AtomicUsize::new(0));
    let execution = if asynchronous {
        service
            .execute_agent_turn_with_provider_async(&turn, &provider, profile)
            .await
            .unwrap()
    } else {
        service
            .execute_agent_turn_with_provider(&turn, &provider, profile)
            .unwrap()
    };
    assert_eq!(
        execution.terminal_state,
        mez_agent::AgentTurnState::Completed
    );
    assert!(!service.agent_turn_contexts().contains_key(&turn));
    let context = if ephemeral {
        service
            .agent_context_for_pane_prompt("%1", "next task preview", 0)
            .unwrap()
    } else {
        let entries = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries;
        let (_other_fixture, mut reopened, _) = setup();
        reopened.set_agent_transcript_store(crate::storage::transcript::AgentTranscriptStore::new(
            &fixture.root,
        ));
        reopened
            .agent_shell_store_mut()
            .bind_conversation("%1", &conversation, entries)
            .unwrap();
        reopened
            .agent_context_for_pane_prompt("%1", "next task preview", 0)
            .unwrap()
    };
    let groups = context
        .chronology()
        .iter()
        .filter_map(|event| event.execution_group_id().cloned())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        !groups.is_empty(),
        "actual provider execution was not retained"
    );
    for group in groups {
        assert!(
            !context.execution_group_is_pending(&group),
            "completed execution protection was reopened during replay (NoStore={ephemeral})"
        );
    }
    assert_eq!(provider.0.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn raw_replay_bootstrap_completed_group_survives_saved_cold_replay() {
    for asynchronous in [false, true] {
        assert_completed_group_replay(false, asynchronous).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn raw_replay_bootstrap_completed_group_survives_no_store_replay() {
    for asynchronous in [false, true] {
        assert_completed_group_replay(true, asynchronous).await;
    }
}

/// An actual completed task supplies eligible raw source to its successor.
/// The normal request/claim/worker/adoption path must retain that old producer,
/// preserve exact active input, and reopen the first durable head without replay.
#[tokio::test(flavor = "current_thread")]
async fn raw_replay_bootstrap_first_saved_head_uses_actual_raw_producer() {
    first_saved_head_uses_actual_raw_producer(false).await;
}

/// Holds the original ordinary write claim while its actual successor publishes
/// a head. Receipt recovery may make those bytes durable, but neither publication
/// nor replay may consume the absent ordinary acknowledgment or regenerate rows.
#[tokio::test(flavor = "current_thread")]
async fn raw_replay_bootstrap_first_head_preserves_pending_original_append_owner() {
    first_saved_head_uses_actual_raw_producer(true).await;
}

async fn first_saved_head_uses_actual_raw_producer(pending_append: bool) {
    let first_prompt = format!("ORIGINAL_RAW_TASK {}", "original user intent ".repeat(4000));
    let (fixture, mut service, conversation, first, primary) =
        fresh_task(false, false, &first_prompt);
    let original_root = service.agent_turn_contexts()[&first].chronology()[0].clone();
    assert_eq!(
        original_root.block().source,
        mez_agent::ContextSourceKind::UserInstruction
    );
    assert_eq!(original_root.block().content, first_prompt);
    let profile = service.agent_turn_model_profile(&first).unwrap().clone();
    let provider = FinalProvider(std::sync::atomic::AtomicUsize::new(0));
    if pending_append {
        service.use_transcript_effect_adapter();
    }
    service
        .execute_agent_turn_with_provider(&first, &provider, profile)
        .unwrap();
    let audit = if pending_append {
        let mut candidates = service.claim_bookkeeping_candidates();
        assert_eq!(candidates.len(), 1);
        let original = candidates.pop().unwrap();
        let checked = original.execute();
        assert!(
            service
                .complete_bookkeeping_candidate(original, checked)
                .unwrap()
        );
        assert_eq!(
            fixture.store.inspect(&conversation).unwrap_err().kind(),
            crate::MezErrorKind::NotFound
        );
        Vec::new()
    } else {
        let audit = fixture.store.inspect(&conversation).unwrap();
        assert!(!audit.is_empty());
        audit
    };
    assert!(
        fixture
            .store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
    service
        .execute_agent_shell_command(&primary, "ACTIVE_RAW_TASK stays exact")
        .unwrap();
    let next = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .clone()
        .unwrap();
    service.remove_pending_agent_provider_task(&next);
    let context = &service.agent_turn_contexts()[&next];
    assert!(context.has_replay_owner());
    let old = context
        .chronology()
        .iter()
        .find(|event| event.record() == original_root.record())
        .unwrap();
    assert_eq!(old.retention(), mez_agent::ContextRetention::Summarizable);
    let active = context
        .chronology()
        .iter()
        .find(|event| event.block().content == "ACTIVE_RAW_TASK stays exact")
        .unwrap()
        .clone();
    let legacy = mez_agent::context_compaction::plan_model_context_compaction_at_consumed_sequence(
        context,
        16000,
        1,
        context.event_sequence_high_water_mark(),
    )
    .unwrap();
    let profile = service.agent_turn_model_profile(&next).unwrap().clone();
    assert!(
        service
            .queue_agent_active_turn_compaction(
                &next,
                "default".into(),
                profile,
                crate::runtime::RuntimeActiveTurnCompactionTrigger::ProviderContextLimit {
                    attempt: 1
                },
                legacy,
            )
            .unwrap()
    );
    let generation = service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    let (_handle, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut held_append = None;
    if pending_append {
        actor.start_transcript_receipt_admission(0, TranscriptReceiptReply::Startup);
        let completion = super::completion_source_handoff::next_completion(&mut actor).await;
        assert!(
            matches!(&completion, AsyncRuntimeRequest::CompleteTranscriptReceipts { results, .. }
            if !results.is_empty() && results.iter().all(|(_, result)| result.is_ok()))
        );
        Box::pin(actor.handle_request(completion)).await;
        let mut others = Vec::new();
        for (effect, claim) in actor.drain_persistence_claims(128).unwrap() {
            match effect {
                RuntimeSideEffect::PersistTranscriptEntries { store, path, batch } => {
                    let controls = batch.entries().iter().filter_map(|row|
                        mez_agent::transcript::TranscriptExecutionStateRecord::from_transcript_content(&row.content).unwrap()
                    ).collect::<Vec<_>>();
                    assert!(!controls.is_empty());
                    assert!(controls.iter().all(|control| !control.state().is_pending()));
                    assert!(
                        held_append
                            .replace((store, path, batch, claim.unwrap()))
                            .is_none()
                    );
                }
                effect => {
                    assert!(claim.is_none());
                    others.push(effect);
                }
            }
        }
        actor.admit_committed_side_effects(others).unwrap();
        assert!(
            held_append.is_some(),
            "original ordinary append lost its actor claim"
        );
    }
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    actor.admit_committed_side_effects(effects).unwrap();
    let offered = actor
        .drain_agent_provider_dispatch_side_effects(64)
        .unwrap();
    assert!(offered.iter().any(|effect| matches!(effect, RuntimeSideEffect::DispatchAgentCompaction { task_generation,.. } if *task_generation == generation)));
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::ClaimAgentCompactionTask {
            pane_id: "%1".into(),
            task_generation: generation,
            reply,
        })
        .await;
    let dispatch = response.await.unwrap().unwrap().unwrap();
    assert!(dispatch.task.replay_source.is_some());
    let input = &dispatch.task.request.messages.last().unwrap().content;
    assert!(input.contains("ORIGINAL_RAW_TASK"));
    assert!(!input.contains("ACTIVE_RAW_TASK"));
    let mut batch = RuntimeEventBatch::default();
    batch.push(RuntimeEvent::AgentCompaction(
        crate::runtime::AgentCompactionEvent::Completed {
            pane_id: "%1".into(),
            task_generation: generation,
            response: Box::new(mez_agent::ModelResponse {
                provider: "runtime-batch".into(),
                model: "test".into(),
                raw_text: "first raw task summarized".into(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            }),
        },
    ));
    actor.apply_runtime_event_batch(batch).await.unwrap();
    let mut attempt = None;
    let mut other = Vec::new();
    for (effect, claim) in actor.drain_persistence_claims(128).unwrap() {
        match effect {
            RuntimeSideEffect::PersistReplayEpoch { attempt: actual } => {
                assert!(claim.is_none());
                assert!(attempt.replace(actual).is_none());
            }
            effect => other.push(effect),
        }
    }
    actor.admit_committed_side_effects(other).unwrap();
    let attempt = attempt.expect("actual raw-source publication effect");
    let completion = tokio::task::spawn_blocking(move || attempt.execute())
        .await
        .unwrap();
    assert!(
        completion.error().is_none(),
        "raw source publication failed: {:?}",
        completion.error()
    );
    actor
        .apply_runtime_event(RuntimeEvent::Persistence(
            PersistenceEvent::ReplayEpochCompleted { completion },
        ))
        .await
        .unwrap();
    assert!(!actor.service.agent_is_compacting("%1"));
    let body = &actor.service.agent_turn_contexts()[&next];
    let owner = mez_agent::ReplayEpochContext::from_owned_context(
        mez_agent::ReplayConversationId::new(&conversation).unwrap(),
        body.clone(),
    )
    .unwrap();
    assert_eq!(owner.revision().get(), 1);
    assert_eq!(
        body.chronology()[0].block().content,
        "first raw task summarized"
    );
    assert!(
        !body
            .chronology()
            .iter()
            .any(|event| event.record().occurrence_id() == original_root.record().occurrence_id())
    );
    assert!(body.chronology().contains(&active));
    let (_, cold) = fixture
        .store
        .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(cold.context().chronology(), body.chronology());
    if let Some((store, path, original, claim_id)) = held_append {
        assert!(
            actor
                .side_effect_routes
                .owns_claimed_transcript_id(claim_id, &original, &path)
        );
        let expected_body = body.chronology().to_vec();
        let original_image = original.entries().to_vec();
        let worker_store = store.clone();
        let worker_batch = original.clone();
        let bytes = tokio::task::spawn_blocking(move || {
            worker_store.commit_accounted_transcript_batch(&worker_batch, claim_id)
        })
        .await
        .unwrap()
        .unwrap();
        actor
            .apply_runtime_event(RuntimeEvent::Persistence(
                PersistenceEvent::TranscriptClaim {
                    claim_id,
                    outcome: Box::new(PersistenceEvent::TranscriptCompleted {
                        conversation_id: conversation.clone(),
                        first_sequence: original.first_sequence(),
                        batch: original.clone(),
                        path: path.clone(),
                        bytes,
                    }),
                },
            ))
            .await
            .unwrap();
        assert!(
            !actor
                .side_effect_routes
                .owns_claimed_transcript_id(claim_id, &original, &path)
        );
        let after = actor
            .service
            .agent_context_for_pane_prompt(
                "%1",
                "actual preview after original append acknowledgment",
                0,
            )
            .unwrap();
        assert_eq!(after.chronology().len(), expected_body.len() + 1);
        for (actual, expected) in after.chronology().iter().zip(&expected_body) {
            assert_eq!(actual.block(), expected.block());
            assert_eq!(actual.record(), expected.record());
            assert_eq!(actual.sequence(), expected.sequence());
            assert_eq!(actual.execution_group_id(), expected.execution_group_id());
        }
        assert_eq!(
            fixture.store.inspect(&conversation).unwrap(),
            original_image
        );
    } else {
        assert_eq!(fixture.store.inspect(&conversation).unwrap(), audit);
    }
    assert_eq!(provider.0.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// Manual compaction shares canonical source/publication with active turns.
/// Neither zero pane counts nor NoStore source may fabricate a new task,
/// discard exact members or queue an ordinary provider continuation afterward.
#[tokio::test(flavor = "current_thread")]
async fn raw_replay_bootstrap_manual_uses_owned_history_without_a_new_task() {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum SourceChange {
        None,
        FrozenCopy,
        SavePolicy,
    }
    for (ephemeral, change) in [
        (false, SourceChange::None),
        (true, SourceChange::None),
        (true, SourceChange::FrozenCopy),
        (false, SourceChange::SavePolicy),
    ] {
        let (fixture, mut service, primary) = setup();
        let conversation = if ephemeral {
            let frozen = service.freeze_agent_replay_history("%1").unwrap().unwrap();
            let target = String::from(mez_agent::ContextBlockOccurrenceId::allocate());
            service
                .bind_ephemeral_replay_history("%1", &target, &frozen, None)
                .unwrap();
            target
        } else {
            crate::test_support::replay_epoch::CONVERSATION.to_owned()
        };
        service
            .execute_agent_shell_command(&primary, "actual task before manual compaction")
            .unwrap();
        let turn = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id
            .clone()
            .unwrap();
        let active = service.agent_turn_contexts()[&turn]
            .chronology()
            .iter()
            .find(|event| event.block().content == "actual task before manual compaction")
            .unwrap()
            .clone();
        let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
        let provider = FinalProvider(std::sync::atomic::AtomicUsize::new(0));
        service
            .execute_agent_turn_with_provider(&turn, &provider, profile)
            .unwrap();
        if !ephemeral {
            // Typed head/tail/arrivals are authoritative even with zero display
            // counts. This does not delete or fabricate an archive/source row.
            service
                .agent_shell_store_mut()
                .bind_conversation("%1", &conversation, 0)
                .unwrap();
        }
        let (_handle, mut actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let response = actor
            .service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        assert!(response.contains("state=preparing"), "{response}");
        let source = actor
            .service
            .take_manual_compaction_preparations()
            .pop()
            .expect("typed manual source preparation");
        let worker = source.clone();
        let result = tokio::task::spawn_blocking(move || worker.execute_source())
            .await
            .unwrap();
        if change != SourceChange::None {
            let before = fixture
                .store
                .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
                .unwrap()
                .map(|(identity, _)| identity);
            match change {
                SourceChange::FrozenCopy => {
                    // A real same-CID copy has a fresh incarnation despite
                    // equal zero counts, save policy and source bytes.
                    let replacement = actor
                        .service
                        .freeze_agent_replay_history("%1")
                        .unwrap()
                        .unwrap();
                    actor
                        .service
                        .bind_ephemeral_replay_history("%1", &conversation, &replacement, None)
                        .unwrap();
                }
                SourceChange::SavePolicy => {
                    // No frozen source exists on either side of this binding
                    // change. Only the captured save policy can distinguish it.
                    actor
                        .service
                        .agent_shell_store_mut()
                        .bind_ephemeral_conversation_with_lineage("%1", &conversation, 0, None)
                        .unwrap();
                }
                SourceChange::None => unreachable!(),
            }
            assert!(
                actor
                    .service
                    .complete_manual_compaction_preparation(&source, result)
                    .is_err()
            );
            assert!(actor.service.take_manual_compaction_requests().is_empty());
            assert!(actor.service.pending_agent_compaction_task_ids().is_empty());
            assert!(!actor.service.agent_is_compacting("%1"));
            assert!(actor.service.pending_agent_provider_tasks().is_empty());
            assert_eq!(
                actor
                    .service
                    .freeze_agent_replay_history("%1")
                    .unwrap()
                    .is_some(),
                change == SourceChange::FrozenCopy
            );
            assert_eq!(
                actor.service.agent_turn_ledger().turn(&turn).unwrap().state,
                mez_agent::AgentTurnState::Completed
            );
            assert_eq!(
                fixture
                    .store
                    .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
                    .unwrap()
                    .map(|(identity, _)| identity),
                before
            );
            continue;
        }
        assert!(
            actor
                .service
                .complete_manual_compaction_preparation(&source, result)
                .unwrap()
        );
        let request = actor
            .service
            .take_manual_compaction_requests()
            .pop()
            .expect("canonical manual request preparation");
        let worker = request.clone();
        let result = tokio::task::spawn_blocking(move || worker.execute_request())
            .await
            .unwrap();
        assert!(
            actor
                .service
                .complete_manual_compaction_request(&request, result)
                .unwrap()
        );
        let generation = actor
            .service
            .pending_agent_compaction_task_generation("%1")
            .unwrap();
        let effects = actor.pending_provider_dispatch_side_effects().unwrap();
        actor.admit_committed_side_effects(effects).unwrap();
        assert!(actor.drain_agent_provider_dispatch_side_effects(64).unwrap().iter().any(|effect|matches!(effect,RuntimeSideEffect::DispatchAgentCompaction{task_generation,..} if *task_generation==generation)));
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_request(AsyncRuntimeRequest::ClaimAgentCompactionTask {
                pane_id: "%1".into(),
                task_generation: generation,
                reply,
            })
            .await;
        let dispatch = response.await.unwrap().unwrap().unwrap();
        assert!(dispatch.task.replay_source.is_some());
        let input = &dispatch.task.request.messages.last().unwrap().content;
        assert!(input.contains("original long task"));
        assert!(!input.contains("context compaction requested"));
        let mut batch = RuntimeEventBatch::default();
        batch.push(RuntimeEvent::AgentCompaction(
            crate::runtime::AgentCompactionEvent::Completed {
                pane_id: "%1".into(),
                task_generation: generation,
                response: Box::new(mez_agent::ModelResponse {
                    provider: "runtime-batch".into(),
                    model: "test".into(),
                    raw_text: "manual canonical head".into(),
                    usage: Default::default(),
                    latest_request_usage: None,
                    quota_usage: Vec::new(),
                    action_batch: None,
                    provider_transcript_events: Vec::new(),
                }),
            },
        ));
        actor.apply_runtime_event_batch(batch).await.unwrap();
        let attempt = take_manual_publication_attempt(&mut actor);
        let publication = attempt.work().clone();
        if !ephemeral {
            fixture.store.inject_replay_epoch_fault(
                crate::storage::transcript::replay_epoch::Phase::AfterManifestSwap,
                1,
                false,
            );
        }
        let mut completion = tokio::task::spawn_blocking(move || attempt.execute())
            .await
            .unwrap();
        if !ephemeral {
            assert!(completion.error().is_some());
            actor
                .apply_runtime_event(RuntimeEvent::Persistence(
                    PersistenceEvent::ReplayEpochCompleted { completion },
                ))
                .await
                .unwrap();
            assert!(actor.service.agent_is_compacting("%1"));
            assert!(actor.service.pending_agent_provider_tasks().is_empty());
            let mut batch = RuntimeEventBatch::default();
            batch.push(RuntimeEvent::Timer(crate::runtime::TimerEvent {
                key: RuntimeTimerKey::new(RuntimeTimerKind::CompactionClaim, "%1", generation),
                now_ms: 1000,
            }));
            actor.apply_runtime_event_batch(batch).await.unwrap();
            let retry = take_manual_publication_attempt(&mut actor);
            assert_eq!(retry.work(), &publication);
            completion = tokio::task::spawn_blocking(move || retry.execute())
                .await
                .unwrap();
        }
        assert!(completion.error().is_none(), "{:?}", completion.error());
        let duplicate = completion.clone();
        actor
            .apply_runtime_event(RuntimeEvent::Persistence(
                PersistenceEvent::ReplayEpochCompleted { completion },
            ))
            .await
            .unwrap();
        assert!(!actor.service.agent_is_compacting("%1"));
        let lineage = actor
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .prompt_cache_lineage_id
            .clone();
        actor
            .apply_runtime_event(RuntimeEvent::Persistence(
                PersistenceEvent::ReplayEpochCompleted {
                    completion: duplicate,
                },
            ))
            .await
            .unwrap();
        assert_eq!(
            actor
                .service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .prompt_cache_lineage_id,
            lineage
        );
        assert!(
            actor
                .service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .running_turn_id
                .is_none()
        );
        assert!(actor.service.pending_agent_provider_tasks().is_empty());
        let body = if ephemeral {
            assert!(
                actor
                    .service
                    .freeze_agent_replay_history("%1")
                    .unwrap()
                    .is_some()
            );
            // A preview admits only a temporary task over the accepted frozen
            // body; original occurrences remain inspectable without disk I/O.
            actor
                .service
                .agent_context_for_pane_prompt("%1", "preview after manual", 0)
                .unwrap()
        } else {
            fixture
                .store
                .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
                .unwrap()
                .unwrap()
                .1
                .into_owned_context()
        };
        let owner = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new(&conversation).unwrap(),
            body.clone(),
        )
        .unwrap();
        assert_eq!(owner.revision().get(), 4);
        assert_eq!(
            body.chronology()[0].block().content,
            "manual canonical head"
        );
        let retained = body
            .chronology()
            .iter()
            .find(|event| event.record().occurrence_id() == active.record().occurrence_id())
            .unwrap();
        assert_eq!(retained.block(), active.block());
        assert_eq!(retained.record(), active.record());
        assert_eq!(retained.sequence(), active.sequence());
        if !ephemeral {
            assert_eq!(retained.retention(), mez_agent::ContextRetention::Exact);
        }
        assert!(
            !body
                .chronology()
                .iter()
                .any(|event| event.block().content == "[context compaction requested]")
        );
        assert!(body.execution_group_is_pending(&fixture.group));
        if ephemeral {
            assert!(
                fixture
                    .store
                    .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(provider.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}

/// Drives only the actual canonical persistence descriptor. Other admitted
/// effects retain their existing lane; no append claim becomes an epoch receipt.
fn take_manual_publication_attempt(
    actor: &mut AsyncRuntimeSessionActor,
) -> crate::runtime::RuntimeReplayEpochAttempt {
    let mut attempt = None;
    let mut other = Vec::new();
    for (effect, claim) in actor.drain_persistence_claims(128).unwrap() {
        match effect {
            RuntimeSideEffect::PersistReplayEpoch { attempt: actual } => {
                assert!(claim.is_none());
                assert!(attempt.replace(actual).is_none());
            }
            effect => other.push(effect),
        }
    }
    actor.admit_committed_side_effects(other).unwrap();
    attempt.expect("manual canonical publication attempt")
}

/// Idle state is not instruction completion. A manual request over a protected
/// first raw member must queue no model and preserve the original frozen view,
/// even though a temporary request context composes a separate prompt.
#[test]
fn raw_replay_bootstrap_manual_no_work_preserves_exact_idle_source() {
    for ephemeral in [false, true] {
        let (fixture, mut service, primary) = setup();
        let conversation = if ephemeral {
            let selected = service.freeze_agent_replay_history("%1").unwrap().unwrap();
            let target = String::from(mez_agent::ContextBlockOccurrenceId::allocate());
            service
                .bind_ephemeral_replay_history("%1", &target, &selected, None)
                .unwrap();
            target
        } else {
            crate::test_support::replay_epoch::CONVERSATION.to_owned()
        };
        let before = ephemeral.then(|| service.freeze_agent_replay_history("%1").unwrap().unwrap());
        service.use_agent_source_preparation_adapter();
        assert!(
            service
                .execute_agent_shell_command(&primary, "/compact")
                .unwrap()
                .contains("state=preparing")
        );
        let source = service.take_manual_compaction_preparations().pop().unwrap();
        let result = source.execute_source();
        assert!(
            service
                .complete_manual_compaction_preparation(&source, result)
                .unwrap()
        );
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.take_manual_compaction_requests().is_empty());
        assert!(service.pending_agent_compaction_task_ids().is_empty());
        assert!(service.pending_agent_provider_tasks().is_empty());
        assert!(service.agent_turn_ledger().turns().is_empty());
        if let Some(before) = before {
            assert_eq!(
                service.freeze_agent_replay_history("%1").unwrap().unwrap(),
                before
            );
        }
        assert_eq!(
            fixture
                .store
                .load_replay_epoch(
                    crate::test_support::replay_epoch::CONVERSATION,
                    mez_agent::AgentContext::empty()
                )
                .unwrap()
                .unwrap()
                .0,
            fixture.identity
        );
        if ephemeral {
            assert!(
                fixture
                    .store
                    .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
                    .unwrap()
                    .is_none()
            );
        }
    }
}

/// Explicitly selecting an empty NoStore source excludes a saved head with the
/// same CID. This is source policy, not permission to infer saved emptiness
/// from a pane counter or to install unselected history into the private view.
#[test]
fn raw_replay_bootstrap_manual_empty_no_store_does_not_inherit_saved_head() {
    for adapter in [false, true] {
        let (fixture, mut service, primary) = setup();
        if adapter {
            service.use_agent_source_preparation_adapter();
        }
        let conversation = crate::test_support::replay_epoch::CONVERSATION;
        service
            .agent_shell_store_mut()
            .bind_ephemeral_conversation_with_lineage("%1", conversation, 0, None)
            .unwrap();
        let response = service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        assert!(
            response.contains("reason=no-transcript-entries"),
            "{response}"
        );
        assert!(!service.agent_is_compacting("%1"));
        assert!(service.take_manual_compaction_preparations().is_empty());
        assert!(service.pending_agent_compaction_task_ids().is_empty());
        assert!(service.pending_agent_provider_tasks().is_empty());
        assert!(service.freeze_agent_replay_history("%1").unwrap().is_none());
        assert_eq!(
            fixture
                .store
                .load_replay_epoch(conversation, mez_agent::AgentContext::empty())
                .unwrap()
                .unwrap()
                .0,
            fixture.identity
        );
    }
}

/// Actual terminal cleanup keeps the initial NoStore body even before any
/// summary exists. A new user task retires only its original instruction scope;
/// existing source IDs/costs and real completed execution fences survive.
#[tokio::test(flavor = "current_thread")]
async fn raw_replay_bootstrap_no_store_terminal_preserves_initial_owned_body() {
    let (fixture, mut service, conversation, turn, primary) = fresh(true);
    let root = service.agent_turn_contexts()[&turn]
        .chronology()
        .iter()
        .find(|event| event.block().source == mez_agent::ContextSourceKind::UserInstruction)
        .unwrap()
        .clone();
    let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    let provider = FinalProvider(std::sync::atomic::AtomicUsize::new(0));
    let execution = service
        .execute_agent_turn_with_provider(&turn, &provider, profile)
        .unwrap();
    assert_eq!(
        execution.terminal_state,
        mez_agent::AgentTurnState::Completed
    );
    assert_eq!(provider.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!service.agent_turn_contexts().contains_key(&turn));
    assert!(service.freeze_agent_replay_history("%1").unwrap().is_some());
    let preview = service
        .agent_context_for_pane_prompt("%1", "preview new task", 0)
        .unwrap();
    let original = preview
        .chronology()
        .iter()
        .find(|event| event.record().occurrence_id() == root.record().occurrence_id())
        .unwrap();
    assert_eq!(original.block(), root.block());
    assert_eq!(original.record(), root.record());
    assert_eq!(original.sequence(), root.sequence());
    assert_eq!(
        original.retention(),
        mez_agent::ContextRetention::Summarizable
    );
    let recovery = preview
        .chronology()
        .iter()
        .filter(|event| {
            event.block().source == mez_agent::ContextSourceKind::CommittedEvidence
                && event.block().label == "canonical recovery facts"
        })
        .map(|event| (event.block().clone(), event.record()))
        .collect::<Vec<_>>();
    assert_eq!(recovery.len(), 1);
    service
        .execute_agent_shell_command(&primary, "actual second task")
        .unwrap();
    let next = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .as_ref()
        .unwrap();
    assert_ne!(next, &turn);
    let body = &service.agent_turn_contexts()[next];
    let actual = body
        .chronology()
        .iter()
        .filter(|event| {
            event.block().source == mez_agent::ContextSourceKind::CommittedEvidence
                && event.block().label == "canonical recovery facts"
        })
        .map(|event| (event.block().clone(), event.record()))
        .collect::<Vec<_>>();
    assert_eq!(actual, recovery);
    let owner = mez_agent::ReplayEpochContext::from_owned_context(
        mez_agent::ReplayConversationId::new(&conversation).unwrap(),
        body.clone(),
    )
    .unwrap();
    assert_eq!(owner.revision().get(), 0);
    assert!(owner.head().is_none());
    assert_eq!(
        fixture.store.inspect(&conversation).unwrap_err().kind(),
        crate::MezErrorKind::NotFound
    );
    assert!(
        fixture
            .store
            .load_replay_epoch(&conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .is_none()
    );
}

/// A retained historical root and the new active root share the fixed prompt
/// label. Routing must select the actual active instruction, preserving both
/// original occurrences instead of assigning the old task to the new worker.
#[tokio::test(flavor = "current_thread")]
async fn raw_replay_bootstrap_routing_selects_active_task_after_terminal_continuity() {
    let (_fixture, mut service, _conversation, first, primary) = fresh(true);
    let profile = service.agent_turn_model_profile(&first).unwrap().clone();
    let provider = FinalProvider(std::sync::atomic::AtomicUsize::new(0));
    service
        .execute_agent_turn_with_provider(&first, &provider, profile)
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "route the active second task")
        .unwrap();
    let current = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .clone()
        .unwrap();
    let turn = service.agent_turn_ledger().turn(&current).unwrap().clone();
    let context = &service.agent_turn_contexts()[&current];
    let roots = context
        .chronology()
        .iter()
        .filter(|event| {
            event.block().source == mez_agent::ContextSourceKind::UserInstruction
                && event.block().label == "user prompt"
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(roots.len(), 2);
    assert_eq!(
        roots[0].retention(),
        mez_agent::ContextRetention::Summarizable
    );
    assert_eq!(roots[1].retention(), mez_agent::ContextRetention::Exact);
    let profile = service.agent_turn_model_profile(&current).unwrap().clone();
    service
        .apply_routing_selected_transition(
            &mez_core::ids::AgentId::opaque(&turn.agent_id).unwrap(),
            &current,
            mez_agent::AutoSizingRoutingSelection {
                selected_profile: profile,
                selected_profile_name: "default".into(),
                routing_token_usage_by_model: Default::default(),
                decision_summary: None,
                fallback: None,
            },
        )
        .unwrap();
    let workflow = service.routed_workflow_for_tests(&current).unwrap();
    let child = &service.agent_turn_contexts()[workflow.child_turn_id.as_ref().unwrap()];
    let task = child
        .chronology()
        .iter()
        .find(|event| {
            event.block().source == mez_agent::ContextSourceKind::LocalMessage
                && event.block().label == "routed controller task"
        })
        .unwrap();
    assert_eq!(task.block().content, "route the active second task");
    for root in roots {
        assert_eq!(
            child
                .chronology()
                .iter()
                .find(|event| event.record().occurrence_id() == root.record().occurrence_id())
                .unwrap(),
            &root
        );
    }
    assert_eq!(provider.0.load(std::sync::atomic::Ordering::SeqCst), 1);
}
