//! Atomic expected-parent publication of one head plus exact retained source.
//! A usable summary is accepted independently of advisory word/shrink targets.

use super::*;

impl ReplayEpochContext {
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
        let offset = usize::from(self.head.is_some());
        let selected = &self.context.chronology()[offset..plan.prefix_events];
        let group_crosses_frontier = {
            let retained = self.context.chronology()[plan.prefix_events..]
                .iter()
                .filter_map(ConversationEvent::execution_group_id)
                .collect::<BTreeSet<_>>();
            selected
                .iter()
                .filter_map(ConversationEvent::execution_group_id)
                .any(|group| retained.contains(group))
        };
        if selected
            .iter()
            .any(|event| !self.event_is_replaceable(event))
            || group_crosses_frontier
        {
            return Err(AgentContextError::new(
                "replay publication source became protected or its execution group changed",
            ));
        }
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
            });
        } else {
            candidate.context.chronology.drain(..plan.prefix_events);
            candidate.context.rebuild_projections();
            candidate.context.validate_durable().map_err(|_| {
                AgentContextError::new("replay omission adoption failed canonical validation")
            })?;
            candidate.head = None;
        }
        candidate.members.drain(..plan.prefix_members);
        candidate.revision = revision;
        candidate.validate_membership()?;
        *self = candidate;
        Ok(revision)
    }
}
