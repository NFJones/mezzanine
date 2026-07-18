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
    /// Product recovery for one planned attempt completed.
    RecoveryCompleted {
        /// Stable active-turn identity.
        turn_id: String,
        /// One-based retry attempt from the recovery effect.
        attempt: u32,
        /// Observed recovery result.
        result: ProviderRetryRecoveryResult,
    },
    /// The actor clock delivered one retry timer.
    TimerElapsed {
        /// Stable active-turn identity.
        turn_id: String,
        /// One-based retry attempt encoded in the timer generation.
        attempt: u32,
    },
    /// Product dispatch preparation for one elapsed timer completed.
    DispatchCompleted {
        /// Stable active-turn identity.
        turn_id: String,
        /// One-based retry attempt from the dispatch effect.
        attempt: u32,
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
        attempt: u32,
        /// Configured maximum retry attempts.
        max_attempts: u32,
        /// Delay requested after successful recovery.
        delay_ms: u64,
    },
    /// Schedule one actor-owned timer after successful product recovery.
    ScheduleTimer {
        /// Stable active-turn identity.
        turn_id: String,
        /// One-based retry attempt used as the timer generation.
        attempt: u32,
        /// Deterministically planned timer delay.
        delay_ms: u64,
    },
    /// Prepare and dispatch one provider request after the timer elapsed.
    DispatchProvider {
        /// Stable active-turn identity.
        turn_id: String,
        /// One-based retry attempt used for stale-event validation.
        attempt: u32,
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
    attempt: u32,
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

    /// Applies one observed event and returns the next effect or terminal state.
    pub fn apply(&mut self, event: ProviderRetryEvent) -> ProviderRetryTransition {
        match event {
            ProviderRetryEvent::FailureObserved {
                turn_id,
                retry_class,
            } => self.observe_failure(turn_id, retry_class),
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
    pub fn attempt(&self, turn_id: &str) -> u32 {
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
        let delay_ms = self.policy.delay_ms(attempt);
        self.turns.insert(
            turn_id.clone(),
            ProviderRetryState {
                attempt,
                phase: ProviderRetryPhase::Recovering,
            },
        );
        ProviderRetryTransition::Effect(ProviderRetryEffect::Recover {
            turn_id,
            recovery,
            attempt,
            max_attempts: self.policy.max_attempts,
            delay_ms,
        })
    }

    /// Applies the observed recovery result and requests a timer only on success.
    fn complete_recovery(
        &mut self,
        turn_id: &str,
        attempt: u32,
        result: ProviderRetryRecoveryResult,
    ) -> ProviderRetryTransition {
        if !self.state_matches(turn_id, attempt, ProviderRetryPhase::Recovering) {
            return ProviderRetryTransition::Ignored;
        }
        match result {
            ProviderRetryRecoveryResult::Ready => {
                if let Some(state) = self.turns.get_mut(turn_id) {
                    state.phase = ProviderRetryPhase::TimerPending;
                }
                ProviderRetryTransition::Effect(ProviderRetryEffect::ScheduleTimer {
                    turn_id: turn_id.to_string(),
                    attempt,
                    delay_ms: self.policy.delay_ms(attempt),
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
    fn observe_timer(&mut self, turn_id: &str, attempt: u32) -> ProviderRetryTransition {
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
        attempt: u32,
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
    fn state_matches(&self, turn_id: &str, attempt: u32, phase: ProviderRetryPhase) -> bool {
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
