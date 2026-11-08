//! Redacted immutable source and content-free occurrence/byte attribution.

use super::*;

/// An authoritative whole ownership unit. Its rendered interval may be split
/// temporarily for inference, but publication still replaces the original plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionSourceUnit {
    occurrences: Vec<ContextBlockOccurrenceId>,
    bytes: Range<usize>,
}

impl CompactionSourceUnit {
    /// Returns actual canonical occurrences, including equal-text repetitions.
    pub fn occurrences(&self) -> &[ContextBlockOccurrenceId] {
        &self.occurrences
    }
    /// Returns this whole unit's interval in the concatenated redacted source.
    pub fn bytes(&self) -> Range<usize> {
        self.bytes.clone()
    }
}

/// Rendered once from a real canonical plan. The renderer supplies complete
/// read-only framing and redacts BEFORE any packing or temporary fragmentation.
/// Stable instructions, retained tail and post-frontier arrivals are not passed
/// to the callback; model-visible rendering never creates execution authority.
pub struct FrozenEpochCompactionSource {
    pub(super) plan: ReplayCompactionPlan,
    pub(super) text: Arc<str>,
    pub(super) units: Vec<CompactionSourceUnit>,
}

impl FrozenEpochCompactionSource {
    /// Renders each ownership unit in chronological order, preserving every
    /// returned byte. The callback must include any separators in its output;
    /// the engine never trims, truncates, redacts again or invents delimiters.
    /// Empty unit renderings reject instead of silently losing provenance.
    /// `max_bytes` is the existing resource owner's held-source memory bound.
    pub fn render(
        plan: ReplayCompactionPlan,
        max_bytes: usize,
        mut render_redacted: impl FnMut(&[ConversationEvent]) -> AgentContextResult<String>,
    ) -> AgentContextResult<Self> {
        if !plan.requires_model_summary() {
            return Err(invalid("summary-free source cannot dispatch model work"));
        }
        let mut text = String::new();
        let mut units = Vec::new();
        for events in plan.summary_units() {
            let rendered = render_redacted(events)?;
            let end = text
                .len()
                .checked_add(rendered.len())
                .filter(|end| *end <= max_bytes)
                .ok_or_else(|| invalid("epoch compaction source memory bound exceeded"))?;
            if rendered.is_empty() {
                return Err(invalid("epoch compaction source unit rendered empty"));
            }
            units.push(CompactionSourceUnit {
                occurrences: events
                    .iter()
                    .map(|event| event.record().occurrence_id())
                    .collect(),
                bytes: text.len()..end,
            });
            text.push_str(&rendered);
        }
        if units.is_empty() {
            return Err(invalid("epoch compaction source has no selected units"));
        }
        Ok(Self {
            plan,
            text: text.into(),
            units,
        })
    }

    /// Returns immutable original whole-unit attribution without source text.
    pub fn units(&self) -> &[CompactionSourceUnit] {
        &self.units
    }

    /// Returns the exact canonical source/parent and frozen provider view.
    pub fn plan(&self) -> &ReplayCompactionPlan {
        &self.plan
    }

    /// Returns the exact concatenation passed to inference before fragmentation.
    pub fn text(&self) -> &str {
        &self.text
    }
}
