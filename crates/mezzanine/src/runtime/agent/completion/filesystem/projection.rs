//! Original canonical execution/context updates without effect or gate replay.
//!
//! A changed latest response is never temporarily replaced to paint an older
//! result. Failed backing remains the original owner after forced retirement;
//! captured absence cannot fall back to a newly installed execution/context.
//! Captured post-commit confirmations must be present in raw evidence before
//! canonical facts change; missing/revised endpoint data fails with custody held.
//! Successful canonical fact projection leaves later required gates pending.

use super::*;

/// Actual existing canonical backing selected before writing any result facts.
#[derive(Clone, Copy)]
enum Source {
    /// Original execution/context remain in the active canonical maps.
    Live,
    /// Forced retirement moved actual original backing into its group.
    Failed,
    /// Original correction execution is retained under the same group.
    Correction,
}

impl RuntimeSessionService {
    /// Commits original facts/context and advances non-executable hook, audit
    /// and eligible final-display milestones. The bool means some facts/gates
    /// advanced, not whole settlement. Archive and producer custody stay held.
    pub(crate) fn project_native_filesystem_completion(
        &mut self,
        attempt: &NativeFilesystemAttempt,
    ) -> Result<bool> {
        let key = self.filesystem_completion_key(attempt)?;
        if self
            .agent
            .completion_settlements
            .filesystem
            .get(&key)
            .is_some_and(|receipt| {
                matches!(
                    receipt.phase,
                    FilesystemCompletionPhase::Queued | FilesystemCompletionPhase::Issued
                )
            })
        {
            return Err(MezError::conflict(
                "filesystem projection has no observed original outcome",
            ));
        }
        let scope = self.begin_completion_scope(&key.0, CompletionScope::Callback);
        let projected = (|| {
            let changed = self.project_filesystem_facts(&key)?;
            let hooks_changed = self.complete_filesystem_hooks(&key)?;
            let audit_changed = self.append_filesystem_audit(&key)?;
            let physical_changed = self.settle_filesystem_physical_audit(&key)?;
            self.publish_filesystem_completion(&key).map(|published| {
                changed || hooks_changed || audit_changed || physical_changed || published
            })
        })();
        if let Err(error) = &projected {
            self.fail_filesystem_completion(attempt, error);
        }
        self.end_completion_scope(scope, "filesystem_callback", false, projected)
    }

    /// Applies only never-entered canonical/context stages under frozen ownership.
    fn project_filesystem_facts(&mut self, key: &ExternalCompletionKey) -> Result<bool> {
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get(key)
            .ok_or_else(|| MezError::invalid_state("filesystem projection receipt unavailable"))?;
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        if matches!(
            receipt.phase,
            FilesystemCompletionPhase::ContextCommitted
                | FilesystemCompletionPhase::HooksEntered
                | FilesystemCompletionPhase::HooksAccepted
                | FilesystemCompletionPhase::AuditEntered
                | FilesystemCompletionPhase::AuditNotRequired
                | FilesystemCompletionPhase::AuditAppendAccepted
                | FilesystemCompletionPhase::AuditPhysicalAccepted
                | FilesystemCompletionPhase::PublicationHeld
                | FilesystemCompletionPhase::PublicationAccepted
        ) {
            return Ok(false);
        }
        if receipt.phase != FilesystemCompletionPhase::OutcomeRetained {
            return Err(MezError::conflict(
                "filesystem projection is not an unentered retained outcome",
            ));
        }
        let work = Arc::clone(&receipt.work);
        let raw = receipt.raw.as_ref().map(Arc::clone).ok_or_else(|| {
            MezError::invalid_state("filesystem projection lost original raw evidence")
        })?;
        let refusal = self
            .agent
            .native_filesystem_actions
            .refusal(&receipt.attempt);
        // Recover dropped/coalesced notifications while the original result is
        // still running. Display failure cannot invalidate committed effects.
        if let Some(producer) = self.retained_native_filesystem_producer(&receipt.attempt) {
            producer.validate_confirmed_outcome(&raw)?;
            let progress = if refusal.is_none() {
                producer
                    .progress_snapshot()
                    .and_then(|delivery| self.apply_native_filesystem_progress(&delivery))
            } else {
                Ok(false)
            };
            if let Err(error) = progress
                && let Some(receipt) = self.agent.completion_settlements.filesystem.get_mut(key)
            {
                receipt
                    .publication_failure
                    .get_or_insert_with(|| Box::new(error));
            }
        }
        self.filesystem_completion_phase(key, FilesystemCompletionPhase::Projection)?;
        let result = if let Some(refusal) = &refusal {
            result::refusal_result(&work, refusal)?
        } else {
            result::action_result(&work, &raw)?
        };
        let publication = FilesystemPublicationSource::capture(&work, &raw, &result)?;
        self.agent
            .completion_settlements
            .filesystem
            .get_mut(key)
            .ok_or_else(|| MezError::invalid_state("filesystem publication receipt unavailable"))?
            .publication_source = Some(Arc::new(publication));
        let payload = result::completed_hook_payload(&work, &raw, result.status);
        self.agent
            .completion_settlements
            .filesystem
            .get_mut(key)
            .ok_or_else(|| MezError::invalid_state("filesystem hook receipt unavailable"))?
            .completed_hook_payload = Some(payload);
        let (source, mut execution) = self.filesystem_original_execution(&work)?;
        let index = work.owner.ordinal;
        let authored = execution
            .response
            .action_batch
            .as_ref()
            .and_then(|batch| batch.actions.get(index));
        let pending = execution
            .action_results
            .iter()
            .position(|slot| slot.action_id == work.owner.action.id)
            .ok_or_else(|| {
                MezError::invalid_state("filesystem original result slot unavailable")
            })?;
        if authored != Some(&work.owner.action)
            || execution
                .action_results
                .iter()
                .filter(|slot| slot.action_id == work.owner.action.id)
                .count()
                != 1
            || execution.action_results[pending].status != ActionStatus::Running
            || execution.action_results[pending].turn_id != work.owner.turn.turn_id
            || execution.action_results[pending].agent_id != work.owner.turn.agent_id
            || execution.action_results[pending].action_type != work.owner.action.action_type()
        {
            return Err(MezError::conflict(
                "filesystem original canonical action/result changed",
            ));
        }
        execution.action_results[pending] = result.clone();
        let revoked = self
            .agent
            .completion_settlements
            .groups
            .get(&key.0)
            .is_some_and(|group| group.paint_revoked);
        if !revoked {
            execution.terminal_state = runtime_agent_turn_state_from_action_results(
                &execution.action_results,
                execution.final_turn,
            );
        }
        self.store_filesystem_original_execution(&key.0, source, execution)?;
        if result.status == ActionStatus::Succeeded {
            self.agent
                .native_filesystem_actions
                .selection
                .confirm(&work)?;
        }
        self.filesystem_completion_phase(key, FilesystemCompletionPhase::CanonicalRetained)?;
        // This pane-local cache is paint, not original effect attribution. Never
        // populate a replacement conversation's cache from an old completion.
        if self
            .agent_shell_store()
            .get(&work.owner.turn.pane_id)
            .is_some_and(|session| session.session_id == work.owner.turn.conversation_id)
        {
            for receipt in raw.receipts.iter().filter(|receipt| receipt.is_file()) {
                self.record_agent_modified_file_delta(
                    &work.owner.turn.pane_id,
                    receipt.path.clone(),
                    receipt.added_lines,
                    receipt.removed_lines,
                );
            }
        }
        self.filesystem_completion_phase(key, FilesystemCompletionPhase::ContextEntered)?;
        self.commit_filesystem_original_context(&work, source, &result)?;
        self.filesystem_completion_phase(key, FilesystemCompletionPhase::ContextCommitted)?;
        Ok(true)
    }

    /// Returns actual original canonical backing for display comparison without
    /// swapping it into a newer current execution or replaying its chronology.
    pub(super) fn filesystem_execution_for_publication(
        &self,
        work: &NativeFilesystemWork,
    ) -> Result<AgentTurnExecution> {
        self.filesystem_original_execution(work)
            .map(|(_, execution)| execution)
    }

    /// Reads the actual original source; failed captured absence stays final.
    fn filesystem_original_execution(
        &self,
        work: &NativeFilesystemWork,
    ) -> Result<(Source, AgentTurnExecution)> {
        let group = self
            .agent
            .completion_settlements
            .groups
            .get(&work.owner.turn.turn_id)
            .filter(|group| group.group == work.owner.group)
            .ok_or_else(|| {
                MezError::invalid_state("filesystem original completion group changed")
            })?;
        let (source, execution) = if let Some(backing) = &group.failed_backing {
            (Source::Failed, backing.execution.as_ref())
        } else if let retirement::CompletionCapture::Captured(execution) =
            &group.correction_execution
        {
            (Source::Correction, execution.as_ref())
        } else {
            (
                Source::Live,
                self.agent_turn_executions().get(&work.owner.turn.turn_id),
            )
        };
        let execution = execution.ok_or_else(|| {
            MezError::invalid_state("filesystem original execution backing unavailable")
        })?;
        if super::super::super::provider_execution::provider_log_execution_group_id(execution)?
            != work.owner.group
            || execution.request.turn_id != work.owner.turn.turn_id
            || execution.request.agent_id != work.owner.turn.agent_id
        {
            return Err(MezError::conflict(
                "filesystem latest execution is not its original source",
            ));
        }
        Ok((source, execution.clone()))
    }

    /// Writes back to the same selected owner without swapping the latest view.
    fn store_filesystem_original_execution(
        &mut self,
        turn_id: &str,
        source: Source,
        execution: AgentTurnExecution,
    ) -> Result<()> {
        match source {
            Source::Live => {
                self.agent_turn_executions_mut()
                    .insert(turn_id.to_owned(), execution);
            }
            Source::Failed => {
                let backing = self
                    .agent
                    .completion_settlements
                    .groups
                    .get_mut(turn_id)
                    .and_then(|group| group.failed_backing.as_mut())
                    .ok_or_else(|| {
                        MezError::invalid_state("filesystem failed backing disappeared")
                    })?;
                backing.execution = retirement::CompletionCapture::Captured(Some(execution));
            }
            Source::Correction => {
                let group = self
                    .agent
                    .completion_settlements
                    .groups
                    .get_mut(turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("filesystem correction backing disappeared")
                    })?;
                group.correction_execution =
                    retirement::CompletionCapture::Captured(Some(execution));
            }
        }
        Ok(())
    }

    /// Commits exact original group chronology and original provider-native calls.
    fn commit_filesystem_original_context(
        &mut self,
        work: &NativeFilesystemWork,
        source: Source,
        result: &ActionResult,
    ) -> Result<()> {
        if matches!(source, Source::Live | Source::Correction) {
            if self
                .agent
                .agent_execution_groups_by_turn
                .get(&work.owner.turn.turn_id)
                .and_then(|groups| groups.get(&work.owner.action.id))
                != Some(&work.owner.group)
            {
                return Err(MezError::conflict(
                    "filesystem original context group binding changed",
                ));
            }
            return self.append_action_result_context_if_absent(&work.owner.turn.turn_id, result);
        }
        let group = self
            .agent
            .completion_settlements
            .groups
            .get_mut(&work.owner.turn.turn_id)
            .ok_or_else(|| {
                MezError::invalid_state("filesystem original context group unavailable")
            })?;
        let action_ids = group
            .actions
            .iter()
            .map(|(id, _, _)| id.clone())
            .collect::<Vec<_>>();
        let context = group
            .failed_backing
            .as_mut()
            .and_then(|backing| match &mut backing.context {
                retirement::CompletionCapture::Captured(context) => context.as_mut(),
                retirement::CompletionCapture::NotCaptured => None,
            })
            .ok_or_else(|| {
                MezError::invalid_state("filesystem original captured context unavailable")
            })?;
        context
            .commit_settled_action_results_in_group(
                std::slice::from_ref(result),
                work.owner.group.clone(),
            )
            .map_err(|error| MezError::invalid_state(error.to_string()))?;
        super::super::super::outcome::append_provider_tool_results_in_group(
            context,
            &work.owner.group,
            &action_ids,
            &group.provider_calls,
        )
    }
}
