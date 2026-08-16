//! Pane-facing terminal protocol events.
//!
//! These events are emitted while parsing one terminal surface. Consumers may
//! interpret them as multiplexer or product effects, but those policies remain
//! outside this crate.

use std::fmt;

/// Current shell-neutral managed-adapter protocol version.
pub const MANAGED_SHELL_PROTOCOL_VERSION: u16 = 2;

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
    /// A versioned semantic event from a managed interactive shell adapter.
    ManagedShell {
        /// Managed-shell protocol version understood by the adapter.
        version: u16,
        /// Shell adapter that emitted the event.
        shell: ManagedShellAdapter,
        /// Pane-scoped token authenticating the adapter process.
        token: String,
        /// Semantic lifecycle event independent from shell-native syntax.
        event: ManagedShellProtocolEvent,
    },
    /// A managed Zsh ZLE widget accepted its fixed private receiver command.
    ShellReceiverAwaiting {
        /// Pane-scoped receiver token installed at Zsh startup.
        token: String,
    },
    /// A managed shell startup shim installed its non-destructive receiver.
    ShellReceiverAvailable {
        /// Pane-scoped token authenticating the startup shim.
        token: String,
        /// Managed shell that published availability.
        shell: String,
        /// Fixed trigger identifier selected without replacing user bindings.
        trigger: String,
    },
    /// A managed shell startup shim could not install safely.
    ShellReceiverUnavailable {
        /// Pane-scoped token authenticating the startup shim.
        token: String,
        /// Managed shell that published the failure.
        shell: String,
        /// Bounded machine-readable failure reason.
        reason: String,
    },
    /// A managed Zsh parent restored its saved editor state after admission.
    ShellParentRestored {
        /// Pane-scoped receiver token installed at Zsh startup.
        token: String,
        /// Unpredictable transaction marker whose parent state was restored.
        marker: String,
        /// Status returned by the admitted source.
        exit_code: i32,
    },
    /// A managed Bash receiver admitted one private source transaction.
    ShellReceiverReady {
        /// Pane-scoped receiver token installed at Bash startup.
        token: String,
        /// Unpredictable transaction marker awaiting source delivery.
        marker: String,
    },
    /// A managed Bash child installed its receiver and is awaiting admission.
    ShellReceiverInstalled {
        /// Pane-scoped receiver token installed at Bash startup.
        token: String,
        /// Unpredictable transaction marker awaiting the child trigger.
        marker: String,
    },
    /// A managed Bash receiver completed eval and callback cleanup.
    ShellReceiverComplete {
        /// Pane-scoped receiver token installed at Bash startup.
        token: String,
        /// Unpredictable transaction marker that completed.
        marker: String,
        /// Eval status returned by the private receiver.
        exit_code: i32,
    },
    /// A Fish transaction receiver is armed for its deferred payload.
    ShellTransactionPayloadReceiverReady {
        /// Unpredictable transaction marker awaiting payload delivery.
        marker: String,
        /// Agent turn identifier associated with the transaction.
        turn_id: String,
        /// Agent identifier associated with the transaction.
        agent_id: String,
        /// Pane identifier associated with the transaction.
        pane_id: String,
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

/// Managed interactive shell adapter named by the semantic handoff protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedShellAdapter {
    /// GNU Bash Readline adapter.
    Bash,
    /// Fish command-line editor adapter.
    Fish,
    /// Zsh ZLE adapter.
    Zsh,
}

/// Shell-neutral lifecycle event emitted by a managed shell adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedShellProtocolEvent {
    /// The startup adapter installed its private trigger safely.
    AdapterAvailable,
    /// The native editor saved and cleared user-owned input.
    EditorHeld {
        /// Unpredictable handoff marker owned by the editor callback.
        marker: String,
    },
    /// The adapter admitted a bounded private source frame.
    FrameAdmitted {
        /// Unpredictable handoff or transaction marker.
        marker: String,
    },
    /// The persistent child installed its private receiver.
    ChildInstalled {
        /// Unpredictable handoff marker inherited by the child.
        marker: String,
    },
    /// The adapter rejected admission without evaluating source.
    ReceiverRejected {
        /// Marker when the adapter parsed one safely, otherwise absent.
        marker: Option<String>,
        /// Bounded machine-readable rejection reason.
        reason: String,
    },
    /// The persistent child process exited.
    ChildExited {
        /// Unpredictable handoff marker owned by the child.
        marker: String,
        /// Child process status observed by the adapter.
        exit_code: i32,
    },
    /// The original parent editor is restored and ready for ordinary input.
    ParentReady {
        /// Unpredictable handoff or transaction marker.
        marker: String,
        /// Typed adapter outcome for the completed callback.
        outcome: ManagedShellParentOutcome,
        /// Source or child status retained for diagnostics.
        exit_code: i32,
        /// Optional parent-only provenance proof for persistent handoffs.
        proof: Option<String>,
    },
}

/// Typed terminal outcome for a managed-shell parent callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedShellParentOutcome {
    /// The admitted source completed and the parent editor was restored.
    Completed,
    /// Runtime cancelled admission before source evaluation.
    Cancelled,
    /// The private frame failed bounded validation.
    FrameRejected,
    /// Fully admitted source failed during evaluation.
    SourceFailed,
    /// The persistent child could not be launched.
    ChildLaunchFailed,
}
