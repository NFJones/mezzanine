//! Readline data types and IO traits.
//!
//! These definitions describe edits, outcomes, prompt kinds, loop configuration,
//! and state containers while leaving behavior to focused sibling modules.

#[cfg(test)]
use crate::error::Result;
use crate::selector::{ActiveSelector, SelectorExtraCandidate};

/// Default number of submitted prompt entries retained by a readline buffer.
pub const DEFAULT_READLINE_HISTORY_LIMIT: usize = 1000;
/// Minimum pasted text byte count rendered as one collapsed prompt block.
pub const READLINE_PASTE_BLOCK_THRESHOLD_BYTES: usize = 1024;

/// A normalized editing command for a readline-style prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadlineEdit {
    /// Represents the Insert case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Insert(char),
    /// Represents the Insert Text case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    InsertText(String),
    /// Represents the Move Left case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    MoveLeft,
    /// Represents the Move Right case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    MoveRight,
    /// Move left by one shell-style word.
    MoveWordLeft,
    /// Move right by one shell-style word.
    MoveWordRight,
    /// Represents the Move Home case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    MoveHome,
    /// Represents the Move End case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    MoveEnd,
    /// Move to the beginning of the whole editable buffer.
    MoveBufferStart,
    /// Move to the end of the whole editable buffer.
    MoveBufferEnd,
    /// Move to the previous prompt row when editing multiline input, or
    /// history when no previous row exists.
    MoveRowUpOrHistoryPrevious,
    /// Move to the next prompt row when editing multiline input, or history
    /// when no next row exists.
    MoveRowDownOrHistoryNext,
    /// Represents the Backspace case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Backspace,
    /// Represents the Delete Forward case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DeleteForward,
    /// Delete the shell-style word before the cursor.
    KillWordLeft,
    /// Delete the shell-style word after the cursor.
    KillWordRight,
    /// Represents the Kill To Start case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    KillToStart,
    /// Represents the Kill To End case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    KillToEnd,
    /// Represents the History Previous case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HistoryPrevious,
    /// Represents the History Next case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HistoryNext,
    /// Represents the History Search Backward case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HistorySearchBackward,
    /// Represents the Submit case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Submit,
}

/// The observable result of applying an editing command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadlineOutcome {
    /// Represents the Edited case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Edited,
    /// Represents the Submitted case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Submitted(String),
    /// A submitted value whose prompt display was intentionally collapsed.
    ///
    /// The raw `text` is sent to the command or agent backend while `display`
    /// is safe to echo in the pane transcript.
    SubmittedWithDisplay {
        /// Exact submitted text.
        text: String,
        /// Human-readable collapsed display text.
        display: String,
    },
    /// Represents the Cancelled case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Cancelled,
    /// Represents the Eof case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Eof,
    /// Represents the Noop case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Noop,
}

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

/// Editable line state with bounded submission history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadlineBuffer {
    /// Stores the line value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) line: String,
    /// Stores the cursor value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) cursor: usize,
    /// Stores the history value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) history: Vec<String>,
    /// Stores the history limit value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) history_limit: usize,
    /// Stores the history cursor value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) history_cursor: Option<usize>,
    /// Tracks whether cursor-only navigation has entered the recalled history entry.
    ///
    /// History traversal first treats each recalled prompt as one whole entry.
    /// Once the user moves the editing cursor inside that entry, Up and Down
    /// navigate its rows before falling back to older or newer history.
    pub(super) history_entry_cursor_navigation: bool,
    /// Preferred display column retained across consecutive vertical moves.
    ///
    /// Wrapped or multiline rows can be shorter than the column where vertical
    /// navigation began. Keeping the original target column lets later Up/Down
    /// moves return to that column after passing through a shorter row.
    pub(super) vertical_navigation_column: Option<usize>,
    /// Stores the draft before history value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) draft_before_history: String,
    /// Stores opaque pasted text blocks referenced from the editable line.
    ///
    /// Large pasted content is represented by a single private-use marker in
    /// `line` so rendering and cursor motion stay bounded while submission can
    /// still recover the exact pasted text.
    pub(super) paste_blocks: Vec<ReadlinePasteBlock>,
    /// Stores the next private-use marker identifier for pasted blocks.
    ///
    /// The monotonically increasing counter keeps multiple pasted blocks
    /// distinguishable even when users insert and delete around them.
    pub(super) next_paste_block_id: u32,
    /// Stores pasted blocks that belong to the draft line before history navigation.
    ///
    /// Readline history temporarily replaces the editable line; preserving the
    /// draft blocks lets Down restore an in-progress prompt containing large
    /// pasted content.
    pub(super) draft_before_history_paste_blocks: Vec<ReadlinePasteBlock>,
    /// Stores the next pasted-block id for the draft before history navigation.
    pub(super) draft_before_history_next_paste_block_id: u32,
}

/// One large pasted payload collapsed in prompt rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadlinePasteBlock {
    /// Private-use marker stored in the editable line.
    pub(super) marker: char,
    /// Exact pasted text restored on submission.
    pub(super) content: String,
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
