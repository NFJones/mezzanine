//! Conversation-owned native working-directory intent and adoption generation.
//!
//! This state records an already validated runtime navigation choice without
//! modifying the daemon or pane process. It is not persisted, filesystem proof,
//! a trust decision or permission to launch work. Product code owns native-mode,
//! async-operation, busy-state and filesystem checks before adoption. Exact
//! session clones preserve intent for rollback; a different conversation cannot
//! inherit it merely by reusing a pane or cloning its session fields. Instruction
//! discovery is immutable data bound to the same adoption; late pane-root cache
//! writes cannot replace it and empty discovery is distinct from unknown data.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::instructions::DiscoveredInstructionFile;

use crate::{AgentShellSession, AgentShellSessionError, AgentShellSessionResult};

/// One runtime conversation's native navigation choice, fenced by generation.
#[derive(Clone, PartialEq, Eq)]
pub struct AgentNativeWorkingDirectory {
    /// Exact pane whose conversation accepted the choice.
    pane_id: String,
    /// Exact conversation that accepted the choice.
    conversation_id: String,
    /// Absolute representable directory supplied by the validating runtime.
    directory: PathBuf,
    /// Checked adoption generation within this exact conversation owner.
    generation: u64,
    /// Immutable discovery for this exact adoption; empty is a successful clear,
    /// while absence means no instruction snapshot was prepared.
    instruction_files: Option<Arc<[DiscoveredInstructionFile]>>,
}

impl std::fmt::Debug for AgentNativeWorkingDirectory {
    /// Includes owner/path diagnostics while redacting instruction bodies.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentNativeWorkingDirectory")
            .field("pane_id", &self.pane_id)
            .field("conversation_id", &self.conversation_id)
            .field("directory", &self.directory)
            .field("generation", &self.generation)
            .field(
                "instruction_file_count",
                &self.instruction_files.as_ref().map(|files| files.len()),
            )
            .finish()
    }
}

impl AgentNativeWorkingDirectory {
    /// Checks the original data owner; this grants no execution or filesystem rights.
    pub fn is_owned_by(&self, pane_id: &str, conversation_id: &str) -> bool {
        self.pane_id == pane_id && self.conversation_id == conversation_id
    }

    /// Returns the captured directory. Callers outside its session must first
    /// qualify the owner or an explicit controller-owned inherited work binding.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Returns immutable discovery, including a successfully discovered empty
    /// set. Absence is unknown discovery, never permission to reuse another cwd.
    pub fn instruction_files(&self) -> Option<&[DiscoveredInstructionFile]> {
        self.instruction_files.as_deref()
    }
}

impl AgentShellSession {
    /// Returns navigation intent only for this exact conversation. Absence
    /// leaves initial cwd inference to the runtime's existing native owner.
    pub fn native_working_directory(&self) -> Option<&Path> {
        self.native_directory_snapshot()
            .map(|state| state.directory.as_path())
    }

    /// Returns the current owned adoption generation, or zero before navigation.
    /// A copied intent from another conversation supplies no generation proof.
    pub fn native_working_directory_generation(&self) -> u64 {
        self.native_directory_snapshot()
            .map_or(0, |state| state.generation)
    }

    /// Records an already validated native navigation result for this owner.
    /// The runtime must first fence the exact pane/session, source cwd, mode,
    /// command operation and policy, and validate filesystem existence/access.
    /// This method performs no I/O. Relative/unrepresentable/control-bearing
    /// paths, a stale generation, an active turn or generation exhaustion fail
    /// without modifying the previous intent. Successful adoption returns its
    /// new generation; it does not change any process, grants or accounting.
    pub fn record_native_working_directory(
        &mut self,
        directory: PathBuf,
        expected_generation: u64,
    ) -> AgentShellSessionResult<u64> {
        self.record_native_directory_snapshot(directory, expected_generation, None)
    }

    /// Atomically adopts a directory and its already validated instruction
    /// discovery. The same runtime checks as `record_native_working_directory`
    /// are required. Empty discovery explicitly clears prior guidance; the
    /// immutable files share the directory's exact owner and generation. Errors
    /// leave both the previous cwd and instruction snapshot unchanged.
    pub fn record_native_working_directory_with_instructions(
        &mut self,
        directory: PathBuf,
        expected_generation: u64,
        files: Vec<DiscoveredInstructionFile>,
    ) -> AgentShellSessionResult<u64> {
        self.record_native_directory_snapshot(directory, expected_generation, Some(files.into()))
    }

    /// Performs one checked, I/O-free adoption without publishing partial data.
    fn record_native_directory_snapshot(
        &mut self,
        directory: PathBuf,
        expected_generation: u64,
        instruction_files: Option<Arc<[DiscoveredInstructionFile]>>,
    ) -> AgentShellSessionResult<u64> {
        if self.running_turn_id.is_some() {
            return Err(AgentShellSessionError::conflict(
                "cannot change native working directory while a turn is running",
            ));
        }
        let generation = self.native_working_directory_generation();
        if generation != expected_generation {
            return Err(AgentShellSessionError::conflict(
                "native working directory generation changed",
            ));
        }
        if !directory.is_absolute()
            || directory
                .to_str()
                .is_none_or(|value| value.chars().any(char::is_control))
        {
            return Err(AgentShellSessionError::invalid_state(
                "native working directory must be an absolute representable path without controls",
            ));
        }
        let generation = generation.checked_add(1).ok_or_else(|| {
            AgentShellSessionError::invalid_state("native working directory generation exhausted")
        })?;
        self.native_working_directory = Some(AgentNativeWorkingDirectory {
            pane_id: self.pane_id.clone(),
            conversation_id: self.session_id.clone(),
            directory,
            generation,
            instruction_files,
        });
        Ok(generation)
    }

    /// Captures the immutable directory/discovery snapshot only for this exact
    /// session owner. Runtime controllers may retain it across their own work
    /// bindings; copied history labels alone do not authorize inheritance.
    pub fn native_directory_snapshot(&self) -> Option<&AgentNativeWorkingDirectory> {
        self.native_working_directory
            .as_ref()
            .filter(|state| state.is_owned_by(&self.pane_id, &self.session_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentShellStore;

    /// Navigation belongs to the exact conversation and survives its ordinary
    /// session clone/rollback. Pane reuse or a copied different conversation
    /// cannot inherit intent, and an older validation cannot overwrite a newer
    /// adopted generation. No test needs filesystem or process operations.
    #[test]
    fn native_directory_intent_is_exact_owner_and_generation_scoped() {
        let mut store = AgentShellStore::default();
        let session = store.ensure_session("pane-a").unwrap();
        assert_eq!(session.native_working_directory(), None);
        assert_eq!(session.native_working_directory_generation(), 0);
        assert_eq!(
            session
                .record_native_working_directory("/first".into(), 0)
                .unwrap(),
            1
        );
        let retained = session.clone();
        assert!(
            session
                .record_native_working_directory("/stale".into(), 0)
                .is_err()
        );
        assert_eq!(session, &retained);
        assert_eq!(
            session
                .record_native_working_directory("/second".into(), 1)
                .unwrap(),
            2
        );
        let mut foreign_pane = session.clone();
        foreign_pane.pane_id = "pane-b".to_string();
        assert_eq!(foreign_pane.native_working_directory(), None);
        assert_eq!(foreign_pane.native_working_directory_generation(), 0);
        let mut foreign = session.clone();
        foreign.session_id = "another-conversation".to_string();
        assert_eq!(foreign.native_working_directory(), None);
        assert_eq!(foreign.native_working_directory_generation(), 0);
        assert_eq!(
            foreign
                .record_native_working_directory("/foreign".into(), 0)
                .unwrap(),
            1
        );
        assert_eq!(
            session.native_working_directory(),
            Some(Path::new("/second"))
        );
        assert_eq!(
            retained.native_working_directory(),
            Some(Path::new("/first"))
        );
    }

    /// Bad paths, a live-turn dependency and exhausted generation all leave
    /// intent byte-for-byte unchanged. This state guard supplements rather than
    /// substitutes the runtime's filesystem, policy and async-claim checks.
    #[test]
    fn native_directory_intent_rejects_invalid_or_busy_adoption() {
        let mut store = AgentShellStore::default();
        let session = store.ensure_session("pane-a").unwrap();
        for path in ["relative", "", "/bad\npath", "/bad\0path"] {
            let before = session.clone();
            assert!(
                session
                    .record_native_working_directory(path.into(), 0)
                    .is_err()
            );
            assert_eq!(session, &before);
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let before = session.clone();
            let path = PathBuf::from(std::ffi::OsString::from_vec(b"/invalid-\xff".to_vec()));
            assert!(session.record_native_working_directory(path, 0).is_err());
            assert_eq!(session, &before);
        }
        session.running_turn_id = Some("running".into());
        assert!(
            session
                .record_native_working_directory("/target".into(), 0)
                .is_err()
        );
        session.running_turn_id = None;
        session.native_working_directory = Some(AgentNativeWorkingDirectory {
            pane_id: session.pane_id.clone(),
            conversation_id: session.session_id.clone(),
            directory: "/previous".into(),
            generation: u64::MAX,
            instruction_files: None,
        });
        let before = session.clone();
        assert!(
            session
                .record_native_working_directory("/target".into(), u64::MAX)
                .is_err()
        );
        assert_eq!(session, &before);
    }

    /// Directory and discovery are one immutable adoption generation. A stale
    /// worker cannot publish either half, successful empty discovery clears old
    /// guidance, and session clones retain the exact previous rollback snapshot.
    #[test]
    fn native_directory_snapshot_owns_immutable_instruction_discovery() {
        let mut store = AgentShellStore::default();
        let session = store.ensure_session("pane-a").unwrap();
        let files = vec![DiscoveredInstructionFile {
            path: "/target/AGENTS.md".to_string(),
            scope_root: "/target".to_string(),
            bytes: 18,
            truncated: false,
            content: "private-rule-body".to_string(),
        }];
        session
            .record_native_working_directory_with_instructions("/target".into(), 0, files.clone())
            .unwrap();
        let retained = session.clone();
        let snapshot = session.native_directory_snapshot().unwrap();
        assert_eq!(snapshot.directory(), Path::new("/target"));
        assert_eq!(snapshot.instruction_files(), Some(files.as_slice()));
        assert!(snapshot.is_owned_by("pane-a", &session.session_id));
        assert!(!snapshot.is_owned_by("pane-b", &session.session_id));
        assert!(!format!("{snapshot:?}").contains("private-rule-body"));
        assert!(
            session
                .record_native_working_directory_with_instructions("/stale".into(), 0, Vec::new())
                .is_err()
        );
        assert_eq!(session, &retained);
        session.running_turn_id = Some("running".to_string());
        let busy = session.clone();
        assert!(
            session
                .record_native_working_directory_with_instructions("/busy".into(), 1, Vec::new())
                .is_err()
        );
        assert_eq!(session, &busy);
        session.running_turn_id = None;
        session
            .record_native_working_directory_with_instructions("/empty".into(), 1, Vec::new())
            .unwrap();
        assert_eq!(
            session
                .native_directory_snapshot()
                .unwrap()
                .instruction_files(),
            Some([].as_slice())
        );
        assert_eq!(
            retained
                .native_directory_snapshot()
                .unwrap()
                .instruction_files(),
            Some(files.as_slice())
        );
        session
            .record_native_working_directory("/unknown".into(), 2)
            .unwrap();
        assert_eq!(
            session
                .native_directory_snapshot()
                .unwrap()
                .instruction_files(),
            None
        );
        session.session_id = "foreign".to_string();
        assert_eq!(session.native_directory_snapshot(), None);
    }
}
