//! Actor-owned shell-preview composition, settlement, and retirement.
//!
//! Previews are transient layers over the existing conversation screen, never
//! a second presentation store. Every baseline reuse and installation retains
//! the exact conversation and installed-screen lineage checks; stale metadata
//! cannot erase intervening durable output or another action's live suffix.

use super::*;
use crate::runtime::render::{RuntimeAgentShellPreview, RuntimeAgentShellPreviewPresentation};

impl RuntimeSessionService {
    /// Injects one pre-install failure after the specified successful installs.
    #[cfg(test)]
    pub(crate) fn fail_agent_presentation_install_for_tests(&mut self, after: usize) {
        self.presentation.agent_install_failure_countdown = Some(after);
    }

    /// Holds the existing preinstall failure cut across callback/idle attempts.
    /// This does not change execution, queues, policy or production retry limits.
    #[cfg(test)]
    pub(crate) fn hold_agent_presentation_installs_for_tests(&mut self, held: bool) {
        self.presentation.agent_install_failure_held = held;
        self.presentation.agent_install_failure_target = None;
    }

    /// Holds the same preinstall cut for one exact accepted entry. Other panes
    /// and unrelated status rows remain healthy so child settlement can finish.
    #[cfg(test)]
    pub(crate) fn hold_agent_presentation_entry_for_tests(
        &mut self,
        pane: &str,
        entry: &str,
        held: bool,
    ) {
        self.presentation.agent_install_failure_held = held;
        self.presentation.agent_install_failure_target =
            held.then(|| (pane.to_string(), entry.to_string()));
    }

    /// Reports whether the current install matches the selected test-only cut.
    /// This affects presentation refusal only, never policy or execution state.
    #[cfg(test)]
    pub(crate) fn agent_presentation_install_held_for_tests(&self, pane: &str) -> bool {
        self.presentation.agent_install_failure_held
            && self
                .presentation
                .agent_install_failure_target
                .as_ref()
                .is_none_or(|(target_pane, target_entry)| {
                    target_pane == pane
                        && self
                            .presentation
                            .occurrence_override
                            .as_ref()
                            .is_some_and(|active| active.0 == pane && active.1.id == *target_entry)
                })
    }

    /// Removes previews whose final output has already been retained.
    pub(super) fn retire_settled_agent_shell_previews(
        presentation: &mut RuntimeAgentShellPreviewPresentation,
    ) {
        for owner in std::mem::take(&mut presentation.settled_owners) {
            presentation.previews.remove(&owner);
            presentation.retirement_requested.remove(&owner);
        }
    }

    /// Projects all active shell previews onto one preview-free pane screen.
    pub(in crate::runtime::render::presentation) fn append_agent_shell_previews_to_screen(
        screen: &mut TerminalScreen,
        previews: &std::collections::BTreeMap<
            RuntimeAgentShellPreviewOwner,
            RuntimeAgentShellPreview,
        >,
        ui_theme: &mez_mux::theme::UiTheme,
        max_visual_rows: usize,
        column_cap: usize,
    ) -> Result<usize> {
        let mut ordered = previews.values().collect::<Vec<_>>();
        ordered.sort_by_key(|preview| preview.first_seen_order);
        let (policy, _) = screen.log_frame_settings();
        let content_columns = if policy.timestamps {
            mez_core::presentation::LogFramePolicy {
                wrap_cap: column_cap,
                ..policy
            }
            .frame(usize::from(screen.size().columns))
            .body_budget
            .max(1)
        } else {
            bounded_agent_terminal_presentation_columns(
                usize::from(screen.size().columns),
                column_cap,
            )
            .saturating_sub(UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX))
            .max(1)
        };
        // A live window cannot own more rows than the pane can display. Keep
        // each owner's source, but clip the composite before feeding terminal
        // bytes so its head never becomes an obsolete insertion point in history.
        let visual_rows = ordered
            .into_iter()
            .flat_map(|preview| {
                let rows =
                    shell_output_preview_visual_rows(&preview.lines, content_columns, usize::MAX);
                let omitted = rows.len().saturating_sub(max_visual_rows);
                rows.into_iter()
                    .enumerate()
                    .skip(omitted)
                    .map(|(index, line)| {
                        let mut entry = preview.entry.clone();
                        entry.first_row = preview.starts_at_entry && index == 0;
                        (line, entry)
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let start = visual_rows
            .len()
            .saturating_sub(usize::from(screen.size().rows));
        let mut bytes = String::new();
        let mut physical_rows = 0usize;
        let cursor = screen.cursor_state();
        let current_line_has_content = screen
            .visible_lines()
            .get(cursor.row)
            .is_some_and(|line| !line.trim().is_empty());
        if cursor.column == 0 && !current_line_has_content {
            bytes.push('\r');
        } else {
            bytes.push_str("\r\n");
        }
        let mut first_line = true;
        let mut occurrence_ranges = Vec::new();
        for (line, entry) in visual_rows.into_iter().skip(start) {
            if !first_line {
                bytes.push_str("\r\n");
            }
            let occurrence_start = bytes.len();
            first_line = false;
            physical_rows = physical_rows.saturating_add(1);
            let rendition =
                agent_terminal_label_rendition(AgentTerminalPresentationStyle::Status, ui_theme);
            append_styled_agent_terminal_rendered_line(
                &mut bytes,
                AgentTerminalPresentationStyle::Status,
                &RichTextLine {
                    display: line,
                    style_spans: vec![TerminalStyleSpan {
                        start: 0,
                        length: content_columns,
                        rendition,
                    }],
                    copy_text: None,
                    kind: RichTextLineKind::Normal,
                },
                ui_theme,
            );
            bytes.push_str("\x1b[0m");
            occurrence_ranges.push((occurrence_start..bytes.len(), entry));
        }
        if first_line {
            return Ok(0);
        }
        Self::feed_presentation_ranges(
            screen,
            bytes.as_bytes(),
            &occurrence_ranges,
            "projecting shell previews",
        )?;
        Ok(physical_rows)
    }

    /// Removes installed transient layers while preserving their viewport displacement.
    pub(in crate::runtime::render::presentation) fn clear_installed_agent_transient_suffixes(
        &self,
        pane_id: &str,
        conversation_id: &str,
        installed_lineage: u64,
        preview_rows: usize,
        progress_rows: usize,
    ) -> Option<TerminalScreen> {
        if self.agent_pane_screen_lineage(pane_id, conversation_id) != Some(installed_lineage) {
            return None;
        }
        let mut screen = self.agent_screen_without_pending_steering(pane_id, conversation_id)?;
        let pending_rows = self
            .presentation
            .pending_steering_suffixes
            .get(pane_id)
            .filter(|(owner, lineage, _)| owner == conversation_id && *lineage == installed_lineage)
            .map_or(0, |(_, _, rows)| *rows);
        if progress_rows > 0 {
            let suffix = screen.capture_transient_suffix(progress_rows, true)?;
            if !screen.clear_transient_suffix(suffix) {
                return None;
            }
        }
        if preview_rows > 0 {
            let suffix = screen
                .capture_transient_suffix(preview_rows, progress_rows > 0 || pending_rows > 0)?;
            if !screen.clear_transient_suffix(suffix) {
                return None;
            }
        }
        Some(screen)
    }

    /// Returns a preview-free candidate and retained projection for one pane write.
    ///
    /// Exact installed-screen lineage is required before the durable baseline is
    /// reused. A mismatch discards stale projection metadata without changing
    /// the pane, so an intervening durable row can never be erased.
    pub(super) fn agent_shell_preview_write_base(
        &mut self,
        pane_id: &str,
    ) -> Result<(
        String,
        TerminalScreen,
        Option<RuntimeAgentShellPreviewPresentation>,
    )> {
        let (conversation_id, _) = self.agent_presentation_target(pane_id)?;
        let current_screen = self
            .agent_screen_without_pending_steering(pane_id, &conversation_id)
            .ok_or_else(|| {
                MezError::invalid_state("agent terminal presentation screen was not initialized")
            })?;
        let current_lineage = self
            .agent_pane_screen_lineage(pane_id, &conversation_id)
            .ok_or_else(|| {
                MezError::invalid_state("agent terminal presentation lineage was not initialized")
            })?;
        let presentation = self
            .presentation
            .agent_shell_output_previews
            .get(pane_id)
            .cloned();
        let owns_live_screen = presentation.as_ref().is_some_and(|presentation| {
            presentation.conversation_id == conversation_id
                && presentation.installed_lineage == current_lineage
        });
        if owns_live_screen {
            let mut presentation = presentation.ok_or_else(|| {
                MezError::invalid_state("checked shell preview presentation disappeared")
            })?;
            let progress_rows = self
                .presentation
                .action_presentation_progress
                .get(pane_id)
                .filter(|progress| {
                    progress.conversation_id == conversation_id
                        && progress.installed_lineage == current_lineage
                })
                .map_or(0, |progress| progress.transient_rows);
            let Some(baseline) = self.clear_installed_agent_transient_suffixes(
                pane_id,
                &conversation_id,
                current_lineage,
                presentation.transient_rows,
                progress_rows,
            ) else {
                if !presentation.retirement_requested.is_empty() {
                    self.defer_retained_shell_preview_cleanup(pane_id);
                    return Err(MezError::invalid_state(
                        "shell preview cleanup source is retained",
                    ));
                }
                self.presentation
                    .agent_shell_output_previews
                    .remove(pane_id);
                return Ok((conversation_id, current_screen, None));
            };
            Self::retire_settled_agent_shell_previews(&mut presentation);
            if presentation.previews.is_empty() {
                return Ok((conversation_id, baseline, None));
            }
            presentation.baseline_screen = std::sync::Arc::new(baseline.clone());
            presentation.transient_rows = 0;
            return Ok((conversation_id, baseline, Some(presentation)));
        }
        if presentation
            .as_ref()
            .is_some_and(|owner| !owner.retirement_requested.is_empty())
        {
            self.defer_retained_shell_preview_cleanup(pane_id);
            return Err(MezError::invalid_state(
                "shell preview cleanup source is retained",
            ));
        }
        if presentation.is_some() {
            self.presentation
                .agent_shell_output_previews
                .remove(pane_id);
        }
        Ok((conversation_id, current_screen, None))
    }

    /// Atomically installs one durable candidate and reprojects active previews.
    pub(in crate::runtime::render::presentation) fn install_agent_shell_preview_write(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        baseline_screen: TerminalScreen,
        presentation: Option<RuntimeAgentShellPreviewPresentation>,
    ) -> Result<()> {
        #[cfg(test)]
        if self.agent_presentation_install_held_for_tests(pane_id) {
            return Err(MezError::invalid_state(
                "injected held pre-install presentation failure",
            ));
        }
        #[cfg(test)]
        if let Some(remaining) = self.presentation.agent_install_failure_countdown.as_mut() {
            if *remaining == 0 {
                self.presentation.agent_install_failure_countdown = None;
                return Err(MezError::invalid_state(
                    "injected pre-install presentation failure",
                ));
            }
            *remaining -= 1;
        }
        let current_lineage = self
            .agent_pane_screen_lineage(pane_id, conversation_id)
            .ok_or_else(|| {
                MezError::invalid_state("agent durable presentation lineage was not initialized")
            })?;
        let mut composite_screen = baseline_screen.clone();
        let mut presentation = presentation;
        let mut transient_rows = 0;
        if let Some(presentation) = presentation.as_ref() {
            let ui_theme = self.presentation.settings.ui_theme.clone();
            let max_preview_rows = self.terminal_shell_output_preview_lines();
            transient_rows = Self::append_agent_shell_previews_to_screen(
                &mut composite_screen,
                &presentation.previews,
                &ui_theme,
                max_preview_rows,
                self.presentation.settings.terminal_agent_wrap_column_cap,
            )?;
        }
        let (composite_screen, progress_presentation) = self
            .compose_action_presentation_progress_over(
                pane_id,
                conversation_id,
                current_lineage,
                composite_screen,
            )?;
        let installed_lineage = self
            .update_agent_pane_screen_preserving_interaction(
                pane_id,
                conversation_id,
                composite_screen,
            )
            .ok_or_else(|| {
                MezError::invalid_state("agent durable presentation conversation changed")
            })?;
        if let Some(mut presentation) = presentation.take() {
            presentation.installed_lineage = installed_lineage;
            presentation.baseline_screen = std::sync::Arc::new(baseline_screen);
            presentation.transient_rows = transient_rows;
            self.presentation
                .commit_shell_preview(pane_id, Some(presentation));
        } else {
            self.presentation.commit_shell_preview(pane_id, None);
        }
        self.restore_composed_action_presentation_progress(
            pane_id,
            installed_lineage,
            progress_presentation,
        )?;
        Ok(())
    }

    /// Exercises a tail-only preview without producer start-boundary proof.
    ///
    /// The full preview set is rebuilt from a preview-free baseline. Exact
    /// installed-screen lineage prevents a delayed update from replacing any
    /// durable output that appeared after the previous projection.
    #[cfg(test)]
    pub(crate) fn update_agent_shell_output_preview(
        &mut self,
        pane_id: &str,
        owner: RuntimeAgentShellPreviewOwner,
        revision: u64,
        lines: &[String],
    ) -> Result<()> {
        self.update_agent_shell_output_preview_with_boundary(pane_id, owner, revision, lines, false)
    }

    /// Retains explicit producer start/crop evidence; bounded tails alone cannot
    /// prove an original row boundary. Identity and clock survive every revision.
    pub(crate) fn update_agent_shell_output_preview_with_boundary(
        &mut self,
        pane_id: &str,
        owner: RuntimeAgentShellPreviewOwner,
        revision: u64,
        lines: &[String],
        starts_at_entry: bool,
    ) -> Result<()> {
        if self.agent_shell_view_enabled(pane_id) || lines.is_empty() {
            return Ok(());
        }
        self.ensure_current_agent_presentation_screen(pane_id)?;
        let lines = lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| sanitized_agent_terminal_line(line))
            .collect::<Vec<_>>();
        if lines.is_empty() {
            return Ok(());
        }
        let (conversation_id, _) = self.agent_presentation_target(pane_id)?;
        let current_screen = self
            .agent_screen_without_pending_steering(pane_id, &conversation_id)
            .ok_or_else(|| {
                MezError::invalid_state("agent terminal presentation screen was not initialized")
            })?;
        let current_lineage = self
            .agent_pane_screen_lineage(pane_id, &conversation_id)
            .ok_or_else(|| {
                MezError::invalid_state("agent terminal presentation lineage was not initialized")
            })?;
        let stale_projection = self
            .presentation
            .agent_shell_output_previews
            .get(pane_id)
            .is_some_and(|presentation| {
                presentation.conversation_id != conversation_id
                    || presentation.installed_lineage != current_lineage
            });
        if stale_projection {
            if self
                .presentation
                .agent_shell_output_previews
                .get(pane_id)
                .is_some_and(|owner| !owner.retirement_requested.is_empty())
            {
                return Ok(());
            }
            self.presentation
                .agent_shell_output_previews
                .remove(pane_id);
        }
        let preview_baseline = self
            .presentation
            .agent_streaming_say_presentations
            .get(pane_id)
            .filter(|streaming| {
                !streaming.layout_retired
                    && streaming.conversation_id == conversation_id
                    && streaming.installed_lineage == current_lineage
            })
            .map(|streaming| streaming.provider_screen.as_ref().clone())
            .or_else(|| {
                self.presentation
                    .action_presentation_progress
                    .get(pane_id)
                    .filter(|progress| {
                        progress.conversation_id == conversation_id
                            && progress.installed_lineage == current_lineage
                    })
                    .map(|progress| progress.baseline_screen.as_ref().clone())
            })
            .unwrap_or_else(|| current_screen.clone());
        let mut presentation = self
            .presentation
            .agent_shell_output_previews
            .get(pane_id)
            .cloned()
            .unwrap_or_else(|| RuntimeAgentShellPreviewPresentation {
                snapshot_current: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
                conversation_id: conversation_id.clone(),
                installed_lineage: self
                    .agent_pane_screen_lineage(pane_id, &conversation_id)
                    .unwrap_or_default(),
                baseline_screen: std::sync::Arc::new(preview_baseline),
                transient_rows: 0,
                next_order: 0,
                previews: std::collections::BTreeMap::new(),
                settled_owners: std::collections::BTreeSet::new(),
                retirement_requested: std::collections::BTreeSet::new(),
                retirement_refused: false,
                replay_unavailable: None,
            });
        if presentation.settled_owners.contains(&owner)
            || presentation
                .previews
                .get(&owner)
                .is_some_and(|preview| revision <= preview.revision)
        {
            self.presentation
                .agent_shell_output_previews
                .insert(pane_id.to_string(), presentation);
            return Ok(());
        }
        let first_seen_order = presentation
            .previews
            .get(&owner)
            .map(|preview| preview.first_seen_order)
            .unwrap_or_else(|| {
                let order = presentation.next_order;
                presentation.next_order = presentation.next_order.saturating_add(1);
                order
            });
        let entry = presentation
            .previews
            .get(&owner)
            .map(|preview| preview.entry.clone())
            .unwrap_or_else(|| {
                self.presentation_entry(
                    pane_id,
                    mez_core::presentation::PresentationOccurrenceBasis::LocalObservation,
                )
            });
        presentation.previews.insert(
            owner,
            RuntimeAgentShellPreview {
                first_seen_order,
                entry,
                starts_at_entry,
                revision,
                lines,
            },
        );
        let ui_theme = self.presentation.settings.ui_theme.clone();
        let max_preview_rows = self.terminal_shell_output_preview_lines();
        let mut candidate = presentation.baseline_screen.as_ref().clone();
        let transient_rows = Self::append_agent_shell_previews_to_screen(
            &mut candidate,
            &presentation.previews,
            &ui_theme,
            max_preview_rows,
            self.presentation.settings.terminal_agent_wrap_column_cap,
        )?;
        let (candidate, progress_presentation) = self.compose_action_presentation_progress_over(
            pane_id,
            &conversation_id,
            current_lineage,
            candidate,
        )?;
        let installed_lineage = self
            .update_agent_pane_screen_preserving_interaction(pane_id, &conversation_id, candidate)
            .ok_or_else(|| {
                MezError::invalid_state("shell preview presentation conversation changed")
            })?;
        if let Some(streaming) = self
            .presentation
            .agent_streaming_say_presentations
            .get_mut(pane_id)
            .filter(|streaming| {
                !streaming.layout_retired
                    && streaming.conversation_id == conversation_id
                    && streaming.installed_lineage == current_lineage
            })
        {
            streaming.installed_lineage = installed_lineage;
            if streaming.projected_lineage.is_some() {
                streaming.projected_lineage = Some(installed_lineage);
            }
        }
        presentation.installed_lineage = installed_lineage;
        presentation.transient_rows = transient_rows;
        self.presentation
            .commit_shell_preview(pane_id, Some(presentation));
        self.restore_composed_action_presentation_progress(
            pane_id,
            installed_lineage,
            progress_presentation,
        )?;
        Ok(())
    }

    /// Settles one owner while retaining its final tail until later pane content.
    ///
    /// Settled owners reject later preview revisions. Their final rows remain
    /// part of the transient projection until the next durable pane append or
    /// provider streaming update drops settled rows before recomposing active previews.
    pub(crate) fn settle_agent_shell_output_preview(
        &mut self,
        pane_id: &str,
        owner: &RuntimeAgentShellPreviewOwner,
    ) -> bool {
        let Some(presentation) = self
            .presentation
            .agent_shell_output_previews
            .get_mut(pane_id)
        else {
            return false;
        };
        if !presentation.previews.contains_key(owner) {
            return false;
        }
        if !presentation.settled_owners.contains(owner) {
            presentation
                .snapshot_current
                .store(false, std::sync::atomic::Ordering::Release);
            presentation.snapshot_current =
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        }
        presentation.settled_owners.insert(owner.clone())
    }

    /// Invalidates stale workers/snapshots before cleanup intent is retained.
    /// This changes no pixels and grants no worker or terminal authority.
    fn fence_shell_preview_cleanup_intent(&mut self, pane_id: &str) {
        if let Some(owner) = self
            .presentation
            .agent_shell_output_previews
            .get_mut(pane_id)
        {
            owner
                .snapshot_current
                .store(false, std::sync::atomic::Ordering::Release);
            owner.snapshot_current = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
            owner.retirement_refused = false;
            owner.replay_unavailable = None;
        }
    }

    /// Retires every preview owned by one turn without disturbing other owners.
    ///
    /// Exact installed-screen lineage is required before the remaining owners
    /// are reprojected. Refusal retains selected source and cleanup intent;
    /// neither unrelated owners nor replacement pixels are consumed on error.
    pub(crate) fn retire_agent_shell_output_previews_for_turn(
        &mut self,
        turn_id: &str,
    ) -> Result<usize> {
        let pane_ids = self
            .presentation
            .agent_shell_output_previews
            .iter()
            .filter(|(_pane_id, presentation)| {
                presentation
                    .previews
                    .keys()
                    .any(|owner| owner.turn_id == turn_id)
            })
            .map(|(pane_id, _presentation)| pane_id.clone())
            .collect::<Vec<_>>();
        let mut retired = 0usize;
        for pane_id in pane_ids {
            let Some(mut presentation) = self
                .presentation
                .agent_shell_output_previews
                .get(&pane_id)
                .cloned()
            else {
                continue;
            };
            let owners = presentation
                .previews
                .keys()
                .filter(|owner| owner.turn_id == turn_id)
                .cloned()
                .collect::<Vec<_>>();
            if owners
                .iter()
                .any(|owner| !presentation.retirement_requested.contains(owner))
            {
                self.fence_shell_preview_cleanup_intent(&pane_id);
            }
            if let Some(current) = self
                .presentation
                .agent_shell_output_previews
                .get_mut(&pane_id)
            {
                current.retirement_requested.extend(owners.iter().cloned());
                current.settled_owners.extend(owners);
                presentation = current.clone();
            }
            let selected_count = presentation.retirement_requested.len();
            let owns_live_screen = self
                .agent_pane_screen_lineage(&pane_id, &presentation.conversation_id)
                == Some(presentation.installed_lineage)
                && self
                    .agent_shell_store()
                    .get(&pane_id)
                    .is_some_and(|session| session.session_id == presentation.conversation_id);
            if !owns_live_screen {
                self.defer_retained_shell_preview_cleanup(&pane_id);
                continue;
            }
            for owner in std::mem::take(&mut presentation.retirement_requested) {
                presentation.previews.remove(&owner);
                presentation.settled_owners.remove(&owner);
            }
            let progress_rows = self
                .presentation
                .action_presentation_progress
                .get(&pane_id)
                .filter(|progress| {
                    progress.conversation_id == presentation.conversation_id
                        && progress.installed_lineage == presentation.installed_lineage
                })
                .map_or(0, |progress| progress.transient_rows);
            let Some(baseline) = self.clear_installed_agent_transient_suffixes(
                &pane_id,
                &presentation.conversation_id,
                presentation.installed_lineage,
                presentation.transient_rows,
                progress_rows,
            ) else {
                self.defer_retained_shell_preview_cleanup(&pane_id);
                continue;
            };
            let conversation_id = presentation.conversation_id.clone();
            let retained = (!presentation.previews.is_empty()).then_some(presentation);
            self.install_agent_shell_preview_write(&pane_id, &conversation_id, baseline, retained)?;
            retired = retired.saturating_add(selected_count);
        }
        Ok(retired)
    }

    /// Checks the existing renderer's actual canonical-replay eligibility.
    fn shell_preview_rebuild_is_eligible(
        &self,
        pane: &str,
        owner: &RuntimeAgentShellPreviewPresentation,
    ) -> bool {
        !self.presentation_replay_is_unavailable(
            pane,
            &owner.conversation_id,
            owner.replay_unavailable,
        ) && self.persistence.transcript_store().is_some()
            && self.find_pane_descriptor(pane).is_some()
            && self.agent_shell_store().get(pane).is_some_and(|session| {
                session.session_id == owner.conversation_id
                    && session.visibility == mez_agent::AgentShellVisibility::Visible
                    && !session.ephemeral
            })
            && self
                .agent_pane_screen_lineage(pane, &owner.conversation_id)
                .is_some()
            && self
                .agent_pane_screen(pane)
                .is_some_and(|screen| !screen.normal_viewport_detached_from_history())
    }

    /// Retains the unowned source while handing recovery to the already bounded
    /// canonical renderer. Ineligible/no-store sources remain explicit refusals,
    /// not cleanup acknowledgments or endless suffix-only idle attempts.
    fn defer_retained_shell_preview_cleanup(&mut self, pane: &str) {
        if let Some(owner) = self.presentation.agent_shell_output_previews.get_mut(pane) {
            owner.retirement_refused = true;
        }
        let eligible = self
            .presentation
            .agent_shell_output_previews
            .get(pane)
            .is_some_and(|owner| self.shell_preview_rebuild_is_eligible(pane, owner));
        if eligible && let Some(size) = self.agent_pane_screen(pane).map(TerminalScreen::size) {
            self.presentation
                .agent_presentation_projection_cache
                .remove(pane);
            self.presentation
                .defer_agent_presentation_resize(pane, size);
        }
    }

    /// Keeps existing idle cleanup enrolled for exact bound cancelled sources.
    pub(crate) fn shell_preview_retirement_retry_needed(&self) -> bool {
        self.presentation
            .agent_shell_output_previews
            .iter()
            .any(|(pane, owner)| {
                !owner.retirement_requested.is_empty()
                    && (!owner.retirement_refused
                        || self.shell_preview_rebuild_is_eligible(pane, owner))
                    && self
                        .agent_shell_store()
                        .get(pane)
                        .is_some_and(|session| session.session_id == owner.conversation_id)
                    && self
                        .agent_pane_screen_lineage(pane, &owner.conversation_id)
                        .is_some()
            })
    }

    /// Retries only existing cleanup intent on at most eight pane owners. No
    /// worker, hook, canonical result or provider continuation is re-entered.
    pub(crate) fn retry_shell_preview_retirements(&mut self) -> usize {
        let mut panes = self
            .presentation
            .agent_shell_output_previews
            .iter()
            .filter(|(pane, owner)| {
                !owner.retirement_requested.is_empty()
                    && (!owner.retirement_refused
                        || self.shell_preview_rebuild_is_eligible(pane, owner))
                    && self
                        .agent_shell_store()
                        .get(pane)
                        .is_some_and(|session| session.session_id == owner.conversation_id)
                    && self
                        .agent_pane_screen_lineage(pane, &owner.conversation_id)
                        .is_some()
            })
            .map(|(pane, owner)| {
                (
                    pane.clone(),
                    owner
                        .retirement_requested
                        .iter()
                        .next()
                        .map(|owner| owner.turn_id.clone()),
                )
            })
            .collect::<Vec<_>>();
        if let Some(cursor) = &self.presentation.shell_preview_retirement_cursor {
            let split = panes.partition_point(|(pane, _)| pane <= cursor);
            panes.rotate_left(split);
        }
        let mut retired = 0usize;
        for (pane, turn) in panes.into_iter().take(8) {
            self.presentation.shell_preview_retirement_cursor = Some(pane);
            if let Some(turn) = turn {
                match self.retire_agent_shell_output_previews_for_turn(&turn) {
                    Ok(count) => retired = retired.saturating_add(count),
                    Err(error) => self.note_completion_optional_error(&turn, Err(error)),
                }
            }
        }
        retired
    }

    /// Returns structured shell preview state for chronology regressions.
    #[cfg(test)]
    pub(crate) fn agent_shell_output_previews_for_tests(
        &self,
        pane_id: &str,
    ) -> Vec<(RuntimeAgentShellPreviewOwner, u64, u64, Vec<String>)> {
        let Some(presentation) = self.presentation.agent_shell_output_previews.get(pane_id) else {
            return Vec::new();
        };
        let mut previews = presentation
            .previews
            .iter()
            .map(|(owner, preview)| {
                (
                    owner.clone(),
                    preview.first_seen_order,
                    preview.revision,
                    preview.lines.clone(),
                )
            })
            .collect::<Vec<_>>();
        previews.sort_by_key(|(_owner, first_seen_order, _revision, _lines)| *first_seen_order);
        previews
    }

    /// Clears transient shell previews only while their installed lineage owns the pane.
    pub(crate) fn clear_agent_shell_output_status_line(&mut self, pane_id: &str) -> Result<()> {
        let Some(presentation) = self
            .presentation
            .agent_shell_output_previews
            .remove(pane_id)
        else {
            return Ok(());
        };
        let owns_live_screen = self
            .agent_pane_screen_lineage(pane_id, &presentation.conversation_id)
            == Some(presentation.installed_lineage);
        if owns_live_screen
            && self
                .update_agent_pane_screen_preserving_interaction(
                    pane_id,
                    &presentation.conversation_id,
                    presentation.baseline_screen.as_ref().clone(),
                )
                .is_none()
        {
            return Err(MezError::invalid_state(
                "shell preview cleanup conversation changed",
            ));
        }
        Ok(())
    }
}
