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

type RenderedProgressSection = (
    AgentTerminalPresentationStyle,
    Vec<RichTextLine>,
    mez_core::presentation::PresentationEntry,
);

impl RuntimeSessionService {
    /// Routes native shell output to the same owner used by final settlement.
    ///
    /// Validate the executor identity before touching preview state. Shell tails
    /// must never also occupy the executor-progress layer: composing both layers
    /// at completion would permanently double their viewport displacement.
    pub(crate) fn apply_native_shell_output_progress(
        &mut self,
        progress: &ActionPresentationProgress,
    ) -> Result<bool> {
        let Some((pane_id, _action)) = self.action_progress_live_target(progress)? else {
            return Ok(false);
        };
        if progress.source.len() > ACTION_PRESENTATION_PROGRESS_MAX_SOURCE_BYTES {
            return Ok(false);
        }
        let ActionPresentationExecutionIdentity::Attempt(marker) = &progress.execution else {
            return Ok(false);
        };
        if self.agent_shell_view_enabled(&pane_id)
            || !self.agent_shell_transaction_action_shows_live_output(
                &progress.turn_id,
                &progress.action_id,
            )
        {
            return Ok(false);
        }
        self.ensure_current_agent_presentation_screen(&pane_id)?;
        let (conversation_id, _) = self.agent_presentation_target(&pane_id)?;
        let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
            turn_id: progress.turn_id.clone(),
            action_id: progress.action_id.clone(),
            marker: marker.clone(),
        };
        if let Some(preview) = self.presentation.agent_shell_output_previews.get(&pane_id)
            && (preview.conversation_id != conversation_id
                || self.agent_pane_screen_lineage(&pane_id, &conversation_id)
                    != Some(preview.installed_lineage)
                || preview.settled_owners.contains(&owner)
                || preview
                    .previews
                    .get(&owner)
                    .is_some_and(|source| progress.revision <= source.revision))
        {
            return Ok(false);
        }
        let lines = mez_agent::shell_observation::latest_agent_shell_transaction_output_lines(
            &progress.source,
            self.terminal_shell_output_preview_lines(),
        );
        let starts_at_entry = !progress.source_truncated
            && progress
                .source
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count()
                <= self.terminal_shell_output_preview_lines();
        self.update_agent_shell_output_preview_with_boundary(
            &pane_id,
            owner,
            progress.revision,
            &lines,
            starts_at_entry,
        )?;
        Ok(true)
    }

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
        let current_screen = self
            .agent_screen_without_pending_steering(&pane_id, &conversation_id)
            .ok_or_else(|| {
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
            .get(&pane_id)
            .cloned()
        {
            Some(presentation)
                if presentation.conversation_id == conversation_id
                    && presentation.installed_lineage == current_lineage =>
            {
                presentation
            }
            Some(_) => {
                self.defer_stale_action_progress_projection(&pane_id);
                return Ok(false);
            }
            None => RuntimeActionPresentationProgressPresentation {
                snapshot_current: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
                conversation_id: conversation_id.clone(),
                installed_lineage: current_lineage,
                baseline_screen: std::sync::Arc::new(current_screen),
                transient_rows: 0,
                projected_context: context.clone(),
                next_order: 0,
                components: std::collections::BTreeMap::new(),
                promoted_components: std::collections::BTreeMap::new(),
                retirement_requested: std::collections::BTreeSet::new(),
                retirement_error: None,
                retirement_retry_at: None,
                retirement_refused: false,
                replay_unavailable: None,
            },
        };
        let key = RuntimeActionPresentationProgressKey {
            turn_id: progress.turn_id,
            action_id: progress.action_id,
            execution: progress.execution,
            component: progress.component,
        };
        if presentation.retirement_requested.contains(&key)
            || presentation.promoted_components.contains_key(&key)
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
        let entry = presentation
            .components
            .get(&key)
            .map(|component| component.entry.clone())
            .unwrap_or_else(|| {
                self.presentation_entry(
                    &pane_id,
                    mez_core::presentation::PresentationOccurrenceBasis::LocalObservation,
                )
            });
        presentation.components.insert(
            key,
            RuntimeActionPresentationProgressComponent {
                first_seen_order,
                entry,
                revision: progress.revision,
                source: progress.source,
                source_truncated: progress.source_truncated,
            },
        );
        presentation.projected_context = context;
        let sections = self.render_action_progress_components(&pane_id, &presentation)?;
        if sections.is_empty() {
            self.presentation
                .commit_action_progress(&pane_id, Some(presentation));
            return Ok(true);
        }
        let mut candidate = presentation.baseline_screen.as_ref().clone();
        let (_ansi_text, transient_rows) = Self::append_action_progress_sections_to_screen(
            &mut candidate,
            &sections,
            self.ui_theme(),
        )?;
        #[cfg(test)]
        if self.agent_presentation_install_held_for_tests(&pane_id) {
            return Err(MezError::invalid_state(
                "injected held pre-install presentation failure",
            ));
        }
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
            .commit_action_progress(&pane_id, Some(presentation));
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
            .filter(|owner| !owner.retirement_requested.contains(&key))
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
            .filter(|owner| !owner.retirement_requested.contains(&key))
            .and_then(|presentation| presentation.promoted_components.get(&key))
            .is_some_and(|component| component.source == progress.source);
        if !exact_retained && !exact_promoted {
            return Ok(false);
        }
        if key.component.is_confirmed() {
            if exact_retained {
                // Promotion is the only proof that this exact confirmed body is
                // installed. A refused promotion leaves the body unproven, so it
                // cannot acknowledge the terminal result by itself.
                if !self.promote_confirmed_action_presentation_progress(progress)? {
                    return Ok(false);
                }
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
            if self.promote_action_progress_component(&pane_id, &key, progress)? {
                return Ok(true);
            }
            // Refused promotion never installed this exact source. Retain the
            // superseded provisional as bounded cleanup intent and let the
            // canonical RESULT publish instead of acknowledging absent content.
            self.retain_superseded_action_presentation_progress(&pane_id, &key);
            return Ok(false);
        }
        // The terminal body supersedes the retained provisional source. Record
        // that decision before any fallible rollback so replay cannot redraw the
        // superseded rows when physical cleanup still has to be retried.
        self.retain_superseded_action_presentation_progress(&pane_id, &key);
        if self.rollback_action_progress_component(&pane_id, &key, progress)? {
            self.discharge_superseded_action_presentation_progress(&pane_id, &key);
        }
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
        // A prior reconciliation may have promoted one component before a
        // later component failed. Its exact source still suppresses replay;
        // absence from the provisional map is not evidence it was unpublished.
        let mut suppress_final_replay = result.status == ActionStatus::Succeeded
            && !result.is_error
            && self
                .presentation
                .action_presentation_progress
                .values()
                .flat_map(|presentation| {
                    presentation
                        .promoted_components
                        .iter()
                        .filter(|(key, _)| !presentation.retirement_requested.contains(*key))
                })
                .any(|(key, component)| {
                    key.turn_id == turn_id
                        && key.action_id == action_id
                        && &key.execution == execution
                        && component.source == result.content_text()
                });
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
    ) -> crate::runtime::render::RuntimeActionProgressRetirement {
        self.retire_action_presentation_progress_matching(|key| {
            key.turn_id == turn_id && key.action_id == action_id
        })
    }

    /// Removes every transient or suppression record owned by one turn.
    pub(crate) fn retire_action_presentation_progress_for_turn(
        &mut self,
        turn_id: &str,
    ) -> crate::runtime::render::RuntimeActionProgressRetirement {
        self.retire_action_presentation_progress_matching(|key| key.turn_id == turn_id)
    }

    /// Returns immutable active/promoted provenance without rendering or clocks.
    #[cfg(test)]
    pub(crate) fn action_presentation_progress_entries_for_tests(
        &self,
        pane_id: &str,
    ) -> Vec<mez_core::presentation::PresentationEntry> {
        self.presentation
            .action_presentation_progress
            .get(pane_id)
            .map(|owner| {
                owner
                    .components
                    .values()
                    .chain(owner.promoted_components.values())
                    .map(|component| component.entry.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Reads retained revisions and payloads without advancing presentation.
    #[cfg(test)]
    pub(crate) fn action_presentation_progress_sources_for_tests(
        &self,
        pane_id: &str,
    ) -> Vec<(u64, String)> {
        self.presentation
            .action_presentation_progress
            .get(pane_id)
            .map(|owner| {
                owner
                    .components
                    .values()
                    .chain(owner.promoted_components.values())
                    .map(|component| (component.revision, component.source.clone()))
                    .collect()
            })
            .unwrap_or_default()
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
            .flat_map(|presentation| {
                presentation
                    .promoted_components
                    .iter()
                    .filter(|(key, _)| !presentation.retirement_requested.contains(*key))
            })
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
                        && source.source.as_str() == section.diff
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
            .flat_map(|presentation| {
                presentation
                    .promoted_components
                    .iter()
                    .filter(|(key, _)| !presentation.retirement_requested.contains(*key))
            })
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
                .all(|(_key, source)| output.contains(source.source.as_str()))
    }

    /// Prepares retained executor progress over a newly rebuilt lower baseline.
    ///
    /// Keep the current owner until installation succeeds: rendering failure
    /// cannot consume its immutable components or promoted settlement receipts.
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
            .get(pane_id)
            .cloned()
        else {
            return Ok((baseline, None));
        };
        if presentation.conversation_id != conversation_id
            || presentation.installed_lineage != expected_lineage
        {
            self.defer_stale_action_progress_projection(pane_id);
            return Ok((baseline, None));
        }
        if !presentation.retirement_requested.is_empty() {
            // The incoming candidate has already had durable content appended,
            // so its tail is not the original transient suffix and numerical
            // row counts are not transferable ownership proof. A durable write
            // therefore never discharges cleanup here: refuse to compose over
            // this unproven baseline and keep the bounded retirement obligation
            // owned by the existing retirement/replay owner.
            self.defer_stale_action_progress_projection(pane_id);
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
    ) -> Result<()> {
        if let Some(mut presentation) = presentation.take() {
            if !presentation.snapshot_is_current()
                || self
                    .presentation
                    .action_presentation_progress
                    .get(pane_id)
                    .is_none_or(|current| {
                        !std::sync::Arc::ptr_eq(
                            &current.snapshot_current,
                            &presentation.snapshot_current,
                        ) || current.installed_lineage != presentation.installed_lineage
                    })
                || self.agent_pane_screen_lineage(pane_id, &presentation.conversation_id)
                    != Some(installed_lineage)
            {
                return Err(MezError::invalid_state(
                    "composed progress source changed before commit",
                ));
            }
            presentation.installed_lineage = installed_lineage;
            presentation
                .components
                .retain(|key, _| !presentation.retirement_requested.contains(key));
            presentation
                .promoted_components
                .retain(|key, _| !presentation.retirement_requested.contains(key));
            presentation.retirement_requested.clear();
            presentation.retirement_error = None;
            let retained = (!presentation.components.is_empty()
                || !presentation.promoted_components.is_empty())
            .then_some(presentation);
            self.presentation.commit_action_progress(pane_id, retained);
        }
        Ok(())
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

    pub(super) fn action_progress_projection_context(
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

    pub(super) fn render_action_progress_components(
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
            if presentation.retirement_requested.contains(key) {
                continue;
            }
            let mut entry = component.entry.clone();
            let rendered = match &key.component {
                ActionPresentationComponentIdentity::ShellOutput
                    if !presentation.projected_context.shell_view
                        && self.agent_shell_transaction_action_shows_live_output(
                            &key.turn_id,
                            &key.action_id,
                        ) =>
                {
                    let max_rows = self.terminal_shell_output_preview_lines();
                    let source_rows = component
                        .source
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .count();
                    entry.first_row &= !component.source_truncated && source_rows <= max_rows;
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
                        entry.first_row = false;
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
                sections.push((style, rendered, entry));
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

    pub(super) fn append_action_progress_sections_to_screen(
        screen: &mut TerminalScreen,
        sections: &[RenderedProgressSection],
        ui_theme: &mez_mux::theme::UiTheme,
    ) -> Result<(String, usize)> {
        let mut bytes = String::new();
        let mut physical_rows = 0usize;
        let mut occurrence_ranges = Vec::new();
        for (style, lines, entry) in sections {
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
            let start = bytes.len();
            for line in lines {
                physical_rows = physical_rows.saturating_add(1);
                append_styled_agent_terminal_rendered_line(&mut bytes, *style, line, ui_theme);
                bytes.push_str("\x1b[0m\r\n");
            }
            occurrence_ranges.push((start..bytes.len(), entry.clone()));
        }
        if !bytes.is_empty() {
            Self::feed_presentation_ranges(
                screen,
                bytes.as_bytes(),
                &occurrence_ranges,
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
        // Compose from a snapshot while the original source/ack owner remains
        // installed. Any fallible pre-install step leaves that owner intact.
        let Some(mut presentation) = self
            .presentation
            .action_presentation_progress
            .get(pane_id)
            .cloned()
        else {
            return Ok(false);
        };
        if presentation.retirement_requested.contains(key)
            || presentation.promoted_components.contains_key(key)
        {
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
            .insert(key.clone(), component.clone());

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
        // Extend only the provider's immutable rollback prefix. `durable_base`
        // still includes mutable provider rows and cannot become that prefix.
        let streaming_baseline = self
            .presentation
            .agent_streaming_say_presentations
            .get(pane_id)
            .filter(|streaming| {
                !streaming.layout_retired
                    && streaming.conversation_id == presentation.conversation_id
                    && streaming.installed_lineage == old_lineage
            })
            .map(|streaming| streaming.baseline_screen.as_ref().clone());
        let streaming_baseline = if let Some(mut baseline) = streaming_baseline {
            Self::append_action_progress_sections_to_screen(
                &mut baseline,
                &target_sections,
                self.ui_theme(),
            )?;
            Some(std::sync::Arc::new(baseline))
        } else {
            None
        };
        let mut progress_baseline = durable_base.clone();
        let mut new_preview_rows = None;
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
            new_preview_rows = Some(reprojected_preview_rows);
        }
        presentation.baseline_screen = std::sync::Arc::new(progress_baseline.clone());
        let remaining = self.render_action_progress_components(pane_id, &presentation)?;
        let mut candidate = progress_baseline;
        let (_remaining_ansi, transient_rows) = Self::append_action_progress_sections_to_screen(
            &mut candidate,
            &remaining,
            self.ui_theme(),
        )?;
        #[cfg(test)]
        if self.agent_presentation_install_held_for_tests(pane_id) {
            return Err(MezError::invalid_state(
                "injected held pre-install presentation failure",
            ));
        }
        let installed_lineage = self
            .update_agent_pane_screen_preserving_interaction(
                pane_id,
                &presentation.conversation_id,
                candidate,
            )
            .ok_or_else(|| {
                MezError::invalid_state("executor progress promotion conversation changed")
            })?;
        if let Some(baseline) = streaming_baseline
            && let Some(streaming) = self
                .presentation
                .agent_streaming_say_presentations
                .get_mut(pane_id)
        {
            streaming.baseline_screen = baseline;
            streaming.provider_screen = std::sync::Arc::new(durable_base.clone());
        }
        if let Some(rows) = new_preview_rows
            && let Some(preview) = self
                .presentation
                .agent_shell_output_previews
                .get_mut(pane_id)
                .filter(|preview| {
                    preview.conversation_id == presentation.conversation_id
                        && preview.installed_lineage == old_lineage
                })
        {
            preview.baseline_screen = std::sync::Arc::new(durable_base);
            preview.transient_rows = rows;
        }
        self.synchronize_action_progress_companion_lineage(
            pane_id,
            &presentation.conversation_id,
            old_lineage,
            installed_lineage,
        );
        presentation.installed_lineage = installed_lineage;
        presentation.transient_rows = transient_rows;
        self.presentation
            .commit_action_progress(pane_id, Some(presentation));
        if !target_sections.is_empty() {
            let flattened = target_sections
                .iter()
                .flat_map(|(style, lines, _)| lines.iter().map(move |line| (*style, line)))
                .collect::<Vec<_>>();
            let source_content_type = if key.component.is_confirmed() {
                "text/x-diff; charset=utf-8"
            } else {
                AGENT_PRESENTATION_STYLED_LINES_CONTENT_TYPE
            };
            let styled_source = if key.component.is_confirmed() {
                None
            } else {
                Some(
                    serde_json::to_string(&vec![("status", &component.source)]).map_err(
                        |error| {
                            MezError::invalid_state(format!(
                                "progress source encoding failed: {error}"
                            ))
                        },
                    )?,
                )
            };
            let replay_source = styled_source.as_deref().unwrap_or(&component.source);
            let preview_source = if key.component.is_confirmed() {
                None
            } else {
                Some(
                    serde_json::to_string(
                        &flattened
                            .iter()
                            .map(|(style, line)| (style.persistence_name(), &line.display))
                            .collect::<Vec<_>>(),
                    )
                    .map_err(|error| {
                        MezError::invalid_state(format!(
                            "progress preview encoding failed: {error}"
                        ))
                    })?,
                )
            };
            // Confirmation is per mutation component, not whole-action success.
            // Promotion has already checked exact execution/source/lineage fences.
            let activity = {
                if let Some(execution) = self.agent_turn_executions().get(&key.turn_id)
                    && let Some(ordinal) =
                        execution.response.action_batch.as_ref().and_then(|batch| {
                            batch
                                .actions
                                .iter()
                                .position(|action| action.id == key.action_id)
                        })
                {
                    let kind = if key.component.is_confirmed() {
                        crate::storage::transcript::activity::ActivityComponentKind::ConfirmedMutation
                    } else {
                        crate::storage::transcript::activity::ActivityComponentKind::Result
                    };
                    self.activity_action_source(
                        pane_id,
                        execution,
                        ordinal,
                        kind,
                        (replay_source, source_content_type),
                    )?
                } else {
                    None
                }
            };
            let encoded = activity.and_then(|mut source| {
                source.status = if key.component.is_confirmed() {
                    "confirmed"
                } else {
                    "succeeded"
                }
                .to_string();
                source.preview_source = preview_source.clone();
                if let ActionPresentationComponentIdentity::ConfirmedMutation {
                    section_index,
                    path,
                } = &key.component
                {
                    source.mutation =
                        Some(crate::storage::transcript::activity::ActivityMutation {
                            section_index: *section_index,
                            path: path.clone(),
                        });
                }
                source.transaction = Some(match &key.execution {
                    ActionPresentationExecutionIdentity::Attempt(marker) => {
                        format!("attempt:{marker}")
                    }
                    ActionPresentationExecutionIdentity::Transaction(marker) => {
                        format!("transaction:{marker}")
                    }
                });
                source.encode().ok()
            });
            let entry = target_sections
                .first()
                .map(|section| section.2.clone())
                .unwrap_or_else(|| component.entry.clone());
            self.with_presentation_entry(pane_id, entry, |service| {
                service.persist_agent_presentation_entry(
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
                    Some(encoded.as_deref().map_or(
                        (
                            preview_source.as_deref().unwrap_or(replay_source),
                            source_content_type,
                        ),
                        |source| {
                            (
                                source,
                                crate::storage::transcript::activity::ACTIVITY_CONTENT_TYPE,
                            )
                        },
                    )),
                );
                Ok(())
            })?;
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
        let mut screen =
            self.agent_screen_without_pending_steering(pane_id, &presentation.conversation_id)?;
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
            .get(pane_id)
            .cloned()
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
        #[cfg(test)]
        if self.agent_presentation_install_held_for_tests(pane_id) {
            return Err(MezError::invalid_state(
                "injected held pre-install presentation failure",
            ));
        }
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
            self.presentation.commit_action_progress(pane_id, None);
            return Ok(true);
        }
        presentation.installed_lineage = installed_lineage;
        presentation.transient_rows = transient_rows;
        self.presentation
            .commit_action_progress(pane_id, Some(presentation));
        Ok(true)
    }

    /// Records the exact superseded source as cleanup intent before any
    /// fallible presentation work.
    ///
    /// The decision that a retained component no longer owns its screen rows is
    /// canonical before preparation, so replay, resize acceptance or an old
    /// captured snapshot cannot turn superseded provisional rows back into live
    /// progress merely because physical cleanup is still unproven. Recording the
    /// intent never repeats worker, hook, audit or effect work; the retained
    /// source is removed only after an actual successful install.
    fn retain_superseded_action_presentation_progress(
        &mut self,
        pane_id: &str,
        key: &RuntimeActionPresentationProgressKey,
    ) {
        let Some(owner) = self
            .presentation
            .action_presentation_progress
            .get_mut(pane_id)
        else {
            return;
        };
        if !owner.components.contains_key(key) && !owner.promoted_components.contains_key(key) {
            return;
        }
        if !owner.retirement_requested.contains(key) {
            owner.advance_snapshot_fence();
            owner.retirement_refused = false;
            owner.replay_unavailable = None;
        }
        owner.retirement_requested.insert(key.clone());
        owner.retirement_retry_at =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(250));
        self.defer_requested_action_progress_retirement(pane_id);
    }

    /// Clears cleanup intent only once the exact source is physically gone.
    fn discharge_superseded_action_presentation_progress(
        &mut self,
        pane_id: &str,
        key: &RuntimeActionPresentationProgressKey,
    ) {
        let Some(owner) = self
            .presentation
            .action_presentation_progress
            .get_mut(pane_id)
        else {
            return;
        };
        if owner.components.contains_key(key) || owner.promoted_components.contains_key(key) {
            return;
        }
        owner.retirement_requested.remove(key);
        if owner.retirement_requested.is_empty() {
            owner.retirement_error = None;
            owner.retirement_retry_at = None;
            owner.retirement_refused = false;
            owner.replay_unavailable = None;
        }
    }

    fn retire_action_presentation_progress_matching(
        &mut self,
        mut matches: impl FnMut(&RuntimeActionPresentationProgressKey) -> bool,
    ) -> crate::runtime::render::RuntimeActionProgressRetirement {
        let pane_ids = self
            .presentation
            .action_presentation_progress
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut report = crate::runtime::render::RuntimeActionProgressRetirement::default();
        for pane_id in pane_ids {
            let Some(presentation) = self
                .presentation
                .action_presentation_progress
                .get_mut(&pane_id)
            else {
                continue;
            };
            let selected = presentation
                .components
                .keys()
                .chain(presentation.promoted_components.keys())
                .filter(|key| matches(key))
                .cloned()
                .collect::<Vec<_>>();
            if selected.is_empty() {
                continue;
            }
            // Intent is canonical before any fallible paint. A clone or a
            // replacement screen is never an acknowledgment of this request.
            if selected
                .iter()
                .any(|key| !presentation.retirement_requested.contains(key))
            {
                presentation.advance_snapshot_fence();
                presentation.retirement_refused = false;
                presentation.replay_unavailable = None;
            }
            presentation
                .retirement_requested
                .extend(selected.iter().cloned());
            presentation.retirement_retry_at =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(250));
            match self.install_requested_action_progress_retirement(&pane_id) {
                Ok(retired) => report.retired = report.retired.saturating_add(retired),
                Err(error) => {
                    if let Some(owner) = self
                        .presentation
                        .action_presentation_progress
                        .get_mut(&pane_id)
                    {
                        owner.advance_snapshot_fence();
                        owner.retirement_error = Some(Box::new(error.clone()));
                    }
                    report.errors.push((pane_id.clone(), error));
                }
            }
            if let Some(owner) = self.presentation.action_presentation_progress.get(&pane_id) {
                report.retained = report.retained.saturating_add(
                    selected
                        .iter()
                        .filter(|key| owner.retirement_requested.contains(*key))
                        .count(),
                );
            }
            self.defer_requested_action_progress_retirement(&pane_id);
        }
        report
    }

    /// Attempts only the optional screen/source transaction of an existing
    /// request. Refusal retains its source and intent; no turn state is needed.
    fn install_requested_action_progress_retirement(&mut self, pane_id: &str) -> Result<usize> {
        let Some(mut presentation) = self
            .presentation
            .action_presentation_progress
            .get(pane_id)
            .cloned()
        else {
            return Ok(0);
        };
        let count = presentation.retirement_requested.len();
        if count == 0 {
            return Ok(0);
        }
        let visible = presentation
            .components
            .keys()
            .any(|key| presentation.retirement_requested.contains(key));
        let old_lineage = presentation.installed_lineage;
        if visible {
            let Some(mut candidate) =
                self.clear_installed_action_progress_suffix(pane_id, &presentation)
            else {
                if let Some(current) = self
                    .presentation
                    .action_presentation_progress
                    .get_mut(pane_id)
                {
                    current.retirement_refused = true;
                }
                return Ok(0);
            };
            presentation
                .components
                .retain(|key, _| !presentation.retirement_requested.contains(key));
            presentation
                .promoted_components
                .retain(|key, _| !presentation.retirement_requested.contains(key));
            presentation.projected_context = self.action_progress_projection_context(pane_id)?;
            let remaining = self.render_action_progress_components(pane_id, &presentation)?;
            presentation.baseline_screen = std::sync::Arc::new(candidate.clone());
            let (_ansi_text, transient_rows) = Self::append_action_progress_sections_to_screen(
                &mut candidate,
                &remaining,
                self.ui_theme(),
            )?;
            #[cfg(test)]
            if self.agent_presentation_install_held_for_tests(pane_id) {
                return Err(MezError::invalid_state(
                    "injected held pre-install presentation failure",
                ));
            }
            let installed_lineage = self
                .update_agent_pane_screen_preserving_interaction(
                    pane_id,
                    &presentation.conversation_id,
                    candidate,
                )
                .ok_or_else(|| {
                    MezError::invalid_state("executor progress cleanup conversation changed")
                })?;
            self.synchronize_action_progress_companion_lineage(
                pane_id,
                &presentation.conversation_id,
                old_lineage,
                installed_lineage,
            );
            presentation.installed_lineage = installed_lineage;
            presentation.transient_rows = transient_rows;
        } else {
            presentation
                .promoted_components
                .retain(|key, _| !presentation.retirement_requested.contains(key));
        }
        presentation.retirement_requested.clear();
        presentation.retirement_error = None;
        presentation.retirement_retry_at = None;
        presentation.retirement_refused = false;
        presentation.replay_unavailable = None;
        if presentation.components.is_empty() && presentation.promoted_components.is_empty() {
            self.presentation.commit_action_progress(pane_id, None);
        } else {
            self.presentation
                .commit_action_progress(pane_id, Some(presentation));
        }
        Ok(count)
    }

    /// Checks whether the existing canonical renderer can advance this refused
    /// source. Safely owned suffix cleanup does not require a replay backend.
    fn action_progress_rebuild_is_eligible(
        &self,
        pane: &str,
        owner: &RuntimeActionPresentationProgressPresentation,
    ) -> bool {
        !self.presentation_replay_is_unavailable(
            pane,
            &owner.conversation_id,
            owner.replay_unavailable,
        ) && self.persistence.transcript_store().is_some()
            && self.find_pane_descriptor(pane).is_some()
            && self.agent_shell_store().get(pane).is_some_and(|session| {
                session.session_id == owner.conversation_id
                    && session.visibility == AgentShellVisibility::Visible
                    && !session.ephemeral
            })
            && self
                .agent_pane_screen_lineage(pane, &owner.conversation_id)
                .is_some()
            && self
                .agent_pane_screen(pane)
                .is_some_and(|screen| !screen.normal_viewport_detached_from_history())
    }

    /// Keeps the idle timer enrolled for bound pending work, even before its
    /// next attempt deadline. Due checks belong only to the bounded attempt.
    pub(crate) fn action_progress_retirement_retry_needed(&self) -> bool {
        self.presentation
            .action_presentation_progress
            .iter()
            .any(|(pane, owner)| {
                !owner.retirement_requested.is_empty()
                    && (!owner.retirement_refused
                        || self.action_progress_rebuild_is_eligible(pane, owner))
                    && self
                        .agent_shell_store()
                        .get(pane)
                        .is_some_and(|session| session.session_id == owner.conversation_id)
                    && self
                        .agent_pane_screen_lineage(pane, &owner.conversation_id)
                        .is_some()
            })
    }

    /// Attempts at most eight existing presentation owners in fair pane order.
    /// This never re-enters worker delivery, canonical settlement or effects.
    pub(crate) fn retry_action_progress_retirements(&mut self) -> usize {
        let now = std::time::Instant::now();
        let mut panes = self
            .presentation
            .action_presentation_progress
            .iter()
            .filter(|(pane, owner)| {
                !owner.retirement_requested.is_empty()
                    && (!owner.retirement_refused
                        || self.action_progress_rebuild_is_eligible(pane, owner))
                    && owner.retirement_retry_at.is_none_or(|at| now >= at)
                    && self
                        .agent_shell_store()
                        .get(pane)
                        .is_some_and(|session| session.session_id == owner.conversation_id)
                    && self
                        .agent_pane_screen_lineage(pane, &owner.conversation_id)
                        .is_some()
            })
            .map(|(pane, _)| pane.clone())
            .collect::<Vec<_>>();
        if let Some(cursor) = &self.presentation.action_progress_retirement_cursor {
            let split = panes.partition_point(|pane| pane <= cursor);
            panes.rotate_left(split);
        }
        let mut retired = 0usize;
        for pane in panes.into_iter().take(8) {
            self.presentation.action_progress_retirement_cursor = Some(pane.clone());
            match self.install_requested_action_progress_retirement(&pane) {
                Ok(count) => retired = retired.saturating_add(count),
                Err(error) => {
                    if let Some(owner) = self
                        .presentation
                        .action_presentation_progress
                        .get_mut(&pane)
                    {
                        owner.advance_snapshot_fence();
                        owner.retirement_error = Some(Box::new(error));
                    }
                }
            }
            if let Some(owner) = self
                .presentation
                .action_presentation_progress
                .get_mut(&pane)
                && !owner.retirement_requested.is_empty()
            {
                owner.retirement_retry_at = Some(now + std::time::Duration::from_millis(250));
            }
            self.defer_requested_action_progress_retirement(&pane);
        }
        retired
    }

    /// Requests an actual canonical rebuild for a stale, still-bound source.
    /// Without a repository or a proven suffix, retain bounded source rather
    /// than repaint unrelated rows or fabricate cleanup acknowledgment.
    fn defer_stale_action_progress_projection(&mut self, pane_id: &str) {
        let Some(owner) = self.presentation.action_presentation_progress.get(pane_id) else {
            return;
        };
        if !self.action_progress_rebuild_is_eligible(pane_id, owner) {
            return;
        }
        if let Some(size) = self.agent_pane_screen(pane_id).map(TerminalScreen::size) {
            self.presentation
                .agent_presentation_projection_cache
                .remove(pane_id);
            self.presentation
                .defer_agent_presentation_resize(pane_id, size);
        }
    }

    /// Reuses the existing coalesced canonical replay worker, never the generic
    /// screen setter, to recover cleanup whose installed suffix is unproven.
    fn defer_requested_action_progress_retirement(&mut self, pane_id: &str) {
        let Some(owner) = self.presentation.action_presentation_progress.get(pane_id) else {
            return;
        };
        if owner.retirement_requested.is_empty() {
            return;
        }
        self.defer_stale_action_progress_projection(pane_id);
    }

    /// Advances only companion projections that shared the exact prior image.
    pub(super) fn synchronize_action_progress_companion_lineage(
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
