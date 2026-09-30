//! Captured durable presentation resize work and actor freshness acceptance.
//!
//! History decoding and canonical reprojection run on a worker using immutable
//! conversation, geometry, settings, source revision and screen-lineage inputs.
//! Acceptance remains on the existing runtime service and rejects stale work
//! before replacing any screen or restoring transient ownership.

use super::*;

impl RuntimeSessionService {
    /// Captures the newest eligible resize generation without reading durable history.
    pub(crate) fn take_agent_presentation_resize_work(
        &mut self,
        pane_id: &str,
    ) -> Result<Option<crate::runtime::RuntimeAgentPresentationResizeWork>> {
        if self.presentation.mouse_resize_drag_active() {
            return Ok(None);
        }
        let Some(size) = self
            .presentation
            .pending_agent_presentation_resize_sizes
            .get(pane_id)
            .copied()
        else {
            return Ok(None);
        };
        let Some(agent_session) = self.agent_shell_store().get(pane_id).cloned() else {
            self.presentation
                .pending_agent_presentation_resize_sizes
                .remove(pane_id);
            return Ok(None);
        };
        let conversation_id = agent_session.session_id.clone();
        let eligible = agent_session.visibility == AgentShellVisibility::Visible
            && !agent_session.ephemeral
            && self.agent_pane_screen(pane_id).is_some_and(|screen| {
                screen.size() == size && !screen.normal_viewport_detached_from_history()
            })
            && !self
                .presentation
                .agent_presentation_projection_cache
                .get(pane_id)
                .is_some_and(|(cached_conversation_id, cached_size)| {
                    cached_conversation_id == &conversation_id && *cached_size == size
                });
        let Some(transcript_store) = self.persistence.cloned_transcript_store() else {
            self.presentation
                .pending_agent_presentation_resize_sizes
                .remove(pane_id);
            return Ok(None);
        };
        if !eligible {
            self.presentation
                .pending_agent_presentation_resize_sizes
                .remove(pane_id);
            return Ok(None);
        }
        if self
            .persistence
            .presentation_write_pending(&conversation_id)
        {
            return Ok(None);
        }
        let captured_lineage = self
            .agent_pane_screen_lineage(pane_id, &conversation_id)
            .ok_or_else(|| {
                MezError::invalid_state("resized agent presentation lost its lineage")
            })?;
        let screen = self.agent_pane_screen(pane_id).cloned().ok_or_else(|| {
            MezError::invalid_state("resized agent presentation screen disappeared")
        })?;
        let source_revision = self
            .presentation
            .agent_presentation_source_revision(&conversation_id);
        let presentation_settings = self.presentation.settings.clone();
        let history_limit = self.terminal_history_limit();
        let history_rotate_lines = self.terminal_history_rotate_lines();
        let has_transient_projection = self
            .presentation
            .agent_shell_output_previews
            .contains_key(pane_id)
            || self
                .presentation
                .action_presentation_progress
                .contains_key(pane_id)
            || self
                .presentation
                .agent_streaming_say_presentations
                .contains_key(pane_id);
        let (cached_entries, cached_latest_sequence) = self
            .presentation
            .agent_presentation_replay_cache
            .decoded(&conversation_id, source_revision)
            .map_or((None, 0), |(entries, latest_sequence)| {
                (Some(entries), latest_sequence)
            });
        let cached_snapshot = if has_transient_projection || cached_entries.is_none() {
            None
        } else {
            let key = crate::runtime::render::RuntimeAgentPresentationSnapshotKey {
                conversation_id: conversation_id.clone(),
                source_revision,
                latest_sequence: cached_latest_sequence,
                size,
                presentation_settings: presentation_settings.clone(),
                history_limit,
                history_rotate_lines,
            };
            self.presentation
                .agent_presentation_replay_cache
                .snapshot(&key)
        };
        self.presentation
            .pending_agent_presentation_resize_sizes
            .remove(pane_id);
        Ok(Some(crate::runtime::RuntimeAgentPresentationResizeWork {
            pane_id: pane_id.to_string(),
            conversation_id,
            captured_lineage,
            source_revision,
            size,
            transcript_store,
            cached_entries,
            cached_latest_sequence,
            cached_snapshot,
            session: self.session().clone(),
            socket_path: self.session.socket_path().to_path_buf(),
            created_at_unix_seconds: self.session.created_at_unix_seconds(),
            agent_session,
            presentation_settings,
            history_limit,
            history_rotate_lines,
            screen,
            shell_output_previews: self
                .presentation
                .agent_shell_output_previews
                .get(pane_id)
                .cloned(),
            action_presentation_progress: self
                .presentation
                .action_presentation_progress
                .get(pane_id)
                .cloned(),
            streaming_say_presentation: self
                .presentation
                .agent_streaming_say_presentations
                .get(pane_id)
                .cloned(),
        }))
    }

    /// Builds one complete canonical resize candidate on a blocking worker.
    pub(crate) fn build_agent_presentation_resize(
        mut work: crate::runtime::RuntimeAgentPresentationResizeWork,
    ) -> Result<Option<crate::runtime::RuntimeAgentPresentationResizeResult>> {
        if work.cached_entries.is_some() {
            let durable_latest_sequence = work
                .transcript_store
                .next_presentation_sequence(&work.conversation_id)?
                .saturating_sub(1);
            if durable_latest_sequence != work.cached_latest_sequence {
                work.cached_entries = None;
                work.cached_snapshot = None;
                work.cached_latest_sequence = 0;
            }
        }
        if let Some(snapshot) = work.cached_snapshot.as_ref()
            && work.shell_output_previews.is_none()
            && work.action_presentation_progress.is_none()
            && work.streaming_say_presentation.is_none()
        {
            return Ok(Some(crate::runtime::RuntimeAgentPresentationResizeResult {
                pane_id: work.pane_id,
                conversation_id: work.conversation_id,
                captured_lineage: work.captured_lineage,
                source_revision: work.source_revision,
                latest_sequence: work.cached_latest_sequence,
                size: work.size,
                presentation_settings: work.presentation_settings,
                history_limit: work.history_limit,
                history_rotate_lines: work.history_rotate_lines,
                screen: snapshot.as_ref().clone(),
                decoded_entries: None,
                decoded_cache_hit: true,
                snapshot_cache_hit: true,
                replayed_entries: 0,
                cacheable_snapshot: true,
                shell_output_previews: None,
                action_presentation_progress: None,
                streaming_say_presentation: None,
            }));
        }
        let decoded_cache_hit = work.cached_entries.is_some();
        let entries = match work.cached_entries.as_ref() {
            Some(entries) => entries.clone(),
            None => std::sync::Arc::from(
                work.transcript_store
                    .inspect_presentation(&work.conversation_id)?,
            ),
        };
        let latest_sequence = entries
            .iter()
            .map(|entry| entry.sequence)
            .max()
            .unwrap_or_default();
        let decoded_entries = (!decoded_cache_hit).then(|| entries.clone());
        let cacheable_snapshot = work.shell_output_previews.is_none()
            && work.action_presentation_progress.is_none()
            && work.streaming_say_presentation.is_none();
        let mut projection = RuntimeSessionService::for_agent_presentation_projection(
            work.session,
            work.socket_path,
            work.created_at_unix_seconds,
            work.presentation_settings.clone(),
            work.history_limit,
            work.history_rotate_lines,
        )?;
        projection
            .agent_shell_store_mut()
            .restore_session(&work.pane_id, work.agent_session)?;
        projection
            .persistence
            .set_transcript_store(work.transcript_store);
        projection.set_agent_pane_screen(
            work.pane_id.clone(),
            work.conversation_id.clone(),
            work.screen,
        );
        let projection_lineage = projection
            .agent_pane_screen_lineage(&work.pane_id, &work.conversation_id)
            .ok_or_else(|| MezError::invalid_state("resize projection screen lost its lineage"))?;
        if let Some(mut preview) = work.shell_output_previews {
            preview.installed_lineage = projection_lineage;
            projection
                .presentation
                .agent_shell_output_previews
                .insert(work.pane_id.clone(), preview);
        }
        if let Some(mut progress) = work.action_presentation_progress {
            progress.installed_lineage = projection_lineage;
            projection
                .presentation
                .action_presentation_progress
                .insert(work.pane_id.clone(), progress);
        }
        if let Some(mut streaming) = work.streaming_say_presentation {
            streaming.installed_lineage = projection_lineage;
            projection
                .presentation
                .agent_streaming_say_presentations
                .insert(work.pane_id.clone(), streaming);
        }
        if !projection.rebuild_agent_presentation_after_resize_from_entries(
            &work.pane_id,
            work.size,
            &entries,
        )? {
            return Ok(None);
        }
        let screen = projection
            .agent_pane_screen(&work.pane_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("resize projection candidate disappeared"))?;
        Ok(Some(crate::runtime::RuntimeAgentPresentationResizeResult {
            pane_id: work.pane_id.clone(),
            conversation_id: work.conversation_id,
            captured_lineage: work.captured_lineage,
            source_revision: work.source_revision,
            latest_sequence,
            size: work.size,
            presentation_settings: work.presentation_settings,
            history_limit: work.history_limit,
            history_rotate_lines: work.history_rotate_lines,
            screen,
            decoded_entries,
            decoded_cache_hit,
            snapshot_cache_hit: false,
            replayed_entries: entries.len(),
            cacheable_snapshot,
            shell_output_previews: projection
                .presentation
                .agent_shell_output_previews
                .remove(&work.pane_id),
            action_presentation_progress: projection
                .presentation
                .action_presentation_progress
                .remove(&work.pane_id),
            streaming_say_presentation: projection
                .presentation
                .agent_streaming_say_presentations
                .remove(&work.pane_id),
        }))
    }

    /// Installs a worker candidate only while every captured input is current.
    pub(crate) fn apply_agent_presentation_resize_result(
        &mut self,
        mut result: crate::runtime::RuntimeAgentPresentationResizeResult,
    ) -> Result<bool> {
        let current_source_revision = self
            .presentation
            .agent_presentation_source_revision(&result.conversation_id);
        let current = self
            .agent_shell_store()
            .get(&result.pane_id)
            .is_some_and(|session| {
                session.session_id == result.conversation_id
                    && session.visibility == AgentShellVisibility::Visible
                    && !session.ephemeral
            })
            && self.agent_pane_screen_lineage(&result.pane_id, &result.conversation_id)
                == Some(result.captured_lineage)
            && self
                .agent_pane_screen(&result.pane_id)
                .is_some_and(|screen| {
                    screen.size() == result.size && !screen.normal_viewport_detached_from_history()
                })
            && self.presentation.settings == result.presentation_settings
            && self.terminal_history_limit() == result.history_limit
            && self.terminal_history_rotate_lines() == result.history_rotate_lines
            && current_source_revision == result.source_revision;
        if !current {
            let retry_size = self
                .agent_shell_store()
                .get(&result.pane_id)
                .filter(|session| {
                    session.visibility == AgentShellVisibility::Visible && !session.ephemeral
                })
                .and_then(|_session| self.agent_pane_screen(&result.pane_id))
                .filter(|screen| !screen.normal_viewport_detached_from_history())
                .map(TerminalScreen::size);
            if self.find_pane_descriptor(&result.pane_id).is_some()
                && self.persistence.transcript_store().is_some()
                && let Some(size) = retry_size
            {
                self.presentation
                    .defer_agent_presentation_resize(&result.pane_id, size);
            }
            return Ok(false);
        }
        let snapshot = result
            .cacheable_snapshot
            .then(|| std::sync::Arc::new(result.screen.clone()));
        let Some(installed_lineage) = self.update_agent_pane_screen_preserving_interaction(
            &result.pane_id,
            &result.conversation_id,
            result.screen,
        ) else {
            return Ok(false);
        };
        self.presentation
            .agent_shell_output_previews
            .remove(&result.pane_id);
        self.presentation
            .action_presentation_progress
            .remove(&result.pane_id);
        self.presentation
            .agent_streaming_say_presentations
            .remove(&result.pane_id);
        if let Some(mut preview) = result.shell_output_previews.take() {
            preview.installed_lineage = installed_lineage;
            self.presentation
                .agent_shell_output_previews
                .insert(result.pane_id.clone(), preview);
        }
        if let Some(mut progress) = result.action_presentation_progress.take() {
            progress.installed_lineage = installed_lineage;
            self.presentation
                .action_presentation_progress
                .insert(result.pane_id.clone(), progress);
        }
        if let Some(mut streaming) = result.streaming_say_presentation.take() {
            streaming.installed_lineage = installed_lineage;
            if streaming.projected_lineage.is_some() {
                streaming.projected_lineage = Some(installed_lineage);
            }
            self.presentation
                .agent_streaming_say_presentations
                .insert(result.pane_id.clone(), streaming);
        }
        self.presentation
            .agent_presentation_projection_cache
            .insert(
                result.pane_id.clone(),
                (result.conversation_id.clone(), result.size),
            );
        let mut evictions = 0u64;
        if let Some(entries) = result.decoded_entries.take() {
            evictions = evictions.saturating_add(
                self.presentation
                    .agent_presentation_replay_cache
                    .insert_decoded(
                        result.conversation_id.clone(),
                        result.source_revision,
                        result.latest_sequence,
                        entries,
                    ),
            );
        }
        if let Some(screen) = snapshot {
            evictions = evictions.saturating_add(
                self.presentation
                    .agent_presentation_replay_cache
                    .insert_snapshot(
                        crate::runtime::render::RuntimeAgentPresentationSnapshotKey {
                            conversation_id: result.conversation_id,
                            source_revision: result.source_revision,
                            latest_sequence: result.latest_sequence,
                            size: result.size,
                            presentation_settings: result.presentation_settings,
                            history_limit: result.history_limit,
                            history_rotate_lines: result.history_rotate_lines,
                        },
                        screen,
                    ),
            );
        }
        self.integration
            .runtime_metrics_mut()
            .record_agent_presentation_resize_cache(
                result.decoded_cache_hit,
                result.snapshot_cache_hit,
                result.replayed_entries,
                evictions,
            );
        Ok(true)
    }

    /// Returns bounded replay-cache occupancy for focused regression coverage.
    #[cfg(test)]
    pub(crate) fn agent_presentation_replay_cache_stats_for_tests(&self) -> (usize, usize, usize) {
        (
            self.presentation
                .agent_presentation_replay_cache
                .decoded
                .len(),
            self.presentation
                .agent_presentation_replay_cache
                .snapshots
                .len(),
            self.presentation
                .agent_presentation_replay_cache
                .estimated_bytes,
        )
    }
}
