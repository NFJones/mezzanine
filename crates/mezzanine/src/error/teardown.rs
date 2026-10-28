//! Bounded diagnostics for ordered teardown outcomes, independent of custody.
//!
//! Callers perform every safe cleanup stage through their existing owners. This
//! collector changes only error reporting: the first observed error retains its
//! kind and typed evidence; later failures have bounded, sorted diagnostics and
//! an explicit omitted count. Recording or rendering a failure grants no retry,
//! process retirement, durability or successful-shutdown authority.

use super::{MezError, Result};

/// Maximum detailed later failures retained beside the complete primary error.
const MAX_SECONDARY_FAILURES: usize = 8;
/// Byte allowance for each non-authorizing diagnostic stage label.
const MAX_STAGE_BYTES: usize = 128;
/// Byte allowance for later error text, followed by an explicit truncation tag.
const MAX_SECONDARY_BYTES: usize = 1024;

/// Retains the initiating error and bounded evidence from subsequent stages.
#[derive(Default)]
pub(crate) struct TeardownErrors {
    /// The complete original error remains authoritative for classification.
    primary: Option<MezError>,
    /// Lexically selected diagnostics keep output stable across peer completion.
    secondary: Vec<(String, String)>,
    /// Counts all later failures, including diagnostics beyond the retained cap.
    secondary_count: usize,
}

impl TeardownErrors {
    /// Records one failed stage without replacing an earlier cause. Stage names
    /// and secondary text are bounded on UTF-8 boundaries; typed primary evidence
    /// survives unchanged apart from added human-readable diagnostic context.
    pub(crate) fn record(&mut self, stage: &str, error: MezError) {
        let stage = bounded(stage, MAX_STAGE_BYTES);
        if self.primary.is_none() {
            self.primary = Some(error.with_context(stage));
            return;
        }
        self.secondary_count = self.secondary_count.saturating_add(1);
        let diagnostic = bounded(&error.to_string(), MAX_SECONDARY_BYTES);
        self.secondary.push((stage, diagnostic));
        self.secondary.sort();
        self.secondary.truncate(MAX_SECONDARY_FAILURES);
    }

    /// Returns success only when every recorded stage succeeded. Failure keeps
    /// the first error's kind and policy observations, adds bounded secondary
    /// diagnostics, and explicitly reports any omitted failure count.
    pub(crate) fn finish(self) -> Result<()> {
        let Some(mut primary) = self.primary else {
            return Ok(());
        };
        for (stage, diagnostic) in &self.secondary {
            primary
                .message
                .push_str(&format!("; {stage} also failed: {diagnostic}"));
        }
        let omitted = self.secondary_count.saturating_sub(self.secondary.len());
        if omitted > 0 {
            primary
                .message
                .push_str(&format!("; {omitted} additional teardown failures omitted"));
        }
        Err(primary)
    }
}

/// Copies at most the accepted byte budget plus an explicit truncation marker.
/// Unicode input never causes a split-codepoint panic or an unbounded suffix.
fn bounded(text: &str, maximum: usize) -> String {
    if text.len() <= maximum {
        return text.into();
    }
    let mut end = maximum;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} [truncated]", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Aggregation must retain the original I/O classification and policy
    /// evidence even when later cleanup errors have different kinds. Sorting
    /// diagnostic labels must not promote a later error into the primary cause.
    #[test]
    fn teardown_errors_preserve_primary_evidence_and_order_secondary_diagnostics() {
        let original = MezError::from(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            .mark_pane_backend_failure()
            .with_provider_raw_text("original-evidence");
        let kind = original.kind();
        let mut errors = TeardownErrors::default();
        errors.record("initiating stage", original);
        errors.record("z-stage", MezError::invalid_state("Z_SECONDARY"));
        errors.record("a-stage", MezError::conflict("A_SECONDARY"));
        let error = errors.finish().unwrap_err();
        assert_eq!(error.kind(), kind);
        assert_eq!(error.io_kind(), Some(std::io::ErrorKind::PermissionDenied));
        assert!(error.is_pane_backend_failure());
        assert_eq!(error.provider_raw_text(), Some("original-evidence"));
        assert!(
            error.message().find("A_SECONDARY").unwrap()
                < error.message().find("Z_SECONDARY").unwrap()
        );
    }

    /// Many long Unicode failures cannot grow cleanup diagnostics without bound.
    /// Stable lexical retention and the omitted count remain identical when the
    /// same secondary outcomes are observed in a different completion order.
    #[test]
    fn teardown_errors_bound_secondary_diagnostics_and_disclose_omissions() {
        let render = |reverse: bool| {
            let mut errors = TeardownErrors::default();
            errors.record("primary", MezError::forbidden("PRIMARY"));
            let mut sequence = (0..20).collect::<Vec<_>>();
            if reverse {
                sequence.reverse();
            }
            for n in sequence {
                errors.record(
                    &format!("stage-{n:02}"),
                    MezError::invalid_state("🦀".repeat(2048)),
                );
            }
            errors.finish().unwrap_err().message().to_string()
        };
        let message = render(false);
        assert_eq!(message, render(true));
        assert!(message.len() < 10_000);
        assert!(message.contains("12 additional teardown failures omitted"));
        assert_eq!(message.matches("[truncated]").count(), 8);
        assert!(TeardownErrors::default().finish().is_ok());
    }
}
