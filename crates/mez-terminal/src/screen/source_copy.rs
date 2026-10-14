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

    /// Attaches one logical source-group plan after the corresponding producer
    /// feed. Rejects incoherent provenance, wrong owners or mixed entries before
    /// mutating any row. An older cropped portion need not exist, but every
    /// retained target must have this exact entry ID and original occurrence.
    pub fn set_recent_normal_copy_source_associations(
        &mut self,
        owner: &PresentationEntry,
        logical_sources: &[Option<Arc<TerminalCopySource>>],
    ) -> bool {
        if logical_sources.is_empty() {
            return true;
        }
        if !owner.is_valid() || self.alternate.active() {
            return false;
        }
        if logical_sources
            .iter()
            .flatten()
            .any(|source| source.id.owner() != owner.id)
        {
            return false;
        }
        let matches = |entry: Option<&PresentationEntry>| {
            entry.is_some_and(|entry| entry.id == owner.id && entry.occurrence == owner.occurrence)
        };
        let mut target_end = self.normal_physical_line_count();
        while target_end > 0 {
            let last = target_end - 1;
            if !self.normal_physical_line_is_blank(last)
                || self.normal_physical_line_wraps_to_next(last)
                || matches(self.normal_copy_source_entry(last))
            {
                break;
            }
            target_end = last;
        }
        let mut assignments = Vec::new();
        // A captured composite may contain later independently owned entries.
        // Locate this exact producer from the bounded retained tail; every
        // selected target below is still checked before the atomic assignment.
        while target_end > 0 && !matches(self.normal_copy_source_entry(target_end - 1)) {
            target_end -= 1;
        }
        if target_end == 0 {
            return false;
        }
        for source in logical_sources.iter().rev() {
            if target_end == 0 {
                break;
            }
            let mut start = target_end - 1;
            while start > 0 && self.normal_physical_line_wraps_to_next(start - 1) {
                start -= 1;
            }
            for offset in start..target_end {
                if !matches(self.normal_copy_source_entry(offset)) {
                    return false;
                }
                let Some(index) = self.normal_physical_line_index(offset) else {
                    return false;
                };
                assignments.push((index, source.clone()));
            }
            target_end = start;
        }
        for (index, source) in assignments {
            match index {
                NormalPhysicalLineIndex::History(row) => self.history.set_copy_source(row, source),
                NormalPhysicalLineIndex::Visible(row) => self.line_copy_sources[row] = source,
            }
        }
        self.mark_render_changed();
        true
    }

    /// Resolves existing annotation without cloning payloads or scanning history.
    fn normal_copy_source_entry(&self, offset: usize) -> Option<&PresentationEntry> {
        match self.normal_physical_line_index(offset)? {
            NormalPhysicalLineIndex::History(row) => self.history.presentation_at(row),
            NormalPhysicalLineIndex::Visible(row) => self.line_presentations.get(row)?.as_ref(),
        }
    }
}
