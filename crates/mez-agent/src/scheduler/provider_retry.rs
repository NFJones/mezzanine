//! Deterministic provider-retry scheduling.
//!
//! This module owns retry attempts and the pure failure/recovery/timer/dispatch
//! state machine. The product runtime interprets returned effects using its
//! clock, timers, provider workers, transcript, and recovery services, then
//! feeds the observed effect result back as another event. No effect is assumed
//! to have succeeded inside this reducer.

use std::collections::BTreeMap;

use crate::{DEFAULT_PROVIDER_RETRY_POLICY, ProviderErrorRetryClass, ProviderRetryPolicy};

/// Product-independent recovery required before a provider retry can wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRetryRecovery {
    /// The same request can be retried after backoff without context mutation.
    None,
    /// The product must apply context-limit recovery before scheduling.
    ContextLimit,
    /// The product must apply output-limit recovery before scheduling.
    OutputLimit,
}

impl ProviderRetryRecovery {
    /// Maps one retry class to its required product recovery effect.
    const fn from_retry_class(retry_class: ProviderErrorRetryClass) -> Option<Self> {
        match retry_class {
            ProviderErrorRetryClass::ContextLimit => Some(Self::ContextLimit),
            ProviderErrorRetryClass::OutputLimit => Some(Self::OutputLimit),
            ProviderErrorRetryClass::RetryableTransport => Some(Self::None),
            ProviderErrorRetryClass::NonRetryable => None,
        }
    }
}

/// Observed result of a product-owned retry recovery effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRetryRecoveryResult {
    /// Recovery and product event recording completed successfully.
    Ready,
    /// Recovery failed and the provider failure should become terminal.
    Failed,
    /// The target turn became unavailable before recovery could be retained.
    TurnUnavailable,
}

/// Observed result of product-owned provider dispatch preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRetryDispatchResult {
    /// The provider request was made ready for dispatch.
    Ready,
    /// The target turn became unavailable before dispatch.
    TurnUnavailable,
}

/// One observed event applied to the provider-retry reducer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderRetryEvent {
    /// A provider request failed with a dependency-neutral retry class.
    FailureObserved {
        /// Stable active-turn identity.
        turn_id: String,
        /// Provider-domain classification of the failure.
        retry_class: ProviderErrorRetryClass,
    },
    /// A provider request failed with normalized timing inputs.
    FailureObservedWithTiming {
        /// Stable active-turn identity.
        turn_id: String,
        /// Provider-domain classification of the failure.
        retry_class: ProviderErrorRetryClass,
        /// Optional provider-advised minimum delay.
        advised_delay_ms: Option<u64>,
        /// Runtime-provided randomness used to jitter local backoff.
        jitter_sample: u64,
    },
    /// Product recovery for one planned attempt completed.
    RecoveryCompleted {
        /// Stable active-turn identity.
        turn_id: String,
        /// One-based retry attempt from the recovery effect.
        attempt: u64,
        /// Observed recovery result.
        result: ProviderRetryRecoveryResult,
    },
    /// The actor clock delivered one retry timer.
    TimerElapsed {
        /// Stable active-turn identity.
        turn_id: String,
        /// One-based retry attempt encoded in the timer generation.
        attempt: u64,
    },
    /// Product dispatch preparation for one elapsed timer completed.
    DispatchCompleted {
        /// Stable active-turn identity.
        turn_id: String,
        /// One-based retry attempt from the dispatch effect.
        attempt: u64,
        /// Observed dispatch-preparation result.
        result: ProviderRetryDispatchResult,
    },
    /// The active turn completed, failed terminally, or was cancelled.
    TurnSettled {
        /// Stable turn identity whose retry state should be removed.
        turn_id: String,
    },
}

/// External effect requested by one provider-retry transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderRetryEffect {
    /// Apply product recovery and record the planned failure before backoff.
    Recover {
        /// Stable active-turn identity.
        turn_id: String,
        /// Product-independent recovery kind.
        recovery: ProviderRetryRecovery,
        /// One-based retry attempt.
        attempt: u64,
        /// Configured maximum retry attempts.
        max_attempts: u32,
        /// Whether retryable failures bypass the configured finite limit.
        unlimited: bool,
        /// Delay requested after successful recovery.
        delay_ms: u64,
    },
    /// Schedule one actor-owned timer after successful product recovery.
    ScheduleTimer {
        /// Stable active-turn identity.
        turn_id: String,
        /// One-based retry attempt used as the timer generation.
        attempt: u64,
        /// Deterministically planned timer delay.
        delay_ms: u64,
    },
    /// Prepare and dispatch one provider request after the timer elapsed.
    DispatchProvider {
        /// Stable active-turn identity.
        turn_id: String,
        /// One-based retry attempt used for stale-event validation.
        attempt: u64,
    },
}

/// Result of applying one provider-retry event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderRetryTransition {
    /// The reducer requests one product-owned effect.
    Effect(ProviderRetryEffect),
    /// State changed without requesting an external effect.
    Applied,
    /// The failure is ineligible or exhausted and should become terminal.
    Terminal,
    /// Product state disappeared, so retry work was safely abandoned.
    Abandoned,
    /// A duplicate, stale, or otherwise inapplicable event was ignored.
    Ignored,
}

/// Internal phase for one active provider retry attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderRetryPhase {
    /// Waiting for product recovery and event recording.
    Recovering,
    /// A timer effect was requested; only `TimerElapsed` proves delivery.
    TimerPending,
    /// Waiting for product dispatch preparation.
    Dispatching,
    /// The retry request has been made ready for provider execution.
    Dispatched,
}

/// Internal state for one active-turn retry sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProviderRetryState {
    /// Latest one-based retry attempt.
    attempt: u64,
    /// Delay selected once when this attempt was planned.
    delay_ms: u64,
    /// Current effect boundary for the attempt.
    phase: ProviderRetryPhase,
}

/// Pure scheduler for provider retry attempts and effect boundaries.
#[derive(Debug, Clone)]
pub struct ProviderRetryScheduler {
    /// Retry budget and deterministic backoff settings.
    policy: ProviderRetryPolicy,
    /// Active retry state keyed by turn identity.
    turns: BTreeMap<String, ProviderRetryState>,
}

impl Default for ProviderRetryScheduler {
    fn default() -> Self {
        Self::new(DEFAULT_PROVIDER_RETRY_POLICY)
    }
}

impl ProviderRetryScheduler {
    /// Creates an empty reducer with the supplied retry policy.
    pub fn new(policy: ProviderRetryPolicy) -> Self {
        Self {
            policy,
            turns: BTreeMap::new(),
        }
    }

    /// Returns the retry budget and backoff policy used for new failures.
    pub const fn policy(&self) -> ProviderRetryPolicy {
        self.policy
    }

    /// Replaces retry policy without discarding active turn generations.
    ///
    /// Delays already emitted to product timers remain immutable; the new
    /// policy applies when the next provider failure is observed.
    pub fn set_policy(&mut self, policy: ProviderRetryPolicy) {
        self.policy = policy;
    }

    /// Applies one observed event and returns the next effect or terminal state.
    pub fn apply(&mut self, event: ProviderRetryEvent) -> ProviderRetryTransition {
        match event {
            ProviderRetryEvent::FailureObserved {
                turn_id,
                retry_class,
            } => self.observe_failure(turn_id, retry_class, None, None),
            ProviderRetryEvent::FailureObservedWithTiming {
                turn_id,
                retry_class,
                advised_delay_ms,
                jitter_sample,
            } => self.observe_failure(turn_id, retry_class, advised_delay_ms, Some(jitter_sample)),
            ProviderRetryEvent::RecoveryCompleted {
                turn_id,
                attempt,
                result,
            } => self.complete_recovery(&turn_id, attempt, result),
            ProviderRetryEvent::TimerElapsed { turn_id, attempt } => {
                self.observe_timer(&turn_id, attempt)
            }
            ProviderRetryEvent::DispatchCompleted {
                turn_id,
                attempt,
                result,
            } => self.complete_dispatch(&turn_id, attempt, result),
            ProviderRetryEvent::TurnSettled { turn_id } => {
                if self.turns.remove(&turn_id).is_some() {
                    ProviderRetryTransition::Applied
                } else {
                    ProviderRetryTransition::Ignored
                }
            }
        }
    }

    /// Returns the latest planned or dispatched attempt for one turn.
    pub fn attempt(&self, turn_id: &str) -> u64 {
        self.turns
            .get(turn_id)
            .map(|state| state.attempt)
            .unwrap_or(0)
    }

    /// Iterates turns whose progress currently depends on retry state.
    pub fn turn_ids(&self) -> impl Iterator<Item = &String> {
        self.turns.keys()
    }

    /// Plans eligible recovery without claiming that the effect succeeded.
    fn observe_failure(
        &mut self,
        turn_id: String,
        retry_class: ProviderErrorRetryClass,
        advised_delay_ms: Option<u64>,
        jitter_sample: Option<u64>,
    ) -> ProviderRetryTransition {
        if turn_id.trim().is_empty() {
            return ProviderRetryTransition::Terminal;
        }
        let recorded_attempts = match self.turns.get(&turn_id) {
            Some(state) if state.phase == ProviderRetryPhase::Dispatched => state.attempt,
            Some(_) => return ProviderRetryTransition::Ignored,
            None => 0,
        };
        let Some(recovery) = ProviderRetryRecovery::from_retry_class(retry_class) else {
            self.turns.remove(&turn_id);
            return ProviderRetryTransition::Terminal;
        };
        if !self.policy.should_retry(recorded_attempts, retry_class) {
            self.turns.remove(&turn_id);
            return ProviderRetryTransition::Terminal;
        }
        let attempt = recorded_attempts.saturating_add(1);
        let delay_ms = self
            .policy
            .delay_ms(attempt, advised_delay_ms, jitter_sample);
        self.turns.insert(
            turn_id.clone(),
            ProviderRetryState {
                attempt,
                delay_ms,
                phase: ProviderRetryPhase::Recovering,
            },
        );
        ProviderRetryTransition::Effect(ProviderRetryEffect::Recover {
            turn_id,
            recovery,
            attempt,
            max_attempts: self.policy.max_attempts,
            unlimited: self.policy.unlimited,
            delay_ms,
        })
    }

    /// Applies the observed recovery result and requests a timer only on success.
    fn complete_recovery(
        &mut self,
        turn_id: &str,
        attempt: u64,
        result: ProviderRetryRecoveryResult,
    ) -> ProviderRetryTransition {
        if !self.state_matches(turn_id, attempt, ProviderRetryPhase::Recovering) {
            return ProviderRetryTransition::Ignored;
        }
        match result {
            ProviderRetryRecoveryResult::Ready => {
                let Some(state) = self.turns.get_mut(turn_id) else {
                    return ProviderRetryTransition::Ignored;
                };
                state.phase = ProviderRetryPhase::TimerPending;
                let delay_ms = state.delay_ms;
                ProviderRetryTransition::Effect(ProviderRetryEffect::ScheduleTimer {
                    turn_id: turn_id.to_string(),
                    attempt,
                    delay_ms,
                })
            }
            ProviderRetryRecoveryResult::Failed => {
                self.turns.remove(turn_id);
                ProviderRetryTransition::Terminal
            }
            ProviderRetryRecoveryResult::TurnUnavailable => {
                self.turns.remove(turn_id);
                ProviderRetryTransition::Abandoned
            }
        }
    }

    /// Accepts only the current scheduled timer and requests dispatch preparation.
    fn observe_timer(&mut self, turn_id: &str, attempt: u64) -> ProviderRetryTransition {
        if !self.state_matches(turn_id, attempt, ProviderRetryPhase::TimerPending) {
            return ProviderRetryTransition::Ignored;
        }
        if let Some(state) = self.turns.get_mut(turn_id) {
            state.phase = ProviderRetryPhase::Dispatching;
        }
        ProviderRetryTransition::Effect(ProviderRetryEffect::DispatchProvider {
            turn_id: turn_id.to_string(),
            attempt,
        })
    }

    /// Records observed dispatch readiness without claiming product work succeeded.
    fn complete_dispatch(
        &mut self,
        turn_id: &str,
        attempt: u64,
        result: ProviderRetryDispatchResult,
    ) -> ProviderRetryTransition {
        if !self.state_matches(turn_id, attempt, ProviderRetryPhase::Dispatching) {
            return ProviderRetryTransition::Ignored;
        }
        match result {
            ProviderRetryDispatchResult::Ready => {
                if let Some(state) = self.turns.get_mut(turn_id) {
                    state.phase = ProviderRetryPhase::Dispatched;
                }
                ProviderRetryTransition::Applied
            }
            ProviderRetryDispatchResult::TurnUnavailable => {
                self.turns.remove(turn_id);
                ProviderRetryTransition::Abandoned
            }
        }
    }

    /// Reports whether one event identifies the current attempt and phase.
    fn state_matches(&self, turn_id: &str, attempt: u64, phase: ProviderRetryPhase) -> bool {
        self.turns
            .get(turn_id)
            .is_some_and(|state| state.attempt == attempt && state.phase == phase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns the recovery effect for one newly observed retryable failure.
    fn observe_retry(
        scheduler: &mut ProviderRetryScheduler,
        turn_id: &str,
        retry_class: ProviderErrorRetryClass,
    ) -> ProviderRetryEffect {
        let ProviderRetryTransition::Effect(effect) =
            scheduler.apply(ProviderRetryEvent::FailureObserved {
                turn_id: turn_id.to_string(),
                retry_class,
            })
        else {
            panic!("retryable failure should request recovery")
        };
        effect
    }

    /// Verifies normal recovery, timer, and dispatch completion advance one
    /// attempt without assuming any product-owned effect succeeded.
    #[test]
    fn provider_retry_reducer_requires_effect_results_before_advancing() {
        let mut scheduler = ProviderRetryScheduler::default();
        assert_eq!(
            observe_retry(
                &mut scheduler,
                "turn-1",
                ProviderErrorRetryClass::RetryableTransport,
            ),
            ProviderRetryEffect::Recover {
                turn_id: "turn-1".to_string(),
                recovery: ProviderRetryRecovery::None,
                attempt: 1,
                max_attempts: 5,
                unlimited: false,
                delay_ms: 1_000,
            }
        );
        assert_eq!(scheduler.attempt("turn-1"), 1);
        assert_eq!(
            scheduler.apply(ProviderRetryEvent::RecoveryCompleted {
                turn_id: "turn-1".to_string(),
                attempt: 1,
                result: ProviderRetryRecoveryResult::Ready,
            }),
            ProviderRetryTransition::Effect(ProviderRetryEffect::ScheduleTimer {
                turn_id: "turn-1".to_string(),
                attempt: 1,
                delay_ms: 1_000,
            })
        );
        assert_eq!(
            scheduler.apply(ProviderRetryEvent::TimerElapsed {
                turn_id: "turn-1".to_string(),
                attempt: 1,
            }),
            ProviderRetryTransition::Effect(ProviderRetryEffect::DispatchProvider {
                turn_id: "turn-1".to_string(),
                attempt: 1,
            })
        );
        assert_eq!(
            scheduler.apply(ProviderRetryEvent::DispatchCompleted {
                turn_id: "turn-1".to_string(),
                attempt: 1,
                result: ProviderRetryDispatchResult::Ready,
            }),
            ProviderRetryTransition::Applied
        );
    }

    /// Verifies provider advice and injected jitter select one bounded delay
    /// that remains unchanged across the recovery effect boundary.
    #[test]
    fn provider_retry_reducer_retains_advised_jittered_delay() {
        let mut scheduler = ProviderRetryScheduler::default();
        let recovery = scheduler.apply(ProviderRetryEvent::FailureObservedWithTiming {
            turn_id: "turn-advised".to_string(),
            retry_class: ProviderErrorRetryClass::RetryableTransport,
            advised_delay_ms: Some(1_750),
            jitter_sample: 0,
        });
        assert!(matches!(
            recovery,
            ProviderRetryTransition::Effect(ProviderRetryEffect::Recover {
                delay_ms: 1_750,
                ..
            })
        ));
        assert_eq!(
            scheduler.apply(ProviderRetryEvent::RecoveryCompleted {
                turn_id: "turn-advised".to_string(),
                attempt: 1,
                result: ProviderRetryRecoveryResult::Ready,
            }),
            ProviderRetryTransition::Effect(ProviderRetryEffect::ScheduleTimer {
                turn_id: "turn-advised".to_string(),
                attempt: 1,
                delay_ms: 1_750,
            })
        );
    }

    /// Verifies failure classes select the correct recovery interpreter while
    /// non-retryable failures become terminal without retained scheduler state.
    #[test]
    fn provider_retry_reducer_classifies_recovery_and_terminal_failures() {
        let mut scheduler = ProviderRetryScheduler::default();
        let context = observe_retry(
            &mut scheduler,
            "context",
            ProviderErrorRetryClass::ContextLimit,
        );
        assert!(matches!(
            context,
            ProviderRetryEffect::Recover {
                recovery: ProviderRetryRecovery::ContextLimit,
                ..
            }
        ));
        let output = observe_retry(
            &mut scheduler,
            "output",
            ProviderErrorRetryClass::OutputLimit,
        );
        assert!(matches!(
            output,
            ProviderRetryEffect::Recover {
                recovery: ProviderRetryRecovery::OutputLimit,
                ..
            }
        ));
        assert_eq!(
            scheduler.apply(ProviderRetryEvent::FailureObserved {
                turn_id: "terminal".to_string(),
                retry_class: ProviderErrorRetryClass::NonRetryable,
            }),
            ProviderRetryTransition::Terminal
        );
        assert_eq!(scheduler.attempt("terminal"), 0);
    }

    /// Verifies stale and duplicate recovery or timer events cannot schedule
    /// extra effects or alter the current attempt.
    #[test]
    fn provider_retry_reducer_ignores_duplicate_and_stale_events() {
        let mut scheduler = ProviderRetryScheduler::default();
        observe_retry(
            &mut scheduler,
            "turn-1",
            ProviderErrorRetryClass::RetryableTransport,
        );
        assert_eq!(
            scheduler.apply(ProviderRetryEvent::FailureObserved {
                turn_id: "turn-1".to_string(),
                retry_class: ProviderErrorRetryClass::RetryableTransport,
            }),
            ProviderRetryTransition::Ignored
        );
        assert_eq!(
            scheduler.apply(ProviderRetryEvent::RecoveryCompleted {
                turn_id: "turn-1".to_string(),
                attempt: 9,
                result: ProviderRetryRecoveryResult::Ready,
            }),
            ProviderRetryTransition::Ignored
        );
        scheduler.apply(ProviderRetryEvent::RecoveryCompleted {
            turn_id: "turn-1".to_string(),
            attempt: 1,
            result: ProviderRetryRecoveryResult::Ready,
        });
        scheduler.apply(ProviderRetryEvent::TimerElapsed {
            turn_id: "turn-1".to_string(),
            attempt: 1,
        });
        assert_eq!(
            scheduler.apply(ProviderRetryEvent::TimerElapsed {
                turn_id: "turn-1".to_string(),
                attempt: 1,
            }),
            ProviderRetryTransition::Ignored
        );
    }

    /// Verifies failed or unavailable product effects clear retry state and
    /// distinguish terminal recovery failure from a vanished turn.
    #[test]
    fn provider_retry_reducer_applies_effect_failures_explicitly() {
        let mut scheduler = ProviderRetryScheduler::default();
        observe_retry(
            &mut scheduler,
            "failed",
            ProviderErrorRetryClass::ContextLimit,
        );
        assert_eq!(
            scheduler.apply(ProviderRetryEvent::RecoveryCompleted {
                turn_id: "failed".to_string(),
                attempt: 1,
                result: ProviderRetryRecoveryResult::Failed,
            }),
            ProviderRetryTransition::Terminal
        );
        observe_retry(
            &mut scheduler,
            "gone",
            ProviderErrorRetryClass::RetryableTransport,
        );
        assert_eq!(
            scheduler.apply(ProviderRetryEvent::RecoveryCompleted {
                turn_id: "gone".to_string(),
                attempt: 1,
                result: ProviderRetryRecoveryResult::TurnUnavailable,
            }),
            ProviderRetryTransition::Abandoned
        );
        assert_eq!(scheduler.turn_ids().count(), 0);
    }

    /// Verifies deterministic backoff reaches the configured budget, then the
    /// next provider failure becomes terminal and releases state.
    #[test]
    fn provider_retry_reducer_bounds_attempts_and_backoff() {
        let mut scheduler = ProviderRetryScheduler::default();
        for attempt in 1..=5 {
            let effect = observe_retry(
                &mut scheduler,
                "turn-1",
                ProviderErrorRetryClass::RetryableTransport,
            );
            let ProviderRetryEffect::Recover { delay_ms, .. } = effect else {
                panic!("failure should request recovery")
            };
            assert_eq!(delay_ms, 1_000u64 << (attempt - 1));
            scheduler.apply(ProviderRetryEvent::RecoveryCompleted {
                turn_id: "turn-1".to_string(),
                attempt,
                result: ProviderRetryRecoveryResult::Ready,
            });
            scheduler.apply(ProviderRetryEvent::TimerElapsed {
                turn_id: "turn-1".to_string(),
                attempt,
            });
            scheduler.apply(ProviderRetryEvent::DispatchCompleted {
                turn_id: "turn-1".to_string(),
                attempt,
                result: ProviderRetryDispatchResult::Ready,
            });
        }
        assert_eq!(
            scheduler.apply(ProviderRetryEvent::FailureObserved {
                turn_id: "turn-1".to_string(),
                retry_class: ProviderErrorRetryClass::RetryableTransport,
            }),
            ProviderRetryTransition::Terminal
        );
        assert_eq!(scheduler.attempt("turn-1"), 0);
    }

    /// Verifies explicit unlimited mode continues transient provider retries
    /// beyond the finite count while preserving the fifteen-minute delay cap.
    #[test]
    fn provider_retry_reducer_continues_unlimited_transport_retries_at_delay_cap() {
        let mut scheduler = ProviderRetryScheduler::new(ProviderRetryPolicy {
            max_attempts: 2,
            unlimited: true,
            initial_delay_ms: 1_000,
            max_delay_ms: 900_000,
        });
        for attempt in 1..=12 {
            let ProviderRetryEffect::Recover {
                attempt: observed_attempt,
                max_attempts,
                unlimited,
                delay_ms,
                ..
            } = observe_retry(
                &mut scheduler,
                "turn-unlimited",
                ProviderErrorRetryClass::RetryableTransport,
            )
            else {
                panic!("unlimited retry should request recovery")
            };
            assert_eq!(observed_attempt, attempt);
            assert_eq!(max_attempts, 2);
            assert!(unlimited);
            if attempt >= 11 {
                assert_eq!(delay_ms, 900_000);
            }
            scheduler.apply(ProviderRetryEvent::RecoveryCompleted {
                turn_id: "turn-unlimited".to_string(),
                attempt,
                result: ProviderRetryRecoveryResult::Ready,
            });
            scheduler.apply(ProviderRetryEvent::TimerElapsed {
                turn_id: "turn-unlimited".to_string(),
                attempt,
            });
            scheduler.apply(ProviderRetryEvent::DispatchCompleted {
                turn_id: "turn-unlimited".to_string(),
                attempt,
                result: ProviderRetryDispatchResult::Ready,
            });
        }
        assert_eq!(scheduler.attempt("turn-unlimited"), 12);
    }

    /// Verifies a live policy replacement affects the next failure without
    /// invalidating a timer already emitted for the active retry generation.
    #[test]
    fn provider_retry_reducer_preserves_active_state_across_policy_update() {
        let mut scheduler = ProviderRetryScheduler::default();
        observe_retry(
            &mut scheduler,
            "turn-reconfigured",
            ProviderErrorRetryClass::RetryableTransport,
        );
        scheduler.apply(ProviderRetryEvent::RecoveryCompleted {
            turn_id: "turn-reconfigured".to_string(),
            attempt: 1,
            result: ProviderRetryRecoveryResult::Ready,
        });
        scheduler.set_policy(ProviderRetryPolicy {
            max_attempts: 1,
            unlimited: true,
            ..DEFAULT_PROVIDER_RETRY_POLICY
        });

        assert!(matches!(
            scheduler.apply(ProviderRetryEvent::TimerElapsed {
                turn_id: "turn-reconfigured".to_string(),
                attempt: 1,
            }),
            ProviderRetryTransition::Effect(ProviderRetryEffect::DispatchProvider { .. })
        ));
        scheduler.apply(ProviderRetryEvent::DispatchCompleted {
            turn_id: "turn-reconfigured".to_string(),
            attempt: 1,
            result: ProviderRetryDispatchResult::Ready,
        });
        assert!(matches!(
            observe_retry(
                &mut scheduler,
                "turn-reconfigured",
                ProviderErrorRetryClass::RetryableTransport,
            ),
            ProviderRetryEffect::Recover {
                attempt: 2,
                max_attempts: 1,
                unlimited: true,
                ..
            }
        ));
    }

    /// Verifies cancellation and shutdown settlement remove retry state from
    /// any in-flight phase and make later timer events stale.
    #[test]
    fn provider_retry_reducer_settlement_clears_inflight_work() {
        let mut scheduler = ProviderRetryScheduler::default();
        observe_retry(
            &mut scheduler,
            "turn-1",
            ProviderErrorRetryClass::RetryableTransport,
        );
        assert_eq!(
            scheduler.apply(ProviderRetryEvent::TurnSettled {
                turn_id: "turn-1".to_string(),
            }),
            ProviderRetryTransition::Applied
        );
        assert_eq!(
            scheduler.apply(ProviderRetryEvent::TimerElapsed {
                turn_id: "turn-1".to_string(),
                attempt: 1,
            }),
            ProviderRetryTransition::Ignored
        );
    }
}
