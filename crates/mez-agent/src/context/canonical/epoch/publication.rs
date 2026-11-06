//! Atomic expected-parent publication of one head plus exact retained source.
//! A usable summary is accepted independently of advisory word/shrink targets.

use super::*;

/// Immutable output of one actual source selection and summary acceptance.
/// Clones retain the original live incarnation, exact head record and source;
/// this value grants no storage acknowledgment or execution authority.
#[derive(Clone, Debug)]
pub struct ReplayPreparedCompaction {
    source: ReplayEpochContext,
    candidate: ReplayEpochContext,
    plan: ReplayCompactionPlan,
}

impl ReplayPreparedCompaction {
    /// Borrows the original live source, including private lifecycle witnesses.
    pub fn source(&self) -> &ReplayEpochContext {
        &self.source
    }

    /// Borrows the exact frozen output to persist, without granting mutation.
    pub fn candidate(&self) -> &ReplayEpochContext {
        &self.candidate
    }
}

impl ReplayEpochContext {
    /// Accepts summary output once without changing the live source. The
    /// returned proposal freezes the allocated head and can cross a storage
    /// worker boundary. It is not evidence that any bytes are durable.
    pub fn prepare_compaction_publication(
        &self,
        plan: ReplayCompactionPlan,
        publication: ReplayHeadPublication,
    ) -> AgentContextResult<ReplayPreparedCompaction> {
        let mut candidate = self.clone();
        candidate.publish_compaction(plan.clone(), publication)?;
        Ok(ReplayPreparedCompaction {
            source: self.clone(),
            candidate,
            plan,
        })
    }

    /// Adopts the exact prepared head into its original live owner, preserving
    /// current policy slots, request metadata, lifecycle handles and arrivals.
    /// The product must separately qualify storage acknowledgment. A foreign
    /// incarnation, changed parent/source or newly protected selection rejects
    /// atomically. Repeating the same adoption validates the current extension
    /// and preserves subsequent work rather than replacing it with old bytes.
    pub fn adopt_compaction_publication(
        &mut self,
        prepared: &ReplayPreparedCompaction,
    ) -> AgentContextResult<ReplayEpochRevision> {
        if self.owner_identity != prepared.source.owner_identity {
            return Err(AgentContextError::new(
                "prepared replay publication belongs to a different live owner",
            ));
        }
        if self.revision == prepared.candidate.revision && self.head == prepared.candidate.head {
            self.validate_source_extension(&prepared.candidate)?;
            return Ok(self.revision);
        }
        self.validate_source_extension(&prepared.source)?;
        let plan = &prepared.plan;
        self.validate_publication_selection(plan)?;
        let mut candidate = self.clone();
        let head_events = usize::from(prepared.candidate.head.is_some());
        for member in &self.members[prepared.source.members.len()..] {
            candidate
                .source_lifecycle
                .carried_arrivals
                .insert(member.occurrence_id, member.clone());
        }
        candidate.context.chronology.splice(
            ..plan.prefix_events,
            prepared.candidate.context.chronology()[..head_events]
                .iter()
                .cloned(),
        );
        candidate.context.rebuild_projections();
        candidate.head = prepared.candidate.head.clone();
        candidate.publish_accounting(plan)?;
        candidate.members.drain(..plan.prefix_members);
        candidate.revision = prepared.candidate.revision;
        candidate.revoke_retrieved_mcp_grants();
        candidate.prune_source_lifecycle();
        candidate.validate_membership()?;
        *self = candidate;
        Ok(self.revision)
    }

    /// Adopts a completed publication against its exact frozen parent/source.
    /// Append-only arrivals survive with their original IDs and birth revisions.
    /// Stale parents, newly protected source, crossing execution groups, policy
    /// mismatch or unusable summary reject without changing any state.
    pub fn publish_compaction(
        &mut self,
        plan: ReplayCompactionPlan,
        publication: ReplayHeadPublication,
    ) -> AgentContextResult<ReplayEpochRevision> {
        self.validate_membership()?;
        if plan.owner_identity != self.owner_identity
            || plan.conversation != self.conversation
            || plan.parent != self.revision
            || plan.head != self.head
            || plan.policy != self.policy
            || plan.provider_projection.as_deref() != self.provider_projection()
            || !self.context.chronology().starts_with(&plan.frozen)
            || !self.members.starts_with(&plan.members)
            || plan.prefix_members == 0
            || plan.prefix_members > plan.members.len()
            || plan.prefix_events > plan.frozen.len()
        {
            return Err(AgentContextError::new(
                "replay publication has a stale parent or frozen source",
            ));
        }
        self.validate_publication_selection(&plan)?;
        let summary = match (self.policy, publication) {
            (ReplayHeadPolicy::Summary, ReplayHeadPublication::Summary(text)) => {
                crate::context_compaction::validate_model_compaction_summary(
                    &text,
                    usize::MAX,
                    false,
                )
                .map_err(|_| {
                    AgentContextError::new(
                        "replay summary must be completed, nonempty and resource-bounded",
                    )
                })?;
                Some(text)
            }
            (ReplayHeadPolicy::IntentionalOmission, ReplayHeadPublication::IntentionalOmission) => {
                None
            }
            _ => {
                return Err(AgentContextError::new(
                    "replay head publication does not match its explicit policy",
                ));
            }
        };
        let revision = ReplayEpochRevision(
            self.revision
                .0
                .checked_add(1)
                .ok_or_else(|| AgentContextError::new("replay epoch revision exhausted"))?,
        );
        let mut candidate = self.clone();
        if let Some(text) = summary {
            candidate
                .context
                .compact_execution_ranges_into_summary(
                    std::iter::once(0..plan.prefix_events).collect(),
                    ContextBlock::reference_event(
                        ContextSourceKind::CompactionPrefix,
                        "conversation epoch summary",
                        text,
                    ),
                )
                .map_err(|_| {
                    AgentContextError::new("replay summary adoption failed canonical validation")
                })?;
            candidate.head = Some(ReplaySummaryIdentity {
                conversation: self.conversation.clone(),
                revision,
                occurrence_id: candidate
                    .context
                    .chronology()
                    .first()
                    .ok_or_else(|| AgentContextError::new("published summary is missing"))?
                    .record()
                    .occurrence_id(),
            });
        } else {
            candidate.context.chronology.drain(..plan.prefix_events);
            candidate.context.rebuild_projections();
            candidate.context.validate_durable().map_err(|_| {
                AgentContextError::new("replay omission adoption failed canonical validation")
            })?;
            candidate.head = None;
        }
        candidate.publish_accounting(&plan)?;
        candidate.members.drain(..plan.prefix_members);
        candidate.revision = revision;
        candidate.revoke_retrieved_mcp_grants();
        candidate.prune_source_lifecycle();
        candidate.validate_membership()?;
        *self = candidate;
        Ok(revision)
    }

    /// Rechecks the selected prefix against protection and group membership
    /// at acceptance/adoption. The source check must establish prefix bounds.
    fn validate_publication_selection(
        &self,
        plan: &ReplayCompactionPlan,
    ) -> AgentContextResult<()> {
        let offset = usize::from(self.head.is_some());
        let selected = &self.context.chronology()[offset..plan.prefix_events];
        let retained_groups = self.context.chronology()[plan.prefix_events..]
            .iter()
            .filter_map(ConversationEvent::execution_group_id)
            .collect::<BTreeSet<_>>();
        if selected.iter().any(|event| {
            !self.event_is_replaceable(event)
                || event
                    .execution_group_id()
                    .is_some_and(|group| retained_groups.contains(group))
        }) {
            return Err(AgentContextError::new(
                "replay publication source became protected or its execution group changed",
            ));
        }
        Ok(())
    }
}
