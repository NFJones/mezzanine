//! Configuration and reports for pane workers and supervisors.

use super::*;

/// Configuration for one pane I/O side-effect worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncPaneIoSideEffectServiceConfig {
    /// Maximum side-effect polls before the service returns.
    pub max_polls: u64,
    /// Maximum pane I/O side effects drained per poll.
    pub drain_limit: usize,
    /// Sleep interval used after an empty drain.
    pub idle_interval: Duration,
}

impl Default for AsyncPaneIoSideEffectServiceConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_polls: u64::MAX,
            drain_limit: 1,
            idle_interval: Duration::from_millis(16),
        }
    }
}

impl AsyncPaneIoSideEffectServiceConfig {
    /// Validates pane side-effect worker bounds.
    pub fn validate(self) -> Result<()> {
        if self.max_polls == 0 {
            return Err(MezError::invalid_args(
                "async pane I/O side-effect service max_polls must be greater than zero",
            ));
        }
        if self.drain_limit == 0 {
            return Err(MezError::invalid_args(
                "async pane I/O side-effect service drain_limit must be greater than zero",
            ));
        }
        if self.idle_interval.is_zero() {
            return Err(MezError::invalid_args(
                "async pane I/O side-effect service idle interval must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// Report returned by one pane I/O side-effect worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncPaneIoSideEffectServiceReport {
    /// Number of side-effect polls attempted.
    pub polls: u64,
    /// Number of pane side effects drained.
    pub drained: u64,
    /// Number of runtime events submitted after pane I/O execution.
    pub submitted_events: usize,
    /// Number of submitted events applied to runtime state.
    pub applied_events: usize,
    /// Last observed runtime lifecycle state.
    pub terminal_state: RuntimeLifecycleState,
}

/// Configuration for one combined pane process worker.
///
/// This service shape is the migration target for Phase 4: one task owns one
/// pane backend, drains output, executes pane I/O side effects, and reports all
/// resulting runtime events in the same per-pane order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncPaneProcessServiceConfig {
    /// Maximum service polls before returning.
    pub max_polls: u64,
    /// Maximum PTY output chunks coalesced into one actor submission per poll.
    pub output_drain_limit: usize,
    /// Maximum pane I/O side effects drained per poll.
    pub drain_limit: usize,
    /// Sleep interval used when neither output nor side effects are ready.
    pub idle_interval: Duration,
    /// Minimum interval between foreground process metadata polls.
    pub foreground_metadata_interval: Duration,
}

impl Default for AsyncPaneProcessServiceConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_polls: u64::MAX,
            output_drain_limit: 4,
            drain_limit: 1,
            idle_interval: Duration::from_millis(16),
            foreground_metadata_interval: Duration::from_secs(1),
        }
    }
}

impl AsyncPaneProcessServiceConfig {
    /// Validates service loop bounds.
    pub fn validate(self) -> Result<()> {
        if self.max_polls == 0 {
            return Err(MezError::invalid_args(
                "async pane process service max_polls must be greater than zero",
            ));
        }
        if self.drain_limit == 0 {
            return Err(MezError::invalid_args(
                "async pane process service drain_limit must be greater than zero",
            ));
        }
        if self.output_drain_limit == 0 {
            return Err(MezError::invalid_args(
                "async pane process service output_drain_limit must be greater than zero",
            ));
        }
        if self.idle_interval.is_zero() {
            return Err(MezError::invalid_args(
                "async pane process service idle interval must be greater than zero",
            ));
        }
        if self.foreground_metadata_interval.is_zero() {
            return Err(MezError::invalid_args(
                "async pane process service foreground metadata interval must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// Report returned by one combined pane process worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncPaneProcessServiceReport {
    /// Number of service polls attempted.
    pub polls: u64,
    /// Number of pane output events observed.
    pub output_events: u64,
    /// Number of natural process exit events observed.
    pub exit_events: u64,
    /// Number of pane I/O side effects drained.
    pub drained: u64,
    /// Number of runtime events submitted after pane I/O execution.
    pub submitted_events: usize,
    /// Number of submitted events applied to runtime state.
    pub applied_events: usize,
    /// Last observed runtime lifecycle state.
    pub terminal_state: RuntimeLifecycleState,
}

impl AsyncPaneProcessServiceReport {
    /// Creates a report initialized with the actor lifecycle state observed at
    /// service startup.
    pub(super) fn new(initial_state: RuntimeLifecycleState) -> Self {
        Self {
            polls: 0,
            output_events: 0,
            exit_events: 0,
            drained: 0,
            submitted_events: 0,
            applied_events: 0,
            terminal_state: initial_state,
        }
    }
}

/// Configuration for the daemon pane-process supervisor.
///
/// The supervisor is responsible for dynamic ownership transfer: it asks the
/// actor for running pane processes that are still manager-owned, starts one
/// combined async pane worker per process, and continues watching for panes
/// created after daemon startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncPaneProcessSupervisorServiceConfig {
    /// Maximum supervisor polling iterations before returning.
    pub max_polls: u64,
    /// Maximum pane processes to claim from the actor per poll.
    pub take_limit: usize,
    /// Sleep interval used when no handoff, side effect, event, or worker
    /// completion is ready.
    pub idle_interval: Duration,
    /// Configuration passed to each per-pane process worker.
    pub pane_service: AsyncPaneProcessServiceConfig,
}

impl Default for AsyncPaneProcessSupervisorServiceConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_polls: u64::MAX,
            take_limit: 16,
            idle_interval: Duration::from_millis(100),
            pane_service: AsyncPaneProcessServiceConfig::default(),
        }
    }
}

impl AsyncPaneProcessSupervisorServiceConfig {
    /// Validates supervisor and per-pane worker bounds.
    pub fn validate(self) -> Result<()> {
        if self.max_polls == 0 {
            return Err(MezError::invalid_args(
                "async pane process supervisor max_polls must be greater than zero",
            ));
        }
        if self.take_limit == 0 {
            return Err(MezError::invalid_args(
                "async pane process supervisor take_limit must be greater than zero",
            ));
        }
        if self.idle_interval.is_zero() {
            return Err(MezError::invalid_args(
                "async pane process supervisor idle interval must be greater than zero",
            ));
        }
        self.pane_service.validate()
    }
}

/// Report returned by the dynamic pane-process supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncPaneProcessSupervisorServiceReport {
    /// Number of supervisor polling iterations.
    pub polls: u64,
    /// Number of pane workers spawned from actor-owned handoffs.
    pub spawned_workers: u64,
    /// Number of pane workers that completed successfully.
    pub completed_workers: u64,
    /// Last observed runtime lifecycle state.
    pub terminal_state: RuntimeLifecycleState,
}

impl AsyncPaneProcessSupervisorServiceReport {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn new(initial_state: RuntimeLifecycleState) -> Self {
        Self {
            polls: 0,
            spawned_workers: 0,
            completed_workers: 0,
            terminal_state: initial_state,
        }
    }
}
