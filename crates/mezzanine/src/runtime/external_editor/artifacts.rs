//! Private filesystem artifacts exchanged with blocking terminal editors.

use std::fs::{self, OpenOptions};
use std::io::Write;
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
}
