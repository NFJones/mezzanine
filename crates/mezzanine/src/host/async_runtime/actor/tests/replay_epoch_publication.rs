//! Supplied summaries cross the real canonical request/claim/actor/worker ports.
//! No inference or native action is executed. A physical conversation lease
//! proves publication does not run inline on the actor or release a busy claim.

use super::super::*;
use crate::test_support::replay_epoch::{CONVERSATION, Fixture};
use mez_agent::{
    AgentContext, ModelContextMetadata, ModelResponse, ReplayConversationId, ReplayEpochContext,
};

struct Case {
    fixture: Fixture,
    actor: AsyncRuntimeSessionActor,
    handle: AsyncRuntimeSessionHandle,
    turn: String,
    generation: u64,
    conversation: String,
    primary: mez_core::ids::ClientId,
    native_owner: mez_agent::ReplayExecutionOwner,
    native_sequence: mez_agent::ContextEventSequence,
    before: AgentContext,
    raw: bool,
}

pub(super) fn setup() -> (
    Fixture,
    crate::runtime::RuntimeSessionService,
    mez_core::ids::ClientId,
) {
    setup_with_mode(false)
}

fn setup_with_mode(
    raw: bool,
) -> (
    Fixture,
    crate::runtime::RuntimeSessionService,
    mez_core::ids::ClientId,
) {
    let fixture = Fixture::with_instruction(&format!(
        "original long task {}",
        "synthetic historical source ".repeat(4000)
    ));
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service.set_agent_transcript_store(fixture.store.clone());
    if !raw {
        service.set_auth_store(crate::security::auth::AuthStore::new(
            crate::security::auth::AuthPaths::under_config_root(&fixture.root),
        ));
    }
    service.replace_config_layers(vec![crate::config::ConfigLayer {
        name:"canonical-publication".into(),path:None,format:crate::config::ConfigFormat::Toml,
        scope:crate::config::ConfigScope::Primary,trusted:true,
        text:"[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"default\"\nshell_mode=\"pane\"\ncompaction_raw_retention_percent=1\n[providers.runtime-batch]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:9/v1\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.default]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=128000\n[permissions]\nsandbox=\"policy-only\"\n".into(),
    }]).unwrap();
    if raw {
        let mut layers = service.config_layers().to_vec();
        layers[0].text =
            layers[0]
                .text
                .replacen("[agents]", "[agents]\ncompaction_mode=\"raw_tail\"", 1);
        service.replace_config_layers(layers).unwrap();
        assert!(service.auth_store().is_none());
    }
    let primary = service
        .attach_primary("canonical-primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", CONVERSATION, 0)
        .unwrap();
    (fixture, service, primary)
}

async fn case(ephemeral: bool) -> Case {
    case_with_mode(ephemeral, false).await
}

async fn case_with_mode(ephemeral: bool, raw: bool) -> Case {
    let (fixture, mut service, primary) = setup_with_mode(raw);
    if ephemeral {
        let frozen = service.freeze_agent_replay_history("%1").unwrap().unwrap();
        let target = String::from(mez_agent::ContextBlockOccurrenceId::allocate());
        service
            .bind_ephemeral_replay_history("%1", &target, &frozen, None)
            .unwrap();
    }
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let admission = service
        .execute_agent_shell_command(&primary, "continue with a new exact task")
        .unwrap();
    let turn = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .clone()
        .unwrap_or_else(|| panic!("actual prompt was not admitted: {admission:?}"));
    service.remove_pending_agent_provider_task(&turn);
    let context = service.agent_turn_contexts_mut().get_mut(&turn).unwrap();
    assert!(context.has_replay_owner());
    context.set_metadata(ModelContextMetadata::new(
        Some("original-live-session"),
        Some("original-cache-lineage"),
    ));
    let native_sequence = context
        .chronology()
        .iter()
        .find(|event| event.execution_group_id() == Some(&fixture.group))
        .unwrap()
        .sequence();
    let native_owner = context
        .update_replay_owner(|owner| owner.capture_execution_owner(&fixture.group, native_sequence))
        .unwrap();
    let before = context.clone();
    let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    assert!(
        service
            .queue_agent_active_turn_compaction(
                &turn,
                "default".into(),
                profile,
                crate::runtime::RuntimeActiveTurnCompactionTrigger::ProviderContextLimit {
                    attempt: 1
                },
            )
            .unwrap()
    );
    let generation = service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    let (handle, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    actor.admit_committed_side_effects(effects).unwrap();
    let offered = actor
        .drain_agent_provider_dispatch_side_effects(64)
        .unwrap();
    assert!(offered.iter().any(|effect|matches!(effect,RuntimeSideEffect::DispatchAgentCompaction {task_generation,..} if *task_generation==generation)));
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::ClaimAgentCompactionTask {
            pane_id: "%1".into(),
            task_generation: generation,
            reply,
        })
        .await;
    let dispatch = response.await.unwrap().unwrap();
    if raw {
        assert!(dispatch.is_none(), "raw claim dispatched model work");
        assert!(
            !actor
                .service
                .retains_compaction_expense_for_tests("%1", generation)
        );
        // The real timer worker drains its new lease. This is the existing
        // capacity-progress boundary that routes service-owned persistence.
        let timers = actor.drain_timer_side_effects(128).unwrap();
        assert!(timers.iter().any(|effect| matches!(effect,
            RuntimeSideEffect::ScheduleTimer { key, .. } if key.kind == RuntimeTimerKind::CompactionClaim)));
    } else {
        let dispatch = dispatch.unwrap();
        assert!(dispatch.task.replay_source.is_some());
        let input =
            crate::test_support::compaction_source::rendered_source(&dispatch.task).unwrap();
        assert!(input.contains("model head 3"));
        assert!(input.contains("synthetic historical source"));
        assert!(!input.contains("COVERED_SOURCE_SENTINEL"));
        assert!(!input.contains("original native declaration"));
    }
    Case {
        fixture,
        actor,
        handle,
        turn,
        generation,
        conversation,
        primary,
        native_owner,
        native_sequence,
        before,
        raw,
    }
}

fn response() -> ModelResponse {
    ModelResponse {
        provider: "runtime-batch".into(),
        model: "test".into(),
        raw_text: "accepted canonical head".into(),
        usage: Default::default(),
        latest_request_usage: None,
        quota_usage: Vec::new(),
        action_batch: None,
        provider_transcript_events: Vec::new(),
    }
}

async fn complete(case: &mut Case) -> crate::runtime::RuntimeReplayEpochAttempt {
    let task = case
        .actor
        .service
        .claimed_agent_compaction_task("%1", case.generation)
        .unwrap()
        .clone();
    if task.request.is_none() {
        // Raw admission already queued the original publication; there is no
        // model event or dummy response to synthesize at this boundary.
        return take_attempt(case);
    }
    let (result, _) = crate::test_support::compaction_source::execute_wave(
        &case.actor.service,
        &task,
        Ok(response()),
    );
    let mut batch = RuntimeEventBatch::default();
    batch.push(RuntimeEvent::AgentCompaction(
        crate::test_support::compaction_source::model_result_event("%1", case.generation, result),
    ));
    case.actor.apply_runtime_event_batch(batch).await.unwrap();
    assert!(
        case.actor
            .service
            .agent_compaction_task_is_claimed("%1", case.generation)
    );
    assert!(case.actor.service.agent_is_compacting("%1"));
    take_attempt(case)
}

fn take_attempt(case: &mut Case) -> crate::runtime::RuntimeReplayEpochAttempt {
    let mut attempt = None;
    let mut other = Vec::new();
    for (effect, claim) in case.actor.drain_persistence_claims(128).unwrap() {
        match effect {
            RuntimeSideEffect::PersistReplayEpoch { attempt: actual } => {
                assert!(claim.is_none());
                assert!(attempt.replace(actual).is_none());
            }
            effect => {
                assert!(
                    !matches!(
                        effect,
                        RuntimeSideEffect::PersistTranscriptEntries { .. }
                            | RuntimeSideEffect::PersistInitialTranscript { .. }
                    ),
                    "this pending-source fixture must not skip ordinary append claims"
                );
                other.push(effect);
            }
        }
    }
    case.actor.admit_committed_side_effects(other).unwrap();
    attempt.expect("actual canonical worker effect")
}

fn timer(case: &mut Case) -> crate::Result<RuntimeTransition> {
    case.actor
        .apply_runtime_timer_event(crate::runtime::TimerEvent {
            key: RuntimeTimerKey::new(RuntimeTimerKind::CompactionClaim, "%1", case.generation),
            now_ms: 1000,
        })
}

async fn retry(case: &mut Case) -> crate::runtime::RuntimeReplayEpochAttempt {
    let mut batch = RuntimeEventBatch::default();
    batch.push(RuntimeEvent::Timer(crate::runtime::TimerEvent {
        key: RuntimeTimerKey::new(RuntimeTimerKind::CompactionClaim, "%1", case.generation),
        now_ms: 1000,
    }));
    case.actor.apply_runtime_event_batch(batch).await.unwrap();
    take_attempt(case)
}

async fn deliver(case: &mut Case, completion: crate::runtime::RuntimeReplayEpochCompletion) {
    case.actor
        .apply_runtime_event(RuntimeEvent::Persistence(
            PersistenceEvent::ReplayEpochCompleted { completion },
        ))
        .await
        .unwrap();
}

/// Both durable and NoStore raw claims use the real actor timer/persistence
/// routes with no provider dispatch. Losing the storage callback still adopts
/// that exact receipt once, without a fake model completion or another request.
#[tokio::test(flavor = "current_thread")]
async fn async_raw_tail_publication_routes_and_recovers_lost_callback() {
    for ephemeral in [false, true] {
        let mut case = case_with_mode(ephemeral, true).await;
        assert_eq!(
            case.actor.service.agent_turn_contexts()[&case.turn],
            case.before
        );
        let attempt = take_attempt(&mut case);
        let completion = tokio::task::spawn_blocking(move || attempt.execute())
            .await
            .unwrap();
        assert!(completion.error().is_none(), "{:?}", completion.error());
        assert_eq!(
            case.actor.service.agent_turn_contexts()[&case.turn],
            case.before
        );
        drop(completion);
        timer(&mut case).unwrap();
        let after = case.actor.service.agent_turn_contexts()[&case.turn].clone();
        let owner = ReplayEpochContext::from_owned_context(
            ReplayConversationId::new(&case.conversation).unwrap(),
            after.clone(),
        )
        .unwrap();
        assert!(owner.head().is_none());
        assert_eq!(
            owner.head_policy(),
            mez_agent::ReplayHeadPolicy::IntentionalOmission
        );
        assert!(!case.actor.service.agent_is_compacting("%1"));
        assert!(
            case.actor
                .service
                .agent_provider_task_is_pending(&case.turn)
        );
        assert_ne!(
            after.metadata().prompt_cache_lineage_id,
            case.before.metadata().prompt_cache_lineage_id
        );
        assert!(
            !after
                .blocks()
                .iter()
                .any(|block| block.content.contains("COVERED_SOURCE_SENTINEL"))
        );
        timer(&mut case).unwrap();
        assert_eq!(case.actor.service.agent_turn_contexts()[&case.turn], after);
        assert_adopted(&mut case);
        if !ephemeral {
            let (_, reopened) = case
                .fixture
                .store
                .load_replay_epoch(&case.conversation, AgentContext::empty())
                .unwrap()
                .unwrap();
            assert!(reopened.head().is_none());
            assert_eq!(reopened.context().chronology(), after.chronology());
        }
    }
}

/// Raw publication retries the original omission proposal after uncertain
/// writes. No model result, credential or second logical compaction is created.
#[tokio::test(flavor = "current_thread")]
async fn async_raw_tail_publication_recovers_real_storage_phase_failures() {
    use crate::storage::transcript::replay_epoch::Phase;
    for phase in [
        Phase::BeforePayloadSync,
        Phase::BeforeSourceReceiptSync,
        Phase::BeforeManifestSwap,
        Phase::AfterManifestSwap,
        Phase::BeforeRequiredMarker,
        Phase::BeforeAcknowledgment,
    ] {
        let mut case = case_with_mode(false, true).await;
        case.fixture
            .store
            .inject_replay_epoch_fault(phase, 1, false);
        let attempt = take_attempt(&mut case);
        let failed = tokio::task::spawn_blocking(move || attempt.execute())
            .await
            .unwrap();
        assert!(failed.error().is_some(), "{phase:?}");
        deliver(&mut case, failed).await;
        assert!(case.actor.service.agent_is_compacting("%1"));
        assert_eq!(
            case.actor.service.agent_turn_contexts()[&case.turn],
            case.before
        );
        let attempt = retry(&mut case).await;
        let completed = tokio::task::spawn_blocking(move || attempt.execute())
            .await
            .unwrap();
        assert!(
            completed.error().is_none(),
            "{phase:?}: {:?}",
            completed.error()
        );
        deliver(&mut case, completed.clone()).await;
        assert!(!case.actor.service.agent_is_compacting("%1"));
        assert!(
            case.actor
                .service
                .agent_provider_task_is_pending(&case.turn)
        );
        let after = case.actor.service.agent_turn_contexts()[&case.turn].clone();
        let owner = ReplayEpochContext::from_owned_context(
            ReplayConversationId::new(&case.conversation).unwrap(),
            after.clone(),
        )
        .unwrap();
        assert!(owner.head().is_none());
        assert_eq!(
            owner.head_policy(),
            mez_agent::ReplayHeadPolicy::IntentionalOmission
        );
        assert_eq!(
            owner.revision().get(),
            4,
            "retry republished a second epoch"
        );
        deliver(&mut case, completed).await;
        assert_eq!(case.actor.service.agent_turn_contexts()[&case.turn], after);
    }
}

/// Every fault is injected inside the real writer. Retry uses the originally
/// prepared proposal; a post-swap error cannot generate another head.
#[tokio::test(flavor = "current_thread")]
async fn async_replay_epoch_publication_recovers_real_storage_phase_failures() {
    use crate::storage::transcript::replay_epoch::Phase;
    for phase in [
        Phase::BeforePayloadSync,
        Phase::BeforeSourceReceiptSync,
        Phase::BeforeManifestSwap,
        Phase::AfterManifestSwap,
        Phase::BeforeRequiredMarker,
        Phase::BeforeAcknowledgment,
    ] {
        let mut case = case(false).await;
        case.fixture
            .store
            .inject_replay_epoch_fault(phase, 1, false);
        let attempt = complete(&mut case).await;
        let completion = tokio::task::spawn_blocking(move || attempt.execute())
            .await
            .unwrap();
        assert!(completion.error().is_some(), "{phase:?}");
        deliver(&mut case, completion).await;
        assert!(case.actor.service.agent_is_compacting("%1"));
        assert_eq!(
            case.actor.service.agent_turn_contexts()[&case.turn].metadata(),
            case.before.metadata()
        );
        let committed = if matches!(
            phase,
            Phase::AfterManifestSwap | Phase::BeforeRequiredMarker | Phase::BeforeAcknowledgment
        ) {
            Some(
                case.fixture
                    .store
                    .load_replay_epoch(CONVERSATION, AgentContext::empty())
                    .unwrap()
                    .unwrap()
                    .0,
            )
        } else {
            None
        };
        let attempt = retry(&mut case).await;
        assert_eq!(attempt.number(), 2);
        let completion = tokio::task::spawn_blocking(move || attempt.execute())
            .await
            .unwrap();
        assert!(completion.error().is_none(), "{phase:?}");
        deliver(&mut case, completion).await;
        assert_adopted(&mut case);
        if let Some(committed) = committed {
            assert_eq!(
                case.fixture
                    .store
                    .load_replay_epoch(CONVERSATION, AgentContext::empty())
                    .unwrap()
                    .unwrap()
                    .0,
                committed
            );
        }
    }
}

/// Recovery stops at three writes, fails only the waiting turn, and leaves
/// the prior complete head and ordinary pane/client admission usable.
#[tokio::test(flavor = "current_thread")]
async fn async_replay_epoch_publication_exhausts_recovery_without_daemon_failure() {
    exhausts_recovery_without_daemon_failure(false).await;
}

/// Zero-model work retains the same finite write bound and confines exhausted
/// recovery to its original waiting turn, while other panes remain usable.
#[tokio::test(flavor = "current_thread")]
async fn async_raw_tail_publication_exhausts_recovery_without_daemon_failure() {
    exhausts_recovery_without_daemon_failure(true).await;
}

async fn exhausts_recovery_without_daemon_failure(raw: bool) {
    use crate::storage::transcript::replay_epoch::Phase;
    let mut case = case_with_mode(false, raw).await;
    case.fixture
        .store
        .inject_replay_epoch_fault(Phase::BeforeManifestSwap, 3, false);
    let mut attempt = complete(&mut case).await;
    let original_calls = case
        .actor
        .service
        .claimed_agent_compaction_task("%1", case.generation)
        .unwrap()
        .replay_source
        .as_ref()
        .unwrap()
        .model_progress()
        .model_calls;
    if raw {
        assert_eq!(original_calls, 0);
    } else {
        assert!(original_calls > 0);
    }
    for number in 1..=3 {
        assert_eq!(attempt.number(), number);
        let completion = tokio::task::spawn_blocking(move || attempt.execute())
            .await
            .unwrap();
        assert!(completion.error().is_some());
        deliver(&mut case, completion).await;
        if number == 3 {
            break;
        }
        assert!(case.actor.service.agent_is_compacting("%1"));
        attempt = retry(&mut case).await;
    }
    assert!(!case.actor.service.agent_is_compacting("%1"));
    let diagnostic: String = case
        .actor
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("")
        .split_whitespace()
        .collect();
    assert!(diagnostic.contains("Auxiliarycompaction:"));
    assert!(diagnostic.contains("phase=publishing"));
    assert!(diagnostic.contains(&format!("dispatch={};", case.generation)));
    assert!(diagnostic.contains(&format!("model_calls={original_calls};")));
    assert_eq!(
        case.actor
            .service
            .agent_turn_ledger()
            .turn(&case.turn)
            .unwrap()
            .state,
        mez_agent::AgentTurnState::Failed
    );
    assert_eq!(
        case.fixture
            .store
            .load_replay_epoch(CONVERSATION, AgentContext::empty())
            .unwrap()
            .unwrap()
            .0,
        case.fixture.identity
    );
    assert!(!timer(&mut case).unwrap().applied);
    // The failed conversation retains its bookkeeping fence. A distinct pane
    // and conversation must still admit useful work through the ordinary shell.
    let other = case
        .actor
        .service
        .session_mut_for_tests()
        .split_active_pane(&case.primary, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    case.actor
        .service
        .agent_shell_store_mut()
        .enter_or_resume(other.as_str())
        .unwrap();
    let admission = case
        .actor
        .service
        .execute_agent_shell_command(&case.primary, "unaffected task")
        .unwrap();
    assert!(
        case.actor
            .service
            .agent_shell_store()
            .get(other.as_str())
            .unwrap()
            .running_turn_id
            .is_some(),
        "{admission:?}"
    );
}

/// Cancellation does not release the physical guard. A worker already waiting
/// behind the real conversation lease cannot publish after it is cancelled.
#[tokio::test(flavor = "current_thread")]
async fn async_replay_epoch_publication_cancelled_physical_write_cannot_publish() {
    cancelled_physical_write_cannot_publish(false).await;
}

/// Raw cancellation must retain the physical attempt guard and prevent a
/// pending real write, without relying on a provider worker to settle it.
#[tokio::test(flavor = "current_thread")]
async fn async_raw_tail_publication_cancelled_physical_write_cannot_publish() {
    cancelled_physical_write_cannot_publish(true).await;
}

async fn cancelled_physical_write_cannot_publish(raw: bool) {
    let mut case = case_with_mode(false, raw).await;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            case.fixture
                .root
                .join(".conversation-locks")
                .join(format!("{CONVERSATION}.lock")),
        )
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let attempt = complete(&mut case).await;
    let (started, ready) = tokio::sync::oneshot::channel();
    let worker = tokio::task::spawn_blocking(move || {
        let _ = started.send(());
        attempt.execute()
    });
    ready.await.unwrap();
    case.actor
        .service
        .finish_agent_compaction_task("%1", case.generation);
    let blocked = !worker.is_finished();
    drop(lock);
    let completion = worker.await.unwrap();
    assert!(blocked);
    assert!(completion.error().is_some());
    deliver(&mut case, completion).await;
    assert_eq!(
        case.actor.service.agent_turn_contexts()[&case.turn],
        case.before
    );
    assert_eq!(
        case.fixture
            .store
            .load_replay_epoch(CONVERSATION, AgentContext::empty())
            .unwrap()
            .unwrap()
            .0,
        case.fixture.identity
    );
}

/// Real service spawn/join catches a worker panic after the manifest swap. Its
/// next owned timer retries that exact poisoned-but-complete retained proposal.
#[tokio::test(flavor = "current_thread")]
async fn async_replay_epoch_publication_persistence_service_recovers_joined_panic() {
    use crate::host::async_runtime::{
        AsyncRuntimeSideEffectServiceConfig, run_async_persistence_side_effect_service,
    };
    use crate::storage::transcript::replay_epoch::Phase;
    let mut case = case(false).await;
    case.fixture
        .store
        .inject_replay_epoch_fault(Phase::AfterManifestSwap, 1, true);
    let attempt = complete(&mut case).await;
    case.actor
        .admit_committed_side_effects(vec![RuntimeSideEffect::PersistReplayEpoch { attempt }])
        .unwrap();
    let Case {
        actor,
        handle,
        fixture,
        generation,
        ..
    } = case;
    let client = async {
        let config = AsyncRuntimeSideEffectServiceConfig {
            max_polls: 1,
            drain_limit: 64,
            idle_interval: std::time::Duration::from_millis(1),
        };
        let first = run_async_persistence_side_effect_service(&handle, config, |_, _| false)
            .await
            .unwrap();
        assert_eq!(first.failed, 1, "{first:?}");
        let identity = fixture
            .store
            .load_replay_epoch(CONVERSATION, AgentContext::empty())
            .unwrap()
            .unwrap()
            .0;
        let mut batch = RuntimeEventBatch::default();
        batch.push(RuntimeEvent::Timer(crate::runtime::TimerEvent {
            key: RuntimeTimerKey::new(RuntimeTimerKind::CompactionClaim, "%1", generation),
            now_ms: 1000,
        }));
        handle.submit_runtime_events(batch).await.unwrap();
        let next = run_async_persistence_side_effect_service(&handle, config, |_, _| false)
            .await
            .unwrap();
        let shutdown = handle.shutdown().await;
        (identity, next, shutdown)
    };
    let ((identity, next, shutdown), exit) = tokio::join!(client, actor.run());
    let screen = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .visible_lines()
        .join("\n");
    let errors = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&crate::protocol::event::EventAudience::AllPrimaries)
        .into_iter()
        .filter(|event| event.payload.contains("\"state\":\"failed\""))
        .map(|event| event.payload)
        .collect::<Vec<_>>();
    assert_eq!(next.failed, 0, "{next:?}\n{screen}\n{errors:#?}");
    assert!(next.completed >= 1, "{next:?}\n{screen}");
    shutdown.unwrap();
    assert!(!exit.service.agent_is_compacting("%1"));
    let (actual, cold) = fixture
        .store
        .load_replay_epoch(CONVERSATION, AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(actual, identity);
    assert_eq!(cold.revision().get(), 4);
    assert_eq!(
        cold.context().chronology()[0].block().content,
        "accepted canonical head"
    );
}

fn assert_adopted(case: &mut Case) {
    let session_lineage = case
        .actor
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .prompt_cache_lineage_id
        .clone();
    assert!(!case.actor.service.agent_is_compacting("%1"));
    assert!(
        !case
            .actor
            .service
            .agent_compaction_task_is_claimed("%1", case.generation)
    );
    let body = case
        .actor
        .service
        .agent_turn_contexts_mut()
        .get_mut(&case.turn)
        .unwrap();
    let native = body
        .update_replay_owner(|owner| {
            owner.capture_execution_owner(&case.fixture.group, case.native_sequence)
        })
        .unwrap();
    assert_eq!(native, case.native_owner);
    assert_eq!(
        body.metadata().prompt_cache_session_id.as_deref(),
        Some("original-live-session")
    );
    assert_ne!(
        body.metadata().prompt_cache_lineage_id,
        case.before.metadata().prompt_cache_lineage_id
    );
    assert_eq!(
        body.metadata().prompt_cache_lineage_id.as_deref(),
        Some(session_lineage.as_str())
    );
    let owner = ReplayEpochContext::from_owned_context(
        ReplayConversationId::new(case.conversation.clone()).unwrap(),
        body.clone(),
    )
    .unwrap();
    assert_eq!(owner.revision().get(), 4);
    if case.raw {
        assert!(owner.head().is_none());
        assert_eq!(
            owner.head_policy(),
            mez_agent::ReplayHeadPolicy::IntentionalOmission
        );
    } else {
        assert_eq!(
            owner.context().chronology()[0].block().content,
            "accepted canonical head"
        );
        assert_eq!(owner.head_policy(), mez_agent::ReplayHeadPolicy::Summary);
    }
    assert_eq!(
        owner
            .context()
            .chronology()
            .iter()
            .filter(|event| event.block().source == mez_agent::ContextSourceKind::CompactionPrefix)
            .count(),
        usize::from(!case.raw)
    );
}

/// The actor completes and polls while a real writer lease excludes the worker.
/// Pending source received during inference is in the acknowledged checkpoint.
#[tokio::test(flavor = "current_thread")]
async fn async_replay_epoch_publication_retains_claim_through_physical_write_and_ack() {
    let mut case = case(false).await;
    let context = case
        .actor
        .service
        .agent_turn_contexts_mut()
        .get_mut(&case.turn)
        .unwrap();
    context
        .append_reference_event(
            mez_agent::ContextSourceKind::RuntimeHint,
            "during inference",
            "original during-inference arrival",
        )
        .unwrap();
    let arrival = context.chronology().last().unwrap().clone();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            case.fixture
                .root
                .join(".conversation-locks")
                .join(format!("{CONVERSATION}.lock")),
        )
        .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let attempt = complete(&mut case).await;
    assert_eq!(
        case.actor.service.agent_turn_contexts()[&case.turn].metadata(),
        case.before.metadata()
    );
    let (started, ready) = tokio::sync::oneshot::channel();
    let worker = tokio::task::spawn_blocking(move || {
        let _ = started.send(());
        attempt.execute()
    });
    ready.await.unwrap();
    let polled = timer(&mut case);
    let still_running = !worker.is_finished();
    let still_claimed = case
        .actor
        .service
        .agent_compaction_task_is_claimed("%1", case.generation);
    drop(lock);
    let completion = worker.await.unwrap();
    assert!(!polled.unwrap().applied);
    assert!(still_running && still_claimed);
    assert!(completion.error().is_none());
    let duplicate = completion.clone();
    let transition = case
        .actor
        .apply_runtime_event(RuntimeEvent::Persistence(
            PersistenceEvent::ReplayEpochCompleted { completion },
        ))
        .await
        .unwrap();
    assert!(transition.applied);
    assert_adopted(&mut case);
    let body = case.actor.service.agent_turn_contexts()[&case.turn].clone();
    let cold = case
        .fixture
        .store
        .load_replay_epoch(CONVERSATION, AgentContext::empty())
        .unwrap()
        .unwrap()
        .1;
    assert!(cold.context().chronology().contains(&arrival));
    assert_eq!(cold.context().chronology(), body.chronology());
    assert!(
        !case
            .actor
            .apply_runtime_event(RuntimeEvent::Persistence(
                PersistenceEvent::ReplayEpochCompleted {
                    completion: duplicate
                }
            ))
            .await
            .unwrap()
            .applied
    );
    assert_eq!(case.actor.service.agent_turn_contexts()[&case.turn], body);
}

/// A watchdog observes the retained real receipt after the callback is dropped.
/// Late duplicate delivery cannot replace current context or allocate a new head.
#[tokio::test(flavor = "current_thread")]
async fn async_replay_epoch_publication_recovers_lost_ack_without_regenerating_head() {
    let mut case = case(false).await;
    let attempt = complete(&mut case).await;
    let completion = tokio::task::spawn_blocking(move || attempt.execute())
        .await
        .unwrap();
    assert!(completion.error().is_none());
    let identity = case
        .fixture
        .store
        .load_replay_epoch(CONVERSATION, AgentContext::empty())
        .unwrap()
        .unwrap()
        .0;
    assert!(
        case.actor
            .service
            .agent_compaction_task_is_claimed("%1", case.generation)
    );
    assert!(timer(&mut case).unwrap().applied);
    assert_adopted(&mut case);
    let before = case.actor.service.agent_turn_contexts()[&case.turn].clone();
    assert!(
        !case
            .actor
            .apply_runtime_event(RuntimeEvent::Persistence(
                PersistenceEvent::ReplayEpochCompleted { completion }
            ))
            .await
            .unwrap()
            .applied
    );
    assert_eq!(case.actor.service.agent_turn_contexts()[&case.turn], before);
    assert_eq!(
        case.fixture
            .store
            .load_replay_epoch(CONVERSATION, AgentContext::empty())
            .unwrap()
            .unwrap()
            .0,
        identity
    );
}

/// Actual admitted target policy remains independent of its saved source.
#[tokio::test(flavor = "current_thread")]
async fn async_replay_epoch_publication_no_store_adopts_without_durable_authority() {
    let mut case = case(true).await;
    let path = case
        .fixture
        .root
        .join(CONVERSATION)
        .join("compaction-epoch.json");
    let original = std::fs::read(&path).unwrap();
    let attempt = complete(&mut case).await;
    let completion = tokio::task::spawn_blocking(move || attempt.execute())
        .await
        .unwrap();
    assert!(completion.error().is_none());
    assert!(
        case.actor
            .apply_runtime_event(RuntimeEvent::Persistence(
                PersistenceEvent::ReplayEpochCompleted { completion }
            ))
            .await
            .unwrap()
            .applied
    );
    assert_adopted(&mut case);
    assert_eq!(std::fs::read(path).unwrap(), original);
    assert!(
        !case
            .fixture
            .root
            .join(&case.conversation)
            .join("compaction-epoch.json")
            .exists()
    );
    assert_eq!(
        case.fixture
            .store
            .load_replay_epoch(CONVERSATION, AgentContext::empty())
            .unwrap()
            .unwrap()
            .0,
        case.fixture.identity
    );
    case.actor
        .service
        .agent_turn_contexts_mut()
        .get_mut(&case.turn)
        .unwrap()
        .append_reference_event(
            mez_agent::ContextSourceKind::RuntimeHint,
            "after ACK",
            "EXACT_POST_ACK_ARRIVAL",
        )
        .unwrap();
    let accepted = case.actor.service.agent_turn_contexts()[&case.turn]
        .chronology()
        .to_vec();
    let lineage = case
        .actor
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .prompt_cache_lineage_id
        .clone();
    let turn = case
        .actor
        .service
        .agent_turn_ledger()
        .turn(&case.turn)
        .unwrap()
        .clone();
    case.actor
        .service
        .complete_running_agent_turn_and_start_ready(
            &turn,
            mez_agent::AgentTurnState::Completed,
            "synthetic_terminal_acceptance",
        )
        .unwrap();
    assert!(
        !case
            .actor
            .service
            .agent_turn_contexts()
            .contains_key(&case.turn)
    );
    let admission = case
        .actor
        .service
        .execute_agent_shell_command(&case.primary, "next NoStore task")
        .unwrap();
    let next = case
        .actor
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .as_ref()
        .unwrap_or_else(|| panic!("next prompt was not admitted: {admission:?}"));
    let next = &case.actor.service.agent_turn_contexts()[next];
    assert_eq!(
        next.metadata().prompt_cache_lineage_id.as_deref(),
        Some(lineage.as_str())
    );
    // Explicit new-user admission may retire prior instruction protection;
    // source bytes, original records and order must remain unchanged.
    assert!(next.chronology().len() >= accepted.len());
    for (actual, original) in next.chronology().iter().zip(&accepted) {
        assert_eq!(actual.block(), original.block());
        assert_eq!(actual.record(), original.record());
        assert_eq!(actual.sequence(), original.sequence());
        assert_eq!(actual.execution_group_id(), original.execution_group_id());
        assert_eq!(actual.provider_owner(), original.provider_owner());
    }
    assert_eq!(
        next.chronology()
            .iter()
            .filter(|event| event.block().content == "EXACT_POST_ACK_ARRIVAL")
            .count(),
        1
    );
    assert!(
        !case
            .fixture
            .root
            .join(&case.conversation)
            .join("compaction-epoch.json")
            .exists()
    );
}

/// Once captured, a NoStore history read uses its original typed head even if
/// the parent becomes unreadable and the runtime installs a different store.
#[tokio::test(flavor = "current_thread")]
async fn async_replay_epoch_publication_frozen_source_ignores_later_parent_and_store() {
    frozen_source_ignores_later_parent_and_store(false).await;
}

/// Raw NoStore publication keeps original custody after both its saved parent
/// and newly installed store become unsuitable. Neither may retarget the work.
#[tokio::test(flavor = "current_thread")]
async fn async_raw_tail_publication_frozen_source_ignores_later_parent_and_store() {
    frozen_source_ignores_later_parent_and_store(true).await;
}

async fn frozen_source_ignores_later_parent_and_store(raw: bool) {
    let mut case = case_with_mode(true, raw).await;
    let before = case
        .actor
        .service
        .agent_context_for_pane_prompt("%1", "history probe", 0)
        .unwrap();
    let path = case
        .fixture
        .root
        .join(CONVERSATION)
        .join("compaction-epoch.json");
    std::fs::write(path, b"later parent is deliberately unreadable").unwrap();
    case.actor.service.set_agent_transcript_store(
        crate::storage::transcript::AgentTranscriptStore::new(
            case.fixture.root.join("replacement"),
        ),
    );
    let after = case
        .actor
        .service
        .agent_context_for_pane_prompt("%1", "history probe", 0)
        .unwrap();
    assert!(
        after
            .chronology()
            .iter()
            .map(|event| event.block())
            .eq(before.chronology().iter().map(|event| event.block()))
    );
    let attempt = complete(&mut case).await;
    let completion = tokio::task::spawn_blocking(move || attempt.execute())
        .await
        .unwrap();
    assert!(completion.error().is_none());
    case.actor
        .apply_runtime_event(RuntimeEvent::Persistence(
            PersistenceEvent::ReplayEpochCompleted { completion },
        ))
        .await
        .unwrap();
    assert_adopted(&mut case);
}

/// The ordinary loop command captures the typed parent once. The controller's
/// next work admission must not reread a later parent, even with zero raw rows.
#[tokio::test(flavor = "current_thread")]
async fn async_replay_epoch_publication_loop_iterations_keep_original_parent() {
    let (fixture, mut service, primary) = setup();
    let admission = service
        .execute_agent_shell_command(&primary, "/loop --fork --limit 2 inspect frozen source")
        .unwrap();
    let first = service.agent_shell_store().get("%1").unwrap().clone();
    assert!(first.ephemeral, "{admission:?}");
    let first_turn = first
        .running_turn_id
        .as_ref()
        .unwrap_or_else(|| panic!("{admission:?}"));
    let body = &service.agent_turn_contexts()[first_turn];
    for (actual, original) in body
        .chronology()
        .iter()
        .zip(fixture.source.context().chronology())
    {
        assert_eq!(actual.block(), original.block());
        assert_eq!(actual.record(), original.record());
    }
    let frozen = service
        .agent_loop_state("%1")
        .unwrap()
        .parent_frozen_replay
        .clone()
        .unwrap();
    std::fs::write(
        fixture
            .root
            .join(CONVERSATION)
            .join("compaction-epoch.json"),
        b"later parent cannot be reread",
    )
    .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(first_turn)
        .unwrap()
        .clone();
    service
        .complete_running_agent_turn_and_start_ready(
            &turn,
            mez_agent::AgentTurnState::Completed,
            "synthetic_loop_result",
        )
        .unwrap();
    service.agent_loop_state_mut("%1").unwrap().iteration = 2;
    let second_turn = service.start_agent_loop_work_turn("%1").unwrap();
    let second = service.agent_shell_store().get("%1").unwrap();
    assert_ne!(second.session_id, first.session_id);
    assert!(second.ephemeral);
    assert_eq!(
        service
            .agent_loop_state("%1")
            .unwrap()
            .parent_frozen_replay
            .as_ref(),
        Some(&frozen)
    );
    let body = &service.agent_turn_contexts()[&second_turn.turn_id];
    assert!(body.chronology().len() > fixture.source.context().chronology().len());
    for (actual, original) in body
        .chronology()
        .iter()
        .zip(fixture.source.context().chronology())
    {
        assert_eq!(actual.block(), original.block());
        assert_eq!(actual.record(), original.record());
        assert_eq!(actual.sequence(), original.sequence());
    }
    assert!(
        !fixture
            .root
            .join(&second.session_id)
            .join("compaction-epoch.json")
            .exists()
    );
}

/// Counts remain insufficient to bind a typed head. The new strong capture port
/// must not make the older bare numeric binding silently read a later parent.
#[tokio::test(flavor = "current_thread")]
async fn async_replay_epoch_publication_count_only_source_still_refuses_admission() {
    let (_fixture, mut service, primary) = setup();
    service
        .agent_shell_store_mut()
        .bind_ephemeral_conversation_with_lineage_and_transcript_source(
            "%1",
            String::from(mez_agent::ContextBlockOccurrenceId::allocate()),
            0,
            None,
            Some(CONVERSATION.into()),
            0,
        )
        .unwrap();
    let admission = service
        .execute_agent_shell_command(&primary, "count-only source must not run")
        .unwrap();
    assert!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id
            .is_none()
    );
    assert!(
        format!("{admission:?}").contains("frozen source requires its original replay checkpoint"),
        "{admission:?}"
    );
}

/// Both loop isolation modes return to the same unsaved typed parent without
/// promoting it to restart authority or losing its original immutable view.
#[tokio::test(flavor = "current_thread")]
async fn async_replay_epoch_publication_loop_restores_no_store_parent_policy() {
    for mode in ["--fork", "--new"] {
        let (fixture, mut service, primary) = setup();
        let source = service.freeze_agent_replay_history("%1").unwrap().unwrap();
        let target = String::from(mez_agent::ContextBlockOccurrenceId::allocate());
        service
            .bind_ephemeral_replay_history("%1", &target, &source, None)
            .unwrap();
        let parent = service.agent_shell_store().get("%1").unwrap().clone();
        let original = service
            .frozen_replay_binding("%1", &target)
            .unwrap()
            .clone();
        let admission = service
            .execute_agent_shell_command(
                &primary,
                &format!("/loop {mode} --limit 1 inspect unsaved parent"),
            )
            .unwrap();
        let work = service.agent_shell_store().get("%1").unwrap().clone();
        let turn_id = work
            .running_turn_id
            .as_ref()
            .unwrap_or_else(|| panic!("{admission:?}"));
        let state = service.agent_loop_state("%1").unwrap().clone();
        service.checkpoint_agent_session_metadata().unwrap();
        let saved = fixture
            .store
            .load_agent_session_metadata(service.session().id.as_str())
            .unwrap();
        assert!(
            saved
                .iter()
                .any(|record| record.conversation_id == CONVERSATION)
        );
        assert!(
            saved.iter().all(|record| record.conversation_id != target
                && record.conversation_id != work.session_id)
        );
        let turn = service.agent_turn_ledger().turn(turn_id).unwrap().clone();
        service
            .complete_running_agent_turn_and_start_ready(
                &turn,
                mez_agent::AgentTurnState::Completed,
                "synthetic_no_store_loop_result",
            )
            .unwrap();
        service
            .restore_agent_loop_parent_conversation("%1", &state, None)
            .unwrap();
        let restored = service.agent_shell_store().get("%1").unwrap();
        assert_eq!(restored.session_id, target);
        assert!(restored.ephemeral);
        assert_eq!(
            restored.ephemeral_transcript_source_conversation_id,
            parent.ephemeral_transcript_source_conversation_id
        );
        assert_eq!(
            restored.ephemeral_transcript_source_entries,
            parent.ephemeral_transcript_source_entries
        );
        assert_eq!(restored.transcript_high_water, parent.transcript_high_water);
        assert_eq!(
            restored.prompt_cache_lineage_id,
            parent.prompt_cache_lineage_id
        );
        assert_eq!(
            service.frozen_replay_binding("%1", &target),
            Some(&original)
        );
        let next = service
            .agent_context_for_pane_prompt("%1", "continue original NoStore parent", 0)
            .unwrap();
        assert_eq!(next.chronology()[0].block().content, "model head 3");
        assert!(
            !fixture
                .root
                .join(&target)
                .join("compaction-epoch.json")
                .exists()
        );
    }
}

/// An original durable receipt cannot mutate a replacement pane binding. The
/// new owner keeps its session/cache identity while the old turn is isolated.
///
/// Raw work also refuses a late source/config/action-scope replacement. Its
/// exact receipt has already committed, so refusal cannot roll durable truth
/// back to the old summary or erase the newly authoritative membership.
#[tokio::test(flavor = "current_thread")]
async fn async_raw_tail_publication_stale_callback_keeps_durable_truth() {
    for change in ["config", "actions", "source", "pane"] {
        let mut case = case_with_mode(false, true).await;
        let attempt = take_attempt(&mut case);
        let completion = tokio::task::spawn_blocking(move || attempt.execute())
            .await
            .unwrap();
        assert!(completion.error().is_none());
        let path = case
            .fixture
            .root
            .join(CONVERSATION)
            .join("compaction-epoch.json");
        let committed = std::fs::read(&path).unwrap();
        match change {
            "config" => {
                let mut layers = case.actor.service.config_layers().to_vec();
                layers[0]
                    .text
                    .push_str("\n# changed after durable publication\n");
                case.actor.service.replace_config_layers(layers).unwrap();
            }
            "actions" | "pane" => {
                let mut replacement = case
                    .actor
                    .service
                    .agent_shell_store()
                    .get("%1")
                    .unwrap()
                    .clone();
                if change == "actions" {
                    replacement.allowed_actions = Some(mez_agent::AllowedActionSet::say_only());
                } else {
                    replacement.session_id =
                        String::from(mez_agent::ContextBlockOccurrenceId::allocate());
                    replacement.running_turn_id = None;
                    replacement.prompt_cache_lineage_id = "replacement-cache-lineage".into();
                }
                case.actor
                    .service
                    .agent_shell_store_mut()
                    .restore_session("%1", replacement)
                    .unwrap();
            }
            "source" => {
                case.actor.service.agent_turn_contexts_mut().insert(
                    case.turn.clone(),
                    AgentContext::new_durable(vec![mez_agent::ContextBlock::user_event(
                        "replacement source",
                        "new unowned body",
                    )])
                    .unwrap(),
                );
            }
            _ => unreachable!(),
        }
        let expected = case
            .actor
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .clone();
        deliver(&mut case, completion.clone()).await;
        assert!(!case.actor.service.agent_is_compacting("%1"), "{change}");
        assert!(
            !case
                .actor
                .service
                .agent_provider_task_is_pending(&case.turn),
            "{change}"
        );
        assert_eq!(
            case.actor
                .service
                .agent_turn_ledger()
                .turn(&case.turn)
                .unwrap()
                .state,
            if change == "source" {
                // Lost semantic ownership cannot authorize terminal bookkeeping.
                // Preserve the explicit refusal instead of inventing retirement.
                mez_agent::AgentTurnState::Blocked
            } else {
                mez_agent::AgentTurnState::Failed
            },
            "{change}"
        );
        let current = case.actor.service.agent_shell_store().get("%1").unwrap();
        assert_eq!(current.session_id, expected.session_id, "{change}");
        assert_eq!(
            current.prompt_cache_lineage_id, expected.prompt_cache_lineage_id,
            "{change}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            committed,
            "stale callback rewrote committed authority: {change}"
        );
        let (_, cold) = case
            .fixture
            .store
            .load_replay_epoch(CONVERSATION, AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(cold.revision().get(), 4);
        assert!(cold.head().is_none());
        assert_eq!(
            cold.head_policy(),
            mez_agent::ReplayHeadPolicy::IntentionalOmission
        );
        deliver(&mut case, completion).await;
        assert_eq!(std::fs::read(&path).unwrap(), committed);
        assert!(!timer(&mut case).unwrap().applied);
    }
}

/// An original durable receipt cannot mutate a replacement pane binding. The
/// new owner keeps its session/cache identity while the old turn is isolated.
#[tokio::test(flavor = "current_thread")]
async fn async_replay_epoch_publication_receipt_cannot_adopt_into_replaced_binding() {
    let mut case = case(false).await;
    let attempt = complete(&mut case).await;
    let completion = tokio::task::spawn_blocking(move || attempt.execute())
        .await
        .unwrap();
    assert!(completion.error().is_none());
    let mut replacement = case
        .actor
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .clone();
    replacement.session_id = String::from(mez_agent::ContextBlockOccurrenceId::allocate());
    replacement.running_turn_id = None;
    let lineage = replacement.prompt_cache_lineage_id.clone();
    let conversation = replacement.session_id.clone();
    case.actor
        .service
        .agent_shell_store_mut()
        .restore_session("%1", replacement)
        .unwrap();
    deliver(&mut case, completion).await;
    let current = case.actor.service.agent_shell_store().get("%1").unwrap();
    assert_eq!(current.session_id, conversation);
    assert_eq!(current.prompt_cache_lineage_id, lineage);
    assert!(current.running_turn_id.is_none());
    assert_eq!(
        case.actor
            .service
            .agent_turn_ledger()
            .turn(&case.turn)
            .unwrap()
            .state,
        mez_agent::AgentTurnState::Failed
    );
}
