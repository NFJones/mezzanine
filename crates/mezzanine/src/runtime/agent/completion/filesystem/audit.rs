//! Original audit obligations and one-shot filesystem record append acceptance.
//!
//! Queue ownership freezes writer allocation, policy and action attribution.
//! Admission and every commit lease reject a replaced or unavailable source.
//! Settlement never samples a replacement policy to waive an original record.
//! Entered append failures cannot replay, including lost acknowledgements after
//! real writes. Deferred append acceptance is not physical persistence, archive
//! acceptance or permission to release the original producer/attempt owners.

use super::*;
use crate::runtime::persistence::RuntimeAuditWriterIdentity;
use crate::security::audit::{AuditActor, AuditConfig, AuditRecord};

/// Original registered policy plus record data, not a cloned writer/hash state.
#[derive(Clone)]
pub(super) struct FilesystemAuditObligation {
    /// Actual attached allocation, absent only for original writer absence.
    writer: Option<RuntimeAuditWriterIdentity>,
    /// Immutable original policy, independent of later config or attachment.
    policy: Option<AuditConfig>,
    /// Original session/action/policy attribution; no shell command or output.
    record: AuditRecord,
}

impl FilesystemAuditObligation {
    /// Captures the original source without performing audit I/O or cloning the
    /// mutable log. A mismatched internal attachment is an invariant error.
    pub(super) fn capture(
        service: &RuntimeSessionService,
        work: &NativeFilesystemWork,
    ) -> Result<Self> {
        let writer = service.persistence.audit_writer_identity();
        let policy = service
            .persistence
            .audit_log()
            .map(|log| log.configuration().clone());
        if writer.is_some() != policy.is_some() {
            return Err(MezError::invalid_state(
                "native filesystem audit registration is inconsistent",
            ));
        }
        let permission = service.permission_policy_for_turn(&work.owner.turn);
        let mut record = AuditRecord::new(
            service.session.id.to_string(),
            AuditActor {
                kind: "agent".into(),
                id: work.owner.turn.agent_id.clone(),
            },
            "native_filesystem",
            "apply_patch",
        )
        .with_pane_id(work.owner.turn.pane_id.clone())
        .with_agent_id(work.owner.turn.agent_id.clone())
        .with_metadata("turn_id", work.owner.turn.turn_id.clone())
        .with_metadata("conversation_id", work.owner.turn.conversation_id.clone())
        .with_metadata("action_id", work.owner.action.id.clone())
        .with_metadata("execution_group", work.owner.group.as_str())
        .with_metadata("transaction", work.identity.transaction.clone())
        .with_metadata("execution_target", "native_runtime")
        .with_metadata("sent_to_pane", "false")
        .with_metadata(
            "enforcement",
            mez_agent::native_action::NativeFilesystemEnforcement::RuntimeCapabilities.as_str(),
        )
        .with_metadata(
            "sandbox_configured",
            service
                .sandbox_config_for_pane(&work.owner.turn.pane_id)
                .as_str(),
        );
        // Timestamp the actual append, while attribution/policy remain frozen.
        record.timestamp.clear();
        record.policy_mode =
            crate::runtime::runtime_permission_preset_name(permission.preset).into();
        record.approval_state = if permission.approval_policy.bypasses_sandbox() {
            "host-policy-bypass"
        } else {
            "not_required_or_preapproved"
        }
        .into();
        Ok(Self {
            writer,
            policy,
            record,
        })
    }

    /// Reports an actual original record obligation; optional disabled/absent
    /// policy is distinct from an accepted append and cannot fabricate one.
    fn requires_append(&self) -> bool {
        self.policy
            .as_ref()
            .is_some_and(|policy| policy.enabled || policy.required)
    }

    /// Revalidates exact writer identity and required availability without I/O.
    /// Equal replacement paths/settings never refresh the original allocation.
    fn validate(&self, service: &RuntimeSessionService) -> Result<()> {
        let current = match &self.writer {
            Some(writer) => service.persistence.audit_writer_is_current(writer),
            None => service.persistence.audit_log().is_none(),
        };
        if !current {
            return Err(MezError::conflict(
                "native filesystem original audit writer changed",
            ));
        }
        if service.persistence.audit_writer_is_unproven() {
            return Err(MezError::invalid_state(
                "native filesystem original audit writer has an unproven staged append",
            ));
        }
        if self
            .policy
            .as_ref()
            .is_some_and(|policy| policy.required && !policy.enabled)
        {
            return Err(MezError::forbidden(
                "native filesystem required audit writer is disabled",
            ));
        }
        Ok(())
    }
}

impl RuntimeSessionService {
    /// Checks the frozen obligation before issuance and each endpoint lease.
    /// This proves registration/availability only, not a future physical write.
    pub(in crate::runtime::agent) fn validate_native_filesystem_audit_source(
        &self,
        attempt: &NativeFilesystemAttempt,
    ) -> Result<()> {
        let key = self.filesystem_completion_key(attempt)?;
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get(&key)
            .ok_or_else(|| MezError::invalid_state("filesystem audit owner unavailable"))?;
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        receipt.audit_obligation.validate(self)
    }

    /// Appends exactly once after original context/hooks acceptance. Returned
    /// event/hash evidence stays owned even if the subsequent acknowledgement
    /// fails. Deferred writes remain subject to later physical settlement gates.
    pub(super) fn append_filesystem_audit(&mut self, key: &ExternalCompletionKey) -> Result<bool> {
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get(key)
            .ok_or_else(|| MezError::invalid_state("filesystem audit receipt unavailable"))?;
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        if matches!(
            receipt.phase,
            FilesystemCompletionPhase::AuditNotRequired
                | FilesystemCompletionPhase::AuditAppendAccepted
                | FilesystemCompletionPhase::AuditPhysicalAccepted
        ) {
            return Ok(false);
        }
        if receipt.phase != FilesystemCompletionPhase::HooksAccepted {
            return Err(MezError::conflict(
                "filesystem audit append is not an unentered accepted-hook gate",
            ));
        }
        if !receipt.audit_obligation.requires_append() {
            self.filesystem_completion_phase(key, FilesystemCompletionPhase::AuditNotRequired)?;
            return Ok(true);
        }
        let original = receipt.audit_obligation.clone();
        let payload = receipt.completed_hook_payload.as_deref().ok_or_else(|| {
            MezError::invalid_state("filesystem original audit outcome unavailable")
        })?;
        let value: serde_json::Value = serde_json::from_str(payload).map_err(|_| {
            MezError::invalid_state("filesystem original audit outcome is malformed")
        })?;
        let outcome = value
            .get("action_status")
            .and_then(serde_json::Value::as_str)
            .filter(|status| matches!(*status, "succeeded" | "failed"))
            .ok_or_else(|| {
                MezError::invalid_state("filesystem original audit status unavailable")
            })?;
        let mut record = original
            .record
            .clone()
            .with_metadata("semantic_outcome", payload);
        record.outcome = outcome.into();
        self.filesystem_completion_phase(key, FilesystemCompletionPhase::AuditEntered)?;
        original.validate(self)?;
        let writer = self.persistence.audit_log_mut().ok_or_else(|| {
            MezError::invalid_state("filesystem original audit writer unavailable")
        })?;
        let deferred = writer.writes_are_deferred();
        let write = writer
            .append_tracked(record)
            .map_err(|error| error.with_context("native filesystem audit append"))?
            .ok_or_else(|| {
                MezError::invalid_state("filesystem original enabled audit append was not accepted")
            })?;
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get_mut(key)
            .ok_or_else(|| {
                MezError::invalid_state("filesystem audit owner missing after append")
            })?;
        receipt.audit_append = Some(write);
        receipt.audit_append_deferred = deferred;
        #[cfg(test)]
        if std::mem::take(&mut receipt.reject_audit_ack_for_tests) {
            return Err(MezError::invalid_state(
                "injected filesystem failure after audit append before acknowledgement",
            ));
        }
        self.filesystem_completion_phase(key, FilesystemCompletionPhase::AuditAppendAccepted)?;
        Ok(true)
    }

    /// Accepts only the original physical receipt's own actual result. Pending
    /// work remains held; path/count notifications cannot reach this boundary.
    /// Physical acceptance is not publication/archive/producer or worker-join
    /// acknowledgement. An uncertain failed append is never reissued here.
    pub(super) fn settle_filesystem_physical_audit(
        &mut self,
        key: &ExternalCompletionKey,
    ) -> Result<bool> {
        use crate::security::audit::AuditAppendStatus;
        let terminal = self
            .agent
            .completion_settlements
            .filesystem
            .get(key)
            .and_then(|receipt| receipt.audit_append.as_ref())
            .and_then(|write| write.receipt.as_ref())
            .is_some_and(|physical| {
                matches!(
                    physical.status(),
                    AuditAppendStatus::Accepted | AuditAppendStatus::Failed(_)
                )
            });
        if terminal {
            let receipt = self
                .agent
                .completion_settlements
                .filesystem
                .get_mut(key)
                .ok_or_else(|| {
                    MezError::invalid_state("filesystem physical fact owner unavailable")
                })?;
            if !receipt.audit_physical_observed {
                receipt.audit_physical_observed = true;
                if let Some(group) = self.agent.completion_settlements.groups.get_mut(&key.0) {
                    group.source_changed();
                }
            }
        }
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get(key)
            .ok_or_else(|| {
                MezError::invalid_state("filesystem physical audit owner unavailable")
            })?;
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        if matches!(
            receipt.phase,
            FilesystemCompletionPhase::AuditNotRequired
                | FilesystemCompletionPhase::AuditPhysicalAccepted
        ) {
            return Ok(false);
        }
        if receipt.phase != FilesystemCompletionPhase::AuditAppendAccepted {
            return Err(MezError::conflict(
                "filesystem physical audit has no accepted original append",
            ));
        }
        let physical = receipt
            .audit_append
            .as_ref()
            .and_then(|write| write.receipt.as_ref())
            .ok_or_else(|| {
                MezError::invalid_state(
                    "filesystem accepted append has no original physical receipt",
                )
            })?;
        match physical.status() {
            AuditAppendStatus::Queued | AuditAppendStatus::InFlight => Ok(false),
            AuditAppendStatus::Accepted => {
                if !physical.worker_joined() {
                    return Ok(false);
                }
                self.filesystem_completion_phase(
                    key,
                    FilesystemCompletionPhase::AuditPhysicalAccepted,
                )?;
                Ok(true)
            }
            AuditAppendStatus::Failed(error) => {
                Err(error.with_context("native filesystem physical audit settlement"))
            }
        }
    }

    /// Observes only an actual receipt already owned by a filesystem attempt.
    /// Callback labels contain no success bit. Lost notifications can be
    /// reconciled by the ordinary projection callback reading the same receipt.
    pub(crate) fn observe_native_filesystem_audit(
        &mut self,
        physical: &crate::security::audit::AuditAppendReceipt,
    ) -> Result<bool> {
        let owned = self
            .agent
            .completion_settlements
            .filesystem
            .iter()
            .find(|(_, receipt)| {
                receipt
                    .audit_append
                    .as_ref()
                    .and_then(|write| write.receipt.as_ref())
                    == Some(physical)
            })
            .map(|(key, receipt)| (key.clone(), receipt.attempt.clone()));
        let Some((key, attempt)) = owned else {
            return Ok(false);
        };
        let scope = self.begin_completion_scope(&key.0, CompletionScope::Callback);
        let settled = self.settle_filesystem_physical_audit(&key);
        if let Err(error) = &settled {
            self.fail_filesystem_completion(&attempt, error);
        }
        self.end_completion_scope(scope, "filesystem_physical_audit", false, settled)
    }

    /// Injects one acknowledgement loss after the actual original append only.
    #[cfg(test)]
    pub(crate) fn reject_filesystem_audit_ack_for_tests(
        &mut self,
        attempt: &NativeFilesystemAttempt,
    ) -> Result<()> {
        let key = self.filesystem_completion_key(attempt)?;
        self.agent
            .completion_settlements
            .filesystem
            .get_mut(&key)
            .ok_or_else(|| MezError::invalid_state("filesystem audit fault owner unavailable"))?
            .reject_audit_ack_for_tests = true;
        Ok(())
    }

    /// Exposes actual returned append evidence and deferred status without
    /// satisfying physical persistence or changing any production owner.
    #[cfg(test)]
    pub(crate) fn filesystem_audit_append_for_tests(
        &self,
        attempt: &NativeFilesystemAttempt,
    ) -> Result<Option<(crate::security::audit::AuditWrite, bool)>> {
        let key = self.filesystem_completion_key(attempt)?;
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get(&key)
            .ok_or_else(|| MezError::invalid_state("filesystem audit test owner unavailable"))?;
        Ok(receipt
            .audit_append
            .clone()
            .map(|write| (write, receipt.audit_append_deferred)))
    }
}
