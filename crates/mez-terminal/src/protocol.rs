//! Pane-facing terminal protocol events.
//!
//! These events are emitted while parsing one terminal surface. Consumers may
//! interpret them as multiplexer or product effects, but those policies remain
//! outside this crate.

/// Maximum bytes retained while parsing one operating-system-command payload.
///
/// Parsers should continue consuming an oversized sequence through its
/// terminator while discarding bytes beyond this bound.
pub const MAX_OSC_STRING_BYTES: usize = 4096;

/// A structured event produced by an operating-system-command sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalOscEvent {
    /// OSC 133 supplied a shell-integration payload for product interpretation.
    ///
    /// The terminal compatibility layer preserves the payload without
    /// assigning application-specific transaction semantics to its fields.
    ShellIntegration {
        /// Payload after the `133;` command prefix.
        payload: String,
    },
    /// OSC 0 or OSC 2 changed the terminal title.
    TitleChanged {
        /// Newly requested terminal title.
        title: String,
    },
    /// OSC 52 requested clipboard content to be set.
    ClipboardSet {
        /// Clipboard selection identifier from the OSC payload.
        selection: String,
        /// Decoded UTF-8 clipboard content.
        content: String,
    },
    /// OSC 133 marked the start of a shell prompt.
    ShellPromptStart,
    /// OSC 133 marked the end of a shell prompt.
    ShellPromptEnd,
    /// OSC 133 marked the start of command output.
    ShellCommandOutputStart,
    /// OSC 133 marked command completion.
    ShellCommandFinished {
        /// Parsed process exit code, when supplied by the terminal program.
        exit_code: Option<i32>,
    },
    /// A Mezzanine-owned OSC 133 marker started a shell transaction.
    ShellTransactionStart {
        /// Unpredictable transaction marker used to correlate boundaries.
        marker: String,
        /// Agent turn identifier associated with the transaction.
        turn_id: String,
        /// Agent identifier associated with the transaction.
        agent_id: String,
        /// Pane identifier associated with the transaction.
        pane_id: String,
    },
    /// A Mezzanine-owned OSC 133 marker ended a shell transaction.
    ShellTransactionEnd {
        /// Unpredictable transaction marker used to correlate boundaries.
        marker: String,
        /// Agent turn identifier associated with the transaction.
        turn_id: String,
        /// Agent identifier associated with the transaction.
        agent_id: String,
        /// Pane identifier associated with the transaction.
        pane_id: String,
        /// Process exit code supplied by the shell wrapper.
        exit_code: i32,
    },
}
