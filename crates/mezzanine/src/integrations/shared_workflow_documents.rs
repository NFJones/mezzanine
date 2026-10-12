//! Descriptor-qualified reads for the two compiled `.agents` workflow formats.
//!
//! Callers select a currently authorized project root and an existing format;
//! this module grants no trust or execution authority. It protects the namespace,
//! catalog, entry and document with held no-follow descriptors and finite reads.
//! It discovers no plugins, scripts, configuration or repository instructions.
//! Legacy user/native readers remain with their existing subsystem owners.

use crate::{MezError, Result};
use rustix::fs::{Mode, OFlags, openat};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path};

/// The only admitted shared workflow placements, with compiled format limits.
#[derive(Clone, Copy)]
pub(crate) enum SharedWorkflowDocument {
    /// Existing Mezzanine skill document and its established 1 MiB limit.
    Skill,
    /// Existing Mezzanine macro document and its established 256 KiB limit.
    Macro,
}

impl SharedWorkflowDocument {
    /// Returns the fixed catalog directory, leaf file and complete byte limit.
    fn format(self) -> (&'static str, &'static str, u64) {
        match self {
            Self::Skill => ("skills", mez_agent::SKILL_FILE_NAME, 1024 * 1024),
            Self::Macro => (
                "macros",
                mez_agent::MACRO_FILE_NAME,
                mez_agent::MAX_MACRO_FILE_BYTES,
            ),
        }
    }

    /// Applies the existing format's direct-child name grammar before I/O.
    fn valid_name(self, name: &str) -> bool {
        match self {
            Self::Skill => mez_agent::is_valid_skill_name(name),
            Self::Macro => mez_agent::is_valid_macro_name(name),
        }
    }
}

/// Opens the fixed namespace below the caller-selected project base. Base aliases
/// retain existing shared-skill semantics, but every protected suffix and opened
/// canonical ancestor rejects links, special nodes and traversal spellings.
fn open_root(root: &Path, format: SharedWorkflowDocument) -> Result<File> {
    let (catalog, _, _) = format.format();
    let namespace = root
        .parent()
        .ok_or_else(|| MezError::forbidden("shared workflow root unavailable"))?;
    if root.file_name().is_none_or(|name| name != catalog)
        || namespace.file_name().is_none_or(|name| name != ".agents")
        || (matches!(format, SharedWorkflowDocument::Macro)
            && root
                .components()
                .any(|component| matches!(component, Component::ParentDir)))
    {
        return Err(MezError::forbidden("shared workflow root unavailable"));
    }
    let base = namespace
        .parent()
        .ok_or_else(|| MezError::forbidden("shared workflow root unavailable"))?;
    let canonical = std::fs::canonicalize(base)?.join(".agents").join(catalog);
    let mut directory = File::open("/")?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    for component in canonical.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(segment) => {
                directory = File::from(
                    openat(&directory, segment, flags, Mode::empty())
                        .map_err(std::io::Error::from)?,
                );
            }
            _ => return Err(MezError::forbidden("shared workflow root unavailable")),
        }
    }
    Ok(directory)
}

/// Lists sorted direct-child names through the held namespace descriptor. The
/// established 4096-entry bound prevents unbounded shared catalog enumeration;
/// missing/unsafe roots are returned to the caller for inert/diagnostic handling.
pub(crate) fn entry_names(
    root: &Path,
    format: SharedWorkflowDocument,
) -> Result<Vec<std::ffi::OsString>> {
    use std::os::unix::ffi::OsStrExt;
    let directory = open_root(root, format)?;
    let entries = rustix::fs::Dir::read_from(&directory).map_err(std::io::Error::from)?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(std::io::Error::from)?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if names.len() == 4096 {
            return Err(MezError::invalid_args(
                "shared workflow catalog exceeds entry limit",
            ));
        }
        names.push(std::ffi::OsStr::from_bytes(name).to_os_string());
    }
    names.sort();
    Ok(names)
}

/// Reads only the fixed regular UTF-8 leaf through descriptor-relative opens.
/// NONBLOCK prevents FIFO/device startup waits; fstat qualifies the opened node,
/// and the limit+1 reader detects growth after the initial size observation.
pub(crate) fn read(root: &Path, name: &str, format: SharedWorkflowDocument) -> Result<String> {
    if !format.valid_name(name) {
        return Err(MezError::invalid_args("shared workflow name unavailable"));
    }
    let (_, document, limit) = format.format();
    let directory = open_root(root, format)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let entry =
        File::from(openat(&directory, name, flags, Mode::empty()).map_err(std::io::Error::from)?);
    let file = File::from(
        openat(
            &entry,
            document,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(MezError::invalid_args(
            "shared workflow document unavailable or oversized",
        ));
    }
    read_bounded(file, limit)
}

/// Finite read over an already qualified descriptor, independent of path or
/// later size changes. Kept separate so growth can be deterministically tested.
fn read_bounded(mut file: File, limit: u64) -> Result<String> {
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(MezError::invalid_args("shared workflow document oversized"));
    }
    String::from_utf8(bytes)
        .map_err(|_| MezError::invalid_args("shared workflow document is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A descriptor observed below the limit can grow before its actual read.
    /// The shared reader must reject the extra byte without relying on stale
    /// metadata or allocating/reading the unbounded replacement document.
    #[test]
    fn shared_workflow_opened_document_growth_is_bounded() {
        let path = std::env::temp_dir().join(format!(
            "mez-shared-growth-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::write(&path, "small").unwrap();
        let file = File::open(&path).unwrap();
        assert!(file.metadata().unwrap().len() < 64);
        std::fs::write(&path, vec![b'x'; 4096]).unwrap();
        assert!(read_bounded(file, 64).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
