//! Dependency-neutral multiplexer presentation contracts.
//!
//! This module owns small value types used to compose terminal surfaces into
//! pane and window presentation. Product configuration and agent-specific
//! frame metadata remain in the Mezzanine composition crate.

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
