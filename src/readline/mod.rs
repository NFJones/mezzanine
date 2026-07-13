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
/// Exposes the prompt loop module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod prompt_loop;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

pub use mez_mux::readline::apply_readline_terminal_input;
pub use mez_mux::readline::{
    DEFAULT_READLINE_HISTORY_LIMIT, ReadlineBuffer, ReadlineEdit, ReadlineOutcome,
};
#[cfg(test)]
pub use prompt_loop::run_readline_prompt_loop;
pub use types::{ReadlineInputDecoder, ReadlinePrompt, ReadlinePromptKind};
#[cfg(test)]
pub use types::{ReadlinePromptLoopConfig, ReadlinePromptLoopIo, ReadlinePromptLoopReport};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
