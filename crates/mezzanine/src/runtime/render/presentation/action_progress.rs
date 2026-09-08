//! Executor-owned action progress composition and settlement.
//!
//! This layer is intentionally separate from provider streaming, shell tail
//! previews, canonical action results, and durable chronology. It accepts only
//! an exact live execution identity, retains bounded cumulative source, and
//! projects from an exact conversation/screen baseline. Provisional rows become
//! durable presentation only after exact successful settlement; confirmed
//! mutation rows may be promoted as soon as the executor confirms their write.

use super::actions::bounded_agent_action_result_display_lines;
use super::buffer_apply::AGENT_PRESENTATION_STYLED_LINES_CONTENT_TYPE;
use super::diff::readable_agent_diff_display_lines_for_width;
use super::style::AgentTerminalPresentationStyle;
use super::text::{
    agent_terminal_label_rendition, agent_terminal_text_width,
    append_styled_agent_terminal_rendered_line, sanitized_agent_terminal_line,
};
use super::{RichTextLine, RichTextLineKind};
use crate::runtime::render::{
    MezError, Result, RuntimeActionPresentationProgressComponent,
    RuntimeActionPresentationProgressKey, RuntimeActionPresentationProgressPresentation,
    RuntimeActionPresentationProjectionContext, RuntimeSessionService, TerminalScreen,
    wrap_rich_text_line_to_width_with_source_ranges_hard,
};
use mez_agent::{
    ACTION_PRESENTATION_PROGRESS_MAX_SOURCE_BYTES, ActionPresentationComponentIdentity,
    ActionPresentationExecutionIdentity, ActionPresentationProgress, ActionResult, ActionStatus,
    AgentAction, AgentActionPayload, AgentShellVisibility, AgentTurnState,
};

/// Maximum live and promoted component identities retained for one pane.
const ACTION_PRESENTATION_PROGRESS_MAX_COMPONENTS_PER_PANE: usize = 128;
/// Maximum component identities retained for one action execution.
const ACTION_PRESENTATION_PROGRESS_MAX_COMPONENTS_PER_ACTION: usize = 64;

type RenderedProgressSection = (AgentTerminalPresentationStyle, Vec<RichTextLine>);

impl RuntimeSessionService {
    /// Applies one bounded cumulative executor progress snapshot.
    ///
    /// Acceptance requires an exact running turn/action and either the claimed
    /// native attempt marker or the live managed transaction marker and phase
    /// appropriate for the component. A stale identity, revision, conversation,
    /// or screen lineage returns `false` without changing the pane.
    pub(crate) fn apply_action_presentation_progress(
        &mut self,
        progress: ActionPresentationProgress,
    ) -> Result<bool> {
        let Some((pane_id, _action)) = self.action_progress_live_target(&progress)? else {
            return Ok(false);
        };
        if progress.source.len() > ACTION_PRESENTATION_PROGRESS_MAX_SOURCE_BYTES {
            return Ok(false);
        }
        self.ensure_current_agent_presentation_screen(&pane_id)?;
        let (conversation_id, _) = self.agent_presentation_target(&pane_id)?;
        let current_screen = self.agent_pane_screen(&pane_id).cloned().ok_or_else(|| {
            MezError::invalid_state("executor progress pane screen was not initialized")
        })?;
        let current_lineage = self
            .agent_pane_screen_lineage(&pane_id, &conversation_id)
            .ok_or_else(|| {
                MezError::invalid_state("executor progress lineage was not initialized")
            })?;
        let context = self.action_progress_projection_context(&pane_id)?;
        let mut presentation = match self
            .presentation
            .action_presentation_progress
            .remove(&pane_id)
        {
            Some(presentation)
                if presentation.conversation_id == conversation_id
                    && presentation.installed_lineage == current_lineage =>
            {
                presentation
            }
            Some(_) => return Ok(false),
            None => RuntimeActionPresentationProgressPresentation {
                conversation_id: conversation_id.clone(),
                installed_lineage: current_lineage,
                baseline_screen: std::sync::Arc::new(current_screen),
                transient_rows: 0,
                projected_context: context.clone(),
                next_order: 0,
                components: std::collections::BTreeMap::new(),
                promoted_components: std::collections::BTreeMap::new(),
            },
        };
        let key = RuntimeActionPresentationProgressKey {
            turn_id: progress.turn_id,
            action_id: progress.action_id,
            execution: progress.execution,
            component: progress.component,
        };
        if presentation.promoted_components.contains_key(&key)
            || presentation
                .components
                .get(&key)
                .is_some_and(|component| progress.revision <= component.revision)
        {
            self.presentation
                .action_presentation_progress
                .insert(pane_id, presentation);
            return Ok(false);
        }
        if !presentation.components.contains_key(&key) {
            let pane_components = presentation
                .components
                .len()
                .saturating_add(presentation.promoted_components.len());
            let action_components = presentation
                .components
                .keys()
                .chain(presentation.promoted_components.keys())
                .filter(|candidate| {
                    candidate.turn_id == key.turn_id && candidate.action_id == key.action_id
                })
                .count();
            if pane_components >= ACTION_PRESENTATION_PROGRESS_MAX_COMPONENTS_PER_PANE
                || action_components >= ACTION_PRESENTATION_PROGRESS_MAX_COMPONENTS_PER_ACTION
            {
                self.presentation
                    .action_presentation_progress
                    .insert(pane_id, presentation);
                return Ok(false);
            }
        }
        let first_seen_order = presentation
            .components
            .get(&key)
            .map(|component| component.first_seen_order)
            .unwrap_or_else(|| {
                let order = presentation.next_order;
                presentation.next_order = presentation.next_order.saturating_add(1);
                order
            });
        presentation.components.insert(
            key,
            RuntimeActionPresentationProgressComponent {
                first_seen_order,
                revision: progress.revision,
                source: progress.source,
                source_truncated: progress.source_truncated,
            },
        );
        presentation.projected_context = context;
        let sections = self.render_action_progress_components(&pane_id, &presentation)?;
        if sections.is_empty() {
            self.presentation
                .action_presentation_progress
                .insert(pane_id, presentation);
            return Ok(true);
        }
        let mut candidate = presentation.baseline_screen.as_ref().clone();
        let (_ansi_text, transient_rows) = Self::append_action_progress_sections_to_screen(
            &mut candidate,
            &sections,
            self.ui_theme(),
        )?;
        let installed_lineage = self
            .update_agent_pane_screen_preserving_interaction(&pane_id, &conversation_id, candidate)
            .ok_or_else(|| MezError::invalid_state("executor progress conversation changed"))?;
        self.synchronize_action_progress_companion_lineage(
            &pane_id,
            &conversation_id,
            current_lineage,
            installed_lineage,
        );
        presentation.installed_lineage = installed_lineage;
        presentation.transient_rows = transient_rows;
        self.presentation
            .action_presentation_progress
            .insert(pane_id, presentation);
        Ok(true)
    }

    /// Reconciles one exact retained component with a canonical terminal result.
    ///
    /// Exact successful provisional content is promoted and returns `true` so
    /// the caller may suppress final replay. Failed or mismatched provisional
    /// content is rolled back. Confirmed mutation evidence is promoted once and
    /// is never removed merely because a later action settlement failed.
    pub(crate) fn reconcile_action_presentation_progress(
        &mut self,
        progress: &ActionPresentationProgress,
        result: &ActionResult,
    ) -> Result<bool> {
        if result.turn_id != progress.turn_id
            || result.action_id != progress.action_id
            || !result.is_terminal()
        {
            return Ok(false);
        }
        let key = RuntimeActionPresentationProgressKey {
            turn_id: progress.turn_id.clone(),
            action_id: progress.action_id.clone(),
            execution: progress.execution.clone(),
            component: progress.component.clone(),
        };
        let Some(pane_id) = self.action_progress_pane_for_key(&key) else {
            return Ok(false);
        };
        let exact_retained = self
            .presentation
            .action_presentation_progress
            .get(&pane_id)
            .and_then(|presentation| presentation.components.get(&key))
            .is_some_and(|component| {
                component.revision == progress.revision
                    && component.source == progress.source
                    && component.source_truncated == progress.source_truncated
            });
        let exact_promoted = self
            .presentation
            .action_presentation_progress
            .get(&pane_id)
            .and_then(|presentation| presentation.promoted_components.get(&key))
            .is_some_and(|source| source == &progress.source);
        if !exact_retained && !exact_promoted {
            return Ok(false);
        }
        if key.component.is_confirmed() {
            if exact_retained {
                let _ = self.promote_confirmed_action_presentation_progress(progress)?;
            }
            return Ok(result.status == ActionStatus::Succeeded
                && result.content_text() == progress.source);
        }
        if exact_promoted {
            return Ok(result.status == ActionStatus::Succeeded
                && !result.is_error
                && result.content_text() == progress.source);
        }
        if result.status == ActionStatus::Succeeded
            && !result.is_error
            && result.content_text() == progress.source
        {
            self.promote_action_progress_component(&pane_id, &key, progress)?;
            return Ok(true);
        }
        self.rollback_action_progress_component(&pane_id, &key, progress)?;
        Ok(false)
    }

    /// Reconciles every retained component for one exact executor generation.
    ///
    /// The terminal result remains the only authority for success or failure.
    /// This helper reconstructs retained snapshots solely to retire or promote
    /// their presentation ownership at the matching actor settlement boundary.
    pub(crate) fn reconcile_action_presentation_progress_for_execution(
        &mut self,
        turn_id: &str,
        action_id: &str,
        execution: &ActionPresentationExecutionIdentity,
        result: &ActionResult,
    ) -> Result<bool> {
        let retained = self
            .presentation
            .action_presentation_progress
            .values()
            .flat_map(|presentation| presentation.components.iter())
            .filter(|(key, _)| {
                key.turn_id == turn_id && key.action_id == action_id && &key.execution == execution
            })
            .map(|(key, component)| ActionPresentationProgress {
                turn_id: key.turn_id.clone(),
                action_id: key.action_id.clone(),
                execution: key.execution.clone(),
                revision: component.revision,
                component: key.component.clone(),
                source: component.source.clone(),
                source_truncated: component.source_truncated,
            })
            .collect::<Vec<_>>();
        let mut suppress_final_replay = false;
        for progress in retained {
            suppress_final_replay |=
                self.reconcile_action_presentation_progress(&progress, result)?;
        }
        Ok(suppress_final_replay)
    }

    /// Promotes one exact executor-confirmed mutation into durable presentation.
    pub(crate) fn promote_confirmed_action_presentation_progress(
        &mut self,
        progress: &ActionPresentationProgress,
    ) -> Result<bool> {
        if !progress.component.is_confirmed() {
            return Ok(false);
        }
        let key = RuntimeActionPresentationProgressKey {
            turn_id: progress.turn_id.clone(),
            action_id: progress.action_id.clone(),
            execution: progress.execution.clone(),
            component: progress.component.clone(),
        };
        let Some(pane_id) = self.action_progress_pane_for_key(&key) else {
            return Ok(false);
        };
        self.promote_action_progress_component(&pane_id, &key, progress)
    }

    /// Removes every transient or suppression record owned by one action.
    pub(crate) fn retire_action_presentation_progress_for_action(
        &mut self,
        turn_id: &str,
        action_id: &str,
    ) -> Result<usize> {
        self.retire_action_presentation_progress_matching(|key| {
            key.turn_id == turn_id && key.action_id == action_id
        })
    }

    /// Removes every transient or suppression record owned by one turn.
    pub(crate) fn retire_action_presentation_progress_for_turn(
        &mut self,
        turn_id: &str,
    ) -> Result<usize> {
        self.retire_action_presentation_progress_matching(|key| key.turn_id == turn_id)
    }

    /// Returns active and promoted source counts for focused regressions.
    #[cfg(test)]
    pub(crate) fn action_presentation_progress_counts_for_tests(
        &self,
        pane_id: &str,
    ) -> (usize, usize) {
        self.presentation
            .action_presentation_progress
            .get(pane_id)
            .map_or((0, 0), |presentation| {
                (
                    presentation.components.len(),
                    presentation.promoted_components.len(),
                )
            })
    }

    /// Reports whether promoted mutation components exactly match final sections.
    pub(crate) fn promoted_action_patch_sections_match(
        &self,
        turn_id: &str,
        action_id: &str,
        execution: &ActionPresentationExecutionIdentity,
        sections: &[mez_agent::semantic_patch_planning::ApplyPatchConfirmedSection],
    ) -> bool {
        let promoted = self
            .presentation
            .action_presentation_progress
            .values()
            .flat_map(|presentation| presentation.promoted_components.iter())
            .filter(|(key, _)| {
                key.turn_id == turn_id
                    && key.action_id == action_id
                    && &key.execution == execution
                    && key.component.is_confirmed()
            })
            .collect::<Vec<_>>();
        promoted.len() == sections.len()
            && sections.iter().all(|section| {
                promoted.iter().any(|(key, source)| {
                    key.component
                        == ActionPresentationComponentIdentity::confirmed_mutation(
                            section.ordinal,
                            section.path.clone(),
                        )
                        && source.as_str() == section.diff
                })
            })
    }

    /// Reports whether final managed output contains every promoted mutation.
    pub(crate) fn promoted_action_patch_output_matches(
        &self,
        turn_id: &str,
        action_id: &str,
        execution: &ActionPresentationExecutionIdentity,
        output: &str,
    ) -> bool {
        let promoted = self
            .presentation
            .action_presentation_progress
            .values()
            .flat_map(|presentation| presentation.promoted_components.iter())
            .filter(|(key, _)| {
                key.turn_id == turn_id
                    && key.action_id == action_id
                    && &key.execution == execution
                    && key.component.is_confirmed()
            })
            .collect::<Vec<_>>();
        !promoted.is_empty()
            && promoted
                .iter()
                .all(|(_key, source)| output.contains(source.as_str()))
    }

    /// Composes retained executor progress over a newly rebuilt lower baseline.
    pub(super) fn compose_action_presentation_progress_over(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        expected_lineage: u64,
        baseline: TerminalScreen,
    ) -> Result<(
        TerminalScreen,
        Option<RuntimeActionPresentationProgressPresentation>,
    )> {
        let Some(mut presentation) = self
            .presentation
            .action_presentation_progress
            .remove(pane_id)
        else {
            return Ok((baseline, None));
        };
        if presentation.conversation_id != conversation_id
            || presentation.installed_lineage != expected_lineage
        {
            return Ok((baseline, None));
        }
        presentation.baseline_screen = std::sync::Arc::new(baseline.clone());
        presentation.projected_context = self.action_progress_projection_context(pane_id)?;
        let sections = self.render_action_progress_components(pane_id, &presentation)?;
        let mut composite = baseline;
        let (_ansi_text, transient_rows) = Self::append_action_progress_sections_to_screen(
            &mut composite,
            &sections,
            self.ui_theme(),
        )?;
        presentation.transient_rows = transient_rows;
        Ok((composite, Some(presentation)))
    }

    /// Restores action-progress ownership after a lower compositor installs a screen.
    pub(super) fn restore_composed_action_presentation_progress(
        &mut self,
        pane_id: &str,
        installed_lineage: u64,
        mut presentation: Option<RuntimeActionPresentationProgressPresentation>,
    ) {
        if let Some(mut presentation) = presentation.take() {
            presentation.installed_lineage = installed_lineage;
            self.presentation
                .action_presentation_progress
                .insert(pane_id.to_string(), presentation);
        }
    }

    fn action_progress_live_target(
        &self,
        progress: &ActionPresentationProgress,
    ) -> Result<Option<(String, AgentAction)>> {
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == progress.turn_id)
        else {
            return Ok(None);
        };
        if !matches!(
            turn.state,
            AgentTurnState::Running | AgentTurnState::Blocked
        ) {
            return Ok(None);
        }
        let Some(session) = self.agent_shell_store().get(&turn.pane_id) else {
            return Ok(None);
        };
        if session.session_id != turn.conversation_id {
            return Ok(None);
        }
        let Some(execution) = self.agent_turn_executions().get(&progress.turn_id) else {
            return Ok(None);
        };
        if !execution.action_results.iter().any(|result| {
            result.action_id == progress.action_id && result.status == ActionStatus::Running
        }) {
            return Ok(None);
        }
        let Some(action) = execution
            .response
            .action_batch
            .as_ref()
            .and_then(|batch| {
                batch
                    .actions
                    .iter()
                    .find(|action| action.id == progress.action_id)
            })
            .cloned()
        else {
            return Ok(None);
        };
        let identity_is_current = match &progress.execution {
            ActionPresentationExecutionIdentity::Attempt(marker) => {
                matches!(
                    (&action.payload, &progress.component),
                    (
                        AgentActionPayload::ShellCommand { .. },
                        ActionPresentationComponentIdentity::ShellOutput
                    ) | (
                        AgentActionPayload::ApplyPatch { .. },
                        ActionPresentationComponentIdentity::ConfirmedMutation { .. }
                    ) | (
                        AgentActionPayload::FetchUrl { .. } | AgentActionPayload::WebSearch { .. },
                        ActionPresentationComponentIdentity::ProvisionalReadBody
                    )
                ) && (self.native_shell_action_attempt_is_current(
                    &progress.turn_id,
                    &progress.action_id,
                    marker,
                ) || self.approved_external_action_attempt_is_current(
                    &progress.turn_id,
                    &progress.action_id,
                    marker,
                ))
            }
            ActionPresentationExecutionIdentity::Transaction(marker) => self
                .running_shell_transaction(marker)
                .is_some_and(|transaction| {
                    transaction.turn_id == progress.turn_id
                        && transaction.pane_id == turn.pane_id
                        && matches!(
                            &transaction.kind,
                            crate::runtime::RunningShellTransactionKind::AgentAction {
                                action_id
                            } if action_id.as_str() == progress.action_id.as_str()
                        )
                        && match &progress.component {
                            ActionPresentationComponentIdentity::ShellOutput => {
                                matches!(action.payload, AgentActionPayload::ShellCommand { .. })
                                    && mez_agent::semantic_patch_planning::apply_patch_transaction_phase(
                                        &transaction.command,
                                    )
                                    .is_none()
                            }
                            ActionPresentationComponentIdentity::ProvisionalReadBody => {
                                mez_agent::semantic_patch_planning::apply_patch_transaction_phase(
                                    &transaction.command,
                                ) == Some(
                                    mez_agent::semantic_patch_planning::ApplyPatchTransactionPhase::Read,
                                )
                            }
                            ActionPresentationComponentIdentity::ConfirmedMutation { .. } => {
                                matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
                                    && mez_agent::semantic_patch_planning::apply_patch_transaction_phase(
                                        &transaction.command,
                                    ) == Some(
                                        mez_agent::semantic_patch_planning::ApplyPatchTransactionPhase::Write,
                                    )
                            }
                        }
                }),
        };
        Ok(identity_is_current.then(|| (turn.pane_id.clone(), action)))
    }

    fn action_progress_projection_context(
        &self,
        pane_id: &str,
    ) -> Result<RuntimeActionPresentationProjectionContext> {
        let screen = self.agent_pane_screen(pane_id).ok_or_else(|| {
            MezError::invalid_state("executor progress projection screen is unavailable")
        })?;
        let visibility = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.visibility)
            .ok_or_else(|| {
                MezError::invalid_state("executor progress agent session is unavailable")
            })?;
        Ok(RuntimeActionPresentationProjectionContext {
            size: screen.size(),
            settings: self.presentation.settings.clone(),
            visibility,
            debug: self.agent_debug_enabled(pane_id),
            trace: self.agent_trace_enabled(pane_id),
            shell_view: self.agent_shell_view_enabled(pane_id),
        })
    }

    fn render_action_progress_components(
        &self,
        pane_id: &str,
        presentation: &RuntimeActionPresentationProgressPresentation,
    ) -> Result<Vec<RenderedProgressSection>> {
        if presentation.projected_context.visibility != AgentShellVisibility::Visible {
            return Ok(Vec::new());
        }
        let display_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let mut ordered = presentation.components.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|(_key, component)| component.first_seen_order);
        let mut sections = Vec::new();
        for (key, component) in ordered {
            let rendered = match &key.component {
                ActionPresentationComponentIdentity::ShellOutput
                    if !presentation.projected_context.shell_view
                        && self.agent_shell_transaction_action_shows_live_output(
                            &key.turn_id,
                            &key.action_id,
                        ) =>
                {
                    let max_rows = self.terminal_shell_output_preview_lines();
                    let mut rendered =
                        mez_agent::shell_observation::latest_agent_shell_transaction_output_lines(
                            &component.source,
                            max_rows,
                        )
                        .into_iter()
                        .flat_map(|line| {
                            Self::shell_output_progress_lines(line, display_width, self.ui_theme())
                        })
                        .collect::<Vec<_>>();
                    if rendered.len() > max_rows {
                        rendered.drain(..rendered.len() - max_rows);
                    }
                    rendered
                }
                ActionPresentationComponentIdentity::ProvisionalReadBody
                    if !presentation.projected_context.shell_view
                        && (presentation.projected_context.debug
                            || presentation.projected_context.trace) =>
                {
                    bounded_agent_action_result_display_lines(&component.source)
                        .into_iter()
                        .flat_map(|line| Self::plain_action_progress_lines(line, display_width))
                        .collect::<Vec<_>>()
                }
                ActionPresentationComponentIdentity::ConfirmedMutation { .. }
                    if !presentation.projected_context.shell_view =>
                {
                    readable_agent_diff_display_lines_for_width(
                        &component.source,
                        self.ui_theme(),
                        display_width,
                    )
                }
                _ => Vec::new(),
            };
            if !rendered.is_empty() {
                let style = if key.component.is_confirmed() {
                    AgentTerminalPresentationStyle::DiffContext
                } else {
                    AgentTerminalPresentationStyle::Status
                };
                sections.push((style, rendered));
            }
        }
        Ok(sections)
    }

    fn plain_action_progress_lines(display: String, display_width: usize) -> Vec<RichTextLine> {
        wrap_rich_text_line_to_width_with_source_ranges_hard(
            RichTextLine {
                display: sanitized_agent_terminal_line(&display),
                style_spans: Vec::new(),
                copy_text: None,
                kind: RichTextLineKind::Normal,
            },
            display_width,
        )
        .into_iter()
        .map(|wrapped| wrapped.line)
        .collect()
    }

    fn shell_output_progress_lines(
        display: String,
        display_width: usize,
        ui_theme: &mez_mux::theme::UiTheme,
    ) -> Vec<RichTextLine> {
        let display = sanitized_agent_terminal_line(&display);
        let length = agent_terminal_text_width(&display);
        wrap_rich_text_line_to_width_with_source_ranges_hard(
            RichTextLine {
                display,
                style_spans: vec![mez_terminal::TerminalStyleSpan {
                    start: 0,
                    length,
                    rendition: agent_terminal_label_rendition(
                        AgentTerminalPresentationStyle::Status,
                        ui_theme,
                    ),
                }],
                copy_text: None,
                kind: RichTextLineKind::Normal,
            },
            display_width,
        )
        .into_iter()
        .map(|wrapped| wrapped.line)
        .collect()
    }

    fn append_action_progress_sections_to_screen(
        screen: &mut TerminalScreen,
        sections: &[RenderedProgressSection],
        ui_theme: &mez_mux::theme::UiTheme,
    ) -> Result<(String, usize)> {
        let mut bytes = String::new();
        let mut physical_rows = 0usize;
        for (style, lines) in sections {
            if lines.is_empty() {
                continue;
            }
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
            for line in lines {
                physical_rows = physical_rows.saturating_add(1);
                append_styled_agent_terminal_rendered_line(&mut bytes, *style, line, ui_theme);
                bytes.push_str("\x1b[0m\r\n");
            }
        }
        if !bytes.is_empty() {
            Self::feed_agent_terminal_screen(
                screen,
                bytes.as_bytes(),
                "projecting executor-owned action progress",
            )?;
        }
        Ok((bytes, physical_rows))
    }

    fn action_progress_pane_for_key(
        &self,
        key: &RuntimeActionPresentationProgressKey,
    ) -> Option<String> {
        self.presentation
            .action_presentation_progress
            .iter()
            .find(|(_pane_id, presentation)| {
                presentation.components.contains_key(key)
                    || presentation.promoted_components.contains_key(key)
            })
            .map(|(pane_id, _presentation)| pane_id.clone())
    }

    fn promote_action_progress_component(
        &mut self,
        pane_id: &str,
        key: &RuntimeActionPresentationProgressKey,
        progress: &ActionPresentationProgress,
    ) -> Result<bool> {
        let Some(mut presentation) = self
            .presentation
            .action_presentation_progress
            .remove(pane_id)
        else {
            return Ok(false);
        };
        if presentation.promoted_components.contains_key(key) {
            self.presentation
                .action_presentation_progress
                .insert(pane_id.to_string(), presentation);
            return Ok(false);
        }
        let current_lineage =
            self.agent_pane_screen_lineage(pane_id, &presentation.conversation_id);
        let current_context = self.action_progress_projection_context(pane_id)?;
        let Some(component) = presentation.components.get(key).cloned() else {
            self.presentation
                .action_presentation_progress
                .insert(pane_id.to_string(), presentation);
            return Ok(false);
        };
        if current_lineage != Some(presentation.installed_lineage)
            || component.revision != progress.revision
            || component.source != progress.source
            || component.source_truncated != progress.source_truncated
        {
            return Ok(false);
        }
        presentation.projected_context = current_context;
        let target_sections =
            self.render_one_action_progress_component(pane_id, key, &component, &presentation)?;
        presentation.components.remove(key);
        presentation
            .promoted_components
            .insert(key.clone(), component.source.clone());

        let old_lineage = presentation.installed_lineage;
        let preview_rows = self
            .presentation
            .agent_shell_output_previews
            .get(pane_id)
            .filter(|preview| {
                preview.conversation_id == presentation.conversation_id
                    && preview.installed_lineage == old_lineage
            })
            .map_or(0, |preview| preview.transient_rows);
        let Some(mut durable_base) = self.clear_installed_agent_transient_suffixes(
            pane_id,
            &presentation.conversation_id,
            old_lineage,
            preview_rows,
            presentation.transient_rows,
        ) else {
            return Ok(false);
        };
        let (ansi_text, _promoted_rows) = Self::append_action_progress_sections_to_screen(
            &mut durable_base,
            &target_sections,
            self.ui_theme(),
        )?;
        if let Some(streaming) = self
            .presentation
            .agent_streaming_say_presentations
            .get_mut(pane_id)
            .filter(|streaming| {
                streaming.conversation_id == presentation.conversation_id
                    && streaming.installed_lineage == old_lineage
            })
        {
            streaming.provider_screen = std::sync::Arc::new(durable_base.clone());
        }
        let mut progress_baseline = durable_base.clone();
        let preview_source = self
            .presentation
            .agent_shell_output_previews
            .get(pane_id)
            .filter(|preview| {
                preview.conversation_id == presentation.conversation_id
                    && preview.installed_lineage == old_lineage
            })
            .map(|preview| preview.previews.clone());
        if let Some(previews) = preview_source {
            let reprojected_preview_rows = Self::append_agent_shell_previews_to_screen(
                &mut progress_baseline,
                &previews,
                self.ui_theme(),
                self.terminal_shell_output_preview_lines(),
                self.presentation.settings.terminal_agent_wrap_column_cap,
            )?;
            if let Some(preview) = self
                .presentation
                .agent_shell_output_previews
                .get_mut(pane_id)
            {
                preview.baseline_screen = std::sync::Arc::new(durable_base.clone());
                preview.transient_rows = reprojected_preview_rows;
            }
        }
        presentation.baseline_screen = std::sync::Arc::new(progress_baseline.clone());
        let remaining = self.render_action_progress_components(pane_id, &presentation)?;
        let mut candidate = progress_baseline;
        let (_remaining_ansi, transient_rows) = Self::append_action_progress_sections_to_screen(
            &mut candidate,
            &remaining,
            self.ui_theme(),
        )?;
        let installed_lineage = self
            .update_agent_pane_screen_preserving_interaction(
                pane_id,
                &presentation.conversation_id,
                candidate,
            )
            .ok_or_else(|| {
                MezError::invalid_state("executor progress promotion conversation changed")
            })?;
        self.synchronize_action_progress_companion_lineage(
            pane_id,
            &presentation.conversation_id,
            old_lineage,
            installed_lineage,
        );
        presentation.installed_lineage = installed_lineage;
        presentation.transient_rows = transient_rows;
        self.presentation
            .action_presentation_progress
            .insert(pane_id.to_string(), presentation);
        if !target_sections.is_empty() {
            let flattened = target_sections
                .iter()
                .flat_map(|(style, lines)| lines.iter().map(move |line| (*style, line)))
                .collect::<Vec<_>>();
            let source_content_type = if key.component.is_confirmed() {
                "text/x-diff; charset=utf-8"
            } else {
                AGENT_PRESENTATION_STYLED_LINES_CONTENT_TYPE
            };
            self.persist_agent_presentation_entry(
                pane_id,
                flattened
                    .iter()
                    .map(|(style, _line)| style.persistence_name().to_string())
                    .collect(),
                flattened
                    .iter()
                    .map(|(_style, line)| line.display.clone())
                    .collect(),
                Vec::new(),
                ansi_text,
                Some((&component.source, source_content_type)),
            );
        }
        Ok(true)
    }

    fn render_one_action_progress_component(
        &self,
        pane_id: &str,
        key: &RuntimeActionPresentationProgressKey,
        component: &RuntimeActionPresentationProgressComponent,
        presentation: &RuntimeActionPresentationProgressPresentation,
    ) -> Result<Vec<RenderedProgressSection>> {
        let mut one = presentation.clone();
        one.components.clear();
        one.components.insert(key.clone(), component.clone());
        self.render_action_progress_components(pane_id, &one)
    }

    /// Removes the exact installed executor-progress suffix in place.
    ///
    /// The returned screen retains any viewport displacement already exposed
    /// by the transient rows. A stale lineage or suffix leaves the installed
    /// screen untouched and rejects cleanup.
    fn clear_installed_action_progress_suffix(
        &self,
        pane_id: &str,
        presentation: &RuntimeActionPresentationProgressPresentation,
    ) -> Option<TerminalScreen> {
        if self.agent_pane_screen_lineage(pane_id, &presentation.conversation_id)
            != Some(presentation.installed_lineage)
        {
            return None;
        }
        let mut screen = self.agent_pane_screen(pane_id)?.clone();
        if presentation.transient_rows == 0 {
            return Some(screen);
        }
        let suffix = screen.capture_transient_suffix(presentation.transient_rows, true)?;
        screen.clear_transient_suffix(suffix).then_some(screen)
    }

    fn rollback_action_progress_component(
        &mut self,
        pane_id: &str,
        key: &RuntimeActionPresentationProgressKey,
        progress: &ActionPresentationProgress,
    ) -> Result<bool> {
        let Some(mut presentation) = self
            .presentation
            .action_presentation_progress
            .remove(pane_id)
        else {
            return Ok(false);
        };
        let old_lineage = presentation.installed_lineage;
        if self.agent_pane_screen_lineage(pane_id, &presentation.conversation_id)
            != Some(old_lineage)
        {
            return Ok(false);
        }
        let Some(component) = presentation.components.get(key) else {
            self.presentation
                .action_presentation_progress
                .insert(pane_id.to_string(), presentation);
            return Ok(false);
        };
        if component.revision != progress.revision
            || component.source != progress.source
            || component.source_truncated != progress.source_truncated
        {
            self.presentation
                .action_presentation_progress
                .insert(pane_id.to_string(), presentation);
            return Ok(false);
        }
        let Some(mut candidate) =
            self.clear_installed_action_progress_suffix(pane_id, &presentation)
        else {
            return Ok(false);
        };
        presentation.components.remove(key);
        presentation.projected_context = self.action_progress_projection_context(pane_id)?;
        let remaining = self.render_action_progress_components(pane_id, &presentation)?;
        presentation.baseline_screen = std::sync::Arc::new(candidate.clone());
        let (_ansi_text, transient_rows) = Self::append_action_progress_sections_to_screen(
            &mut candidate,
            &remaining,
            self.ui_theme(),
        )?;
        let installed_lineage = self
            .update_agent_pane_screen_preserving_interaction(
                pane_id,
                &presentation.conversation_id,
                candidate,
            )
            .ok_or_else(|| {
                MezError::invalid_state("executor progress rollback conversation changed")
            })?;
        self.synchronize_action_progress_companion_lineage(
            pane_id,
            &presentation.conversation_id,
            old_lineage,
            installed_lineage,
        );
        if presentation.components.is_empty() && presentation.promoted_components.is_empty() {
            return Ok(true);
        }
        presentation.installed_lineage = installed_lineage;
        presentation.transient_rows = transient_rows;
        self.presentation
            .action_presentation_progress
            .insert(pane_id.to_string(), presentation);
        Ok(true)
    }

    fn retire_action_presentation_progress_matching(
        &mut self,
        mut matches: impl FnMut(&RuntimeActionPresentationProgressKey) -> bool,
    ) -> Result<usize> {
        let pane_ids = self
            .presentation
            .action_presentation_progress
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut retired = 0usize;
        for pane_id in pane_ids {
            let Some(mut presentation) = self
                .presentation
                .action_presentation_progress
                .remove(&pane_id)
            else {
                continue;
            };
            let component_keys = presentation
                .components
                .keys()
                .filter(|key| matches(key))
                .cloned()
                .collect::<Vec<_>>();
            let promoted_keys = presentation
                .promoted_components
                .keys()
                .filter(|key| matches(key))
                .cloned()
                .collect::<Vec<_>>();
            let removes_visible_components = !component_keys.is_empty();
            retired = retired
                .saturating_add(component_keys.len())
                .saturating_add(promoted_keys.len());
            if component_keys.is_empty() && promoted_keys.is_empty() {
                self.presentation
                    .action_presentation_progress
                    .insert(pane_id, presentation);
                continue;
            }
            let old_lineage = presentation.installed_lineage;
            for key in component_keys {
                presentation.components.remove(&key);
            }
            for key in promoted_keys {
                presentation.promoted_components.remove(&key);
            }
            if !removes_visible_components {
                if !presentation.components.is_empty()
                    || !presentation.promoted_components.is_empty()
                {
                    self.presentation
                        .action_presentation_progress
                        .insert(pane_id, presentation);
                }
                continue;
            }
            if self.agent_pane_screen_lineage(&pane_id, &presentation.conversation_id)
                != Some(old_lineage)
            {
                continue;
            }
            let Some(mut candidate) =
                self.clear_installed_action_progress_suffix(&pane_id, &presentation)
            else {
                continue;
            };
            presentation.projected_context = self.action_progress_projection_context(&pane_id)?;
            let remaining = self.render_action_progress_components(&pane_id, &presentation)?;
            presentation.baseline_screen = std::sync::Arc::new(candidate.clone());
            let (_ansi_text, transient_rows) = Self::append_action_progress_sections_to_screen(
                &mut candidate,
                &remaining,
                self.ui_theme(),
            )?;
            let installed_lineage = self
                .update_agent_pane_screen_preserving_interaction(
                    &pane_id,
                    &presentation.conversation_id,
                    candidate,
                )
                .ok_or_else(|| {
                    MezError::invalid_state("executor progress cleanup conversation changed")
                })?;
            self.synchronize_action_progress_companion_lineage(
                &pane_id,
                &presentation.conversation_id,
                old_lineage,
                installed_lineage,
            );
            if presentation.components.is_empty() && presentation.promoted_components.is_empty() {
                continue;
            }
            presentation.installed_lineage = installed_lineage;
            presentation.transient_rows = transient_rows;
            self.presentation
                .action_presentation_progress
                .insert(pane_id, presentation);
        }
        Ok(retired)
    }

    fn synchronize_action_progress_companion_lineage(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        old_lineage: u64,
        installed_lineage: u64,
    ) {
        if let Some(preview) = self
            .presentation
            .agent_shell_output_previews
            .get_mut(pane_id)
            .filter(|preview| {
                preview.conversation_id == conversation_id
                    && preview.installed_lineage == old_lineage
            })
        {
            preview.installed_lineage = installed_lineage;
        }
        if let Some(streaming) = self
            .presentation
            .agent_streaming_say_presentations
            .get_mut(pane_id)
            .filter(|streaming| {
                streaming.conversation_id == conversation_id
                    && streaming.installed_lineage == old_lineage
            })
        {
            streaming.installed_lineage = installed_lineage;
            if streaming.projected_lineage == Some(old_lineage) {
                streaming.projected_lineage = Some(installed_lineage);
            }
        }
    }
}
