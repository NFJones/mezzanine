//! Dependency-neutral multiplexer presentation contracts.
//!
//! This module owns small value types used to compose terminal surfaces into
//! pane and window presentation. Product configuration and agent-specific
//! frame metadata remain in the Mezzanine composition crate.

use std::collections::BTreeMap;

/// Per-pane metadata consumed by mux frame and body presentation.
///
/// Scalar fields are presentation-only values. The prompt and supplemental
/// body-line types remain generic so the product can supply richer UI state
/// without making the mux depend on product-owned prompt or agent types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPaneFrameContext<Prompt = (), DisplayLines = Vec<String>> {
    /// Primary process id shown by `pane.primary_pid`.
    pub primary_pid: Option<u32>,
    /// Primary process name shown by `pane.process_name` when known.
    pub process_name: Option<String>,
    /// Primary process exit status shown by `pane.exit_status` when known.
    pub exit_status: Option<String>,
    /// Home-relative current working directory shown by `pane.pwd` when known.
    pub current_working_directory: Option<String>,
    /// Current pane interaction mode shown by `pane.mode`.
    pub mode: Option<String>,
    /// Opaque agent identity shown by `agent.id`.
    pub agent_id: Option<String>,
    /// Human-readable agent label shown by `agent.name`.
    pub agent_name: Option<String>,
    /// Opaque agent state shown by `agent.status`.
    pub agent_status: Option<String>,
    /// Active model label shown by `agent.model`.
    pub agent_model: Option<String>,
    /// Active reasoning label shown by `agent.reasoning`.
    pub agent_reasoning: Option<String>,
    /// Provider thinking-mode label shown by `agent.thinking`.
    pub agent_thinking: Option<String>,
    /// Pane-local routing label shown by `agent.routing`.
    pub agent_routing: Option<String>,
    /// Active latency label shown by `agent.latency`.
    pub agent_latency: Option<String>,
    /// Active model preset label shown by `agent.preset`.
    pub agent_preset: Option<String>,
    /// Last known input-context usage shown by `agent.context_usage`.
    pub agent_context_usage: Option<String>,
    /// Scrollback position shown by `history.position` when not at the live bottom.
    pub history_position: Option<String>,
    /// Product-owned prompt state rendered inside the pane body.
    pub agent_prompt: Option<Prompt>,
    /// Product-owned supplemental lines rendered above the prompt.
    pub agent_display_lines: DisplayLines,
}

impl<Prompt, DisplayLines: Default> Default for TerminalPaneFrameContext<Prompt, DisplayLines> {
    fn default() -> Self {
        Self {
            primary_pid: None,
            process_name: None,
            exit_status: None,
            current_working_directory: None,
            mode: None,
            agent_id: None,
            agent_name: None,
            agent_status: None,
            agent_model: None,
            agent_reasoning: None,
            agent_thinking: None,
            agent_routing: None,
            agent_latency: None,
            agent_preset: None,
            agent_context_usage: None,
            history_position: None,
            agent_prompt: None,
            agent_display_lines: DisplayLines::default(),
        }
    }
}

/// Runtime fields available to the right side of a window status line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalWindowStatusContext {
    /// Named-field template for the right status line.
    pub template: String,
    /// Home-relative active pane working directory shown by `pane.pwd`.
    pub active_pane_working_directory: Option<String>,
    /// Cached command-backed pill values keyed by configured pill name.
    pub status_pills: BTreeMap<String, String>,
    /// Human-readable system uptime shown by `system.uptime`.
    pub system_uptime: String,
    /// Human-readable local datetime shown by `datetime.local`.
    pub datetime_local: String,
}

/// Runtime window metadata made available to default window-frame rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalWindowFrameContext {
    /// Stable window identity.
    pub id: String,
    /// Display index in the session window list.
    pub index: usize,
    /// User-facing title or name for the window.
    pub title: String,
    /// Whether this window is currently focused.
    pub active: bool,
    /// Whether this window is dedicated to spawned subagent panes.
    pub subagent: bool,
}

/// Runtime window-group metadata made available to group-frame rendering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalWindowGroupFrameContext {
    /// Stable group identity.
    pub id: String,
    /// Display index in the session group list.
    pub index: usize,
    /// User-facing title or name for the group.
    pub title: String,
    /// Whether this group is currently focused.
    pub active: bool,
}

/// Placement of a one-row terminal frame within its owning region.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerminalFramePosition {
    /// Render the frame before region body content.
    #[default]
    Top,
    /// Render the frame after region body content.
    Bottom,
}

/// Style applied to a rendered frame row when styled terminal output is used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerminalFrameStyle {
    /// Leave frame text unstyled.
    #[default]
    Default,
    /// Render the frame with bold/intense text.
    Bold,
    /// Render the frame with underline text.
    Underline,
    /// Render the frame with inverse video.
    Inverse,
}

#[cfg(test)]
mod tests {
    use super::{TerminalFramePosition, TerminalFrameStyle};

    /// Verifies neutral frame contracts retain the product's established
    /// top-positioned and unstyled defaults after ownership moves to the mux.
    #[test]
    fn frame_contract_defaults_remain_stable() {
        assert_eq!(TerminalFramePosition::default(), TerminalFramePosition::Top);
        assert_eq!(TerminalFrameStyle::default(), TerminalFrameStyle::Default);
    }
}
