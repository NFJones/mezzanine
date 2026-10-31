//! Exact endpoint admission between the serialized actor and filesystem worker.
//!
//! A proposal binds authored occurrence, held capability allocation, physical
//! path and raw byte digests. Only an actor authorization callback should issue
//! a lease in production. Issuance owns the shared writer guard immediately:
//! cancellation reports in-flight work until that actual guard is released.
//! The worker must consume the short lease before publication; expiration or
//! cancellation before entry cannot become a write. Entry does not interrupt an
//! OS operation or certify a completed effect, producer join or durable result.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mez_agent::native_action::{NativeCommitIdentity, PatchEffect};
use mez_agent::permissions::{PathResolutionStatus, PathScopes};
use mez_agent::semantic_patch_planning::{ApplyPatchFileChange, ApplyPatchOriginalState};
use sha2::{Digest, Sha256};

use super::commit::{NativePatchCommitFence, NativePatchCommitGuard};
use crate::error::{MezError, Result};
use crate::security::filesystem::capability::NativeFileCapability;

/// A bounded exact endpoint proposal; it is intent, never mutation evidence.
#[derive(Clone)]
pub(crate) struct NativePatchCommitProposal {
    /// Authored operation occurrence in the immutable accepted patch.
    operation: usize,
    /// Ordered endpoint, with move destination preceding source deletion.
    endpoint: usize,
    /// Authored spelling rather than a later display path.
    path: String,
    /// Physical target selected by the held capability.
    resolved: String,
    /// Exact capture allocation; equal paths/digests cannot substitute objects.
    allocation: Arc<()>,
    /// Missing and an existing empty file remain distinct.
    before: Option<[u8; 32]>,
    /// None denotes deletion; an empty final file has a digest.
    after: Option<[u8; 32]>,
}

impl NativePatchCommitProposal {
    /// Captures exact proposed bytes only after checking the held preimage and
    /// resolved path against the deterministic plan. No filesystem mutation occurs.
    pub(crate) fn capture(
        capability: &NativeFileCapability,
        change: &ApplyPatchFileChange,
        operation: usize,
        endpoint: usize,
    ) -> Result<Self> {
        let before = match &change.original {
            ApplyPatchOriginalState::Missing => None,
            ApplyPatchOriginalState::Regular(bytes) => Some(bytes.as_slice()),
        };
        if capability.preimage() != before
            || capability.path().to_str() != Some(change.resolved_path.as_str())
            || change.path.len() > 4096
            || change.resolved_path.len() > 4096
            || change.final_bytes.as_ref().is_some_and(|bytes| {
                bytes.len() > mez_agent::semantic_patch_planning::native::NATIVE_PATCH_FILE_BYTES
            })
        {
            return Err(MezError::conflict(
                "native commit proposal differs from held capture",
            ));
        }
        Ok(Self {
            operation,
            endpoint,
            path: change.path.clone(),
            resolved: change.resolved_path.clone(),
            allocation: capability.commit_allocation(),
            before: before.map(|bytes| Sha256::digest(bytes).into()),
            after: change
                .final_bytes
                .as_ref()
                .map(|bytes| Sha256::digest(bytes).into()),
        })
    }

    /// Returns a finite semantic occurrence key, independent of path aliases.
    pub(crate) fn key(&self) -> (usize, usize) {
        (self.operation, self.endpoint)
    }

    /// Returns original spelling for current actor path-policy authorization.
    #[cfg(test)]
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    /// Verifies occurrence/kind against the exact actor-owned patch descriptor
    /// and physical target against current resolved read/write authority.
    pub(crate) fn authorize(
        &self,
        identity: &NativeCommitIdentity,
        scopes: &PathScopes,
    ) -> Result<()> {
        let matches = match identity.operation.effects.get(self.operation) {
            Some(PatchEffect::Create(path)) => {
                self.endpoint == 0
                    && self.path == *path
                    && self.before.is_none()
                    && self.after.is_some()
            }
            Some(PatchEffect::Update(path)) => {
                self.endpoint == 0
                    && self.path == *path
                    && self.before.is_some()
                    && self.after.is_some()
            }
            Some(PatchEffect::Delete(path)) => {
                self.endpoint == 0
                    && self.path == *path
                    && self.before.is_some()
                    && self.after.is_none()
            }
            Some(PatchEffect::Move {
                source,
                destination,
            }) => match self.endpoint {
                0 => self.path == *destination && self.after.is_some(),
                1 => self.path == *source && self.before.is_some() && self.after.is_none(),
                _ => false,
            },
            None => false,
        };
        let target = Path::new(&self.resolved);
        let contains =
            |paths: &[String]| paths.iter().any(|path| target.starts_with(Path::new(path)));
        if !matches
            || !target.is_absolute()
            || scopes.resolution_status == PathResolutionStatus::Unresolved
            || !contains(&scopes.read_scopes)
            || !contains(&scopes.write_scopes)
        {
            return Err(MezError::forbidden(
                "native commit endpoint exceeds accepted effect or current scopes",
            ));
        }
        Ok(())
    }

    /// Compares the actual held capture and exact proposed bytes, not Arc values.
    fn same(&self, other: &Self) -> bool {
        self.operation == other.operation
            && self.endpoint == other.endpoint
            && self.path == other.path
            && self.resolved == other.resolved
            && Arc::ptr_eq(&self.allocation, &other.allocation)
            && self.before == other.before
            && self.after == other.after
    }
}

/// A non-cloneable, single-use current actor admission for one endpoint.
pub(crate) struct NativePatchCommitLease {
    /// Exact proposed endpoint and capture allocation authorized by the actor.
    proposal: NativePatchCommitProposal,
    /// Actor scopes clipped to the original issued ceiling.
    scopes: PathScopes,
    /// Short delivery/entry window; an entered syscall may outlive this instant.
    deadline: Instant,
    /// Already-admitted writer, retained until actual commit processing ends.
    guard: NativePatchCommitGuard,
}

impl NativePatchCommitLease {
    /// Issues from current actor authorization and an already acquired guard.
    /// The caller must validate original ownership and semantic duplicates first.
    pub(crate) fn issue(
        proposal: NativePatchCommitProposal,
        scopes: PathScopes,
        guard: NativePatchCommitGuard,
    ) -> Result<Self> {
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(250))
            .ok_or_else(|| MezError::invalid_state("native commit lease deadline overflow"))?;
        Ok(Self {
            proposal,
            scopes,
            deadline,
            guard,
        })
    }

    /// Consumes exact admission once before physical revalidation/publication.
    /// A delayed, cancelled, wrong-capability or wrong-fence lease releases its
    /// guard on error. Success is admission only, never a confirmed file effect.
    pub(crate) fn enter(
        self,
        proposal: &NativePatchCommitProposal,
        fence: &NativePatchCommitFence,
    ) -> Result<(PathScopes, NativePatchCommitGuard)> {
        if Instant::now() >= self.deadline
            || !self.proposal.same(proposal)
            || !fence.owns_guard(&self.guard)
        {
            return Err(MezError::conflict(
                "native commit lease expired or changed identity",
            ));
        }
        fence.check()?;
        Ok((self.scopes, self.guard))
    }
}

#[cfg(test)]
mod tests;
