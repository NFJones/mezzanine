//! Bounded descriptor-relative skill reads without symlink or special-node traversal.
//!
//! A configured catalog root is canonicalized once, then opened component by
//! component with no-follow directory handles. Descendant reads remain relative
//! to those handles, so path swaps cannot redirect the opened document. This is
//! same-user filesystem evidence, not execution authority or script execution.

use crate::{MezError, Result};
use rustix::fs::{Mode, OFlags, openat};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path};

/// Maximum complete skill document admitted to discovery or loading.
pub(super) const MAX_SKILL_BYTES: u64 = 1024 * 1024;

/// Reads one named regular UTF-8 document under an explicitly selected catalog root.
pub(super) fn read(root: &Path, name: &str) -> Result<String> {
    read_impl(root, name, false)
}

/// Reads new shared placement while anchoring `.agents` below the project base.
/// Legacy user and native readers keep their established base-alias semantics.
pub(super) fn read_shared(root: &Path, name: &str) -> Result<String> {
    read_impl(root, name, true)
}

/// Opens a catalog without following protected namespace or descendant links.
fn open_root(root: &Path, shared: bool) -> Result<File> {
    let parent = root
        .parent()
        .ok_or_else(|| MezError::forbidden("skill root unavailable"))?;
    if shared
        && (root.file_name().is_none_or(|name| name != "skills")
            || parent.file_name().is_none_or(|name| name != ".agents"))
    {
        return Err(MezError::forbidden("shared skill root unavailable"));
    }
    let base = if shared || parent.file_name().is_some_and(|name| name == ".mezzanine") {
        parent
            .parent()
            .ok_or_else(|| MezError::forbidden("skill root unavailable"))?
    } else {
        parent
    };
    let suffix = root
        .strip_prefix(base)
        .map_err(|_| MezError::forbidden("skill root unavailable"))?;
    let canonical = std::fs::canonicalize(base)?.join(suffix);
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
            _ => return Err(MezError::forbidden("skill root unavailable")),
        }
    }
    Ok(directory)
}

/// Lists a finite shared namespace through its held directory descriptor.
/// Symlink swaps cannot redirect enumeration outside the selected project.
pub(super) fn shared_entry_names(root: &Path) -> Result<Vec<std::ffi::OsString>> {
    use std::os::unix::ffi::OsStrExt;
    let directory = open_root(root, true)?;
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
                "shared skill catalog exceeds entry limit",
            ));
        }
        names.push(std::ffi::OsStr::from_bytes(name).to_os_string());
    }
    names.sort();
    Ok(names)
}

/// Reads one bounded regular document through the selected namespace policy.
fn read_impl(root: &Path, name: &str, shared: bool) -> Result<String> {
    if !mez_agent::is_valid_skill_name(name) {
        return Err(MezError::invalid_args("skill unavailable"));
    }
    // Permit aliases of the explicitly configured user/project base, but never
    // follow .mezzanine or skills links below that base into an unrelated tree.
    let directory = open_root(root, shared)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let skill =
        File::from(openat(&directory, name, flags, Mode::empty()).map_err(std::io::Error::from)?);
    let mut file = File::from(
        openat(
            &skill,
            "SKILL.md",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_SKILL_BYTES {
        return Err(MezError::invalid_args(
            "skill document unavailable or oversized",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_SKILL_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SKILL_BYTES {
        return Err(MezError::invalid_args("skill document oversized"));
    }
    String::from_utf8(bytes).map_err(|_| MezError::invalid_args("skill document is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// Descriptor-relative reads reject directory/file symlink escapes and
    /// oversized documents while retaining ordinary bounded UTF-8 reads.
    #[test]
    fn model_skill_reads_reject_symlinks_and_oversized_documents() {
        let base = std::env::temp_dir().join(format!(
            "mez-skill-read-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        let root = base.join("skills");
        std::fs::create_dir_all(root.join("review")).unwrap();
        std::fs::write(root.join("review/SKILL.md"), "safe").unwrap();
        assert_eq!(read(&root, "review").unwrap(), "safe");
        std::fs::write(base.join("outside"), "secret").unwrap();
        std::fs::remove_file(root.join("review/SKILL.md")).unwrap();
        symlink(base.join("outside"), root.join("review/SKILL.md")).unwrap();
        assert!(read(&root, "review").is_err());
        std::fs::remove_file(root.join("review/SKILL.md")).unwrap();
        std::fs::write(
            root.join("review/SKILL.md"),
            vec![b'x'; MAX_SKILL_BYTES as usize + 1],
        )
        .unwrap();
        assert!(read(&root, "review").is_err());
        symlink(root.join("review"), root.join("linked")).unwrap();
        assert!(read(&root, "linked").is_err());
        std::fs::rename(&root, base.join("moved")).unwrap();
        symlink(base.join("moved"), &root).unwrap();
        assert!(read(&root, "review").is_err());
        std::fs::remove_dir_all(base).unwrap();
    }
}
