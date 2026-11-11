//! Human-only explicit resolution of otherwise unprovable legacy ancestry.
//!
//! The opaque confirmation is an in-memory proposal, not a serialized grant.
//! Dropping it cancels without I/O. Confirmation only installs the original
//! proposal; the ordinary preparation worker still validates and publishes it.
//! No provider, model action, supplied lineage or decoded history calls this port.

use super::*;

/// A human-selected accounting association, never inferred structural lineage.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RuntimeBudgetLegacyChoice {
    NewIndependent,
    /// An immutable active membership anchors an existing account, even when
    /// that conversation is not live. Neither identity conveys teardown rights.
    Existing {
        root: BudgetRootId,
        anchor: String,
    },
}

/// Labeled boundary semantics: an existing account's original start is retained,
/// not replaced by this membership's proposed start. No actor-side DB read or
/// invented account timestamp is required to disclose that distinction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeBudgetLegacyAccountStart {
    NewAt(u64),
    PreserveExisting,
}

/// Frozen display facts, never a permission or a durable membership receipt.
pub(crate) struct RuntimeBudgetLegacyDisclosure<'a> {
    pub conversation: &'a str,
    pub root: &'a BudgetRootId,
    pub choice: &'a RuntimeBudgetLegacyChoice,
    pub membership_started_at: u64,
    pub account_start: RuntimeBudgetLegacyAccountStart,
    pub historical_usage_incomplete: bool,
}

/// Original private capture retained while the human sees the start boundary.
/// Not Clone/Serialize: retries use the installed proposal, not this confirmation.
pub(crate) struct RuntimeBudgetLegacyConfirmation {
    primary: mez_core::ids::ClientId,
    work: RuntimeBudgetMembershipWork,
    started_at: u64,
    root: BudgetRootId,
    choice: RuntimeBudgetLegacyChoice,
}

impl RuntimeBudgetLegacyConfirmation {
    /// Display facts only; the UI must explicitly disclose incomplete historical
    /// expense and obtain confirmation before calling the mutation port.
    pub(crate) fn disclosure(&self) -> RuntimeBudgetLegacyDisclosure<'_> {
        RuntimeBudgetLegacyDisclosure {
            conversation: &self.work.conversation,
            root: &self.root,
            choice: &self.choice,
            membership_started_at: self.started_at,
            account_start: match self.choice {
                RuntimeBudgetLegacyChoice::NewIndependent => {
                    RuntimeBudgetLegacyAccountStart::NewAt(self.started_at)
                }
                RuntimeBudgetLegacyChoice::Existing { .. } => {
                    RuntimeBudgetLegacyAccountStart::PreserveExisting
                }
            },
            historical_usage_incomplete: true,
        }
    }
}

impl RuntimeSessionService {
    /// Captures one unresolved original entry without database/filesystem I/O.
    /// A held worker or another confirmation refuses rather than replacing an
    /// operation that may already have published. No shell/turn state is reset.
    pub(crate) fn prepare_runtime_budget_legacy_resolution(
        &self,
        primary: &mez_core::ids::ClientId,
        pane: &str,
        choice: RuntimeBudgetLegacyChoice,
    ) -> Result<RuntimeBudgetLegacyConfirmation> {
        if !self.agent.budget_memberships.enabled || !self.session.is_attached_primary(primary) {
            return Err(MezError::forbidden(
                "legacy budget resolution requires attached primary",
            ));
        }
        let session = self
            .agent_shell_store()
            .get(pane)
            .ok_or_else(|| MezError::conflict("legacy budget conversation unavailable"))?;
        let entry = self
            .agent
            .budget_memberships
            .entries
            .get(&session.session_id)
            .filter(|entry| {
                matches!(entry.source, Source::Existing)
                    && entry.namespace.is_some()
                    && entry.member.is_none()
                    && entry.lease.is_none()
                    && !entry.ready
                    && entry.pane == pane
                    && entry.native == session.native_message_incarnation
                    && self.persistence.token_usage_store() == Some(&entry.store)
                    && self.persistence.transcript_store() == Some(&entry.transcripts)
                    && Arc::strong_count(&entry.identity) == 1
                    && Arc::strong_count(&entry.preparation) == 1
            })
            .ok_or_else(|| {
                MezError::conflict("legacy budget resolution is unavailable or outstanding")
            })?;
        let started_at = crate::runtime::current_unix_seconds();
        let (source, root) = match &choice {
            RuntimeBudgetLegacyChoice::NewIndependent => {
                let source = Source::independent(started_at, true)?;
                let Source::Independent { root, .. } = &source else {
                    unreachable!()
                };
                let root = root.clone();
                (source, root)
            }
            RuntimeBudgetLegacyChoice::Existing { root, anchor } => {
                mez_agent::transcript::validate_conversation_id(anchor)?;
                if anchor == &session.session_id || anchor.len() > 256 {
                    return Err(MezError::invalid_args(
                        "invalid legacy budget account anchor",
                    ));
                }
                (
                    Source::LegacyExisting {
                        root: root.clone(),
                        anchor: anchor.clone(),
                        operation: crate::storage::token_usage::new_token_usage_event_id(),
                        started_at,
                    },
                    root.clone(),
                )
            }
        };
        Ok(RuntimeBudgetLegacyConfirmation {
            primary: primary.clone(),
            started_at,
            root,
            choice,
            work: RuntimeBudgetMembershipWork {
                store: entry.store.clone(),
                transcripts: entry.transcripts.clone(),
                namespace: entry.namespace.clone(),
                conversation: session.session_id.clone(),
                pane: pane.into(),
                native: entry.native.clone(),
                identity: entry.identity.clone(),
                preparation: entry.preparation.clone(),
                owner: entry.owner.clone(),
                source,
                lease: None,
            },
        })
    }

    /// Requires the same authenticated primary and exact still-unresolved entry.
    /// Success is capture only, NOT durable membership or execution readiness.
    /// The UI owner must not call this on cancellation or treat it as an ACK.
    pub(crate) fn confirm_runtime_budget_legacy_resolution(
        &mut self,
        primary: &mez_core::ids::ClientId,
        confirmation: RuntimeBudgetLegacyConfirmation,
    ) -> Result<()> {
        let work = confirmation.work;
        if !self.agent.budget_memberships.enabled
            || primary != &confirmation.primary
            || !self.session.is_attached_primary(primary)
            || self.persistence.token_usage_store() != Some(&work.store)
            || self.persistence.transcript_store() != Some(&work.transcripts)
            || !self
                .agent_shell_store()
                .get(&work.pane)
                .is_some_and(|session| {
                    session.session_id == work.conversation
                        && session.native_message_incarnation == work.native
                })
        {
            return Err(MezError::conflict(
                "legacy budget confirmation owner changed",
            ));
        }
        let entry = self
            .agent
            .budget_memberships
            .entries
            .get_mut(&work.conversation)
            .filter(|entry| {
                matches!(entry.source, Source::Existing)
                    && entry.namespace == work.namespace
                    && entry.member.is_none()
                    && entry.lease.is_none()
                    && !entry.ready
                    && entry.pane == work.pane
                    && entry.native == work.native
                    && entry.owner == work.owner
                    && Arc::ptr_eq(&entry.identity, &work.identity)
                    && Arc::ptr_eq(&entry.preparation, &work.preparation)
                    && Arc::strong_count(&entry.identity) == 2
                    && Arc::strong_count(&entry.preparation) == 2
            })
            .ok_or_else(|| MezError::conflict("legacy budget confirmation is stale"))?;
        entry.source = work.source;
        entry.identity = Arc::new(());
        entry.preparation = Arc::new(());
        Ok(())
    }
}
