//! Agent-owned POSIX shell startup admission.
//!
//! Runtime-created pane-mode agents may use `/bin/sh` on both Linux and
//! macOS. POSIX shells do not expose the editor-specific private receivers
//! used by Bash, Fish, and Zsh, so this adapter installs an owner-only `ENV`
//! file that prefixes the interactive prompt with an authenticated OSC
//! admission record. Generated bootstrap input is released only after that
//! prompt record is observed, never while the shell is still starting.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use mez_agent::MarkerToken;
use mez_mux::process::PaneProcessLaunch;

use super::{MezError, Result};
use crate::error::MezErrorKind;

/// Pane-process-scoped POSIX startup admission state.
#[derive(Debug)]
pub(super) struct ManagedPosixCompatibility {
    token: MarkerToken,
    directory: PathBuf,
    env_file: PathBuf,
}

impl ManagedPosixCompatibility {
    /// Creates an owner-only `ENV` file for one runtime-owned pane process.
    pub(super) fn create(socket_path: &Path, pane_id: &str, token: MarkerToken) -> Result<Self> {
        let parent = socket_path.parent().ok_or_else(|| {
            MezError::invalid_state("control socket has no parent for managed POSIX startup")
        })?;
        let pane_component = pane_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let directory = parent.join(format!(
            ".mez-posix-{pane_component}-{}",
            &token.as_str()[..16]
        ));
        fs::create_dir(&directory).map_err(|error| {
            MezError::new(
                MezErrorKind::Io,
                format!(
                    "failed to create managed POSIX startup directory `{}`: {error}",
                    directory.display()
                ),
            )
        })?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|error| {
            let _ = fs::remove_dir_all(&directory);
            MezError::new(
                MezErrorKind::Io,
                format!(
                    "failed to restrict managed POSIX startup directory `{}`: {error}",
                    directory.display()
                ),
            )
        })?;
        let env_file = directory.join("env");
        if let Err(error) = write_private_file(&env_file, &managed_posix_env_source(&token)) {
            let _ = fs::remove_dir_all(&directory);
            return Err(error);
        }
        Ok(Self {
            token,
            directory,
            env_file,
        })
    }

    /// Selects the private startup file for the interactive POSIX shell.
    pub(super) fn configure_launch(&self, launch: PaneProcessLaunch) -> PaneProcessLaunch {
        launch.with_environment_variable("ENV", self.env_file.as_os_str())
    }

    /// Returns the token authenticating the prompt-bound admission record.
    pub(super) fn token(&self) -> &MarkerToken {
        &self.token
    }
}

impl Drop for ManagedPosixCompatibility {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// Renders startup source whose admission record is emitted as the first prompt.
fn managed_posix_env_source(token: &MarkerToken) -> String {
    format!(
        "if [ \"${{PS1+x}}\" != x ]; then PS1='$ '; fi\n\
MEZ_POSIX_PROMPT_RECORD=$(printf '\\033]133;R;mez_protocol=2;mez_shell=posix;mez_token=%s;mez_event=adapter-available\\033\\\\' '{}')\n\
PS1=\"${{MEZ_POSIX_PROMPT_RECORD}}${{PS1}}\"\n\
unset MEZ_POSIX_PROMPT_RECORD\n",
        token.as_str()
    )
}

/// Writes one owner-only startup artifact without following an existing file.
fn write_private_file(path: &Path, contents: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| {
            MezError::new(
                MezErrorKind::Io,
                format!(
                    "failed to create managed POSIX startup file `{}`: {error}",
                    path.display()
                ),
            )
        })?;
    file.write_all(contents.as_bytes()).map_err(|error| {
        MezError::new(
            MezErrorKind::Io,
            format!(
                "failed to write managed POSIX startup file `{}`: {error}",
                path.display()
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies POSIX admission is prompt-bound and carries no generated bootstrap source.
    #[test]
    fn managed_posix_startup_emits_only_authenticated_prompt_admission() {
        let token = MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap();
        let source = managed_posix_env_source(&token);

        assert!(source.contains("PS1=\"${MEZ_POSIX_PROMPT_RECORD}${PS1}\""));
        assert!(source.contains("mez_shell=posix"));
        assert!(source.contains(token.as_str()));
        assert!(!source.contains("bootstrap"));
    }
}
