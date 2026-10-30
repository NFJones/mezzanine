//! Publication-only recovery for the existing completion receipt owner.
//!
//! Canonical execution/context and required gates precede this module. It does
//! not consume worker outcomes, re-enter projection, replay hooks/audit, or issue
//! completed effects. Each pump snapshots existing keys and revalidates their
//! exact live owner before use. Required-failed/revoked owners retain evidence
//! but are never advertised as actionable paint work. A successful native action
//! prefix may hand an unfinished admitted sibling to the existing dispatcher;
//! this is distinct from acknowledging the group or scheduling another model.

use super::*;

/// Publication progress carries distinct execution and whole-group authority.
/// An accepted action prefix permits only an already admitted sibling handoff;
/// it never acknowledges the held group or authorizes provider continuation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::runtime::agent) enum NativePublicationProgress {
    /// Current ownership or this action's ordered prefix remains unsettled.
    Held,
    /// This action's prefix installed, but a later group component is held.
    ActionPublished,
    /// All ordered components installed; existing group gates still apply.
    GroupPublished,
}

impl RuntimeSessionService {
    /// Checks live paint binding independently of retired worker delivery maps.
    fn completion_paint_owner_is_current(&self, owner: &ExternalCompletionOwner) -> bool {
        self.agent
            .completion_settlements
            .groups
            .get(&owner.turn_id)
            .is_some_and(|group| {
                group.group == owner.group
                    && !group.paint_revoked
                    && group.primary_failure.is_none()
            })
            && self
                .completion_owner(&owner.turn_id, &owner.action_id, &owner.attempt, true)
                .is_ok_and(|current| current == *owner)
    }

    /// A single settled sibling never grants group continuation across another
    /// receipt's pending delivery, required gate, raw payload or publication.
    pub(in crate::runtime::agent) fn completion_group_publication_ready(
        &self,
        turn_id: &str,
    ) -> bool {
        self.ensure_completion_required_gates_ready(turn_id).is_ok()
            && self
                .agent
                .completion_settlements
                .external
                .values()
                .filter(|receipt| receipt.owner.turn_id == turn_id)
                .all(|receipt| {
                    receipt.raw.is_none()
                        && receipt.failure.is_none()
                        && receipt.phase == ExternalCompletionPhase::PublicationAccepted
                })
            && self
                .agent
                .completion_settlements
                .native
                .values()
                .filter(|receipt| receipt.owner.turn_id == turn_id)
                .all(|receipt| {
                    receipt.raw.is_none()
                        && receipt.failure.is_none()
                        && matches!(
                            receipt.phase,
                            NativeCompletionPhase::PublicationAccepted
                                | NativeCompletionPhase::NextPhaseOwned
                        )
                })
    }

    /// Checks immutable ordered components in the exact group, optionally only
    /// through an action ordinal. A prefix check is dispatch eligibility, never
    /// an acknowledgement of whole-group publication.
    fn completion_ordered_publication_pending(
        &self,
        owner: &ExternalCompletionOwner,
        through: Option<usize>,
    ) -> bool {
        self.presentation
            .agent_queued_provider_headers
            .keys()
            .chain(self.presentation.agent_queued_provider_commands.keys())
            .chain(self.presentation.agent_queued_provider_results.keys())
            .chain(self.presentation.agent_queued_provider_outcomes.keys())
            .any(|(pane, turn, conversation, group, ordinal)| {
                pane == &owner.pane_id
                    && turn == &owner.turn_id
                    && conversation == &owner.conversation_id
                    && group == &owner.group
                    && through.is_none_or(|bound| *ordinal <= bound)
            })
    }

    /// Replays only idempotent progress reconciliation and immutable RESULT
    /// installation. Trace is optional, attempted once, and never a required gate.
    pub(in crate::runtime::agent) fn publish_retained_external_completion(
        &mut self,
        key: &ExternalCompletionKey,
    ) -> Result<bool> {
        let owner = self
            .agent
            .completion_settlements
            .external
            .get(key)
            .ok_or_else(|| MezError::invalid_state("external publication owner is unavailable"))?
            .owner
            .clone();
        if !self.completion_paint_owner_is_current(&owner) {
            return Ok(false);
        }
        let execution = self
            .agent_turn_executions()
            .get(&key.0)
            .cloned()
            .ok_or_else(|| {
                MezError::invalid_state("external publication execution is unavailable")
            })?;
        let action = execution
            .response
            .action_batch
            .as_ref()
            .and_then(|batch| batch.actions.get(owner.ordinal))
            .cloned()
            .ok_or_else(|| MezError::invalid_state("external publication action is unavailable"))?;
        let result = execution
            .action_results
            .iter()
            .find(|result| result.action_id == key.1)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("external publication result is unavailable"))?;
        let publication = (|| -> Result<bool> {
            if !matches!(action.payload, AgentActionPayload::McpCall { .. }) {
                self.external_completion_phase(key, ExternalCompletionPhase::ProgressPublication)?;
                let suppressed = self.reconcile_action_presentation_progress_for_execution(
                    &key.0,
                    &key.1,
                    &mez_agent::ActionPresentationExecutionIdentity::Attempt(key.2.clone()),
                    &result,
                )?;
                if !result.is_error && !suppressed && self.agent_verbose_enabled(&owner.pane_id) {
                    self.external_completion_phase(
                        key,
                        ExternalCompletionPhase::ResultPublication,
                    )?;
                    self.queue_ordered_provider_result(
                        &owner.pane_id,
                        &execution,
                        &action,
                        &result,
                    )?;
                    let installed = self
                        .presentation
                        .agent_published_provider_results
                        .contains(&(
                            (
                                owner.pane_id.clone(),
                                owner.turn_id.clone(),
                                owner.conversation_id.clone(),
                                owner.group.clone(),
                                owner.ordinal,
                            ),
                            result.content_text(),
                        ));
                    if !installed && !self.external_result_publication_is_retained(key, &result) {
                        return Err(MezError::invalid_state(
                            "external RESULT has no exact presentation owner",
                        ));
                    }
                }
            }
            // Terminal outcome lines are canonical display source, exactly like
            // the native path's publication step. Presenting them here keeps the
            // existing ordered barrier honest: a failed sibling's outcome must be
            // acknowledged before a later sibling's components may install, and
            // it never re-executes the completed action or re-runs required gates.
            self.present_agent_action_outcomes_to_terminal_buffer(&owner.pane_id, &execution)?;
            self.flush_ordered_provider_headers(&owner.pane_id, &execution)?;
            Ok(!self.completion_ordered_publication_pending(&owner, None))
        })();
        match publication {
            Err(error) => {
                self.hold_external_publication(key, &error)?;
                return Ok(false);
            }
            Ok(false) => {
                self.external_completion_phase(key, ExternalCompletionPhase::PublicationHeld)?;
                return Ok(false);
            }
            Ok(true) => {}
        }
        let trace_attempted = self
            .agent
            .completion_settlements
            .external
            .get_mut(key)
            .map(|receipt| std::mem::replace(&mut receipt.trace_attempted, true))
            .unwrap_or(true);
        if !trace_attempted {
            self.external_completion_phase(key, ExternalCompletionPhase::TracePublication)?;
            let trace = self.append_agent_trace_turn_event(
                &owner.pane_id,
                &key.0,
                &format!(
                    "action {} {} reason=approved_external_action",
                    key.1,
                    runtime_action_status_name(result.status)
                ),
            );
            if let Err(error) = trace
                && let Some(receipt) = self.agent.completion_settlements.external.get_mut(key)
            {
                receipt.publication_failure = Some(Box::new(error));
            }
        }
        self.external_completion_phase(key, ExternalCompletionPhase::PublicationAccepted)?;
        Ok(true)
    }

    /// Native terminal paint uses canonical results, never retained probe/read
    /// work. NextPhaseOwned is deliberately not eligible for this transition.
    pub(in crate::runtime::agent) fn publish_retained_native_completion(
        &mut self,
        key: &NativeCompletionKey,
    ) -> Result<NativePublicationProgress> {
        let receipt = self
            .agent
            .completion_settlements
            .native
            .get(key)
            .ok_or_else(|| MezError::invalid_state("native publication owner is unavailable"))?;
        if receipt.phase == NativeCompletionPhase::NextPhaseOwned {
            return Ok(NativePublicationProgress::Held);
        }
        let owner = receipt.owner.clone();
        if !self.completion_paint_owner_is_current(&owner) {
            return Ok(NativePublicationProgress::Held);
        }
        let execution = self
            .agent_turn_executions()
            .get(&key.0)
            .cloned()
            .ok_or_else(|| {
                MezError::invalid_state("native publication execution is unavailable")
            })?;
        let mut presentation_execution = execution.clone();
        // Failed patch diagnostics have a separate established source policy.
        presentation_execution.action_results.retain(|result| {
            !result.is_error
                || !execution
                    .response
                    .action_batch
                    .as_ref()
                    .is_some_and(|batch| {
                        batch.actions.iter().any(|action| {
                            action.id == result.action_id
                                && matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
                        })
                    })
        });
        let publication = (|| -> Result<NativePublicationProgress> {
            if let Some(result) = execution
                .action_results
                .iter()
                .find(|result| result.action_id == key.1)
                .cloned()
            {
                let suppressed = self.reconcile_action_presentation_progress_for_execution(
                    &key.0,
                    &key.1,
                    &mez_agent::ActionPresentationExecutionIdentity::Attempt(key.2.clone()),
                    &result,
                )?;
                // Native terminal RESULT is canonical display source exactly like
                // the external path: retain the immutable ordered source before
                // the fallible install. The pending gate below, not this queueing,
                // is the acknowledgment; nothing here replays the native action.
                // An existing native projection source (marker transaction and
                // shell-view raw mode) or an installed acknowledgment is the
                // original immutable owner: drain that tuple instead of proposing
                // different provenance for the same ordered key.
                let ordered_key = (
                    owner.pane_id.clone(),
                    owner.turn_id.clone(),
                    owner.conversation_id.clone(),
                    owner.group.clone(),
                    owner.ordinal,
                );
                let already_owned = self
                    .presentation
                    .agent_queued_provider_results
                    .contains_key(&ordered_key)
                    || self
                        .presentation
                        .agent_published_provider_results
                        .iter()
                        .any(|(owned, _)| *owned == ordered_key);
                if !result.is_error
                    && !suppressed
                    && !already_owned
                    && self.agent_verbose_enabled(&owner.pane_id)
                    && let Some(action) = execution
                        .response
                        .action_batch
                        .as_ref()
                        .and_then(|batch| batch.actions.iter().find(|action| action.id == key.1))
                        .cloned()
                {
                    // Only retain a source that this ordered renderer can
                    // actually install. Appended diff previews and actions with
                    // an execution display header own a result row; headerless
                    // payloads such as ShellCommand are presented by their own
                    // preview/settle owner and must not create a phantom source
                    // whose empty append would still be acknowledged.
                    let renderer_owns_row = matches!(
                        action.payload,
                        mez_agent::AgentActionPayload::ApplyPatch { .. }
                    )
                        || crate::runtime::render::agent_action_execution_display_header(&action)
                            .is_some();
                    if renderer_owns_row {
                        self.queue_ordered_provider_result(
                            &owner.pane_id,
                            &execution,
                            &action,
                            &result,
                        )?;
                    }
                }
            }
            self.present_agent_action_outcomes_to_terminal_buffer(
                &owner.pane_id,
                &presentation_execution,
            )?;
            self.flush_ordered_provider_headers(&owner.pane_id, &execution)?;
            self.present_deferred_agent_say_actions_to_terminal_buffer(&owner.pane_id, &execution)?;
            Ok(
                if !self.completion_ordered_publication_pending(&owner, None) {
                    NativePublicationProgress::GroupPublished
                } else if !self.completion_ordered_publication_pending(&owner, Some(owner.ordinal))
                {
                    NativePublicationProgress::ActionPublished
                } else {
                    NativePublicationProgress::Held
                },
            )
        })();
        let published = match publication {
            Ok(published) => published,
            Err(error) => {
                self.note_native_publication_error(key, Err(error));
                NativePublicationProgress::Held
            }
        };
        self.native_completion_phase(
            key,
            if published == NativePublicationProgress::GroupPublished {
                NativeCompletionPhase::PublicationAccepted
            } else {
                NativeCompletionPhase::PublicationHeld
            },
        )?;
        Ok(published)
    }

    /// Transfers a published prefix to the existing admitted-action dispatcher.
    /// Required gates, exact paint identity, pause and current execution still
    /// govern dispatch. Group continuation remains solely behind its existing
    /// acknowledgement barrier. A retry reports progress only when ownership or
    /// a pending action state actually changes, not merely on another attempt.
    pub(in crate::runtime::agent) fn finish_native_completion_publication(
        &mut self,
        key: &NativeCompletionKey,
        progress: NativePublicationProgress,
    ) -> Result<bool> {
        match progress {
            NativePublicationProgress::Held => Ok(false),
            NativePublicationProgress::GroupPublished => {
                self.continue_native_completion(key)?;
                Ok(true)
            }
            NativePublicationProgress::ActionPublished => {
                let Some(receipt) = self.agent.completion_settlements.native.get(key) else {
                    return Ok(false);
                };
                if receipt.phase != NativeCompletionPhase::PublicationHeld
                    || receipt.raw.is_some()
                    || receipt.failure.is_some()
                    || !self.completion_paint_owner_is_current(&receipt.owner)
                    || self.agent_turn_is_human_paused(&key.0)
                {
                    return Ok(false);
                }
                self.ensure_completion_required_gates_ready(&key.0)?;
                let Some(execution) = self.agent_turn_executions().get(&key.0).cloned() else {
                    return Ok(false);
                };
                if !self.execution_has_pending_shell_dispatch(&key.0, &execution) {
                    return Ok(false);
                }
                let pending = execution
                    .action_results
                    .iter()
                    .filter(|result| {
                        result.status == ActionStatus::Running
                            && !self.agent_action_has_native_shell_owner(&key.0, &result.action_id)
                            && !self.agent_action_has_running_shell_transaction(
                                &key.0,
                                &result.action_id,
                            )
                            && !self
                                .agent_action_has_pending_pre_shell_hook(&key.0, &result.action_id)
                    })
                    .map(|result| result.action_id.clone())
                    .collect::<Vec<_>>();
                let Some(updated) = self.dispatch_stored_running_shell_actions(&key.0)? else {
                    return Ok(false);
                };
                Ok(pending.iter().any(|id| {
                    self.agent_action_has_native_shell_owner(&key.0, id)
                        || self.agent_action_has_running_shell_transaction(&key.0, id)
                        || self.agent_action_has_pending_pre_shell_hook(&key.0, id)
                        || updated.action_results.iter().any(|result| {
                            result.action_id == *id && result.status != ActionStatus::Running
                        })
                }))
            }
        }
    }

    /// Shared actionable predicate for pump, idle scheduling and turn liveness.
    /// Real delivery barriers wake their own owners instead of a paint timer.
    pub(crate) fn completion_publication_retry_needed_for_turn(&self, turn_id: &str) -> bool {
        let delivery_pending = self
            .agent
            .pending_approved_external_actions
            .keys()
            .chain(self.agent.pending_native_shell_dispatches.keys())
            .any(|(turn, _)| turn == turn_id);
        !delivery_pending
            && (self
                .agent
                .completion_settlements
                .external
                .values()
                .any(|receipt| {
                    receipt.owner.turn_id == turn_id
                        && receipt.phase == ExternalCompletionPhase::PublicationHeld
                        && receipt.raw.is_none()
                        && receipt.failure.is_none()
                        && self.completion_paint_owner_is_current(&receipt.owner)
                })
                || self
                    .agent
                    .completion_settlements
                    .native
                    .values()
                    .any(|receipt| {
                        receipt.owner.turn_id == turn_id
                            && receipt.phase == NativeCompletionPhase::PublicationHeld
                            && receipt.raw.is_none()
                            && receipt.failure.is_none()
                            && self.completion_paint_owner_is_current(&receipt.owner)
                    })
                || self.retained_completion_settlement_retry_needed(turn_id))
    }

    /// Reports only work that the bounded publication pump can actually advance.
    pub(crate) fn completion_publication_retry_needed(&self) -> bool {
        self.agent
            .completion_settlements
            .groups
            .keys()
            .any(|turn| self.completion_publication_retry_needed_for_turn(turn))
            || self.action_progress_retirement_retry_needed()
            || self.shell_preview_retirement_retry_needed()
            || !self.presentation.agent_provider_log_handoffs.is_empty()
    }

    /// One bounded pass; successful earlier items may retire later snapshot keys.
    /// Required suffix failures stay retained rather than taking down the pump.
    pub(crate) fn retry_completion_publications(&mut self) -> usize {
        if std::mem::replace(
            &mut self.agent.completion_settlements.publication_pump_active,
            true,
        ) {
            return 0;
        }
        let keys = self
            .agent
            .completion_settlements
            .external
            .iter()
            .filter(|(key, receipt)| {
                receipt.phase == ExternalCompletionPhase::PublicationHeld
                    && self.completion_publication_retry_needed_for_turn(&key.0)
            })
            .map(|(key, _)| (false, key.clone()))
            .chain(
                self.agent
                    .completion_settlements
                    .native
                    .iter()
                    .filter(|(key, receipt)| {
                        receipt.phase == NativeCompletionPhase::PublicationHeld
                            && self.completion_publication_retry_needed_for_turn(&key.0)
                    })
                    .map(|(key, _)| (true, key.clone())),
            )
            .collect::<Vec<_>>();
        let mut advanced = 0usize;
        for (native, key) in keys {
            if !self.completion_publication_retry_needed_for_turn(&key.0)
                || !(if native {
                    self.agent.completion_settlements.native.contains_key(&key)
                } else {
                    self.agent
                        .completion_settlements
                        .external
                        .contains_key(&key)
                })
            {
                continue;
            }
            let scope = self.begin_completion_scope(&key.0, CompletionScope::Callback);
            let result = if native {
                self.publish_retained_native_completion(&key)
                    .and_then(|progress| self.finish_native_completion_publication(&key, progress))
            } else {
                self.publish_retained_external_completion(&key)
                    .and_then(|published| {
                        if published {
                            self.continue_external_completion(&key)?;
                        }
                        Ok(published)
                    })
            };
            if let Err(error) = &result {
                if native {
                    self.fail_native_completion(&key, error);
                } else {
                    self.fail_external_completion(&key, error);
                }
            }
            if self
                .end_completion_scope(scope, "publication_continuation", false, result)
                .is_ok_and(|changed| changed)
            {
                advanced += 1;
            }
        }
        let groups = self
            .agent
            .completion_settlements
            .groups
            .keys()
            .filter(|turn| {
                self.completion_publication_retry_needed_for_turn(turn)
                    && self.retained_completion_settlement_retry_needed(turn)
            })
            .cloned()
            .collect::<Vec<_>>();
        for turn_id in groups {
            if self.completion_publication_retry_needed_for_turn(&turn_id)
                && self
                    .recover_retained_completion_settlement(&turn_id)
                    .is_ok_and(|changed| changed)
            {
                advanced += 1;
            }
        }
        advanced = advanced.saturating_add(self.retry_action_progress_retirements());
        advanced = advanced.saturating_add(self.retry_provider_log_handoffs());
        advanced = advanced.saturating_add(self.retry_shell_preview_retirements());
        self.agent.completion_settlements.publication_pump_active = false;
        advanced
    }
}
