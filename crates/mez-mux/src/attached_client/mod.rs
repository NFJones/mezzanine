//! Neutral attached-terminal client policy.
//!
//! The attached-client boundary owns terminal frame encoding and other
//! deterministic host-presentation plans. Product crates supply raw terminal
//! I/O, readiness, command routing, and endpoint error handling.

mod input;
mod mouse;
mod output;

pub use input::{earliest_sequence_start, input_sequence_start, prefix_sequence_len};
pub use mouse::{
    application_cursor_forwarding_bytes, application_mouse_forwarding_bytes,
    malformed_sgr_mouse_prefix_len, sgr_mouse_sequence_len, sgr_mouse_sequence_start,
};
pub use output::{
    AttachedTerminalOutputFrameState, attached_terminal_enter_presentation_frame,
    attached_terminal_restore_presentation_frame,
    encode_attached_terminal_output_frame_with_styles,
    encode_attached_terminal_output_update_frame_with_styles,
};

#[cfg(test)]
mod tests;
