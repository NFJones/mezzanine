//! Original accepted/rejected action results independent of current pane binding.
//!
//! Canonical facts are installed before context, preview or continuation errors.
//! Entered required failures stay latched; callback retry never reissues storage
//! or repeats an approval. Retired sources update only captured original backing.

use super::*;

impl RuntimeSessionService {
    /// Projects a retained actual outcome into the exact original result/context.
    /// Required failure preserves its receipt and typed error without replay.
    pub(crate) fn project_stored_message_action(
        &mut self,
        source: &RuntimeMessageActionSource,
    ) -> Result<bool> {
        let receipt = self.message_source(source)?;
        if let Some(error) = &receipt.failure {
            return Err(error.clone());
        }
        if receipt.phase == Phase::Published {
            return Ok(false);
        }
        let scope = self.begin_completion_scope(&source.turn_id, CompletionScope::Callback);
        let result = self.project_stored_message_action_inner(source);
        if let Err(error) = &result {
            if let Some(receipt) = self
                .agent
                .completion_settlements
                .messages
                .get_mut(&(source.turn_id.clone(), source.action_id.clone()))
            {
                receipt.failure.get_or_insert_with(|| error.clone());
            }
            self.record_completion_required_failure(
                &source.turn_id,
                "message_result_projection",
                Some(&source.action_id),
                None,
                error,
            );
        }
        self.end_completion_scope(scope, "message_storage_callback", false, result)
    }

    /// Runs only unentered original result/context stages; physical work is absent.
    fn project_stored_message_action_inner(
        &mut self,
        source: &RuntimeMessageActionSource,
    ) -> Result<bool> {
        let receipt = self.message_source(source)?;
        #[cfg(test)]
        let approved = matches!(&receipt.authorization, Authorization::Approval(_));
        let turn = receipt.turn.clone();
        let action = receipt.action.clone();
        let owner = receipt.owner.clone();
        let index = receipt.result_index;
        let outcome = receipt
            .outcome
            .clone()
            .ok_or_else(|| MezError::conflict("message source has no retained actual outcome"))?;
        if receipt.phase == Phase::Outcome {
            let (backing, mut execution) =
                self.completion_original_execution(&turn, &owner.group)?;
            if execution
                .response
                .action_batch
                .as_ref()
                .and_then(|batch| batch.actions.get(owner.ordinal))
                != Some(&action)
                || !execution.action_results.get(index).is_some_and(|slot| {
                    slot.action_id == action.id
                        && slot.turn_id == turn.turn_id
                        && slot.agent_id == turn.agent_id
                        && slot.status == ActionStatus::Running
                })
            {
                return Err(MezError::conflict(
                    "message original canonical result changed",
                ));
            }
            let AgentActionPayload::SendMessage {
                recipient,
                scope,
                expect_reply,
                ..
            } = &action.payload
            else {
                return Err(MezError::invalid_state(
                    "message original action changed kind",
                ));
            };
            let scope = match runtime_message_scope(scope.as_deref())? {
                MessageScope::Project => "project",
                MessageScope::Session => "session",
            };
            let mut result = match &outcome {
                Ok(delivery) => ActionResult::succeeded(&turn,&action,
                    vec![format!("message {} accepted for {} recipient(s)",delivery.message_id,delivery.queued_recipients)],
                    Some(serde_json::json!({"recipient":recipient,"scope":scope,"message_id":delivery.message_id,
                        "delivery_status":"accepted","queued_recipients":delivery.queued_recipients,
                        "sequence":delivery.sequence,"expect_reply":expect_reply,"protocol_error":null}).to_string())),
                Err(error) => {
                    let mut failed=ActionResult::failed(&turn,&action,ActionStatus::Failed,"transport_error",error.message().to_owned())?;
                    failed.structured_content_json=Some(serde_json::json!({"recipient":recipient,"scope":scope,
                        "message_id":null,"delivery_status":"rejected","accepted":false,"delivery_applied":false,
                        "protocol_error":{"code":"transport_error","message":error.message()}}).to_string());
                    failed
                }
            };
            result.permission_evaluation = execution.action_results[index]
                .permission_evaluation
                .clone();
            execution.action_results[index] = result.clone();
            let revoked = self
                .agent
                .completion_settlements
                .groups
                .get(&turn.turn_id)
                .is_some_and(|group| group.paint_revoked);
            if !revoked {
                execution.terminal_state = runtime_agent_turn_state_from_action_results(
                    &execution.action_results,
                    execution.final_turn,
                );
            }
            self.store_completion_original_execution(&turn.turn_id, backing, execution)?;
            let receipt = self
                .agent
                .completion_settlements
                .messages
                .get_mut(&(source.turn_id.clone(), source.action_id.clone()))
                .ok_or_else(|| MezError::invalid_state("message canonical receipt unavailable"))?;
            receipt.result = Some(result.clone());
            receipt.phase = Phase::Canonical;
            if let Ok(delivery) = &outcome {
                self.retain_outbound_delivery_owner(&turn, &action, delivery);
                if !expect_reply {
                    self.record_joined_coordination_reply(&turn, delivery);
                }
            }
        }
        let receipt = self.message_source(source)?;
        let result = receipt
            .result
            .clone()
            .ok_or_else(|| MezError::invalid_state("message canonical facts unavailable"))?;
        if receipt.phase == Phase::Canonical {
            let (backing, _) = self.completion_original_execution(&turn, &owner.group)?;
            self.commit_completion_original_context(
                &turn.turn_id,
                &owner.group,
                &action.id,
                backing,
                &result,
            )?;
            let receipt = self
                .agent
                .completion_settlements
                .messages
                .get_mut(&(source.turn_id.clone(), source.action_id.clone()))
                .ok_or_else(|| MezError::invalid_state("message context receipt unavailable"))?;
            receipt.phase = Phase::Context;
            if let Some(group) = self
                .agent
                .completion_settlements
                .groups
                .get_mut(&turn.turn_id)
            {
                group
                    .context_committed
                    .insert((action.id.clone(), owner.attempt.clone()));
                group.source_changed();
            }
        }
        let current = self
            .agent
            .completion_settlements
            .groups
            .get(&turn.turn_id)
            .is_some_and(|group| !group.paint_revoked)
            && self
                .agent_shell_store()
                .get(&turn.pane_id)
                .is_some_and(|shell| shell.session_id == turn.conversation_id);
        if current && outcome.is_ok() {
            self.settle_accepted_outbound_message_preview(
                &turn.pane_id,
                &turn.turn_id,
                index,
                &action,
            )?;
        }
        #[cfg(test)]
        if approved
            && outcome.is_ok()
            && std::mem::take(&mut self.agent.fail_next_approved_message_late_publication)
        {
            return Err(MezError::invalid_state(
                "injected approved message late publication failure",
            ));
        }
        self.agent
            .completion_settlements
            .messages
            .get_mut(&(source.turn_id.clone(), source.action_id.clone()))
            .ok_or_else(|| MezError::invalid_state("message publication receipt unavailable"))?
            .phase = Phase::Published;
        if current
            && !self.agent.completion_settlements.message_storage_stopping
            && !self.message_completion_pending(&turn.turn_id)
        {
            let (_, mut execution) = self.completion_original_execution(&turn, &owner.group)?;
            self.admit_request_wait_actions(&turn, &mut execution)?;
            execution.terminal_state = runtime_agent_turn_state_from_action_results(
                &execution.action_results,
                execution.final_turn,
            );
            self.agent_turn_executions_mut()
                .insert(turn.turn_id.clone(), execution.clone());
            self.retain_completion_settlement_intent(
                &turn.turn_id,
                if execution.terminal_state == AgentTurnState::Failed {
                    CompletionSettlementIntent::Failure
                } else {
                    CompletionSettlementIntent::Ordinary
                },
            );
            self.retain_stored_message_continuation(&turn.turn_id)?;
        } else if !current || self.agent.completion_settlements.message_storage_stopping {
            self.retain_completion_group_transcript(&turn.turn_id)?;
        }
        Ok(true)
    }
}
