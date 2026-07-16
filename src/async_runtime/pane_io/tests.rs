//! Pane I/O configuration invariants.

use super::*;

/// Verifies async pane-input writes allow ten seconds for PTY writability
/// before reporting a bounded failure.
///
/// The async runtime drives live agent shell transactions, so this timeout
/// must be long enough for slower pane transports while still preventing
/// indefinite runtime worker stalls.
#[test]
fn async_pane_input_write_timeout_is_ten_seconds() {
    assert_eq!(PANE_INPUT_WRITE_READY_TIMEOUT, Duration::from_secs(10));
}
