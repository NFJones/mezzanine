//! Readline data types and IO traits.
//!
//! These definitions describe edits, outcomes, prompt kinds, loop configuration,
//! and state containers while leaving behavior to focused sibling modules.

#[cfg(test)]
use crate::error::Result;
use crate::selector::{ActiveSelector, SelectorExtraCandidate};
use std::path::PathBuf;

pub use mez_mux::readline::{ReadlineBuffer, ReadlineEdit, ReadlineOutcome};

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
    pub buffer: ReadlineBuffer,
    /// Stores the selector value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub selector: Option<ActiveSelector>,
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
    /// Active incremental reverse history search state, when `Ctrl+R` is open.
    pub(super) reverse_search: Option<ReadlineReverseSearch>,
    /// Display cells available for the editable prompt body, when known.
    ///
    /// Runtime-owned prompt surfaces refresh this width before applying input so
    /// vertical arrow navigation can move through soft-wrapped visible rows
    /// before falling back to history traversal.
    pub(super) prompt_body_columns: Option<usize>,
}

/// Incremental reverse history search state for one prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReadlineReverseSearch {
    /// Prompt text before reverse search started.
    pub draft_line: String,
    /// Cursor byte offset before reverse search started.
    pub draft_cursor: usize,
    /// User-entered search substring.
    pub query: String,
    /// Currently selected history index, when one matches `query`.
    pub matched_index: Option<usize>,
}

/// Bounded driver settings for a live readline prompt surface.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadlinePromptLoopConfig {
    /// Stores the max iterations value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_iterations: usize,
    /// Stores the max input bytes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_input_bytes: usize,
    /// Stores the redraw on noop value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub redraw_on_noop: bool,
}

/// Summary of a bounded prompt-loop run.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadlinePromptLoopReport {
    /// Stores the iterations value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub iterations: usize,
    /// Stores the outcomes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub outcomes: Vec<ReadlineOutcome>,
    /// Stores the submissions value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub submissions: Vec<String>,
    /// Stores the cancelled value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cancelled: bool,
    /// Stores the eof value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub eof: bool,
    /// Stores the prompts rendered value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub prompts_rendered: usize,
    /// Stores the bytes written value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bytes_written: usize,
    /// Stores the pending input bytes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pending_input_bytes: usize,
}

/// Minimal IO boundary for command, configuration, and agent prompt loops.
#[cfg(test)]
pub trait ReadlinePromptLoopIo {
    /// Runs the input ready operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn input_ready(&mut self) -> Result<bool>;
    /// Runs the read input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_input(&mut self, max_bytes: usize) -> Result<Vec<u8>>;
    /// Runs the write prompt operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_prompt(&mut self, prompt: &ReadlinePrompt) -> Result<usize>;
}

/// Stateful terminal-input decoder for readline prompt surfaces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadlineInputDecoder {
    /// Stores the pending value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pending: Vec<u8>,
    /// Whether the decoder is inside a host bracketed-paste payload.
    pub(super) bracketed_paste_active: bool,
    /// Bytes captured for the current host bracketed-paste payload.
    pub(super) bracketed_paste: Vec<u8>,
}
