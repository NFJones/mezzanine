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

use super::commit::{NativePatchCommitFence, NativePatchCommitGuard};
use crate::error::{MezError, Result};
use crate::security::filesystem::capability::NativeFileCapability;
use crate::security::filesystem::capability::parent::{
    MAX_NATIVE_PARENT_DIRECTORIES, NativeParentCapability,
};

/// Mutation purpose remains separate from file-byte evidence. Parent intent is
/// constrained to a prepared missing create/move destination and original scope.
#[derive(Clone)]
enum NativePatchCommitPurpose {
    /// Existing exact authored file endpoint.
    File,
    /// Explicit separate ancestor creation; never an implied file write.
    Parent {
        /// Original authored destination spelling from the accepted operation.
        intended_path: String,
        /// Physical missing file target captured before creating ancestors.
        intended_resolved: String,
        /// Exact prepared file capture; equal pathname labels cannot substitute it.
        intended_allocation: Arc<()>,
        /// Bounded separately admitted parent occurrence within the action.
        ordinal: usize,
    },
}

/// A bounded exact endpoint proposal; it is intent, never mutation evidence.
#[derive(Clone)]
pub(crate) struct NativePatchCommitProposal {
    /// Typed mutation purpose; directory proposals carry no fake file bytes.
    purpose: NativePatchCommitPurpose,
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
        if !capability.preimage_matches(before)?
            || capability.requested_path() != change.path
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
            purpose: NativePatchCommitPurpose::File,
            operation,
            endpoint,
            path: change.path.clone(),
            resolved: change.resolved_path.clone(),
            allocation: capability.commit_allocation(),
            before: before
                .map(|bytes| capability.planned_digest(bytes))
                .transpose()?,
            after: change
                .final_bytes
                .as_ref()
                .map(|bytes| capability.planned_digest(bytes))
                .transpose()?,
        })
    }

    /// Captures one separately scoped missing parent of an exact create target.
    /// Requires the first missing parent, original scope ceiling and held capture
    /// allocation; it does not grant mkdir or manufacture a file-byte proof.
    pub(crate) fn capture_parent(
        parent: &NativeParentCapability,
        intended: &NativeFileCapability,
        change: &ApplyPatchFileChange,
        operation: usize,
        parent_ordinal: usize,
    ) -> Result<Self> {
        intended.authorize_parent_ceiling(parent.path())?;
        if parent_ordinal >= MAX_NATIVE_PARENT_DIRECTORIES
            || !matches!(change.original, ApplyPatchOriginalState::Missing)
            || change.final_bytes.is_none()
            || intended.preimage().is_some()
            || intended.requested_path() != change.path
            || intended.path().to_str() != Some(change.resolved_path.as_str())
            || intended
                .missing_parent_paths()?
                .first()
                .map(std::path::PathBuf::as_path)
                != Some(parent.path())
            || change.path.len() > 4096
            || change.resolved_path.len() > 4096
        {
            return Err(MezError::conflict(
                "native parent proposal differs from prepared missing destination",
            ));
        }
        let path = parent
            .path()
            .to_str()
            .ok_or_else(|| MezError::invalid_args("native parent proposal requires UTF-8"))?
            .to_owned();
        Ok(Self {
            purpose: NativePatchCommitPurpose::Parent {
                intended_path: change.path.clone(),
                intended_resolved: change.resolved_path.clone(),
                intended_allocation: intended.commit_allocation(),
                ordinal: parent_ordinal,
            },
            operation,
            endpoint: 0,
            path: path.clone(),
            resolved: path,
            allocation: parent.commit_allocation(),
            before: None,
            after: None,
        })
    }

    /// Returns a finite semantic occurrence key, independent of path aliases.
    pub(crate) fn key(&self) -> (usize, usize) {
        (
            self.operation,
            match &self.purpose {
                NativePatchCommitPurpose::File => self.endpoint,
                NativePatchCommitPurpose::Parent { ordinal, .. } => 2 + ordinal,
            },
        )
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
        if let NativePatchCommitPurpose::Parent {
            intended_path,
            intended_resolved,
            ordinal,
            ..
        } = &self.purpose
        {
            let accepted = match identity.operation.effects.get(self.operation) {
                Some(PatchEffect::Create(path)) => path == intended_path,
                Some(PatchEffect::Move { destination, .. }) => destination == intended_path,
                _ => false,
            };
            let parent = Path::new(&self.resolved);
            let child = Path::new(intended_resolved);
            let normal = |path: &Path| {
                path.is_absolute()
                    && path
                        .components()
                        .collect::<std::path::PathBuf>()
                        .as_os_str()
                        == path.as_os_str()
                    && !path.components().any(|component| {
                        matches!(
                            component,
                            std::path::Component::CurDir | std::path::Component::ParentDir
                        )
                    })
            };
            let contains = |paths: &[String], target: &Path| {
                paths.iter().any(|path| target.starts_with(Path::new(path)))
            };
            if !accepted
                || *ordinal >= MAX_NATIVE_PARENT_DIRECTORIES
                || self.endpoint != 0
                || self.path != self.resolved
                || self.before.is_some()
                || self.after.is_some()
                || !normal(parent)
                || !normal(child)
                || parent == child
                || !child.starts_with(parent)
                || scopes.resolution_status == PathResolutionStatus::Unresolved
                || !contains(&scopes.read_scopes, parent)
                || !contains(&scopes.write_scopes, parent)
                || !contains(&scopes.read_scopes, child)
                || !contains(&scopes.write_scopes, child)
            {
                return Err(MezError::forbidden(
                    "native parent commit exceeds accepted destination or separate scopes",
                ));
            }
            return Ok(());
        }
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
                0 => self.path == *destination && self.before.is_none() && self.after.is_some(),
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
        self.same_purpose(other)
            && self.operation == other.operation
            && self.endpoint == other.endpoint
            && self.path == other.path
            && self.resolved == other.resolved
            && Arc::ptr_eq(&self.allocation, &other.allocation)
            && self.before == other.before
            && self.after == other.after
    }

    /// Compares exact parent/file purpose including both opaque allocations.
    /// Arc values are not identity: equal unit payloads must never substitute.
    fn same_purpose(&self, other: &Self) -> bool {
        match (&self.purpose, &other.purpose) {
            (NativePatchCommitPurpose::File, NativePatchCommitPurpose::File) => true,
            (
                NativePatchCommitPurpose::Parent {
                    intended_path: left,
                    intended_resolved: left_resolved,
                    intended_allocation: left_capture,
                    ordinal: left_ordinal,
                },
                NativePatchCommitPurpose::Parent {
                    intended_path: right,
                    intended_resolved: right_resolved,
                    intended_allocation: right_capture,
                    ordinal: right_ordinal,
                },
            ) => {
                left == right
                    && left_resolved == right_resolved
                    && left_ordinal == right_ordinal
                    && Arc::ptr_eq(left_capture, right_capture)
            }
            _ => false,
        }
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
