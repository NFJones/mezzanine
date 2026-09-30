//! Admission boundary for worker claims that require a bounded timer lease.
//!
//! Claim state is recorded on the actor before admission, and worker dispatch
//! remains withheld until this operation succeeds. A rejected admission retires
//! only the exact undispatched owner. Provider and compaction settlement retain
//! their separate service policies; no queued unrelated work is drained here.

use super::{
    AgentId, AsyncRuntimeSessionActor, MezError, Result, RuntimeSideEffect, RuntimeTimerKey,
    RuntimeTimerKind,
};

/// Exact owner of an undispatched provider or compaction worker claim.
pub(super) enum WorkerClaimLease<'a> {
    /// Provider turn and generation validated before dispatch leaves the actor.
    Provider {
        agent_id: &'a AgentId,
        turn_id: &'a str,
        generation: u64,
    },
    /// Compaction task whose pane may retain independently leased generations.
    Compaction { pane_id: &'a str, generation: u64 },
}

impl WorkerClaimLease<'_> {
    /// Returns the mandatory timer identity for this exact claim.
    fn key(&self) -> RuntimeTimerKey {
        match self {
            Self::Provider {
                turn_id,
                generation,
                ..
            } => RuntimeTimerKey::new(RuntimeTimerKind::ProviderClaim, *turn_id, *generation),
            Self::Compaction {
                pane_id,
                generation,
            } => RuntimeTimerKey::new(RuntimeTimerKind::CompactionClaim, *pane_id, *generation),
        }
    }

    /// Checks claim ownership without assuming the pane id alone is authority.
    fn is_current(&self, actor: &AsyncRuntimeSessionActor) -> bool {
        match self {
            Self::Provider {
                agent_id,
                turn_id,
                generation,
            } => actor
                .service
                .agent_provider_claim_matches(agent_id, turn_id, *generation),
            Self::Compaction {
                pane_id,
                generation,
            } => actor
                .service
                .agent_compaction_task_is_claimed(pane_id, *generation),
        }
    }
}

impl AsyncRuntimeSessionActor {
    /// Admits the mandatory lease before returning work to an external worker.
    /// Returns false after rejecting stale work or settling failed admission;
    /// settlement errors remain visible instead of orphaning an unleased claim.
    pub(super) fn admit_worker_claim_lease(
        &mut self,
        owner: WorkerClaimLease<'_>,
        effects: Vec<RuntimeSideEffect>,
    ) -> Result<bool> {
        if !owner.is_current(self) {
            return Ok(false);
        }
        let key = owner.key();
        let valid = effects.iter().filter(|effect| matches!(effect,
            RuntimeSideEffect::ScheduleTimer { key: candidate, delay_ms } if candidate == &key && *delay_ms > 0
        )).count() == 1;
        let admitted = if valid {
            self.queue_runtime_side_effects(effects)
        } else {
            Err(MezError::invalid_state(
                "worker claim admission requires exactly one matching bounded lease",
            ))
        };
        let Err(error) = admitted else {
            return Ok(true);
        };
        // No await or worker handoff occurs between recording and this guard.
        // Still check identity explicitly so a future caller cannot settle a replacement.
        if owner.is_current(self) {
            match owner {
                WorkerClaimLease::Provider { turn_id, .. } => self
                    .service
                    .fail_configured_agent_provider_task(turn_id, &error)?,
                WorkerClaimLease::Compaction {
                    pane_id,
                    generation,
                } => {
                    self.service.expire_claimed_agent_compaction_task(pane_id, generation)
                        .map_err(|settlement_error| MezError::invalid_state(format!(
                            "compaction claim timer admission failed: {}; settlement failed: {}", error.message(), settlement_error.message()
                        )))?;
                }
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::async_runtime::AsyncRuntimeActorConfig;
    use crate::test_support::runtime::RuntimeServiceFixture;
    use mez_mux::layout::Size;

    /// A stale owner cannot enqueue a timer or settle a replacement claim. The
    /// replacement can still admit its exact positive lease afterward.
    #[tokio::test]
    async fn claim_admission_rejects_stale_owner_without_touching_replacement() {
        let mut actor = claimed_provider_actor();
        let agent = AgentId::opaque("agent-%1").unwrap();
        let old = WorkerClaimLease::Provider {
            agent_id: &agent,
            turn_id: "turn-1",
            generation: 1,
        };
        assert!(!actor.admit_worker_claim_lease(old, Vec::new()).unwrap());
        assert!(
            actor
                .service
                .agent_provider_claim_matches(&agent, "turn-1", 2)
        );
        let key = RuntimeTimerKey::new(RuntimeTimerKind::ProviderClaim, "turn-1", 2);
        assert!(
            actor
                .admit_worker_claim_lease(
                    WorkerClaimLease::Provider {
                        agent_id: &agent,
                        turn_id: "turn-1",
                        generation: 2
                    },
                    vec![RuntimeSideEffect::ScheduleTimer {
                        key: key.clone(),
                        delay_ms: 30_000
                    }],
                )
                .unwrap()
        );
        assert_eq!(actor.timers.provider_claim.get("turn-1"), Some(&key));
    }

    /// Missing, duplicated, zero-delay, and wrong-generation lease effects
    /// fail before dispatch and retire only the exact undispatched claim.
    #[tokio::test]
    async fn claim_admission_rejects_invalid_provider_lease_effects() {
        let agent = AgentId::opaque("agent-%1").unwrap();
        let timer = |generation, delay_ms| RuntimeSideEffect::ScheduleTimer {
            key: RuntimeTimerKey::new(RuntimeTimerKind::ProviderClaim, "turn-1", generation),
            delay_ms,
        };
        for effects in [
            Vec::new(),
            vec![timer(2, 0)],
            vec![timer(1, 30_000)],
            vec![timer(2, 30_000), timer(2, 30_000)],
        ] {
            let mut actor = claimed_provider_actor();
            assert!(
                !actor
                    .admit_worker_claim_lease(
                        WorkerClaimLease::Provider {
                            agent_id: &agent,
                            turn_id: "turn-1",
                            generation: 2
                        },
                        effects,
                    )
                    .unwrap()
            );
            assert!(!actor.service.agent_provider_task_is_owned("turn-1"));
            assert_eq!(
                actor
                    .service
                    .agent_turn_ledger()
                    .turn("turn-1")
                    .unwrap()
                    .state,
                mez_agent::AgentTurnState::Failed
            );
            assert!(actor.timers.provider_claim.is_empty());
        }
    }

    /// Builds one exact undispatched claim without provider or credential I/O.
    fn claimed_provider_actor() -> AsyncRuntimeSessionActor {
        let mut service = RuntimeServiceFixture::new().build();
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service
            .execute_agent_shell_command(&primary, "lease admission")
            .unwrap();
        service
            .record_claimed_agent_provider_generation_for_tests("turn-1", 2)
            .unwrap();
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default())
            .unwrap()
            .1
    }
}
