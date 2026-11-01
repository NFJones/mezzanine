//! Separate directory admission and truthful positive mkdir ownership.
//!
//! A file grant never authorizes its parent. Capture requires an independently
//! scoped, single missing directory under a held existing parent and the exact
//! original root/fence. Commit returns positive creation before any fallible
//! observation or cancellation check. Callers must retain that fact immediately;
//! failed observation does not undo or erase the directory effect.
//!
//! Portable mkdir returns no descriptor. Observation pins the first stable
//! no-follow directory object seen afterwards; it does not prove an atomic
//! mkdir-to-open identity against arbitrary external writers. Later replacement
//! or relocation of an observed object revokes descendant recapture. This owner
//! never deletes directories, including unobserved/replaced ones, on error/Drop.
//! Actor leases and typed worker/canonical directory receipts are separate
//! integration responsibilities; these primitives alone do not dispatch mkdir.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustix::fs::{AtFlags, FileType, Mode, OFlags, fstat, mkdirat, openat, statat};

use super::{
    File, NativeFileCapability, NativeFilesystemRoot, NativeIoControl, PathScopes, PhysicalPath,
    authorize, resolve_physical, revalidate_ancestry, same_object,
};
use crate::error::{MezError, Result};

/// Bounds separate parent effects without admitting recursive mkdir authority.
pub(crate) const MAX_NATIVE_PARENT_DIRECTORIES: usize = 128;

/// Independently authorized one-entry directory proposal, never a file grant.
pub(crate) struct NativeParentCapability {
    /// Exact root allocation rather than equal PID/cwd labels.
    root: Arc<()>,
    /// Opaque held capture identity for a future exact actor commit proposal.
    allocation: Arc<()>,
    /// Original spelling, retained to reject physical/link retargeting.
    requested: String,
    /// Single missing entry and complete held existing ancestry.
    physical: PhysicalPath,
    /// Original directory authority ceiling, not a file's broader interpretation.
    scopes: PathScopes,
    /// Original cancellation/deadline; capture and later observation never renew it.
    control: NativeIoControl,
}

/// Positive mkdir completion, retained even when later observation fails.
pub(crate) struct NativeCreatedDirectory {
    /// Actual successful syscall's original held parent/path/root admission.
    capability: NativeParentCapability,
    /// First stable post-mkdir observation; absence is not nonexecution.
    observed: Option<File>,
}

impl NativeParentCapability {
    /// Captures exactly one absent directory under explicit read/write scopes.
    /// Rejects unresolved/recursive/existing targets and original-fence failures.
    pub(crate) fn capture(
        root: &NativeFilesystemRoot,
        scopes: &PathScopes,
        requested: &str,
        control: NativeIoControl,
    ) -> Result<Self> {
        control.check()?;
        root.revalidate()?;
        if requested.len() > 4096 {
            return Err(MezError::invalid_args(
                "native parent path exceeds byte budget",
            ));
        }
        let physical = resolve_physical(root.cwd(), requested)?;
        authorize(scopes, root.cwd(), &physical.path, true)?;
        if physical.missing.len() != 1 || physical.path.file_name().is_none() {
            return Err(MezError::conflict(
                "native parent capture requires exactly one missing directory",
            ));
        }
        revalidate_ancestry(&physical)?;
        Ok(Self {
            root: root.allocation.clone(),
            allocation: Arc::new(()),
            requested: requested.to_owned(),
            physical,
            scopes: scopes.clone(),
            control,
        })
    }

    /// Returns the captured physical path as proposal data, never permission.
    pub(crate) fn path(&self) -> &Path {
        &self.physical.path
    }

    /// Returns opaque exact-capture identity for future short actor leases.
    pub(crate) fn commit_allocation(&self) -> Arc<()> {
        self.allocation.clone()
    }

    /// Rechecks original root, independent scopes, existing ancestry and absence.
    /// Equal labels or a competing directory cannot refresh this admission.
    fn revalidate(&self, root: &NativeFilesystemRoot, scopes: &PathScopes) -> Result<()> {
        self.validate_authority(root, scopes)?;
        let current = resolve_physical(root.cwd(), &self.requested)?;
        if current.path != self.physical.path
            || current.missing != self.physical.missing
            || !same_object(&current.stat, &self.physical.stat)
        {
            return Err(MezError::conflict(
                "native parent target or absence changed",
            ));
        }
        Ok(())
    }

    /// Checks immutable root allocation and both original/current directory grants.
    /// Held parent ancestry cannot silently follow relocation or replacement.
    fn validate_authority(&self, root: &NativeFilesystemRoot, scopes: &PathScopes) -> Result<()> {
        self.control.check()?;
        if !Arc::ptr_eq(&self.root, &root.allocation) {
            return Err(MezError::conflict("native parent root allocation changed"));
        }
        root.revalidate()?;
        authorize(&self.scopes, root.cwd(), self.path(), true)?;
        authorize(scopes, root.cwd(), self.path(), true)?;
        revalidate_ancestry(&self.physical)
    }

    /// Performs one no-replace mkdir through its held parent after fresh checks.
    /// Returns positive creation without a fallible post-syscall observation.
    /// The caller must retain this effect before invoking observe or checkpoint.
    pub(crate) fn commit(
        self,
        root: &NativeFilesystemRoot,
        scopes: &PathScopes,
    ) -> Result<NativeCreatedDirectory> {
        self.revalidate(root, scopes)?;
        // Construct the evidence owner before mutation; no later allocation or
        // fallible checkpoint may turn successful mkdir into an Err result.
        let created = NativeCreatedDirectory {
            capability: self,
            observed: None,
        };
        let physical = &created.capability.physical;
        let parent = &physical
            .directories
            .last()
            .ok_or_else(|| MezError::invalid_state("native parent creation lost parent"))?
            .file;
        created.capability.control.check()?;
        mkdirat(
            parent,
            &physical.missing[0],
            Mode::RUSR | Mode::WUSR | Mode::XUSR,
        )
        .map_err(std::io::Error::from)?;
        Ok(created)
    }
}

impl NativeCreatedDirectory {
    /// Returns the positively created path even after budget or authority loss.
    /// This is effect attribution, never access to a descendant or cleanup grant.
    pub(crate) fn path(&self) -> &Path {
        self.capability.path()
    }

    /// Returns actual original admission identity without fabricating a file write.
    pub(crate) fn commit_allocation(&self) -> Arc<()> {
        self.capability.commit_allocation()
    }

    /// Pins the first stable post-mkdir directory through no-follow observation.
    /// Failure leaves positive creation retained and grants no descendant access.
    /// Repeated calls validate the existing witness instead of refreshing it.
    pub(crate) fn observe(
        &mut self,
        root: &NativeFilesystemRoot,
        scopes: &PathScopes,
    ) -> Result<()> {
        if self.observed.is_some() {
            return self.revalidate(root, scopes);
        }
        self.capability.validate_authority(root, scopes)?;
        let physical = &self.capability.physical;
        let parent = &physical
            .directories
            .last()
            .ok_or_else(|| MezError::invalid_state("created native directory lost parent"))?
            .file;
        let name = &physical.missing[0];
        let before =
            statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(std::io::Error::from)?;
        if FileType::from_raw_mode(before.st_mode) != FileType::Directory {
            return Err(MezError::conflict(
                "created native directory is no longer a directory",
            ));
        }
        let file = File::from(
            openat(
                parent,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let held = fstat(&file).map_err(std::io::Error::from)?;
        let after =
            statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(std::io::Error::from)?;
        if !same_object(&before, &held) || !same_object(&held, &after) {
            return Err(MezError::conflict(
                "created native directory changed during observation",
            ));
        }
        self.capability.control.check()?;
        self.observed = Some(file);
        Ok(())
    }

    /// Rechecks exact held post-mkdir observation and current separate authority.
    /// Path equality cannot substitute a replacement directory or moved ancestor.
    pub(crate) fn revalidate(
        &self,
        root: &NativeFilesystemRoot,
        scopes: &PathScopes,
    ) -> Result<()> {
        self.capability.validate_authority(root, scopes)?;
        let file = self.observed.as_ref().ok_or_else(|| {
            MezError::conflict("created native directory has no held observation")
        })?;
        let physical = &self.capability.physical;
        let parent = &physical
            .directories
            .last()
            .ok_or_else(|| MezError::invalid_state("created native directory lost parent"))?
            .file;
        let current = statat(parent, &physical.missing[0], AtFlags::SYMLINK_NOFOLLOW)
            .map_err(std::io::Error::from)?;
        if !same_object(&current, &fstat(file).map_err(std::io::Error::from)?) {
            return Err(MezError::conflict(
                "created native directory observation was replaced",
            ));
        }
        self.capability.control.check()
    }
}

impl NativeFileCapability {
    /// Checks that a separate parent target also fits the original file action's
    /// scope ceiling; a newly broader directory snapshot cannot widen it.
    pub(crate) fn authorize_parent_ceiling(&self, parent: &Path) -> Result<()> {
        self.control.check()?;
        authorize(
            &self.scopes,
            Path::new(&self.scopes.current_directory),
            parent,
            true,
        )
    }

    /// Returns bounded missing parent intentions, excluding the final file leaf.
    /// These paths grant no directory access and cannot execute recursive mkdir.
    pub(crate) fn missing_parent_paths(&self) -> Result<Vec<PathBuf>> {
        self.control.check()?;
        let count = self.physical.missing.len().saturating_sub(1);
        if count > MAX_NATIVE_PARENT_DIRECTORIES {
            return Err(MezError::invalid_args(
                "native missing parent count exceeds budget",
            ));
        }
        let mut path = self
            .physical
            .directories
            .last()
            .ok_or_else(|| MezError::invalid_state("native missing file lost parent"))?
            .path
            .clone();
        let mut paths = Vec::new();
        for component in self.physical.missing.iter().take(count) {
            self.control.check()?;
            path.push(component);
            if path.as_os_str().len() > 4096 {
                return Err(MezError::invalid_args(
                    "native missing parent path exceeds byte budget",
                ));
            }
            paths.push(path.clone());
        }
        Ok(paths)
    }

    /// Evolves a captured missing target only through separately held positive
    /// directory effects. Preserves original root ancestry, path and scope ceiling.
    /// Competing files or unrecorded/replaced new ancestors reject the transfer.
    pub(crate) fn recapture_with_created_parents(
        &self,
        root: &NativeFilesystemRoot,
        scopes: &PathScopes,
        created: &[NativeCreatedDirectory],
    ) -> Result<Self> {
        self.control.check()?;
        if !self.writable || self.preimage.is_some() {
            return Err(MezError::conflict(
                "native parent transfer requires an original missing write target",
            ));
        }
        root.revalidate()?;
        authorize(&self.scopes, root.cwd(), self.path(), true)?;
        authorize(scopes, root.cwd(), self.path(), true)?;
        revalidate_ancestry(&self.physical)?;
        let mut next = Self::capture_with_io_control(
            root,
            scopes,
            &self.requested,
            true,
            self.control.clone(),
        )?;
        if next.path() != self.path()
            || next.preimage.is_some()
            || next.physical.directories.len() < self.physical.directories.len()
        {
            return Err(MezError::conflict(
                "native parent transfer changed missing target",
            ));
        }
        for (index, current) in next.physical.directories.iter().enumerate() {
            self.control.check()?;
            if let Some(original) = self.physical.directories.get(index) {
                if original.path != current.path
                    || !same_object(
                        &fstat(&original.file).map_err(std::io::Error::from)?,
                        &fstat(&current.file).map_err(std::io::Error::from)?,
                    )
                {
                    return Err(MezError::conflict(
                        "native parent transfer changed original ancestry",
                    ));
                }
            } else {
                let witness = created
                    .iter()
                    .find(|witness| witness.path() == current.path)
                    .ok_or_else(|| {
                        MezError::conflict("native parent transfer has an unrecorded ancestor")
                    })?;
                witness.revalidate(root, scopes)?;
                let held = witness.observed.as_ref().ok_or_else(|| {
                    MezError::conflict("native parent transfer lacks observation")
                })?;
                if !same_object(
                    &fstat(held).map_err(std::io::Error::from)?,
                    &fstat(&current.file).map_err(std::io::Error::from)?,
                ) {
                    return Err(MezError::conflict(
                        "native parent transfer observation changed",
                    ));
                }
            }
        }
        next.scopes = self.scopes.clone();
        self.control.check()?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests;
