//! Readline data types and IO traits.
//!
//! These definitions describe edits, outcomes, prompt kinds, loop configuration,
//! and state containers while leaving behavior to focused sibling modules.

use crate::selector::{SelectorExtraCandidate, SelectorSurface};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;

use mez_mux::readline::ReadlinePromptState;

/// The interactive surface using a readline buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadlinePromptKind {
    /// Represents the Command case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Command,
    /// Represents the Agent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Agent,
}

/// A prompt instance for one interactive command surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadlinePrompt {
    /// Stores the kind value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub kind: ReadlinePromptKind,
    /// Stores the buffer value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub state: ReadlinePromptState,
    /// Stores the selector value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub selector: Option<mez_mux::selector::ActiveSelector<SelectorSurface>>,
    /// Runtime-provided selector candidates scoped to this prompt instance.
    ///
    /// These values are refreshed by the owning runtime before user input is
    /// applied so completions can include dynamic objects such as saved agent
    /// conversation ids without making the selector depend on runtime state.
    pub selector_extra_candidates: Vec<SelectorExtraCandidate>,
    /// Prompt-local working directory used for relative path completion.
    ///
    /// Runtime-owned prompt surfaces refresh this value before applying input
    /// so selector filesystem candidates resolve against the active pane cwd
    /// instead of the Mez server process working directory.
    pub selector_working_directory: Option<PathBuf>,
}

impl Deref for ReadlinePrompt {
    type Target = ReadlinePromptState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl DerefMut for ReadlinePrompt {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

/// Stateful terminal-input decoder for readline prompt surfaces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadlineInputDecoder {
    /// Dependency-neutral terminal sequence decoder owned by `mez-mux`.
    pub(super) inner: mez_mux::readline::ReadlineTerminalInputDecoder,
}
