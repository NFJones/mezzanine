//! Native directory/absence witnesses for bootstrap inspection and creation.
//!
//! Existing roots retain exact no-follow descriptors. Missing roots retain the
//! closest existing ancestor, its spelling/incarnation, and bounded uncreated
//! components. Read-only validation requires that original first missing entry
//! stay absent. Explicit installation may create only those components using
//! held directory handles; competing creation never supplies implicit ownership.
//! No symlink traversal, cwd mutation, chmod/chown, helpers or recursive cleanup.
//! Creation is not an atomic tree transaction: partial created directories may
//! remain on error; no foreign/nonempty nodes are deleted to hide that outcome.
//! Existing root eligibility remains the current publisher policy, independently
//! tracked for migration; no ownership/mode eligibility is imposed on ancestors.

use crate::error::{MezError, Result};
use rustix::fs::{AtFlags, Mode, OFlags, mkdirat, openat, statat};
use std::ffi::OsString;
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

/// Exact existing root or original bounded ancestor/absence witness.
pub(super) enum RootDirectory {
    /// Held final root; never substituted by an unqualified spelling.
    Existing(File),
    /// No root I/O is directed at this ancestor as if it were the vendor root.
    Missing {
        ancestor: File,
        spelling: PathBuf,
        suffix: Vec<OsString>,
    },
}

/// Shared no-follow absolute walk. NOENT alone means absence; all other errors
/// retain their I/O cause. Paths/components are finite before any filesystem I/O.
pub(super) fn walk(path: &Path) -> Result<(File, PathBuf, Vec<OsString>)> {
    if !path.is_absolute() || path.as_os_str().as_bytes().len() > 4096 {
        return Err(MezError::invalid_args(
            "bootstrap root must be bounded and absolute",
        ));
    }
    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name) => parts.push(name.to_os_string()),
            _ => return Err(MezError::forbidden("bootstrap root traversal rejected")),
        }
    }
    if parts.len() > 256 {
        return Err(MezError::invalid_args("bootstrap root component limit"));
    }
    let mut directory = File::open("/")?;
    let mut spelling = PathBuf::from("/");
    for (index, part) in parts.iter().enumerate() {
        match openat(
            &directory,
            part,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(child) => {
                directory = File::from(child);
                spelling.push(part);
            }
            Err(rustix::io::Errno::NOENT) => {
                return Ok((directory, spelling, parts[index..].to_vec()));
            }
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
    }
    Ok((directory, spelling, Vec::new()))
}

/// Preserves current root/new-private-directory policy, never policing existing
/// ancestors. Fresh suffix handles must remain owned/private before descent.
fn eligible(directory: &File) -> Result<()> {
    let metadata = directory.metadata()?;
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
        return Err(MezError::forbidden(
            "bootstrap root must be owned and not writable by other users",
        ));
    }
    Ok(())
}

impl RootDirectory {
    /// Captures a root/absence witness without creating any directory/state.
    pub(super) fn inspect(root: &Path) -> Result<Self> {
        let (directory, spelling, suffix) = walk(root)?;
        if suffix.is_empty() {
            eligible(&directory)?;
            Ok(Self::Existing(directory))
        } else {
            Ok(Self::Missing {
                ancestor: directory,
                spelling,
                suffix,
            })
        }
    }

    /// Reports captured absence, not a new mutable focus/path lookup.
    pub(super) fn is_missing(&self) -> bool {
        matches!(self, Self::Missing { .. })
    }

    /// Only existing roots expose a final directory for publication primitives.
    /// Missing ancestor descriptors can never accidentally receive root files.
    pub(super) fn file(&self) -> Result<&File> {
        match self {
            Self::Existing(directory) => Ok(directory),
            Self::Missing { .. } => Err(MezError::invalid_state(
                "bootstrap root is not materialized",
            )),
        }
    }

    /// Revalidates the exact root or original ancestor and missing entry.
    /// Appeared entries (including symlinks/special nodes) are conflicts, not
    /// replacement authority; unreadable trees retain actual I/O errors.
    pub(super) fn validate(&self, root: &Path) -> Result<()> {
        let (held, current, suffix) = match self {
            Self::Existing(directory) => (directory, walk(root)?, None),
            Self::Missing {
                ancestor,
                spelling,
                suffix,
            } => (ancestor, walk(spelling)?, Some(suffix)),
        };
        let original = held.metadata()?;
        let observed = current.0.metadata()?;
        if !current.2.is_empty()
            || original.dev() != observed.dev()
            || original.ino() != observed.ino()
        {
            return Err(MezError::conflict(
                "bootstrap root or ancestor replaced; reinspection required",
            ));
        }
        if let Some(suffix) = suffix {
            let first = suffix.first().ok_or_else(|| {
                MezError::invalid_state("bootstrap absence witness missing suffix")
            })?;
            match statat(held, first, AtFlags::SYMLINK_NOFOLLOW) {
                Err(rustix::io::Errno::NOENT) => Ok(()),
                Ok(_) => Err(MezError::conflict(
                    "bootstrap missing root entry appeared; reinspection required",
                )),
                Err(error) => Err(std::io::Error::from(error).into()),
            }
        } else {
            eligible(&current.0)
        }
    }

    /// Materializes only the captured absent suffix after native revalidation.
    /// Existing directories are never chmod/chown'd; CREATE collisions reject.
    /// Root descriptor/spelling is checked again before any lock/artifact work.
    pub(super) fn materialize(&mut self, root: &Path) -> Result<()> {
        self.validate(root)?;
        let Self::Missing {
            ancestor, suffix, ..
        } = self
        else {
            return Ok(());
        };
        let mut directory = ancestor.try_clone()?;
        for part in suffix {
            mkdirat(
                &directory,
                part.as_os_str(),
                Mode::RUSR | Mode::WUSR | Mode::XUSR,
            )
            .map_err(|error| {
                if error == rustix::io::Errno::EXIST {
                    MezError::conflict("bootstrap root creation raced; reinspection required")
                } else {
                    std::io::Error::from(error).into()
                }
            })?;
            directory.sync_all()?;
            directory = File::from(
                openat(
                    &directory,
                    part.as_os_str(),
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(std::io::Error::from)?,
            );
            eligible(&directory)?;
        }
        eligible(&directory)?;
        *self = Self::Existing(directory);
        self.validate(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;

    /// OS-writable shared ancestors may host new privately created roots. Their
    /// mode is neither an eligibility gate nor normalized to installer needs;
    /// only created suffix directories receive the safe creation mode.
    #[test]
    fn bootstrap_root_directory_creation_preserves_shared_ancestor_mode() {
        let parent = parent();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        let path = parent.join("config/vendor");
        let mut owner = RootDirectory::inspect(&path).unwrap();
        assert_eq!(std::fs::metadata(&parent).unwrap().mode() & 0o777, 0o777);
        owner.materialize(&path).unwrap();
        assert_eq!(std::fs::metadata(&parent).unwrap().mode() & 0o777, 0o777);
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o700);
        std::fs::remove_dir_all(parent).unwrap();
    }

    /// Unique owned fixture roots avoid global cwd/env and fixed absolute paths.
    fn parent() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mez-root-witness-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }

    /// Two previews cannot acquire the same initially absent entry through
    /// implicit adoption. One creates its suffix; the other must reject without
    /// overwriting its files or using the appeared tree as original evidence.
    #[test]
    fn bootstrap_root_directory_competing_creation_is_not_adopted() {
        let parent = parent();
        let path = parent.join("config/vendor");
        let mut first = RootDirectory::inspect(&path).unwrap();
        let mut second = RootDirectory::inspect(&path).unwrap();
        assert!(first.is_missing());
        assert!(first.file().is_err());
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 0);
        first.materialize(&path).unwrap();
        std::fs::write(path.join("authored"), b"preserve").unwrap();
        assert!(second.validate(&path).is_err());
        assert!(second.materialize(&path).is_err());
        assert_eq!(std::fs::read(path.join("authored")).unwrap(), b"preserve");
        std::fs::remove_dir_all(parent).unwrap();
    }

    /// Replacing the closest existing ancestor must not retarget creation or
    /// use a relocated descriptor to mutate the old tree after spelling drift.
    #[test]
    fn bootstrap_root_directory_replaced_ancestor_rejects_before_creation() {
        let parent = parent();
        let path = parent.join("config/vendor");
        let mut owner = RootDirectory::inspect(&path).unwrap();
        let moved = parent.with_extension("moved");
        std::fs::rename(&parent, &moved).unwrap();
        std::fs::create_dir(&parent).unwrap();
        assert!(owner.materialize(&path).is_err());
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 0);
        std::fs::remove_dir_all(parent).unwrap();
        std::fs::remove_dir_all(moved).unwrap();
    }

    /// A newly appeared symlink or regular node at the absent entry is not a
    /// creatable directory. Initial no-follow inspection also rejects a symlink
    /// ancestor; forbidden destinations remain untouched without a retry path.
    #[test]
    fn bootstrap_root_directory_unsafe_appeared_entries_stay_unchanged() {
        let parent = parent();
        let outside = parent.join("outside");
        std::fs::create_dir(&outside).unwrap();
        let path = parent.join("config/vendor");
        for link in [true, false] {
            let mut owner = RootDirectory::inspect(&path).unwrap();
            if link {
                symlink(&outside, parent.join("config")).unwrap();
            } else {
                std::fs::write(parent.join("config"), b"foreign").unwrap();
            }
            assert!(owner.materialize(&path).is_err());
            assert!(RootDirectory::inspect(&path).is_err());
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
            std::fs::remove_file(parent.join("config")).unwrap();
        }
        std::fs::remove_dir_all(parent).unwrap();
    }
}
