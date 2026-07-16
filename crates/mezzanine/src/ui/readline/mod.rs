//! Product readline adapters for Mezzanine command surfaces.
//!
//! `mez-mux` owns the generic editable buffer, history search, input decoding,
//! and selection state. This module specializes that state with Mezzanine and
//! agent prompt kinds, prefixes, selector policy, and rendering inputs.

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
