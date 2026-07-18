//! Pane-facing terminal protocol events.
//!
//! These events are emitted while parsing one terminal surface. Consumers may
//! interpret them as multiplexer or product effects, but those policies remain
//! outside this crate.

use std::fmt;

/// Maximum bytes retained while parsing one operating-system-command payload.
///
/// Parsers should continue consuming an oversized sequence through its
/// terminator while discarding bytes beyond this bound.
pub const MAX_OSC_STRING_BYTES: usize = 4096;

/// Identifies the clipboard selection named by one OSC 52 request.
///
/// The terminal protocol permits an empty selection and implementation-defined
/// selection identifiers. This type therefore preserves the bounded protocol
/// value without assigning product routing semantics to it.
#[derive(Clone, PartialEq, Eq)]
pub struct TerminalClipboardSelection(String);

impl TerminalClipboardSelection {
    /// Preserves one OSC 52 selection parameter for downstream policy.
    pub fn new(selection: impl Into<String>) -> Self {
        Self(selection.into())
    }

    /// Returns the selection parameter exactly as it appeared in the request.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for TerminalClipboardSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("TerminalClipboardSelection")
            .field(&self.0)
            .finish()
    }
}

/// Holds decoded UTF-8 clipboard content without exposing it through `Debug`.
///
/// Clipboard text may contain credentials or other sensitive user data. The
/// protocol layer exposes the content deliberately through `as_str` while
/// ordinary diagnostics retain only its byte length.
#[derive(Clone, PartialEq, Eq)]
pub struct TerminalClipboardContent(String);

impl TerminalClipboardContent {
    /// Wraps decoded UTF-8 clipboard content emitted by the terminal parser.
    pub fn new(content: impl Into<String>) -> Self {
        Self(content.into())
    }

    /// Returns the decoded clipboard text to an authorized effect adapter.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for TerminalClipboardContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalClipboardContent")
            .field("bytes", &self.0.len())
            .finish()
    }
}

/// A typed OSC 52 clipboard operation emitted by one terminal surface.
///
/// Parsing distinguishes writes from queries, but authorization, clipboard
/// routing, host access, and query support remain mux or product decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalClipboardRequest {
    /// Requests that decoded UTF-8 content replace a terminal selection.
    Write {
        /// Clipboard selection named by the terminal application.
        selection: TerminalClipboardSelection,
        /// Decoded UTF-8 content carried by the request.
        content: TerminalClipboardContent,
    },
    /// Requests the current content of a terminal selection.
    Query {
        /// Clipboard selection named by the terminal application.
        selection: TerminalClipboardSelection,
    },
}

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
    /// OSC 52 supplied a typed clipboard write or query request.
    Clipboard(TerminalClipboardRequest),
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
