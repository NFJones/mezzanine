//! Retirement, promotion bookkeeping, and deferred-final streaming settlement.
//!
//! Only the exact conversation and installed screen lineage may be restored or
//! promoted. Turn cleanup revokes provisional authority without replaying rows
//! or disturbing intervening screen writes.

use super::*;

impl RuntimeSessionService {
    /// Retires one unvalidated live response and conditionally restores its baseline.
    ///
    /// The baseline is restored only while the pane still exactly matches the
    /// screen installed by this presentation. Any intervening pane mutation
    /// revokes streaming ownership and must survive retirement.
    pub(crate) fn discard_agent_streaming_say_presentation(
        &mut self,
        pane_id: &str,
        expected_turn_id: Option<&str>,
    ) -> Result<bool> {
        let Some(presentation) = self
            .presentation
            .agent_streaming_say_presentations
            .remove(pane_id)
        else {
            return Ok(false);
        };
        if expected_turn_id.is_some_and(|expected| expected != presentation.turn_id) {
            self.presentation
                .agent_streaming_say_presentations
                .insert(pane_id.to_string(), presentation);
            return Ok(false);
        }
        self.presentation
            .agent_promoted_streaming_say_actions
            .remove(&(pane_id.to_string(), presentation.turn_id.clone()));
        if self
            .agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.session_id == presentation.conversation_id)
            && self.agent_pane_screen_lineage(pane_id, &presentation.conversation_id)
                == Some(presentation.installed_lineage)
        {
            self.update_agent_streaming_screen(
                pane_id,
                &presentation.conversation_id,
                presentation.baseline_screen.as_ref().clone(),
            )?;
        }
        Ok(true)
    }

    /// Finalizes one live response while retaining its installed pane output.
    ///
    /// Interrupted turns have no authoritative provider completion to
    /// reconcile, but output already streamed to the user is still a useful
    /// record. Removing only the live ownership prevents later projection
    /// work from replacing the pane while preserving the installed generation
    /// in the terminal buffer.
    pub(crate) fn finalize_agent_streaming_say_presentation(
        &mut self,
        pane_id: &str,
        expected_turn_id: Option<&str>,
    ) -> Result<bool> {
        let Some(presentation) = self
            .presentation
            .agent_streaming_say_presentations
            .remove(pane_id)
        else {
            return Ok(false);
        };
        if expected_turn_id.is_some_and(|expected| expected != presentation.turn_id) {
            self.presentation
                .agent_streaming_say_presentations
                .insert(pane_id.to_string(), presentation);
            return Ok(false);
        }
        self.presentation
            .agent_promoted_streaming_say_actions
            .remove(&(pane_id.to_string(), presentation.turn_id));
        Ok(true)
    }

    /// Discards every live presentation owned by one provider turn.
    pub(crate) fn discard_agent_streaming_say_presentations_for_turn(
        &mut self,
        turn_id: &str,
    ) -> Result<usize> {
        let pane_ids = self
            .presentation
            .agent_streaming_say_presentations
            .iter()
            .filter(|(_pane_id, presentation)| presentation.turn_id == turn_id)
            .map(|(pane_id, _presentation)| pane_id.clone())
            .collect::<Vec<_>>();
        let mut discarded = 0usize;
        for pane_id in pane_ids {
            if self.discard_agent_streaming_say_presentation(&pane_id, Some(turn_id))? {
                discarded = discarded.saturating_add(1);
            }
        }
        Ok(discarded)
    }

    /// Discards every provisional provider-output presentation.
    pub(crate) fn discard_all_agent_streaming_say_presentations(&mut self) -> Result<usize> {
        let pane_ids = self
            .presentation
            .agent_streaming_say_presentations
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut discarded = 0usize;
        for pane_id in pane_ids {
            if self.discard_agent_streaming_say_presentation(&pane_id, None)? {
                discarded = discarded.saturating_add(1);
            }
        }
        Ok(discarded)
    }

    /// Reports whether completion already promoted one streamed action in place.
    pub(crate) fn agent_streaming_say_action_is_promoted(
        &self,
        pane_id: &str,
        turn_id: &str,
        action_index: usize,
    ) -> bool {
        if action_index == STREAMED_RATIONALE_PRESENTED_MARKER {
            return false;
        }
        self.presentation
            .agent_promoted_streaming_say_actions
            .get(&(pane_id.to_string(), turn_id.to_string()))
            .is_some_and(|indices| indices.contains(&action_index))
    }

    /// Reports whether the validated batch rationale already owns visible rows.
    pub(crate) fn agent_streaming_rationale_is_promoted(
        &self,
        pane_id: &str,
        turn_id: &str,
    ) -> bool {
        self.presentation
            .agent_promoted_streaming_say_actions
            .get(&(pane_id.to_string(), turn_id.to_string()))
            .is_some_and(|indices| indices.contains(&STREAMED_RATIONALE_PRESENTED_MARKER))
    }

    /// Retires unclaimed accepted header handoffs when their turn terminates.
    pub(crate) fn clear_accepted_streaming_headers_for_turn(&mut self, turn_id: &str) {
        self.presentation
            .agent_accepted_streaming_headers
            .retain(|(_, candidate_turn_id, _), _| candidate_turn_id != turn_id);
    }

    /// Retires any provisional final component when its turn ends.
    pub(crate) fn clear_pending_final_say_previews_for_turn(&mut self, turn_id: &str) {
        self.presentation
            .agent_pending_final_say_previews
            .retain(|_, preview| preview.turn_id != turn_id);
    }

    /// Settles a still-owned final say only after runtime-visible work completes.
    /// An intervening pane write retires the preview and lets the ordinary
    /// deferred presenter append the final output at its normal boundary.
    pub(crate) fn settle_pending_final_say_preview(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        completed: bool,
    ) -> Result<bool> {
        let Some(preview) = self
            .presentation
            .agent_pending_final_say_previews
            .remove(pane_id)
        else {
            return Ok(false);
        };
        if preview.turn_id != turn_id
            || self.agent_shell_store().get(pane_id).is_none_or(|session| {
                session.session_id != preview.conversation_id
                    || session.running_turn_id.as_deref() != Some(turn_id)
            })
            || self.agent_pane_screen_lineage(pane_id, &preview.conversation_id)
                != Some(preview.installed_lineage)
        {
            return Ok(false);
        }
        if completed {
            for (action_index, source, row) in &preview.finals {
                self.persist_agent_presentation_entry(
                    pane_id,
                    vec![row.style.clone(); row.rendered_lines.len()],
                    row.rendered_lines.clone(),
                    row.copy_lines.clone(),
                    String::new(),
                    Some((source.text.as_str(), source.content_type.as_str())),
                );
                self.presentation
                    .agent_promoted_streaming_say_actions
                    .entry((pane_id.to_string(), turn_id.to_string()))
                    .or_default()
                    .insert(*action_index);
                self.integration
                    .runtime_metrics_mut()
                    .record_agent_streaming_settled_component("say");
            }
        } else {
            self.update_agent_streaming_screen(
                pane_id,
                &preview.conversation_id,
                preview.without_final_screen.as_ref().clone(),
            )?;
            self.integration
                .runtime_metrics_mut()
                .record_agent_streaming_settlement_screen_change(true);
        }
        Ok(completed)
    }
}
