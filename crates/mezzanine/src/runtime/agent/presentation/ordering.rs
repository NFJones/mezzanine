//! Ordered installation of accepted headers, commands, results and outcomes.
//!
//! Each component keeps its immutable response/ordinal/entry source until a
//! successful install. Deferred speech and unfinished predecessors retain FIFO.

use super::*;

impl RuntimeSessionService {
    /// Captures an executor-approved header without publishing it ahead of earlier ordinals.
    pub(crate) fn queue_ordered_provider_header(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        action: &mez_agent::AgentAction,
    ) -> Result<bool> {
        let Some(header) = crate::runtime::render::agent_action_execution_display_header(action)
        else {
            return Ok(false);
        };
        self.queue_ordered_provider_header_with_text(pane_id, execution, action, header)?;
        Ok(true)
    }

    /// Queues an approved transaction-specific header at its validated action ordinal.
    pub(crate) fn queue_ordered_provider_header_with_text(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        action: &mez_agent::AgentAction,
        header: String,
    ) -> Result<()> {
        let Some(conversation_id) = self
            .agent_shell_store()
            .get(pane_id)
            .map(|s| s.session_id.clone())
        else {
            return Ok(());
        };
        let group = super::super::provider_execution::provider_log_execution_group_id(execution)?;
        let Some(index) = execution.response.action_batch.as_ref().and_then(|batch| {
            batch
                .actions
                .iter()
                .position(|candidate| candidate.id == action.id)
        }) else {
            return Ok(());
        };
        let key = (
            pane_id.to_string(),
            execution.request.turn_id.clone(),
            conversation_id,
            group,
            index,
        );
        if self
            .presentation
            .agent_published_provider_headers
            .contains(&key)
        {
            return Ok(());
        }
        if !self
            .presentation
            .agent_queued_provider_headers
            .contains_key(&key)
        {
            let entry = self
                .retained_provider_component_entry(pane_id, execution, Some(index), "header")?
                .unwrap_or_else(|| {
                    self.presentation_entry(
                        pane_id,
                        mez_core::presentation::PresentationOccurrenceBasis::LocalAcceptance,
                    )
                });
            self.presentation
                .agent_queued_provider_headers
                .insert(key, (action.clone(), header, Some(entry)));
        }
        Ok(())
    }

    /// Presents one executor-selected command under its exact transaction owner.
    /// Duplicate callbacks retain the installed/queued entry without a new clock.
    pub(crate) fn queue_ordered_provider_transaction_command(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        action: &mez_agent::AgentAction,
        command: &str,
        marker: &str,
    ) -> Result<()> {
        let Some(conversation) = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
        else {
            return Ok(());
        };
        let group = super::super::provider_execution::provider_log_execution_group_id(execution)?;
        let Some(index) = execution.response.action_batch.as_ref().and_then(|batch| {
            batch
                .actions
                .iter()
                .position(|candidate| candidate.id == action.id)
        }) else {
            return Ok(());
        };
        let key = (
            pane_id.to_string(),
            execution.request.turn_id.clone(),
            conversation.clone(),
            group.clone(),
            index,
        );
        let id = format!(
            "transaction-command:{conversation}:{}:{index}:{marker}",
            group.as_str()
        );
        if self
            .presentation
            .agent_published_provider_commands
            .contains(&(key.clone(), id.clone()))
        {
            return Ok(());
        }
        let entry = self
            .presentation
            .agent_queued_provider_commands
            .get(&key)
            .and_then(|entries| entries.iter().find(|(_, entry)| entry.id == id))
            .map(|(_, entry)| entry.clone())
            .unwrap_or_else(|| mez_core::presentation::PresentationEntry {
                id,
                occurrence: self.capture_presentation_occurrence(
                    mez_core::presentation::PresentationOccurrenceBasis::LocalAcceptance,
                ),
                first_row: true,
            });
        self.with_presentation_entry(pane_id, entry, |service| {
            service.queue_ordered_provider_command(pane_id, execution, action, command)
        })
    }

    /// Queues a validated model command intent for publication at its action ordinal.
    pub(crate) fn queue_ordered_provider_command(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        action: &mez_agent::AgentAction,
        command: &str,
    ) -> Result<()> {
        let Some(conversation_id) = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
        else {
            return Ok(());
        };
        let group = super::super::provider_execution::provider_log_execution_group_id(execution)?;
        let Some(index) = execution.response.action_batch.as_ref().and_then(|batch| {
            batch
                .actions
                .iter()
                .position(|candidate| candidate.id == action.id)
        }) else {
            return Ok(());
        };
        let key = (
            pane_id.to_string(),
            execution.request.turn_id.clone(),
            conversation_id,
            group,
            index,
        );
        let entry =
            self.current_presentation_entry(pane_id)
                .unwrap_or(self.provider_component_entry(
                    pane_id,
                    execution,
                    Some(index),
                    "command",
                )?);
        let source_key = (key.clone(), entry.id.clone());
        if self
            .presentation
            .agent_published_provider_commands
            .contains(&source_key)
        {
            return Ok(());
        }
        if self
            .presentation
            .agent_published_provider_headers
            .contains(&key)
        {
            self.with_presentation_entry(pane_id, entry, |service| {
                service.append_activity_command_for_execution(pane_id, execution, action, command)
            })?;
            self.presentation
                .agent_published_provider_commands
                .insert(source_key);
            return Ok(());
        }
        self.presentation
            .agent_queued_provider_headers
            .entry(key.clone())
            .or_insert_with(|| (action.clone(), String::new(), None));
        let commands = self
            .presentation
            .agent_queued_provider_commands
            .entry(key)
            .or_default();
        if !commands.iter().any(|(_, existing)| existing.id == entry.id) {
            commands.push((command.to_string(), entry));
        }
        self.flush_ordered_provider_headers(pane_id, execution)
    }

    /// Retains a settled verbose result behind its accepted action header.
    /// Immutable source and occurrence ownership precede immediate installation;
    /// installation errors retain that tuple for publication-only recovery.
    /// This does not commit execution or authorize repeating the action.
    pub(crate) fn queue_ordered_provider_result(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        action: &mez_agent::AgentAction,
        result: &mez_agent::ActionResult,
    ) -> Result<()> {
        self.queue_ordered_provider_result_source(
            pane_id,
            execution,
            action,
            result,
            (&result.content_text(), None, false),
        )
    }

    /// Retains the exact original RESULT body and optional native transaction
    /// in the same immutable source owner used by ordinary provider results.
    /// The final source flag freezes shell-view raw diagnostic rendering.
    pub(crate) fn queue_ordered_provider_result_source(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        action: &mez_agent::AgentAction,
        result: &mez_agent::ActionResult,
        source: (&str, Option<&str>, bool),
    ) -> Result<()> {
        if result.turn_id != execution.request.turn_id
            || result.action_id != action.id
            || result.action_type != action.action_type()
        {
            return Err(crate::error::MezError::invalid_state(
                "activity result differs from execution owner",
            ));
        }
        let Some(conversation_id) = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
        else {
            return Ok(());
        };
        let group = super::super::provider_execution::provider_log_execution_group_id(execution)?;
        let Some(index) = execution.response.action_batch.as_ref().and_then(|batch| {
            batch
                .actions
                .iter()
                .position(|candidate| candidate.id == action.id)
        }) else {
            return Ok(());
        };
        let key = (
            pane_id.to_string(),
            execution.request.turn_id.clone(),
            conversation_id,
            group,
            index,
        );
        let text = source.0.to_string();
        let transaction = source.1.map(str::to_string);
        let raw_diagnostic = source.2;
        let source_key = (key.clone(), text.clone());
        if self
            .presentation
            .agent_published_provider_results
            .contains(&source_key)
        {
            return Ok(());
        }
        // Acceptance owns the original immutable source even when its header
        // is already visible. A failed immediate install must not discard the
        // payload or allocate a different occurrence on a later publication try.
        if !self
            .presentation
            .agent_queued_provider_results
            .contains_key(&key)
        {
            let entry = self.presentation_entry(
                pane_id,
                mez_core::presentation::PresentationOccurrenceBasis::LocalAcceptance,
            );
            self.presentation.agent_queued_provider_results.insert(
                key.clone(),
                (
                    action.clone(),
                    result.clone(),
                    text.clone(),
                    entry,
                    transaction.clone(),
                    raw_diagnostic,
                ),
            );
        }
        if self
            .presentation
            .agent_queued_provider_results
            .get(&key)
            .is_some_and(
                |(
                    accepted_action,
                    accepted_result,
                    accepted_text,
                    _,
                    accepted_transaction,
                    accepted_raw,
                )| {
                    accepted_action != action
                        || accepted_result != result
                        || *accepted_text != text
                        || *accepted_transaction != transaction
                        || *accepted_raw != raw_diagnostic
                },
            )
        {
            return Err(crate::error::MezError::invalid_state(
                "RESULT differs from retained source owner",
            ));
        }
        if self
            .presentation
            .agent_published_provider_headers
            .contains(&key)
        {
            let (
                accepted_action,
                accepted_result,
                accepted_text,
                entry,
                accepted_transaction,
                accepted_raw,
            ) = self
                .presentation
                .agent_queued_provider_results
                .get(&key)
                .cloned()
                .ok_or_else(|| {
                    crate::error::MezError::invalid_state("accepted result source is unavailable")
                })?;
            // Queueing into an already-published header installs immediately.
            // That install is the same acknowledgment boundary as the ordered
            // drain, so a held install must fail before acknowledging and leave
            // the exact source queued for publication retry.
            #[cfg(test)]
            if self.agent_presentation_install_held_for_tests(pane_id) {
                return Err(crate::error::MezError::invalid_state(
                    "injected held pre-install presentation failure",
                ));
            }
            let entry_id = entry.id.clone();
            self.with_presentation_entry(pane_id, entry, |service| {
                if accepted_raw {
                    return service.append_agent_pty_diagnostic_bytes_to_terminal_buffer(
                        pane_id,
                        accepted_text.as_bytes(),
                    );
                }
                service.append_ordered_activity_result(
                    pane_id,
                    (key.3.as_str(), key.4, accepted_transaction.as_deref()),
                    &accepted_action,
                    &accepted_result,
                    &accepted_text,
                    service.activity_intent_for_result(pane_id, execution, &accepted_action),
                )
            })?;
            self.presentation
                .agent_published_provider_results
                .insert((key.clone(), accepted_text.clone()));
            if self
                .presentation
                .agent_queued_provider_results
                .get(&key)
                .is_some_and(|(_, _, text, entry, transaction, raw)| {
                    *text == accepted_text
                        && entry.id == entry_id
                        && *transaction == accepted_transaction
                        && *raw == accepted_raw
                })
            {
                self.presentation.agent_queued_provider_results.remove(&key);
            }
            return Ok(());
        }
        Ok(())
    }

    /// Publishes ready executor headers in response order, without delaying execution.
    pub(crate) fn flush_ordered_provider_headers(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        if let Err(first) = self.drain_ordered_provider_headers_once(pane_id, execution) {
            // Only presentation is retried. Each successfully installed component
            // was acknowledged individually, and every failed component retained
            // its source. A second failure remains explicit, never a silent drop.
            return self.drain_ordered_provider_headers_once(pane_id, execution)
                .map_err(|second| crate::error::MezError::new(second.kind(), format!(
                    "ordered log publication failed after one presentation-only retry: {}; initial failure: {}",
                    second.message(), first.message(),
                )));
        }
        Ok(())
    }

    /// Drains one accepted-source snapshot, acknowledging only installed parts.
    /// Pending command/result/outcome components retain their ordinal barrier even
    /// when their header is already visible. Errors retain the failed source.
    pub(crate) fn drain_ordered_provider_headers_once(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let Some(conversation_id) = self
            .agent_shell_store()
            .get(pane_id)
            .map(|s| s.session_id.clone())
        else {
            return Ok(());
        };
        let group = super::super::provider_execution::provider_log_execution_group_id(execution)?;
        let owner = (
            pane_id.to_string(),
            execution.request.turn_id.clone(),
            conversation_id,
            group,
        );
        let Some(actions) = self
            .presentation
            .agent_provider_log_orders
            .get(&owner)
            .map(|owner| owner.actions.clone())
        else {
            return Ok(());
        };
        self.present_deferred_agent_say_actions_to_terminal_buffer(pane_id, execution)?;
        for (index, action) in actions.iter().enumerate() {
            let key = (
                owner.0.clone(),
                owner.1.clone(),
                owner.2.clone(),
                owner.3.clone(),
                index,
            );
            if self
                .presentation
                .agent_queued_provider_headers
                .contains_key(&key)
                || self
                    .presentation
                    .agent_queued_provider_commands
                    .contains_key(&key)
                || self
                    .presentation
                    .agent_queued_provider_results
                    .contains_key(&key)
                || self
                    .presentation
                    .agent_queued_provider_outcomes
                    .contains_key(&key)
            {
                if actions[..index]
                    .iter()
                    .enumerate()
                    .any(|(prior_index, prior)| {
                        let prior_key = (
                            owner.0.clone(),
                            owner.1.clone(),
                            owner.2.clone(),
                            owner.3.clone(),
                            prior_index,
                        );
                        if matches!(prior.payload, AgentActionPayload::Say { .. }) {
                            if self
                                .presentation
                                .agent_retired_provider_says
                                .contains(&prior_key)
                            {
                                return false;
                            }
                            // A failed, blocked, or interrupted response cannot
                            // promote this deferred say. Retire its ordering
                            // slot without presenting successful assistant text.
                            if !matches!(
                                execution.terminal_state,
                                AgentTurnState::Running | AgentTurnState::Completed
                            ) {
                                return false;
                            }
                            return !self.agent_streaming_say_action_is_promoted(
                                pane_id,
                                &execution.request.turn_id,
                                prior_index,
                            ) && !self
                                .presentation
                                .agent_deferred_provider_progress
                                .contains(&prior_key);
                        }
                        // A failed response may retain a shell sibling that never
                        // started: no live transaction, native owner or pre-shell
                        // hook means it cannot render a later row, so it must not
                        // hold the ordered log behind it. A Running/Completed
                        // response keeps the strict barrier because that shell
                        // could still start and render after a successor.
                        let prior_is_inactive_failed_shell = execution.terminal_state
                            == AgentTurnState::Failed
                            && execution.action_results.iter().any(|result| {
                                result.action_id == prior.id
                                    && self.action_result_is_inactive_pending_shell_sibling(
                                        &execution.request.turn_id,
                                        result,
                                    )
                            });
                        !prior_is_inactive_failed_shell
                            && action_holds_later_log(prior)
                            && !self
                                .presentation
                                .agent_published_provider_headers
                                .contains(&prior_key)
                            && !execution.action_results.iter().any(|result| {
                                result.action_id == prior.id
                                    && result.is_terminal()
                                    && runtime_agent_action_outcome_line(
                                        prior,
                                        result,
                                        self.agent_verbose_enabled(pane_id)
                                            || self.agent_trace_enabled(pane_id),
                                    )
                                    .is_none_or(
                                        |(is_error, line)| {
                                            self.presentation
                                                .agent_published_provider_outcomes
                                                .contains(&(prior_key.clone(), is_error, line))
                                        },
                                    )
                            })
                    })
                {
                    break;
                }
                if let Some((approved_action, header, entry)) = self
                    .presentation
                    .agent_queued_provider_headers
                    .get(&key)
                    .cloned()
                {
                    if !header.is_empty()
                        && !self
                            .presentation
                            .agent_published_provider_headers
                            .contains(&key)
                    {
                        let activity = self.activity_action_source(
                        pane_id, execution, index,
                        crate::storage::transcript::activity::ActivityComponentKind::Header,
                        (&header, "application/vnd.mezzanine.agent-presentation.action-header+text; charset=utf-8"),
                    )?;
                        let entry = entry.unwrap_or(self.provider_component_entry(
                            pane_id,
                            execution,
                            Some(index),
                            "header",
                        )?);
                        self.with_presentation_entry(pane_id, entry, |service| {
                            service.append_agent_action_header_with_activity(
                                pane_id,
                                &approved_action,
                                &header,
                                activity,
                            )
                        })?;
                    }
                    self.presentation
                        .agent_published_provider_headers
                        .insert(key.clone());
                    self.presentation.agent_queued_provider_headers.remove(&key);
                }
                if let Some(commands) = self
                    .presentation
                    .agent_queued_provider_commands
                    .get(&key)
                    .cloned()
                {
                    for (command, entry) in commands {
                        let entry_id = entry.id.clone();
                        self.with_presentation_entry(pane_id, entry, |service| {
                            service.append_activity_command_for_execution(
                                pane_id, execution, action, &command,
                            )
                        })?;
                        self.presentation
                            .agent_published_provider_commands
                            .insert((key.clone(), entry_id.clone()));
                        // Installation is the acknowledgment boundary for this
                        // exact entry, not for the entire command vector. Keep
                        // a later failed command and its suffix unchanged.
                        if let Some(pending) = self
                            .presentation
                            .agent_queued_provider_commands
                            .get_mut(&key)
                        {
                            pending.retain(|(_, entry)| entry.id != entry_id);
                        }
                    }
                    self.presentation
                        .agent_queued_provider_commands
                        .remove(&key);
                }
                if let Some((action, result, text, entry, transaction, raw_diagnostic)) = self
                    .presentation
                    .agent_queued_provider_results
                    .get(&key)
                    .cloned()
                {
                    let entry_id = entry.id.clone();
                    // The acknowledged boundary is the successful install, so a
                    // held install must fail first and leave the exact source in
                    // the queue. Otherwise publication could accept an install
                    // that never happened and drop its retry obligation.
                    #[cfg(test)]
                    if self.agent_presentation_install_held_for_tests(pane_id) {
                        return Err(MezError::invalid_state(
                            "injected held pre-install presentation failure",
                        ));
                    }
                    self.with_presentation_entry(pane_id, entry, |service| {
                        if raw_diagnostic {
                            return service.append_agent_pty_diagnostic_bytes_to_terminal_buffer(
                                pane_id,
                                text.as_bytes(),
                            );
                        }
                        service.append_ordered_activity_result(
                            pane_id,
                            (key.3.as_str(), key.4, transaction.as_deref()),
                            &action,
                            &result,
                            &text,
                            service.activity_intent_for_result(pane_id, execution, &action),
                        )
                    })?;
                    self.presentation
                        .agent_published_provider_results
                        .insert((key.clone(), text.clone()));
                    if self
                        .presentation
                        .agent_queued_provider_results
                        .get(&key)
                        .is_some_and(|(_, _, retained_text, entry, retained_transaction, raw)| {
                            *retained_text == text
                                && entry.id == entry_id
                                && *retained_transaction == transaction
                                && *raw == raw_diagnostic
                        })
                    {
                        self.presentation.agent_queued_provider_results.remove(&key);
                    }
                }
                if let Some(outcomes) = self
                    .presentation
                    .agent_queued_provider_outcomes
                    .get(&key)
                    .cloned()
                {
                    for (is_error, line, entry, accepted_result) in outcomes {
                        let entry_id = entry.id.clone();
                        self.with_presentation_entry(pane_id, entry, |service| {
                            service.append_ordered_activity_outcome(
                                pane_id,
                                execution,
                                action,
                                &accepted_result,
                                is_error,
                                &line,
                            )?;
                            Ok(())
                        })?;
                        self.presentation.agent_published_provider_outcomes.insert((
                            key.clone(),
                            is_error,
                            line.clone(),
                        ));
                        if let Some(pending) = self
                            .presentation
                            .agent_queued_provider_outcomes
                            .get_mut(&key)
                        {
                            pending.retain(|entry| {
                                entry.0 != is_error
                                    || entry.1 != line
                                    || entry.2.id != entry_id
                                    || entry.3 != accepted_result
                            });
                        }
                    }
                    if self
                        .presentation
                        .agent_queued_provider_outcomes
                        .get(&key)
                        .is_some_and(Vec::is_empty)
                    {
                        self.presentation
                            .agent_queued_provider_outcomes
                            .remove(&key);
                    }
                }
                self.present_deferred_agent_say_actions_to_terminal_buffer(pane_id, execution)?;
            } else if !(execution.terminal_state == AgentTurnState::Failed
                && execution.action_results.iter().any(|result| {
                    result.action_id == action.id
                        && self.action_result_is_inactive_pending_shell_sibling(
                            &execution.request.turn_id,
                            result,
                        )
                }))
                && action_holds_later_log(action)
                && !(matches!(action.payload, AgentActionPayload::ShellCommand { .. })
                    && self
                        .presentation
                        .agent_published_provider_headers
                        .contains(&key))
                && !execution.action_results.iter().any(|result| {
                    result.action_id == action.id
                        && result.is_terminal()
                        && runtime_agent_action_outcome_line(
                            action,
                            result,
                            self.agent_verbose_enabled(pane_id)
                                || self.agent_trace_enabled(pane_id),
                        )
                        .is_none_or(|(is_error, line)| {
                            self.presentation
                                .agent_published_provider_outcomes
                                .contains(&(key.clone(), is_error, line))
                        })
                })
            {
                break;
            }
        }
        self.present_deferred_agent_say_actions_to_terminal_buffer(pane_id, execution)?;
        Ok(())
    }
}
