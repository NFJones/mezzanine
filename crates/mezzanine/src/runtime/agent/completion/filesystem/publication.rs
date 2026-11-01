//! Ordered immutable final filesystem display sources after required gates.
//!
//! Only executor-confirmed endpoint diffs enter this owner, including positive
//! partial effects. Installation reuses existing ordered presentation entries
//! and acknowledgements; retry cannot re-enter projection, hooks, audit or I/O.
//! Paint requires the exact original live group/conversation. Detached sources
//! stay held for original archive ownership, never paint a newer pane. Accepted
//! action-prefix publication is not group continuation or producer transfer.

use super::*;

/// Frozen final display data derived only from validated original raw evidence.
pub(super) struct FilesystemPublicationSource {
    /// Exact canonical result, compared before any current-pane publication.
    result: ActionResult,
    /// Ordered confirmed endpoint diffs; no shell framing/output is fabricated.
    endpoints: Vec<mez_agent::ActionPresentationProgress>,
    /// Native semantic certainty/count/diagnostic summary, with no shell lowering.
    outcome: Option<(bool, String)>,
}

impl FilesystemPublicationSource {
    /// Captures bounded original final sources once alongside canonical facts.
    pub(super) fn capture(
        work: &NativeFilesystemWork,
        raw: &NativePatchWorkerOutcome,
        result: &ActionResult,
    ) -> Result<Self> {
        let endpoints = raw
            .receipts
            .iter()
            .map(|receipt| {
                crate::runtime::processes::native_filesystem_producer::endpoint_progress(
                    work, receipt,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        // Known unexecuted patch failures use the established correction or
        // terminal diagnostic owner. This explicit absence is not an installed
        // outcome acknowledgement; confirmed/unknown effects retain their row.
        let suppress_failure = result.status == ActionStatus::Failed
            && raw.result.certainty == mez_agent::native_action::NativeEffectCertainty::Unexecuted;
        let mut outcome = if suppress_failure {
            None
        } else {
            Some(
                runtime_agent_action_outcome_line(&work.owner.action, result, false).ok_or_else(
                    || MezError::invalid_state("filesystem semantic outcome source unavailable"),
                )?,
            )
        };
        if raw.receipts.iter().any(|receipt| receipt.diff_truncated)
            && let Some((_, line)) = &mut outcome
        {
            line.push_str("; diff preview truncated");
        }
        Ok(Self {
            result: result.clone(),
            endpoints,
            outcome,
        })
    }
}

impl RuntimeSessionService {
    /// Retries only this original display source. It cannot execute an action,
    /// append a canonical result/audit record or acknowledge archive transfer.
    pub(crate) fn publish_native_filesystem_completion(
        &mut self,
        attempt: &NativeFilesystemAttempt,
    ) -> Result<bool> {
        let key = self.filesystem_completion_key(attempt)?;
        let scope = self.begin_completion_scope(&key.0, CompletionScope::Callback);
        let result = self.publish_filesystem_completion(&key);
        if let Err(error) = &result {
            self.fail_filesystem_completion(attempt, error);
        }
        self.end_completion_scope(scope, "filesystem_publication", false, result)
    }

    /// Installs an eligible original action prefix; optional display errors keep
    /// exact queued sources and never become required worker/result failures.
    pub(super) fn publish_filesystem_completion(
        &mut self,
        key: &ExternalCompletionKey,
    ) -> Result<bool> {
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get(key)
            .ok_or_else(|| MezError::invalid_state("filesystem publication owner unavailable"))?;
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        if receipt.phase == FilesystemCompletionPhase::PublicationAccepted {
            return Ok(false);
        }
        if !matches!(
            receipt.phase,
            FilesystemCompletionPhase::AuditNotRequired
                | FilesystemCompletionPhase::AuditPhysicalAccepted
                | FilesystemCompletionPhase::PublicationHeld
        ) {
            return Ok(false);
        }
        let owner = receipt.owner.clone();
        let work = Arc::clone(&receipt.work);
        let source = receipt
            .publication_source
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| {
                MezError::invalid_state("filesystem final publication source unavailable")
            })?;
        if !self.completion_paint_owner_is_current(&owner)
            || !self.completion_settlement_binding_current(&owner.turn_id)
        {
            if receipt.phase != FilesystemCompletionPhase::PublicationHeld {
                self.filesystem_completion_phase(key, FilesystemCompletionPhase::PublicationHeld)?;
            }
            return Ok(false);
        }
        if self
            .agent
            .completion_settlements
            .filesystem
            .values()
            .any(|prior| {
                prior.owner.turn_id == owner.turn_id
                    && prior.owner.group == owner.group
                    && prior.owner.ordinal < owner.ordinal
                    && prior.phase != FilesystemCompletionPhase::PublicationAccepted
            })
        {
            if receipt.phase != FilesystemCompletionPhase::PublicationHeld {
                self.filesystem_completion_phase(key, FilesystemCompletionPhase::PublicationHeld)?;
            }
            return Ok(false);
        }
        let execution = self.filesystem_execution_for_publication(&work)?;
        if execution
            .response
            .action_batch
            .as_ref()
            .and_then(|batch| batch.actions.get(owner.ordinal))
            != Some(&work.owner.action)
            || execution
                .action_results
                .iter()
                .find(|result| result.action_id == owner.action_id)
                != Some(&source.result)
        {
            return Err(MezError::conflict(
                "filesystem publication canonical source changed",
            ));
        }
        let ordered = (
            owner.pane_id.clone(),
            owner.turn_id.clone(),
            owner.conversation_id.clone(),
            owner.group.clone(),
            owner.ordinal,
        );
        let acknowledgements = |service: &RuntimeSessionService| {
            usize::from(
                service
                    .presentation
                    .agent_published_provider_headers
                    .contains(&ordered),
            ) + service
                .presentation
                .agent_published_provider_results
                .iter()
                .filter(|(key, _)| key == &ordered)
                .count()
                + service
                    .presentation
                    .agent_published_provider_outcomes
                    .iter()
                    .filter(|(key, _, _)| key == &ordered)
                    .count()
        };
        let before = acknowledgements(self);
        let install = (|| -> Result<bool> {
            let selected = self
                .agent
                .completion_settlements
                .filesystem
                .get(key)
                .and_then(|receipt| receipt.final_publication_diff.clone());
            let diff = if let Some(selected) = selected {
                selected
            } else {
                self.reconcile_action_presentation_progress_for_execution(
                    &owner.turn_id,
                    &owner.action_id,
                    &mez_agent::ActionPresentationExecutionIdentity::Attempt(owner.attempt.clone()),
                    &source.result,
                )?;
                let mut remaining = String::new();
                for progress in &source.endpoints {
                    if self.confirmed_progress_source_is_promoted(progress) {
                        continue;
                    }
                    if self.confirmed_progress_source_is_retained(progress) {
                        return Err(MezError::conflict(
                            "filesystem confirmed progress promotion pending",
                        ));
                    }
                    remaining.push_str(&progress.source);
                }
                if source
                    .endpoints
                    .iter()
                    .any(|progress| progress.source_truncated)
                    && !remaining.is_empty()
                {
                    remaining.push_str("\n[Native diff preview truncated]\n");
                }
                self.agent
                    .completion_settlements
                    .filesystem
                    .get_mut(key)
                    .ok_or_else(|| MezError::invalid_state("filesystem publication owner lost"))?
                    .final_publication_diff = Some(remaining.clone());
                remaining
            };
            if self
                .presentation
                .agent_published_provider_results
                .iter()
                .any(|(key, text)| key == &ordered && text != &diff)
            {
                return Err(MezError::conflict(
                    "filesystem RESULT differs from its original confirmed source",
                ));
            }

            self.queue_ordered_provider_header(&owner.pane_id, &execution, &work.owner.action)?;
            if !diff.is_empty() {
                self.queue_ordered_provider_result_source(
                    &owner.pane_id,
                    &execution,
                    &work.owner.action,
                    &source.result,
                    (&diff, Some(&owner.attempt), false),
                )?;
            }
            if let Some(outcome) = &source.outcome {
                self.publish_ordered_runtime_outcome(
                    &owner.pane_id,
                    &execution,
                    &work.owner.action,
                    &source.result,
                    outcome.clone(),
                )?;
            }
            self.flush_ordered_provider_headers(&owner.pane_id, &execution)?;
            Ok(!self.completion_ordered_publication_pending(&owner, Some(owner.ordinal)))
        })();
        let published = match install {
            Ok(published) => published,
            Err(error) => {
                if let Some(receipt) = self.agent.completion_settlements.filesystem.get_mut(key) {
                    receipt
                        .publication_failure
                        .get_or_insert_with(|| Box::new(error));
                }
                false
            }
        };
        let phase = if published {
            FilesystemCompletionPhase::PublicationAccepted
        } else {
            FilesystemCompletionPhase::PublicationHeld
        };
        let previous = self
            .agent
            .completion_settlements
            .filesystem
            .get(key)
            .map(|receipt| receipt.phase);
        if previous != Some(phase) {
            self.filesystem_completion_phase(key, phase)?;
        } else if acknowledgements(self) != before
            && let Some(group) = self.agent.completion_settlements.groups.get_mut(&key.0)
        {
            group.source_changed();
        }
        Ok(published)
    }

    /// Checks the exact held owner against current binding and original
    /// predecessor readiness. Earlier filesystem required gates cannot be
    /// replaced by a visible header; other actions keep shared log semantics.
    fn filesystem_publication_owner_retryable(
        &self,
        receipt: &FilesystemCompletionReceipt,
    ) -> bool {
        let owner = &receipt.owner;
        if receipt.phase != FilesystemCompletionPhase::PublicationHeld
            || receipt.failure.is_some()
            || !self.completion_paint_owner_is_current(owner)
            || !self.completion_settlement_binding_current(&owner.turn_id)
        {
            return false;
        }
        self.filesystem_publication_predecessors_ready(receipt)
    }

    /// Checks original action order for both live confirmations and final sources.
    pub(super) fn filesystem_publication_predecessors_ready(
        &self,
        receipt: &FilesystemCompletionReceipt,
    ) -> bool {
        let owner = &receipt.owner;
        let Some(execution) = self.agent_turn_executions().get(&owner.turn_id) else {
            return false;
        };
        let Some(batch) = &execution.response.action_batch else {
            return false;
        };
        let pending =
            batch
                .actions
                .iter()
                .enumerate()
                .take(owner.ordinal)
                .any(|(ordinal, prior)| {
                    crate::runtime::agent::presentation::action_holds_later_log(prior)
                        && !self.joined_spawn_allows_later_logs(execution, prior)
                        && !self
                            .presentation
                            .agent_published_provider_headers
                            .contains(&(
                                owner.pane_id.clone(),
                                owner.turn_id.clone(),
                                owner.conversation_id.clone(),
                                owner.group.clone(),
                                ordinal,
                            ))
                        && !execution
                            .action_results
                            .iter()
                            .any(|result| result.action_id == prior.id && result.is_terminal())
                });
        !pending
            && !self
                .agent
                .completion_settlements
                .filesystem
                .values()
                .any(|prior| {
                    prior.owner.turn_id == owner.turn_id
                        && prior.owner.group == owner.group
                        && prior.owner.ordinal < owner.ordinal
                        && prior.phase != FilesystemCompletionPhase::PublicationAccepted
                })
    }

    /// Reports current actionable display work without retrying stale, failed
    /// or predecessor-blocked sources on a paint timer.
    pub(in crate::runtime::agent) fn filesystem_publication_retry_needed(
        &self,
        turn: &str,
    ) -> bool {
        self.agent
            .completion_settlements
            .filesystem
            .values()
            .any(|receipt| {
                receipt.owner.turn_id == turn
                    && self.filesystem_publication_owner_retryable(receipt)
            })
    }

    /// One bounded source-only pass under the common publication pump. No
    /// provider/sibling dispatch or ownership release is authorized by success.
    pub(in crate::runtime::agent) fn retry_filesystem_publications(&mut self) -> usize {
        let attempts = self
            .agent
            .completion_settlements
            .filesystem
            .values()
            .filter(|receipt| self.filesystem_publication_owner_retryable(receipt))
            .map(|receipt| receipt.attempt.clone())
            .collect::<Vec<_>>();
        attempts
            .into_iter()
            .filter(|attempt| {
                self.publish_native_filesystem_completion(attempt)
                    .is_ok_and(|changed| changed)
            })
            .count()
    }
}
