//! Async Runtime Side Effects implementation.
//!
//! This module owns the async runtime side effects boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AsyncAttachedTerminalIo, AsyncHookEvent, AsyncRuntimeService, AsyncRuntimeServiceExit,
    AsyncRuntimeSessionHandle, AttachedTerminalFdRole, ClientId, ClientStatusLine,
    DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES, Duration, MezError, PersistenceEvent,
    PersistenceTarget, PersistenceWriteMode, Result, RuntimeEvent, RuntimeEventBatch,
    RuntimeLifecycleState, RuntimeSideEffect, RuntimeTimerKey, TerminalClientLoopConfig,
    TimerEvent, sleep,
};
use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::audit::AuditRetentionPolicy;
use crate::hooks::{
    HookExecutionPlan, HookExecutionResult, HookExecutionStatus, HookFailure, HookFailureKind,
    execute_program_hook_async,
};
use crate::runtime::apply_registry_update_async;
use crate::terminal::attached_terminal_output_disconnected;
use crate::transcript::AgentTranscriptStore;
use mez_agent::transcript::TranscriptEntry;

// Async side-effect worker scaffolding.

/// Configuration for the async runtime side-effect drain service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncRuntimeSideEffectServiceConfig {
    /// Maximum drain polls before the service returns.
    pub max_polls: u64,
    /// Maximum side effects drained from the actor in one poll.
    pub drain_limit: usize,
    /// Sleep interval after an empty drain.
    pub idle_interval: Duration,
}

impl Default for AsyncRuntimeSideEffectServiceConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_polls: u64::MAX,
            drain_limit: 64,
            idle_interval: Duration::from_millis(100),
        }
    }
}

impl AsyncRuntimeSideEffectServiceConfig {
    /// Validates side-effect worker bounds.
    pub fn validate(self) -> Result<()> {
        if self.max_polls == 0 {
            return Err(MezError::invalid_args(
                "async side-effect service max_polls must be greater than zero",
            ));
        }
        if self.drain_limit == 0 {
            return Err(MezError::invalid_args(
                "async side-effect service drain limit must be greater than zero",
            ));
        }
        if self.idle_interval.is_zero() {
            return Err(MezError::invalid_args(
                "async side-effect service idle interval must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// Report returned by the side-effect drain service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncRuntimeSideEffectServiceReport {
    /// Number of actor drain polls attempted.
    pub polls: u64,
    /// Number of side effects drained from the actor.
    pub drained: u64,
    /// Number of side effects successfully handed to the worker callback.
    pub applied: u64,
    /// Runtime lifecycle state seen at service exit.
    pub terminal_state: RuntimeLifecycleState,
}

/// Carries Async Client Output Flush Service Report state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncClientOutputFlushServiceReport {
    /// Stores the polls value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub polls: u64,
    /// Stores the drained value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub drained: u64,
    /// Stores the flushed value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub flushed: u64,
    /// Stores the bytes written value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bytes_written: usize,
    /// Number of bounded output writes that left bytes pending.
    pub partial_writes: u64,
    /// Bytes retained by the attached terminal endpoint after this flush pass.
    pub pending_output_bytes: usize,
    /// Stores the output hangups value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub output_hangups: u64,
    /// Stores the error roles value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub error_roles: Vec<AttachedTerminalFdRole>,
    /// Stores the terminal state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub terminal_state: RuntimeLifecycleState,
}

/// Report returned by the runtime timer side-effect worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncRuntimeTimerServiceReport {
    /// Number of drain/deadline polls attempted.
    pub polls: u64,
    /// Number of timer side effects drained from the actor.
    pub drained: u64,
    /// Number of schedule requests accepted into the worker's timer table.
    pub scheduled: u64,
    /// Number of active timer entries cancelled before firing.
    pub cancelled: u64,
    /// Number of timers fired and submitted as runtime events.
    pub fired: u64,
    /// Number of timer events accepted by actor ingress.
    pub submitted_events: usize,
    /// Number of timer events that applied a state transition.
    pub applied_events: usize,
    /// Runtime lifecycle state seen at service exit.
    pub terminal_state: RuntimeLifecycleState,
}

/// Report returned by the async persistence side-effect worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncPersistenceSideEffectServiceReport {
    /// Number of actor drain polls attempted.
    pub polls: u64,
    /// Number of persistence side effects drained from the actor.
    pub drained: u64,
    /// Number of writes that completed successfully.
    pub completed: u64,
    /// Number of writes that failed and were reported through typed events.
    pub failed: u64,
    /// Number of payload bytes successfully written.
    pub bytes_written: usize,
    /// Number of persistence events accepted by actor ingress.
    pub submitted_events: usize,
    /// Number of persistence events that applied a state transition.
    pub applied_events: usize,
    /// Runtime lifecycle state seen at service exit.
    pub terminal_state: RuntimeLifecycleState,
}

/// Report returned by the async hook side-effect worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncHookSideEffectServiceReport {
    /// Number of actor drain polls attempted.
    pub polls: u64,
    /// Number of hook side effects drained from the actor.
    pub drained: u64,
    /// Number of hook executions completed and submitted to the actor.
    pub completed: u64,
    /// Number of hook execution worker failures submitted to the actor.
    pub failed: u64,
    /// Number of hook events accepted by actor ingress.
    pub submitted_events: usize,
    /// Number of hook events that applied a state transition.
    pub applied_events: usize,
    /// Runtime lifecycle state seen at service exit.
    pub terminal_state: RuntimeLifecycleState,
}

/// Drains queued runtime side effects and hands each item to a worker callback.
///
/// This is the generic service boundary used while concrete render, pane I/O,
/// provider, hook, and persistence workers are split out. The callback is
/// synchronous by design: it should enqueue into a specific worker or update a
/// fake test sink, not perform blocking I/O inline.
pub async fn run_async_runtime_side_effect_service<A, S>(
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeSideEffectServiceConfig,
    mut apply: A,
    mut should_stop: S,
) -> Result<AsyncRuntimeSideEffectServiceReport>
where
    A: FnMut(RuntimeSideEffect) -> Result<()>,
    S: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    config.validate()?;
    let mut lifecycle_watcher = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut report = AsyncRuntimeSideEffectServiceReport {
        polls: 0,
        drained: 0,
        applied: 0,
        terminal_state: *lifecycle_watcher.borrow(),
    };

    while report.polls < config.max_polls {
        let state = *lifecycle_watcher.borrow_and_update();
        report.terminal_state = state;
        if should_stop(report.polls, state) {
            return Ok(report);
        }

        report.polls = report.polls.saturating_add(1);
        let effects = handle
            .drain_runtime_side_effects(config.drain_limit)
            .await?;
        if effects.is_empty() {
            if report.polls >= config.max_polls {
                return Ok(report);
            }
            if should_stop(report.polls, state) {
                return Ok(report);
            }
            wait_for_side_effects_or_bounded_idle(
                &mut lifecycle_watcher,
                &mut side_effect_watcher,
                config,
            )
            .await;
            continue;
        }
        report.drained = report
            .drained
            .saturating_add(u64::try_from(effects.len()).unwrap_or(u64::MAX));
        for effect in effects {
            apply(effect)?;
            report.applied = report.applied.saturating_add(1);
        }
    }

    report.terminal_state = *lifecycle_watcher.borrow();
    Ok(report)
}

/// Builds an auxiliary supervised service for draining runtime side effects.
pub fn build_async_runtime_side_effect_service<A>(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    config: AsyncRuntimeSideEffectServiceConfig,
    apply: A,
) -> Result<AsyncRuntimeService>
where
    A: FnMut(RuntimeSideEffect) -> Result<()> + Send + 'static,
{
    config.validate()?;
    let mut apply = apply;
    Ok(AsyncRuntimeService::new_auxiliary(name, async move {
        let report =
            run_async_runtime_side_effect_service(&handle, config, &mut apply, |_, state| {
                is_terminal_runtime_lifecycle_state(state)
            })
            .await?;
        Ok(AsyncRuntimeServiceExit::completed(report.applied))
    }))
}

/// Drains timer side effects, maintains active Tokio deadlines, and submits
/// `TimerEvent` values through the runtime actor when deadlines expire.
///
/// The worker owns only timer deadlines. Timer effects stay in the actor queue
/// until this worker drains them, and timer firings re-enter actor state through
/// normal typed event ingress so stale generation checks and side effects remain
/// centralized.
pub async fn run_async_runtime_timer_side_effect_service<S>(
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeSideEffectServiceConfig,
    base_now_ms: u64,
    mut should_stop: S,
) -> Result<AsyncRuntimeTimerServiceReport>
where
    S: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    config.validate()?;
    let start = Instant::now();
    let mut active_timers: BTreeMap<RuntimeTimerKey, Instant> = BTreeMap::new();
    let mut lifecycle_watcher = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut report = AsyncRuntimeTimerServiceReport {
        polls: 0,
        drained: 0,
        scheduled: 0,
        cancelled: 0,
        fired: 0,
        submitted_events: 0,
        applied_events: 0,
        terminal_state: *lifecycle_watcher.borrow(),
    };

    while report.polls < config.max_polls {
        let state = *lifecycle_watcher.borrow_and_update();
        report.terminal_state = state;
        if should_stop(report.polls, state) {
            return Ok(report);
        }

        report.polls = report.polls.saturating_add(1);
        let effects = handle.drain_timer_side_effects(config.drain_limit).await?;
        let had_effects = !effects.is_empty();
        if had_effects {
            report.drained = report
                .drained
                .saturating_add(u64::try_from(effects.len()).unwrap_or(u64::MAX));
            apply_timer_side_effects(&mut active_timers, effects, &mut report);
        }

        let fired = drain_due_timers(&mut active_timers, Instant::now());
        if !fired.is_empty() {
            let mut batch = RuntimeEventBatch::new();
            for key in fired {
                batch.push(RuntimeEvent::Timer(TimerEvent {
                    key,
                    now_ms: runtime_timer_now_ms(start, base_now_ms),
                }));
            }
            let ingress = handle.submit_runtime_events(batch).await?;
            report.fired = report
                .fired
                .saturating_add(u64::try_from(ingress.accepted).unwrap_or(u64::MAX));
            report.submitted_events = report.submitted_events.saturating_add(ingress.accepted);
            report.applied_events = report.applied_events.saturating_add(ingress.applied);
            continue;
        }

        if !had_effects {
            if report.polls >= config.max_polls {
                return Ok(report);
            }
            if should_stop(report.polls, state) {
                return Ok(report);
            }
            if let Some(delay) = next_timer_delay(&active_timers, Instant::now()) {
                tokio::select! {
                    result = side_effect_watcher.changed() => {
                        let _ = result;
                    }
                    result = lifecycle_watcher.changed() => {
                        let _ = result;
                    }
                    _ = sleep(delay) => {}
                }
            } else {
                wait_for_side_effects_or_bounded_idle(
                    &mut lifecycle_watcher,
                    &mut side_effect_watcher,
                    config,
                )
                .await;
            }
        }
    }

    report.terminal_state = *lifecycle_watcher.borrow();
    Ok(report)
}

/// Builds an auxiliary supervised service for runtime timer side effects.
pub fn build_async_runtime_timer_side_effect_service(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    config: AsyncRuntimeSideEffectServiceConfig,
    base_now_ms: u64,
) -> Result<AsyncRuntimeService> {
    config.validate()?;
    Ok(AsyncRuntimeService::new_auxiliary(name, async move {
        let report = run_async_runtime_timer_side_effect_service(
            &handle,
            config,
            base_now_ms,
            |_, state| is_terminal_runtime_lifecycle_state(state),
        )
        .await?;
        Ok(AsyncRuntimeServiceExit::completed(report.fired))
    }))
}

/// Drains program-hook side effects, executes them outside the actor, and
/// reports completion through typed hook events.
pub async fn run_async_hook_side_effect_service<S>(
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeSideEffectServiceConfig,
    mut should_stop: S,
) -> Result<AsyncHookSideEffectServiceReport>
where
    S: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    config.validate()?;
    let mut lifecycle_watcher = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut report = AsyncHookSideEffectServiceReport {
        polls: 0,
        drained: 0,
        completed: 0,
        failed: 0,
        submitted_events: 0,
        applied_events: 0,
        terminal_state: *lifecycle_watcher.borrow(),
    };

    while report.polls < config.max_polls {
        let state = *lifecycle_watcher.borrow_and_update();
        report.terminal_state = state;
        if should_stop(report.polls, state) {
            return Ok(report);
        }

        report.polls = report.polls.saturating_add(1);
        let effects = handle.drain_hook_side_effects(config.drain_limit).await?;
        if effects.is_empty() {
            if report.polls >= config.max_polls {
                return Ok(report);
            }
            if should_stop(report.polls, state) {
                return Ok(report);
            }
            wait_for_side_effects_or_bounded_idle(
                &mut lifecycle_watcher,
                &mut side_effect_watcher,
                config,
            )
            .await;
            continue;
        }

        report.drained = report
            .drained
            .saturating_add(u64::try_from(effects.len()).unwrap_or(u64::MAX));
        let mut batch = RuntimeEventBatch::new();
        for effect in effects {
            let RuntimeSideEffect::RunProgramHook {
                plan,
                triggering_event_completed,
            } = effect
            else {
                continue;
            };
            let plan = *plan;
            let hook_id = plan.hook_id.clone();
            match execute_program_hook_on_async_worker(plan).await {
                Ok((plan, result)) => {
                    report.completed = report.completed.saturating_add(1);
                    batch.push(RuntimeEvent::Hook(AsyncHookEvent::ProgramCompleted {
                        plan: Box::new(plan),
                        result: Box::new(result),
                        triggering_event_completed,
                    }));
                }
                Err(error) => {
                    report.failed = report.failed.saturating_add(1);
                    batch.push(RuntimeEvent::Hook(AsyncHookEvent::Failed {
                        hook_id,
                        error,
                    }));
                }
            }
        }
        if !batch.events.is_empty() {
            let ingress = handle.submit_runtime_events(batch).await?;
            report.submitted_events = report.submitted_events.saturating_add(ingress.accepted);
            report.applied_events = report.applied_events.saturating_add(ingress.applied);
        }
    }

    report.terminal_state = *lifecycle_watcher.borrow();
    Ok(report)
}

/// Builds an auxiliary supervised service for program-hook side effects.
pub fn build_async_hook_side_effect_service(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    config: AsyncRuntimeSideEffectServiceConfig,
) -> Result<AsyncRuntimeService> {
    config.validate()?;
    Ok(AsyncRuntimeService::new_auxiliary(name, async move {
        let report = run_async_hook_side_effect_service(&handle, config, |_, state| {
            is_terminal_runtime_lifecycle_state(state)
        })
        .await?;
        Ok(AsyncRuntimeServiceExit::completed(
            report.completed.saturating_add(report.failed),
        ))
    }))
}

/// Drains persistence side effects, performs bounded Tokio file writes, and
/// reports completion or failure back through typed actor events.
pub async fn run_async_persistence_side_effect_service<S>(
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeSideEffectServiceConfig,
    mut should_stop: S,
) -> Result<AsyncPersistenceSideEffectServiceReport>
where
    S: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    config.validate()?;
    let mut lifecycle_watcher = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut report = AsyncPersistenceSideEffectServiceReport {
        polls: 0,
        drained: 0,
        completed: 0,
        failed: 0,
        bytes_written: 0,
        submitted_events: 0,
        applied_events: 0,
        terminal_state: *lifecycle_watcher.borrow(),
    };

    while report.polls < config.max_polls {
        let state = *lifecycle_watcher.borrow_and_update();
        report.terminal_state = state;
        if should_stop(report.polls, state) {
            return Ok(report);
        }

        report.polls = report.polls.saturating_add(1);
        let effects = handle
            .drain_persistence_side_effects(config.drain_limit)
            .await?;
        if effects.is_empty() {
            if report.polls >= config.max_polls {
                return Ok(report);
            }
            if should_stop(report.polls, state) {
                return Ok(report);
            }
            wait_for_side_effects_or_bounded_idle(
                &mut lifecycle_watcher,
                &mut side_effect_watcher,
                config,
            )
            .await;
            continue;
        }

        report.drained = report
            .drained
            .saturating_add(u64::try_from(effects.len()).unwrap_or(u64::MAX));
        let mut batch = RuntimeEventBatch::new();
        for effect in effects {
            match effect {
                RuntimeSideEffect::Persist {
                    target,
                    path,
                    bytes,
                    mode,
                } => {
                    let byte_count = bytes.len();
                    match persist_side_effect_bytes(target, path.as_path(), &bytes, mode).await {
                        Ok(()) => {
                            report.completed = report.completed.saturating_add(1);
                            report.bytes_written = report.bytes_written.saturating_add(byte_count);
                            batch.push(RuntimeEvent::Persistence(PersistenceEvent::Completed {
                                target,
                                path,
                                bytes: byte_count,
                            }));
                        }
                        Err(error) => {
                            report.failed = report.failed.saturating_add(1);
                            batch.push(RuntimeEvent::Persistence(PersistenceEvent::Failed {
                                target,
                                path,
                                error: error.message().to_string(),
                            }));
                        }
                    }
                }
                RuntimeSideEffect::PersistAuditLog {
                    path,
                    bytes,
                    retention,
                } => {
                    let byte_count = bytes.len();
                    match persist_audit_log_side_effect(path.clone(), &bytes, retention).await {
                        Ok(()) => {
                            report.completed = report.completed.saturating_add(1);
                            report.bytes_written = report.bytes_written.saturating_add(byte_count);
                            batch.push(RuntimeEvent::Persistence(PersistenceEvent::Completed {
                                target: PersistenceTarget::AuditLog,
                                path,
                                bytes: byte_count,
                            }));
                        }
                        Err(error) => {
                            report.failed = report.failed.saturating_add(1);
                            batch.push(RuntimeEvent::Persistence(PersistenceEvent::Failed {
                                target: PersistenceTarget::AuditLog,
                                path,
                                error: error.message().to_string(),
                            }));
                        }
                    }
                }
                RuntimeSideEffect::PersistTranscriptEntries {
                    store,
                    path,
                    entries,
                } => match persist_transcript_entries(store, entries).await {
                    Ok(bytes) => {
                        report.completed = report.completed.saturating_add(1);
                        report.bytes_written = report.bytes_written.saturating_add(bytes);
                        batch.push(RuntimeEvent::Persistence(PersistenceEvent::Completed {
                            target: PersistenceTarget::Transcript,
                            path,
                            bytes,
                        }));
                    }
                    Err(error) => {
                        report.failed = report.failed.saturating_add(1);
                        batch.push(RuntimeEvent::Persistence(PersistenceEvent::Failed {
                            target: PersistenceTarget::Transcript,
                            path,
                            error: error.message().to_string(),
                        }));
                    }
                },
                RuntimeSideEffect::PersistPromptHistory {
                    store,
                    path,
                    conversation_id,
                    prompt,
                } => match persist_prompt_history(store, conversation_id, prompt).await {
                    Ok(bytes) => {
                        report.completed = report.completed.saturating_add(1);
                        report.bytes_written = report.bytes_written.saturating_add(bytes);
                        batch.push(RuntimeEvent::Persistence(PersistenceEvent::Completed {
                            target: PersistenceTarget::Transcript,
                            path,
                            bytes,
                        }));
                    }
                    Err(error) => {
                        report.failed = report.failed.saturating_add(1);
                        batch.push(RuntimeEvent::Persistence(PersistenceEvent::Failed {
                            target: PersistenceTarget::Transcript,
                            path,
                            error: error.message().to_string(),
                        }));
                    }
                },
                RuntimeSideEffect::PersistCommandPromptHistory {
                    store,
                    path,
                    command,
                } => match persist_command_prompt_history(store, command).await {
                    Ok(bytes) => {
                        report.completed = report.completed.saturating_add(1);
                        report.bytes_written = report.bytes_written.saturating_add(bytes);
                        batch.push(RuntimeEvent::Persistence(PersistenceEvent::Completed {
                            target: PersistenceTarget::Transcript,
                            path,
                            bytes,
                        }));
                    }
                    Err(error) => {
                        report.failed = report.failed.saturating_add(1);
                        batch.push(RuntimeEvent::Persistence(PersistenceEvent::Failed {
                            target: PersistenceTarget::Transcript,
                            path,
                            error: error.message().to_string(),
                        }));
                    }
                },
                RuntimeSideEffect::PersistRegistry { registry, update } => {
                    let path = registry.registry_file();
                    match apply_registry_update_async(&registry, &update).await {
                        Ok(_changed) => {
                            report.completed = report.completed.saturating_add(1);
                            batch.push(RuntimeEvent::Persistence(PersistenceEvent::Completed {
                                target: PersistenceTarget::Registry,
                                path,
                                bytes: 0,
                            }));
                        }
                        Err(error) => {
                            report.failed = report.failed.saturating_add(1);
                            batch.push(RuntimeEvent::Persistence(PersistenceEvent::Failed {
                                target: PersistenceTarget::Registry,
                                path,
                                error: error.message().to_string(),
                            }));
                        }
                    }
                }
                _ => {}
            }
        }
        if !batch.events.is_empty() {
            let ingress = handle.submit_runtime_events(batch).await?;
            report.submitted_events = report.submitted_events.saturating_add(ingress.accepted);
            report.applied_events = report.applied_events.saturating_add(ingress.applied);
        }
    }

    report.terminal_state = *lifecycle_watcher.borrow();
    Ok(report)
}

/// Builds an auxiliary supervised service for persistence side effects.
pub fn build_async_persistence_side_effect_service(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    config: AsyncRuntimeSideEffectServiceConfig,
) -> Result<AsyncRuntimeService> {
    config.validate()?;
    Ok(AsyncRuntimeService::new_auxiliary(name, async move {
        let report = run_async_persistence_side_effect_service(&handle, config, |_, state| {
            is_terminal_runtime_lifecycle_state(state)
        })
        .await?;
        Ok(AsyncRuntimeServiceExit::completed(
            report.completed.saturating_add(report.failed),
        ))
    }))
}

/// Drains render invalidation effects, composes frames through the actor, and
/// hands resulting client-output flush effects to a concrete output worker.
///
/// This service is the Phase 6 render boundary. It deliberately drains only
/// `RenderClient` effects, leaving provider, pane I/O, timer, persistence, and
/// other side-effect families queued for their own workers.
pub async fn run_async_render_side_effect_service<A, P, S>(
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeSideEffectServiceConfig,
    terminal_config: TerminalClientLoopConfig,
    mut status_provider: P,
    mut apply: A,
    mut should_stop: S,
) -> Result<AsyncRuntimeSideEffectServiceReport>
where
    A: FnMut(RuntimeSideEffect) -> Result<()>,
    P: FnMut(&ClientId, u64) -> Result<Option<ClientStatusLine>>,
    S: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    config.validate()?;
    let mut lifecycle_watcher = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut report = AsyncRuntimeSideEffectServiceReport {
        polls: 0,
        drained: 0,
        applied: 0,
        terminal_state: *lifecycle_watcher.borrow(),
    };

    while report.polls < config.max_polls {
        let state = *lifecycle_watcher.borrow_and_update();
        report.terminal_state = state;
        if should_stop(report.polls, state) {
            return Ok(report);
        }

        report.polls = report.polls.saturating_add(1);
        let effects = handle.drain_render_side_effects(config.drain_limit).await?;
        if effects.is_empty() {
            if report.polls >= config.max_polls {
                return Ok(report);
            }
            if should_stop(report.polls, state) {
                return Ok(report);
            }
            wait_for_side_effects_or_bounded_idle(
                &mut lifecycle_watcher,
                &mut side_effect_watcher,
                config,
            )
            .await;
            continue;
        }
        report.drained = report
            .drained
            .saturating_add(u64::try_from(effects.len()).unwrap_or(u64::MAX));
        for effect in effects {
            let RuntimeSideEffect::RenderClient { client_id, .. } = effect else {
                continue;
            };
            let status = status_provider(&client_id, report.applied)?;
            let Some(flush) = handle
                .render_client_side_effect(client_id, terminal_config.clone(), status, 0)
                .await?
            else {
                continue;
            };
            apply(RuntimeSideEffect::FlushClientOutput {
                client_id: flush.client_id,
                lines: flush.lines,
                line_style_spans: flush.line_style_spans,
                modes: flush.modes,
            })?;
            report.applied = report.applied.saturating_add(1);
        }
    }

    report.terminal_state = *lifecycle_watcher.borrow();
    Ok(report)
}

/// Builds an auxiliary supervised service for render side effects.
pub fn build_async_render_side_effect_service<A, P>(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    config: AsyncRuntimeSideEffectServiceConfig,
    terminal_config: TerminalClientLoopConfig,
    mut status_provider: P,
    mut apply: A,
) -> Result<AsyncRuntimeService>
where
    A: FnMut(RuntimeSideEffect) -> Result<()> + Send + 'static,
    P: FnMut(&ClientId, u64) -> Result<Option<ClientStatusLine>> + Send + 'static,
{
    config.validate()?;
    Ok(AsyncRuntimeService::new_auxiliary(name, async move {
        let report = run_async_render_side_effect_service(
            &handle,
            config,
            terminal_config,
            &mut status_provider,
            &mut apply,
            |_, state| is_terminal_runtime_lifecycle_state(state),
        )
        .await?;
        Ok(AsyncRuntimeServiceExit::completed(report.applied))
    }))
}

/// Drains styled client-output flush effects and writes them to an attached
/// terminal I/O endpoint.
pub async fn run_async_client_output_flush_service<I, S>(
    handle: &AsyncRuntimeSessionHandle,
    client_id: ClientId,
    io: &mut I,
    config: AsyncRuntimeSideEffectServiceConfig,
    mut should_stop: S,
) -> Result<AsyncClientOutputFlushServiceReport>
where
    I: AsyncAttachedTerminalIo,
    S: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    config.validate()?;
    let mut lifecycle_watcher = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut report = AsyncClientOutputFlushServiceReport {
        polls: 0,
        drained: 0,
        flushed: 0,
        bytes_written: 0,
        partial_writes: 0,
        pending_output_bytes: 0,
        output_hangups: 0,
        error_roles: Vec::new(),
        terminal_state: *lifecycle_watcher.borrow(),
    };

    while report.polls < config.max_polls {
        let state = *lifecycle_watcher.borrow_and_update();
        report.terminal_state = state;
        if should_stop(report.polls, state) {
            return Ok(report);
        }

        report.polls = report.polls.saturating_add(1);
        let effects = handle
            .drain_client_output_flush_side_effects(Some(client_id.clone()), config.drain_limit)
            .await?;
        if effects.is_empty() {
            if io.pending_output_bytes() > 0 {
                let write_report = io
                    .flush_pending_output(DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES)
                    .await?;
                report.bytes_written = report
                    .bytes_written
                    .saturating_add(write_report.bytes_written);
                report.pending_output_bytes = write_report.pending_bytes;
                if write_report.is_partial() {
                    report.partial_writes = report.partial_writes.saturating_add(1);
                    return Ok(report);
                }
                if write_report.bytes_written > 0 {
                    report.flushed = report.flushed.saturating_add(1);
                }
            }
            if report.polls >= config.max_polls {
                return Ok(report);
            }
            if should_stop(report.polls, state) {
                return Ok(report);
            }
            wait_for_side_effects_or_bounded_idle(
                &mut lifecycle_watcher,
                &mut side_effect_watcher,
                config,
            )
            .await;
            continue;
        }
        report.drained = report
            .drained
            .saturating_add(u64::try_from(effects.len()).unwrap_or(u64::MAX));
        for effect in effects {
            let RuntimeSideEffect::FlushClientOutput {
                lines,
                line_style_spans,
                modes,
                ..
            } = effect
            else {
                continue;
            };
            match io
                .write_styled_output_with_modes_bounded(
                    &lines,
                    &line_style_spans,
                    modes,
                    DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES,
                )
                .await
            {
                Ok(write_report) => {
                    report.bytes_written = report
                        .bytes_written
                        .saturating_add(write_report.bytes_written);
                    report.pending_output_bytes = write_report.pending_bytes;
                    if write_report.is_partial() {
                        report.partial_writes = report.partial_writes.saturating_add(1);
                        return Ok(report);
                    }
                    if write_report.bytes_written > 0 {
                        report.flushed = report.flushed.saturating_add(1);
                    }
                }
                Err(error) if attached_terminal_output_disconnected(&error) => {
                    report.output_hangups = report.output_hangups.saturating_add(1);
                }
                Err(error) => return Err(error),
            }
        }
    }

    report.terminal_state = *lifecycle_watcher.borrow();
    Ok(report)
}

/// Builds an auxiliary supervised service for client output flushes.
pub fn build_async_client_output_flush_service<I>(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    client_id: ClientId,
    mut io: I,
    config: AsyncRuntimeSideEffectServiceConfig,
) -> Result<AsyncRuntimeService>
where
    I: AsyncAttachedTerminalIo + Send + 'static,
{
    config.validate()?;
    Ok(AsyncRuntimeService::new_auxiliary(name, async move {
        let report = run_async_client_output_flush_service(
            &handle,
            client_id,
            &mut io,
            config,
            |_, state| is_terminal_runtime_lifecycle_state(state),
        )
        .await?;
        Ok(AsyncRuntimeServiceExit::completed(report.flushed))
    }))
}

/// Runs the wait for side effects or bounded idle operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn wait_for_side_effects_or_bounded_idle(
    lifecycle_watcher: &mut watch::Receiver<RuntimeLifecycleState>,
    side_effect_watcher: &mut watch::Receiver<u64>,
    config: AsyncRuntimeSideEffectServiceConfig,
) {
    tokio::select! {
        result = side_effect_watcher.changed() => {
            let _ = result;
        }
        result = lifecycle_watcher.changed() => {
            let _ = result;
        }
        _ = sleep(config.idle_interval) => {}
    }
}

/// Runs the apply timer side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn apply_timer_side_effects(
    active_timers: &mut BTreeMap<RuntimeTimerKey, Instant>,
    effects: Vec<RuntimeSideEffect>,
    report: &mut AsyncRuntimeTimerServiceReport,
) {
    for effect in effects {
        match effect {
            RuntimeSideEffect::ScheduleTimer { key, delay_ms } => {
                let delay = Duration::from_millis(delay_ms.max(1));
                active_timers.insert(key, Instant::now() + delay);
                report.scheduled = report.scheduled.saturating_add(1);
            }
            RuntimeSideEffect::CancelTimer { key } if active_timers.remove(&key).is_some() => {
                report.cancelled = report.cancelled.saturating_add(1);
            }
            _ => {}
        }
    }
}

/// File-system behavior required for one persistence target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PersistenceFilePolicy {
    /// Whether parent directories should be created.
    ensure_parent: bool,
    /// Whether parent directories should be made private.
    private_parent: bool,
    /// Whether the destination file should be made private after writing.
    private_file: bool,
    /// Whether the file should be durably synchronized before completion.
    sync_all: bool,
    /// Whether the destination file must not be a symbolic link.
    reject_file_symlink: bool,
}

/// Returns the persistence file policy for a target.
fn persistence_file_policy(target: PersistenceTarget) -> PersistenceFilePolicy {
    match target {
        PersistenceTarget::AuditLog
        | PersistenceTarget::Transcript
        | PersistenceTarget::Snapshot
        | PersistenceTarget::Config
        | PersistenceTarget::Registry => PersistenceFilePolicy {
            ensure_parent: true,
            private_parent: true,
            private_file: true,
            sync_all: true,
            reject_file_symlink: matches!(target, PersistenceTarget::Config),
        },
        PersistenceTarget::PanePipe => PersistenceFilePolicy {
            ensure_parent: false,
            private_parent: false,
            private_file: false,
            sync_all: false,
            reject_file_symlink: false,
        },
        PersistenceTarget::ProjectConfig => PersistenceFilePolicy {
            ensure_parent: true,
            private_parent: false,
            private_file: false,
            sync_all: true,
            reject_file_symlink: true,
        },
        PersistenceTarget::ProjectInstruction => PersistenceFilePolicy {
            ensure_parent: false,
            private_parent: false,
            private_file: false,
            sync_all: true,
            reject_file_symlink: true,
        },
    }
}

/// Persists byte payloads using the filesystem policy for the target.
async fn persist_side_effect_bytes(
    target: PersistenceTarget,
    path: &Path,
    bytes: &[u8],
    mode: PersistenceWriteMode,
) -> Result<()> {
    let policy = persistence_file_policy(target);
    if policy.reject_file_symlink {
        reject_persistence_file_symlink(path).await?;
    }
    if policy.ensure_parent {
        if policy.private_parent {
            ensure_private_persistence_parent(path).await?;
        } else {
            ensure_persistence_parent(path).await?;
        }
    }

    if mode == PersistenceWriteMode::Replace {
        return atomic_replace_side_effect_bytes(path, bytes, policy).await;
    }

    let mut options = OpenOptions::new();
    options.write(true);
    match mode {
        PersistenceWriteMode::Append => {
            options.create(true).append(true);
        }
        PersistenceWriteMode::Replace => {
            options.create(true).truncate(true);
        }
        PersistenceWriteMode::CreateNew => {
            options.create_new(true);
        }
    }
    let mut file = options.open(path).await?;
    file.write_all(bytes).await?;
    file.flush().await?;
    if policy.private_file {
        set_private_persistence_file_permissions(path).await?;
    }
    if policy.sync_all {
        file.sync_all().await?;
    }
    Ok(())
}

/// Atomically replaces one persistence target with a sibling temp-file rename.
///
/// # Parameters
/// - `path`: Destination file to replace.
/// - `bytes`: Complete replacement contents.
/// - `policy`: File posture required by the persistence target.
async fn atomic_replace_side_effect_bytes(
    path: &Path,
    bytes: &[u8],
    policy: PersistenceFilePolicy,
) -> Result<()> {
    let temp_path = persistence_temp_path(path);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .await?;
    let write_result = async {
        file.write_all(bytes).await?;
        file.flush().await?;
        if policy.private_file {
            set_private_persistence_file_permissions(&temp_path).await?;
        }
        if policy.sync_all {
            file.sync_all().await?;
        }
        fs::rename(&temp_path, path).await?;
        if policy.private_file {
            set_private_persistence_file_permissions(path).await?;
        }
        if policy.sync_all {
            sync_persistence_parent(path).await;
        }
        Ok(())
    }
    .await;
    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path).await;
    }
    write_result
}

/// Builds a sibling temporary path used for atomic replacement.
///
/// # Parameters
/// - `path`: Destination file.
fn persistence_temp_path(path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("persist");
    path.with_file_name(format!(
        ".{file_name}.mez-tmp-{}-{nonce}",
        std::process::id()
    ))
}

/// Best-effort directory fsync after a successful atomic replacement.
///
/// # Parameters
/// - `path`: File whose parent directory should be synchronized.
async fn sync_persistence_parent(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(directory) = fs::File::open(parent).await
    {
        let _ = directory.sync_all().await;
    }
}

/// Appends an audit JSONL record and enforces its retention policy off actor state.
async fn persist_audit_log_side_effect(
    path: PathBuf,
    bytes: &[u8],
    retention: AuditRetentionPolicy,
) -> Result<()> {
    persist_side_effect_bytes(
        PersistenceTarget::AuditLog,
        path.as_path(),
        bytes,
        PersistenceWriteMode::Append,
    )
    .await?;
    if audit_retention_policy_disabled(&retention) {
        return Ok(());
    }
    retention.enforce_jsonl_async(path.as_path()).await?;
    Ok(())
}

/// Returns whether an audit retention policy has no active pruning rules.
fn audit_retention_policy_disabled(retention: &AuditRetentionPolicy) -> bool {
    retention.max_age_days.is_none()
        && retention.max_records.is_none()
        && retention.max_bytes.is_none()
}

/// Appends transcript entries through the transcript store's async filesystem API.
async fn persist_transcript_entries(
    store: AgentTranscriptStore,
    entries: Vec<TranscriptEntry>,
) -> Result<usize> {
    store.append_many_async(&entries).await
}

/// Appends one shared prompt-history item through the async transcript store.
async fn persist_prompt_history(
    store: AgentTranscriptStore,
    conversation_id: String,
    prompt: String,
) -> Result<usize> {
    let byte_count = prompt.len();
    store
        .append_prompt_history_async(&conversation_id, &prompt)
        .await
        .map(|changed| if changed { byte_count } else { 0 })
}

/// Appends one shared command prompt history item through the async transcript
/// store.
async fn persist_command_prompt_history(
    store: AgentTranscriptStore,
    command: String,
) -> Result<usize> {
    let byte_count = command.len();
    store
        .append_command_prompt_history_async(&command)
        .await
        .map(|changed| if changed { byte_count } else { 0 })
}

/// Rejects symlink destinations for targets that require direct private files.
async fn reject_persistence_file_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(MezError::config(format!(
            "persistence file {} must not be a symlink",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Ensures the destination's parent directory exists without changing permissions.
async fn ensure_persistence_parent(path: &Path) -> Result<()> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    ensure_persistence_dir(parent).await
}

/// Ensures a persistence directory exists and is not a symlink.
async fn ensure_persistence_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(MezError::config(format!(
                    "persistence directory {} must not be a symlink",
                    path.display()
                )));
            }
            if !metadata.is_dir() {
                return Err(MezError::config(format!(
                    "persistence path {} exists but is not a directory",
                    path.display()
                )));
            }
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::create_dir_all(path).await?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

/// Ensures the destination's parent directory exists with private permissions.
async fn ensure_private_persistence_parent(path: &Path) -> Result<()> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    ensure_private_persistence_dir(parent).await
}

/// Ensures a persistence directory exists, is not a symlink, and is private.
async fn ensure_private_persistence_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(MezError::config(format!(
                    "persistence directory {} must not be a symlink",
                    path.display()
                )));
            }
            if !metadata.is_dir() {
                return Err(MezError::config(format!(
                    "persistence path {} exists but is not a directory",
                    path.display()
                )));
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::create_dir_all(path).await?;
        }
        Err(error) => return Err(error.into()),
    }

    set_private_persistence_dir_permissions(path).await
}

/// Applies private directory permissions for persistence storage.
async fn set_private_persistence_dir_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Applies private file permissions for persistence storage.
async fn set_private_persistence_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Runs the execute program hook on async worker operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn execute_program_hook_on_async_worker(
    plan: HookExecutionPlan,
) -> std::result::Result<(HookExecutionPlan, HookExecutionResult), String> {
    let result = match execute_program_hook_async(&plan).await {
        Ok(result) => result,
        Err(error) => hook_spawn_failure_result(&plan, error.to_string()),
    };
    Ok((plan, result))
}

/// Runs the hook spawn failure result operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn hook_spawn_failure_result(plan: &HookExecutionPlan, message: String) -> HookExecutionResult {
    HookExecutionResult {
        hook_id: plan.hook_id.clone(),
        event: plan.event,
        status: HookExecutionStatus::Failed,
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        failure: Some(HookFailure {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            kind: HookFailureKind::Spawn,
            message,
            retryable: false,
        }),
    }
}

/// Runs the drain due timers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn drain_due_timers(
    active_timers: &mut BTreeMap<RuntimeTimerKey, Instant>,
    now: Instant,
) -> Vec<RuntimeTimerKey> {
    let due = active_timers
        .iter()
        .filter(|(_, deadline)| **deadline <= now)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in &due {
        active_timers.remove(key);
    }
    due
}

/// Runs the next timer delay operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn next_timer_delay(
    active_timers: &BTreeMap<RuntimeTimerKey, Instant>,
    now: Instant,
) -> Option<Duration> {
    active_timers
        .values()
        .min()
        .map(|deadline| deadline.saturating_duration_since(now))
}

/// Runs the runtime timer now ms operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_timer_now_ms(start: Instant, base_now_ms: u64) -> u64 {
    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    base_now_ms.saturating_add(elapsed_ms)
}

/// Runs the is terminal runtime lifecycle state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_terminal_runtime_lifecycle_state(state: RuntimeLifecycleState) -> bool {
    matches!(
        state,
        RuntimeLifecycleState::Stopping
            | RuntimeLifecycleState::Killed
            | RuntimeLifecycleState::Failed
    )
}
