//! Original filesystem archive acceptance after actual checked append admission.
//!
//! Candidate retention is separate from its exact checked read and accepted
//! append ownership. The opaque gate here shares the actual candidate receipt,
//! never a path/count, generation or success boolean. Original no-save policy
//! is explicit and cannot be refreshed from later repository attachment. This
//! metadata gate releases no attempt, raw outcome, producer or group. Physical
//! persistence, actual worker joins and final transfer remain separate gates.

use super::super::retirement::{
    CompletionNoPersistencePolicy, CompletionTranscriptOwnership, CompletionTranscriptTarget,
};
use super::*;
use crate::runtime::persistence::RuntimeCompletionCandidateWitness;

/// Original checked archive destination or original intentional no-save policy.
#[derive(Clone)]
enum ArchiveDestination {
    /// Exact retained candidate with its actual accepted checked-source receipt.
    Saved(RuntimeCompletionCandidateWitness),
    /// Original admitted runtime-only/ephemeral policy, not a disk receipt.
    NoPersistence(CompletionNoPersistencePolicy),
}

/// Opaque original archive gate, never permission to retire a producer.
#[derive(Clone)]
pub(crate) struct NativeFilesystemArchiveAcceptance {
    /// Actual actor attempt remains independently registered until final transfer.
    attempt: NativeFilesystemAttempt,
    /// Original completion group allocation, not a matching public group label.
    group: Arc<()>,
    /// Actual registered producer allocation, without retaining that owner.
    producer: Option<
        crate::runtime::processes::native_filesystem_producer::NativeFilesystemProducerIdentity,
    >,
    /// Actual checked source or explicit original no-save policy.
    destination: ArchiveDestination,
}

impl std::fmt::Debug for NativeFilesystemArchiveAcceptance {
    /// Never expose path, row bytes, private errors or opaque allocation values.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeFilesystemArchiveAcceptance")
    }
}

impl NativeFilesystemArchiveAcceptance {
    /// Returns exact original attribution for root-owned verification custody.
    pub(crate) fn attempt(&self) -> &NativeFilesystemAttempt {
        &self.attempt
    }

    /// Rejects a lookalike producer even when its attempt labels are identical.
    pub(crate) fn producer_is(
        &self,
        producer: &crate::runtime::processes::native_filesystem_producer::NativeFilesystemProducer,
    ) -> bool {
        self.producer.as_ref() == Some(&producer.identity())
    }

    /// Retrieves only the actual checked source's immutable physical work.
    /// None denotes original no-save policy, never a fabricated physical fact.
    pub(crate) fn physical_receipt(
        &self,
    ) -> Result<Option<crate::runtime::agent::RuntimeTranscriptPrefixReceipt>> {
        match &self.destination {
            ArchiveDestination::NoPersistence(_) => Ok(None),
            ArchiveDestination::Saved(witness) => witness
                .checked_source()
                .physical_receipt()?
                .map(Some)
                .ok_or_else(|| {
                    MezError::invalid_state(
                        "filesystem physical source was not bound at checked admission",
                    )
                }),
        }
    }

    /// Checks actual opaque source ownership, including its immutable witness.
    pub(crate) fn same_source(&self, other: &Self) -> bool {
        self.attempt == other.attempt
            && self.producer == other.producer
            && Arc::ptr_eq(&self.group, &other.group)
            && match (&self.destination, &other.destination) {
                (ArchiveDestination::Saved(left), ArchiveDestination::Saved(right)) => {
                    left == right
                }
                (
                    ArchiveDestination::NoPersistence(left),
                    ArchiveDestination::NoPersistence(right),
                ) => left == right,
                _ => false,
            }
    }
}

impl RuntimeSessionService {
    /// Retains original source in the existing candidate owner, then observes
    /// only actual checked acceptance. Pending work remains owned and returns
    /// None. This cannot perform inline archive I/O on the serialized actor.
    pub(crate) fn prepare_native_filesystem_archive(
        &mut self,
        attempt: &NativeFilesystemAttempt,
    ) -> Result<Option<NativeFilesystemArchiveAcceptance>> {
        let key = self.filesystem_completion_key(attempt)?;
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get(&key)
            .ok_or_else(|| MezError::invalid_state("filesystem archive owner unavailable"))?;
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        let group = self
            .agent
            .completion_settlements
            .groups
            .get(&key.0)
            .ok_or_else(|| {
                MezError::invalid_state("filesystem original archive group unavailable")
            })?;
        let ready = receipt.phase == FilesystemCompletionPhase::PublicationAccepted
            || (group.paint_revoked
                && matches!(
                    receipt.phase,
                    FilesystemCompletionPhase::AuditNotRequired
                        | FilesystemCompletionPhase::AuditPhysicalAccepted
                        | FilesystemCompletionPhase::PublicationHeld
                ));
        if !ready {
            return Ok(None);
        }
        if matches!(
            group.transcript,
            CompletionTranscriptTarget::Original { .. }
        ) && !self.persistence.transcript_uses_adapter()
        {
            return Err(MezError::invalid_state(
                "filesystem archive requires off-actor checked persistence",
            ));
        }
        let original_policy = match &group.transcript {
            CompletionTranscriptTarget::NoPersistencePolicy(policy) => Some(*policy),
            CompletionTranscriptTarget::Original { .. } => None,
        };
        let scope = self.begin_completion_scope(&key.0, CompletionScope::Callback);
        let prepared = (|| {
            // Original no-save policy acknowledges only absence of an archive
            // obligation. It cannot clear the group's retained first failure.
            let destination = if let Some(policy) = original_policy {
                ArchiveDestination::NoPersistence(policy)
            } else {
                match self.retain_completion_group_transcript(&key.0)? {
                    CompletionTranscriptOwnership::Candidate(witness) => {
                        if let Some(error) = witness.checked_source().failure() {
                            return Err(error);
                        }
                        if !witness.checked_source().accepted() {
                            return Ok(None);
                        }
                        ArchiveDestination::Saved(witness)
                    }
                    CompletionTranscriptOwnership::NoPersistencePolicy(_) => {
                        return Err(MezError::invalid_state(
                            "filesystem original archive policy changed",
                        ));
                    }
                }
            };
            let group = self
                .agent
                .completion_settlements
                .groups
                .get(&key.0)
                .ok_or_else(|| MezError::invalid_state("filesystem archive group disappeared"))?;
            let acceptance = NativeFilesystemArchiveAcceptance {
                attempt: attempt.clone(),
                group: Arc::clone(&group.identity),
                producer: self
                    .retained_native_filesystem_producer(attempt)
                    .map(|producer| producer.identity()),
                destination,
            };
            self.agent
                .completion_settlements
                .filesystem
                .get_mut(&key)
                .ok_or_else(|| MezError::invalid_state("filesystem archive receipt disappeared"))?
                .archive_source = Some(acceptance.clone());
            if !self.filesystem_archive_acceptance_matches_retained_owner(&acceptance) {
                return Err(MezError::invalid_state(
                    "filesystem archive acceptance lost original owner",
                ));
            }
            Ok(Some(acceptance))
        })();
        if let Err(error) = &prepared {
            self.fail_filesystem_completion(attempt, error);
        }
        self.end_completion_scope(scope, "filesystem_archive", false, prepared)
    }

    /// Restages saved canonical source without I/O before checking its exact
    /// receipt. This may invalidate a stale cached witness, but cannot append,
    /// replay an effect or retire an owner. No-save proves only original policy.
    pub(crate) fn native_filesystem_archive_acceptance_is_current(
        &mut self,
        acceptance: &NativeFilesystemArchiveAcceptance,
    ) -> bool {
        if !self.filesystem_archive_acceptance_matches_retained_owner(acceptance) {
            return false;
        }
        if matches!(acceptance.destination, ArchiveDestination::Saved(_)) {
            let Ok(key) = self.filesystem_completion_key(&acceptance.attempt) else {
                return false;
            };
            if self.stage_completion_group_transcript(&key.0).is_err() {
                return false;
            }
        }
        self.filesystem_archive_acceptance_matches_retained_owner(acceptance)
    }

    /// Checks actual retained artifacts only. Mint calls this immediately after
    /// fresh preparation; consumers must use the restaging public check above.
    fn filesystem_archive_acceptance_matches_retained_owner(
        &self,
        acceptance: &NativeFilesystemArchiveAcceptance,
    ) -> bool {
        let Ok(key) = self.filesystem_completion_key(&acceptance.attempt) else {
            return false;
        };
        let Some(receipt) = self.agent.completion_settlements.filesystem.get(&key) else {
            return false;
        };
        if receipt.failure.is_some() {
            return false;
        }
        let Some(stored) = &receipt.archive_source else {
            return false;
        };
        if stored.producer != acceptance.producer
            || stored.attempt != acceptance.attempt
            || !Arc::ptr_eq(&stored.group, &acceptance.group)
            || !match (&stored.destination, &acceptance.destination) {
                (ArchiveDestination::Saved(left), ArchiveDestination::Saved(right)) => {
                    left == right
                }
                (
                    ArchiveDestination::NoPersistence(left),
                    ArchiveDestination::NoPersistence(right),
                ) => left == right,
                _ => false,
            }
        {
            return false;
        }
        let Some(group) = self.agent.completion_settlements.groups.get(&key.0) else {
            return false;
        };
        if !Arc::ptr_eq(&group.identity, &acceptance.group) {
            return false;
        }
        match (&group.transcript, &acceptance.destination) {
            (CompletionTranscriptTarget::Original { .. }, ArchiveDestination::Saved(witness)) => {
                witness.checked_source().accepted()
                    && group
                        .handoff
                        .as_ref()
                        .and_then(|handoff| handoff.witness.as_ref())
                        == Some(witness)
            }
            (
                CompletionTranscriptTarget::NoPersistencePolicy(policy),
                ArchiveDestination::NoPersistence(original),
            ) => policy == original,
            _ => false,
        }
    }
}
