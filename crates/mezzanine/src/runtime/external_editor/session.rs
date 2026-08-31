//! Pane-scoped external-editor session and completion records.

use std::collections::BTreeMap;

use super::artifacts::ExternalEditorArtifacts;
use super::command::ResolvedExternalEditorCommand;
use super::recovery::{
    ExternalEditorRecoveryManifest, ExternalEditorRecoveryRecord, ExternalEditorRecoveryState,
    discover_external_editor_recoveries,
};
use crate::error::{MezError, Result};
use crate::runtime::PaneProcessInstance;
use mez_mux::process::PaneProcess;
use mez_terminal::TerminalScreen;
use std::time::Duration;

/// Typed artifact being edited by one external-editor session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExternalEditTarget {
    /// Pane-local agent prompt text.
    AgentPrompt,
    /// Free-form issue body content.
    IssueBody {
        project: String,
        issue_id: String,
        expected_revision: String,
    },
    /// Free-form issue notes content.
    IssueNotes {
        project: String,
        issue_id: String,
        expected_revision: String,
    },
    /// Durable memory content.
    MemoryContent {
        memory_id: String,
        expected_revision: String,
    },
    /// One content field in the durable transcript owned by a pane.
    TranscriptEntry {
        conversation_id: String,
        sequence: u64,
        pane_id: String,
        expected_revision: String,
    },
    /// Persisted user-owned context document content.
    ContextDocument {
        document_id: String,
        project: Option<String>,
        expected_revision: String,
    },
}

impl ExternalEditTarget {
    /// Returns the bounded display label used by recovery listings.
    pub(super) const fn as_str(&self) -> &'static str {
        match self {
            Self::AgentPrompt => "agent_prompt",
            Self::IssueBody { .. } => "issue_body",
            Self::IssueNotes { .. } => "issue_notes",
            Self::MemoryContent { .. } => "memory_content",
            Self::TranscriptEntry { .. } => "transcript_entry",
            Self::ContextDocument { .. } => "context_document",
        }
    }
}

/// Process identity fenced when an editor session starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExternalEditorPaneIdentity {
    /// Pane root process id.
    pub(super) primary_pid: u32,
    /// Async pane-worker generation when adapter ownership is active.
    pub(super) generation: Option<u64>,
}

/// One active runtime-owned editor session.
#[derive(Debug, Clone)]
pub(super) struct ExternalEditorSession {
    pub(super) session_id: String,
    pub(super) completion_nonce: String,
    pub(super) marker: String,
    pub(super) initiating_client_id: String,
    pub(super) pane_id: String,
    pub(super) pane_identity: ExternalEditorPaneIdentity,
    pub(super) target: ExternalEditTarget,
    pub(super) original_content: String,
    pub(super) apply_on_success: bool,
    pub(super) artifacts: ExternalEditorArtifacts,
    pub(super) commands: Vec<ResolvedExternalEditorCommand>,
    pub(super) recovery_manifest: ExternalEditorRecoveryManifest,
    pub(super) process_instance: PaneProcessInstance,
}

/// Non-secret launch facts returned to target-specific UI code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalEditorSessionStart {
    /// Opaque editor-session identity.
    pub(crate) session_id: String,
    /// Completion nonce correlated through the shell transaction.
    pub(crate) completion_nonce: String,
    /// Shell transaction marker that owns pane input until completion.
    pub(crate) marker: String,
    /// Pane whose foreground PTY is leased to the editor.
    pub(crate) pane_id: String,
}

/// Completed editor result retained for target-specific validation/application.
#[derive(Debug, Clone)]
pub(crate) struct ExternalEditorCompletion {
    /// Opaque editor-session identity.
    pub(crate) session_id: String,
    /// Completion nonce accepted exactly once for this session.
    pub(crate) completion_nonce: String,
    /// Pane that hosted the blocking editor.
    pub(crate) pane_id: String,
    /// Typed target awaiting target-specific validation and application.
    pub(crate) target: ExternalEditTarget,
    /// Original target text retained for unchanged and rollback decisions.
    pub(crate) original_content: String,
    /// Whether a successful changed draft may be applied automatically.
    pub(crate) apply_on_success: bool,
    /// Private draft path retained until target-specific settlement.
    pub(crate) draft_path: std::path::PathBuf,
    /// Blocking editor process exit code.
    pub(crate) exit_code: i32,
    /// Valid UTF-8 content reopened through the hardened final-path validator.
    pub(crate) validated_content: Option<String>,
    /// Durable reason this artifact remains recoverable, when any.
    pub(crate) recovery_state: Option<ExternalEditorRecoveryState>,
}

/// Editor-session state owned independently from prompt or durable targets.
#[derive(Debug, Default)]
pub(crate) struct RuntimeExternalEditorComponent {
    pub(super) active_by_pane: BTreeMap<String, ExternalEditorSession>,
    pub(super) completed_by_pane: BTreeMap<String, ExternalEditorCompletion>,
    pub(super) recoveries_by_id: BTreeMap<String, ExternalEditorRecoveryRecord>,
    /// Direct editor PTYs awaiting handoff to the async process supervisor.
    pending_processes: BTreeMap<String, PaneProcess>,
    /// Terminal state rendered during full-client editor takeover.
    screens_by_pane: BTreeMap<String, TerminalScreen>,
    /// Next synthetic process generation assigned to a direct editor child.
    next_process_generation: u64,
    /// Test-only one-shot failure for completion-time recovery persistence.
    #[cfg(test)]
    fail_next_completion_recovery_write: bool,
}

impl RuntimeExternalEditorComponent {
    /// Allocates the process identity used by one server-local editor child.
    pub(super) fn allocate_process_instance(
        &mut self,
        session_id: &str,
    ) -> Result<PaneProcessInstance> {
        self.next_process_generation =
            self.next_process_generation.checked_add(1).ok_or_else(|| {
                MezError::invalid_state("external-editor process generation exhausted")
            })?;
        Ok(PaneProcessInstance {
            pane_id: format!("@external-editor:{session_id}"),
            generation: self.next_process_generation,
        })
    }

    /// Installs the direct editor PTY and its independent terminal screen.
    pub(super) fn install_process(
        &mut self,
        pane_id: &str,
        instance: &PaneProcessInstance,
        process: PaneProcess,
        screen: TerminalScreen,
    ) -> Result<()> {
        if self.pending_processes.contains_key(&instance.pane_id)
            || self.screens_by_pane.contains_key(pane_id)
        {
            return Err(MezError::conflict(
                "external-editor process state already exists",
            ));
        }
        self.pending_processes
            .insert(instance.pane_id.clone(), process);
        self.screens_by_pane.insert(pane_id.to_string(), screen);
        Ok(())
    }

    /// Moves pending direct editor PTYs to the async process supervisor.
    pub(super) fn take_pending_processes(
        &mut self,
        limit: usize,
    ) -> Vec<(PaneProcessInstance, PaneProcess)> {
        let keys = self
            .pending_processes
            .keys()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|key| {
                let process = self.pending_processes.remove(&key)?;
                let instance = self
                    .active_by_pane
                    .values()
                    .find(|session| session.process_instance.pane_id == key)?
                    .process_instance
                    .clone();
                Some((instance, process))
            })
            .collect()
    }

    /// Reports whether an event belongs to the current direct editor process.
    pub(super) fn process_instance_is_current(&self, instance: &PaneProcessInstance) -> bool {
        self.active_by_pane
            .values()
            .any(|session| session.process_instance == *instance)
    }

    /// Returns the pane lease owning one synthetic editor process.
    pub(super) fn pane_for_process_instance(&self, instance: &PaneProcessInstance) -> Option<&str> {
        self.active_by_pane
            .values()
            .find(|session| session.process_instance == *instance)
            .map(|session| session.pane_id.as_str())
    }

    /// Returns the direct process identity for one active pane editor.
    pub(super) fn process_instance(&self, pane_id: &str) -> Option<PaneProcessInstance> {
        self.active(pane_id)
            .map(|session| session.process_instance.clone())
    }

    /// Returns the independently modeled editor terminal screen.
    pub(super) fn screen(&self, pane_id: &str) -> Option<&TerminalScreen> {
        self.screens_by_pane.get(pane_id)
    }

    /// Returns mutable editor terminal state.
    pub(super) fn screen_mut(&mut self, pane_id: &str) -> Option<&mut TerminalScreen> {
        self.screens_by_pane.get_mut(pane_id)
    }

    /// Writes input directly while the process still awaits async handoff.
    pub(super) fn write_pending_process_input(
        &mut self,
        instance: &PaneProcessInstance,
        input: &[u8],
    ) -> Result<bool> {
        let Some(process) = self.pending_processes.get_mut(&instance.pane_id) else {
            return Ok(false);
        };
        process.write_input(input)?;
        Ok(true)
    }

    /// Resizes the direct PTY before its async worker claims ownership.
    pub(super) fn resize_pending_process(
        &self,
        instance: &PaneProcessInstance,
        size: crate::runtime::Size,
    ) -> Result<bool> {
        let Some(process) = self.pending_processes.get(&instance.pane_id) else {
            return Ok(false);
        };
        process.resize(size)?;
        Ok(true)
    }

    /// Removes process and terminal state after completion or abort.
    pub(super) fn remove_process_state(&mut self, session: &ExternalEditorSession) {
        if let Some(mut process) = self
            .pending_processes
            .remove(&session.process_instance.pane_id)
        {
            let _ = process.terminate(Duration::from_millis(100));
        }
        self.screens_by_pane.remove(&session.pane_id);
    }

    /// Reports whether the process still awaits async handoff.
    pub(super) fn process_is_pending(&self, instance: &PaneProcessInstance) -> bool {
        self.pending_processes.contains_key(&instance.pane_id)
    }
    /// Discovers retained recovery records for one exact runtime session.
    pub(crate) fn discover(
        runtime_root: &std::path::Path,
        runtime_session_id: &str,
    ) -> Result<Self> {
        let recoveries_by_id =
            discover_external_editor_recoveries(runtime_root, runtime_session_id)?
                .into_iter()
                .map(|record| (record.session_id.clone(), record))
                .collect();
        Ok(Self {
            recoveries_by_id,
            ..Self::default()
        })
    }

    /// Reports whether one pane currently leases its PTY to an editor.
    pub(super) fn is_active(&self, pane_id: &str) -> bool {
        self.active_by_pane.contains_key(pane_id)
    }

    /// Returns the active session retained for one pane.
    pub(super) fn active(&self, pane_id: &str) -> Option<&ExternalEditorSession> {
        self.active_by_pane.get(pane_id)
    }

    /// Returns exact pane and transaction identities owned by one primary client.
    pub(super) fn active_targets_for_client(&self, client_id: &str) -> Vec<(String, String)> {
        self.active_by_pane
            .values()
            .filter(|session| session.initiating_client_id == client_id)
            .map(|session| (session.pane_id.clone(), session.marker.clone()))
            .collect()
    }

    /// Installs one pane-scoped lease, rejecting duplicate ownership.
    pub(super) fn start(&mut self, session: ExternalEditorSession) -> Result<()> {
        if self.active_by_pane.contains_key(&session.pane_id) {
            return Err(MezError::conflict(
                "pane already has an active external-editor session",
            ));
        }
        self.completed_by_pane.remove(&session.pane_id);
        self.active_by_pane.insert(session.pane_id.clone(), session);
        Ok(())
    }

    /// Accepts one exactly matching completion and releases the pane lease.
    pub(super) fn complete(
        &mut self,
        pane_id: &str,
        session_id: &str,
        completion_nonce: &str,
        marker: &str,
        exit_code: i32,
    ) -> Option<ExternalEditorCompletion> {
        let matches = self.active_by_pane.get(pane_id).is_some_and(|session| {
            session.session_id == session_id
                && session.completion_nonce == completion_nonce
                && session.marker == marker
        });
        if !matches {
            return None;
        }
        let session = self.active_by_pane.remove(pane_id)?;
        let completion = ExternalEditorCompletion {
            session_id: session.session_id,
            completion_nonce: session.completion_nonce,
            pane_id: session.pane_id.clone(),
            target: session.target,
            original_content: session.original_content,
            apply_on_success: session.apply_on_success,
            draft_path: session.artifacts.draft_path,
            exit_code,
            validated_content: None,
            recovery_state: None,
        };
        self.completed_by_pane
            .insert(session.pane_id, completion.clone());
        Some(completion)
    }

    /// Replaces one retained completion only when its fenced identities match.
    pub(super) fn update_completion(&mut self, completion: ExternalEditorCompletion) -> bool {
        let matches = self
            .completed_by_pane
            .get(&completion.pane_id)
            .is_some_and(|retained| {
                retained.session_id == completion.session_id
                    && retained.completion_nonce == completion.completion_nonce
            });
        if matches {
            self.completed_by_pane
                .insert(completion.pane_id.clone(), completion);
        }
        matches
    }

    /// Removes an active lease without fabricating a successful completion.
    pub(super) fn abort(&mut self, pane_id: &str) -> Option<ExternalEditorSession> {
        self.active_by_pane.remove(pane_id)
    }

    /// Arms one deterministic completion-manifest persistence failure.
    #[cfg(test)]
    pub(super) fn fail_next_completion_recovery_write_for_tests(&mut self) {
        self.fail_next_completion_recovery_write = true;
    }

    /// Consumes the deterministic completion-manifest persistence failure.
    #[cfg(test)]
    pub(super) fn take_completion_recovery_write_failure_for_tests(&mut self) -> bool {
        std::mem::take(&mut self.fail_next_completion_recovery_write)
    }

    /// Inserts or replaces one validated recovery record by opaque id.
    pub(super) fn retain_recovery(&mut self, record: ExternalEditorRecoveryRecord) {
        self.recoveries_by_id
            .insert(record.session_id.clone(), record);
    }

    /// Lists retained recovery records in stable identity order.
    pub(crate) fn recoveries(&self) -> Vec<ExternalEditorRecoveryRecord> {
        self.recoveries_by_id.values().cloned().collect()
    }

    /// Returns one retained recovery record by opaque id.
    pub(super) fn recovery(&self, session_id: &str) -> Option<&ExternalEditorRecoveryRecord> {
        self.recoveries_by_id.get(session_id)
    }

    /// Removes one retained recovery record by opaque id.
    pub(super) fn remove_recovery(
        &mut self,
        session_id: &str,
    ) -> Option<ExternalEditorRecoveryRecord> {
        self.recoveries_by_id.remove(session_id)
    }

    /// Takes one completion only when all supplied identities match.
    pub(super) fn take_completion(
        &mut self,
        pane_id: &str,
        session_id: &str,
        completion_nonce: &str,
    ) -> Option<ExternalEditorCompletion> {
        let matches = self
            .completed_by_pane
            .get(pane_id)
            .is_some_and(|completion| {
                completion.session_id == session_id
                    && completion.completion_nonce == completion_nonce
            });
        matches
            .then(|| self.completed_by_pane.remove(pane_id))
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn session() -> ExternalEditorSession {
        ExternalEditorSession {
            session_id: "session-a".to_string(),
            completion_nonce: "nonce-a".to_string(),
            marker: "marker-a".to_string(),
            initiating_client_id: "client-a".to_string(),
            pane_id: "%1".to_string(),
            pane_identity: ExternalEditorPaneIdentity {
                primary_pid: 42,
                generation: Some(7),
            },
            target: ExternalEditTarget::AgentPrompt,
            original_content: "before".to_string(),
            apply_on_success: true,
            artifacts: ExternalEditorArtifacts {
                session_directory: PathBuf::from("/private/session-a"),
                draft_path: PathBuf::from("/private/session-a/draft.md"),
            },
            commands: vec![ResolvedExternalEditorCommand {
                executable: "/usr/bin/editor".to_string(),
                arguments: vec!["/private/session-a/draft.md".to_string()],
            }],
            recovery_manifest: ExternalEditorRecoveryManifest::new(
                "session-a".to_string(),
                "runtime-session".to_string(),
                "%1".to_string(),
                ExternalEditTarget::AgentPrompt,
                "before",
            ),
            process_instance: PaneProcessInstance {
                pane_id: "@external-editor:session-a".to_string(),
                generation: 1,
            },
        }
    }

    /// Verifies one pane lease rejects duplicates and accepts completion only
    /// for the exact session, nonce, and transaction marker.
    #[test]
    fn pane_lease_and_completion_are_identity_fenced() {
        let mut component = RuntimeExternalEditorComponent::default();
        component.start(session()).unwrap();
        assert!(component.is_active("%1"));
        assert!(component.start(session()).is_err());
        assert!(
            component
                .complete("%1", "session-a", "stale", "marker-a", 0)
                .is_none()
        );
        assert!(component.is_active("%1"));

        let completion = component
            .complete("%1", "session-a", "nonce-a", "marker-a", 0)
            .unwrap();
        assert_eq!(completion.exit_code, 0);
        assert!(!component.is_active("%1"));
        assert!(
            component
                .complete("%1", "session-a", "nonce-a", "marker-a", 0)
                .is_none()
        );
        assert!(
            component
                .take_completion("%1", "session-a", "stale")
                .is_none()
        );
        assert!(
            component
                .take_completion("%1", "session-a", "nonce-a")
                .is_some()
        );
    }
}
