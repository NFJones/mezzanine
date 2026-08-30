//! Private filesystem artifacts exchanged with blocking terminal editors.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{MezError, Result};

/// Private files owned by one external-editor session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExternalEditorArtifacts {
    /// Owner-only directory unique to one opaque session id.
    pub(super) session_directory: PathBuf,
    /// Owner-only draft passed to the configured editor.
    pub(super) draft_path: PathBuf,
}

/// Validated final draft reopened after the editor process exits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ValidatedExternalEditorDraft {
    /// UTF-8 draft text after bounded, race-resistant reopening.
    pub(super) content: String,
}

/// Creates one unique `0700` session directory and exclusive `0600` draft.
pub(super) fn create_external_editor_artifacts(
    runtime_root: &Path,
    session_id: &str,
    content: &str,
) -> Result<ExternalEditorArtifacts> {
    let root = runtime_root.join("editor-sessions");
    create_private_directory_all(&root)?;
    let session_directory = root.join(session_id);
    create_private_directory(&session_directory)?;
    let draft_path = session_directory.join("draft.md");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut draft = options.open(&draft_path).map_err(|error| {
        MezError::invalid_state(format!("failed to create private editor draft: {error}"))
    })?;
    draft.write_all(content.as_bytes()).map_err(|error| {
        MezError::invalid_state(format!("failed to write private editor draft: {error}"))
    })?;
    draft.sync_all().map_err(|error| {
        MezError::invalid_state(format!("failed to sync private editor draft: {error}"))
    })?;
    set_private_file_permissions(&draft_path)?;
    Ok(ExternalEditorArtifacts {
        session_directory,
        draft_path,
    })
}

/// Reopens and validates the final editor draft, including atomic replacements.
///
/// The session directory is opened first and the fixed `draft.md` child is
/// then opened relative to that descriptor with `O_NOFOLLOW`. This permits a
/// normal same-directory atomic save while preventing a symlink or path-parent
/// swap from redirecting validation outside the private session directory.
pub(super) fn validate_external_editor_draft(
    artifacts: &ExternalEditorArtifacts,
    max_bytes: u64,
    max_lines: usize,
) -> Result<ValidatedExternalEditorDraft> {
    if artifacts.draft_path.parent() != Some(artifacts.session_directory.as_path())
        || artifacts
            .draft_path
            .file_name()
            .and_then(|name| name.to_str())
            != Some("draft.md")
    {
        return Err(MezError::forbidden(
            "external editor draft path escaped its session directory",
        ));
    }
    validate_private_session_directory(&artifacts.session_directory)?;

    let directory = fs::File::open(&artifacts.session_directory)?;
    let descriptor = rustix::fs::openat(
        &directory,
        "draft.md",
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let mut draft = fs::File::from(descriptor);
    let metadata = draft.metadata()?;
    validate_private_draft_metadata(&metadata, max_bytes)?;

    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut draft)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(MezError::invalid_args(
            "external editor draft exceeds the byte limit",
        ));
    }
    let content = String::from_utf8(bytes)
        .map_err(|_| MezError::invalid_args("external editor draft must be valid UTF-8"))?;
    if content.lines().count() > max_lines {
        return Err(MezError::invalid_args(
            "external editor draft exceeds the line limit",
        ));
    }
    Ok(ValidatedExternalEditorDraft { content })
}

fn validate_private_session_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(MezError::forbidden(
                "external editor session directory must be private and owned by the current user",
            ));
        }
    }
    #[cfg(not(unix))]
    if !metadata.is_dir() {
        return Err(MezError::forbidden(
            "external editor session path must be a directory",
        ));
    }
    Ok(())
}

fn validate_private_draft_metadata(metadata: &fs::Metadata, max_bytes: u64) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(MezError::forbidden(
                "external editor draft must be a private single-link regular file owned by the current user",
            ));
        }
    }
    #[cfg(not(unix))]
    if !metadata.is_file() {
        return Err(MezError::forbidden(
            "external editor draft must be a regular file",
        ));
    }
    if metadata.len() > max_bytes {
        return Err(MezError::invalid_args(
            "external editor draft exceeds the byte limit",
        ));
    }
    Ok(())
}

fn create_private_directory_all(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|error| {
        MezError::invalid_state(format!("failed to create editor artifact root: {error}"))
    })?;
    set_private_directory_permissions(path)
}

fn create_private_directory(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|error| {
        MezError::invalid_state(format!("failed to create private editor session: {error}"))
    })?;
    set_private_directory_permissions(path)
}

fn set_private_directory_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn set_private_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies drafts are exclusive owner-only files beneath a unique
    /// owner-only session directory and preserve their exact initial text.
    #[test]
    fn creates_private_editor_artifacts() {
        let root =
            std::env::temp_dir().join(format!("mez-editor-artifact-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let artifacts =
            create_external_editor_artifacts(&root, "opaque-session", "draft text\n").unwrap();

        assert_eq!(
            fs::read_to_string(&artifacts.draft_path).unwrap(),
            "draft text\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&artifacts.session_directory)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&artifacts.draft_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    /// Verifies safe same-directory atomic replacement is accepted while
    /// symlinks, hard links, weak permissions, invalid UTF-8, and oversized
    /// output are rejected before target-specific application.
    #[test]
    fn validates_atomic_replacement_and_rejects_unsafe_drafts() {
        let root =
            std::env::temp_dir().join(format!("mez-editor-validation-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let artifacts = create_external_editor_artifacts(&root, "atomic", "before").unwrap();
        let replacement = artifacts.session_directory.join("replacement");
        fs::write(&replacement, b"after\n").unwrap();
        set_private_file_permissions(&replacement).unwrap();
        fs::rename(&replacement, &artifacts.draft_path).unwrap();
        assert_eq!(
            validate_external_editor_draft(&artifacts, 64, 4)
                .unwrap()
                .content,
            "after\n"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};

            let outside = root.join("outside");
            fs::write(&outside, b"outside").unwrap();
            set_private_file_permissions(&outside).unwrap();
            fs::remove_file(&artifacts.draft_path).unwrap();
            symlink(&outside, &artifacts.draft_path).unwrap();
            assert!(validate_external_editor_draft(&artifacts, 64, 4).is_err());

            fs::remove_file(&artifacts.draft_path).unwrap();
            fs::hard_link(&outside, &artifacts.draft_path).unwrap();
            assert!(validate_external_editor_draft(&artifacts, 64, 4).is_err());

            fs::remove_file(&artifacts.draft_path).unwrap();
            fs::write(&artifacts.draft_path, b"weak").unwrap();
            fs::set_permissions(&artifacts.draft_path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(validate_external_editor_draft(&artifacts, 64, 4).is_err());
        }

        fs::write(&artifacts.draft_path, [0xff, 0xfe]).unwrap();
        set_private_file_permissions(&artifacts.draft_path).unwrap();
        assert!(validate_external_editor_draft(&artifacts, 64, 4).is_err());

        fs::write(&artifacts.draft_path, b"0123456789").unwrap();
        set_private_file_permissions(&artifacts.draft_path).unwrap();
        assert!(validate_external_editor_draft(&artifacts, 4, 4).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
