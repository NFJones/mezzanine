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
        match plan.selection.as_ref() {
            ReplayCompactionSelection::Prefix { events, .. } => {
                candidate.context.chronology.splice(
                    ..*events,
                    prepared.candidate.context.chronology()[..head_events]
                        .iter()
                        .cloned(),
                );
            }
            ReplayCompactionSelection::RawTail { omitted, .. } => {
                let previous_head = plan.head.as_ref().map(ReplaySummaryIdentity::occurrence_id);
                candidate.context.chronology.retain(|event| {
                    let id = event.record().occurrence_id();
                    Some(id) != previous_head && !omitted.contains(&id)
                });
            }
        }
        candidate.context.rebuild_projections();
        candidate.head = prepared.candidate.head.clone();
        candidate.policy = prepared.candidate.policy;
        candidate.publish_accounting(plan)?;
        candidate.retire_compaction_members(plan);
        candidate.revision = prepared.candidate.revision;
        candidate.compaction_reentry = prepared.candidate.compaction_reentry.clone();
        candidate.bind_mcp_retrieval_scope();
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
        self.validate_compaction_source(&plan)?;
        self.publish_validated_compaction(plan, publication)
    }

    /// Rechecks frozen selection and actual live protection without allocating
    /// a replacement head. Actor admission and bounded output correction can
    /// refuse stale work before dispatching a persistence worker.
    pub fn validate_compaction_source(
        &self,
        plan: &ReplayCompactionPlan,
    ) -> AgentContextResult<()> {
        self.validate_membership()?;
        if plan.owner_identity != self.owner_identity
            || plan.conversation != self.conversation
            || plan.parent != self.revision
            || plan.head != self.head
            || plan.source_policy != self.policy
            || plan.provider_projection.as_deref() != self.provider_projection()
            || !self.context.chronology().starts_with(&plan.frozen)
            || !self.members.starts_with(&plan.members)
            || plan.frozen.len() != plan.members.len() + usize::from(plan.head.is_some())
            || !plan.selection_is_valid()
        {
            return Err(AgentContextError::new(
                "replay publication has a stale parent or frozen source",
            ));
        }
        self.validate_publication_selection(plan)
    }

    /// Publication follows the same checked source contract as actor preflight.
    fn publish_validated_compaction(
        &mut self,
        plan: ReplayCompactionPlan,
        publication: ReplayHeadPublication,
    ) -> AgentContextResult<ReplayEpochRevision> {
        let summary = match (plan.policy, publication) {
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
            let ReplayCompactionSelection::Prefix { events, .. } = plan.selection.as_ref() else {
                return Err(AgentContextError::new(
                    "model summary requires a prefix selection",
                ));
            };
            candidate
                .context
                .compact_execution_ranges_into_summary(
                    std::iter::once(0..*events).collect(),
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
            let previous_head = plan.head.as_ref().map(ReplaySummaryIdentity::occurrence_id);
            let ReplayCompactionSelection::RawTail { omitted, .. } = plan.selection.as_ref() else {
                return Err(AgentContextError::new(
                    "intentional omission requires a raw selection",
                ));
            };
            candidate.context.chronology.retain(|event| {
                let id = event.record().occurrence_id();
                Some(id) != previous_head && !omitted.contains(&id)
            });
            candidate.context.rebuild_projections();
            candidate.context.validate_durable().map_err(|_| {
                AgentContextError::new("replay omission adoption failed canonical validation")
            })?;
            candidate.head = None;
        }
        candidate.policy = plan.policy;
        candidate.publish_accounting(&plan)?;
        candidate.retire_compaction_members(&plan);
        candidate.revision = revision;
        candidate.compaction_reentry = candidate
            .head
            .as_ref()
            .map(|head| ReplayCompactionReentry::new(head, *plan.reentry.clone()));
        candidate.bind_mcp_retrieval_scope();
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
        let retained_groups = self
            .context
            .chronology()
            .iter()
            .enumerate()
            .filter(|(index, _)| !plan.removes_event(*index))
            .filter_map(|(_, event)| event.execution_group_id())
            .collect::<BTreeSet<_>>();
        if self
            .context
            .chronology()
            .iter()
            .enumerate()
            .skip(offset)
            .filter(|(index, _)| plan.removes_event(*index))
            .any(|(_, event)| {
                !self.event_is_replaceable(event)
                    || event
                        .execution_group_id()
                        .is_some_and(|group| retained_groups.contains(group))
            })
        {
            return Err(AgentContextError::new(
                "replay publication source became protected or its execution group changed",
            ));
        }
        Ok(())
    }

    /// Retires only the plan's selected original membership after accounting
    /// has removed those exact entries. Append-only arrivals remain untouched.
    fn retire_compaction_members(&mut self, plan: &ReplayCompactionPlan) {
        match plan.selection.as_ref() {
            ReplayCompactionSelection::Prefix { members, .. } => {
                self.members.drain(..*members);
            }
            ReplayCompactionSelection::RawTail { omitted, .. } => {
                self.members
                    .retain(|member| !omitted.contains(&member.occurrence_id));
            }
        }
    }
}

impl ReplayCompactionPlan {
    /// Checks the selection's policy, source bounds and actual original IDs.
    /// Plans are minted locally, never reconstructed from descriptive metadata.
    fn selection_is_valid(&self) -> bool {
        match self.selection.as_ref() {
            ReplayCompactionSelection::Prefix { members, events } => {
                self.policy == ReplayHeadPolicy::Summary
                    && *members > 0
                    && *members <= self.members.len()
                    && *events == members + usize::from(self.head.is_some())
                    && *events <= self.frozen.len()
            }
            ReplayCompactionSelection::RawTail {
                omitted,
                tail_start,
                ..
            } => {
                let members = self
                    .members
                    .iter()
                    .map(ReplayEpochMember::occurrence_id)
                    .collect::<BTreeSet<_>>();
                self.policy == ReplayHeadPolicy::IntentionalOmission
                    && (!omitted.is_empty() || self.head.is_some())
                    && *tail_start <= self.frozen.len()
                    && omitted.is_subset(&members)
            }
        }
    }
}
