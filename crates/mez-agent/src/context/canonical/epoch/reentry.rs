//! Inert handled-source metadata for automatic compaction admission. It neither
//! selects model input nor proves publication, lifecycle completion or execution.
//! Only an actual canonical head carries it through the existing checkpoint.

use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Exact published head and the remaining old eligible units handled by its
/// original plan. Post-freeze arrivals and protected source are not folded into
/// this fingerprint merely because they coexist in the published candidate.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReplayCompactionReentry {
    revision: u64,
    head: ContextBlockOccurrenceId,
    input: ReplayCompactionReentryInput,
}

/// Descriptive source/configuration evidence, never source/commit authority.
/// Occurrence revisions are bounded by the retained raw membership, without
/// another event body or lifetime ledger. The scalar hash preserves v4 encoding.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReplayCompactionReentryInput {
    context_tokens: usize,
    retained_percent: u8,
    projection: Option<[u8; 32]>,
    handled_source: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    handled_members: Option<BTreeMap<ContextBlockOccurrenceId, [u8; 32]>>,
}

impl ReplayCompactionReentryInput {
    pub(super) fn capture<'a>(
        policy: ReplayTailPolicy,
        projection: Option<&ReplayProviderProjection>,
        old_closed_tail: impl IntoIterator<Item = &'a ConversationEvent>,
    ) -> Self {
        let events = old_closed_tail.into_iter().collect::<Vec<_>>();
        Self {
            context_tokens: policy.context_tokens,
            retained_percent: policy.retained_percent,
            projection: projection_fingerprint(projection),
            handled_source: source_fingerprint(events.iter().copied()),
            handled_members: Some(
                events
                    .into_iter()
                    .map(|event| {
                        (
                            event.record().occurrence_id(),
                            event.record().rendered_fingerprint(),
                        )
                    })
                    .collect(),
            ),
        }
    }
}

impl ReplayCompactionReentry {
    pub(super) fn has_member_evidence(&self) -> bool {
        self.input.handled_members.is_some()
    }

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
        }) || self.input.handled_members.as_ref().is_some_and(|handled| {
            let members = owner
                .members
                .iter()
                .map(ReplayEpochMember::occurrence_id)
                .collect::<BTreeSet<_>>();
            handled.len() > members.len() || handled.keys().any(|id| !members.contains(id))
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
        let mut eligible = units
            .into_iter()
            .filter(|unit| {
                raw[unit.clone()]
                    .iter()
                    .all(|event| self.event_is_replaceable(event))
            })
            .flat_map(|unit| raw[unit].iter());
        if let Some(handled) = &recorded.input.handled_members {
            // Losing eligibility is not new work. Only a currently eligible
            // occurrence absent from the frozen handling evidence, or revised
            // since then, rearms automatic admission. Genuine group closure
            // exposes its previously unhandled originals through this rule.
            return Ok(eligible.any(|event| {
                handled.get(&event.record().occurrence_id())
                    != Some(&event.record().rendered_fingerprint())
            }));
        }
        // Original v4 checkpoints have only an aggregate prefix signature.
        // Preserve its encoding and suppression when equal; uncertain changed
        // source may rearm once, then a real publication records member evidence.
        Ok(source_fingerprint(eligible) != recorded.input.handled_source)
    }
}

/// Hashes cached original occurrence/revision records, not model text. Equal
/// bodies in different occurrences remain distinct; an explicit same-ID content
/// revision also changes the fingerprint without rehashing source on inspection.
fn source_fingerprint<'a>(events: impl IntoIterator<Item = &'a ConversationEvent>) -> [u8; 32] {
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
