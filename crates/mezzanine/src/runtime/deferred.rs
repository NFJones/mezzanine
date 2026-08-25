//! Deferred runtime side-effect value types.
//!
//! Runtime service methods collect these records while mutating in-memory
//! session state, then hand them to async owners for process, persistence, hook,
//! and terminal-client work. Keeping the records in one module makes deferred
//! side-effect boundaries explicit without changing when the effects are
//! scheduled or drained.

use std::fmt;

/// Sensitive clipboard payload produced by one interactive attached-client step.
///
/// The actor consumes this value before returning the application report. Its
/// debug representation deliberately exposes only byte length.
#[derive(Clone, PartialEq, Eq)]
pub struct AttachedClientClipboardWrite {
    content: String,
}

impl AttachedClientClipboardWrite {
    /// Creates one transient clipboard candidate from the selected text.
    pub(crate) fn new(content: String) -> Self {
        Self { content }
    }

    /// Consumes the wrapper and returns the payload for exact-route enqueue.
    pub(crate) fn into_content(self) -> String {
        self.content
    }
}

impl fmt::Debug for AttachedClientClipboardWrite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttachedClientClipboardWrite")
            .field("byte_len", &self.content.len())
            .finish()
    }
}

/// Effects applied while processing one attached terminal client step.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttachedClientStepApplication {
    /// Number of raw input bytes forwarded to panes.
    pub forwarded_bytes: usize,
    /// Number of mux actions successfully applied.
    pub mux_actions_applied: usize,
    /// Number of mouse actions reported by input routing.
    pub mouse_actions_reported: usize,
    /// Mux actions that were recognized but not supported by the runtime.
    pub unsupported_actions: Vec<String>,
    /// Number of agent prompt inputs applied from this client step.
    pub agent_prompt_inputs_applied: usize,
    /// Whether the client view should be refreshed after the step.
    pub view_refresh_required: bool,
    /// Whether the client needs a full redraw after the step.
    pub full_redraw_required: bool,
    /// Whether this step changed session metadata persisted by the registry.
    pub registry_persistence_required: bool,
    /// Transient client-local clipboard candidate produced by this step.
    pub(crate) client_clipboard_write: Option<AttachedClientClipboardWrite>,
}
