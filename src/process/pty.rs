//! PTY sizing helpers.
//!
//! This module keeps portable-pty size conversion separate from process
//! spawning so the pane process handle can own nonblocking PTY I/O directly.

use portable_pty::PtySize;

use crate::layout::Size;

/// Defines the PTY IO CHUNK BYTES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const PTY_IO_CHUNK_BYTES: usize = 64 * 1024;

/// Runs the pty size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pty_size(size: Size) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.columns,
        pixel_width: 0,
        pixel_height: 0,
    }
}
