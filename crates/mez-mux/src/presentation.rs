//! Dependency-neutral multiplexer presentation contracts.
//!
//! This module owns small value types used to compose terminal surfaces into
//! pane and window presentation. Product configuration and agent-specific
//! frame metadata remain in the Mezzanine composition crate.

use std::collections::BTreeMap;

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
