//! Product agent-log wrapping defaults and legacy persistence helpers.
//!
//! `mez-terminal` owns Unicode segmentation and display-width measurement,
//! while `mez-mux` owns neutral wrapping. Live runtime rendering applies each
//! service's configured cap directly. This module retains the product default
//! used to normalize source-free persisted presentation records.

/// Default maximum display-cell width for Mezzanine-owned agent log rows.
pub(crate) const DEFAULT_AGENT_WRAP_COLUMN_CAP: usize = 120;

/// Returns the bounded display width used for Mezzanine-owned agent log rows.
pub(crate) fn agent_log_wrap_width(terminal_width: u16) -> usize {
    usize::from(terminal_width).clamp(1, DEFAULT_AGENT_WRAP_COLUMN_CAP)
}

/// Word-wraps one Mezzanine-owned agent log text block for terminal display.
///
/// Explicit newlines are preserved as row breaks. Individual logical rows wrap
/// at the nearest whitespace boundary before the display-cell limit, falling
/// back to hard grapheme boundaries when an unbroken token exceeds the limit.
pub(crate) fn wrap_agent_log_text(value: &str, terminal_width: u16) -> Vec<String> {
    mez_mux::render::wrap_text(value, agent_log_wrap_width(terminal_width))
}

/// Word-wraps Mezzanine-owned agent log rows for terminal display.
pub(crate) fn wrap_agent_log_lines(lines: &[String], terminal_width: u16) -> Vec<String> {
    let mut wrapped = Vec::new();
    for line in lines {
        wrapped.extend(wrap_agent_log_text(line, terminal_width));
    }
    wrapped
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_AGENT_WRAP_COLUMN_CAP, agent_log_wrap_width, wrap_agent_log_text};
    use mez_terminal::active_terminal_text_width;

    /// Verifies agent log wrapping uses the pane width until the default cap
    /// applies, so very wide terminals do not create unbounded transcript rows.
    #[test]
    fn agent_log_wrap_width_caps_terminal_width_at_default_columns() {
        assert_eq!(agent_log_wrap_width(0), 1);
        assert_eq!(agent_log_wrap_width(80), 80);
        assert_eq!(agent_log_wrap_width(200), DEFAULT_AGENT_WRAP_COLUMN_CAP);
    }

    /// Verifies the 120-column cap is applied even when the active pane is
    /// wider, protecting persisted replay rows from host-width drift.
    #[test]
    fn wrap_agent_log_text_applies_global_column_cap() {
        let wrapped = wrap_agent_log_text(&"x".repeat(130), 200);

        assert_eq!(active_terminal_text_width(&wrapped[0]), 120);
        assert_eq!(active_terminal_text_width(&wrapped[1]), 10);
    }
}
