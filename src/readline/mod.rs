//! Readline-style line editing primitives for Mezzanine command surfaces.
//!
//! The agent shell and configuration shell both need predictable editable
//! prompt behavior. This module keeps the editing state independent from any
//! concrete terminal renderer so command surfaces can be tested without a live
//! terminal and later wired to key decoding and drawing code.

/// Exposes the buffer module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
/// Exposes the decoder module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod decoder;
/// Exposes the prompt module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod prompt;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

#[cfg(test)]
use mez_mux::readline::{ReadlineEdit, ReadlineOutcome};
pub use types::{ReadlineInputDecoder, ReadlinePrompt, ReadlinePromptKind};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
