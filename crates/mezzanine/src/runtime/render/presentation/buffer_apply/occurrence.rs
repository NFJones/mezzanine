//! Actor-local occurrence capture and bounded explicit provenance overrides.
//!
//! Capture happens at a semantic producer boundary, never in worker rendering,
//! persistence, or geometry reconstruction. Overrides transport an already
//! captured entry (including explicit unknown) through existing appenders and
//! restore their predecessor on every ordinary success/error path. They hold
//! no event history and cannot confer execution or delivery authority.

use super::*;
use mez_core::presentation::{
    PresentationEntry, PresentationOccurrence, PresentationOccurrenceBasis,
};

impl RuntimeSessionService {
    /// Validates all owned logical-row ranges before any screen mutation, then
    /// feeds identical bytes with paired entry/source scope. Gaps remain raw
    /// separators, and no source plan performs a retained-history owner search.
    pub(in crate::runtime::render::presentation) fn feed_presentation_source_ranges(
        screen: &mut TerminalScreen,
        bytes: &[u8],
        ranges: &[(
            std::ops::Range<usize>,
            PresentationEntry,
            Option<std::sync::Arc<mez_terminal::TerminalCopySource>>,
        )],
        context: &str,
    ) -> Result<()> {
        let text = std::str::from_utf8(bytes).map_err(|_| {
            MezError::invalid_state("owned presentation source bytes are not UTF-8")
        })?;
        let mut offset = 0;
        for (range, entry, source) in ranges {
            if range.start < offset
                || range.end < range.start
                || range.end > bytes.len()
                || !text.is_char_boundary(range.start)
                || !text.is_char_boundary(range.end)
                || !entry.is_valid()
                || source
                    .as_ref()
                    .is_some_and(|source| source.id.owner() != entry.id)
            {
                return Err(MezError::invalid_state(
                    "invalid complete producer source range plan",
                ));
            }
            offset = range.end;
        }
        offset = 0;
        for (range, entry, source) in ranges {
            Self::feed_agent_terminal_screen(screen, &bytes[offset..range.start], context)?;
            let mut accepted = false;
            catch_agent_terminal_presentation_panic(context, || {
                accepted = screen.feed_presentation_with_copy_source(
                    &bytes[range.clone()],
                    entry.clone(),
                    source.clone(),
                );
            })?;
            if !accepted {
                return Err(MezError::invalid_state(
                    "validated producer source scope was rejected",
                ));
            }
            offset = range.end;
        }
        Self::feed_agent_terminal_screen(screen, &bytes[offset..], context)
    }

    /// Publishes already-installed streamed rows with their original source owner.
    /// Missing metadata stays unknown; publication never captures another clock.
    pub(super) fn persist_streamed_presentation(
        &mut self,
        pane: &str,
        entry: Option<PresentationEntry>,
        rows: (Vec<String>, Vec<String>, Vec<String>),
        source: Option<(&str, &str)>,
    ) {
        let previous = self.presentation.occurrence_override.take();
        self.presentation.occurrence_override =
            entry.map(|entry| Box::new((pane.to_string(), entry)));
        self.persist_agent_presentation_entry(pane, rows.0, rows.1, rows.2, String::new(), source);
        self.presentation.occurrence_override = previous;
    }

    /// Feeds producer-delimited byte ranges without changing bytes or sampling.
    /// Separator ranges remain untagged so a new entry cannot relabel its predecessor.
    pub(in crate::runtime::render::presentation) fn feed_presentation_ranges(
        screen: &mut TerminalScreen,
        bytes: &[u8],
        ranges: &[(std::ops::Range<usize>, PresentationEntry)],
        context: &str,
    ) -> Result<()> {
        let mut offset = 0;
        for (range, entry) in ranges {
            if range.start < offset || range.end < range.start || range.end > bytes.len() {
                return Err(MezError::invalid_state(
                    "invalid producer presentation byte range",
                ));
            }
            Self::feed_agent_terminal_screen(screen, &bytes[offset..range.start], context)?;
            catch_agent_terminal_presentation_panic(context, || {
                screen.feed_presentation(&bytes[range.clone()], entry.clone())
            })?;
            offset = range.end;
        }
        Self::feed_agent_terminal_screen(screen, &bytes[offset..], context)
    }

    /// Samples the injected actor clock once; unrepresentable values stay unknown.
    pub(crate) fn capture_presentation_occurrence(
        &self,
        basis: PresentationOccurrenceBasis,
    ) -> PresentationOccurrence {
        #[cfg(test)]
        if let Some(value) = self.presentation.occurrence_clock_millis {
            return PresentationOccurrence::new(value, basis);
        }
        let value = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => i64::try_from(duration.as_millis()).ok(),
            Err(error) => i64::try_from(error.duration().as_millis())
                .ok()
                .and_then(i64::checked_neg),
        };
        PresentationOccurrence::new(value, basis)
    }

    /// Establishes an independent semantic entry, or transfers its scoped owner.
    pub(in crate::runtime) fn current_presentation_entry(
        &self,
        pane: &str,
    ) -> Option<PresentationEntry> {
        self.presentation
            .occurrence_override
            .as_deref()
            .filter(|(owner, _)| owner == pane)
            .map(|(_, entry)| entry.clone())
    }

    /// Establishes an independent semantic entry, or transfers its scoped owner.
    pub(in crate::runtime) fn presentation_entry(
        &self,
        pane: &str,
        basis: PresentationOccurrenceBasis,
    ) -> PresentationEntry {
        self.presentation
            .occurrence_override
            .as_deref()
            .filter(|(owner, _)| owner == pane)
            .map(|(_, entry)| entry.clone())
            .unwrap_or_else(|| PresentationEntry {
                id: crate::storage::token_usage::new_token_usage_event_id(),
                occurrence: if self
                    .presentation
                    .agent_presentation_replay_panes
                    .contains(pane)
                {
                    PresentationOccurrence::default()
                } else {
                    self.capture_presentation_occurrence(basis)
                },
                first_row: true,
            })
    }

    /// Opaque replay has no producer-proven entry; it must remain untagged.
    pub(super) fn has_presentation_boundary(&self, pane: &str) -> bool {
        if let Some((owner, entry)) = self
            .presentation
            .occurrence_override
            .as_deref()
            .filter(|(owner, _)| owner == pane)
        {
            let _ = owner;
            return entry.is_valid();
        }
        !self
            .presentation
            .agent_presentation_replay_panes
            .contains(pane)
    }

    /// Runs a pane-local projection with immutable supplied provenance.
    pub(in crate::runtime) fn with_presentation_entry<T>(
        &mut self,
        pane: &str,
        entry: PresentationEntry,
        operation: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        let previous = self
            .presentation
            .occurrence_override
            .replace(Box::new((pane.to_string(), entry)));
        let result = operation(self);
        self.presentation.occurrence_override = previous;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wrong source in a later logical range rejects the entire plan before
    /// bytes, cursor, styles or parser state change. Valid adjacent source groups
    /// retain their own row across unscoped CRLF and authored empty rows.
    #[test]
    fn owned_source_feed_plan_is_atomic_and_preserves_separator_ownership() {
        let entry = PresentationEntry {
            id: "owned-source".into(),
            occurrence: PresentationOccurrence::default(),
            first_row: true,
        };
        let foreign = PresentationEntry {
            id: "foreign".into(),
            ..entry.clone()
        };
        let make = |owner: &PresentationEntry, ordinal, text: &str| {
            mez_terminal::TerminalCopySource::new(
                mez_terminal::TerminalCopySourceId::new(
                    owner,
                    mez_terminal::TerminalCopySourceGroup::SourceLine(ordinal),
                )
                .unwrap(),
                text,
            )
        };
        let first = make(&entry, 0, "first");
        let empty = make(&entry, 1, "");
        let last = make(&entry, 2, "last");
        let wrong = make(&foreign, 2, "wrong");
        let bytes = b"first\r\n\r\nlast";
        let mut continuation = entry.clone();
        continuation.first_row = false;
        let mut screen =
            TerminalScreen::new(mez_terminal::TerminalSize::new(24, 6).unwrap(), 16).unwrap();
        let before = screen.clone();
        let bad = [
            (0..5, entry.clone(), Some(first.clone())),
            (9..13, continuation.clone(), Some(wrong)),
        ];
        assert!(
            RuntimeSessionService::feed_presentation_source_ranges(
                &mut screen,
                bytes,
                &bad,
                "invalid later source"
            )
            .is_err()
        );
        assert_eq!(screen, before);
        let good = [
            (0..7, entry.clone(), Some(first.clone())),
            (7..9, continuation.clone(), Some(empty.clone())),
            (9..13, continuation, Some(last.clone())),
        ];
        RuntimeSessionService::feed_presentation_source_ranges(
            &mut screen,
            bytes,
            &good,
            "owned source rows",
        )
        .unwrap();
        let rows = screen.visible_styled_lines();
        assert_eq!(rows[0].copy_source.as_ref().unwrap().id, first.id);
        assert_eq!(rows[1].copy_source.as_ref().unwrap().id, empty.id);
        assert_eq!(rows[2].copy_source.as_ref().unwrap().id, last.id);
        assert!(rows[0].presentation.as_ref().unwrap().first_row);
        assert!(!rows[1].presentation.as_ref().unwrap().first_row);
        assert!(!rows[2].presentation.as_ref().unwrap().first_row);
    }

    /// The temporary override must occupy one pointer, not a full entry in every
    /// actor/future frame. The ordinary-stack MCP cancellation regression caught
    /// a stack overflow when this payload was inline; heap ownership preserves
    /// the same entry/clock semantics without changing test stack configuration.
    #[test]
    fn runtime_occurrence_override_has_bounded_inline_footprint() {
        let owner = crate::runtime::render::RuntimePresentationComponent::default();
        assert_eq!(
            std::mem::size_of_val(&owner.occurrence_override),
            std::mem::size_of::<usize>()
        );
        println!(
            "runtime_service_bytes={} temporary_override_bytes={} unboxed_override_bytes={}",
            std::mem::size_of::<crate::runtime::RuntimeSessionService>(),
            std::mem::size_of_val(&owner.occurrence_override),
            std::mem::size_of::<Option<(String, mez_core::presentation::PresentationEntry)>>()
        );
    }
}
