//! Tracks the directory generation owned by one foreground fixture.
//!
//! An open directory keeps its identity alive across rename and replacement.
//! Retirement compares the current path without following symlinks, and removes
//! storage only after its caller has proved native resource quiescence. These
//! private test roots have no concurrent mutator during verification/removal;
//! this is an ownership check, not a general hostile-filesystem deletion API.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Exact directory generation retained until explicit, verified retirement.
pub(super) struct OwnedRoot {
    /// Original path; clearing it makes repeated retirement inert.
    path: PathBuf,
    /// Open directory prevents inode reuse while this fixture owns it.
    directory: File,
}

impl OwnedRoot {
    /// Captures an existing private directory before a live fixture is spawned.
    /// Rejects a symlink or non-directory instead of adopting its target.
    pub(super) fn capture(path: &Path) -> io::Result<Self> {
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            directory,
        })
    }

    /// Reports whether a previous retirement already released this generation.
    pub(super) fn is_retired(&self) -> bool {
        self.path.as_os_str().is_empty()
    }

    /// Removes the captured generation, preserving replacements and uncertainty.
    ///
    /// An absent path is sufficient only when the held original directory has
    /// also been unlinked. A renamed original remains an outstanding obligation.
    pub(super) fn retire(&mut self) -> io::Result<()> {
        if self.is_retired() {
            return Ok(());
        }
        let owned = self.directory.metadata()?;
        match fs::symlink_metadata(&self.path) {
            Ok(current) => {
                if !current.is_dir() || current.dev() != owned.dev() || current.ino() != owned.ino()
                {
                    return Err(io::Error::other(
                        "fixture root generation changed; replacement retained",
                    ));
                }
                fs::remove_dir_all(&self.path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && owned.nlink() == 0 => {}
            Err(error) => return Err(error),
        }
        self.path.clear();
        Ok(())
    }
}
