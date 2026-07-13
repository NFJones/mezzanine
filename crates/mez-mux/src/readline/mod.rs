//! Readline-style editing state for multiplexer-owned prompt surfaces.
//!
//! This module owns dependency-neutral text editing, history, collapsed paste,
//! Unicode cursor, and word-boundary behavior. Product selector policy and live
//! prompt I/O remain in the Mezzanine composition crate.

mod buffer;

pub use buffer::{
    DEFAULT_READLINE_HISTORY_LIMIT, ReadlineBuffer, ReadlineEdit, ReadlineOutcome,
    readline_word_column_range,
};
