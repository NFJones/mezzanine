//! Deferred runtime side-effect value types.
//!
//! Runtime service methods collect these records while mutating in-memory
//! session state, then hand them to async owners for process, persistence, hook,
//! and terminal-client work. Keeping the records in one module makes deferred
//! side-effect boundaries explicit without changing when the effects are
//! scheduled or drained.

/// Effects applied while processing one attached terminal client step.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}
