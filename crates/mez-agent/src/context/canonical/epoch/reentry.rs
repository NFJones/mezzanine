//! Inert handled-source metadata for automatic compaction admission. It neither
//! selects model input nor proves publication, lifecycle completion or execution.
//! Only an actual canonical head carries it through the existing checkpoint.

use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Exact published head and the remaining old closed prefix handled by its
/// original plan. Post-freeze arrivals and protected source are not folded into
/// this fingerprint merely because they coexist in the published candidate.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReplayCompactionReentry {
    revision: u64,
    head: ContextBlockOccurrenceId,
    input: ReplayCompactionReentryInput,
}

/// Fixed-size source/configuration signature, without another event body or
/// membership list. It is descriptive metadata, not source/commit authority.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReplayCompactionReentryInput {
    context_tokens: usize,
    retained_percent: u8,
    projection: Option<[u8; 32]>,
    handled_source: [u8; 32],
}

impl ReplayCompactionReentryInput {
    pub(super) fn capture(
        policy: ReplayTailPolicy,
        projection: Option<&ReplayProviderProjection>,
        old_closed_tail: &[ConversationEvent],
    ) -> Self {
        Self {
            context_tokens: policy.context_tokens,
            retained_percent: policy.retained_percent,
            projection: projection_fingerprint(projection),
            handled_source: source_fingerprint(old_closed_tail),
        }
    }
}

impl ReplayCompactionReentry {
    pub(super) fn new(head: &ReplaySummaryIdentity, input: ReplayCompactionReentryInput) -> Self {
        Self {
            revision: head.revision().get(),
            head: head.occurrence_id(),
            input,
        }
    }

    pub(super) fn validate(&self, owner: &ReplayEpochContext) -> AgentContextResult<()> {
        ReplayTailPolicy::new(self.input.context_tokens, self.input.retained_percent)?;
        if owner.head().is_none_or(|head| {
            head.occurrence_id() != self.head || head.revision().get() != self.revision
        }) {
            return Err(AgentContextError::new(
                "compaction reentry metadata has a foreign head",
            ));
        }
        Ok(())
    }
}

impl ReplayEpochContext {
    /// Reports whether automatic pressure has a changed head, relevant input
    /// configuration, or genuinely fresh eligible closed source. A fresh usage
    /// sample or new protected instruction alone cannot rearm an unchanged head.
    /// Explicit manual compaction bypasses this predicate and uses the same
    /// planner. False authorizes no request or mutation; legacy checkpoints with
    /// no recorded handling remain unknown rather than inventing a receipt.
    pub fn automatic_compaction_is_fresh(
        &self,
        policy: ReplayTailPolicy,
    ) -> AgentContextResult<bool> {
        self.validate_membership()?;
        if policy
            .head_policy
            .is_some_and(|target| target != self.policy)
        {
            return Ok(true);
        }
        let Some(recorded) = &self.compaction_reentry else {
            return Ok(true);
        };
        if recorded.input.context_tokens != policy.context_tokens
            || recorded.input.retained_percent != policy.retained_percent
            || recorded.input.projection != projection_fingerprint(self.provider_projection())
        {
            return Ok(true);
        }
        let raw = &self.context.chronology()[usize::from(self.head.is_some())..];
        let units = planning::execution_units(raw);
        let protected = self.closed_prefix_unit_count(raw, &units);
        let end = units.get(protected).map_or(raw.len(), |unit| unit.start);
        Ok(source_fingerprint(&raw[..end]) != recorded.input.handled_source)
    }
}

/// Hashes cached original occurrence/revision records, not model text. Equal
/// bodies in different occurrences remain distinct; an explicit same-ID content
/// revision also changes the fingerprint without rehashing source on inspection.
fn source_fingerprint(events: &[ConversationEvent]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"mez-compaction-handled-closed-source-v1\0");
    for event in events {
        let record = event.record();
        hash.update(String::from(record.occurrence_id()).as_bytes());
        hash.update(record.rendered_fingerprint());
    }
    hash.finalize().into()
}

fn projection_fingerprint(projection: Option<&ReplayProviderProjection>) -> Option<[u8; 32]> {
    projection.map(|projection| {
        let mut hash = Sha256::new();
        hash.update(b"mez-compaction-provider-projection-v1\0");
        for value in [
            projection.api().as_str(),
            projection.provider_id(),
            projection.model(),
        ] {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
        hash.finalize().into()
    })
}
