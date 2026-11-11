//! Actor-captured accounting membership across the existing preparation lane.
//!
//! No SQLite or transcript I/O occurs on the actor. A retained entry owns the
//! original registration proposal and native incarnation across worker failure;
//! stale results cannot activate replacement bindings. These accounting records
//! grant no cleanup, MMP, provider or policy authority. Production activation
//! remains disabled until workflow-specific creation, restored-binding retirement
//! and checkpoint owners are connected; repository attachment alone cannot turn
//! these incomplete ports into an operational budget feature.

use std::collections::BTreeMap;
use std::sync::Arc;

use mez_agent::token_budget::BudgetRootId;
use mez_agent::token_budget::{BudgetCheckpointReference, BudgetReceiptId};
use mez_agent::{AgentConversationKind, AgentTurnRecord};

use crate::error::{MezError, Result};
use crate::runtime::RuntimeSessionService;
use crate::storage::token_usage::TokenUsageStore;
use crate::storage::token_usage::budget::membership::BudgetMembership;
use crate::storage::transcript::{AgentTranscriptStore, BudgetBindingLease};

/// One runtime-selected source, never inferred from a current depth-zero label.
#[derive(Debug, Clone)]
enum Source {
    /// Only the actual direct-user root path may create an independent account.
    Independent {
        root: BudgetRootId,
        operation: String,
        started_at: u64,
        legacy_gap: bool,
    },
    /// Original active parent membership frozen before child preparation.
    Inherited {
        parent: BudgetMembership,
        operation: String,
        started_at: u64,
    },
    /// No creation permission: durable original membership must already exist.
    Existing,
}

#[derive(Debug)]
struct Entry {
    identity: Arc<()>,
    preparation: Arc<()>,
    ready: bool,
    store: TokenUsageStore,
    transcripts: AgentTranscriptStore,
    pane: String,
    native: mez_agent::messaging::MessageNativeIncarnation,
    owner: String,
    source: Source,
    member: Option<BudgetMembership>,
    lease: Option<BudgetBindingLease>,
}

/// Bounded retained ownership, not a second work queue or scheduler.
#[derive(Debug, Default)]
pub(crate) struct RuntimeBudgetMemberships {
    /// Assembly gate, not user policy. The unfinished lifecycle must not change
    /// ordinary production resume/fork or pretend to enforce finite limits.
    enabled: bool,
    entries: BTreeMap<String, Entry>,
    #[cfg(test)]
    fail_restore_after_install: bool,
}

/// Immutable worker snapshot; the actor retains its original entry throughout I/O.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBudgetMembershipWork {
    store: TokenUsageStore,
    transcripts: AgentTranscriptStore,
    conversation: String,
    pane: String,
    native: mez_agent::messaging::MessageNativeIncarnation,
    identity: Arc<()>,
    preparation: Arc<()>,
    owner: String,
    source: Source,
    lease: Option<BudgetBindingLease>,
}

/// Exact returned source plus its publication outcome, never an inferred ACK.
pub(crate) struct RuntimeBudgetMembershipOutcome {
    work: RuntimeBudgetMembershipWork,
    result: Result<(BudgetMembership, BudgetBindingLease)>,
}

/// Restore preflight retains the original stores and acquired kernel holder
/// through all fallible hydration. Dropping a refused proposal only releases
/// that holder; no saved checkpoint, expense or active pane is rewritten.
pub(crate) struct RuntimeBudgetRestore {
    store: TokenUsageStore,
    transcripts: AgentTranscriptStore,
    member: BudgetMembership,
    owner: String,
    lease: BudgetBindingLease,
}

impl RuntimeBudgetMembershipWork {
    /// Runs only on the existing supervised external-preparation worker.
    pub(crate) fn execute(self) -> RuntimeBudgetMembershipOutcome {
        let result = self.publish();
        RuntimeBudgetMembershipOutcome { work: self, result }
    }

    fn publish(&self) -> Result<(BudgetMembership, BudgetBindingLease)> {
        let namespace = self.store.budget_namespace()?;
        let existing = self
            .transcripts
            .conversation_budget_membership(&self.conversation)?;
        let member = if matches!(&self.source, Source::Existing) {
            let member = existing.ok_or_else(|| {
                MezError::invalid_state("original budget membership requires explicit resolution")
            })?;
            if member.namespace != namespace {
                return Err(MezError::conflict("runtime budget namespace changed"));
            }
            member
        } else {
            let (root, parent, operation, started_at, legacy_coverage_gap) = match &self.source {
                Source::Independent {
                    root,
                    operation,
                    started_at,
                    legacy_gap,
                } => (
                    root.clone(),
                    None,
                    operation.clone(),
                    *started_at,
                    *legacy_gap,
                ),
                Source::Inherited {
                    parent,
                    operation,
                    started_at,
                } => {
                    if parent.namespace != namespace {
                        return Err(MezError::conflict(
                            "runtime budget parent namespace changed",
                        ));
                    }
                    (
                        parent.root.clone(),
                        Some(parent.conversation.clone()),
                        operation.clone(),
                        *started_at,
                        parent.legacy_coverage_gap,
                    )
                }
                Source::Existing => {
                    return Err(MezError::invalid_state(
                        "original budget membership requires explicit resolution",
                    ));
                }
            };
            let member = BudgetMembership {
                namespace,
                root,
                conversation: self.conversation.clone(),
                parent,
                operation,
                started_at,
                legacy_coverage_gap,
            };
            if existing
                .as_ref()
                .is_some_and(|original| original != &member)
            {
                return Err(MezError::conflict(
                    "budget publication conflicts with captured proposal",
                ));
            }
            member
        };
        let lease = self.store.publish_native_budget_membership(
            &self.transcripts,
            &member,
            &self.owner,
            self.lease.as_ref(),
        )?;
        Ok((member, lease))
    }
}

impl RuntimeSessionService {
    /// Captures immutable work without disk I/O or changing the selected root on
    /// retry. Disabled assembly preserves existing behavior. Once enabled,
    /// either missing repository is unavailable accounting, never zero expense.
    pub(crate) fn prepare_runtime_budget_membership(
        &mut self,
        turn_id: &str,
    ) -> Result<Option<RuntimeBudgetMembershipWork>> {
        if !self.agent.budget_memberships.enabled {
            return Ok(None);
        }
        let turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("budget preparation turn unavailable"))?;
        // Starting a new validation attempt revokes any prior acknowledgement,
        // including when the checks below refuse before new work can be issued.
        // Retain the original ownership proposal, not the old readiness receipt.
        if let Some(entry) = self
            .agent
            .budget_memberships
            .entries
            .get_mut(&turn.conversation_id)
        {
            entry.preparation = Arc::new(());
            entry.ready = false;
        }
        let store = self
            .persistence
            .cloned_token_usage_store()
            .ok_or_else(|| MezError::invalid_state("budget accounting repository unavailable"))?;
        let transcripts = self
            .persistence
            .cloned_transcript_store()
            .ok_or_else(|| MezError::invalid_state("budget transcript repository unavailable"))?;
        let session = self
            .agent_shell_store()
            .get(&turn.pane_id)
            .filter(|session| session.session_id == turn.conversation_id)
            .cloned()
            .ok_or_else(|| MezError::conflict("budget preparation conversation changed"))?;
        if !self
            .agent
            .budget_memberships
            .entries
            .contains_key(&turn.conversation_id)
        {
            if self.agent.budget_memberships.entries.len() >= 4096 {
                return Err(MezError::invalid_state(
                    "runtime budget membership capacity reached",
                ));
            }
            let source = if turn.parent_turn_id.is_none()
                && session
                    .budget_conversation_birth
                    .matches(&session.session_id, &session.native_message_incarnation)
                && session.conversation_kind == AgentConversationKind::Root
                && !session.ephemeral
            {
                Source::Independent {
                    root:
                        BudgetRootId::new(crate::storage::token_usage::new_token_usage_event_id())
                            .map_err(|_| {
                            MezError::invalid_state("generated budget root identity invalid")
                        })?,
                    operation: crate::storage::token_usage::new_token_usage_event_id(),
                    started_at: turn.started_at_unix_seconds,
                    legacy_gap: false,
                }
            } else {
                Source::Existing
            };
            self.agent.budget_memberships.entries.insert(
                turn.conversation_id.clone(),
                Entry {
                    identity: Arc::new(()),
                    preparation: Arc::new(()),
                    ready: false,
                    store: store.clone(),
                    transcripts: transcripts.clone(),
                    pane: turn.pane_id.clone(),
                    native: session.native_message_incarnation.clone(),
                    owner: crate::storage::token_usage::new_token_usage_event_id(),
                    source,
                    member: None,
                    lease: None,
                },
            );
        }
        let entry = self
            .agent
            .budget_memberships
            .entries
            .get_mut(&turn.conversation_id)
            .ok_or_else(|| MezError::invalid_state("budget preparation entry unavailable"))?;
        if entry.pane != turn.pane_id || entry.native != session.native_message_incarnation {
            if entry.store != store || entry.transcripts != transcripts {
                return Err(MezError::conflict(
                    "budget repository replacement requires explicit resolution",
                ));
            }
            // The ordinary conversation owner has already rebound this session.
            // Revoke actor custody, not worker custody: issued dispatches and
            // outstanding preparations keep their original Arc-held inode lock.
            // The replacement worker must acquire that SAME lock off actor and
            // load existing provenance; it cannot mint a new root on rebind.
            entry.identity = Arc::new(());
            entry.pane = turn.pane_id.clone();
            entry.native = session.native_message_incarnation.clone();
            entry.owner = crate::storage::token_usage::new_token_usage_event_id();
            entry.source = Source::Existing;
            entry.member = None;
            entry.lease = None;
        }
        if entry.store != store || entry.transcripts != transcripts {
            return Err(MezError::conflict(
                "budget repository replacement requires explicit resolution",
            ));
        }
        entry.preparation = Arc::new(());
        entry.ready = false;
        Ok(Some(RuntimeBudgetMembershipWork {
            store,
            transcripts,
            conversation: turn.conversation_id,
            pane: turn.pane_id,
            native: entry.native.clone(),
            identity: entry.identity.clone(),
            preparation: entry.preparation.clone(),
            owner: entry.owner.clone(),
            source: entry.source.clone(),
            lease: entry.lease.clone(),
        }))
    }

    /// Adopts only the exact entry/native/store captured before the worker began.
    /// A late successful disk write remains original bookkeeping, not permission
    /// to activate a replacement conversation or release its provider request.
    pub(crate) fn apply_runtime_budget_membership(
        &mut self,
        outcome: RuntimeBudgetMembershipOutcome,
    ) -> Result<()> {
        let work = outcome.work;
        let current = self
            .agent_shell_store()
            .get(&work.pane)
            .is_some_and(|session| {
                session.session_id == work.conversation
                    && session.native_message_incarnation == work.native
            });
        let same_store = self.persistence.token_usage_store() == Some(&work.store)
            && self
                .persistence
                .transcript_store()
                .is_some_and(|store| store == &work.transcripts);
        let Some(entry) = self
            .agent
            .budget_memberships
            .entries
            .get_mut(&work.conversation)
        else {
            return Ok(());
        };
        if !current
            || !same_store
            || !Arc::ptr_eq(&entry.identity, &work.identity)
            || !Arc::ptr_eq(&entry.preparation, &work.preparation)
            || entry.owner != work.owner
        {
            return Ok(());
        }
        let (member, lease) = outcome.result?;
        if member.conversation != work.conversation {
            return Err(MezError::conflict(
                "budget preparation outcome subject mismatch",
            ));
        }
        if entry
            .member
            .as_ref()
            .is_some_and(|original| original != &member)
        {
            return Err(MezError::conflict(
                "budget preparation changed original membership",
            ));
        }
        entry.member = Some(member);
        entry.lease = Some(lease);
        entry.ready = false;
        // The original membership/mirror is already durable. In adapter mode
        // this captures a row9 obligation only: enqueue is NOT its durable ACK.
        // Production activation still requires composed persistence admission.
        self.checkpoint_agent_session_metadata()?;
        if let Some(entry) = self
            .agent
            .budget_memberships
            .entries
            .get_mut(&work.conversation)
        {
            entry.ready = true;
        }
        Ok(())
    }

    /// Closest pre-claim check; this does not replace per-send policy/accounting
    /// revision admission supplied by the downstream provider-attempt owner.
    pub(crate) fn runtime_budget_membership_is_ready(&self, turn: &AgentTurnRecord) -> bool {
        if !self.agent.budget_memberships.enabled {
            return true;
        }
        if self.persistence.token_usage_store().is_none()
            || self.persistence.transcript_store().is_none()
        {
            return false;
        }
        self.agent
            .budget_memberships
            .entries
            .get(&turn.conversation_id)
            .is_some_and(|entry| {
                entry.ready
                    && entry.member.is_some()
                    && entry.lease.is_some()
                    && self.persistence.token_usage_store() == Some(&entry.store)
                    && self.persistence.transcript_store() == Some(&entry.transcripts)
                    && entry.pane == turn.pane_id
                    && self
                        .agent_shell_store()
                        .get(&turn.pane_id)
                        .is_some_and(|session| {
                            session.session_id == turn.conversation_id
                                && session.native_message_incarnation == entry.native
                        })
            })
    }

    /// Resolves the actual parent shell only after native spawn authorization.
    /// Disabled assembly does not impose new identity requirements on callers.
    pub(crate) fn capture_runtime_budget_spawn_inheritance(
        &mut self,
        parent_agent: &str,
        child_pane: &str,
    ) -> Result<()> {
        if !self.agent.budget_memberships.enabled {
            return Ok(());
        }
        let parent_pane = parent_agent
            .strip_prefix("agent-")
            .ok_or_else(|| MezError::invalid_state("budget parent native identity unavailable"))?;
        let parent_conversation = self
            .agent_shell_store()
            .get(parent_pane)
            .ok_or_else(|| MezError::invalid_state("budget parent conversation unavailable"))?
            .session_id
            .clone();
        self.capture_runtime_budget_inheritance(&parent_conversation, child_pane)
    }

    /// Freezes an original active parent's accounting membership at a genuine
    /// lifecycle creation/rebind boundary, before child work is queued. Callers
    /// supply a conversation from their validated native owner/controller, never
    /// a model-supplied lineage or a current depth-zero label. No I/O, scheduling,
    /// teardown, message authority or parent readiness is granted by this capture.
    /// The child must still complete its exact off-actor publication/adoption.
    pub(crate) fn capture_runtime_budget_inheritance(
        &mut self,
        parent_conversation: &str,
        child_pane: &str,
    ) -> Result<()> {
        if !self.agent.budget_memberships.enabled {
            return Ok(());
        }
        let parent = self
            .agent
            .budget_memberships
            .entries
            .get(parent_conversation)
            .ok_or_else(|| {
                MezError::invalid_state("original parent budget ownership unavailable")
            })?;
        if self.persistence.token_usage_store() != Some(&parent.store)
            || self.persistence.transcript_store() != Some(&parent.transcripts)
        {
            return Err(MezError::conflict("parent budget repository changed"));
        }
        let store = parent.store.clone();
        let transcripts = parent.transcripts.clone();
        let parent = parent.member.clone().ok_or_else(|| {
            MezError::invalid_state("original parent budget membership is not active")
        })?;
        let child = self
            .agent_shell_store()
            .get(child_pane)
            .ok_or_else(|| MezError::invalid_state("budget child native binding unavailable"))?;
        if child.session_id == parent.conversation {
            return Err(MezError::conflict(
                "budget inheritance cannot parent itself",
            ));
        }
        if let Some(original) = self.agent.budget_memberships.entries.get(&child.session_id) {
            if original.pane == child_pane
                && original.native == child.native_message_incarnation
                && original.store == store
                && original.transcripts == transcripts
                && matches!(&original.source, Source::Inherited { parent: captured, .. } if captured == &parent)
            {
                return Ok(());
            }
            return Err(MezError::conflict(
                "budget child already has different original ownership",
            ));
        }
        if self.agent.budget_memberships.entries.len() >= 4096 {
            return Err(MezError::invalid_state(
                "runtime budget membership capacity reached",
            ));
        }
        let conversation = child.session_id.clone();
        let native = child.native_message_incarnation.clone();
        self.agent.budget_memberships.entries.insert(
            conversation,
            Entry {
                identity: Arc::new(()),
                preparation: Arc::new(()),
                ready: false,
                store,
                transcripts,
                pane: child_pane.to_owned(),
                native,
                owner: crate::storage::token_usage::new_token_usage_event_id(),
                source: Source::Inherited {
                    parent,
                    operation: crate::storage::token_usage::new_token_usage_event_id(),
                    started_at: crate::runtime::current_unix_seconds(),
                },
                member: None,
                lease: None,
            },
        );
        Ok(())
    }

    /// Captures qualified retained accounting custody, never performs actor I/O.
    /// A loop may checkpoint its saved anchor rather than its live ephemeral
    /// conversation, and rebind may precede preparation. This reference grants
    /// neither native-incarnation continuity nor provider readiness.
    pub(crate) fn runtime_budget_checkpoint_reference(
        &self,
        conversation: &str,
    ) -> Result<Option<BudgetCheckpointReference>> {
        let Some(entry) = self.agent.budget_memberships.entries.get(conversation) else {
            return Ok(None);
        };
        let Some(member) = &entry.member else {
            return Ok(None);
        };
        if self.persistence.token_usage_store() != Some(&entry.store)
            || self.persistence.transcript_store() != Some(&entry.transcripts)
            || entry
                .lease
                .as_ref()
                .is_none_or(|lease| !lease.matches(member, &entry.owner))
        {
            return Err(MezError::conflict(
                "budget checkpoint live binding mismatch",
            ));
        }
        let id = |value: &str| {
            BudgetReceiptId::new(value)
                .map_err(|_| MezError::invalid_state("budget checkpoint identity invalid"))
        };
        Ok(Some(BudgetCheckpointReference {
            namespace: id(&member.namespace)?,
            root: member.root.clone(),
            conversation: member.conversation.clone(),
            operation: id(&member.operation)?,
            owner: id(&entry.owner)?,
        }))
    }

    /// Startup/restore worker only: qualify every affected original reference
    /// before hydration changes any pane. Legacy absence remains unknown.
    pub(crate) fn prepare_restored_budget_memberships<'a>(
        &self,
        transcripts: &AgentTranscriptStore,
        records: impl IntoIterator<Item = &'a mez_agent::transcript::AgentSessionMetadata>,
    ) -> Result<BTreeMap<String, RuntimeBudgetRestore>> {
        let mut prepared = BTreeMap::new();
        for record in records {
            record.validate()?;
            let Some(reference) = &record.budget_reference else {
                if transcripts
                    .conversation_budget_membership(&record.conversation_id)?
                    .is_some()
                {
                    return Err(MezError::conflict(
                        "original budget checkpoint reference missing",
                    ));
                }
                continue;
            };
            if prepared.contains_key(&record.conversation_id)
                || self.agent.budget_memberships.entries.len() + prepared.len() >= 4096
            {
                return Err(MezError::conflict(
                    "budget restore capacity or duplicate subject",
                ));
            }
            let store = self.persistence.cloned_token_usage_store().ok_or_else(|| {
                MezError::invalid_state("budget restore accounting store unavailable")
            })?;
            let (member, lease) = store.restore_native_budget_membership(transcripts, reference)?;
            prepared.insert(
                record.conversation_id.clone(),
                RuntimeBudgetRestore {
                    store,
                    transcripts: transcripts.clone(),
                    member,
                    owner: reference.owner.as_str().to_owned(),
                    lease,
                },
            );
        }
        Ok(prepared)
    }

    /// Adopts a preflight holder only after ordinary hydration succeeded. It
    /// preserves accounting, not native shell/MMP/cleanup identity or readiness.
    pub(crate) fn adopt_restored_budget_membership(
        &mut self,
        pane: &str,
        work: RuntimeBudgetRestore,
    ) -> Result<()> {
        let session = self
            .agent_shell_store()
            .get(pane)
            .filter(|session| session.session_id == work.member.conversation)
            .ok_or_else(|| MezError::conflict("budget restore target changed"))?;
        let native = session.native_message_incarnation.clone();
        self.agent.budget_memberships.entries.insert(
            work.member.conversation.clone(),
            Entry {
                identity: Arc::new(()),
                preparation: Arc::new(()),
                ready: false,
                store: work.store,
                transcripts: work.transcripts,
                pane: pane.to_owned(),
                native,
                owner: work.owner,
                source: Source::Existing,
                member: Some(work.member),
                lease: Some(work.lease),
            },
        );
        Ok(())
    }

    /// Exercises the real preparation/claim boundary before product activation.
    /// Only isolated tests enable these incomplete lifecycle ports for now.
    #[cfg(test)]
    pub(crate) fn fail_budget_restore_after_install_for_tests(&mut self) {
        self.agent.budget_memberships.fail_restore_after_install = true;
    }

    /// Fault at the actual post-install/pre-custody boundary, scoped to one
    /// generated runtime rather than a global process environment switch.
    #[cfg(test)]
    pub(crate) fn check_budget_restore_after_install_for_tests(&mut self) -> Result<()> {
        if std::mem::take(&mut self.agent.budget_memberships.fail_restore_after_install) {
            return Err(MezError::invalid_state(
                "injected budget restore post-install failure",
            ));
        }
        Ok(())
    }

    /// Exercises the real preparation/claim boundary before product activation.
    /// Only isolated tests enable these incomplete lifecycle ports for now.
    #[cfg(test)]
    pub(crate) fn enable_runtime_budget_membership_for_tests(&mut self) {
        self.agent.budget_memberships.enabled = true;
    }

    /// Issued provider work retains native binding custody independently of the
    /// actor. Dropping/replacing an actor cannot retire a worker's original lock.
    pub(crate) fn runtime_budget_lease_for_turn(
        &self,
        turn: &AgentTurnRecord,
    ) -> Option<BudgetBindingLease> {
        self.agent
            .budget_memberships
            .entries
            .get(&turn.conversation_id)
            .and_then(|entry| entry.lease.clone())
    }
}
