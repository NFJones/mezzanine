//! Product content-length protocol framing and configured template rendering.
//!
//! Both the control endpoint and local message protocol use an ASCII header
//! block followed by a UTF-8 JSON body. This module provides shared frame
//! encoding and strict bounded decoding. Its template records support product
//! control requests; terminal UI template expansion lives in `mez-mux`.

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
