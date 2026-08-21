//! Runtime-owned agent execution-surface startup.
//!
//! Runtime-created agent panes select their shell mode before process launch
//! and retain one pane-keyed startup owner until the visible agent surface and
//! its execution backend are ready. This boundary deliberately excludes
//! ordinary user panes: foreign-shell prompt discovery remains an adapter for
//! explicit entry into an existing user-controlled environment.

use super::{
    AgentTurnState, BTreeMap, BTreeSet, EventKind, Result, RuntimeAgentComponent,
    RuntimeSessionService, ShellMode, current_unix_millis, json_escape,
};

/// Maximum time an agent-owned pane may wait for authenticated startup admission.
const RUNTIME_AGENT_SURFACE_ADMISSION_TIMEOUT_MS: u64 = 15_000;

/// Current startup phase for one runtime-owned agent pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeAgentSurfaceStartup {
    /// The pane process exists and native root-process context is being checked.
    NativeValidating { primary_process_id: u32 },
    /// An agent-owned pane shell is publishing authenticated startup admission.
    ManagedPaneAdmitting {
        /// Exact pane root process that must publish admission.
        primary_process_id: u32,
        /// Time when the bounded admission owner was created.
        started_at_unix_ms: u64,
    },
    /// The admitted pane shell is publishing and certifying its environment.
    ManagedPaneBootstrapping { primary_process_id: u32 },
    /// The visible prompt and selected execution backend are usable.
    Ready {
        mode: ShellMode,
        primary_process_id: u32,
    },
    /// Startup failed and queued child work was settled terminally.
    Failed {
        /// Mode whose startup contract failed.
        mode: ShellMode,
        /// Exact pane root process fenced by the failed contract.
        primary_process_id: u32,
    },
}

impl RuntimeAgentSurfaceStartup {
    /// Reports whether scheduler work may acquire this pane.
    pub(crate) fn is_ready(self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    /// Returns the selected shell mode for diagnostics and invariants.
    pub(crate) fn mode(self) -> ShellMode {
        match self {
            Self::NativeValidating { .. } => ShellMode::Native,
            Self::ManagedPaneAdmitting { .. } | Self::ManagedPaneBootstrapping { .. } => {
                ShellMode::Pane
            }
            Self::Ready { mode, .. } | Self::Failed { mode, .. } => mode,
        }
    }

    /// Returns the primary process fenced by this startup owner.
    pub(crate) fn primary_process_id(self) -> u32 {
        match self {
            Self::NativeValidating { primary_process_id }
            | Self::ManagedPaneBootstrapping { primary_process_id }
            | Self::Ready {
                primary_process_id, ..
            }
            | Self::Failed {
                primary_process_id, ..
            } => primary_process_id,
            Self::ManagedPaneAdmitting {
                primary_process_id, ..
            } => primary_process_id,
        }
    }
}

impl RuntimeAgentComponent {
    /// Returns mutable storage for runtime-owned pane startup state.
    pub(super) fn agent_surface_startups_mut(
        &mut self,
    ) -> &mut BTreeMap<String, RuntimeAgentSurfaceStartup> {
        &mut self.agent_surface_startups
    }
}

impl RuntimeSessionService {
    /// Begins mode-specific startup for a newly launched runtime-owned pane.
    pub(crate) fn begin_runtime_agent_surface_startup(
        &mut self,
        pane_id: &str,
        mode: ShellMode,
        primary_process_id: u32,
    ) {
        let startup = match mode {
            ShellMode::Native => {
                RuntimeAgentSurfaceStartup::NativeValidating { primary_process_id }
            }
            ShellMode::Pane => RuntimeAgentSurfaceStartup::ManagedPaneAdmitting {
                primary_process_id,
                started_at_unix_ms: current_unix_millis(),
            },
        };
        self.agent
            .agent_surface_startups_mut()
            .insert(pane_id.to_string(), startup);
    }

    /// Marks native startup ready after live root-process context validates.
    pub(crate) fn complete_native_agent_surface_startup(&mut self, pane_id: &str) -> bool {
        let Some(startup) = self.agent.agent_surface_startups.get(pane_id).copied() else {
            return false;
        };
        let RuntimeAgentSurfaceStartup::NativeValidating { primary_process_id } = startup else {
            return false;
        };
        self.agent.agent_surface_startups.insert(
            pane_id.to_string(),
            RuntimeAgentSurfaceStartup::Ready {
                mode: ShellMode::Native,
                primary_process_id,
            },
        );
        true
    }

    /// Advances authenticated managed startup into environment bootstrap.
    pub(crate) fn admit_managed_agent_surface_startup(&mut self, pane_id: &str) -> bool {
        let Some(startup) = self.agent.agent_surface_startups.get(pane_id).copied() else {
            return false;
        };
        let RuntimeAgentSurfaceStartup::ManagedPaneAdmitting {
            primary_process_id, ..
        } = startup
        else {
            return false;
        };
        self.agent.agent_surface_startups.insert(
            pane_id.to_string(),
            RuntimeAgentSurfaceStartup::ManagedPaneBootstrapping { primary_process_id },
        );
        true
    }

    /// Marks managed pane startup ready after environment authority settles.
    pub(crate) fn complete_managed_agent_surface_startup(&mut self, pane_id: &str) -> bool {
        let Some(startup) = self.agent.agent_surface_startups.get(pane_id).copied() else {
            return false;
        };
        let RuntimeAgentSurfaceStartup::ManagedPaneBootstrapping { primary_process_id } = startup
        else {
            return false;
        };
        self.agent.agent_surface_startups.insert(
            pane_id.to_string(),
            RuntimeAgentSurfaceStartup::Ready {
                mode: ShellMode::Pane,
                primary_process_id,
            },
        );
        true
    }

    /// Reports whether one pane has a runtime-owned startup record.
    pub(crate) fn runtime_agent_surface_startup(
        &self,
        pane_id: &str,
    ) -> Option<RuntimeAgentSurfaceStartup> {
        self.agent.agent_surface_startups.get(pane_id).copied()
    }

    /// Reports whether queued work may acquire one pane's execution surface.
    pub(crate) fn agent_surface_allows_scheduler_start(&self, pane_id: &str) -> bool {
        self.runtime_agent_surface_startup(pane_id)
            .is_none_or(RuntimeAgentSurfaceStartup::is_ready)
    }

    /// Starts root-shell bootstrap after authenticated managed startup admission.
    pub(crate) fn begin_managed_agent_surface_bootstrap(&mut self, pane_id: &str) -> Result<bool> {
        let Some(startup) = self.runtime_agent_surface_startup(pane_id) else {
            return Ok(false);
        };
        if !matches!(
            startup,
            RuntimeAgentSurfaceStartup::ManagedPaneAdmitting { .. }
        ) {
            return Ok(false);
        }
        if self.primary_pid_for_live_pane_process(pane_id) != Some(startup.primary_process_id()) {
            self.fail_runtime_agent_surface_startup(
                pane_id,
                "primary process changed before managed startup admission",
            )?;
            return Ok(false);
        }
        if !self.admit_managed_agent_surface_startup(pane_id) {
            return Ok(false);
        }
        self.dispatch_bootstrap_to_pane(pane_id)?;
        Ok(true)
    }

    /// Completes or fails a managed startup after its root-shell bootstrap settles.
    pub(crate) fn settle_managed_agent_surface_bootstrap(&mut self, pane_id: &str) -> Result<bool> {
        if !matches!(
            self.runtime_agent_surface_startup(pane_id),
            Some(RuntimeAgentSurfaceStartup::ManagedPaneBootstrapping { .. })
        ) {
            return Ok(false);
        }
        if self.pane_readiness_state(pane_id) == super::PaneReadinessState::Ready {
            if !self.complete_managed_agent_surface_startup(pane_id) {
                return Ok(false);
            }
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_surface_startup":"ready","mode":"pane"}}"#,
                    json_escape(pane_id)
                ),
            )?;
            self.start_ready_agent_turns()?;
            return Ok(true);
        }
        self.fail_runtime_agent_surface_startup(
            pane_id,
            "managed pane bootstrap did not establish environment authority",
        )?;
        Ok(true)
    }

    /// Fails queued or running child work retained behind one startup owner.
    pub(crate) fn fail_runtime_agent_surface_startup(
        &mut self,
        pane_id: &str,
        reason: &str,
    ) -> Result<usize> {
        let Some(startup) = self.runtime_agent_surface_startup(pane_id) else {
            return Ok(0);
        };
        if matches!(
            startup,
            RuntimeAgentSurfaceStartup::Ready { .. } | RuntimeAgentSurfaceStartup::Failed { .. }
        ) {
            return Ok(0);
        }
        self.agent.agent_surface_startups.insert(
            pane_id.to_string(),
            RuntimeAgentSurfaceStartup::Failed {
                mode: startup.mode(),
                primary_process_id: startup.primary_process_id(),
            },
        );
        self.clear_pane_bootstrap_pending(pane_id);
        self.append_agent_error_text_to_terminal_buffer(
            pane_id,
            &format!("agent: agent-owned startup failed ({reason})"),
        )?;
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_surface_startup":"failed","mode":"{}","reason":"{}"}}"#,
                json_escape(pane_id),
                startup.mode().name(),
                json_escape(reason)
            ),
        )?;
        let turns = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .filter(|turn| {
                turn.pane_id == pane_id
                    && matches!(
                        turn.state,
                        AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut failed = 0usize;
        for turn in turns {
            let _ = self.agent.agent_scheduler.cancel(&turn.turn_id);
            let running_in_shell = self
                .agent_shell_store()
                .get(pane_id)
                .and_then(|session| session.running_turn_id.as_deref())
                == Some(turn.turn_id.as_str());
            if running_in_shell {
                self.finish_agent_turn(pane_id, &turn.turn_id, AgentTurnState::Failed)?;
            } else {
                self.finish_agent_turn_without_shell_session(&turn, AgentTurnState::Failed)?;
            }
            failed = failed.saturating_add(1);
        }
        self.start_ready_agent_turns()?;
        Ok(failed)
    }

    /// Expires managed startup admission that never produced authenticated evidence.
    pub(crate) fn recover_expired_runtime_agent_surface_startups(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<usize> {
        let expired = self
            .agent
            .agent_surface_startups
            .iter()
            .filter_map(|(pane_id, startup)| match startup {
                RuntimeAgentSurfaceStartup::ManagedPaneAdmitting {
                    started_at_unix_ms, ..
                } if now_unix_ms.saturating_sub(*started_at_unix_ms)
                    >= RUNTIME_AGENT_SURFACE_ADMISSION_TIMEOUT_MS =>
                {
                    Some(pane_id.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for pane_id in &expired {
            self.fail_runtime_agent_surface_startup(
                pane_id,
                "managed pane startup admission timed out; select native shell mode or a supported shell",
            )?;
        }
        Ok(expired.len())
    }

    /// Reports whether a pending managed startup still needs its expiry timer.
    pub(crate) fn runtime_agent_surface_startup_timer_needed(&self) -> bool {
        self.agent.agent_surface_startups.values().any(|startup| {
            matches!(
                startup,
                RuntimeAgentSurfaceStartup::ManagedPaneAdmitting { .. }
            )
        })
    }

    /// Returns the stable startup phase name for runtime regression tests.
    #[cfg(test)]
    pub(crate) fn runtime_agent_surface_startup_phase_for_tests(
        &self,
        pane_id: &str,
    ) -> Option<&'static str> {
        self.runtime_agent_surface_startup(pane_id)
            .map(|startup| match startup {
                RuntimeAgentSurfaceStartup::NativeValidating { .. } => "native-validating",
                RuntimeAgentSurfaceStartup::ManagedPaneAdmitting { .. } => "managed-admitting",
                RuntimeAgentSurfaceStartup::ManagedPaneBootstrapping { .. } => {
                    "managed-bootstrapping"
                }
                RuntimeAgentSurfaceStartup::Ready { .. } => "ready",
                RuntimeAgentSurfaceStartup::Failed { .. } => "failed",
            })
    }

    /// Returns panes whose runtime-owned execution surfaces are not ready.
    pub(crate) fn runtime_agent_surface_blocked_panes(&self) -> BTreeSet<String> {
        self.agent
            .agent_surface_startups
            .iter()
            .filter(|(_, startup)| !startup.is_ready())
            .map(|(pane_id, _)| pane_id.clone())
            .collect()
    }

    /// Clears runtime-owned startup when its pane is rolled back or removed.
    pub(crate) fn clear_runtime_agent_surface_startup(&mut self, pane_id: &str) -> bool {
        self.agent.agent_surface_startups.remove(pane_id).is_some()
    }
}
