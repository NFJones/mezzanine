//! Terminal emulation and compatibility for one terminal surface.
//!
//! This crate will own pane-facing terminal parsing, screen state, history,
//! capability profiles, and mode-aware input encoding. Multiplexer layout,
//! frames, overlays, attached-client policy, and agent presentation remain
//! outside this boundary. The initial empty facade allows those responsibilities
//! to be separated in place before production modules move across packages.
