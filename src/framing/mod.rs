//! Content-length protocol framing and visible frame rendering.
//!
//! Both the control endpoint and local message protocol use an ASCII header
//! block followed by a UTF-8 JSON body. This module provides shared frame
//! encoding and strict bounded decoding. It also contains the small template
//! renderer used by window and pane frames; that renderer is intentionally
//! independent from terminal drawing so it can be tested before the full
//! renderer exists.

/// Exposes the codec module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod codec;
/// Exposes the render module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod render;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;
/// Exposes the wire module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod wire;

pub use render::{render_frame_template, render_pending_observer_status, sanitize_frame_text};
pub use types::{FrameContext, FrameOverflow, ProtocolFrame, ProtocolFrameCodec};
pub use wire::{decode_frame, encode_frame};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
