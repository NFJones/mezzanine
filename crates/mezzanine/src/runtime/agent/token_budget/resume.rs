//! Ordinary saved-session resume shares the original database/mirror/inode
//! checks with startup restore. Captures are never serialized; any retained
//! custody is an actual descriptor, not reconstructed authority. The command
//! worker qualifies custody before taking its replay snapshot lock.

use super::*;

/// Exact actor-selected accounting repository and optional current-pane holder.
#[derive(Debug, Clone, Default)]
pub(crate) struct RuntimeBudgetResumeCapture {
    store: Option<TokenUsageStore>,
    retained: Option<RuntimeBudgetRestore>,
}

impl RuntimeBudgetResumeCapture {
    /// No publication or new account is permitted. An existing account may be
    /// resumed independently of its parent, but grants no teardown authority.
    pub(crate) fn prepare(
        &self,
        transcripts: &AgentTranscriptStore,
        conversation: &str,
    ) -> Result<Option<RuntimeBudgetRestore>> {
        let Some(member) = transcripts.conversation_budget_membership(conversation)? else {
            if let Some(store) = &self.store
                && store.budget_membership(conversation)?.is_some()
            {
                return Err(MezError::conflict(
                    "saved budget membership metadata missing",
                ));
            }
            return Ok(None);
        };
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("saved budget accounting store unavailable"))?;
        let owner = store
            .budget_binding_owner(&member)?
            .ok_or_else(|| MezError::conflict("saved budget binding owner unavailable"))?;
        let id = |value: &str| {
            BudgetReceiptId::new(value)
                .map_err(|_| MezError::invalid_state("saved budget identity invalid"))
        };
        let reference = BudgetCheckpointReference {
            namespace: id(&member.namespace)?,
            root: member.root.clone(),
            conversation: member.conversation.clone(),
            operation: id(&member.operation)?,
            owner: id(&owner)?,
        };
        let retained = self
            .retained
            .as_ref()
            .filter(|original| {
                original.member == member
                    && original.owner == owner
                    && original.store == *store
                    && original.transcripts == *transcripts
            })
            .map(|original| &original.lease);
        let (member, lease) =
            store.restore_retained_native_budget_membership(transcripts, &reference, retained)?;
        Ok(Some(RuntimeBudgetRestore {
            store: store.clone(),
            transcripts: transcripts.clone(),
            member,
            owner,
            lease,
        }))
    }
}

/// Reversible in-memory accounting adoption, retained through checkpointing.
pub(crate) struct RuntimeBudgetResumeRollback {
    conversation: String,
    previous: Option<Entry>,
}

impl RuntimeSessionService {
    /// A committed independent resume retires only the departed actor holder.
    /// Issued workers keep their own Arc leases; no label or Stop attests their
    /// retirement. Failed transactions never call this nonfallible commit step.
    pub(crate) fn retire_runtime_budget_resume_departure(
        &mut self,
        pane: &str,
        previous: &mez_agent::AgentShellSession,
    ) {
        if let Some(entry) = self
            .agent
            .budget_memberships
            .entries
            .get_mut(&previous.session_id)
            && entry.pane == pane
            && entry.native == previous.native_message_incarnation
        {
            entry.ready = false;
            entry.lease = None;
            entry.identity = Arc::new(());
            entry.preparation = Arc::new(());
        }
    }

    /// Freeze only the invoking pane's exact native custody. Another live pane
    /// or a saved lineage label cannot lend its descriptor to this command.
    pub(crate) fn capture_runtime_budget_resume(
        &self,
        pane: &str,
    ) -> Result<RuntimeBudgetResumeCapture> {
        let mut retained = None;
        if let Some(session) = self.agent_shell_store().get(pane)
            && let Some(entry) = self
                .agent
                .budget_memberships
                .entries
                .get(&session.session_id)
            && entry.pane == pane
            && entry.native == session.native_message_incarnation
            && self
                .runtime_budget_checkpoint_reference(&session.session_id)?
                .is_some()
            && let (Some(member), Some(lease)) = (&entry.member, &entry.lease)
        {
            retained = Some(RuntimeBudgetRestore {
                store: entry.store.clone(),
                transcripts: entry.transcripts.clone(),
                member: member.clone(),
                owner: entry.owner.clone(),
                lease: lease.clone(),
            });
        }
        Ok(RuntimeBudgetResumeCapture {
            store: self.persistence.cloned_token_usage_store(),
            retained,
        })
    }

    pub(crate) fn snapshot_runtime_budget_resume(
        &self,
        conversation: &str,
    ) -> RuntimeBudgetResumeRollback {
        RuntimeBudgetResumeRollback {
            conversation: conversation.into(),
            previous: self
                .agent
                .budget_memberships
                .entries
                .get(conversation)
                .cloned(),
        }
    }

    /// Used only by the synchronous serialized resume transaction, before any
    /// other actor event can replace the provisional adopted entry.
    pub(crate) fn rollback_runtime_budget_resume(&mut self, snapshot: RuntimeBudgetResumeRollback) {
        self.agent
            .budget_memberships
            .entries
            .remove(&snapshot.conversation);
        if let Some(previous) = snapshot.previous {
            self.agent
                .budget_memberships
                .entries
                .insert(snapshot.conversation, previous);
        }
    }
}
