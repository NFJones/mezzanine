//! Bounded assignment of immutable producer-owned source groups to existing rows.
//!
//! This owner performs no product marker parsing. Callers supply an aligned
//! logical-row plan and original entry evidence; all retained targets are
//! validated before any assignment. Cropping may remove older rows but never
//! changes original first-row proof. History owns associations on its existing
//! records, with payload lifetimes governed solely by retained row references.

use super::*;

impl TerminalScreen {
    /// Reports whether this screen's proven copy associations are consumed by
    /// modern framed copy views. Capture itself is independent of this mode;
    /// never-enabled legacy OFF views retain their existing copy-byte contract.
    pub fn uses_presentation_copy_associations(&self) -> bool {
        self.preserve_presentation_copy_associations
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A typed A -> conflicting typed B -> untyped continuation is one logical
    /// wrapped chain. Once invalidated, neither source association nor legacy
    /// copy override may revive on a later row; displayed cells remain intact.
    #[test]
    fn copy_source_conflict_latch_stays_invalid_until_new_logical_line() {
        let row = |id: &str, text: &str, legacy: &str, typed: bool, wraps| {
            let entry = PresentationEntry {
                id: id.into(),
                occurrence: Default::default(),
                first_row: true,
            };
            let mut line = TerminalStyledLine::plain(text);
            line.copy_text = Some(legacy.into());
            if typed {
                line.copy_source = Some(crate::TerminalCopySource::new(
                    crate::TerminalCopySourceId::new(
                        &entry,
                        crate::TerminalCopySourceGroup::WholeEntry,
                    )
                    .unwrap(),
                    legacy,
                ));
                line.presentation = Some(entry);
            }
            PhysicalStyledLine {
                line,
                wraps_to_next: wraps,
            }
        };
        let rows = [
            row("A", "A", "source A", true, true),
            row("B", "B", "source B", true, true),
            row("unused", "C", "resurrected legacy", false, false),
        ];
        let logical = merge_wrapped_physical_lines(&rows, None, terminal_emoji_width(), true);
        assert_eq!(logical[0].text, "ABC");
        assert!(logical[0].presentation.is_none());
        assert!(logical[0].copy_source.is_none());
        assert!(
            logical[0].copy_text.is_none(),
            "a conflict must not revive legacy source recovery"
        );
    }
}
