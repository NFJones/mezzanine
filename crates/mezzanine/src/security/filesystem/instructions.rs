//! Bounded, scope-qualified instruction reads through held physical parents.
//!
//! This harness adapter runs on a preparation worker. It reuses native physical
//! resolution and ancestry checks, authorizes the canonical source before reading,
//! and never starts a helper, follows a replacement link, or changes cwd. Reads
//! are point-in-time snapshots, not claims that external writers are excluded.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use mez_agent::instructions::DiscoveredInstructionFile;
use mez_agent::permissions::{PathResolutionStatus, PathScopes};
use rustix::fs::{FileType, Mode, OFlags, fstat, openat};

use super::capability::revalidate_ancestry;
use super::resolution::{resolve_physical, same_object};
use crate::error::{MezError, Result};

/// Retains the existing bootstrap's finite per-file read limit independently
/// of the model snapshot's configurable aggregate content budget.
pub(crate) const INSTRUCTION_FILE_READ_LIMIT: u64 = 32_768;

/// Reads one regular instruction file under resolved read authority. Missing
/// files return `None`; special nodes, inaccessible/out-of-scope targets, invalid
/// UTF-8, and changed objects fail without publishing partial discovery. The
/// logical scope is supplied separately because a same-project link may point
/// to shared content without changing the scope of its directory entry.
pub(crate) fn read_scoped_instruction_file(
    scopes: &PathScopes,
    requested: &str,
    logical_scope: &Path,
) -> Result<Option<DiscoveredInstructionFile>> {
    if scopes.resolution_status == PathResolutionStatus::Unresolved {
        return Err(MezError::forbidden(
            "instruction discovery requires resolved read authority",
        ));
    }
    let physical = resolve_physical(Path::new(&scopes.current_directory), requested)?;
    if !scopes
        .read_scopes
        .iter()
        .any(|root| physical.path.starts_with(root))
    {
        return Err(MezError::forbidden(
            "instruction source exceeds current read scopes",
        ));
    }
    if !physical.missing.is_empty() {
        return Ok(None);
    }
    if FileType::from_raw_mode(physical.stat.st_mode) == FileType::Directory {
        return Ok(None);
    }
    if FileType::from_raw_mode(physical.stat.st_mode) != FileType::RegularFile {
        return Err(MezError::invalid_args(
            "instruction source must be a regular file",
        ));
    }
    let parent = physical
        .directories
        .last()
        .ok_or_else(|| MezError::invalid_state("instruction source parent is unavailable"))?;
    let name = physical
        .path
        .file_name()
        .ok_or_else(|| MezError::invalid_args("instruction source cannot be a filesystem root"))?;
    let descriptor = openat(
        &parent.file,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let mut file = File::from(descriptor);
    let stat = fstat(&file).map_err(std::io::Error::from)?;
    if !same_object(&stat, &physical.stat)
        || FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
    {
        return Err(MezError::conflict(
            "instruction source changed before reading",
        ));
    }
    let before = file.metadata()?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(INSTRUCTION_FILE_READ_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if before.len() != after.len() || before.modified()? != after.modified()? {
        return Err(MezError::conflict(
            "instruction source changed while reading",
        ));
    }
    let truncated = before.len() > INSTRUCTION_FILE_READ_LIMIT
        || bytes.len() as u64 > INSTRUCTION_FILE_READ_LIMIT;
    bytes.truncate(INSTRUCTION_FILE_READ_LIMIT as usize);
    let content = match String::from_utf8(bytes) {
        Ok(content) => content,
        Err(error) if truncated && error.utf8_error().error_len().is_none() => {
            let valid = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).map_err(|_| {
                MezError::invalid_state("instruction UTF-8 boundary validation failed")
            })?
        }
        Err(_) => return Err(MezError::invalid_args("instruction content is not UTF-8")),
    };
    revalidate_ancestry(&physical)?;
    let path = physical
        .path
        .to_str()
        .filter(|text| !text.chars().any(char::is_control))
        .ok_or_else(|| {
            MezError::invalid_args("instruction path must be representable without controls")
        })?;
    let scope = logical_scope
        .to_str()
        .filter(|text| !text.chars().any(char::is_control))
        .ok_or_else(|| {
            MezError::invalid_args("instruction scope must be representable without controls")
        })?;
    Ok(Some(DiscoveredInstructionFile {
        path: path.to_string(),
        scope_root: scope.to_string(),
        bytes: usize::try_from(before.len())
            .map_err(|_| MezError::invalid_args("instruction size is not representable"))?,
        truncated,
        content,
    }))
}
