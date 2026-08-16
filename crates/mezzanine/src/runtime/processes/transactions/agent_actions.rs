//! Agent shell transaction start and completion.

use super::super::{
    ManagedShellHandoffEffect, ManagedShellHandoffEvent, ManagedShellHandoffIdentity,
    ManagedShellKind, reduce_managed_shell_handoff,
};
use super::{
    ActionContentBlock, ActionResult, ActionStatus, AgentActionPayload, AgentTurnState,
    ApplyPatchTransactionPhase, EventKind, HookEvent, MezError, PaneReadinessState, Result,
    RunningShellTransactionKind, RuntimePaneEnvironmentAuthorityUnavailableReason,
    RuntimeSessionService, RuntimeShellTransactionActionFailure, apply_patch_transaction_phase,
    current_unix_millis, decode_shell_output_transport_with_diagnostics, json_escape,
    local_action_plan, postprocess_shell_action_success_output,
    runtime_agent_turn_state_from_action_results, runtime_agent_turn_state_name,
    runtime_execution_ready_for_provider_continuation, runtime_post_shell_hook_payload,
    runtime_running_shell_transaction_kind_name, shell_action_failure_diagnostic,
    shell_command_result_content,
};
use mez_agent::semantic_patch_planning::{
    APPLY_PATCH_RESULT_MARKER, ApplyPatchFileOutcome, parse_apply_patch_file_outcomes,
};

/// Additional facts used while settling one observed shell transaction.
struct ShellTransactionSettlement<'a> {
    /// Exit status reported by the pane transaction.
    exit_code: i32,
    /// Optional Bubblewrap assessment retained for model recovery.
    sandbox_assessment: Option<&'a mez_agent::SandboxFailureAssessment>,
}

impl RuntimeSessionService {
    /// Applies one versioned shell-neutral adapter event.
    pub(crate) fn observe_managed_shell_protocol_event(
        &mut self,
        output_pane_id: &str,
        version: u16,
        shell: mez_terminal::ManagedShellAdapter,
        token: &str,
        event: &mez_terminal::ManagedShellProtocolEvent,
    ) -> Result<usize> {
        if version != mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION {
            return Ok(0);
        }
        let managed_shell = match shell {
            mez_terminal::ManagedShellAdapter::Bash
                if self
                    .bash_receiver_token_for_pane(output_pane_id)
                    .is_some_and(|expected| expected.as_str() == token) =>
            {
                ManagedShellKind::Bash
            }
            mez_terminal::ManagedShellAdapter::Fish
                if self
                    .fish_receiver_token_for_pane(output_pane_id)
                    .is_some_and(|expected| expected.as_str() == token) =>
            {
                ManagedShellKind::Fish
            }
            mez_terminal::ManagedShellAdapter::Zsh
                if self
                    .zsh_history_token_for_pane(output_pane_id)
                    .is_some_and(|expected| expected.as_str() == token) =>
            {
                ManagedShellKind::Zsh
            }
            _ => return Ok(0),
        };
        match event {
            mez_terminal::ManagedShellProtocolEvent::AdapterAvailable => {
                if managed_shell != ManagedShellKind::Bash {
                    return Ok(1);
                }
                let Some(primary_process_id) =
                    self.primary_pid_for_live_pane_process(output_pane_id)
                else {
                    return Ok(0);
                };
                let admission_matches = matches!(
                    self.process.pane_bash_admissions.get(output_pane_id),
                    Some(crate::runtime::processes::RuntimeManagedBashAdmission::Pending {
                        primary_process_id: expected,
                        ..
                    }) if *expected == primary_process_id
                );
                if !admission_matches {
                    return Ok(0);
                }
                self.process.pane_bash_admissions.insert(
                    output_pane_id.to_string(),
                    crate::runtime::processes::RuntimeManagedBashAdmission::Ready {
                        primary_process_id,
                        version,
                    },
                );
                if self.agent_subshell_entry_is_deferred(output_pane_id) {
                    let _ = self.enter_agent_subshell_if_needed(output_pane_id)?;
                }
                Ok(1)
            }
            mez_terminal::ManagedShellProtocolEvent::FrameAdmitted { marker } => {
                if matches!(
                    managed_shell,
                    ManagedShellKind::Fish | ManagedShellKind::Zsh
                ) && self
                    .process
                    .pane_managed_shell_handoffs
                    .get(output_pane_id)
                    .is_some_and(|handoff| {
                        handoff.shell() == managed_shell
                            && handoff.identity().marker == *marker
                            && handoff.exit_requested()
                    })
                {
                    return self.observe_managed_shell_frame_admitted_cancellation(
                        output_pane_id,
                        managed_shell,
                        token,
                        marker,
                    );
                }
                self.observe_shell_receiver_ready(output_pane_id, token, marker)
            }
            mez_terminal::ManagedShellProtocolEvent::ChildInstalled { marker } => {
                self.observe_shell_receiver_installed(output_pane_id, token, marker)
            }
            mez_terminal::ManagedShellProtocolEvent::ParentReady {
                marker,
                outcome,
                exit_code,
                proof,
            } => {
                if matches!(
                    managed_shell,
                    ManagedShellKind::Fish | ManagedShellKind::Zsh
                ) && proof.is_none()
                {
                    self.observe_managed_shell_parent_ready(
                        output_pane_id,
                        managed_shell,
                        token,
                        marker,
                        None,
                    )
                } else if managed_shell == ManagedShellKind::Bash
                    && let Some(proof) = proof
                {
                    self.observe_managed_shell_parent_ready(
                        output_pane_id,
                        ManagedShellKind::Bash,
                        token,
                        marker,
                        Some(proof),
                    )
                } else if managed_shell == ManagedShellKind::Bash
                    && matches!(
                        outcome,
                        mez_terminal::ManagedShellParentOutcome::Completed
                            | mez_terminal::ManagedShellParentOutcome::SourceFailed
                    )
                {
                    self.observe_shell_receiver_complete(output_pane_id, token, marker, *exit_code)
                } else {
                    Ok(0)
                }
            }
            mez_terminal::ManagedShellProtocolEvent::ReceiverRejected {
                marker: Some(marker),
                reason,
            } => {
                let Some(transaction) =
                    self.process.running_shell_transactions.get(marker).cloned()
                else {
                    return Ok(0);
                };
                self.fail_shell_transaction_protocol_violation(
                    marker,
                    transaction,
                    "managed-receiver-rejected",
                    format!("managed {shell:?} receiver rejected admission ({reason})"),
                )
            }
            mez_terminal::ManagedShellProtocolEvent::EditorHeld { marker } => {
                if matches!(
                    managed_shell,
                    ManagedShellKind::Fish | ManagedShellKind::Zsh
                ) {
                    return self.observe_managed_shell_editor_held(
                        output_pane_id,
                        managed_shell,
                        token,
                        marker,
                    );
                }
                let Some(handoff) = self
                    .process
                    .pane_managed_shell_handoffs
                    .get_mut(output_pane_id)
                else {
                    return Ok(0);
                };
                if handoff.shell() != managed_shell {
                    return Ok(0);
                }
                let transition = reduce_managed_shell_handoff(
                    handoff,
                    ManagedShellHandoffEvent::EditorHeld {
                        marker: marker.clone(),
                    },
                );
                Ok(usize::from(transition.applied))
            }
            mez_terminal::ManagedShellProtocolEvent::ReceiverRejected { marker: None, .. }
            | mez_terminal::ManagedShellProtocolEvent::ChildExited { .. } => Ok(0),
        }
    }

    /// Releases an authenticated BEGIN stage after native editor hold.
    fn observe_managed_shell_editor_held(
        &mut self,
        output_pane_id: &str,
        shell: ManagedShellKind,
        token: &str,
        marker: &str,
    ) -> Result<usize> {
        let Some(transaction) = self.process.running_shell_transactions.get(marker).cloned() else {
            return Ok(0);
        };
        let token_matches = match shell {
            ManagedShellKind::Fish => self
                .fish_receiver_token_for_pane(output_pane_id)
                .is_some_and(|expected| expected.as_str() == token),
            ManagedShellKind::Zsh => self
                .zsh_history_token_for_pane(output_pane_id)
                .is_some_and(|expected| expected.as_str() == token),
            ManagedShellKind::Bash => false,
        };
        if transaction.pane_id != output_pane_id || !token_matches {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "managed-editor-held-metadata-mismatch",
                "managed-shell editor-held metadata does not match runtime dispatch state",
            );
        }
        let transition = {
            let Some(handoff) = self
                .process
                .pane_managed_shell_handoffs
                .get_mut(output_pane_id)
            else {
                return Ok(0);
            };
            if handoff.shell() != shell {
                return Ok(0);
            }
            reduce_managed_shell_handoff(
                handoff,
                ManagedShellHandoffEvent::EditorHeld {
                    marker: marker.to_string(),
                },
            )
        };
        if !transition.applied {
            return Ok(0);
        }
        self.discard_unsubmitted_process_input(output_pane_id);
        let admission = self
            .process
            .shell_receiver_pending_payloads
            .get_mut(marker)
            .and_then(std::collections::VecDeque::pop_front);
        let Some(admission) = admission else {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "managed-editor-held-before-admission",
                "managed shell held its editor without a pending frame admission stage",
            );
        };
        if self
            .process
            .shell_receiver_pending_payloads
            .get(marker)
            .is_none_or(std::collections::VecDeque::is_empty)
        {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "managed-editor-held-without-payload",
                "managed-shell frame admission has no pending DATA and END payload",
            );
        }
        let admission_len = admission.bytes.len();
        if let Err(error) = self.write_runtime_pane_shell_delivery(output_pane_id, admission) {
            self.fail_shell_transactions_for_pane_write_failure(output_pane_id, error.message())?;
            return Ok(0);
        }
        self.append_agent_trace_turn_event(
            output_pane_id,
            &transaction.turn_id,
            &format!(
                "managed_shell editor_held shell={shell:?} marker={marker} admission_bytes={admission_len}"
            ),
        )?;
        Ok(1)
    }

    /// Cancels a managed frame after BEGIN admission but before DATA delivery.
    fn observe_managed_shell_frame_admitted_cancellation(
        &mut self,
        output_pane_id: &str,
        shell: ManagedShellKind,
        token: &str,
        marker: &str,
    ) -> Result<usize> {
        let Some(transaction) = self.process.running_shell_transactions.get(marker).cloned() else {
            return Ok(0);
        };
        let token_matches = match shell {
            ManagedShellKind::Fish => self.fish_receiver_token_for_pane(output_pane_id),
            ManagedShellKind::Zsh => self.zsh_history_token_for_pane(output_pane_id),
            ManagedShellKind::Bash => None,
        }
        .is_some_and(|expected| expected.as_str() == token);
        if !token_matches {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "managed-frame-admitted-cancellation-metadata-mismatch",
                "managed-shell frame admission does not match deferred cancellation ownership",
            );
        }
        if transaction.pane_id != output_pane_id {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "managed-frame-admitted-cancellation-metadata-mismatch",
                "managed-shell frame admission does not match deferred cancellation ownership",
            );
        }
        if !self.complete_managed_shell_admission_cancellation(output_pane_id)? {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "managed-frame-admitted-cancellation-phase-mismatch",
                "managed-shell cancellation reached frame admission outside pre-DATA ownership",
            );
        }
        self.append_agent_trace_turn_event(
            output_pane_id,
            &transaction.turn_id,
            &format!(
                "managed_shell receiver_cancelled shell={shell:?} marker={marker} before_data=true"
            ),
        )?;
        Ok(1)
    }

    /// Re-enters ordinary shell settlement after an internal sandbox-failure
    /// assessment declines or cannot safely request an approval.
    pub(crate) fn settle_sandbox_failure_assessment_as_command_failure(
        &mut self,
        pending: crate::runtime::RuntimeSandboxFailureAssessment,
        reason: &str,
        assessment: Option<&mez_agent::SandboxFailureAssessment>,
    ) -> Result<()> {
        let turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == pending.transaction.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("sandbox assessment turn is unavailable"))?;
        self.append_agent_trace_turn_event(
            &pending.transaction.pane_id,
            &pending.transaction.turn_id,
            &format!(
                "sandbox_failure_assessment settled action={} reason={} automatic_replay=false",
                pending.action_id, reason
            ),
        )?;
        self.process
            .running_shell_transactions
            .insert(pending.marker.clone(), pending.transaction.clone());
        let _ = self.observe_agent_shell_transaction_end_with_sandbox_assessment(
            &pending.transaction.pane_id,
            &pending.marker,
            &pending.transaction.turn_id,
            &turn.agent_id,
            &pending.transaction.pane_id,
            ShellTransactionSettlement {
                exit_code: pending.exit_code,
                sandbox_assessment: assessment,
            },
        )?;
        Ok(())
    }

    /// Sends any deferred transaction payload after the shell wrapper receiver
    /// has started.
    pub(crate) fn observe_agent_shell_transaction_start(
        &mut self,
        output_pane_id: &str,
        marker: &str,
        turn_id: &str,
        _agent_id: &str,
        pane_id: &str,
    ) -> Result<usize> {
        let Some(transaction) = self.process.running_shell_transactions.get(marker).cloned() else {
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
        if self
            .process
            .shell_transaction_started_markers
            .contains(marker)
        {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "duplicate-start-marker",
                "shell transaction emitted a duplicate start marker",
            );
        }
        self.process
            .shell_transaction_started_markers
            .insert(marker.to_string());
        if transaction.kind == RunningShellTransactionKind::Bootstrap
            && self.observe_agent_subshell_bootstrap_start(pane_id, marker)
        {
            return Ok(1);
        }
        if self
            .process
            .shell_transaction_payload_receiver_ready_required
            .contains(marker)
        {
            return Ok(1);
        }
        self.release_agent_shell_transaction_payload_after_start(marker, pane_id)?;
        Ok(1)
    }

    /// Releases a Fish payload only after its correlated start and receiver-ready events.
    pub(crate) fn observe_shell_transaction_payload_receiver_ready(
        &mut self,
        output_pane_id: &str,
        marker: &str,
        turn_id: &str,
        _agent_id: &str,
        pane_id: &str,
    ) -> Result<usize> {
        let Some(transaction) = self.process.running_shell_transactions.get(marker).cloned() else {
            return Ok(0);
        };
        if transaction.turn_id != turn_id
            || transaction.pane_id != pane_id
            || output_pane_id != pane_id
            || !self
                .process
                .shell_transaction_started_markers
                .contains(marker)
            || !self
                .process
                .shell_transaction_payload_receiver_ready_required
                .remove(marker)
        {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "payload-receiver-ready-metadata-mismatch",
                "Fish payload receiver-ready event does not match runtime dispatch state",
            );
        }
        self.release_agent_shell_transaction_payload_after_start(marker, pane_id)?;
        Ok(1)
    }

    /// Releases authenticated managed-shell source frames after frame admission.
    pub(crate) fn observe_shell_receiver_ready(
        &mut self,
        output_pane_id: &str,
        token: &str,
        marker: &str,
    ) -> Result<usize> {
        let Some(transaction) = self.process.running_shell_transactions.get(marker).cloned() else {
            return Ok(0);
        };
        let receiver_token_matches = self
            .bash_receiver_token_for_pane(output_pane_id)
            .is_some_and(|expected| expected.as_str() == token)
            || self
                .fish_receiver_token_for_pane(output_pane_id)
                .is_some_and(|expected| expected.as_str() == token)
            || self
                .zsh_history_token_for_pane(output_pane_id)
                .is_some_and(|expected| expected.as_str() == token);
        if transaction.pane_id != output_pane_id || !receiver_token_matches {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "receiver-ready-metadata-mismatch",
                "managed-shell frame admission metadata does not match runtime dispatch state",
            );
        }
        let payload = self
            .process
            .shell_receiver_pending_payloads
            .get_mut(marker)
            .and_then(std::collections::VecDeque::pop_front);
        let Some(payload) = payload else {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "unexpected-receiver-ready",
                "managed shell admitted a frame without pending private source records",
            );
        };
        if self
            .process
            .shell_receiver_pending_payloads
            .get(marker)
            .is_some_and(std::collections::VecDeque::is_empty)
        {
            self.process.shell_receiver_pending_payloads.remove(marker);
        }
        let payload_len = payload.bytes.len();
        if payload.receiver_acknowledgements {
            let acknowledgement_count = payload
                .bytes
                .split_inclusive(|byte| *byte == b'\n')
                .filter(|record| mez_mux::process::receiver_input_record_requires_ack(record))
                .count();
            self.process
                .shell_transaction_receiver_acknowledgements
                .insert(marker.to_string(), acknowledgement_count);
        }
        if let Some(transaction) = self.process.running_shell_transactions.get_mut(marker) {
            transaction.started_at_unix_ms = current_unix_millis();
        }
        if let Err(error) = self.write_runtime_pane_shell_delivery(output_pane_id, payload) {
            self.fail_shell_transactions_for_pane_write_failure(output_pane_id, error.message())?;
            return Ok(0);
        }
        if self
            .process
            .pane_managed_shell_handoffs
            .get(output_pane_id)
            .is_some_and(|handoff| handoff.identity().marker == marker)
        {
            self.mark_managed_shell_payload_released(output_pane_id, marker);
        }
        self.append_agent_trace_turn_event(
            output_pane_id,
            &transaction.turn_id,
            &format!("shell_receiver admitted marker={marker} bytes={payload_len}"),
        )?;
        Ok(1)
    }

    /// Records authenticated availability of a non-destructive managed-zsh trigger.
    pub(crate) fn observe_zsh_shell_receiver_available(
        &mut self,
        output_pane_id: &str,
        token: &str,
        shell: &str,
        trigger: &str,
    ) -> Result<usize> {
        if shell != "zsh"
            || self
                .zsh_history_token_for_pane(output_pane_id)
                .is_none_or(|expected| expected.as_str() != token)
        {
            return Ok(0);
        }
        let Some(trigger) = mez_agent::ManagedZshTrigger::from_protocol_str(trigger) else {
            return Ok(0);
        };
        let Some(primary_process_id) = self.primary_pid_for_live_pane_process(output_pane_id)
        else {
            return Ok(0);
        };
        self.process.pane_zsh_admissions.insert(
            output_pane_id.to_string(),
            crate::runtime::processes::RuntimeManagedZshAdmission::Ready {
                primary_process_id,
                trigger,
            },
        );
        if self.agent_subshell_entry_is_deferred(output_pane_id)
            && self
                .agent_shell_store()
                .get(output_pane_id)
                .is_some_and(|session| {
                    session.visibility == mez_agent::AgentShellVisibility::Visible
                })
        {
            let _ = self.enter_agent_subshell_if_needed(output_pane_id)?;
        }
        Ok(1)
    }

    /// Records an authenticated managed-zsh admission failure without touching the parent shell.
    pub(crate) fn observe_zsh_shell_receiver_unavailable(
        &mut self,
        output_pane_id: &str,
        token: &str,
        shell: &str,
        reason: &str,
    ) -> Result<usize> {
        if shell != "zsh"
            || self
                .zsh_history_token_for_pane(output_pane_id)
                .is_none_or(|expected| expected.as_str() != token)
        {
            return Ok(0);
        }
        self.process.pane_zsh_admissions.insert(
            output_pane_id.to_string(),
            crate::runtime::processes::RuntimeManagedZshAdmission::Unavailable {
                reason: reason.to_string(),
            },
        );
        if self.clear_deferred_agent_subshell_entry(output_pane_id) {
            self.append_agent_status_text_to_terminal_buffer(
                output_pane_id,
                &format!("agent: managed zsh integration unavailable ({reason})"),
            )?;
        }
        Ok(1)
    }

    /// Releases authenticated HOLD metadata after ZLE starts its fixed receiver.
    pub(crate) fn observe_zsh_shell_receiver_awaiting(
        &mut self,
        output_pane_id: &str,
        token: &str,
    ) -> Result<usize> {
        if self
            .zsh_history_token_for_pane(output_pane_id)
            .is_none_or(|expected| expected.as_str() != token)
        {
            return Ok(0);
        }
        let Some(marker) = self
            .process
            .pane_shell_handoffs
            .get(output_pane_id)
            .and_then(|handoff| handoff.bootstrap_marker.clone())
        else {
            return Ok(0);
        };
        let Some(transaction) = self
            .process
            .running_shell_transactions
            .get(&marker)
            .cloned()
        else {
            return Ok(0);
        };
        if transaction.pane_id != output_pane_id {
            return self.fail_shell_transaction_protocol_violation(
                &marker,
                transaction,
                "zsh-receiver-awaiting-pane-mismatch",
                "Zsh receiver-awaiting event does not match the pending subshell handoff",
            );
        }
        let payload = self
            .process
            .shell_receiver_pending_payloads
            .get_mut(&marker)
            .and_then(std::collections::VecDeque::pop_front);
        let Some(payload) = payload else {
            return self.fail_shell_transaction_protocol_violation(
                &marker,
                transaction,
                "unexpected-zsh-receiver-awaiting",
                "Zsh receiver emitted awaiting without pending HOLD metadata",
            );
        };
        if self
            .process
            .shell_receiver_pending_payloads
            .get(&marker)
            .is_none_or(|pending| pending.len() < 2)
        {
            return self.fail_shell_transaction_protocol_violation(
                &marker,
                transaction,
                "zsh-receiver-awaiting-without-staged-frame",
                "Zsh HOLD metadata has no pending BEGIN and DATA/END stages",
            );
        }
        let payload_len = payload.bytes.len();
        if let Err(error) = self.write_runtime_pane_shell_delivery(output_pane_id, payload) {
            self.fail_shell_transactions_for_pane_write_failure(output_pane_id, error.message())?;
            return Err(error);
        }
        self.append_agent_trace_turn_event(
            output_pane_id,
            &transaction.turn_id,
            &format!("zsh_shell_receiver awaiting_hold marker={marker} bytes={payload_len}"),
        )?;
        Ok(1)
    }

    /// Releases a deferred agent-subshell bootstrap trigger after the managed
    /// child proves that its private receiver is installed and waiting.
    pub(crate) fn observe_shell_receiver_installed(
        &mut self,
        output_pane_id: &str,
        token: &str,
        marker: &str,
    ) -> Result<usize> {
        let Some(transaction) = self.process.running_shell_transactions.get(marker).cloned() else {
            return Ok(0);
        };
        let fish_receiver_installed = self
            .fish_receiver_token_for_pane(output_pane_id)
            .is_some_and(|expected| expected.as_str() == token);
        let zsh_receiver_installed = self
            .zsh_history_token_for_pane(output_pane_id)
            .is_some_and(|expected| expected.as_str() == token);
        let handoff_matches = self
            .process
            .pane_shell_handoffs
            .get(output_pane_id)
            .is_some_and(|handoff| handoff.bootstrap_marker.as_deref() == Some(marker));
        let receiver_token_matches = self
            .bash_receiver_token_for_pane(output_pane_id)
            .is_some_and(|expected| expected.as_str() == token)
            || self
                .fish_receiver_token_for_pane(output_pane_id)
                .is_some_and(|expected| expected.as_str() == token)
            || zsh_receiver_installed;
        if transaction.pane_id != output_pane_id || !receiver_token_matches || !handoff_matches {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "receiver-installed-metadata-mismatch",
                "managed receiver-installed metadata does not match the pending subshell handoff",
            );
        }
        let managed_handoff_installed = self
            .process
            .pane_managed_shell_handoffs
            .get(output_pane_id)
            .is_some_and(|handoff| handoff.identity().marker == marker);
        let managed_exit_requested = if managed_handoff_installed {
            let Some(exit_requested) =
                self.mark_managed_shell_child_installed(output_pane_id, marker)
            else {
                return self.fail_shell_transaction_protocol_violation(
                    marker,
                    transaction,
                    "receiver-installed-phase-mismatch",
                    "managed receiver-installed event arrived outside source-delivery ownership",
                );
            };
            exit_requested
        } else {
            false
        };
        if managed_exit_requested {
            let Some(mut cancelled_bootstrap) =
                self.cancel_agent_subshell_bootstrap_for_exit(output_pane_id)
            else {
                return self.fail_shell_transaction_protocol_violation(
                    marker,
                    transaction,
                    "receiver-installed-exit-settlement-mismatch",
                    "managed child installation could not settle its deferred bootstrap on exit",
                );
            };
            self.enter_agent_subshell(output_pane_id);
            self.mark_agent_subshell_command_exit(output_pane_id.to_string());
            self.remember_hidden_shell_render_suppression(output_pane_id);
            self.remember_agent_subshell_exit_echo(output_pane_id);
            cancelled_bootstrap.extend_from_slice(b"exit\n");
            self.write_runtime_pane_input(output_pane_id, &cancelled_bootstrap)?;
            return Ok(1);
        }
        let wrapper = self
            .process
            .pane_shell_handoffs
            .get_mut(output_pane_id)
            .and_then(|handoff| handoff.deferred_bootstrap_wrapper.take());
        let Some(wrapper) = wrapper else {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "unexpected-receiver-installed",
                "managed child reported receiver installation without a deferred bootstrap trigger",
            );
        };
        if let Err(error) = self.write_runtime_pane_shell_input(output_pane_id, wrapper.as_bytes())
        {
            self.fail_shell_transactions_for_pane_write_failure(output_pane_id, error.message())?;
            return Err(error);
        }
        self.enter_agent_subshell(output_pane_id);
        if fish_receiver_installed || zsh_receiver_installed {
            self.mark_agent_subshell_command_exit(output_pane_id.to_string());
        } else {
            self.take_agent_subshell_command_exit(output_pane_id);
        }
        self.remember_hidden_shell_render_suppression(output_pane_id);
        self.record_bootstrap_sent(output_pane_id, marker)?;
        Ok(1)
    }

    /// Settles a managed receiver transaction only after callback cleanup completes.
    pub(crate) fn observe_shell_receiver_complete(
        &mut self,
        output_pane_id: &str,
        token: &str,
        marker: &str,
        receiver_exit_code: i32,
    ) -> Result<usize> {
        let Some(transaction) = self.process.running_shell_transactions.get(marker).cloned() else {
            return Ok(0);
        };
        let receiver_token_matches = self
            .bash_receiver_token_for_pane(output_pane_id)
            .is_some_and(|expected| expected.as_str() == token)
            || self
                .fish_receiver_token_for_pane(output_pane_id)
                .is_some_and(|expected| expected.as_str() == token)
            || self
                .zsh_history_token_for_pane(output_pane_id)
                .is_some_and(|expected| expected.as_str() == token);
        if transaction.pane_id != output_pane_id
            || !receiver_token_matches
            || !self
                .process
                .shell_receiver_completion_required
                .remove(marker)
        {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "receiver-complete-metadata-mismatch",
                "managed receiver-complete metadata does not match runtime dispatch state",
            );
        }
        let Some((turn_id, agent_id, pane_id, exit_code)) =
            self.process.shell_receiver_pending_ends.remove(marker)
        else {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction,
                "receiver-complete-before-end",
                "managed receiver completed before the evaluated transaction emitted its end marker",
            );
        };
        self.append_agent_trace_turn_event(
            output_pane_id,
            &transaction.turn_id,
            &format!(
                "shell_receiver completed marker={marker} receiver_exit_code={receiver_exit_code}"
            ),
        )?;
        self.observe_agent_shell_transaction_end(
            output_pane_id,
            marker,
            &turn_id,
            &agent_id,
            &pane_id,
            exit_code,
        )
    }

    /// Settles managed-shell parent restoration independently from bootstrap completion.
    ///
    /// Fish sources the persistent child synchronously, so its bootstrap
    /// transaction normally settles long before control returns to the parent
    /// line editor. The authenticated restoration event therefore owns queued
    /// foreground input even when no shell transaction remains.
    pub(crate) fn observe_shell_parent_restored(
        &mut self,
        output_pane_id: &str,
        token: &str,
        marker: &str,
        receiver_exit_code: i32,
    ) -> Result<usize> {
        let fish_token_matches = self
            .fish_receiver_token_for_pane(output_pane_id)
            .is_some_and(|expected| expected.as_str() == token);
        let zsh_token_matches = self
            .zsh_history_token_for_pane(output_pane_id)
            .is_some_and(|expected| expected.as_str() == token);
        if !fish_token_matches && !zsh_token_matches {
            return self.observe_shell_receiver_complete(
                output_pane_id,
                token,
                marker,
                receiver_exit_code,
            );
        }
        let shell = if fish_token_matches {
            ManagedShellKind::Fish
        } else if zsh_token_matches {
            ManagedShellKind::Zsh
        } else {
            return self.observe_shell_receiver_complete(
                output_pane_id,
                token,
                marker,
                receiver_exit_code,
            );
        };
        self.observe_managed_shell_parent_ready(output_pane_id, shell, token, marker, None)
    }

    /// Settles one identity-fenced managed-shell parent-ready event.
    fn observe_managed_shell_parent_ready(
        &mut self,
        output_pane_id: &str,
        shell: ManagedShellKind,
        token: &str,
        marker: &str,
        parent_proof: Option<&str>,
    ) -> Result<usize> {
        let Some(handoff) = self
            .process
            .pane_managed_shell_handoffs
            .get(output_pane_id)
            .cloned()
        else {
            return Ok(0);
        };
        let current_primary_process_id = self.primary_pid_for_live_pane_process(output_pane_id);
        let current_interaction_generation = self
            .process
            .pane_shell_interaction_generations
            .get(output_pane_id)
            .copied();
        let shell_token_matches = match handoff.shell() {
            ManagedShellKind::Bash => self
                .bash_receiver_token_for_pane(output_pane_id)
                .is_some_and(|expected| expected.as_str() == token),
            ManagedShellKind::Fish => self
                .fish_receiver_token_for_pane(output_pane_id)
                .is_some_and(|expected| expected.as_str() == token),
            ManagedShellKind::Zsh => self
                .zsh_history_token_for_pane(output_pane_id)
                .is_some_and(|expected| expected.as_str() == token),
        };
        if handoff.shell() != shell || !shell_token_matches {
            return Ok(0);
        }
        let current_identity = ManagedShellHandoffIdentity {
            marker: marker.to_string(),
            process_instance: self.adapter_owned_pane_process_instance(output_pane_id),
            primary_process_id: current_primary_process_id,
            interaction_generation: current_interaction_generation,
            parent_proof: parent_proof.map(ToOwned::to_owned),
        };
        let transition = {
            let Some(current) = self
                .process
                .pane_managed_shell_handoffs
                .get_mut(output_pane_id)
            else {
                return Ok(0);
            };
            reduce_managed_shell_handoff(
                current,
                ManagedShellHandoffEvent::ParentReady {
                    identity: current_identity,
                },
            )
        };
        let Some(pending_input) = transition
            .effects
            .into_iter()
            .find_map(|effect| match effect {
                ManagedShellHandoffEffect::Settle { pending_input, .. } => Some(pending_input),
                _ => None,
            })
        else {
            return Ok(0);
        };
        let bootstrap_rejected = self
            .process
            .running_shell_transactions
            .get(marker)
            .is_some_and(|transaction| {
                transaction.pane_id == output_pane_id
                    && transaction.kind == RunningShellTransactionKind::Bootstrap
            });
        let resume_deferred_entry = !bootstrap_rejected
            && self.agent_subshell_entry_is_deferred(output_pane_id)
            && self
                .agent_shell_store()
                .get(output_pane_id)
                .is_some_and(|session| {
                    session.visibility == mez_agent::AgentShellVisibility::Visible
                });
        if bootstrap_rejected {
            self.remove_running_shell_transaction(marker);
            self.clear_shell_transaction_protocol_state(marker);
            self.process.pane_bootstrap_pending.remove(output_pane_id);
        }
        self.clear_agent_subshell_shell_identity(output_pane_id);
        if bootstrap_rejected {
            self.mark_pane_environment_authority_unavailable(
                output_pane_id,
                RuntimePaneEnvironmentAuthorityUnavailableReason::BootstrapTransactionFailed,
            );
            self.set_pane_readiness(output_pane_id, PaneReadinessState::Degraded);
            self.append_agent_status_text_to_terminal_buffer(
                output_pane_id,
                "agent: managed shell parent rejected the private shell handoff",
            )?;
        } else if self
            .process
            .pane_environment_authority_failures
            .contains_key(output_pane_id)
        {
            self.set_pane_readiness(output_pane_id, PaneReadinessState::Degraded);
        } else {
            self.set_pane_readiness(output_pane_id, PaneReadinessState::PromptCandidate);
        }

        self.settle_managed_shell_runtime_ownership(output_pane_id, pending_input)?;
        if resume_deferred_entry {
            let _ = self.enter_agent_subshell_if_needed(output_pane_id)?;
        }
        Ok(1)
    }

    /// Releases a deferred transaction payload after its start proof settles.
    pub(crate) fn release_agent_shell_transaction_payload_after_start(
        &mut self,
        marker: &str,
        pane_id: &str,
    ) -> Result<()> {
        let Some(transaction) = self.process.running_shell_transactions.get(marker).cloned() else {
            return Ok(());
        };
        let kind_name = runtime_running_shell_transaction_kind_name(&transaction.kind).to_string();
        let payload = self
            .process
            .running_shell_transactions
            .get_mut(marker)
            .and_then(|transaction| transaction.pending_input_payload.take());
        if let Some(transaction) = self.process.running_shell_transactions.get_mut(marker) {
            transaction.started_at_unix_ms = current_unix_millis();
        }
        let Some(payload) = payload else {
            return Ok(());
        };
        let payload_len = payload.bytes.len();
        if payload.receiver_acknowledgements {
            let acknowledgement_count = payload
                .bytes
                .split_inclusive(|byte| *byte == b'\n')
                .filter(|record| mez_mux::process::receiver_input_record_requires_ack(record))
                .count();
            self.process
                .shell_transaction_receiver_acknowledgements
                .insert(marker.to_string(), acknowledgement_count);
        }
        if let Err(error) = self.write_runtime_pane_shell_delivery(pane_id, payload) {
            self.fail_shell_transactions_for_pane_write_failure(pane_id, error.message())?;
            return Ok(());
        }
        self.append_agent_trace_turn_event(
            pane_id,
            &transaction.turn_id,
            &format!(
                "shell_transaction payload_sent marker={} kind={} bytes={}",
                marker, kind_name, payload_len
            ),
        )?;
        Ok(())
    }

    /// Runs the observe agent shell transaction end operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn observe_agent_shell_transaction_end(
        &mut self,
        output_pane_id: &str,
        marker: &str,
        turn_id: &str,
        agent_id: &str,
        pane_id: &str,
        exit_code: i32,
    ) -> Result<usize> {
        self.observe_agent_shell_transaction_end_with_sandbox_assessment(
            output_pane_id,
            marker,
            turn_id,
            agent_id,
            pane_id,
            ShellTransactionSettlement {
                exit_code,
                sandbox_assessment: None,
            },
        )
    }

    /// Settles one shell transaction while optionally retaining a bounded
    /// Bubblewrap assessment for the acting model's next recovery decision.
    fn observe_agent_shell_transaction_end_with_sandbox_assessment(
        &mut self,
        output_pane_id: &str,
        marker: &str,
        turn_id: &str,
        agent_id: &str,
        pane_id: &str,
        settlement: ShellTransactionSettlement<'_>,
    ) -> Result<usize> {
        let ShellTransactionSettlement {
            exit_code,
            sandbox_assessment,
        } = settlement;
        let Some(transaction_ref) = self.process.running_shell_transactions.get(marker).cloned()
        else {
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
            .process
            .shell_transaction_require_start_markers
            .contains(marker)
            && !self
                .process
                .shell_transaction_started_markers
                .contains(marker)
        {
            return self.fail_shell_transaction_protocol_violation(
                marker,
                transaction_ref,
                "end-marker-before-start-marker",
                "shell transaction end marker arrived before the start marker",
            );
        }
        if self
            .process
            .shell_receiver_completion_required
            .contains(marker)
        {
            if self
                .process
                .shell_receiver_pending_ends
                .contains_key(marker)
            {
                return self.fail_shell_transaction_protocol_violation(
                    marker,
                    transaction_ref,
                    "duplicate-end-before-receiver-complete",
                    "Bash transaction emitted duplicate end markers before receiver completion",
                );
            }
            self.process.shell_receiver_pending_ends.insert(
                marker.to_string(),
                (
                    turn_id.to_string(),
                    agent_id.to_string(),
                    pane_id.to_string(),
                    exit_code,
                ),
            );
            return Ok(1);
        }
        let Some(mut transaction_ref) = self.remove_running_shell_transaction(marker) else {
            return Ok(0);
        };
        let sandboxed = self
            .process
            .sandboxed_shell_transaction_markers
            .contains(marker);
        self.clear_shell_transaction_protocol_state(marker);
        if transaction_ref.kind == RunningShellTransactionKind::FocusedShellHook {
            return self.observe_focused_shell_hook_transaction_end(
                output_pane_id,
                marker,
                pane_id,
                exit_code,
            );
        }
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
        if matches!(
            transaction_ref.kind,
            RunningShellTransactionKind::ShellIdentityProbe { .. }
        ) {
            return self.observe_shell_identity_probe_transaction_end(
                marker,
                exit_code,
                &transaction_ref,
            );
        }
        if let RunningShellTransactionKind::PathResolution { cache_key, waiters } =
            transaction_ref.kind.clone()
        {
            let observed = self.observe_path_resolution_transaction_end(
                marker,
                pane_id,
                exit_code,
                cache_key.clone(),
                &transaction_ref.observed_output_preview,
                transaction_ref.observed_output_truncated,
            )?;
            if !waiters.is_empty() {
                return self.settle_action_path_resolution_transaction(
                    marker,
                    &transaction_ref,
                    &cache_key,
                    &waiters,
                );
            }
            return Ok(observed);
        }
        if let RunningShellTransactionKind::EnvironmentEvidence { cache_key, waiters } =
            transaction_ref.kind.clone()
        {
            return self.observe_environment_evidence_transaction_end(
                marker,
                &transaction_ref,
                exit_code,
                &cache_key,
                &waiters,
            );
        }
        if matches!(
            transaction_ref.kind,
            RunningShellTransactionKind::BubblewrapCapabilityProbe { .. }
        ) {
            return self.observe_bubblewrap_capability_probe_transaction_end(
                marker,
                transaction_ref,
                exit_code,
            );
        }
        let RunningShellTransactionKind::AgentAction { ref action_id } = transaction_ref.kind
        else {
            return Err(MezError::invalid_state(
                "shell transaction kind was not handled",
            ));
        };
        let turn = self
            .agent_turn_ledger()
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
        if sandboxed {
            let status = if transaction_ref.observed_output_truncated {
                Err("Bubblewrap status transport was truncated".to_string())
            } else {
                mez_agent::decode_shell_status_transport(&transaction_ref.observed_output_preview)
                    .map_err(|error| error.message().to_string())
                    .and_then(|status| {
                        crate::security::sandbox::parse_bubblewrap_status(&status)
                            .map_err(|error| error.message().to_string())
                    })
            };
            match status {
                Ok(status) if status.exit_code.is_none() => {
                    self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
                    if self.offer_sandbox_pre_payload_fallback_approval(
                        marker,
                        turn_id,
                        action_id,
                        "trusted_status_closed_without_exit_code",
                    )? {
                        return Ok(1);
                    }
                    let message = crate::security::sandbox::bubblewrap_failure_remediation(
                        "Bubblewrap failed before payload execution",
                    );
                    return self.fail_running_shell_transaction_action(
                        &transaction_ref,
                        marker,
                        RuntimeShellTransactionActionFailure {
                            action_id: action_id.clone(),
                            status: ActionStatus::Failed,
                            code: "bubblewrap_pre_payload_failure".to_string(),
                            message,
                            sent_to_pane: true,
                            terminal_observation: serde_json::json!({
                                "source": "bubblewrap_status",
                                "marker": marker,
                                "exit_code": null,
                                "payload_exec_proven": false,
                                "boundary_state": "bubblewrap-pre-payload-failure"
                            }),
                            trace_reason: "bubblewrap_pre_payload_failure".to_string(),
                        },
                    );
                }
                Ok(status) if status.exit_code != Some(exit_code) => {
                    let message = crate::security::sandbox::bubblewrap_failure_remediation(
                        "Bubblewrap status exit code contradicts the shell transaction",
                    );
                    return self.fail_running_shell_transaction_action(
                        &transaction_ref,
                        marker,
                        RuntimeShellTransactionActionFailure {
                            action_id: action_id.clone(),
                            status: ActionStatus::Failed,
                            code: "bubblewrap_status_mismatch".to_string(),
                            message,
                            sent_to_pane: true,
                            terminal_observation: serde_json::json!({
                                "source": "bubblewrap_status",
                                "marker": marker,
                                "exit_code": exit_code,
                                "reported_exit_code": status.exit_code,
                                "boundary_state": "bubblewrap-status-mismatch"
                            }),
                            trace_reason: "bubblewrap_status_mismatch".to_string(),
                        },
                    );
                }
                Err(message) => {
                    let failure_message = crate::security::sandbox::bubblewrap_failure_remediation(
                        &format!("Bubblewrap status was invalid: {message}"),
                    );
                    return self.fail_running_shell_transaction_action(
                        &transaction_ref,
                        marker,
                        RuntimeShellTransactionActionFailure {
                            action_id: action_id.clone(),
                            status: ActionStatus::Failed,
                            code: "bubblewrap_status_invalid".to_string(),
                            message: failure_message,
                            sent_to_pane: true,
                            terminal_observation: serde_json::json!({
                                "source": "bubblewrap_status",
                                "marker": marker,
                                "exit_code": exit_code,
                                "boundary_state": "bubblewrap-status-invalid",
                                "status_error": message
                            }),
                            trace_reason: "bubblewrap_status_invalid".to_string(),
                        },
                    );
                }
                Ok(_) => {
                    if exit_code != 0
                        && self.queue_sandbox_failure_assessment(
                            &turn,
                            action_id,
                            marker,
                            transaction_ref.clone(),
                            exit_code,
                        )?
                    {
                        return Ok(1);
                    }
                }
            }
        }
        if self.dispatch_apply_patch_followup_if_needed(
            &turn,
            action_id,
            &transaction_ref,
            exit_code,
        )? {
            return Ok(1);
        }
        self.append_sandbox_fallback_result_audit(
            turn_id,
            action_id,
            if exit_code == 0 {
                "succeeded"
            } else {
                "failed"
            },
        )?;
        self.clear_sandbox_bypass_for_action(turn_id, action_id);

        let (
            mut terminal_state,
            ready_for_provider_continuation,
            post_shell_hook_payload,
            action_transition_trace,
            observed_result,
            observed_results,
            observed_action,
            display_output_after_completion,
            apply_patch_file_outcomes,
        ) = {
            let execution = self
                .agent_turn_executions_mut()
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
                let result_is_running = execution.action_results.iter().any(|result| {
                    result.action_id == candidate.id && result.status == ActionStatus::Running
                });
                if result_is_running && local_action_plan(candidate)?.is_some() {
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
            let is_apply_patch_write =
                matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
                    && apply_patch_transaction_phase(&transaction_ref.command)
                        == Some(ApplyPatchTransactionPhase::Write);
            let apply_patch_file_outcomes = if is_apply_patch_write
                && !transaction_ref.observed_output_truncated
                && !transport_diagnostics.output_truncated()
                && !transport_diagnostics.transport_incomplete()
            {
                parse_apply_patch_file_outcomes(&transaction_ref.observed_output_preview)
                    .ok()
                    .filter(|outcomes| !outcomes.is_empty())
            } else {
                None
            };
            if apply_patch_file_outcomes.is_some() {
                transaction_ref.observed_output_preview = transaction_ref
                    .observed_output_preview
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .lines()
                    .filter(|line| !line.starts_with(APPLY_PATCH_RESULT_MARKER))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            transaction_ref.observed_output_bytes = transaction_ref.observed_output_preview.len();
            if exit_code == 0 {
                let processed_output = postprocess_shell_action_success_output(
                    &action,
                    transaction_ref.observed_output_preview.clone(),
                );
                transaction_ref.observed_output_preview = processed_output;
                transaction_ref.observed_output_bytes =
                    transaction_ref.observed_output_preview.len();
            }
            let signal: Option<i32> = if exit_code > 128 && exit_code < 256 {
                Some(exit_code - 128)
            } else {
                None
            };
            let sandbox_assessment = sandbox_assessment.map(|assessment| {
                serde_json::json!({
                    "class": assessment.class.as_str(),
                    "decision": assessment.decision.as_str(),
                    "confidence": assessment.confidence,
                    "rationale": assessment.rationale,
                    "restriction_id": assessment.restriction_id,
                    "sandboxed_recovery_exhausted": assessment.sandboxed_recovery_exhausted,
                    "bubblewrap_status": "payload_executed_nonzero",
                    "partial_effect_warning": true,
                    "automatic_replay": false
                })
            });
            let structured_content = mez_agent::shell_action_structured_content_json(
                &action,
                &local_plan,
                Some("pane_shell"),
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
                    "transport_diagnostics": transport_diagnostics.to_json(),
                    "sandbox_assessment": sandbox_assessment
                }),
            );
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
                let (failure_code, failure_message) = shell_action_failure_diagnostic(
                    &action,
                    exit_code,
                    &transaction_ref.observed_output_preview,
                    &transaction_ref.command,
                );
                let mut result = ActionResult::failed(
                    &turn,
                    &action,
                    ActionStatus::Failed,
                    failure_code,
                    failure_message,
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
                    let skipped_plan = local_action_plan(skipped_action)?.ok_or_else(|| {
                        MezError::invalid_state(
                            "pending shell result does not match shell-backed action payload",
                        )
                    })?;
                    let structured_content = mez_agent::shell_action_structured_content_json(
                        skipped_action,
                        &skipped_plan,
                        Some("pane_shell"),
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
                    );
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
            let post_shell_hook_payload =
                runtime_post_shell_hook_payload(&turn, &action, &observed_result, exit_code);
            let ready_for_provider_continuation = shell_command_nonzero_result
                || runtime_execution_ready_for_provider_continuation(execution);
            (
                execution.terminal_state,
                ready_for_provider_continuation,
                post_shell_hook_payload,
                action_transition_trace,
                observed_result,
                observed_results,
                action,
                local_plan.display_output_after_completion,
                apply_patch_file_outcomes,
            )
        };
        self.integration
            .runtime_metrics_mut()
            .record_shell_transaction_completion(
                transaction_ref.started_at_unix_ms,
                current_unix_millis(),
                transaction_ref.observed_output_bytes,
                exit_code,
            );
        if exit_code == 0 {
            self.record_shell_dispatch_success(turn_id, &transaction_ref.command);
        }
        let confirmed_partial_apply = apply_patch_file_outcomes.as_ref().is_some_and(|outcomes| {
            outcomes
                .iter()
                .any(|outcome| matches!(outcome, ApplyPatchFileOutcome::Applied { .. }))
        });
        let is_apply_patch_write = matches!(
            observed_action.payload,
            AgentActionPayload::ApplyPatch { .. }
        ) && apply_patch_transaction_phase(&transaction_ref.command)
            == Some(ApplyPatchTransactionPhase::Write);
        if is_apply_patch_write && (exit_code == 0 || confirmed_partial_apply) {
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
        if let Some(execution) = self.agent_turn_executions().get(turn_id).cloned() {
            self.record_runtime_agent_patch_results_for_turn(&turn, &execution);
        }
        if (exit_code == 0 || confirmed_partial_apply)
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
        if self.agent_verbose_enabled(pane_id)
            && let Some(outcomes) = &apply_patch_file_outcomes
        {
            for outcome in outcomes {
                if let ApplyPatchFileOutcome::Failed { path, diagnostic } = outcome {
                    let diagnostic = diagnostic.trim();
                    let message = if diagnostic.is_empty() {
                        format!("agent: apply patch failed: {path}")
                    } else {
                        format!("agent: apply patch failed: {path}: {diagnostic}")
                    };
                    self.append_agent_error_text_to_terminal_buffer(pane_id, &message)?;
                }
            }
        }

        self.run_configured_completed_hooks(HookEvent::PostShellCommand, &post_shell_hook_payload)?;

        let mut transcript_entries = 0usize;
        if matches!(
            terminal_state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
            let mut execution = self
                .agent_turn_executions()
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
                self.agent_turn_executions_mut().remove(turn_id);
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
            self.commit_settled_action_results_context(turn_id, &observed_results)?;
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
            if ready_for_provider_continuation {
                self.queue_agent_provider_task(turn_id.to_string());
                self.append_agent_trace_turn_event(
                    pane_id,
                    turn_id,
                    "provider_task queued reason=shell_transaction_result_ready",
                )?;
            } else {
                let should_dispatch_stored_shell = self
                    .agent_turn_executions()
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
