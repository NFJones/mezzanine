//! Receipt-owned final log suffix, independent of execution and durable history.
//!
//! Only the current conversation's pending display sources are projected. The
//! exact installed lineage owns physical suffix rows; lower compositors strip
//! them before capturing baselines. Every installation recomposes this layer
//! last. Rendering never acknowledges input, persists pending as ordinary user
//! history, changes canonical chronology or writes the process surface.
//! Failed settled installations retain an exact pane/conversation obligation.
//! Ordinary drains retry due owners fairly in bounded batches; tracked idle
//! cleanup keeps recovery live with capped backoff, never a lifetime attempt
//! budget. Receipt sources and persistence retries remain independently owned.

use super::*;
use mez_agent::transcript::SteeringRecoveryStatus;

/// Bounds actor work per ordinary drain without bounding obligation lifetime.
const STEERING_DISPLAY_RETRIES_PER_DRAIN: usize = 8;
/// Initial pacing also prevents hot unrelated ingress from spinning on failure.
const STEERING_DISPLAY_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
/// Bounds settled-source installations per owner per ordinary drain.
const STEERING_DISPLAY_RECEIPTS_PER_DRAIN: usize = 8;

/// Whether one bounded reconciliation pass installed every eligible source.
///
/// A budget-exhausted pass is progress, not failure: the caller keeps the exact
/// pane/conversation obligation owned so the uninstalled tail stays paced for a
/// later ordinary drain instead of being dropped or reported as complete.
pub(crate) enum SteeringReconcileOutcome {
    /// No eligible settled source was left uninstalled by this pass.
    Complete,
    /// Another eligible settled source existed beyond the installation budget.
    BudgetExhausted,
}

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
    let reference = mez_mux::copy::encode_copy_source_reference_in_group(occurrence, 0);
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
    /// execution. Failure retains the exact obligation until install/retirement.
    pub(crate) fn request_steering_presentation(&mut self, pane: &str) {
        let Some(conversation) = self
            .agent_shell_store()
            .get(pane)
            .map(|session| session.session_id.clone())
        else {
            return;
        };
        let key = (pane.to_string(), conversation);
        self.presentation
            .steering_presentation_retries
            .retain(|(owner, source), _| owner != pane || source == &key.1);
        // Dirty checkpoint publication and other immediate requests must not
        // bypass pacing or create a second attempt in the same ordinary drain.
        if self
            .presentation
            .steering_presentation_retries
            .contains_key(&key)
        {
            return;
        }
        let outcome =
            self.reconcile_steering_presentation(pane, STEERING_DISPLAY_RECEIPTS_PER_DRAIN);
        // Pacing starts when the attempt finishes, so a slow initial pass cannot
        // return already due and let the next ordinary drain retry without a gap.
        let retained_at = std::time::Instant::now();
        match outcome {
            Ok(SteeringReconcileOutcome::Complete) => {}
            Ok(SteeringReconcileOutcome::BudgetExhausted) => {
                // Bounded progress installed a prefix. Keep the tail owned and
                // paced rather than issuing a second attempt in this drain.
                self.presentation.steering_presentation_retries.insert(
                    key.clone(),
                    crate::runtime::render::RuntimeSteeringPresentationRetry {
                        next_attempt_at: retained_at + STEERING_DISPLAY_RETRY_DELAY,
                        backoff: 0,
                        diagnostic_emitted: false,
                    },
                );
            }
            Err(_) => {
                self.presentation.steering_presentation_retries.insert(
                    key.clone(),
                    crate::runtime::render::RuntimeSteeringPresentationRetry {
                        next_attempt_at: retained_at + STEERING_DISPLAY_RETRY_DELAY,
                        backoff: 0,
                        diagnostic_emitted: false,
                    },
                );
                self.record_steering_display_unavailable(pane, &key.1);
            }
        }
    }

    /// Records one bounded, source-free unavailability event per retained episode.
    ///
    /// A failed append leaves the diagnostic pending so the next paced attempt
    /// retries it once; successful emission disables further attempts. Authored
    /// display source and renderer errors never enter this payload, and this path
    /// cannot acknowledge input, provider work or settled effects.
    fn record_steering_display_unavailable(&mut self, pane: &str, conversation: &str) {
        let key = (pane.to_string(), conversation.to_string());
        let already_emitted = match self.presentation.steering_presentation_retries.get(&key) {
            Some(retry) => retry.diagnostic_emitted,
            None => return,
        };
        if already_emitted {
            return;
        }
        let emitted = self
            .append_lifecycle_event(
                crate::runtime::EventKind::Diagnostic,
                serde_json::json!({
                    "diagnostic": "steering display unavailable; receipt retained for paced display-only retry",
                    "pane_id": pane,
                    "conversation_id": conversation,
                })
                .to_string(),
            )
            .is_ok();
        if emitted
            && let Some(retry) = self
                .presentation
                .steering_presentation_retries
                .get_mut(&key)
        {
            retry.diagnostic_emitted = true;
        }
    }

    /// Keeps existing tracked idle cleanup live while display recovery is owed.
    /// Backoff bounds actual attempts even when other ingress drains frequently.
    pub(crate) fn steering_presentation_retry_timer_needed(&self) -> bool {
        !self.presentation.steering_presentation_retries.is_empty()
    }

    /// Models elapsed display backoff in inert tests without sleeping or workers.
    #[cfg(test)]
    pub(crate) fn make_steering_presentation_retries_due_for_tests(&mut self) {
        for retry in self.presentation.steering_presentation_retries.values_mut() {
            retry.next_attempt_at = std::time::Instant::now();
        }
    }

    /// Attempts a bounded fair batch of due original-conversation obligations.
    /// Failure retains the obligation with capped backoff indefinitely. No
    /// provider, input, persistence acknowledgment or canonical effects replay.
    pub(crate) fn retry_steering_presentations(&mut self) {
        let mut retries = std::mem::take(&mut self.presentation.steering_presentation_retries);
        retries.retain(|(pane, conversation), _| {
            self.find_pane_descriptor(pane).is_some()
                && self
                    .agent_shell_store()
                    .get(pane)
                    .is_some_and(|session| session.session_id == *conversation)
        });
        self.presentation.steering_presentation_retries = retries;
        let now = std::time::Instant::now();
        let cursor = &self.presentation.steering_presentation_retry_cursor;
        let retries = &self.presentation.steering_presentation_retries;
        let due = retries
            .range((
                std::ops::Bound::Excluded(cursor.clone()),
                std::ops::Bound::Unbounded,
            ))
            .chain(retries.range(..=cursor.clone()))
            .filter(|(_, retry)| retry.next_attempt_at <= now)
            .take(STEERING_DISPLAY_RETRIES_PER_DRAIN)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in due {
            self.presentation.steering_presentation_retry_cursor = key.clone();
            match self.reconcile_steering_presentation(&key.0, STEERING_DISPLAY_RECEIPTS_PER_DRAIN)
            {
                Ok(SteeringReconcileOutcome::Complete) => {
                    self.presentation.steering_presentation_retries.remove(&key);
                }
                Ok(SteeringReconcileOutcome::BudgetExhausted) => {
                    // Keep the installed-prefix tail owned and due again on the
                    // next ordinary drain without inflating failure backoff.
                    let progressed_at = std::time::Instant::now();
                    if let Some(retry) = self
                        .presentation
                        .steering_presentation_retries
                        .get_mut(&key)
                    {
                        retry.next_attempt_at = progressed_at + STEERING_DISPLAY_RETRY_DELAY;
                    }
                }
                Err(_) => {
                    // Capture the deadline after the attempt so a slow batch
                    // cannot consume the next backoff window before returning.
                    let failed_at = std::time::Instant::now();
                    if let Some(retry) = self
                        .presentation
                        .steering_presentation_retries
                        .get_mut(&key)
                    {
                        retry.backoff = retry.backoff.saturating_add(1).min(5);
                        retry.next_attempt_at =
                            failed_at + STEERING_DISPLAY_RETRY_DELAY * (1u32 << retry.backoff);
                    }
                    self.record_steering_display_unavailable(&key.0, &key.1);
                }
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
        let entry = mez_core::presentation::PresentationEntry {
            id: source.receipt.id.clone(),
            occurrence: source.receipt.occurrence,
            first_row: true,
        };
        let entry = if self
            .presentation
            .agent_presentation_replay_panes
            .contains(pane)
        {
            self.presentation
                .occurrence_override
                .as_deref()
                .filter(|(owner, _)| owner == pane)
                .map(|(_, entry)| entry.clone())
                .unwrap_or(entry)
        } else {
            entry
        };
        self.with_presentation_entry(pane, entry, |service| {
            service.append_agent_terminal_rendered_lines_to_buffer(
                pane,
                AgentTerminalPresentationStyle::UserPrompt,
                &lines,
                &copies,
                Some((&encoded, crate::storage::transcript::steering::CONTENT_TYPE)),
            )
        })
    }

    /// Reconciles receipt status without acknowledging input or replaying effects.
    /// Successful installations are occurrence-fenced; failed installs can retry.
    /// At most `max_installs` settled sources are installed so one drain cannot do
    /// unbounded rendering work; observed surplus is reported, never dropped.
    pub(crate) fn reconcile_steering_presentation(
        &mut self,
        pane: &str,
        max_installs: usize,
    ) -> Result<SteeringReconcileOutcome> {
        let conversation = self
            .agent_shell_store()
            .get(pane)
            .map(|session| session.session_id.clone())
            .ok_or_else(|| MezError::invalid_state("steering presentation owner unavailable"))?;
        let receipts = self.steering_presentation_receipts(pane)?;
        self.refresh_pending_steering_suffix(pane)?;
        let mut installed = 0usize;
        for receipt in receipts
            .into_iter()
            .filter(|receipt| receipt.status != SteeringRecoveryStatus::Pending)
        {
            let key = (pane.to_string(), conversation.clone(), receipt.id.clone());
            if self.presentation.presented_steering_receipts.contains(&key) {
                continue;
            }
            if installed >= max_installs {
                return Ok(SteeringReconcileOutcome::BudgetExhausted);
            }
            let source = crate::storage::transcript::steering::Source {
                version: 1,
                conversation_id: conversation.clone(),
                receipt,
            };
            self.append_settled_steering_source(pane, &source)?;
            self.presentation.presented_steering_receipts.insert(key);
            installed = installed.saturating_add(1);
            // A later receipt may fail in this same pass. Already installed
            // prefixes must become observable without waiting for whole-pass
            // success; the existing per-client queue coalesces these effects.
            let effects = self.render_effects_for_clients_projecting_pane(
                pane,
                crate::runtime::RenderInvalidationReason::FullRedraw,
            );
            self.presentation.defer_render_effects(effects);
        }
        Ok(SteeringReconcileOutcome::Complete)
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
        // A fixed gutter plus at least one body cell must fit physically.
        // Otherwise terminal wrapping would exceed the owned suffix row count.
        // Keep accepted source in receipts until a usable layout is available.
        if usize::from(size.columns) <= UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX) {
            return Ok((screen, 0));
        }
        let budget = self
            .agent_composer_layout_for_pane(pane, usize::from(size.columns), usize::from(size.rows))
            .log
            .rows
            .saturating_sub(1);
        if budget == 0 {
            return Ok((screen, 0));
        }
        let width = if self.presentation.settings.terminal_agent_log_timestamps {
            self.presentation
                .settings
                .log_frame_settings()
                .0
                .frame(usize::from(size.columns))
                .body_budget
                .max(1)
        } else {
            bounded_agent_terminal_presentation_columns(
                usize::from(size.columns),
                self.presentation.settings.terminal_agent_wrap_column_cap,
            )
            .saturating_sub(UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX))
            .max(1)
        };
        let (mut lines, mut row_entries): (Vec<_>, Vec<_>) = pending
            .iter()
            .flat_map(|entry| {
                wrapped_prefixed_agent_terminal_lines("user> [pending] ", &entry.display, width)
                    .into_iter()
                    .enumerate()
                    .map(|(index, line)| {
                        (
                            line,
                            Some(mez_core::presentation::PresentationEntry {
                                id: entry.id.clone(),
                                occurrence: entry.occurrence,
                                first_row: index == 0,
                            }),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unzip();
        if lines.len() > budget {
            let hidden_rows = lines.len().saturating_sub(budget.saturating_sub(1));
            let tail = lines.split_off(hidden_rows);
            let retained_entries = row_entries.split_off(hidden_rows);
            row_entries = std::iter::once(None).chain(retained_entries).collect();
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
        let mut occurrence_ranges = Vec::new();
        for (line, entry) in lines.iter().zip(row_entries) {
            let start = bytes.len();
            append_styled_agent_terminal_rendered_line(
                &mut bytes,
                AgentTerminalPresentationStyle::UserPrompt,
                line,
                self.ui_theme(),
            );
            bytes.push_str("\x1b[0m\r\n");
            if let Some(entry) = entry {
                occurrence_ranges.push((start..bytes.len(), entry));
            }
        }
        Self::feed_presentation_ranges(
            &mut screen,
            bytes.as_bytes(),
            &occurrence_ranges,
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
