//! Receipt-owned final log suffix, independent of execution and durable history.
//!
//! Only the current conversation's pending display sources are projected. The
//! exact installed lineage owns physical suffix rows; lower compositors strip
//! them before capturing baselines. Every installation recomposes this layer
//! last. Rendering never acknowledges input, persists pending as ordinary user
//! history, changes canonical chronology or writes the process surface.

use super::*;
use mez_agent::transcript::SteeringRecoveryStatus;

/// Associates every display row with one exact accepted occurrence source.
/// Source selection emits the group once and preserves CRLF and trailing bytes;
/// rendered selection remains sanitized, wrapped terminal text.
fn steering_source_rendered_lines(
    prefix: &str,
    display: &str,
    width: usize,
    occurrence: &str,
) -> Vec<RichTextLine> {
    let mut lines = wrapped_prefixed_agent_terminal_lines(prefix, display, width);
    attach_steering_copy_source(&mut lines, display, occurrence);
    lines
}

/// Retains one exact payload at the group tail and small references on preceding
/// rows. Keeping the anchor last preserves source when older history is evicted.
fn attach_steering_copy_source(lines: &mut [RichTextLine], display: &str, occurrence: &str) {
    let reference = format!(
        "{}{occurrence}/0",
        mez_mux::copy::COPY_SOURCE_REFERENCE_PREFIX
    );
    for line in lines.iter_mut() {
        line.copy_text = Some(reference.clone());
    }
    if let Some(anchor) = lines.last_mut() {
        anchor.copy_text = Some(encode_copy_source_line_in_group(occurrence, 0, display));
    }
}

#[cfg(test)]
mod tests;

/// Surface-local receipt projection captured alongside an exact rollback screen.
pub(crate) struct SteeringPresentationSnapshot {
    /// Conversation, installed screen lineage, and physical pending suffix rows.
    suffix: Option<(String, u64, usize)>,
    /// Pane/conversation/occurrence identities already installed on that surface.
    presented: std::collections::BTreeSet<(String, String, String)>,
}

impl RuntimeSessionService {
    /// Captures settled identities belonging to one worker-rendered surface.
    /// These are display suppression only, never delivery acknowledgement.
    pub(crate) fn replayed_steering_receipt_ids(
        &self,
        pane: &str,
        conversation: &str,
    ) -> std::collections::BTreeSet<String> {
        self.presentation
            .presented_steering_receipts
            .iter()
            .filter(|(owner, source, _)| owner == pane && source == conversation)
            .map(|(_, _, id)| id.clone())
            .collect()
    }

    /// Installs replay suppression with its already validated candidate screen.
    /// Resume rollback retains the prior surface snapshot if later work fails.
    pub(crate) fn install_replayed_steering_receipt_ids(
        &mut self,
        pane: &str,
        conversation: &str,
        ids: std::collections::BTreeSet<String>,
    ) -> Result<()> {
        if self.agent_pane_screen_lineage(pane, conversation).is_none()
            || !self
                .agent_shell_store()
                .get(pane)
                .is_some_and(|session| session.session_id == conversation)
        {
            return Err(MezError::invalid_state(
                "steering replay candidate owner changed",
            ));
        }
        self.presentation.presented_steering_receipts.extend(
            ids.into_iter()
                .map(|id| (pane.to_string(), conversation.to_string(), id)),
        );
        Ok(())
    }

    /// Reconciles new receipt evidence without propagating display failure into
    /// execution. One retry is retained for the next persistence drain.
    pub(crate) fn request_steering_presentation(&mut self, pane: &str) {
        let Some(conversation) = self
            .agent_shell_store()
            .get(pane)
            .map(|session| session.session_id.clone())
        else {
            return;
        };
        let key = (pane.to_string(), conversation);
        if self.reconcile_steering_presentation(pane).is_err() {
            self.presentation.steering_presentation_retries.insert(key);
        } else {
            self.presentation.steering_presentation_retries.remove(&key);
        }
    }

    /// Consumes bounded display retries only for their original conversation.
    /// No provider, input or canonical effects are replayed on this path.
    pub(crate) fn retry_steering_presentations(&mut self) {
        let retries = std::mem::take(&mut self.presentation.steering_presentation_retries);
        for (pane, conversation) in retries {
            if self
                .agent_shell_store()
                .get(&pane)
                .is_some_and(|session| session.session_id == conversation)
            {
                let _ = self.reconcile_steering_presentation(&pane);
            }
        }
    }

    /// Captures surface-local receipt ownership for exact screen rollback.
    pub(crate) fn snapshot_steering_presentation_surface(
        &self,
        pane: &str,
    ) -> SteeringPresentationSnapshot {
        SteeringPresentationSnapshot {
            suffix: self
                .presentation
                .pending_steering_suffixes
                .get(pane)
                .cloned(),
            presented: self
                .presentation
                .presented_steering_receipts
                .iter()
                .filter(|(owner, _, _)| owner == pane)
                .cloned()
                .collect(),
        }
    }

    /// Restores receipt ownership only after its exact screen has been restored.
    pub(crate) fn restore_steering_presentation_surface(
        &mut self,
        pane: &str,
        snapshot: SteeringPresentationSnapshot,
    ) {
        self.reset_steering_presentation_surface(pane);
        if let Some((owner, _, rows)) = snapshot.suffix
            && let Some(lineage) = self.agent_pane_screen_lineage(pane, &owner)
        {
            self.presentation
                .pending_steering_suffixes
                .insert(pane.to_string(), (owner, lineage, rows));
        }
        self.presentation.presented_steering_receipts.extend(
            snapshot
                .presented
                .into_iter()
                .filter(|(_, owner, _)| {
                    self.agent_shell_store()
                        .get(pane)
                        .is_some_and(|session| session.session_id == *owner)
                })
                .collect::<Vec<_>>(),
        );
    }

    /// Clears surface-local suppression when a screen is reconstructed or replaced.
    /// Durable receipt writes and delivery state remain owned by their subsystems.
    pub(crate) fn reset_steering_presentation_surface(&mut self, pane: &str) {
        self.presentation.pending_steering_suffixes.remove(pane);
        self.presentation
            .presented_steering_receipts
            .retain(|(owner, _, _)| owner != pane);
    }

    /// Resizes a pending-free lower composite and reprojects current receipts.
    /// No durable replay or process-surface write occurs at this boundary.
    pub(crate) fn resize_agent_screen_with_pending_steering(
        &mut self,
        pane: &str,
        size: Size,
    ) -> Result<()> {
        let Some(conversation) = self
            .agent_pane_screen_state(pane)
            .map(|state| state.conversation_id().to_string())
        else {
            return Ok(());
        };
        let old = self
            .agent_pane_screen_lineage(pane, &conversation)
            .ok_or_else(|| MezError::invalid_state("pending resize lineage unavailable"))?;
        let mut screen = self
            .agent_screen_without_pending_steering(pane, &conversation)
            .ok_or_else(|| MezError::invalid_state("pending resize suffix unavailable"))?;
        screen.resize(size)?;
        let installed = self
            .update_agent_pane_screen_preserving_interaction(pane, &conversation, screen)
            .ok_or_else(|| MezError::invalid_state("pending resize owner changed"))?;
        self.presentation
            .rebase_agent_presentations_after_provisional_resize(pane, old, installed, size)
    }

    /// Renders one settled identity using the normal user style and exact source.
    /// Pending input cannot enter this durable rendering path.
    pub(crate) fn append_settled_steering_source(
        &mut self,
        pane: &str,
        source: &crate::storage::transcript::steering::Source,
    ) -> Result<()> {
        let encoded = source.encode()?;
        let prefix = match source.receipt.status {
            SteeringRecoveryStatus::Admitted(_) => "user> ",
            SteeringRecoveryStatus::NotSent => "user> [not sent] ",
            SteeringRecoveryStatus::AdmissionUnknown => "user> [admission unknown] ",
            SteeringRecoveryStatus::Pending => {
                return Err(MezError::invalid_state(
                    "pending steering cannot be promoted",
                ));
            }
        };
        let width = self.agent_terminal_markdown_frame_width(pane)?;
        let lines = steering_source_rendered_lines(
            prefix,
            &source.receipt.display,
            width,
            &source.receipt.id,
        );
        let copies = lines
            .iter()
            .map(|line| {
                line.copy_text
                    .clone()
                    .unwrap_or_else(|| AGENT_COPY_SKIP_LINE.to_string())
            })
            .collect::<Vec<_>>();
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane,
            AgentTerminalPresentationStyle::UserPrompt,
            &lines,
            &copies,
            Some((&encoded, crate::storage::transcript::steering::CONTENT_TYPE)),
        )
    }

    /// Reconciles receipt status without acknowledging input or replaying effects.
    /// Successful installations are occurrence-fenced; failed installs can retry.
    pub(crate) fn reconcile_steering_presentation(&mut self, pane: &str) -> Result<()> {
        let conversation = self
            .agent_shell_store()
            .get(pane)
            .map(|session| session.session_id.clone())
            .ok_or_else(|| MezError::invalid_state("steering presentation owner unavailable"))?;
        let receipts = self.steering_presentation_receipts(pane)?;
        self.refresh_pending_steering_suffix(pane)?;
        for receipt in receipts
            .into_iter()
            .filter(|receipt| receipt.status != SteeringRecoveryStatus::Pending)
        {
            let key = (pane.to_string(), conversation.clone(), receipt.id.clone());
            if self.presentation.presented_steering_receipts.contains(&key) {
                continue;
            }
            let source = crate::storage::transcript::steering::Source {
                version: 1,
                conversation_id: conversation.clone(),
                receipt,
            };
            self.append_settled_steering_source(pane, &source)?;
            self.presentation.presented_steering_receipts.insert(key);
        }
        Ok(())
    }

    /// Clones the current composite with only its exact pending suffix removed.
    /// Stale metadata cannot authorize erasure of intervening screen content.
    pub(crate) fn agent_screen_without_pending_steering(
        &self,
        pane: &str,
        conversation: &str,
    ) -> Option<TerminalScreen> {
        let mut screen = self.agent_pane_screen(pane)?.clone();
        if let Some((owner, lineage, rows)) = self.presentation.pending_steering_suffixes.get(pane)
            && owner == conversation
            && self.agent_pane_screen_lineage(pane, conversation) == Some(*lineage)
            && *rows > 0
        {
            let suffix = screen.capture_transient_suffix(*rows, true)?;
            if !screen.clear_transient_suffix(suffix) {
                return None;
            }
        }
        Some(screen)
    }

    /// Composes bounded pending rows after all lower-layer content. The caller
    /// records returned row ownership only after successful screen installation.
    pub(crate) fn compose_pending_steering_suffix(
        &self,
        pane: &str,
        conversation: &str,
        mut screen: TerminalScreen,
    ) -> Result<(TerminalScreen, usize)> {
        let Some(session) = self
            .agent_shell_store()
            .get(pane)
            .filter(|session| session.session_id == conversation)
        else {
            return Ok((screen, 0));
        };
        if session.visibility != AgentShellVisibility::Visible {
            return Ok((screen, 0));
        }
        let receipts = self.steering_presentation_receipts(pane)?;
        let pending = receipts
            .iter()
            .filter(|entry| entry.status == SteeringRecoveryStatus::Pending)
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok((screen, 0));
        }
        let size = screen.size();
        let budget = self
            .agent_composer_layout_for_pane(pane, usize::from(size.columns), usize::from(size.rows))
            .log
            .rows
            .saturating_sub(1);
        if budget == 0 {
            return Ok((screen, 0));
        }
        let width = bounded_agent_terminal_presentation_columns(
            usize::from(size.columns),
            self.presentation.settings.terminal_agent_wrap_column_cap,
        )
        .saturating_sub(UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX))
        .max(1);
        let mut lines = pending
            .iter()
            .flat_map(|entry| {
                wrapped_prefixed_agent_terminal_lines("user> [pending] ", &entry.display, width)
            })
            .collect::<Vec<_>>();
        if lines.len() > budget {
            let hidden_rows = lines.len().saturating_sub(budget.saturating_sub(1));
            let tail = lines.split_off(hidden_rows);
            lines = wrapped_prefixed_agent_terminal_lines(
                "user> [pending] ",
                &format!(
                    "{} submissions; {hidden_rows} earlier rows hidden",
                    pending.len()
                ),
                width,
            );
            lines.truncate(1);
            lines.extend(tail);
            // Every overflow row belongs to the same complete source group.
            // Source selection recovers it once even when only a tail row is
            // selected, without reconstructing omitted text from screen cells.
            let full_source = pending
                .iter()
                .map(|entry| entry.display.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            attach_steering_copy_source(
                &mut lines,
                &full_source,
                &format!("steering-overflow-{conversation}"),
            );
        } else {
            let mut offset = 0;
            for entry in &pending {
                let count = wrapped_prefixed_agent_terminal_lines(
                    "user> [pending] ",
                    &entry.display,
                    width,
                )
                .len();
                attach_steering_copy_source(
                    &mut lines[offset..offset + count],
                    &entry.display,
                    &entry.id,
                );
                offset += count;
            }
        }
        let mut bytes = String::new();
        let cursor = screen.cursor_state();
        let empty = screen
            .visible_lines()
            .get(cursor.row)
            .is_none_or(|line| line.trim().is_empty());
        if cursor.column == 0 && empty {
            bytes.push('\r');
        } else {
            bytes.push_str("\r\n");
        }
        for line in &lines {
            append_styled_agent_terminal_rendered_line(
                &mut bytes,
                AgentTerminalPresentationStyle::UserPrompt,
                line,
                self.ui_theme(),
            );
            bytes.push_str("\x1b[0m\r\n");
        }
        Self::feed_agent_terminal_screen(
            &mut screen,
            bytes.as_bytes(),
            "projecting pending steering",
        )?;
        let copies = lines
            .iter()
            .map(|line| {
                line.copy_text
                    .clone()
                    .unwrap_or_else(|| AGENT_COPY_SKIP_LINE.to_string())
            })
            .collect::<Vec<_>>();
        screen.set_recent_normal_copy_texts(&copies, AGENT_COPY_SKIP_LINE);
        Ok((screen, lines.len()))
    }

    /// Reprojects changed receipts while preserving current lower-layer output.
    /// Failure leaves execution and acceptance intact; later installs retry it.
    pub(crate) fn refresh_pending_steering_suffix(&mut self, pane: &str) -> Result<()> {
        self.ensure_current_agent_presentation_screen(pane)?;
        let conversation = self
            .agent_shell_store()
            .get(pane)
            .map(|session| session.session_id.clone())
            .ok_or_else(|| MezError::invalid_state("pending steering owner unavailable"))?;
        let old = self
            .agent_pane_screen_lineage(pane, &conversation)
            .ok_or_else(|| MezError::invalid_state("pending steering lineage unavailable"))?;
        let screen = self
            .agent_screen_without_pending_steering(pane, &conversation)
            .ok_or_else(|| MezError::invalid_state("pending steering suffix unavailable"))?;
        let installed = self
            .update_agent_pane_screen_preserving_interaction(pane, &conversation, screen)
            .ok_or_else(|| MezError::invalid_state("pending steering conversation changed"))?;
        if let Some(preview) = self
            .presentation
            .agent_shell_output_previews
            .get_mut(pane)
            .filter(|preview| {
                preview.conversation_id == conversation && preview.installed_lineage == old
            })
        {
            preview.installed_lineage = installed;
        }
        if let Some(progress) = self
            .presentation
            .action_presentation_progress
            .get_mut(pane)
            .filter(|progress| {
                progress.conversation_id == conversation && progress.installed_lineage == old
            })
        {
            progress.installed_lineage = installed;
        }
        if let Some(streaming) = self
            .presentation
            .agent_streaming_say_presentations
            .get_mut(pane)
            .filter(|streaming| {
                streaming.conversation_id == conversation && streaming.installed_lineage == old
            })
        {
            streaming.installed_lineage = installed;
            if streaming.projected_lineage.is_some() {
                streaming.projected_lineage = Some(installed);
            }
        }
        Ok(())
    }
}
