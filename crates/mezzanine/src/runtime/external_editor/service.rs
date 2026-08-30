//! Runtime service integration for pane-scoped external-editor sessions.
//!
//! Launches reuse authenticated pane-shell transactions with typed argv and an
//! inherited terminal. The editor session owns target content and private
//! artifacts independently from shell-action results or model transcripts.

use std::fs;

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
    AgentShellVisibility, PaneReadinessState, RenderInvalidationReason,
    RunningShellTransactionKind, RunningShellTransactionRef, RuntimeSessionService, Size,
    current_unix_millis, runtime_pane_by_id, runtime_random_marker_token,
};
use crate::ui::readline::ReadlineInputDecoder;
use mez_agent::{
    ShellChildArgument, ShellChildLaunch, ShellLaunchArtifact, ShellLaunchArtifactId,
    ShellTransaction,
};

impl RuntimeSessionService {
    /// Renders editor-owned shell input, allowing Bash without a private
    /// receiver to use the ordinary POSIX wrapper only when no hidden
    /// parent-shell draft is tracked.
    pub(crate) fn render_external_editor_shell_input(
        &self,
        pane_id: &str,
        transaction: &ShellTransaction,
        classification: mez_agent::ShellClassification,
    ) -> mez_agent::ShellTransactionInput {
        let mut input = transaction.render_for_classification_input(classification);
        if input.is_empty()
            && classification == mez_agent::ShellClassification::Bash
            && !self.pane_has_unsubmitted_process_input(pane_id)
        {
            input = transaction
                .render_for_classification_input(mez_agent::ShellClassification::PosixSh);
        }
        input
    }

    /// Starts one blocking editor session through the focused pane shell.
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
        if self.managed_shell_handoff_is_pending(pane_id) {
            return Err(MezError::conflict(
                "pane has an active managed-shell handoff",
            ));
        }
        if self.pane_has_running_shell_transaction(pane_id) {
            return Err(MezError::conflict("pane has an active shell transaction"));
        }
        if self.pane_is_closing(pane_id) {
            return Err(MezError::conflict("pane is closing"));
        }
        self.require_pane_ready_for_agent_command(pane_id)?;
        let primary_pid = self
            .primary_pid_for_live_pane_process(pane_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "pane process not found",
                )
            })?;
        let process_instance = self.adapter_owned_pane_process_instance(pane_id);
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
        let shell_identity = self.shell_execution_identity_for_pane(pane_id)?;
        let manifest_id = ShellLaunchArtifactId::new("editor-manifest")?;
        let manifest = ShellLaunchArtifact::new(manifest_id.clone(), runner_manifest, 0o400)?;
        let runner = std::env::current_exe().map_err(|error| {
            MezError::invalid_state(format!("failed to locate external-editor runner: {error}"))
        })?;
        let runner = runner.to_str().ok_or_else(|| {
            MezError::invalid_state("external-editor runner path is not valid UTF-8")
        })?;
        let child_launch = ShellChildLaunch::new_with_artifacts(
            runner,
            vec![
                ShellChildArgument::Literal(INTERNAL_EDITOR_ARGUMENT.to_string()),
                ShellChildArgument::MaterializedArtifact(manifest_id),
            ],
            vec![manifest],
        )?
        .with_inherited_terminal();
        let transaction = self.configure_shell_transaction_for_pane(
            pane_id,
            ShellTransaction::new(
                marker,
                format!("external-editor-{session_id}"),
                "mez-ui",
                pane_id,
                shell_identity.shell_path(),
                "",
            )?
            .with_child_launch(child_launch),
        );
        let classification = shell_identity.classification();
        let transaction_input =
            self.render_external_editor_shell_input(pane_id, &transaction, classification);
        self.require_generated_shell_input(&transaction_input)?;
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
        let mut wrapper = transaction_input.wrapper;
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        let receiver_payload = (!transaction_input.receiver_payload.is_empty()).then(|| {
            mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                transaction_input.receiver_payload.into_bytes(),
                marker_id.clone(),
                true,
            )
        });
        let requires_payload_receiver_ready = shell_identity.classification()
            == mez_agent::ShellClassification::Fish
            && !transaction_input.payload.is_empty();
        let artifact_session_directory = artifacts.session_directory.clone();
        if let Err(error) = self.external_editor.start(ExternalEditorSession {
            session_id: session_id.clone(),
            completion_nonce: completion_nonce.clone(),
            marker: marker_id.clone(),
            initiating_client_id: primary_client_id.as_str().to_string(),
            pane_id: pane_id.to_string(),
            pane_identity: ExternalEditorPaneIdentity {
                primary_pid,
                generation: process_instance
                    .as_ref()
                    .map(|instance| instance.generation),
            },
            target,
            original_content,
            apply_on_success,
            artifacts,
            commands,
            recovery_manifest,
        }) {
            let _ = fs::remove_dir_all(&artifact_session_directory);
            return Err(error);
        }
        self.set_pane_readiness(pane_id, PaneReadinessState::Busy);
        self.register_running_shell_transaction(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id: format!("external-editor-{session_id}"),
                kind: RunningShellTransactionKind::ExternalEditor {
                    session_id: session_id.clone(),
                    completion_nonce: completion_nonce.clone(),
                },
                pane_id: pane_id.to_string(),
                command: String::new(),
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: None,
                pending_input_payload: (!transaction_input.payload.is_empty()).then(|| {
                    mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                        transaction_input.payload.into_bytes(),
                        marker_id.clone(),
                        transaction_input.payload_receiver_acknowledgements,
                    )
                }),
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
            true,
        );
        if requires_payload_receiver_ready {
            self.require_shell_transaction_payload_receiver_ready(&marker_id);
        }
        if let Some(receiver_payload) = receiver_payload {
            self.register_shell_receiver_payload(&marker_id, receiver_payload);
        }
        if let Err(error) = self.write_runtime_pane_shell_input(pane_id, wrapper.as_bytes()) {
            self.remove_running_shell_transaction(&marker_id);
            self.clear_shell_transaction_protocol_state(&marker_id);
            let _ = self.abort_external_editor_session(pane_id);
            self.set_pane_readiness(pane_id, PaneReadinessState::Degraded);
            return Err(error);
        }
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
        self.remove_running_shell_transaction(&session.marker);
        self.clear_shell_transaction_protocol_state(&session.marker);
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

    /// Settles one editor transaction only for its exact retained identities.
    pub(in crate::runtime) fn observe_external_editor_transaction_end(
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
        let Some(mut completion) =
            self.external_editor
                .complete(pane_id, session_id, completion_nonce, marker, exit_code)
        else {
            return Ok(0);
        };
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
