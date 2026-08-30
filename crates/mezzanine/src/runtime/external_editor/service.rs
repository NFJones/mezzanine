//! Runtime service integration for pane-scoped external-editor sessions.
//!
//! Launches reuse authenticated pane-shell transactions with typed argv and an
//! inherited terminal. The editor session owns target content and private
//! artifacts independently from shell-action results or model transcripts.

use std::fs;

use super::artifacts::create_external_editor_artifacts;
use super::command::resolve_external_editor_commands;
use super::runner::{INTERNAL_EDITOR_ARGUMENT, external_editor_runner_manifest};
use super::session::{
    ExternalEditTarget, ExternalEditorCompletion, ExternalEditorPaneIdentity,
    ExternalEditorSession, ExternalEditorSessionStart,
};
use crate::error::{MezError, Result};
use crate::runtime::{
    PaneReadinessState, RunningShellTransactionKind, RunningShellTransactionRef,
    RuntimeSessionService, current_unix_millis, runtime_random_marker_token,
};
use mez_agent::{
    ShellChildArgument, ShellChildLaunch, ShellLaunchArtifact, ShellLaunchArtifactId,
    ShellTransaction,
};

impl RuntimeSessionService {
    /// Starts one blocking editor session through the focused pane shell.
    pub(crate) fn start_external_editor_session(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        target: ExternalEditTarget,
        original_content: String,
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
        let artifacts =
            create_external_editor_artifacts(runtime_root, &session_id, &original_content)?;
        let commands = match resolve_external_editor_commands(
            self.external_editor_config(),
            self.pane_environment_path(pane_id).as_deref(),
            &artifacts.draft_path,
        ) {
            Ok(command) => command,
            Err(error) => {
                let _ = fs::remove_dir_all(&artifacts.session_directory);
                return Err(error);
            }
        };
        let shell_identity = self.shell_execution_identity_for_pane(pane_id)?;
        let manifest_id = ShellLaunchArtifactId::new("editor-manifest")?;
        let manifest = ShellLaunchArtifact::new(
            manifest_id.clone(),
            external_editor_runner_manifest(&commands)?,
            0o400,
        )?;
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
        let transaction_input =
            transaction.render_for_classification_input(shell_identity.classification());
        self.require_generated_shell_input(&transaction_input)?;
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
        self.external_editor.start(ExternalEditorSession {
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
            artifacts,
            commands,
        })?;
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
            if let Some(session) = self.external_editor.abort(pane_id) {
                let _ = fs::remove_dir_all(session.artifacts.session_directory);
            }
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

    /// Aborts one active editor lease while retaining its private artifacts.
    pub(in crate::runtime) fn abort_external_editor_session(&mut self, pane_id: &str) -> bool {
        self.external_editor.abort(pane_id).is_some()
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
            self.abort_external_editor_session(pane_id);
            self.set_pane_readiness(pane_id, PaneReadinessState::Degraded);
            return Ok(0);
        }
        let Some(completion) =
            self.external_editor
                .complete(pane_id, session_id, completion_nonce, marker, exit_code)
        else {
            return Ok(0);
        };
        self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
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
}
