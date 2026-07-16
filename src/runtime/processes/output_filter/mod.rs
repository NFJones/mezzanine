//! Product pane-output state and terminal OSC integration.
//!
//! Portable wrapper filtering and model-facing observation cleanup live in
//! `mez_agent::shell_observation`. This adapter retains pane-owned fragments,
//! terminal OSC 133 parsing, screen mutation, timers, titles, and PTY routing.

mod pane_state;
mod terminal_apply;
