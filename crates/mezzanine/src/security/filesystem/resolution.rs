//! Portable descriptor-relative physical path resolution for Linux and macOS.
//!
//! Expand links before interpreting subsequent parent components. Only ENOENT
//! creates missing-target evidence; permission, I/O, loops and non-directory
//! ancestors fail closed. This is point-in-time evidence, not atomic authority
//! against arbitrary external renames. No shell or process helper is used.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use mez_agent::permissions::{
    PathScopes, ResolvedPathEvidence, ResolvedPathKind, ResolvedPathObjectKind,
};
use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat, openat, readlinkat, statat};

use crate::error::{MezError, Result};

/// Maximum symlink expansions, matching the existing Unix resolution budget.
const MAX_LINKS: usize = 40;
/// Bounds path components and link expansion independently of input byte limits.
const MAX_COMPONENTS: usize = 4096;

/// Captured directory object retained during descriptor-relative resolution.
pub(super) struct DirectoryWitness {
    /// Physical spelling when this object was captured.
    pub(super) path: PathBuf,
    /// Held directory handle; never follow a replacement symlink.
    pub(super) file: File,
}

/// Physical resolution and held parent objects for a later capability owner.
pub(super) struct PhysicalPath {
    /// Canonical existing or missing destination spelling.
    pub(super) path: PathBuf,
    /// Held chain from root through the existing parent.
    pub(super) directories: Vec<DirectoryWitness>,
    /// Remaining components absent at capture, or empty for existing targets.
    pub(super) missing: Vec<OsString>,
    /// Last existing object metadata, without following a second pathname.
    pub(super) stat: Stat,
}

/// Opens one directory relative to a held parent without following symlinks.
fn open_directory(parent: &File, name: &Path) -> Result<File> {
    openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| MezError::invalid_state(format!("native path directory open failed: {error}")))
}

/// Copies components without collapsing link-bearing parent traversal.
fn components(path: &Path) -> Result<VecDeque<OsString>> {
    let mut result = VecDeque::new();
    // Path::components normalizes trailing `/.`, which would turn a regular
    // file's invalid `file/.` lookup into apparently valid file authority.
    for part in path.as_os_str().as_bytes().split(|byte| *byte == b'/') {
        if !part.is_empty() {
            result.push_back(std::ffi::OsStr::from_bytes(part).to_os_string());
        }
    }
    if result.len() > MAX_COMPONENTS {
        return Err(MezError::invalid_args(
            "native path component budget exceeded",
        ));
    }
    Ok(result)
}

/// Resolves physical objects relative to cwd with bounded symlink expansion.
/// Missing suffixes may contain only normal components, not unresolved `..`.
pub(super) fn resolve_physical(current_directory: &Path, requested: &str) -> Result<PhysicalPath> {
    if requested.is_empty()
        || requested.contains('\0')
        || requested.starts_with('~')
        || !current_directory.is_absolute()
    {
        return Err(MezError::invalid_args(
            "native path resolution requires an absolute cwd and a non-empty, unexpanded path without NUL bytes",
        ));
    }
    let requested_path = Path::new(requested);
    let joined = if requested_path.is_absolute() {
        requested_path.to_path_buf()
    } else {
        current_directory.join(requested_path)
    };
    let mut pending = components(&joined)?;
    let root = File::from(
        rustix::fs::open(
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    let mut directories = vec![DirectoryWitness {
        path: PathBuf::from("/"),
        file: root,
    }];
    let mut links = 0;
    let mut expanded_components = 0;
    while let Some(name) = pending.pop_front() {
        expanded_components += 1;
        if expanded_components > MAX_COMPONENTS {
            return Err(MezError::invalid_args(
                "native path expansion budget exceeded",
            ));
        }
        if name == "." {
            continue;
        }
        if name == ".." {
            if directories.len() > 1 {
                directories.pop();
            }
            continue;
        }
        let parent = directories
            .last()
            .ok_or_else(|| MezError::invalid_state("native resolver lost root handle"))?;
        let path = parent.path.join(&name);
        let stat = match statat(&parent.file, &name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => {
                let mut missing = vec![name];
                missing.extend(pending);
                if missing.iter().any(|part| part == ".." || part == ".") {
                    return Err(MezError::invalid_args(
                        "native path parent traversal through a missing component is unresolved",
                    ));
                }
                let stat = rustix::fs::fstat(&parent.file).map_err(std::io::Error::from)?;
                let path = missing
                    .iter()
                    .fold(parent.path.clone(), |path, part| path.join(part));
                return Ok(PhysicalPath {
                    path,
                    directories,
                    missing,
                    stat,
                });
            }
            Err(error) => {
                return Err(MezError::invalid_state(format!(
                    "native path metadata failed: {error}"
                )));
            }
        };
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::Symlink => {
                links += 1;
                if links > MAX_LINKS {
                    return Err(MezError::invalid_state(
                        "native path symlink expansion limit exceeded",
                    ));
                }
                let target =
                    readlinkat(&parent.file, &name, Vec::new()).map_err(std::io::Error::from)?;
                let again = statat(&parent.file, &name, AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(std::io::Error::from)?;
                if !same_object(&stat, &again) {
                    return Err(MezError::conflict(
                        "native path symlink changed during resolution",
                    ));
                }
                let target = Path::new(std::ffi::OsStr::from_bytes(target.to_bytes()));
                let mut expanded = components(target)?;
                expanded.append(&mut pending);
                pending = expanded;
                if target.is_absolute() {
                    directories.truncate(1);
                }
            }
            FileType::Directory => {
                let file = open_directory(&parent.file, Path::new(&name))?;
                let opened = rustix::fs::fstat(&file).map_err(std::io::Error::from)?;
                if !same_object(&stat, &opened) {
                    return Err(MezError::conflict(
                        "native path ancestor changed during resolution",
                    ));
                }
                if pending.is_empty() {
                    directories.push(DirectoryWitness {
                        path: path.clone(),
                        file,
                    });
                    return Ok(PhysicalPath {
                        path,
                        directories,
                        missing: Vec::new(),
                        stat: opened,
                    });
                }
                directories.push(DirectoryWitness { path, file });
            }
            _ => {
                if !pending.is_empty() || requested.ends_with('/') {
                    return Err(MezError::invalid_state(
                        "native path ancestor is not a directory",
                    ));
                }
                return Ok(PhysicalPath {
                    path,
                    directories,
                    missing: Vec::new(),
                    stat,
                });
            }
        }
    }
    let parent = directories
        .last()
        .ok_or_else(|| MezError::invalid_state("native resolver lost root handle"))?;
    let stat = rustix::fs::fstat(&parent.file).map_err(std::io::Error::from)?;
    Ok(PhysicalPath {
        path: parent.path.clone(),
        directories,
        missing: Vec::new(),
        stat,
    })
}

/// Compares physical object identity and node kind without pathname lookup.
pub(super) fn same_object(left: &Stat, right: &Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && FileType::from_raw_mode(left.st_mode) == FileType::from_raw_mode(right.st_mode)
}

/// Projects point-in-time physical resolution into existing policy evidence.
pub(crate) fn resolve_host_path(
    current_directory: &Path,
    requested: &str,
) -> Result<ResolvedPathEvidence> {
    let resolved = resolve_physical(current_directory, requested)?;
    let canonical_path = resolved
        .path
        .to_str()
        .ok_or_else(|| MezError::invalid_args("native authority path is not UTF-8"))?
        .to_string();
    let existing = resolved.missing.is_empty();
    let nearest_existing_parent = if existing {
        canonical_path.clone()
    } else {
        resolved
            .directories
            .last()
            .ok_or_else(|| MezError::invalid_state("native resolver lost parent"))?
            .path
            .to_str()
            .ok_or_else(|| MezError::invalid_args("native parent path is not UTF-8"))?
            .to_string()
    };
    let object_kind = match FileType::from_raw_mode(resolved.stat.st_mode) {
        FileType::Directory => ResolvedPathObjectKind::Directory,
        FileType::RegularFile => ResolvedPathObjectKind::File,
        FileType::Socket => ResolvedPathObjectKind::UnixSocket,
        _ => ResolvedPathObjectKind::Other,
    };
    Ok(ResolvedPathEvidence {
        canonical_path,
        nearest_existing_parent,
        kind: if existing {
            ResolvedPathKind::Existing
        } else {
            ResolvedPathKind::CreateTarget
        },
        object_kind,
    })
}

/// Resolves configured grants/effects using the existing PathScopes contract.
pub(crate) fn host_resolved_path_scopes(
    current_directory: &Path,
    read_requests: &[String],
    write_requests: &[String],
    additional_requests: &[String],
) -> Result<PathScopes> {
    let cwd = resolve_host_path(current_directory, ".")?;
    if cwd.kind != ResolvedPathKind::Existing
        || cwd.object_kind != ResolvedPathObjectKind::Directory
    {
        return Err(MezError::invalid_state(
            "native root-process working directory is unavailable",
        ));
    }
    let mut evidence = BTreeMap::new();
    for requested in read_requests
        .iter()
        .chain(write_requests)
        .chain(additional_requests)
    {
        if !evidence.contains_key(requested) {
            evidence.insert(
                requested.clone(),
                resolve_host_path(Path::new(&cwd.canonical_path), requested)?,
            );
        }
    }
    let read_scopes = read_requests
        .iter()
        .map(|requested| {
            let resolved = &evidence[requested];
            if resolved.kind != ResolvedPathKind::Existing {
                return Err(MezError::invalid_state(format!(
                    "sandbox read scope does not exist: {requested}"
                )));
            }
            Ok(resolved.canonical_path.clone())
        })
        .collect::<Result<Vec<_>>>()?;
    let write_scopes = write_requests
        .iter()
        .map(|requested| evidence[requested].canonical_path.clone())
        .collect();
    PathScopes::try_host_resolved_with_evidence(
        cwd.canonical_path,
        read_scopes,
        write_scopes,
        evidence,
    )
    .map_err(|error| MezError::invalid_state(error.message()))
}
