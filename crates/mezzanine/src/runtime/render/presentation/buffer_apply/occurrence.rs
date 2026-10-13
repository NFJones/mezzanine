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
