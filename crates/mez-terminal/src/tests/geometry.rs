use crate::{TerminalSize, TerminalSizeError};

/// Verifies valid terminal dimensions preserve both cell axes exactly so
/// terminal emulation and multiplexer adapters share one geometry contract.
#[test]
fn terminal_size_preserves_positive_dimensions() {
    let size = TerminalSize::new(80, 24).unwrap();

    assert_eq!(size.columns, 80);
    assert_eq!(size.rows, 24);
}

/// Rejects unsafe geometry using only dimension validation, never allocating
/// enormous terminal grids. Axis and aggregate-cell budgets are independent.
#[test]
fn terminal_size_rejects_excessive_geometry_before_allocation() {
    for (columns, rows) in [(4097, 1), (1, 4097), (1024, 257), (u16::MAX, u16::MAX)] {
        assert!(
            TerminalSize::new(columns, rows).is_err(),
            "{columns}x{rows}"
        );
    }
    for (columns, rows) in [(4096, 1), (1, 4096), (1024, 256), (512, 512)] {
        assert!(TerminalSize::new(columns, rows).is_ok(), "{columns}x{rows}");
    }
}

/// Verifies each zero axis is rejected with the stable diagnostic rather than
/// allowing an unusable terminal surface into parser or resize state.
#[test]
fn terminal_size_rejects_zero_axes() {
    for (columns, rows) in [(0, 24), (80, 0), (0, 0)] {
        let error = TerminalSize::new(columns, rows).unwrap_err();

        assert_eq!(error, TerminalSizeError::NonPositive);
        assert_eq!(
            error.message(),
            "terminal dimensions must be positive non-zero cells"
        );
    }
}
