use crate::{TerminalSize, TerminalSizeError};

/// Verifies valid terminal dimensions preserve both cell axes exactly so
/// terminal emulation and multiplexer adapters share one geometry contract.
#[test]
fn terminal_size_preserves_positive_dimensions() {
    let size = TerminalSize::new(80, 24).unwrap();

    assert_eq!(size.columns, 80);
    assert_eq!(size.rows, 24);
}

/// Verifies each zero axis is rejected with the stable diagnostic rather than
/// allowing an unusable terminal surface into parser or resize state.
#[test]
fn terminal_size_rejects_zero_axes() {
    for (columns, rows) in [(0, 24), (80, 0), (0, 0)] {
        let error = TerminalSize::new(columns, rows).unwrap_err();

        assert_eq!(error, TerminalSizeError);
        assert_eq!(
            error.message(),
            "terminal dimensions must be positive non-zero cells"
        );
    }
}
