//! Inert actor admission tests for steering DISPLAY retries.
//!
//! Construct actor state but never run its loop, timer/persistence workers,
//! native panes, providers or editors. Tests directly collect bounded effects
//! and deliver synthetic timer events; a unique directory contains all fixture IO.

use super::super::*;
use mez_agent::transcript::{SteeringRecoveryReceipt, SteeringRecoveryStatus};

/// Owns isolated service/actor state and its exact temporary directory.
struct Fixture {
    actor: AsyncRuntimeSessionActor,
    root: std::path::PathBuf,
}

impl Fixture {
    /// Restores inert admitted source before constructing an undispatched actor.
    fn new(capacity: usize) -> Self {
        let root = std::env::temp_dir().join(format!("mez-steering-idle-admission-{}",
            crate::storage::token_usage::new_token_usage_event_id()));
        let mut service = crate::test_support::runtime::RuntimeServiceFixture::new()
            .control_socket(root.join("control.sock")).build();
        service.agent_shell_store_mut().enter_or_resume("%1").unwrap();
        let conversation = service.agent_shell_store().get("%1").unwrap().session_id.clone();
        service.restore_steering_recovery("%1", &conversation, &[SteeringRecoveryReceipt {
            id: "actor-display-occurrence".into(), acceptance_order: 1,
            occurrence: Default::default(), turn_id: Some("inert-original-turn".into()),
            event_sequence: Some(1), display: "actor-owned accepted source".into(),
            status: SteeringRecoveryStatus::Admitted(7),
        }]).unwrap();
        service.mark_steering_recovery_published();
        let (_, mut actor) = AsyncRuntimeSessionActor::new(service,
            AsyncRuntimeActorConfig { side_effect_buffer: capacity, ..Default::default() }).unwrap();
        assert!(actor.drain_runtime_side_effects(32).unwrap().is_empty());
        Self { actor, root }
    }

    /// Captures a transcript destination only after adapters are enabled.
    fn attach_store(&mut self) {
        self.actor.service.set_agent_transcript_store(
            crate::storage::transcript::AgentTranscriptStore::new(self.root.join("transcripts")));
    }

    /// Fails one initial source installation without executing input or effects.
    fn fail_initial_display(&mut self) {
        self.actor.service.fail_agent_presentation_install_for_tests(0);
        self.actor.service.request_steering_presentation("%1");
        assert!(self.actor.service.steering_presentation_retry_timer_needed());
    }

    /// Claims the sole tracked timer effect without running a timer worker.
    fn take_idle(&mut self) -> RuntimeTimerKey {
        let effects = self.actor.drain_timer_side_effects(32).unwrap();
        let [RuntimeSideEffect::ScheduleTimer { key, delay_ms }] = effects.as_slice() else {
            panic!("expected exactly one tracked idle wake");
        };
        assert_eq!(key.kind, RuntimeTimerKind::IdleCleanup);
        assert_eq!(*delay_ms, DEFAULT_SHELL_RECOVERY_INTERVAL_MS);
        assert_eq!(self.actor.timers.idle_cleanup.as_ref(), Some(key));
        key.clone()
    }

    /// Delivers one exact synthetic idle callback through ordinary event ingress.
    async fn idle(&mut self, key: RuntimeTimerKey, fail: bool) {
        self.actor.service.make_steering_presentation_retries_due_for_tests();
        if fail { self.actor.service.fail_agent_presentation_install_for_tests(0); }
        self.actor.apply_runtime_event_batch(RuntimeEventBatch {
            events: vec![RuntimeEvent::Timer(TimerEvent { key, now_ms: 1234 })],
        }).await.unwrap();
    }
}

impl Drop for Fixture {
    /// Removes only this fixture's unique directory; no worker was started.
    fn drop(&mut self) { std::fs::remove_dir_all(&self.root).unwrap(); }
}

/// At capacity one, durable work that was admissible before this fix must remain
/// admissible. A first failed display created inside a dirty drain cannot append
/// an optional timer to that mandatory vector and turn it into consumed loss.
/// Retain the obligation, admit one wake after actual capacity progress, and keep
/// ordinary mandatory overflow semantics unchanged without replaying the drain.
#[tokio::test(flavor = "current_thread")]
async fn steering_display_retry_optional_wake_preserves_full_queue_durable_admission() {
    let mut fixture = Fixture::new(1);
    fixture.actor.queue_runtime_side_effects(vec![RuntimeSideEffect::PreparePaneStatusProviders]).unwrap();
    let unrelated = RuntimeTimerKey::new(RuntimeTimerKind::ResizeDebounce, "unrelated", 1);
    assert!(fixture.actor.queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
        key: unrelated, delay_ms: 250,
    }]).is_err());
    assert!(fixture.actor.timers.resize_debounce.is_empty());
    // No store means publication retains dirty evidence without IO. Attaching
    // the store afterward lets the ordinary drain capture mandatory metadata.
    fixture.actor.service.publish_steering_recovery_checkpoint();
    assert!(fixture.actor.service.steering_recovery_needs_publication());
    fixture.attach_store();
    fixture.actor.service.fail_agent_presentation_install_for_tests(0);
    fixture.actor.apply_runtime_event_batch(RuntimeEventBatch::new()).await.unwrap();
    assert!(fixture.actor.service.steering_presentation_retry_timer_needed());
    assert!(fixture.actor.timers.idle_cleanup.is_none());
    let durable = fixture.actor.drain_persistence_side_effects(32).unwrap();
    assert!(matches!(durable.as_slice(), [RuntimeSideEffect::PersistAgentSessionMetadata { .. }]));
    assert!(fixture.actor.timers.idle_cleanup.is_none());
    let released = fixture.actor.drain_status_pill_side_effects(1).unwrap();
    assert!(matches!(released.as_slice(), [RuntimeSideEffect::PreparePaneStatusProviders]));
    for _ in 0..3 { fixture.actor.queue_steering_presentation_retry_idle_wake().unwrap(); }
    let key = fixture.take_idle();
    assert!(fixture.actor.drain_timer_side_effects(32).unwrap().is_empty());
    fixture.idle(key, false).await;
    let published = fixture.actor.drain_persistence_side_effects(32).unwrap();
    assert!(matches!(published.as_slice(), [RuntimeSideEffect::PersistSteeringPresentation { .. }]));
    assert!(!fixture.actor.service.steering_presentation_retry_timer_needed());
    assert!(fixture.actor.timers.idle_cleanup.is_none());
    assert!(fixture.actor.service.agent_turn_ledger().turns().is_empty());
}

/// More failures than any small attempt budget must continue rearming the same
/// tracked idle facility. No explicit request or new input follows the initial
/// failure. A healthy ordinary idle callback installs the original source once,
/// then leaves neither retry ownership nor a new timer behind.
#[tokio::test(flavor = "current_thread")]
async fn steering_display_retry_indefinite_idle_callbacks_eventually_install_once() {
    let mut fixture = Fixture::new(2);
    fixture.attach_store();
    fixture.fail_initial_display();
    fixture.actor.queue_runtime_side_effects(Vec::new()).unwrap();
    let mut previous = 0;
    for _ in 0..12 {
        let key = fixture.take_idle();
        assert!(key.generation > previous);
        previous = key.generation;
        fixture.idle(key, true).await;
        assert!(fixture.actor.service.steering_presentation_retry_timer_needed());
        assert!(fixture.actor.drain_persistence_side_effects(32).unwrap().is_empty());
    }
    let key = fixture.take_idle();
    fixture.idle(key, false).await;
    let effects = fixture.actor.drain_persistence_side_effects(32).unwrap();
    assert!(matches!(effects.as_slice(), [RuntimeSideEffect::PersistSteeringPresentation { .. }]));
    let text = fixture.actor.service.agent_pane_screen("%1").unwrap().normal_content_lines().join("\n");
    assert_eq!(text.matches("user> actor-owned accepted source").count(), 1);
    assert!(fixture.actor.timers.idle_cleanup.is_none());
    assert!(!fixture.actor.service.steering_presentation_retry_timer_needed());
    assert!(fixture.actor.service.agent_turn_ledger().turns().is_empty());
}

/// A stale timer cannot clear the current tracked wake, and retired conversation
/// source cannot install into its replacement. Terminal lifecycle states must not
/// enroll new work even when an old display obligation is still retained.
#[tokio::test(flavor = "current_thread")]
async fn steering_display_retry_idle_fences_stale_callbacks_and_terminal_lifecycle() {
    let mut fixture = Fixture::new(2);
    fixture.fail_initial_display();
    fixture.actor.queue_steering_presentation_retry_idle_wake().unwrap();
    let key = fixture.take_idle();
    let stale = RuntimeTimerKey::new(key.kind, key.owner_id.clone(), key.generation.saturating_add(1));
    fixture.actor.apply_runtime_timer_event(TimerEvent { key: stale, now_ms: 1234 }).unwrap();
    assert_eq!(fixture.actor.timers.idle_cleanup.as_ref(), Some(&key));
    fixture.actor.service.agent_shell_store_mut().start_new_conversation("%1").unwrap();
    fixture.idle(key, false).await;
    assert!(!fixture.actor.service.steering_presentation_retry_timer_needed());
    assert!(fixture.actor.timers.idle_cleanup.is_none());
    for state in [RuntimeLifecycleState::Killed, RuntimeLifecycleState::Failed] {
        let mut stopped = Fixture::new(1);
        stopped.fail_initial_display();
        stopped.actor.service.session_mut_for_tests().set_lifecycle_state(state);
        stopped.actor.queue_steering_presentation_retry_idle_wake().unwrap();
        assert!(stopped.actor.timers.idle_cleanup.is_none());
        assert!(stopped.actor.drain_timer_side_effects(32).unwrap().is_empty());
    }
}
