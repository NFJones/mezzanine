//! External-editor executable resolution and draft-path argv substitution.

use std::path::{Path, PathBuf};

use crate::error::{MezError, Result};
use crate::runtime::RuntimeExternalEditorConfig;

/// Fully resolved editor argv ready for a typed child launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedExternalEditorCommand {
    /// Absolute executable selected from the preferred and fallback candidates.
    pub(super) executable: String,
    /// Ordered arguments after draft-path substitution.
    pub(super) arguments: Vec<String>,
}

/// Resolves the first available configured candidate against the pane PATH.
pub(super) fn resolve_external_editor_command(
    config: &RuntimeExternalEditorConfig,
    pane_path: Option<&str>,
    draft_path: &Path,
) -> Result<ResolvedExternalEditorCommand> {
    resolve_external_editor_commands(config, pane_path, draft_path).map(|commands| {
        commands
            .into_iter()
            .next()
            .expect("resolver rejects an empty command set")
    })
}

/// Resolves every available configured candidate in fallback order.
pub(super) fn resolve_external_editor_commands(
    config: &RuntimeExternalEditorConfig,
    pane_path: Option<&str>,
    draft_path: &Path,
) -> Result<Vec<ResolvedExternalEditorCommand>> {
    let commands = std::iter::once(&config.command)
        .chain(config.fallback.iter())
        .filter_map(|candidate| resolve_candidate(candidate, pane_path, draft_path))
        .collect::<Vec<_>>();
    if commands.is_empty() {
        Err(MezError::new(
            crate::error::MezErrorKind::NotFound,
            "no configured external editor executable was found in the pane PATH",
        ))
    } else {
        Ok(commands)
    }
}

fn resolve_candidate(
    candidate: &[String],
    pane_path: Option<&str>,
    draft_path: &Path,
) -> Option<ResolvedExternalEditorCommand> {
    let configured_executable = candidate.first()?;
    let executable = resolve_executable(configured_executable, pane_path)?;
    let draft = draft_path.to_string_lossy();
    let mut substituted = false;
    let mut arguments = candidate
        .iter()
        .skip(1)
        .map(|argument| {
            if argument.contains("{file}") {
                substituted = true;
                argument.replace("{file}", draft.as_ref())
            } else {
                argument.clone()
            }
        })
        .collect::<Vec<_>>();
    if !substituted {
        arguments.push(draft.into_owned());
    }
    Some(ResolvedExternalEditorCommand {
        executable: executable.to_string_lossy().into_owned(),
        arguments,
    })
}

fn resolve_executable(executable: &str, pane_path: Option<&str>) -> Option<PathBuf> {
    let path = Path::new(executable);
    if path.is_absolute() {
        return executable_is_runnable(path).then(|| path.to_path_buf());
    }
    pane_path?
        .split(':')
        .filter(|entry| !entry.is_empty())
        .map(Path::new)
        .map(|directory| directory.join(executable))
        .find(|candidate| executable_is_runnable(candidate))
}

fn executable_is_runnable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Verifies missing preferred candidates fall back in order while argv
    /// boundaries and exactly one draft substitution remain intact.
    #[test]
    fn resolves_editor_fallback_and_substitutes_draft() {
        let root =
            std::env::temp_dir().join(format!("mez-editor-command-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let editor = root.join("working-editor");
        fs::write(&editor, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&editor, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let config = RuntimeExternalEditorConfig {
            command: vec!["missing-editor".to_string()],
            fallback: vec![vec![
                "working-editor".to_string(),
                "--label".to_string(),
                "space value".to_string(),
                "{file}".to_string(),
            ]],
        };
        let draft = root.join("draft.md");
        let resolved = resolve_external_editor_command(&config, root.to_str(), &draft).unwrap();

        assert_eq!(resolved.executable, editor.to_string_lossy());
        assert_eq!(
            resolved.arguments,
            ["--label", "space value", draft.to_string_lossy().as_ref()]
        );
        let _ = fs::remove_dir_all(root);
    }
}
