//! Timer tracking and bounded side-effect queue maintenance.
//!
//! Mandatory effect batches retain their existing all-or-error admission. Steering
//! display recovery publishes only an existing watch notification during enqueue.
//! The typed timer drain separately admits one optional existing idle wake only
//! when conservative nonpersistence capacity allows, after event/request work.
//! A full lane leaves the original display obligation with the service; actual
//! capacity progress retries enrollment without replaying any destructive drain.
//! Tracked timer identity and monotonic generation fence recursive and stale work.

use super::coalesce::{
    async_runtime_current_unix_millis, async_runtime_duration_millis,
    coalesce_output_side_effects_for_enqueue, droppable_repaint_effect,
    pane_io_side_effect_targets_pane, runtime_side_effect_is_droppable_repaint,
    runtime_side_effect_is_durable_persistence, runtime_side_effect_kind_summary,
};
use super::{
    AsyncRuntimeSessionActor, ClientId, DEFAULT_ASYNC_IDLE_CLEANUP_INTERVAL,
    DEFAULT_PANE_PIPE_HEALTH_DELAY_MS, DEFAULT_SHELL_RECOVERY_INTERVAL_MS, MezError,
    RenderInvalidationReason, Result, RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind,
};

/// Message prefix of the retryable side-effect-queue-full condition.
pub(super) const SIDE_EFFECT_QUEUE_FULL_PREFIX: &str = "async runtime side-effect queue is full";

/// Returns whether one actor error is the retryable queue-full condition.
///
/// Producers may retry this condition briefly: the queue drains as workers
/// claim effects, so a transient backlog resolves without failing a pane
/// worker or supervised service.
pub(super) fn is_side_effect_queue_full_error(error: &MezError) -> bool {
    error.message().starts_with(SIDE_EFFECT_QUEUE_FULL_PREFIX)
}

/// Returns whether a queue-full error is safe for a producer to retry.
///
/// Only an error raised before any event of the submitted chunk was applied may
/// be retried: re-submitting a partially applied chunk would re-apply committed
/// work, so the event-application path tags queue-full failures with how many
/// events it had already applied. Failures raised after the batch consumed
/// destructive service-side drains or surrendered a raw committed vector carry
/// an extra `consumed=1` marker, even without a durable transfer. That marker
/// forbids retry regardless of whether an outer event wrapper appends `applied=0`
/// after it.
pub(super) fn is_retryable_side_effect_queue_full_error(error: &MezError) -> bool {
    is_side_effect_queue_full_error(error)
        && error.message().ends_with("applied=0")
        && !error.message().contains(" consumed=1")
}

/// Multiplier applied to the configured lane capacity for the committed reserve.
const SIDE_EFFECT_OVERSHOOT_BUFFER_MULTIPLE: usize = 4;

/// Floor of the committed reserve so a small lane still has usable headroom.
///
/// This is a reserve policy, not a statement that any particular committed batch
/// fits: a batch larger than capacity plus the reserve is still refused.
const SIDE_EFFECT_OVERSHOOT_FLOOR: usize = 16;

impl AsyncRuntimeSessionActor {
    /// Reconciles the daily saved-session retention maintenance timer.
    pub(super) fn saved_session_retention_timer_side_effects(
        &self,
        generation_base_ms: u64,
    ) -> Vec<RuntimeSideEffect> {
        const RETENTION_INTERVAL_MS: u64 = 24 * 60 * 60 * 1_000;
        if self.timers.saved_session_retention.is_some() {
            return Vec::new();
        }
        let generation = generation_base_ms.saturating_add(RETENTION_INTERVAL_MS);
        vec![RuntimeSideEffect::ScheduleTimer {
            key: RuntimeTimerKey::new(
                RuntimeTimerKind::SavedSessionRetention,
                "saved-sessions",
                generation,
            ),
            delay_ms: RETENTION_INTERVAL_MS,
        }]
    }

    /// Applies timer scheduling bookkeeping in emitted side-effect order.
    ///
    /// A cancellation followed by a schedule for the same generation must
    /// leave that timer active, while a schedule followed by cancellation must
    /// remove it. Keeping this ordering here mirrors the timer worker contract.
    pub(super) fn track_runtime_timer_side_effect(&mut self, effect: &RuntimeSideEffect) {
        let (key, scheduled) = match effect {
            RuntimeSideEffect::ScheduleTimer { key, .. } => (key, true),
            RuntimeSideEffect::CancelTimer { key } => (key, false),
            _ => return,
        };
        match key.kind {
            RuntimeTimerKind::ShellTransaction
            | RuntimeTimerKind::ReadinessProbe
            | RuntimeTimerKind::Bootstrap
            | RuntimeTimerKind::PathResolution
            | RuntimeTimerKind::FocusedShellHook => {
                if scheduled {
                    self.timers.shell_transactions.insert(key.clone());
                } else {
                    self.timers.shell_transactions.remove(key);
                }
            }
            RuntimeTimerKind::IdleCleanup => {
                if scheduled {
                    self.timers.last_idle_cleanup_generation =
                        self.timers.last_idle_cleanup_generation.max(key.generation);
                    self.timers.idle_cleanup = Some(key.clone());
                } else if self.timers.idle_cleanup.as_ref() == Some(key) {
                    self.timers.idle_cleanup = None;
                }
            }
            RuntimeTimerKind::SavedSessionRetention => {
                if scheduled {
                    self.timers.saved_session_retention = Some(key.clone());
                } else if self.timers.saved_session_retention.as_ref() == Some(key) {
                    self.timers.saved_session_retention = None;
                }
            }
            RuntimeTimerKind::ResizeDebounce => {
                Self::track_owned_timer_key(&mut self.timers.resize_debounce, key, scheduled);
            }
            RuntimeTimerKind::CursorBlink => {
                Self::track_owned_timer_key(&mut self.timers.cursor_blink, key, scheduled);
            }
            RuntimeTimerKind::StatusRefresh => {
                Self::track_owned_timer_key(&mut self.timers.status_refresh, key, scheduled);
            }
            RuntimeTimerKind::ProviderPoll => {
                if scheduled {
                    self.timers.provider_poll = Some(key.clone());
                } else if self.timers.provider_poll.as_ref() == Some(key) {
                    self.timers.provider_poll = None;
                }
            }
            RuntimeTimerKind::PeerMessageDelivery => {
                if scheduled {
                    self.timers.peer_message_delivery = Some(key.clone());
                } else if self.timers.peer_message_delivery.as_ref() == Some(key) {
                    self.timers.peer_message_delivery = None;
                }
            }
            RuntimeTimerKind::ProviderRetry => {
                Self::track_owned_timer_key(&mut self.timers.provider_retry, key, scheduled);
            }
            RuntimeTimerKind::ProviderClaim => {
                Self::track_owned_timer_key(&mut self.timers.provider_claim, key, scheduled);
            }
            RuntimeTimerKind::CompactionClaim => {
                if scheduled {
                    self.timers.compaction_claim.insert(key.clone());
                } else {
                    self.timers.compaction_claim.remove(key);
                }
            }
            RuntimeTimerKind::CompactionRetry => {
                if scheduled {
                    self.timers.compaction_retry.insert(key.clone());
                } else {
                    self.timers.compaction_retry.remove(key);
                }
            }
            RuntimeTimerKind::ProviderPersistence => {
                Self::track_owned_timer_key(&mut self.timers.provider_persistence, key, scheduled);
            }
            RuntimeTimerKind::PanePipeHealth => {
                Self::track_owned_timer_key(&mut self.timers.pane_pipe_health, key, scheduled);
            }
            RuntimeTimerKind::SynchronizedOutput => {
                Self::track_owned_timer_key(&mut self.timers.synchronized_output, key, scheduled);
            }
        }
    }

    /// Updates one owner-keyed timer generation without discarding effect order.
    pub(super) fn track_owned_timer_key(
        timers: &mut std::collections::HashMap<String, RuntimeTimerKey>,
        key: &RuntimeTimerKey,
        scheduled: bool,
    ) {
        if scheduled {
            timers.insert(key.owner_id.clone(), key.clone());
        } else if timers.get(key.owner_id.as_str()) == Some(key) {
            timers.remove(key.owner_id.as_str());
        }
    }

    /// Runs the queue runtime side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn queue_runtime_side_effects(
        &mut self,
        side_effects: Vec<RuntimeSideEffect>,
    ) -> Result<()> {
        let mut side_effects = side_effects;
        self.try_queue_runtime_side_effects_with_allowance(&mut side_effects, 0)
            .map(|_| ())
    }

    /// Enqueues the effects one applied event produced.
    ///
    /// Whole-batch provisional provider presentation is reconstructible, so it
    /// keeps the strict producer budget first: repaint pressure must still evict
    /// pixels instead of growing the lane, and an eviction-only enqueue is still
    /// reported as admitted. A refusal there falls through to committed
    /// admission, because the event is already applied and those effects have no
    /// other owner to retry from.
    pub(super) fn admit_applied_event_side_effects(
        &mut self,
        side_effects: Vec<RuntimeSideEffect>,
        provisional_presentation: bool,
    ) -> Result<()> {
        let mut side_effects = side_effects;
        if provisional_presentation {
            match self.try_queue_runtime_side_effects_with_allowance(&mut side_effects, 0) {
                Ok(enqueued) => {
                    self.record_reserve_admission(enqueued);
                    return Ok(());
                }
                Err(error) if !is_side_effect_queue_full_error(&error) => return Err(error),
                Err(_) => {}
            }
        }
        self.admit_committed_side_effects(side_effects)
    }

    /// Admits side effects whose originating event batch already committed them.
    ///
    /// Event application is irreversible: once a batch has mutated runtime state
    /// its effects have no other owner to retry from, so a refusal must not drop
    /// them. These effects are admitted through ordinary lane routing within a
    /// bounded overshoot of the configured capacity instead of being retained
    /// outside it: timer ownership, dispatch deduplication, persistence
    /// coalescing and receipt enrollment all have to observe the effect when it
    /// is queued, in its original order relative to every later admission.
    ///
    /// The overshoot keeps transient admission pressure from destroying
    /// committed work without becoming unbounded buffering; beyond it the
    /// refusal is reported truthfully to the caller. On transient refusal,
    /// independently capacity-exempt durable records still enter their normal
    /// persistence lane in original order. Every capacity refusal is consumed:
    /// the caller surrendered the original vector even if no durable record
    /// transferred and the originating event reported zero application. This
    /// marker forbids replay; it does not claim recovery of refused raw effects.
    pub(super) fn admit_committed_side_effects(
        &mut self,
        side_effects: Vec<RuntimeSideEffect>,
    ) -> Result<()> {
        let mut side_effects = side_effects;
        let admission = self.try_queue_runtime_side_effects_with_allowance(
            &mut side_effects,
            self.committed_side_effect_overshoot(),
        );
        match admission {
            Ok(enqueued) => {
                self.record_reserve_admission(enqueued);
                Ok(())
            }
            Err(error) if is_side_effect_queue_full_error(&error) => {
                // The failed attempt leaves original unadmitted records here.
                // Only committed callers may split this vector: producers keep
                // their all-or-error contract, including their durable work.
                side_effects.retain(runtime_side_effect_is_durable_persistence);
                if !side_effects.is_empty() {
                    let enqueued = self.try_queue_runtime_side_effects_with_allowance(
                        &mut side_effects,
                        self.committed_side_effect_overshoot(),
                    )?;
                    self.record_reserve_admission(enqueued);
                }
                // Even an entirely transient vector has surrendered custody at
                // this boundary. A zero-applied wrapper cannot prove the event
                // or its side-effect-producing bookkeeping is safe to repeat.
                Err(MezError::invalid_state(format!(
                    "{} consumed=1",
                    error.message()
                )))
            }
            Err(error) => Err(error),
        }
    }

    /// Counts one committed admission that added work while above nominal.
    ///
    /// `enqueued` is the post-coalescing count the admission actually installed,
    /// including compensation redraws, so a coalescing-only or fully evicted
    /// admission never counts. The comparison uses the same transient budget as
    /// the admission verdict: durable persistence has its own lane and never
    /// registers as reserve hold. Cardinality is one increment per admission
    /// call, not per queued effect, and the counter intentionally reports
    /// "committed work admitted while the transient lane is above its configured
    /// capacity" rather than a per-batch proof that the reserve was decisive.
    fn record_reserve_admission(&mut self, enqueued: usize) {
        if enqueued == 0 {
            return;
        }
        if self
            .side_effects
            .len()
            .saturating_add(self.side_effect_routes.non_persistence_len())
            > self.side_effect_buffer
        {
            self.metrics.runtime_side_effects_admitted_over_capacity = self
                .metrics
                .runtime_side_effects_admitted_over_capacity
                .saturating_add(1);
        }
    }

    /// Bounded reserve allowed for effects a committed batch already produced.
    ///
    /// The reserve scales with the configured lane capacity and keeps a floor so
    /// a small lane still has usable headroom. It is the only part of admission
    /// that may exceed the configured capacity, producer-submitted work never
    /// uses it, and it absorbs bounded pressure rather than guaranteeing that a
    /// particular batch size fits.
    pub(super) fn committed_side_effect_overshoot(&self) -> usize {
        self.side_effect_buffer
            .saturating_mul(SIDE_EFFECT_OVERSHOOT_BUFFER_MULTIPLE)
            .max(SIDE_EFFECT_OVERSHOOT_FLOOR)
    }

    /// Reserves a fitting service-transfer prefix at the serialized actor boundary.
    ///
    /// No actor mutation can interleave extraction and admission. Counting every
    /// existing transient effect is conservative: coalescing/eviction can free
    /// more room but cannot make this prefix exceed the finite committed budget.
    pub(super) fn available_committed_side_effect_capacity(&self) -> usize {
        self.side_effect_buffer
            .saturating_add(self.committed_side_effect_overshoot())
            .saturating_sub(self.side_effects.len())
            .saturating_sub(self.side_effect_routes.non_persistence_len())
    }

    /// Runs one admission attempt with an explicit capacity allowance.
    ///
    /// `allowance` extends the capacity verdict only for work the caller already
    /// owns irreversibly; producer-submitted work passes zero.
    fn try_queue_runtime_side_effects_with_allowance(
        &mut self,
        side_effects: &mut Vec<RuntimeSideEffect>,
        allowance: usize,
    ) -> Result<usize> {
        let capacity = self.side_effect_buffer.saturating_add(allowance);
        // Divider reconciliation consumes pending divider gesture state in its
        // superseded branch, and a rejected enqueue must not lose that work, so
        // only the non-destructive filter runs before the capacity verdict. The
        // matching commit runs on the admitted path below, where the effects it
        // merges are admitted outside the capacity budget exactly like the
        // compensation redraws.
        let divider_superseded = self
            .service
            .pending_divider_render_effects_are_superseded(side_effects);
        if !divider_superseded {
            self.service
                .filter_pending_divider_render_effects(side_effects);
        }
        let mut queued_side_effects = self.side_effects.drain(..).collect::<Vec<_>>();
        let queued_divider_superseded = self
            .service
            .pending_divider_render_effects_are_superseded(&queued_side_effects);
        if !queued_divider_superseded {
            self.service
                .filter_pending_divider_render_effects(&mut queued_side_effects);
        }
        self.side_effects.extend(queued_side_effects);
        let terminal_config_invalidated = side_effects
            .iter()
            .any(runtime_side_effect_invalidates_terminal_config);
        let (mut incoming, coalesced) = coalesce_output_side_effects_for_enqueue(
            &mut self.side_effects,
            &mut self.side_effect_routes,
            std::mem::take(side_effects),
        );
        // Repaint work is level-triggered, so a transient backlog drops the
        // oldest droppable repaint effect - queued work first, then incoming
        // work when non-droppable queued work already consumes capacity -
        // instead of failing the producer. The capacity budget covers only the
        // caller's work: compensation redraws are admitted outside it because
        // they are droppable repaint effects bounded by attached clients, and
        // dropping them would strand the pixels the eviction invalidated.
        let mut owed_full_redraws: Vec<ClientId> = Vec::new();
        let mut evicted_render_clients = 0usize;
        let mut evicted_flush_outputs = 0usize;
        // A durable-only (or fully coalesced) admission needs no transient
        // capacity, even when owed compensation redraws exceed the reserve.
        // Recompute after eviction too: dropping the last optional repaint
        // must not turn its accompanying durable write into a refused batch.
        let mut over_capacity = incoming
            .iter()
            .any(|effect| !runtime_side_effect_is_durable_persistence(effect))
            && self
                .side_effects
                .len()
                .saturating_add(self.side_effect_routes.non_persistence_len())
                .saturating_add(
                    incoming
                        .iter()
                        .filter(|effect| !runtime_side_effect_is_durable_persistence(effect))
                        .count(),
                )
                > capacity;
        while over_capacity {
            let dropped = match self
                .side_effects
                .iter()
                .position(runtime_side_effect_is_droppable_repaint)
            {
                Some(position) => self
                    .side_effects
                    .remove(position)
                    .expect("queued repaint position is valid by construction"),
                None => match self.side_effect_routes.pop_droppable_repaint() {
                    Some(effect) => effect,
                    None => match incoming
                        .iter()
                        .position(runtime_side_effect_is_droppable_repaint)
                    {
                        Some(position) => incoming.remove(position),
                        None => break,
                    },
                },
            };
            match droppable_repaint_effect(&dropped) {
                Some(super::coalesce::DroppableRepaintEffect::RenderClient(client_id)) => {
                    if !owed_full_redraws.contains(&client_id) {
                        owed_full_redraws.push(client_id);
                    }
                    evicted_render_clients = evicted_render_clients.saturating_add(1);
                }
                Some(super::coalesce::DroppableRepaintEffect::FlushClientOutput(client_id)) => {
                    if !owed_full_redraws.contains(&client_id) {
                        owed_full_redraws.push(client_id);
                    }
                    evicted_flush_outputs = evicted_flush_outputs.saturating_add(1);
                }
                None => break,
            }
            over_capacity = incoming
                .iter()
                .any(|effect| !runtime_side_effect_is_durable_persistence(effect))
                && self
                    .side_effects
                    .len()
                    .saturating_add(self.side_effect_routes.non_persistence_len())
                    .saturating_add(
                        incoming
                            .iter()
                            .filter(|effect| !runtime_side_effect_is_durable_persistence(effect))
                            .count(),
                    )
                    > capacity;
        }
        let evicted_repaint_effects = evicted_render_clients.saturating_add(evicted_flush_outputs);
        if evicted_repaint_effects > 0 {
            self.metrics.runtime_side_effects_evicted = self
                .metrics
                .runtime_side_effects_evicted
                .saturating_add(u64::try_from(evicted_repaint_effects).unwrap_or(u64::MAX));
            self.metrics.render_client_side_effects_evicted = self
                .metrics
                .render_client_side_effects_evicted
                .saturating_add(u64::try_from(evicted_render_clients).unwrap_or(u64::MAX));
            self.metrics.flush_client_output_side_effects_evicted = self
                .metrics
                .flush_client_output_side_effects_evicted
                .saturating_add(u64::try_from(evicted_flush_outputs).unwrap_or(u64::MAX));
        }
        let compensation_redraws = owed_full_redraws.len();
        for client_id in owed_full_redraws {
            // Any other queued or incoming repaint request for this client is
            // merged with this one by the actor's render-invalidation drain, so
            // each affected client owes exactly one coalesced full redraw. This
            // runs before the capacity verdict so an eviction is never left
            // uncompensated, even when non-droppable work still overflows.
            self.enqueue_runtime_side_effect(RuntimeSideEffect::RenderClient {
                client_id,
                reason: RenderInvalidationReason::FullRedraw,
            });
        }
        if over_capacity {
            if compensation_redraws > 0 {
                // The compensation admitted above is real queued work: count and
                // notify it even though the caller's non-droppable overflow
                // fails, so the owed redraw cannot wait for an unrelated wakeup.
                self.metrics.runtime_side_effects_queued = self
                    .metrics
                    .runtime_side_effects_queued
                    .saturating_add(u64::try_from(compensation_redraws).unwrap_or(u64::MAX));
                self.metrics.side_effect_queue_depth = self
                    .side_effects
                    .len()
                    .saturating_add(self.side_effect_routes.len());
                self.metrics.side_effect_queue_high_water = self
                    .metrics
                    .side_effect_queue_high_water
                    .max(self.metrics.side_effect_queue_depth);
                self.notify_side_effect_delivery();
            }
            // Leave the un-admitted work with the caller so a committed-effect
            // admission can retry it under its bounded allowance.
            *side_effects = std::mem::take(&mut incoming);
            return Err(MezError::invalid_state(format!(
                "{SIDE_EFFECT_QUEUE_FULL_PREFIX}: queued={} incoming={} capacity={} allowance={} queued_kinds={} incoming_kinds={}",
                self.side_effects
                    .len()
                    .saturating_add(self.side_effect_routes.len()),
                side_effects.len(),
                capacity,
                allowance,
                if self.side_effects.is_empty() {
                    self.side_effect_routes.kind_summary()
                } else if self.side_effect_routes.is_empty() {
                    runtime_side_effect_kind_summary(self.side_effects.iter())
                } else {
                    format!(
                        "{},{}",
                        runtime_side_effect_kind_summary(self.side_effects.iter()),
                        self.side_effect_routes.kind_summary()
                    )
                },
                runtime_side_effect_kind_summary(side_effects.iter())
            )));
        }
        // The enqueue is admitted: consume the pending divider gesture state the
        // probes above deliberately left untouched.
        if divider_superseded {
            self.service
                .commit_pending_divider_render_effects(&mut incoming);
        }
        if queued_divider_superseded {
            let mut queued_committed = self.side_effects.drain(..).collect::<Vec<_>>();
            self.service
                .commit_pending_divider_render_effects(&mut queued_committed);
            self.side_effects.extend(queued_committed);
        }
        if terminal_config_invalidated {
            self.terminal_config_generation = self.terminal_config_generation.wrapping_add(1);
            let _ = self
                .terminal_config_generation_tx
                .send(self.terminal_config_generation);
        }
        let timer_schedules = incoming
            .iter()
            .filter(|effect| matches!(effect, RuntimeSideEffect::ScheduleTimer { .. }))
            .count();
        let timer_cancellations = incoming
            .iter()
            .filter(|effect| matches!(effect, RuntimeSideEffect::CancelTimer { .. }))
            .count();
        let pane_processes = incoming
            .iter()
            .filter_map(|effect| match effect {
                RuntimeSideEffect::PaneProcessIo { instance, .. } => Some(instance.clone()),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        let has_non_pane_process_effect = incoming
            .iter()
            .any(|effect| !matches!(effect, RuntimeSideEffect::PaneProcessIo { .. }));
        // Compensation redraws count as queued work and must notify the
        // delivery chain: an eviction-only enqueue adds no incoming effect, so
        // otherwise the redraw could wait for an unrelated notification.
        let queued = incoming.len().saturating_add(compensation_redraws);
        let should_notify = queued > 0;
        for effect in &incoming {
            self.track_runtime_timer_side_effect(effect);
        }
        for effect in incoming {
            self.enqueue_runtime_side_effect(effect);
        }
        if self.side_effect_queue_nonempty_since.is_none()
            && (!self.side_effects.is_empty() || !self.side_effect_routes.is_empty())
        {
            self.side_effect_queue_nonempty_since = Some(std::time::Instant::now());
        }
        if should_notify {
            self.metrics.runtime_side_effects_queued = self
                .metrics
                .runtime_side_effects_queued
                .saturating_add(u64::try_from(queued).unwrap_or(u64::MAX));
            self.metrics.runtime_timer_schedules_queued = self
                .metrics
                .runtime_timer_schedules_queued
                .saturating_add(u64::try_from(timer_schedules).unwrap_or(u64::MAX));
            self.metrics.runtime_timer_cancellations_queued = self
                .metrics
                .runtime_timer_cancellations_queued
                .saturating_add(u64::try_from(timer_cancellations).unwrap_or(u64::MAX));
        }
        self.metrics
            .runtime_side_effect_enqueue_sizes
            .record(u64::try_from(queued).unwrap_or(u64::MAX));
        self.metrics.render_invalidations_coalesced = self
            .metrics
            .render_invalidations_coalesced
            .saturating_add(u64::try_from(coalesced).unwrap_or(u64::MAX));
        self.metrics.side_effect_queue_depth = self
            .side_effects
            .len()
            .saturating_add(self.side_effect_routes.len());
        self.metrics.side_effect_queue_high_water = self
            .metrics
            .side_effect_queue_high_water
            .max(self.metrics.side_effect_queue_depth);
        self.metrics
            .side_effect_queue_depth_samples
            .record(u64::try_from(self.metrics.side_effect_queue_depth).unwrap_or(u64::MAX));
        if should_notify && (has_non_pane_process_effect || compensation_redraws > 0) {
            self.notify_side_effect_delivery();
        }
        for instance in pane_processes {
            self.notify_pane_process_side_effect_delivery(&instance);
        }
        self.notify_steering_presentation_retry_idle_wake();
        Ok(queued)
    }

    /// Publishes a level-triggered timer-worker wake without consuming capacity.
    /// Every filtered worker has its own watch revision; an initially unsubscribed
    /// timer worker also performs a bounded idle poll before claiming timer work.
    /// Empty zero-progress timer drains never call this notification path.
    pub(super) fn notify_steering_presentation_retry_idle_wake(&mut self) {
        if self.steering_presentation_retry_idle_wake_needed() {
            self.notify_side_effect_delivery();
        }
    }

    /// Checks retained display authority and the existing live timer/lifecycle fences.
    fn steering_presentation_retry_idle_wake_needed(&self) -> bool {
        self.service.steering_presentation_retry_timer_needed()
            && self.timers.idle_cleanup.is_none()
            && !matches!(
                self.service.lifecycle_state(),
                crate::runtime::RuntimeLifecycleState::Killed
                    | crate::runtime::RuntimeLifecycleState::Failed
            )
    }

    /// Separately admits one display wake at a typed timer/capacity drain boundary.
    /// A full queue retains the original service obligation; real capacity
    /// progress retries enrollment. Never mix this optional timer into a taken
    /// persistence batch or bypass capacity. The serialized preflight has no
    /// await, and successful tracking prevents recursive/duplicate enrollment.
    /// Enrollment is best effort: an exhausted generation or rejected enqueue
    /// leaves every already-taken mandatory batch intact and the retained display
    /// obligation owned for the next real capacity-progress drain.
    pub(super) fn queue_steering_presentation_retry_idle_wake(&mut self) {
        if !self.steering_presentation_retry_idle_wake_needed() {
            return;
        }
        // Match normal queue admission's conservative nonpersistence occupancy.
        // Even if coalescing could free space, defer optional work instead of
        // risking the already-admitted mandatory batch's success contract.
        if self
            .side_effects
            .len()
            .saturating_add(self.side_effect_routes.non_persistence_len())
            >= self.side_effect_buffer
        {
            return;
        }
        let Some(generation) = self.timers.last_idle_cleanup_generation.checked_add(1) else {
            // Exhausted generation space cannot be recovered at runtime. The
            // retained service obligation stays owned, and no drained batch is
            // failed, replayed or silently acknowledged by this optional path.
            return;
        };
        let generation = generation.max(async_runtime_current_unix_millis());
        let _ = self.queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
            key: RuntimeTimerKey::new(RuntimeTimerKind::IdleCleanup, "session", generation),
            delay_ms: DEFAULT_SHELL_RECOVERY_INTERVAL_MS,
        }]);
    }

    /// Enqueues one runtime side effect, preserving priority pane input order.
    pub(super) fn enqueue_runtime_side_effect(&mut self, effect: RuntimeSideEffect) {
        if let RuntimeSideEffect::PaneProcessIo {
            instance,
            effect: crate::runtime::PaneProcessIoEffect::CancelShellInput { delivery_id },
        } = &effect
        {
            self.side_effects.retain(|queued| {
                !matches!(
                    queued,
                    RuntimeSideEffect::PaneProcessIo {
                        instance: queued_instance,
                        effect: crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery },
                    } if queued_instance == instance
                        && delivery.delivery_id.as_deref() == Some(delivery_id.as_str())
                )
            });
            let instance = instance.clone();
            let delivery_id = delivery_id.clone();
            self.side_effect_routes.cancel_pane_process_shell_input(
                &instance,
                &delivery_id,
                effect,
            );
            return;
        }
        if matches!(effect, RuntimeSideEffect::ReadHostClipboard { .. }) {
            self.side_effect_routes.push_clipboard(effect);
            return;
        }
        if matches!(effect, RuntimeSideEffect::RunProgramHook { .. }) {
            self.side_effect_routes.push_hook(effect);
            return;
        }
        if matches!(
            effect,
            RuntimeSideEffect::Persist { .. }
                | RuntimeSideEffect::PersistAuditLog { .. }
                | RuntimeSideEffect::PersistTranscriptEntries { .. }
                | RuntimeSideEffect::PersistAgentSessionMetadata { .. }
                | RuntimeSideEffect::PersistPresentationEntries { .. }
                | RuntimeSideEffect::PersistSteeringPresentation { .. }
                | RuntimeSideEffect::PersistSessionArchive { .. }
                | RuntimeSideEffect::PersistSavedSessionRetention { .. }
                | RuntimeSideEffect::PersistPromptHistory { .. }
                | RuntimeSideEffect::PersistCommandPromptHistory { .. }
                | RuntimeSideEffect::PersistTokenUsage { .. }
                | RuntimeSideEffect::SettleAgentProviderPersistence { .. }
                | RuntimeSideEffect::PersistRegistry { .. }
        ) {
            self.side_effect_routes.push_persistence(effect);
            return;
        }
        if matches!(
            effect,
            RuntimeSideEffect::RefreshStatusPill { .. }
                | RuntimeSideEffect::PreparePaneStatusProviders
                | RuntimeSideEffect::RefreshPaneStatusProvider { .. }
        ) {
            self.side_effect_routes.push_status(effect);
            return;
        }
        if matches!(
            effect,
            RuntimeSideEffect::ScheduleTimer { .. } | RuntimeSideEffect::CancelTimer { .. }
        ) {
            self.side_effect_routes.push_timer(effect);
            return;
        }
        if matches!(effect, RuntimeSideEffect::DispatchAgentCommand { .. }) {
            self.side_effect_routes.push_command(effect);
            return;
        }
        if matches!(
            effect,
            RuntimeSideEffect::DispatchAgentProvider { .. }
                | RuntimeSideEffect::DispatchApprovedExternalAction { .. }
                | RuntimeSideEffect::DispatchNativeShellAction { .. }
                | RuntimeSideEffect::DispatchRecordBrowserRefresh { .. }
                | RuntimeSideEffect::DispatchAgentCompaction { .. }
                | RuntimeSideEffect::DispatchAgentRemember { .. }
                | RuntimeSideEffect::DispatchAgentSessionTitle { .. }
                | RuntimeSideEffect::DispatchAgentPresentationResize { .. }
        ) {
            self.side_effect_routes.push_provider(effect);
            return;
        }
        if matches!(effect, RuntimeSideEffect::PaneProcessIo { .. }) {
            self.side_effect_routes.push_pane_process(effect);
            return;
        }
        if let RuntimeSideEffect::RenderClient { client_id, reason } = effect {
            self.side_effect_routes.push_render(client_id, reason);
            return;
        }
        if matches!(effect, RuntimeSideEffect::FlushClientOutput { .. }) {
            self.side_effect_routes.push_flush(effect);
            return;
        }
        let priority_pane_id = match &effect {
            RuntimeSideEffect::WritePaneInputPriority { pane_id, .. } => Some(pane_id.as_str()),
            RuntimeSideEffect::WritePaneShellInput { pane_id, delivery } if delivery.priority => {
                Some(pane_id.as_str())
            }
            RuntimeSideEffect::PaneProcessIo {
                instance,
                effect: crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery },
            } if delivery.priority => Some(instance.pane_id.as_str()),
            _ => None,
        };
        match priority_pane_id {
            Some(pane_id) => {
                let insert_at = self
                    .side_effects
                    .iter()
                    .position(|queued| pane_io_side_effect_targets_pane(queued, pane_id))
                    .unwrap_or(self.side_effects.len());
                self.side_effects.insert(insert_at, effect);
            }
            None => self.side_effects.push_back(effect),
        }
    }

    /// Runs the queue shell transaction timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn queue_shell_transaction_timer_side_effects(&mut self) -> Result<usize> {
        let side_effects = self.shell_transaction_timer_side_effects();
        let queued = side_effects.len();
        self.queue_runtime_side_effects(side_effects)?;
        Ok(queued)
    }

    /// Reconciles shell lifecycle timers after a direct actor request mutates
    /// transaction or agent-subshell state outside runtime-event ingress.
    pub(super) fn queue_shell_lifecycle_timer_side_effects(&mut self) -> Result<usize> {
        let mut side_effects = self.cancel_stale_shell_transaction_timer_side_effects();
        side_effects.extend(self.shell_transaction_timer_side_effects());
        side_effects.extend(self.idle_cleanup_timer_side_effects());
        let queued = side_effects.len();
        self.queue_runtime_side_effects(side_effects)?;
        Ok(queued)
    }

    /// Rearms the one actor-owned resize debounce generation for a client.
    pub(super) fn resize_debounce_timer_side_effects(
        &mut self,
        client_id: &ClientId,
    ) -> Result<Vec<RuntimeSideEffect>> {
        let previous = self
            .timers
            .resize_debounce
            .get(client_id.as_str())
            .cloned()
            .into_iter();
        self.timers.next_resize_debounce_generation = self
            .timers
            .next_resize_debounce_generation
            .saturating_add(1);
        let next_key = RuntimeTimerKey::new(
            RuntimeTimerKind::ResizeDebounce,
            client_id.as_str(),
            self.timers.next_resize_debounce_generation,
        );
        let delay_ms =
            self.service
                .terminal_client_loop_config(
                    crate::host::terminal::TerminalClientLoopConfig::default(),
                )?
                .resize_debounce_ms
                .max(1);
        let mut side_effects = previous
            .map(|key| RuntimeSideEffect::CancelTimer { key })
            .collect::<Vec<_>>();
        side_effects.push(RuntimeSideEffect::ScheduleTimer {
            key: next_key,
            delay_ms,
        });
        Ok(side_effects)
    }

    /// Drains coalesced divider intents into actor-owned timer effects.
    pub(super) fn pending_divider_resize_debounce_timer_side_effects(
        &mut self,
    ) -> Result<Vec<RuntimeSideEffect>> {
        let client_ids = self.service.take_divider_resize_debounce_requests();
        let mut side_effects = Vec::new();
        for client_id in client_ids {
            side_effects.extend(self.resize_debounce_timer_side_effects(&client_id)?);
        }
        Ok(side_effects)
    }

    /// Runs the ensure client render timers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn ensure_client_render_timers(&mut self, client_id: &ClientId) -> Result<usize> {
        let side_effects = self.client_render_timer_side_effects(client_id)?;
        let queued = side_effects.len();
        self.queue_runtime_side_effects(side_effects)?;
        Ok(queued)
    }

    /// Defers timer reconciliation when a pending owner render can recover it.
    ///
    /// An attached-terminal step has already mutated presentation state before
    /// this adapter work runs. If its exact-client render invalidation occupies
    /// the remaining queue capacity, rendering will recompute the same timers
    /// after draining that effect. Deferral is allowed only when removing that
    /// one render creates enough capacity for the complete timer effect batch.
    pub(super) fn ensure_client_render_timers_or_defer_to_pending_render(
        &mut self,
        client_id: &ClientId,
    ) -> Result<usize> {
        let side_effects = self.client_render_timer_side_effects(client_id)?;
        let queued = side_effects.len();
        let pending_owner_render = self.side_effect_routes.has_pending_render(client_id);
        let queued_depth = self
            .side_effects
            .len()
            .saturating_add(self.side_effect_routes.len());
        let exceeds_capacity = queued_depth.saturating_add(queued) > self.side_effect_buffer;
        let fits_after_render = self
            .side_effects
            .len()
            .saturating_add(self.side_effect_routes.len())
            .saturating_sub(usize::from(pending_owner_render))
            .saturating_add(queued)
            <= self.side_effect_buffer;
        if pending_owner_render && exceeds_capacity && fits_after_render {
            return Ok(0);
        }
        self.queue_runtime_side_effects(side_effects)?;
        Ok(queued)
    }

    /// Computes cursor and status timer effects for one exact client.
    fn client_render_timer_side_effects(
        &mut self,
        client_id: &ClientId,
    ) -> Result<Vec<RuntimeSideEffect>> {
        let generation_base_ms = async_runtime_current_unix_millis();
        let mut side_effects = self
            .service
            .client_cursor_blink_timer_transition(
                client_id.as_str(),
                self.timers.cursor_blink.get(client_id.as_str()).cloned(),
                generation_base_ms,
            )?
            .side_effects;
        side_effects.extend(
            self.service
                .client_status_refresh_timer_transition(
                    client_id.as_str(),
                    self.timers.status_refresh.get(client_id.as_str()).cloned(),
                    generation_base_ms,
                )?
                .side_effects,
        );
        Ok(side_effects)
    }

    /// Runs the shell transaction timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn shell_transaction_timer_side_effects(&mut self) -> Vec<RuntimeSideEffect> {
        self.service
            .shell_transaction_timer_transition(
                &self.timers.shell_transactions,
                async_runtime_current_unix_millis(),
            )
            .side_effects
            .into_iter()
            .filter(|effect| matches!(effect, RuntimeSideEffect::ScheduleTimer { .. }))
            .collect()
    }

    /// Runs the cancel stale shell transaction timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn cancel_stale_shell_transaction_timer_side_effects(
        &mut self,
    ) -> Vec<RuntimeSideEffect> {
        self.service
            .shell_transaction_timer_transition(
                &self.timers.shell_transactions,
                async_runtime_current_unix_millis(),
            )
            .side_effects
            .into_iter()
            .filter(|effect| matches!(effect, RuntimeSideEffect::CancelTimer { .. }))
            .collect()
    }

    /// Runs the idle cleanup timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn idle_cleanup_timer_side_effects(&self) -> Vec<RuntimeSideEffect> {
        let actor_progress_turn_ids = self.actor_owned_agent_progress_turn_ids();
        self.idle_cleanup_timer_side_effects_with_actor_progress(&actor_progress_turn_ids)
    }

    /// Returns idle-cleanup timer side effects while honoring actor-owned
    /// progress such as delayed provider retry timers.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns whose progress is represented
    ///   by async actor state rather than service-owned queues.
    pub(super) fn idle_cleanup_timer_side_effects_with_actor_progress(
        &self,
        actor_progress_turn_ids: &std::collections::BTreeSet<String>,
    ) -> Vec<RuntimeSideEffect> {
        self.service
            .idle_cleanup_timer_transition_with_actor_progress(
                actor_progress_turn_ids,
                self.timers.idle_cleanup.is_some(),
                async_runtime_current_unix_millis(),
                async_runtime_duration_millis(DEFAULT_ASYNC_IDLE_CLEANUP_INTERVAL),
                DEFAULT_SHELL_RECOVERY_INTERVAL_MS,
            )
            .side_effects
    }

    /// Runs the cursor blink timer side effects for client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn cursor_blink_timer_side_effects_for_client(
        &mut self,
        client_id: &str,
        generation_base_ms: u64,
    ) -> Result<Vec<RuntimeSideEffect>> {
        Ok(self
            .service
            .client_cursor_blink_timer_transition(
                client_id,
                self.timers.cursor_blink.get(client_id).cloned(),
                generation_base_ms,
            )?
            .side_effects)
    }

    /// Runs the status refresh timer side effects for client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn status_refresh_timer_side_effects_for_client(
        &mut self,
        client_id: &str,
        generation_base_ms: u64,
    ) -> Result<Vec<RuntimeSideEffect>> {
        Ok(self
            .service
            .client_status_refresh_timer_transition(
                client_id,
                self.timers.status_refresh.get(client_id).cloned(),
                generation_base_ms,
            )?
            .side_effects)
    }

    /// Reconciles every actor-owned status timer after shared lifecycle cleanup.
    pub(super) fn status_refresh_timer_reconciliation_side_effects(
        &mut self,
        generation_base_ms: u64,
    ) -> Result<Vec<RuntimeSideEffect>> {
        let client_ids = self
            .timers
            .status_refresh
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut side_effects = Vec::new();
        for client_id in client_ids {
            side_effects.extend(
                self.status_refresh_timer_side_effects_for_client(&client_id, generation_base_ms)?,
            );
        }
        Ok(side_effects)
    }

    /// Runs the pane pipe health timer side effects for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn pane_pipe_health_timer_side_effects_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Result<Vec<RuntimeSideEffect>> {
        let next_generation = self
            .timers
            .next_pane_pipe_health_generation
            .saturating_add(1);
        let transition = self.service.pane_pipe_health_timer_transition(
            pane_id,
            self.timers.pane_pipe_health.get(pane_id).cloned(),
            next_generation,
            DEFAULT_PANE_PIPE_HEALTH_DELAY_MS,
        )?;
        if transition.side_effects.iter().any(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == RuntimeTimerKind::PanePipeHealth
            )
        }) {
            self.timers.next_pane_pipe_health_generation = next_generation;
        }
        Ok(transition.side_effects)
    }

    /// Runs the command pane pipe health timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn command_pane_pipe_health_timer_side_effects(
        &mut self,
    ) -> Result<Vec<RuntimeSideEffect>> {
        let pane_ids = self.service.active_command_pane_pipe_ids();
        let mut side_effects = Vec::new();
        for pane_id in pane_ids {
            side_effects.extend(self.pane_pipe_health_timer_side_effects_for_pane(&pane_id)?);
        }
        Ok(side_effects)
    }

    /// Runs the queue command pane pipe health timer side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn queue_command_pane_pipe_health_timer_side_effects(&mut self) -> Result<usize> {
        let side_effects = self.command_pane_pipe_health_timer_side_effects()?;
        let count = side_effects.len();
        self.queue_runtime_side_effects(side_effects)?;
        Ok(count)
    }
}

/// Returns whether one side effect can change actor-resolved terminal state.
fn runtime_side_effect_invalidates_terminal_config(effect: &RuntimeSideEffect) -> bool {
    matches!(
        effect,
        RuntimeSideEffect::RenderClient { reason, .. }
            if !matches!(
                reason,
                RenderInvalidationReason::CursorBlink | RenderInvalidationReason::StatusLine
            )
    )
}

#[cfg(test)]
mod tests {
    use super::{is_retryable_side_effect_queue_full_error, is_side_effect_queue_full_error};
    use crate::MezError;

    /// Verifies only pre-application, pre-drain queue-full failures are retryable.
    ///
    /// A partial application would be re-applied by a retry, and a failure raised
    /// after destructive service-side drains cannot restore that work, so both
    /// must stay out of the retry gate while an untagged queue-full error remains
    /// the diagnostic producers report.
    #[test]
    fn only_pre_application_queue_full_errors_are_retryable() {
        let queue_full = |suffix: &str| {
            MezError::invalid_state(format!(
                "async runtime side-effect queue is full: queued=2 incoming=1 capacity=2{suffix}"
            ))
        };
        assert!(is_side_effect_queue_full_error(&queue_full("")));
        assert!(is_retryable_side_effect_queue_full_error(&queue_full(
            " applied=0"
        )));
        assert!(!is_retryable_side_effect_queue_full_error(&queue_full(
            " applied=1"
        )));
        assert!(!is_retryable_side_effect_queue_full_error(&queue_full(
            " applied=0 consumed=1"
        )));
        assert!(!is_retryable_side_effect_queue_full_error(&queue_full(
            " consumed=1 applied=0"
        )));
        assert!(!is_retryable_side_effect_queue_full_error(
            &MezError::invalid_state("unrelated failure")
        ));
    }
}
