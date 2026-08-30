//! Runtime service integration for pane-scoped external-editor sessions.
//!
//! Editors run as server-local subprocesses on dedicated PTYs. Pane shells are
//! never invoked or written, so editor execution cannot alter user history,
//! drafts, shell protocol state, or pane terminal contents.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use super::artifacts::create_external_editor_artifacts;
use super::command::resolve_external_editor_commands;
use super::durable::DurableExternalEditSettlement;
use super::recovery::{
    ExternalEditorRecoveryManifest, ExternalEditorRecoveryState, discard_recovery_artifacts,
    read_recovery_manifest, write_recovery_manifest,
};
use super::runner::{INTERNAL_EDITOR_ARGUMENT, external_editor_runner_manifest};
use super::session::{
    ExternalEditTarget, ExternalEditorCompletion, ExternalEditorPaneIdentity,
    ExternalEditorSession, ExternalEditorSessionStart,
};
use crate::error::{MezError, Result};
use crate::runtime::render::{
    ExternalPromptEditSettlement, default_runtime_agent_prompt_input,
    normalize_external_agent_prompt,
};
use crate::runtime::{
    AgentShellVisibility, PaneProcessEvent, PaneProcessInstance, PaneProcessIoEffect,
    PaneReadinessState, ProcessEvent, RenderInvalidationReason, RuntimeSessionService,
    RuntimeSideEffect, RuntimeTransition, Size, current_unix_millis, runtime_pane_by_id,
    runtime_random_marker_token,
};
use crate::ui::readline::ReadlineInputDecoder;
use mez_mux::process::{PaneProcess, spawn_argv_pty_process};
use mez_terminal::TerminalScreen;

impl RuntimeSessionService {
    /// Starts one blocking editor session as a server-local PTY subprocess.
    pub(crate) fn start_external_editor_session(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        target: ExternalEditTarget,
        original_content: String,
        initial_draft_content: String,
        apply_on_success: bool,
    ) -> Result<ExternalEditorSessionStart> {
        self.require_live()?;
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "external editing requires an attached primary client",
            ));
        }
        if self.external_editor.is_active(pane_id) {
            return Err(MezError::conflict(
                "pane already has an active external-editor session",
            ));
        }
        if self.pane_is_closing(pane_id) {
            return Err(MezError::conflict("pane is closing"));
        }
        let primary_pid = self
            .primary_pid_for_live_pane_process(pane_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "pane process not found",
                )
            })?;
        let pane_process_instance = self.adapter_owned_pane_process_instance(pane_id);
        let session_id = runtime_random_marker_token(&format!(
            "external-editor-session\0{}\0{pane_id}\0{}",
            self.session.id,
            current_unix_millis()
        ))?
        .as_str()
        .to_string();
        let completion_nonce = runtime_random_marker_token(&format!(
            "external-editor-completion\0{session_id}\0{pane_id}"
        ))?
        .as_str()
        .to_string();
        let marker = runtime_random_marker_token(&format!(
            "external-editor-transaction\0{session_id}\0{completion_nonce}"
        ))?;
        let marker_id = marker.as_str().to_string();
        let runtime_root = self
            .session
            .socket_path()
            .parent()
            .ok_or_else(|| MezError::invalid_state("runtime socket has no parent directory"))?;
        let planned_session_directory = runtime_root.join("editor-sessions").join(&session_id);
        let planned_draft_path = planned_session_directory.join("draft.md");
        let commands = resolve_external_editor_commands(
            self.external_editor_config(),
            self.pane_environment_path(pane_id).as_deref(),
            self.pane_current_working_directory(pane_id).as_deref(),
            &planned_draft_path,
        )?;
        let runner_manifest = external_editor_runner_manifest(&commands)?;
        let runner = std::env::current_exe().map_err(|error| {
            MezError::invalid_state(format!("failed to locate external-editor runner: {error}"))
        })?;
        let artifacts =
            create_external_editor_artifacts(runtime_root, &session_id, &initial_draft_content)?;
        let recovery_manifest = ExternalEditorRecoveryManifest::new(
            session_id.clone(),
            self.session.id.to_string(),
            pane_id.to_string(),
            target.clone(),
            &original_content,
        );
        if let Err(error) = write_recovery_manifest(&artifacts, &recovery_manifest) {
            let _ = fs::remove_dir_all(&artifacts.session_directory);
            return Err(error);
        }
        let manifest_path = artifacts.session_directory.join("runner.json");
        if let Err(error) = write_external_editor_runner_manifest(&manifest_path, &runner_manifest)
        {
            let _ = fs::remove_dir_all(&artifacts.session_directory);
            return Err(error);
        }
        let size = self.external_editor_terminal_size_for_client(primary_client_id)?;
        let process_instance = self
            .external_editor
            .allocate_process_instance(&session_id)?;
        let environment = self
            .pane_environment_path(pane_id)
            .map(|path| vec![("PATH".to_string(), path)])
            .unwrap_or_default()
            .into_iter()
            .chain(std::iter::once((
                "TERM".to_string(),
                self.terminal_term().to_string(),
            )))
            .collect::<Vec<_>>();
        let process = spawn_argv_pty_process(
            &runner,
            &[
                INTERNAL_EDITOR_ARGUMENT.to_string(),
                manifest_path.to_string_lossy().into_owned(),
            ],
            &environment,
            size,
            self.pane_current_working_directory(pane_id).as_deref(),
        )
        .map_err(|error| {
            let _ = fs::remove_dir_all(&artifacts.session_directory);
            MezError::invalid_state(format!("failed to launch external editor: {error}"))
        })?;
        let screen = TerminalScreen::new_with_history_config(size, 1, 1)?;
        let artifact_session_directory = artifacts.session_directory.clone();
        if let Err(error) = self.external_editor.start(ExternalEditorSession {
            session_id: session_id.clone(),
            completion_nonce: completion_nonce.clone(),
            marker: marker_id.clone(),
            initiating_client_id: primary_client_id.as_str().to_string(),
            pane_id: pane_id.to_string(),
            pane_identity: ExternalEditorPaneIdentity {
                primary_pid,
                generation: pane_process_instance
                    .as_ref()
                    .map(|instance| instance.generation),
            },
            target,
            original_content,
            apply_on_success,
            artifacts,
            commands,
            recovery_manifest,
            process_instance: process_instance.clone(),
        }) {
            let _ = fs::remove_dir_all(&artifact_session_directory);
            return Err(error);
        }
        if let Err(error) =
            self.external_editor
                .install_process(pane_id, &process_instance, process, screen)
        {
            let _ = self.external_editor.abort(pane_id);
            let _ = fs::remove_dir_all(&artifact_session_directory);
            return Err(error);
        }
        self.set_pane_readiness(pane_id, PaneReadinessState::Busy);
        Ok(ExternalEditorSessionStart {
            session_id,
            completion_nonce,
            marker: marker_id,
            pane_id: pane_id.to_string(),
        })
    }

    /// Reports whether a pane currently routes raw primary input to an editor.
    pub(crate) fn external_editor_session_is_active(&self, pane_id: &str) -> bool {
        self.external_editor.is_active(pane_id)
    }

    /// Returns the initiating primary client's complete terminal geometry.
    ///
    /// An active editor owns the whole attached terminal rather than the
    /// pane-content rectangle inside Mezzanine's frames. Detach settlement
    /// removes the lease before its client descriptor can become stale.
    pub(crate) fn external_editor_terminal_size(&self, pane_id: &str) -> Option<Size> {
        let initiating_client_id = &self.external_editor.active(pane_id)?.initiating_client_id;
        let terminal = self
            .session
            .clients()
            .iter()
            .find(|client| client.id.as_str() == initiating_client_id)
            .and_then(|client| client.terminal.as_ref())?;
        Size::new(terminal.columns, terminal.rows).ok()
    }

    fn external_editor_terminal_size_for_client(
        &self,
        client_id: &mez_core::ids::ClientId,
    ) -> Result<Size> {
        let terminal = self
            .session
            .clients()
            .iter()
            .find(|client| client.id == *client_id)
            .and_then(|client| client.terminal.as_ref())
            .ok_or_else(|| {
                MezError::invalid_state("external-editor client terminal is unavailable")
            })?;
        Ok(Size::new(terminal.columns, terminal.rows)?)
    }

    /// Returns the terminal state owned exclusively by an active editor.
    pub(crate) fn external_editor_screen(&self, pane_id: &str) -> Option<&TerminalScreen> {
        self.external_editor.screen(pane_id)
    }

    /// Returns mutable editor terminal state for focused runtime tests.
    #[cfg(test)]
    pub(crate) fn external_editor_screen_mut_for_tests(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut TerminalScreen> {
        self.external_editor.screen_mut(pane_id)
    }

    /// Returns the active editor's exact completion identities for tests.
    #[cfg(test)]
    pub(crate) fn external_editor_identities_for_tests(
        &self,
        pane_id: &str,
    ) -> Option<(String, String, String)> {
        self.external_editor.active(pane_id).map(|session| {
            (
                session.marker.clone(),
                session.session_id.clone(),
                session.completion_nonce.clone(),
            )
        })
    }

    /// Returns the active editor process identity for focused event tests.
    #[cfg(test)]
    pub(crate) fn external_editor_process_instance_for_tests(
        &self,
        pane_id: &str,
    ) -> Option<PaneProcessInstance> {
        self.external_editor.process_instance(pane_id)
    }

    /// Moves pending direct editor processes to the async PTY supervisor.
    pub(in crate::runtime) fn take_pending_external_editor_processes(
        &mut self,
        limit: usize,
    ) -> Vec<(PaneProcessInstance, PaneProcess)> {
        self.external_editor.take_pending_processes(limit)
    }

    /// Reports whether one async process identity is the active direct editor.
    pub(crate) fn external_editor_process_instance_is_current(
        &self,
        instance: &PaneProcessInstance,
    ) -> bool {
        self.external_editor.process_instance_is_current(instance)
    }

    /// Builds opaque input delivery for the dedicated editor PTY.
    pub(crate) fn deferred_external_editor_input_effect(
        &self,
        pane_id: &str,
        bytes: Vec<u8>,
    ) -> Result<RuntimeSideEffect> {
        let instance = self
            .external_editor
            .process_instance(pane_id)
            .ok_or_else(|| MezError::invalid_state("external-editor process is unavailable"))?;
        Ok(RuntimeSideEffect::PaneProcessIo {
            instance,
            effect: PaneProcessIoEffect::WriteInput { bytes },
        })
    }

    /// Writes editor input before async handoff, or queues it for the worker.
    pub(crate) fn write_external_editor_input(
        &mut self,
        pane_id: &str,
        bytes: &[u8],
    ) -> Result<()> {
        let instance = self
            .external_editor
            .process_instance(pane_id)
            .ok_or_else(|| MezError::invalid_state("external-editor process is unavailable"))?;
        if self
            .external_editor
            .write_pending_process_input(&instance, bytes)?
        {
            return Ok(());
        }
        self.persistence
            .queue_pane_input(RuntimeSideEffect::PaneProcessIo {
                instance,
                effect: PaneProcessIoEffect::WriteInput {
                    bytes: bytes.to_vec(),
                },
            });
        Ok(())
    }

    /// Synchronizes the dedicated editor PTY and screen to the owning client.
    pub(crate) fn sync_external_editor_size(&mut self, pane_id: &str) -> Result<()> {
        let Some(size) = self.external_editor_terminal_size(pane_id) else {
            return Ok(());
        };
        let Some(instance) = self.external_editor.process_instance(pane_id) else {
            return Ok(());
        };
        if !self
            .external_editor
            .resize_pending_process(&instance, size)?
        {
            self.persistence.queue_pane_resize(
                instance.pane_id.clone(),
                RuntimeSideEffect::PaneProcessIo {
                    instance,
                    effect: PaneProcessIoEffect::Resize { size },
                },
            );
        }
        if let Some(screen) = self.external_editor.screen_mut(pane_id) {
            screen.resize(size);
        }
        Ok(())
    }

    /// Applies one event from a server-local editor PTY worker.
    pub(crate) fn apply_external_editor_process_event(
        &mut self,
        instance: PaneProcessInstance,
        event: PaneProcessEvent,
    ) -> Result<RuntimeTransition> {
        let Some(pane_id) = self
            .external_editor
            .pane_for_process_instance(&instance)
            .map(str::to_string)
        else {
            return Ok(RuntimeTransition::default());
        };
        match event {
            PaneProcessEvent::Pane(crate::runtime::PaneEvent::Output { bytes, .. }) => {
                let screen = self.external_editor.screen_mut(&pane_id).ok_or_else(|| {
                    MezError::invalid_state("external-editor screen is unavailable")
                })?;
                screen.feed(&bytes);
                let terminal_response_bytes = screen.drain_terminal_response_bytes();
                let mut side_effects = Vec::new();
                if !terminal_response_bytes.is_empty() {
                    side_effects.push(RuntimeSideEffect::PaneProcessIo {
                        instance,
                        effect: PaneProcessIoEffect::WriteInputPriority {
                            bytes: terminal_response_bytes,
                        },
                    });
                }
                side_effects.extend(self.render_effects_for_clients_projecting_pane(
                    &pane_id,
                    RenderInvalidationReason::PaneOutput,
                ));
                Ok(RuntimeTransition {
                    applied: true,
                    side_effects,
                })
            }
            PaneProcessEvent::Pane(crate::runtime::PaneEvent::Resized { size, .. }) => {
                if let Some(screen) = self.external_editor.screen_mut(&pane_id) {
                    screen.resize(size);
                }
                Ok(RuntimeTransition::default())
            }
            PaneProcessEvent::Pane(crate::runtime::PaneEvent::WriteFailed { error, .. })
            | PaneProcessEvent::Process(ProcessEvent::Failed { error, .. }) => {
                self.abort_external_editor_session(&pane_id)?;
                self.show_primary_error_overlay(vec![format!(
                    "mez error: external editor failed: {error}"
                )])?;
                Ok(RuntimeTransition {
                    applied: true,
                    side_effects: self.render_effects_for_clients_projecting_pane(
                        &pane_id,
                        RenderInvalidationReason::FullRedraw,
                    ),
                })
            }
            PaneProcessEvent::Process(ProcessEvent::Exited {
                exit_code, signal, ..
            }) => {
                let active = self.external_editor.active(&pane_id).ok_or_else(|| {
                    MezError::invalid_state("external-editor lease is unavailable")
                })?;
                let session_id = active.session_id.clone();
                let completion_nonce = active.completion_nonce.clone();
                let marker = active.marker.clone();
                let code = exit_code.unwrap_or_else(|| {
                    signal
                        .as_deref()
                        .and_then(|value| value.parse::<i32>().ok())
                        .map_or(1, |signal| 128_i32.saturating_add(signal))
                });
                let applied = self.complete_external_editor_session(
                    &pane_id,
                    &session_id,
                    &completion_nonce,
                    &marker,
                    code,
                )? > 0;
                Ok(RuntimeTransition {
                    applied,
                    side_effects: self.render_effects_for_clients_projecting_pane(
                        &pane_id,
                        RenderInvalidationReason::FullRedraw,
                    ),
                })
            }
            PaneProcessEvent::Pane(crate::runtime::PaneEvent::InputWritten { .. })
            | PaneProcessEvent::Pane(crate::runtime::PaneEvent::ForegroundProcess { .. })
            | PaneProcessEvent::Process(ProcessEvent::Spawned { .. })
            | PaneProcessEvent::ForegroundProcessObservation(_) => Ok(RuntimeTransition::default()),
        }
    }

    /// Restores normal pane geometry and invalidates every projection after takeover.
    ///
    /// This target-neutral boundary runs as soon as an editor lease is consumed,
    /// so prompt, issue, memory, and context-document editors all return from the
    /// full-terminal screen through the same resize and redraw lifecycle.
    fn restore_external_editor_terminal_presentation(&mut self, pane_id: &str) -> Result<()> {
        let Some(window_id) = self
            .find_pane_descriptor(pane_id)
            .map(|descriptor| descriptor.window_id.to_string())
        else {
            return Ok(());
        };
        self.sync_tracked_pty_sizes()?;
        let render_effects = self.render_effects_for_clients_projecting_windows(
            &[window_id],
            RenderInvalidationReason::FullRedraw,
        );
        self.presentation.defer_render_effects(render_effects);
        Ok(())
    }

    /// Arms one completion-time recovery persistence failure for integration tests.
    #[cfg(test)]
    pub(crate) fn fail_next_external_editor_completion_recovery_write_for_tests(&mut self) {
        self.external_editor
            .fail_next_completion_recovery_write_for_tests();
    }

    /// Persists completion recovery metadata through the fault-injectable boundary.
    fn write_external_editor_completion_recovery_manifest(
        &mut self,
        artifacts: &super::artifacts::ExternalEditorArtifacts,
        manifest: &ExternalEditorRecoveryManifest,
    ) -> Result<()> {
        #[cfg(test)]
        if self
            .external_editor
            .take_completion_recovery_write_failure_for_tests()
        {
            return Err(MezError::invalid_state(
                "injected external-editor completion recovery write failure",
            ));
        }
        write_recovery_manifest(artifacts, manifest)
    }

    /// Restores usable pane state after completion metadata cannot be persisted.
    ///
    /// The initial interrupted manifest remains authoritative on disk. The
    /// consumed lease is therefore converted into an explicit recovery record,
    /// while prompt ownership, readiness, and completion bookkeeping are
    /// settled exactly once before the original persistence error is returned.
    fn settle_external_editor_completion_persistence_failure(
        &mut self,
        pane_id: &str,
        mut completion: ExternalEditorCompletion,
        recovery_manifest: ExternalEditorRecoveryManifest,
        artifacts: super::artifacts::ExternalEditorArtifacts,
    ) -> Result<()> {
        completion.validated_content = None;
        completion.recovery_state = Some(ExternalEditorRecoveryState::Interrupted);
        self.external_editor.update_completion(completion.clone());
        self.external_editor
            .retain_recovery(recovery_manifest.into_record(artifacts));
        self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
        let prompt_settlement = self.settle_agent_prompt_external_edit(&completion);
        let durable_settlement = self.settle_durable_external_edit(&completion);
        let _ = self.external_editor.take_completion(
            pane_id,
            &completion.session_id,
            &completion.completion_nonce,
        );
        prompt_settlement?;
        durable_settlement?;
        Ok(())
    }

    /// Aborts one active editor lease and settles all presentation ownership.
    ///
    /// The original interrupted recovery remains authoritative and the draft
    /// is never applied. Repeated calls are harmless after the first lease is
    /// consumed.
    pub(in crate::runtime) fn abort_external_editor_session(
        &mut self,
        pane_id: &str,
    ) -> Result<bool> {
        let Some(session) = self.external_editor.abort(pane_id) else {
            return Ok(false);
        };
        let pending = self
            .external_editor
            .process_is_pending(&session.process_instance);
        if !pending {
            self.persistence.queue_pane_termination(
                session.process_instance.pane_id.clone(),
                RuntimeSideEffect::PaneProcessIo {
                    instance: session.process_instance.clone(),
                    effect: PaneProcessIoEffect::Terminate { force: false },
                },
            );
        }
        self.external_editor.remove_process_state(&session);
        let _ = write_recovery_manifest(&session.artifacts, &session.recovery_manifest);
        let completion = ExternalEditorCompletion {
            session_id: session.session_id.clone(),
            completion_nonce: session.completion_nonce.clone(),
            pane_id: session.pane_id.clone(),
            target: session.target.clone(),
            original_content: session.original_content.clone(),
            apply_on_success: session.apply_on_success,
            draft_path: session.artifacts.draft_path.clone(),
            exit_code: 130,
            validated_content: None,
            recovery_state: Some(ExternalEditorRecoveryState::Interrupted),
        };
        self.external_editor
            .retain_recovery(session.recovery_manifest.into_record(session.artifacts));
        self.settle_agent_prompt_external_edit_abort(&completion)?;
        if self.find_pane_descriptor(pane_id).is_some() {
            self.set_pane_readiness(pane_id, PaneReadinessState::PromptCandidate);
            self.restore_external_editor_terminal_presentation(pane_id)?;
        }
        Ok(true)
    }

    /// Aborts every editor lease owned by one detaching primary client.
    ///
    /// The original interrupted manifest remains authoritative. Prompt
    /// snapshots and shell-transaction identities are removed before the
    /// client's presentation state disappears, so late editor completion is
    /// harmless and a replacement primary can explicitly recover the draft.
    pub(in crate::runtime) fn abort_external_editor_sessions_for_client_detach(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
    ) -> Result<usize> {
        let targets = self
            .external_editor
            .active_targets_for_client(primary_client_id.as_str());
        let mut aborted = 0usize;
        for (pane_id, _marker) in targets {
            if self.abort_external_editor_session(&pane_id)? {
                aborted = aborted.saturating_add(1);
            }
        }
        Ok(aborted)
    }

    /// Settles one direct editor process only for its exact retained identities.
    pub(in crate::runtime) fn complete_external_editor_session(
        &mut self,
        pane_id: &str,
        session_id: &str,
        completion_nonce: &str,
        marker: &str,
        exit_code: i32,
    ) -> Result<usize> {
        let Some(active) = self.external_editor.active(pane_id) else {
            return Ok(0);
        };
        let current_primary_pid = self.primary_pid_for_live_pane_process(pane_id);
        let current_generation = self
            .adapter_owned_pane_process_instance(pane_id)
            .map(|instance| instance.generation);
        if current_primary_pid != Some(active.pane_identity.primary_pid)
            || current_generation != active.pane_identity.generation
        {
            self.abort_external_editor_session(pane_id)?;
            self.set_pane_readiness(pane_id, PaneReadinessState::Degraded);
            return Ok(0);
        }
        let active_manifest = active.recovery_manifest.clone();
        let active_artifacts = active.artifacts.clone();
        let active_session = active.clone();
        let Some(mut completion) =
            self.external_editor
                .complete(pane_id, session_id, completion_nonce, marker, exit_code)
        else {
            return Ok(0);
        };
        self.external_editor.remove_process_state(&active_session);
        self.restore_external_editor_terminal_presentation(pane_id)?;
        let mut recovery_manifest = active_manifest.clone();
        let validation = super::artifacts::validate_external_editor_draft(
            &active_artifacts,
            super::recovery::RECOVERY_DRAFT_MAX_BYTES,
            super::recovery::RECOVERY_DRAFT_MAX_LINES,
        );
        match validation {
            Ok(draft) => {
                let changed = recovery_manifest.content_changed(&draft.content);
                completion.validated_content = Some(draft.content);
                completion.recovery_state =
                    match (exit_code == 0, changed || !completion.apply_on_success) {
                        (true, true) => Some(ExternalEditorRecoveryState::ChangedUnapplied),
                        (false, true) => Some(ExternalEditorRecoveryState::NonzeroExit),
                        _ => None,
                    };
            }
            Err(_) => {
                completion.recovery_state = Some(ExternalEditorRecoveryState::Invalid);
            }
        }
        if let Some(state) = completion.recovery_state {
            recovery_manifest.set_state(state, Some(exit_code));
            if let Err(error) = self.write_external_editor_completion_recovery_manifest(
                &active_artifacts,
                &recovery_manifest,
            ) {
                self.settle_external_editor_completion_persistence_failure(
                    pane_id,
                    completion,
                    active_manifest,
                    active_artifacts,
                )?;
                return Err(error);
            }
            self.external_editor.retain_recovery(
                recovery_manifest
                    .clone()
                    .into_record(active_artifacts.clone()),
            );
        }
        self.external_editor.update_completion(completion.clone());
        self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
        let prompt_settlement = self.settle_agent_prompt_external_edit(&completion)?;
        let durable_settlement = self.settle_durable_external_edit(&completion)?;
        if durable_settlement == DurableExternalEditSettlement::Conflicted {
            completion.recovery_state = Some(ExternalEditorRecoveryState::Conflicted);
            recovery_manifest.set_state(ExternalEditorRecoveryState::Conflicted, Some(exit_code));
            write_recovery_manifest(&active_artifacts, &recovery_manifest)?;
            self.external_editor.retain_recovery(
                recovery_manifest
                    .clone()
                    .into_record(active_artifacts.clone()),
            );
            self.external_editor.update_completion(completion.clone());
        }
        if prompt_settlement != ExternalPromptEditSettlement::Unhandled
            || durable_settlement != DurableExternalEditSettlement::Unhandled
        {
            let _ = self.external_editor.take_completion(
                pane_id,
                &completion.session_id,
                &completion.completion_nonce,
            );
        }
        let changed_prompt_applied = prompt_settlement == ExternalPromptEditSettlement::Applied
            && completion.exit_code == 0
            && completion.validated_content.is_some()
            && completion.recovery_state == Some(ExternalEditorRecoveryState::ChangedUnapplied);
        let changed_durable_applied = durable_settlement == DurableExternalEditSettlement::Applied;
        if completion.recovery_state.is_none() || changed_prompt_applied || changed_durable_applied
        {
            let record = recovery_manifest.into_record(active_artifacts);
            discard_recovery_artifacts(&record)?;
            self.external_editor.remove_recovery(&completion.session_id);
        }
        Ok(usize::from(completion.session_id == session_id))
    }

    /// Reports whether the initiating client owns the active pane edit lease.
    pub(crate) fn external_editor_session_owned_by(
        &self,
        pane_id: &str,
        client_id: &mez_core::ids::ClientId,
    ) -> bool {
        self.external_editor
            .active(pane_id)
            .is_some_and(|session| session.initiating_client_id == client_id.as_str())
    }

    /// Takes one completed editor result for target-specific application.
    pub(crate) fn take_external_editor_completion(
        &mut self,
        pane_id: &str,
        session_id: &str,
        completion_nonce: &str,
    ) -> Option<ExternalEditorCompletion> {
        self.external_editor
            .take_completion(pane_id, session_id, completion_nonce)
    }

    /// Lists retained recovery metadata for an attached primary without exposing content.
    pub(crate) fn list_external_editor_recoveries(
        &self,
        primary_client_id: &mez_core::ids::ClientId,
    ) -> Result<String> {
        self.require_live()?;
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "external-editor recovery requires an attached primary client",
            ));
        }
        let records = self.external_editor.recoveries();
        if records.is_empty() {
            return Ok("No retained external-editor recoveries.".to_string());
        }
        let mut display =
            String::from("| Recovery | Pane | Target | State | Exit |\n|---|---|---|---|---:|\n");
        for record in records {
            display.push_str(&format!(
                "| `{}` | `{}` | `{}` | `{}` | {} |\n",
                record.session_id,
                record.pane_id,
                record.target.as_str(),
                record.state.as_str(),
                record
                    .exit_code
                    .map_or_else(|| "-".to_string(), |code| code.to_string())
            ));
        }
        Ok(display)
    }

    /// Applies one validated prompt recovery only to its exact pane and primary projection.
    pub(crate) fn apply_external_editor_recovery(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        session_id: &str,
    ) -> Result<()> {
        self.require_live()?;
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "external-editor recovery requires an attached primary client",
            ));
        }
        runtime_pane_by_id(&self.session, pane_id)?;
        let (record, mut manifest) =
            self.revalidated_external_editor_recovery(pane_id, session_id)?;
        if !matches!(record.target, ExternalEditTarget::AgentPrompt) {
            let draft = super::artifacts::validate_external_editor_draft(
                &record.artifacts,
                super::recovery::RECOVERY_DRAFT_MAX_BYTES,
                super::recovery::RECOVERY_DRAFT_MAX_LINES,
            )?;
            return match self.apply_durable_external_edit_target(
                pane_id,
                &record.target,
                &draft.content,
            )? {
                DurableExternalEditSettlement::Applied => {
                    discard_recovery_artifacts(&record)?;
                    self.external_editor.remove_recovery(session_id);
                    Ok(())
                }
                DurableExternalEditSettlement::Conflicted => {
                    manifest.set_state(ExternalEditorRecoveryState::Conflicted, record.exit_code);
                    write_recovery_manifest(&record.artifacts, &manifest)?;
                    self.external_editor
                        .retain_recovery(manifest.into_record(record.artifacts));
                    Err(MezError::conflict(
                        "durable external-editor recovery target changed or was deleted",
                    ))
                }
                DurableExternalEditSettlement::Retained
                | DurableExternalEditSettlement::Unhandled => Err(MezError::invalid_args(
                    "unsupported external-editor recovery target",
                )),
            };
        }
        let visible = self
            .agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.visibility == AgentShellVisibility::Visible);
        if !visible {
            return Err(MezError::conflict(
                "recovered agent prompt requires a visible agent shell",
            ));
        }
        if self.agent_shell_pane_has_active_turn(pane_id) {
            return Err(MezError::conflict(
                "recovered agent prompt cannot be applied while an agent turn is active",
            ));
        }
        let draft = super::artifacts::validate_external_editor_draft(
            &record.artifacts,
            super::recovery::RECOVERY_DRAFT_MAX_BYTES,
            super::recovery::RECOVERY_DRAFT_MAX_LINES,
        )?;
        let existing_prompt_input = self.agent_prompt_input_for_client(primary_client_id, pane_id);
        let mut prompt_input = existing_prompt_input
            .clone()
            .unwrap_or_else(default_runtime_agent_prompt_input);
        let current = mez_mux::readline::ReadlineBuffer::expanded_draft(
            &prompt_input.prompt.buffer.draft_snapshot(),
        );
        if existing_prompt_input.is_some() && !manifest.original_content_matches(&current) {
            manifest.set_state(ExternalEditorRecoveryState::Conflicted, record.exit_code);
            write_recovery_manifest(&record.artifacts, &manifest)?;
            self.external_editor
                .retain_recovery(manifest.into_record(record.artifacts));
            return Err(MezError::conflict(
                "current agent prompt changed since the retained editor session",
            ));
        }
        prompt_input.prompt.clear_transient_editing_state();
        prompt_input
            .prompt
            .buffer
            .set_line(normalize_external_agent_prompt(draft.content));
        prompt_input.decoder = ReadlineInputDecoder::new();
        prompt_input.pending_ctrl_c_exit_at_unix_ms = None;
        self.set_agent_prompt_input_for_client(primary_client_id, pane_id, prompt_input);
        self.sync_tracked_pty_sizes()?;
        let render_effects = self.render_effects_for_primary_projection(
            primary_client_id,
            RenderInvalidationReason::FullRedraw,
        );
        self.presentation.defer_render_effects(render_effects);
        discard_recovery_artifacts(&record)?;
        self.external_editor.remove_recovery(session_id);
        Ok(())
    }

    /// Explicitly discards one exact retained recovery; repeating a discard is harmless.
    pub(crate) fn discard_external_editor_recovery(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        session_id: &str,
    ) -> Result<bool> {
        self.require_live()?;
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "external-editor recovery requires an attached primary client",
            ));
        }
        let Some(_) = self.external_editor.recovery(session_id) else {
            return Ok(false);
        };
        let (record, _) = self.revalidated_external_editor_recovery(pane_id, session_id)?;
        discard_recovery_artifacts(&record)?;
        self.external_editor.remove_recovery(session_id);
        Ok(true)
    }

    /// Reopens one retained draft in a fresh editor session without applying it.
    pub(crate) fn reopen_external_editor_recovery(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        session_id: &str,
    ) -> Result<()> {
        self.require_live()?;
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "external-editor recovery requires an attached primary client",
            ));
        }
        runtime_pane_by_id(&self.session, pane_id)?;
        let (record, _) = self.revalidated_external_editor_recovery(pane_id, session_id)?;
        let draft = super::artifacts::validate_external_editor_draft(
            &record.artifacts,
            super::recovery::RECOVERY_DRAFT_MAX_BYTES,
            super::recovery::RECOVERY_DRAFT_MAX_LINES,
        )?;
        match &record.target {
            ExternalEditTarget::AgentPrompt => {
                self.reopen_agent_prompt_external_edit(primary_client_id, pane_id, draft.content)?;
            }
            ExternalEditTarget::IssueBody { .. }
            | ExternalEditTarget::IssueNotes { .. }
            | ExternalEditTarget::MemoryContent { .. }
            | ExternalEditTarget::ContextDocument { .. } => {
                self.reopen_durable_external_edit(
                    primary_client_id,
                    pane_id,
                    record.target.clone(),
                    draft.content,
                )?;
            }
        }
        discard_recovery_artifacts(&record)?;
        self.external_editor.remove_recovery(session_id);
        Ok(())
    }

    fn revalidated_external_editor_recovery(
        &self,
        pane_id: &str,
        session_id: &str,
    ) -> Result<(
        super::recovery::ExternalEditorRecoveryRecord,
        ExternalEditorRecoveryManifest,
    )> {
        let record = self
            .external_editor
            .recovery(session_id)
            .cloned()
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "external-editor recovery not found",
                )
            })?;
        if record.runtime_session_id != self.session.id.as_str() || record.pane_id != pane_id {
            return Err(MezError::forbidden(
                "external-editor recovery does not belong to this runtime pane",
            ));
        }
        let manifest = read_recovery_manifest(&record.artifacts)?;
        if manifest.session_id != record.session_id
            || manifest.runtime_session_id != record.runtime_session_id
            || manifest.pane_id != record.pane_id
            || manifest.target != record.target
            || manifest.state != record.state
            || manifest.exit_code != record.exit_code
        {
            return Err(MezError::conflict(
                "external-editor recovery metadata changed since discovery",
            ));
        }
        Ok((record, manifest))
    }
}

/// Creates the owner-only inert manifest consumed by the internal runner.
fn write_external_editor_runner_manifest(path: &Path, content: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o400);
    }
    let mut file = options.open(path).map_err(|error| {
        MezError::invalid_state(format!(
            "failed to create external-editor manifest: {error}"
        ))
    })?;
    file.write_all(content).map_err(|error| {
        MezError::invalid_state(format!("failed to write external-editor manifest: {error}"))
    })?;
    file.sync_all().map_err(|error| {
        MezError::invalid_state(format!("failed to sync external-editor manifest: {error}"))
    })
}
