//! Runtime shell transaction observation, timeout, and OSC event handling.
//!
//! This module owns the agent shell transaction paths that retain command
//! output, expire timed-out transactions, recover stranded shell dispatches,
//! and interpret Mezzanine OSC transaction events. The process facade keeps
//! pane lifecycle orchestration while this module keeps transaction-specific
//! state transitions together.

use super::*;

/// Defines the RUNTIME SHELL TRANSACTION OBSERVATION LIMIT BYTES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const RUNTIME_SHELL_TRANSACTION_OBSERVATION_LIMIT_BYTES: usize = 256 * 1024;
/// Maximum retained snapshot bytes for the read phase of `apply_patch`.
///
/// The read phase carries remote file bytes that Rust must patch internally, so
/// it needs a larger bound than ordinary model-visible shell observations.
pub(super) const RUNTIME_APPLY_PATCH_SNAPSHOT_OBSERVATION_LIMIT_BYTES: usize = 16 * 1024 * 1024;
/// Defines the RUNTIME SHELL WRAPPER FILTER RECENT COMMAND LIMIT const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const RUNTIME_SHELL_WRAPPER_FILTER_RECENT_COMMAND_LIMIT: usize = 16;
/// Defines the RUNTIME SHELL WRAPPER FILTER RETENTION POLLS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const RUNTIME_SHELL_WRAPPER_FILTER_RETENTION_POLLS: usize = 4096;
/// Defines the RUNTIME HIDDEN SHELL RENDER RETENTION POLLS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const RUNTIME_HIDDEN_SHELL_RENDER_RETENTION_POLLS: usize = 32;
/// Prefix for the bounded OSC 133 markers Mezzanine owns.
pub(super) const RUNTIME_MEZ_OSC_PREFIX: &[u8] = b"\x1b]133;";
/// Maximum OSC payload bytes scanned for a Mezzanine-owned transaction marker.
pub(super) const RUNTIME_MEZ_OSC_SCAN_LIMIT_BYTES: usize = 4096;
/// Returns the retained output bound for one running transaction.
///
/// # Parameters
/// - `transaction`: The transaction whose observed output is being retained.
fn runtime_shell_transaction_observation_limit(transaction: &RunningShellTransactionRef) -> usize {
    if matches!(
        transaction.kind,
        RunningShellTransactionKind::AgentAction { .. }
    ) && apply_patch_transaction_phase(&transaction.command)
        == Some(ApplyPatchTransactionPhase::Read)
    {
        RUNTIME_APPLY_PATCH_SNAPSHOT_OBSERVATION_LIMIT_BYTES
    } else {
        RUNTIME_SHELL_TRANSACTION_OBSERVATION_LIMIT_BYTES
    }
}

impl RuntimeSessionService {
    pub(in crate::runtime) fn record_running_shell_transaction_output(
        &mut self,
        pane_id: &str,
        bytes: &[u8],
    ) {
        let mut status_line_updates = Vec::new();
        for (marker, transaction) in self.running_shell_transactions.iter_mut() {
            if transaction.pane_id == pane_id {
                let observed_bytes = match transaction.kind {
                    RunningShellTransactionKind::AgentAction { .. } => {
                        let transaction_bytes =
                            agent_shell_transaction_bytes_before_end_marker(bytes, marker);
                        agent_shell_transaction_observation_bytes(
                            transaction_bytes,
                            &transaction.command,
                        )
                    }
                    RunningShellTransactionKind::ReadinessProbe
                    | RunningShellTransactionKind::Bootstrap => bytes.to_vec(),
                };
                transaction.observed_output_bytes = transaction
                    .observed_output_bytes
                    .saturating_add(observed_bytes.len());
                let observation_limit = runtime_shell_transaction_observation_limit(transaction);
                if transaction.observed_output_preview.len() >= observation_limit {
                    if !observed_bytes.is_empty() {
                        transaction.observed_output_truncated = true;
                    }
                    continue;
                }
                let remaining =
                    observation_limit.saturating_sub(transaction.observed_output_preview.len());
                let text = String::from_utf8_lossy(&observed_bytes);
                let mut appended = 0usize;
                for ch in text.chars() {
                    let char_len = ch.len_utf8();
                    if appended + char_len > remaining {
                        transaction.observed_output_truncated = true;
                        break;
                    }
                    transaction.observed_output_preview.push(ch);
                    appended += char_len;
                }
                if appended < text.len() {
                    transaction.observed_output_truncated = true;
                }
                if let RunningShellTransactionKind::AgentAction { action_id } = &transaction.kind
                    && let Some(line) = latest_agent_shell_transaction_output_line(
                        &transaction.observed_output_preview,
                    )
                {
                    status_line_updates.push((
                        transaction.turn_id.clone(),
                        action_id.clone(),
                        transaction.pane_id.clone(),
                        line,
                    ));
                }
            }
        }
        for (turn_id, action_id, pane_id, line) in status_line_updates {
            if self.agent_shell_transaction_action_shows_live_output(&turn_id, &action_id) {
                let _ =
                    self.append_agent_shell_output_status_line_to_terminal_buffer(&pane_id, &line);
            }
        }
    }

    /// Applies a runtime timer firing for live Mezzanine-owned shell
    /// transactions.
    ///
    /// Returns the number of transactions that were expired. A zero return
    /// means the timer was accepted but no live transaction had reached its
    /// deadline.
    pub fn apply_shell_transaction_timer_event(&mut self, now_unix_ms: u64) -> Result<usize> {
        let expired = self.expire_timed_out_shell_transactions(now_unix_ms)?;
        let focused = self.expire_timed_out_focused_shell_hooks(now_unix_ms)?;
        Ok(expired.saturating_add(focused))
    }

    /// Returns timer-visible snapshots for live shell transactions with
    /// configured timeouts.
    pub fn running_shell_transaction_timers(&self) -> Vec<RuntimeShellTransactionTimerRef> {
        let mut timers = self
            .running_shell_transactions
            .iter()
            .filter_map(|(marker, transaction)| {
                let timeout_ms = runtime_shell_transaction_effective_timeout_ms(transaction)?;
                Some(RuntimeShellTransactionTimerRef {
                    marker: marker.clone(),
                    kind: runtime_shell_transaction_timer_kind(&transaction.kind),
                    started_at_unix_ms: transaction.started_at_unix_ms,
                    timeout_ms,
                })
            })
            .collect::<Vec<_>>();
        timers.extend(
            self.focused_shell_hook_transactions
                .iter()
                .map(|(marker, transaction)| RuntimeShellTransactionTimerRef {
                    marker: marker.clone(),
                    kind: RuntimeShellTransactionTimerKind::FocusedShellHook,
                    started_at_unix_ms: transaction.started_at_unix_ms,
                    timeout_ms: transaction.timeout_ms,
                }),
        );
        timers
    }

    /// Expires live Mezzanine-owned shell transactions whose runtime timeout has
    /// elapsed without observing their expected terminal marker.
    pub(in crate::runtime) fn expire_timed_out_shell_transactions(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<usize> {
        let expired = self
            .running_shell_transactions
            .iter()
            .filter_map(|(marker, transaction)| {
                let timeout_ms = runtime_shell_transaction_effective_timeout_ms(transaction)?;
                let elapsed_ms = now_unix_ms.saturating_sub(transaction.started_at_unix_ms);
                (elapsed_ms >= timeout_ms)
                    .then(|| (marker.clone(), transaction.clone(), timeout_ms, elapsed_ms))
            })
            .collect::<Vec<_>>();
        let mut expired_count = 0usize;
        for (marker, transaction, timeout_ms, elapsed_ms) in expired {
            if self.running_shell_transactions.remove(&marker).is_none() {
                continue;
            }
            self.clear_shell_transaction_protocol_state(&marker);
            expired_count = expired_count.saturating_add(1);
            match transaction.kind.clone() {
                RunningShellTransactionKind::AgentAction { action_id } => {
                    self.expire_agent_action_shell_transaction(
                        &marker,
                        transaction,
                        &action_id,
                        timeout_ms,
                        elapsed_ms,
                    )?;
                }
                RunningShellTransactionKind::ReadinessProbe => {
                    self.expire_readiness_probe_shell_transaction(
                        &marker,
                        transaction,
                        timeout_ms,
                        elapsed_ms,
                    )?;
                }
                RunningShellTransactionKind::Bootstrap => {
                    self.expire_bootstrap_shell_transaction(
                        &marker,
                        transaction,
                        timeout_ms,
                        elapsed_ms,
                    )?;
                }
            }
        }
        Ok(expired_count)
    }

    /// Runs the expire timed out focused shell hooks operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn expire_timed_out_focused_shell_hooks(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<usize> {
        let expired = self
            .focused_shell_hook_transactions
            .iter()
            .filter_map(|(marker, transaction)| {
                let elapsed_ms = now_unix_ms.saturating_sub(transaction.started_at_unix_ms);
                (elapsed_ms >= transaction.timeout_ms).then(|| marker.clone())
            })
            .collect::<Vec<_>>();
        let mut expired_count = 0usize;
        for marker in expired {
            let Some(pending) = self.focused_shell_hook_transactions.remove(&marker) else {
                continue;
            };
            expired_count = expired_count.saturating_add(1);
            let result = focused_shell_pre_action_timeout_result(&pending.plan);
            if let Some(audit_log) = self.audit_log.as_mut() {
                let record = hook_execution_audit_record(
                    &pending.plan,
                    self.session.id.as_str(),
                    AuditActor {
                        kind: "runtime".to_string(),
                        id: "focused-shell-hook-timeout".to_string(),
                    },
                    "runtime_focused_shell_timeout",
                    &result,
                )
                .with_pane_id(pending.pane_id.clone());
                let _ = audit_log.append(record)?;
            }
            self.append_lifecycle_event(
                EventKind::HookFailed,
                format!(
                    r#"{{"hook_id":"{}","event":"{}","pane_id":"{}","marker":"{}","failure_kind":"Timeout"}}"#,
                    json_escape(&pending.plan.hook_id),
                    runtime_hook_event_name(pending.plan.event),
                    json_escape(&pending.pane_id),
                    json_escape(&marker)
                ),
            )?;
            if let Some(continuation) = pending.continuation.as_ref() {
                let decision = self.record_hook_result(&pending.plan, &result, false)?;
                if decision == crate::hooks::HookFailureDecision::Block {
                    let block = RuntimeHookPipelineBlock::from_result(&result);
                    let _ = self.fail_pending_shell_action_for_hook_block(continuation, &block)?;
                } else {
                    self.record_agent_pre_shell_hook_completed(continuation, &pending.plan.hook_id);
                    let _ = self.dispatch_stored_running_shell_actions(&continuation.turn_id)?;
                }
            }
            self.push_focused_shell_hook_result(result);
        }
        Ok(expired_count)
    }

    /// Fails a timed-out agent shell action and interrupts the pane command when
    /// the runtime can still reach the pane process.
    fn expire_agent_action_shell_transaction(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        action_id: &str,
        timeout_ms: u64,
        elapsed_ms: u64,
    ) -> Result<()> {
        self.interrupt_shell_transaction_pane(&transaction.pane_id)?;
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=shell_transaction_timeout marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        let message = format!("shell command timed out after {timeout_ms} ms");
        let terminal_observation = serde_json::json!({
            "source": "pty",
            "stream": "pty_combined",
            "marker": marker,
            "exit_code": null,
            "signal": null,
            "timed_out": true,
            "timeout_ms": timeout_ms,
            "elapsed_ms": elapsed_ms,
            "combined_output_bytes": transaction.observed_output_bytes,
            "combined_output_preview": transaction.observed_output_preview,
            "boundary_state": "timeout",
            "output_truncated": transaction.observed_output_truncated
        });
        let _ = self.fail_running_shell_transaction_action(
            &transaction,
            marker,
            RuntimeShellTransactionActionFailure {
                action_id: action_id.to_string(),
                status: ActionStatus::TimedOut,
                code: "shell_timeout".to_string(),
                message,
                sent_to_pane: true,
                terminal_observation,
                trace_reason: "shell_transaction_timeout".to_string(),
            },
        )?;
        Ok(())
    }

    /// Settles a readiness probe timeout and fails the pending shell action that
    /// depended on the probe, when such an action is still present.
    fn expire_readiness_probe_shell_transaction(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        timeout_ms: u64,
        elapsed_ms: u64,
    ) -> Result<()> {
        self.interrupt_shell_transaction_pane(&transaction.pane_id)?;
        self.pane_readiness_overrides
            .clear_pending_probe(&transaction.pane_id);
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=readiness_probe_timeout marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        if let Some(action_id) = self.pending_shell_action_id_for_turn(&transaction.turn_id) {
            let message =
                format!("shell readiness probe timed out after {timeout_ms} ms before dispatch");
            let terminal_observation = serde_json::json!({
                "source": "pty",
                "stream": "pty_combined",
                "marker": marker,
                "exit_code": null,
                "signal": null,
                "timed_out": true,
                "timeout_ms": timeout_ms,
                "elapsed_ms": elapsed_ms,
                "combined_output_bytes": transaction.observed_output_bytes,
                "combined_output_preview": transaction.observed_output_preview,
                "boundary_state": "readiness-probe-timeout",
                "output_truncated": transaction.observed_output_truncated
            });
            let _ = self.fail_running_shell_transaction_action(
                &transaction,
                marker,
                RuntimeShellTransactionActionFailure {
                    action_id,
                    status: ActionStatus::TimedOut,
                    code: "readiness_probe_timeout".to_string(),
                    message,
                    sent_to_pane: false,
                    terminal_observation,
                    trace_reason: "readiness_probe_timeout".to_string(),
                },
            )?;
        } else {
            self.append_agent_error_text_to_terminal_buffer(
                &transaction.pane_id,
                &format!("agent: shell readiness probe timed out after {timeout_ms} ms"),
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"degraded","readiness_probe":"timed_out","marker":"{}","timeout_ms":{},"elapsed_ms":{}}}"#,
                    json_escape(&transaction.pane_id),
                    json_escape(&transaction.turn_id),
                    json_escape(marker),
                    timeout_ms,
                    elapsed_ms
                ),
            )?;
        }
        Ok(())
    }

    /// Marks a timed-out bootstrap transaction as a degraded one-shot attempt
    /// instead of retrying the hidden bootstrap wrapper indefinitely.
    fn expire_bootstrap_shell_transaction(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        timeout_ms: u64,
        elapsed_ms: u64,
    ) -> Result<()> {
        self.interrupt_shell_transaction_pane(&transaction.pane_id)?;
        self.pane_bootstrap_pending.remove(&transaction.pane_id);
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","bootstrap":"timed_out","marker":"{}","previous_state":"{}","state":"degraded","timeout_ms":{},"elapsed_ms":{},"output_bytes":{},"output_truncated":{}}}"#,
                json_escape(&transaction.pane_id),
                json_escape(marker),
                runtime_pane_readiness_state_name(previous),
                timeout_ms,
                elapsed_ms,
                transaction.observed_output_bytes,
                transaction.observed_output_truncated
            ),
        )?;
        Ok(())
    }

    /// Sends an interrupt to the pane shell for a timed-out transaction while
    /// tolerating panes that have already exited.
    pub(super) fn interrupt_shell_transaction_pane(&mut self, pane_id: &str) -> Result<()> {
        match self.write_runtime_pane_input(pane_id, b"\x03") {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Returns the first still-running shell action that has not produced a
    /// terminal action result for the given turn.
    pub(super) fn pending_shell_action_id_for_turn(&self, turn_id: &str) -> Option<String> {
        let execution = self.agent_turn_executions.get(turn_id)?;
        let batch = execution.response.action_batch.as_ref()?;
        execution
            .action_results
            .iter()
            .find(|result| {
                result.status == ActionStatus::Running
                    && batch
                        .actions
                        .iter()
                        .find(|action| action.id == result.action_id)
                        .and_then(|action| local_action_plan(action).ok().flatten())
                        .is_some()
            })
            .map(|result| result.action_id.clone())
    }

    /// Requeues pending shell dispatches that have no live transaction and are
    /// waiting behind readiness state that can be safely retried.
    pub(in crate::runtime) fn recover_stranded_agent_shell_dispatches(&mut self) -> Result<usize> {
        let candidates = self.stranded_agent_shell_dispatch_recovery_candidates();
        let mut recovered = 0usize;
        for turn_id in candidates {
            let Some(turn) = self
                .agent_turn_ledger
                .turns()
                .iter()
                .find(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)
                .cloned()
            else {
                continue;
            };
            if self
                .agent_turn_executions
                .get(&turn_id)
                .is_some_and(runtime_execution_ready_for_provider_continuation)
            {
                if self
                    .pending_agent_provider_tasks
                    .insert(turn.turn_id.clone())
                {
                    recovered = recovered.saturating_add(1);
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        "provider_task queued reason=ready_provider_continuation_recovery",
                    )?;
                }
                continue;
            }
            let readiness = self.pane_readiness_state(&turn.pane_id);
            match readiness {
                PaneReadinessState::Ready
                | PaneReadinessState::Unknown
                | PaneReadinessState::PromptCandidate
                | PaneReadinessState::Degraded => {
                    if self
                        .pending_agent_provider_tasks
                        .insert(turn.turn_id.clone())
                    {
                        recovered = recovered.saturating_add(1);
                        self.append_agent_trace_turn_event(
                            &turn.pane_id,
                            &turn.turn_id,
                            &format!(
                                "provider_task queued reason=pending_shell_dispatch_recovery readiness={}",
                                runtime_pane_readiness_state_name(readiness)
                            ),
                        )?;
                    }
                }
                PaneReadinessState::Probing => {
                    if !self.turn_has_running_readiness_probe(&turn.turn_id) {
                        self.pane_readiness_overrides
                            .clear_pending_probe(&turn.pane_id);
                        self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Degraded);
                        if self
                            .pending_agent_provider_tasks
                            .insert(turn.turn_id.clone())
                        {
                            recovered = recovered.saturating_add(1);
                            self.append_agent_status_text_to_terminal_buffer(
                                &turn.pane_id,
                                "agent: shell readiness probe was lost; retrying pending shell command",
                            )?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                "provider_task queued reason=lost_readiness_probe_recovery",
                            )?;
                        }
                    }
                }
                PaneReadinessState::Busy => {
                    let recovery = match self.pane_foreground_primary_shell_state(&turn.pane_id) {
                        Some(true) => Some((
                            PaneReadinessState::PromptCandidate,
                            "agent: shell readiness looked stale; retrying pending shell command",
                            "provider_task queued reason=stale_busy_recovery",
                        )),
                        Some(false) => None,
                        None => Some((
                            PaneReadinessState::Degraded,
                            "agent: shell readiness metadata was unavailable; retrying pending shell command",
                            "provider_task queued reason=unknown_busy_recovery",
                        )),
                    };
                    if let Some((next_readiness, status, trace)) = recovery {
                        self.set_pane_readiness(&turn.pane_id, next_readiness);
                        if self
                            .pending_agent_provider_tasks
                            .insert(turn.turn_id.clone())
                        {
                            recovered = recovered.saturating_add(1);
                            self.append_agent_status_text_to_terminal_buffer(
                                &turn.pane_id,
                                status,
                            )?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                trace,
                            )?;
                        }
                    }
                }
                PaneReadinessState::FullScreen
                | PaneReadinessState::PasswordPrompt
                | PaneReadinessState::InteractiveBlocked => {
                    if self.pane_foreground_primary_shell_state(&turn.pane_id) != Some(true) {
                        continue;
                    }
                    self.set_pane_readiness(&turn.pane_id, PaneReadinessState::PromptCandidate);
                    if self
                        .pending_agent_provider_tasks
                        .insert(turn.turn_id.clone())
                    {
                        recovered = recovered.saturating_add(1);
                        self.append_agent_status_text_to_terminal_buffer(
                            &turn.pane_id,
                            "agent: shell interactivity block looked stale; retrying pending shell command",
                        )?;
                        self.append_agent_trace_turn_event(
                            &turn.pane_id,
                            &turn.turn_id,
                            &format!(
                                "provider_task queued reason=stale_interactive_blocked_recovery readiness={}",
                                runtime_pane_readiness_state_name(readiness)
                            ),
                        )?;
                    }
                }
            }
        }
        Ok(recovered)
    }

    /// Runs the stranded agent shell dispatch recovery candidates operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn stranded_agent_shell_dispatch_recovery_candidates(&self) -> Vec<String> {
        self.agent_turn_executions
            .iter()
            .filter(|(turn_id, execution)| {
                (self.execution_has_pending_shell_dispatch(turn_id, execution)
                    || runtime_execution_ready_for_provider_continuation(execution))
                    && !self.pending_agent_provider_tasks.contains(*turn_id)
                    && !self.claimed_agent_provider_tasks.contains_key(*turn_id)
                    && !self
                        .running_shell_transactions
                        .values()
                        .any(|transaction| transaction.turn_id == turn_id.as_str())
            })
            .map(|(turn_id, _)| turn_id.clone())
            .collect()
    }

    /// Fails running turns that have no service-owned or actor-owned progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns with progress represented by
    ///   actor-owned scheduler state.
    pub(super) fn fail_unreachable_running_agent_turns_with_actor_progress(
        &mut self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> Result<usize> {
        let candidates = self.unreachable_running_agent_turn_candidates(actor_progress_turn_ids);
        let mut failed = 0usize;
        for turn_id in candidates {
            let Some(turn) = self
                .agent_turn_ledger
                .turns()
                .iter()
                .find(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)
                .cloned()
            else {
                continue;
            };
            self.append_agent_status_text_to_terminal_buffer(
                &turn.pane_id,
                "agent: runtime found no remaining progress path; failing turn",
            )?;
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "provider_task failed reason=no_runtime_progress_path",
            )?;
            let error = MezError::invalid_state(
                "running agent turn has no pending provider, claimed provider, shell, hook, approval, subagent, or continuation work",
            );
            self.fail_configured_agent_provider_task(&turn.turn_id, &error)?;
            failed = failed.saturating_add(1);
        }
        Ok(failed)
    }

    /// Returns running turns that cannot make forward progress without runtime
    /// intervention.
    pub(super) fn unreachable_running_agent_turn_candidates(
        &self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> Vec<String> {
        self.agent_turn_ledger
            .turns()
            .iter()
            .filter(|turn| turn.state == AgentTurnState::Running)
            .filter(|turn| !self.turn_has_runtime_progress_path(turn, actor_progress_turn_ids))
            .map(|turn| turn.turn_id.clone())
            .collect()
    }

    /// Reports whether a running turn still has a known path to progress.
    fn turn_has_runtime_progress_path(
        &self,
        turn: &AgentTurnRecord,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> bool {
        let turn_id = turn.turn_id.as_str();
        self.pending_agent_provider_tasks.contains(turn_id)
            || actor_progress_turn_ids.contains(turn_id)
            || self.claimed_agent_provider_tasks.contains_key(turn_id)
            || self.agent_turn_pending_steering.contains_key(turn_id)
            || self
                .running_shell_transactions
                .values()
                .any(|transaction| transaction.turn_id == turn_id)
            || self.turn_has_pending_focused_shell_hook_continuation(turn_id)
            || self
                .joined_subagent_dependencies
                .get(turn_id)
                .is_some_and(|dependency| {
                    self.joined_subagent_dependency_has_live_child(dependency)
                })
            || self
                .blocked_agent_approval_refs
                .values()
                .any(|approval_ref| approval_ref.turn_id == turn_id)
            || self
                .agent_turn_executions
                .get(turn_id)
                .is_some_and(|execution| {
                    runtime_execution_ready_for_provider_continuation(execution)
                        || self.execution_has_pending_shell_dispatch(turn_id, execution)
                        || self.execution_waiting_for_live_joined_subagents(turn_id, execution)
                })
    }

    /// Reports whether a focused-shell hook can still resume one of this turn's
    /// shell actions.
    fn turn_has_pending_focused_shell_hook_continuation(&self, turn_id: &str) -> bool {
        self.focused_shell_hook_transactions
            .values()
            .filter_map(|pending| pending.continuation.as_ref())
            .any(|continuation| continuation.turn_id == turn_id)
    }

    /// Reports whether host process metadata can determine if the pane primary
    /// shell is the foreground process group for its PTY.
    pub(in crate::runtime) fn pane_foreground_primary_shell_state(
        &self,
        pane_id: &str,
    ) -> Option<bool> {
        let primary_pid = self.pane_processes.primary_pid(pane_id)?;
        let foreground_pid = self.pane_processes.foreground_process_group_id(pane_id)?;
        Some(foreground_pid == primary_pid)
    }

    /// Runs the observe agent shell transaction events operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn observe_agent_shell_transaction_events(
        &mut self,
        output_pane_id: &str,
        events: &[TerminalOscEvent],
    ) -> Result<usize> {
        let mut observed = 0usize;
        let mut observed_harness_transaction_end = false;
        for event in events {
            match event {
                TerminalOscEvent::TitleChanged { .. } | TerminalOscEvent::ClipboardSet { .. } => {}
                TerminalOscEvent::ShellPromptStart => {
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_prompt_candidate(
                                output_pane_id,
                                "osc133-prompt-start",
                            )?);
                    }
                }
                TerminalOscEvent::ShellPromptEnd => {
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_prompt_candidate(
                                output_pane_id,
                                "osc133-prompt-end",
                            )?);
                    }
                }
                TerminalOscEvent::ShellCommandFinished { .. } => {
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_prompt_candidate(
                                output_pane_id,
                                "osc133-command-finished",
                            )?);
                    }
                }
                TerminalOscEvent::ShellCommandOutputStart => {
                    if !observed_harness_transaction_end {
                        observed =
                            observed.saturating_add(self.observe_passive_shell_busy(
                                output_pane_id,
                                "osc133-command-start",
                            )?);
                    }
                }
                TerminalOscEvent::ShellTransactionStart {
                    marker,
                    turn_id,
                    agent_id,
                    pane_id,
                } => {
                    observed =
                        observed.saturating_add(self.observe_agent_shell_transaction_start(
                            output_pane_id,
                            marker,
                            turn_id,
                            agent_id,
                            pane_id,
                        )?);
                }
                TerminalOscEvent::ShellTransactionEnd {
                    marker,
                    turn_id,
                    agent_id,
                    pane_id,
                    exit_code,
                } => {
                    let agent_observed = self.observe_agent_shell_transaction_end(
                        output_pane_id,
                        marker,
                        turn_id,
                        agent_id,
                        pane_id,
                        *exit_code,
                    )?;
                    if agent_observed == 0 {
                        observed = observed.saturating_add(
                            self.observe_focused_shell_hook_transaction_end(
                                output_pane_id,
                                marker,
                                pane_id,
                                *exit_code,
                            )?,
                        );
                    } else {
                        observed = observed.saturating_add(agent_observed);
                        observed_harness_transaction_end = true;
                    }
                }
            }
        }
        Ok(observed)
    }

    /// Runs the pane agent turn waiting for provider or shell dispatch operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pane_agent_turn_waiting_for_provider_or_shell_dispatch(
        &self,
        pane_id: &str,
    ) -> Option<String> {
        let turn_id = self
            .agent_shell_store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())?;
        let turn_is_running = self
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running);
        if !turn_is_running {
            return None;
        }
        if self.pending_agent_provider_tasks.contains(turn_id) {
            return Some(turn_id.to_string());
        }
        if self.claimed_agent_provider_tasks.contains_key(turn_id) {
            return None;
        }
        let execution = self.agent_turn_executions.get(turn_id)?;
        if runtime_execution_ready_for_provider_continuation(execution)
            || self.execution_has_pending_shell_dispatch(turn_id, execution)
        {
            Some(turn_id.to_string())
        } else {
            None
        }
    }

    /// Runs the queue waiting agent turn for passive readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn queue_waiting_agent_turn_for_passive_readiness(
        &mut self,
        pane_id: &str,
        reason: &str,
    ) -> Result<usize> {
        let Some(turn_id) = self.pane_agent_turn_waiting_for_provider_or_shell_dispatch(pane_id)
        else {
            return Ok(0);
        };
        if !self.pending_agent_provider_tasks.insert(turn_id.clone()) {
            return Ok(0);
        }
        self.append_agent_trace_turn_event(
            pane_id,
            &turn_id,
            &format!("provider_task queued reason={reason}"),
        )?;
        Ok(1)
    }

    /// Runs the apply terminal osc events operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn apply_terminal_osc_events(
        &mut self,
        events: &[TerminalOscEvent],
    ) -> Result<usize> {
        let mut applied = 0usize;
        for event in events {
            match event {
                TerminalOscEvent::ClipboardSet { selection, content }
                    if terminal_clipboard_policy_accepts_osc52(&self.terminal_clipboard) =>
                {
                    self.copy_text_to_buffer_and_host_clipboard(
                        "osc52",
                        content.clone(),
                        format!("terminal-osc52:{selection}"),
                    )?;
                    applied = applied.saturating_add(1);
                }
                TerminalOscEvent::TitleChanged { .. }
                | TerminalOscEvent::ClipboardSet { .. }
                | TerminalOscEvent::ShellPromptStart
                | TerminalOscEvent::ShellPromptEnd
                | TerminalOscEvent::ShellCommandOutputStart
                | TerminalOscEvent::ShellCommandFinished { .. }
                | TerminalOscEvent::ShellTransactionStart { .. }
                | TerminalOscEvent::ShellTransactionEnd { .. } => {}
            }
        }
        Ok(applied)
    }

    /// Runs the observe passive shell prompt candidate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn observe_passive_shell_prompt_candidate(
        &mut self,
        pane_id: &str,
        source: &str,
    ) -> Result<usize> {
        let previous = self.pane_readiness_state(pane_id);
        let may_recover_interactive_block = matches!(
            previous,
            PaneReadinessState::FullScreen
                | PaneReadinessState::PasswordPrompt
                | PaneReadinessState::InteractiveBlocked
        ) && self.pane_foreground_primary_shell_state(pane_id)
            == Some(true);
        if !matches!(
            previous,
            PaneReadinessState::Unknown | PaneReadinessState::Busy
        ) && !may_recover_interactive_block
        {
            return Ok(0);
        }
        self.set_pane_readiness(pane_id, PaneReadinessState::PromptCandidate);
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","readiness_event":"prompt_candidate","source":"{}","previous_state":"{}","state":"prompt-candidate"}}"#,
                json_escape(pane_id),
                json_escape(source),
                runtime_pane_readiness_state_name(previous)
            ),
        )?;
        let queued =
            self.queue_waiting_agent_turn_for_passive_readiness(pane_id, "prompt_candidate")?;
        Ok(1usize.saturating_add(queued))
    }

    /// Runs the observe passive shell busy operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn observe_passive_shell_busy(
        &mut self,
        pane_id: &str,
        source: &str,
    ) -> Result<usize> {
        let previous = self.pane_readiness_state(pane_id);
        if source == "osc133-command-start"
            && let Some(turn_id) =
                self.pane_agent_turn_waiting_for_provider_or_shell_dispatch(pane_id)
        {
            self.append_agent_trace_turn_event(
                pane_id,
                &turn_id,
                "passive command-start ignored reason=agent_turn_waiting",
            )?;
            return Ok(0);
        }
        if matches!(
            previous,
            PaneReadinessState::Probing
                | PaneReadinessState::FullScreen
                | PaneReadinessState::PasswordPrompt
                | PaneReadinessState::InteractiveBlocked
        ) {
            return Ok(0);
        }
        let revoked = self
            .pane_readiness_overrides
            .revoke(pane_id, ReadinessOverrideRevocation::CommandStartMetadata)
            .is_some();
        if previous == PaneReadinessState::Busy && !revoked {
            return Ok(0);
        }
        self.set_pane_readiness(pane_id, PaneReadinessState::Busy);
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","readiness_event":"busy","source":"{}","previous_state":"{}","state":"busy","override_revoked":{}}}"#,
                json_escape(pane_id),
                json_escape(source),
                runtime_pane_readiness_state_name(previous),
                revoked
            ),
        )?;
        Ok(1)
    }

    /// Sends any deferred transaction payload after the shell wrapper receiver
    /// has started.
    pub(in crate::runtime) fn observe_agent_shell_transaction_start(
        &mut self,
        output_pane_id: &str,
        marker: &str,
        turn_id: &str,
        _agent_id: &str,
        pane_id: &str,
    ) -> Result<usize> {
        let Some(transaction) = self.running_shell_transactions.get(marker).cloned() else {
            return Ok(0);
        };
        if transaction.turn_id != turn_id
            || transaction.pane_id != pane_id
            || output_pane_id != pane_id
        {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "start-marker-metadata-mismatch",
                "shell transaction start marker metadata does not match runtime dispatch state",
            );
        }
        if self.shell_transaction_started_markers.contains(marker) {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "duplicate-start-marker",
                "shell transaction emitted a duplicate start marker",
            );
        }
        self.shell_transaction_started_markers
            .insert(marker.to_string());
        let kind_name = runtime_running_shell_transaction_kind_name(&transaction.kind).to_string();
        let payload = self
            .running_shell_transactions
            .get_mut(marker)
            .and_then(|transaction| transaction.pending_input_payload.take());
        if let Some(transaction) = self.running_shell_transactions.get_mut(marker) {
            transaction.started_at_unix_ms = current_unix_millis();
        }
        let Some(payload) = payload else {
            return Ok(1);
        };
        let payload_len = payload.len();
        if let Err(error) = self.write_runtime_pane_input_priority(pane_id, &payload) {
            self.fail_shell_transactions_for_pane_write_failure(pane_id, error.message())?;
            return Ok(1);
        }
        self.append_agent_trace_turn_event(
            pane_id,
            turn_id,
            &format!(
                "shell_transaction payload_sent marker={} kind={} bytes={}",
                marker, kind_name, payload_len
            ),
        )?;
        Ok(1)
    }

    /// Runs the observe agent shell transaction end operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn observe_agent_shell_transaction_end(
        &mut self,
        output_pane_id: &str,
        marker: &str,
        turn_id: &str,
        agent_id: &str,
        pane_id: &str,
        exit_code: i32,
    ) -> Result<usize> {
        let Some(transaction_ref) = self.running_shell_transactions.get(marker).cloned() else {
            return Ok(0);
        };
        self.append_agent_trace_turn_event(
            pane_id,
            turn_id,
            &format!(
                "shell_transaction observed marker={} kind={} exit_code={}",
                marker,
                runtime_running_shell_transaction_kind_name(&transaction_ref.kind),
                exit_code
            ),
        )?;
        if transaction_ref.turn_id != turn_id
            || transaction_ref.pane_id != pane_id
            || output_pane_id != pane_id
        {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction_ref,
                "end-marker-metadata-mismatch",
                "shell transaction marker metadata does not match runtime dispatch state",
            );
        }
        if self
            .shell_transaction_require_start_markers
            .contains(marker)
            && !self.shell_transaction_started_markers.contains(marker)
        {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction_ref,
                "end-marker-before-start-marker",
                "shell transaction end marker arrived before the start marker",
            );
        }
        let Some(mut transaction_ref) = self.running_shell_transactions.remove(marker) else {
            return Ok(0);
        };
        self.clear_shell_transaction_protocol_state(marker);
        if transaction_ref.kind == RunningShellTransactionKind::ReadinessProbe {
            return self.observe_readiness_probe_transaction_end(
                marker, turn_id, agent_id, pane_id, exit_code,
            );
        }
        if transaction_ref.kind == RunningShellTransactionKind::Bootstrap {
            return self.observe_bootstrap_transaction_end(
                marker,
                pane_id,
                exit_code,
                &transaction_ref.observed_output_preview,
                transaction_ref.observed_output_truncated,
            );
        }
        let RunningShellTransactionKind::AgentAction { ref action_id } = transaction_ref.kind
        else {
            return Err(MezError::invalid_state(
                "shell transaction kind was not handled",
            ));
        };
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        if turn.agent_id != agent_id || turn.pane_id != pane_id {
            return Err(MezError::invalid_state(
                "shell transaction marker identity does not match agent turn",
            ));
        }
        if self.dispatch_apply_patch_followup_if_needed(
            &turn,
            action_id,
            &transaction_ref,
            exit_code,
        )? {
            return Ok(1);
        }

        let (
            mut terminal_state,
            observed_contexts,
            ready_for_provider_continuation,
            post_shell_hook_payload,
            action_transition_trace,
            observed_result,
            observed_results,
            observed_action,
            display_output_after_completion,
        ) = {
            let execution = self
                .agent_turn_executions
                .get_mut(turn_id)
                .ok_or_else(|| MezError::invalid_state("running agent execution is unavailable"))?;
            let batch = execution.response.action_batch.as_ref().ok_or_else(|| {
                MezError::invalid_state("running agent execution has no action batch")
            })?;
            let Some(action) = batch
                .actions
                .iter()
                .find(|action| action.id == action_id.as_str())
                .cloned()
            else {
                // A delayed marker for an already-superseded action is stale.
                return Ok(0);
            };
            let mut shell_backed_actions = Vec::new();
            for candidate in &batch.actions {
                if local_action_plan(candidate)?.is_some() {
                    shell_backed_actions.push(candidate.clone());
                }
            }
            let Some(result_index) = execution
                .action_results
                .iter()
                .position(|result| result.action_id == action_id.as_str())
            else {
                // A delayed marker for an already-superseded result is stale.
                return Ok(0);
            };
            if execution.action_results[result_index].status != ActionStatus::Running {
                return Ok(0);
            }
            let Some(local_plan) = local_action_plan(&action)? else {
                return Err(MezError::invalid_state(
                    "shell transaction does not match shell-backed action payload",
                ));
            };
            let raw_output_preview = transaction_ref.observed_output_preview.clone();
            let decoded_transport =
                decode_shell_output_transport_with_diagnostics(&raw_output_preview);
            let transport_diagnostics = decoded_transport.diagnostics.clone();
            transaction_ref.observed_output_preview = if transport_diagnostics.saw_begin_marker {
                decoded_transport.output
            } else {
                raw_output_preview.clone()
            };
            transaction_ref.observed_output_bytes = transaction_ref.observed_output_preview.len();
            if exit_code == 0 {
                let processed_output =
                    postprocess_shell_action_success_output(&action, raw_output_preview)?;
                if transport_diagnostics.saw_begin_marker || !processed_output.trim().is_empty() {
                    transaction_ref.observed_output_preview = processed_output;
                    transaction_ref.observed_output_bytes =
                        transaction_ref.observed_output_preview.len();
                }
            }
            let signal: Option<i32> = if exit_code > 128 && exit_code < 256 {
                Some(exit_code - 128)
            } else {
                None
            };
            let structured_content = shell_command_structured_content_json(
                &action,
                true,
                serde_json::Value::Null,
                &[],
                serde_json::json!({
                    "source": "pty",
                    "stream": "pty_combined",
                    "marker": marker,
                    "exit_code": exit_code,
                    "signal": signal,
                    "timed_out": false,
                    "combined_output_bytes": transaction_ref.observed_output_bytes,
                    "combined_output_preview": transaction_ref.observed_output_preview,
                    "boundary_state": "end-marker-observed",
                    "output_truncated": transaction_ref.observed_output_truncated || transport_diagnostics.output_truncated(),
                    "transport_incomplete": transport_diagnostics.transport_incomplete(),
                    "transport_diagnostics": transport_diagnostics.to_json()
                }),
            )?;
            let plain_shell_command =
                matches!(action.payload, AgentActionPayload::ShellCommand { .. });
            execution.action_results[result_index] = if exit_code == 0 || plain_shell_command {
                let success_content = if plain_shell_command && exit_code != 0 {
                    shell_command_result_content(
                        &transaction_ref.observed_output_preview,
                        Some(exit_code),
                        false,
                        false,
                    )
                } else if local_plan.display_output_after_completion
                    && !transaction_ref.observed_output_preview.trim().is_empty()
                {
                    vec![transaction_ref.observed_output_preview.clone()]
                } else {
                    vec!["shell command exited with status 0".to_string()]
                };
                ActionResult::succeeded(&turn, &action, success_content, Some(structured_content))
            } else {
                let mut result = ActionResult::failed(
                    &turn,
                    &action,
                    ActionStatus::Failed,
                    "shell_command_failed",
                    format!("shell command exited with status {exit_code}"),
                )?;
                if !transaction_ref.observed_output_preview.trim().is_empty() {
                    result.content = vec![ActionContentBlock::text(
                        transaction_ref.observed_output_preview.clone(),
                    )];
                }
                result.structured_content_json = Some(structured_content);
                result
            };
            let shell_command_nonzero_result = exit_code != 0 && plain_shell_command;
            execution.terminal_state = if shell_command_nonzero_result {
                AgentTurnState::Running
            } else {
                runtime_agent_turn_state_from_action_results(
                    &execution.action_results,
                    execution.final_turn,
                )
            };
            let mut observed_results = vec![execution.action_results[result_index].clone()];
            if shell_command_nonzero_result {
                let skipped_content = vec![format!(
                    "shell command not run because `{action_id}` exited with status {exit_code}"
                )];
                for result in &mut execution.action_results {
                    if result.status != ActionStatus::Running
                        || result.action_id == action_id.as_str()
                    {
                        continue;
                    }
                    let Some(skipped_action) = shell_backed_actions
                        .iter()
                        .find(|candidate| candidate.id == result.action_id)
                    else {
                        continue;
                    };
                    let structured_content = shell_command_structured_content_json(
                        skipped_action,
                        false,
                        serde_json::Value::Null,
                        &[],
                        serde_json::json!({
                            "source": "runtime",
                            "stream": "pty_input",
                            "marker": marker,
                            "exit_code": null,
                            "signal": null,
                            "timed_out": false,
                            "combined_output_bytes": 0,
                            "combined_output_preview": "",
                            "boundary_state": "skipped-after-nonzero-shell-exit",
                            "output_truncated": false,
                            "skipped": true,
                            "previous_action_id": action_id,
                            "previous_exit_code": exit_code
                        }),
                    )?;
                    *result = ActionResult::succeeded(
                        &turn,
                        skipped_action,
                        skipped_content.clone(),
                        Some(structured_content),
                    );
                    observed_results.push(result.clone());
                }
            }
            let action_transition_trace = format!(
                "action {} {} reason=shell_transaction_exit terminal_state={}",
                action_id,
                if execution.action_results[result_index].status == ActionStatus::Succeeded {
                    "succeeded"
                } else {
                    "failed"
                },
                runtime_agent_turn_state_name(execution.terminal_state)
            );
            let observed_result = execution.action_results[result_index].clone();
            let observed_contexts = observed_results
                .iter()
                .map(|result| ContextBlock {
                    source: ContextSourceKind::ActionResult,
                    label: format!("action result {}", result.action_id),
                    content: action_result_context_content(result),
                })
                .collect::<Vec<_>>();
            let post_shell_hook_payload =
                runtime_post_shell_hook_payload(&turn, &action, &observed_result, exit_code);
            let ready_for_provider_continuation = shell_command_nonzero_result
                || runtime_execution_ready_for_provider_continuation(execution);
            (
                execution.terminal_state,
                observed_contexts,
                ready_for_provider_continuation,
                post_shell_hook_payload,
                action_transition_trace,
                observed_result,
                observed_results,
                action,
                local_plan.display_output_after_completion,
            )
        };
        self.runtime_metrics.record_shell_transaction_completion(
            transaction_ref.started_at_unix_ms,
            current_unix_millis(),
            transaction_ref.observed_output_bytes,
            exit_code,
        );
        if exit_code == 0 {
            self.record_shell_dispatch_success(turn_id, &transaction_ref.command, &observed_action);
        }
        if exit_code == 0
            && matches!(
                observed_action.payload,
                AgentActionPayload::ApplyPatch { .. }
            )
            && apply_patch_transaction_phase(&transaction_ref.command)
                == Some(ApplyPatchTransactionPhase::Write)
        {
            self.record_agent_modified_files_from_diff(
                pane_id,
                &transaction_ref.observed_output_preview,
            );
        }
        self.append_agent_trace_turn_event(pane_id, turn_id, &action_transition_trace)?;
        self.append_agent_trace_maap_action_results(
            pane_id,
            turn_id,
            "shell_transaction_action_result",
            &observed_results,
        )?;
        if let Some(execution) = self.agent_turn_executions.get(turn_id).cloned() {
            self.record_runtime_agent_patch_results_for_turn(&turn, &execution);
        }
        if exit_code == 0
            && display_output_after_completion
            && (self.agent_debug_enabled(pane_id)
                || self.agent_action_result_renders_in_normal_mode(&observed_action))
            && !self.agent_shell_view_enabled(pane_id)
            && !transaction_ref.observed_output_preview.trim().is_empty()
        {
            self.append_agent_action_result_text_to_terminal_buffer(
                pane_id,
                &observed_action,
                &observed_result,
                &transaction_ref.observed_output_preview,
            )?;
        }

        self.run_configured_completed_hooks(HookEvent::PostShellCommand, &post_shell_hook_payload)?;

        let mut transcript_entries = 0usize;
        if matches!(
            terminal_state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
            let mut execution = self
                .agent_turn_executions
                .get(turn_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("observed agent execution was not stored")
                })?;
            let failure_feedback_queued = if terminal_state == AgentTurnState::Failed {
                self.append_runtime_agent_execution_failure_audit(&turn, &execution)?;
                self.queue_agent_failure_feedback_for_correction(
                    &turn,
                    &mut execution,
                    "shell_transaction_failed_action",
                )?
            } else {
                false
            };
            if failure_feedback_queued {
                self.agent_turn_executions.remove(turn_id);
                terminal_state = AgentTurnState::Running;
            } else {
                self.present_deferred_agent_say_actions_to_terminal_buffer(pane_id, &execution)?;
                transcript_entries =
                    self.persist_runtime_agent_turn_execution_transcript(&turn, &execution)?;
                self.emit_subagent_task_result_for_execution(&turn, &execution)?;
                self.complete_running_agent_turn_and_start_ready(
                    &turn,
                    terminal_state,
                    "shell_transaction_settled",
                )?;
            }
        } else if terminal_state == AgentTurnState::Running {
            self.agent_turn_contexts
                .get_mut(turn_id)
                .ok_or_else(|| {
                    MezError::invalid_state("running agent turn context is unavailable")
                })?
                .blocks
                .extend(observed_contexts);
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
            if ready_for_provider_continuation {
                self.pending_agent_provider_tasks
                    .insert(turn_id.to_string());
                self.append_agent_trace_turn_event(
                    pane_id,
                    turn_id,
                    "provider_task queued reason=shell_transaction_result_ready",
                )?;
            } else {
                let should_dispatch_stored_shell = self
                    .agent_turn_executions
                    .get(turn_id)
                    .is_some_and(|execution| {
                        self.execution_has_pending_shell_dispatch(turn_id, execution)
                    });
                if should_dispatch_stored_shell {
                    self.append_agent_trace_turn_event(
                        pane_id,
                        turn_id,
                        "pending_shell_dispatch available reason=shell_transaction_result",
                    )?;
                    let _ = self.dispatch_stored_running_shell_actions(turn_id)?;
                }
            }
        } else {
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
        }

        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","shell_transaction":"observed","marker":"{}","exit_code":{},"transcript_entries":{}}}"#,
                json_escape(pane_id),
                json_escape(turn_id),
                runtime_agent_turn_state_name(terminal_state),
                json_escape(marker),
                exit_code,
                transcript_entries
            ),
        )?;
        Ok(1)
    }
}

/// Runs the runtime shell transaction timer kind operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_shell_transaction_timer_kind(
    kind: &RunningShellTransactionKind,
) -> RuntimeShellTransactionTimerKind {
    match kind {
        RunningShellTransactionKind::AgentAction { .. } => {
            RuntimeShellTransactionTimerKind::AgentAction
        }
        RunningShellTransactionKind::ReadinessProbe => {
            RuntimeShellTransactionTimerKind::ReadinessProbe
        }
        RunningShellTransactionKind::Bootstrap => RuntimeShellTransactionTimerKind::Bootstrap,
    }
}
