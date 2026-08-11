//! Pane-scoped private Readline delivery for interactive Bash panes.
//!
//! Generated source reaches Bash through a `bind -x` callback rather than a
//! newline-terminated Readline command. The callback admits only an empty edit
//! buffer, consumes one authenticated Base64 frame directly from terminal
//! input, and evaluates it outside the user's command history.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use mez_agent::MarkerToken;
use mez_mux::process::PaneProcessLaunch;

use super::{MezError, Result};
use crate::error::MezErrorKind;

/// Pane-process-scoped Bash private receiver configuration.
#[derive(Debug)]
pub(super) struct ManagedBashCompatibility {
    token: MarkerToken,
    directory: PathBuf,
    rcfile: PathBuf,
}

impl ManagedBashCompatibility {
    /// Creates a private rcfile which sources the user's rcfile once before
    /// installing the receiver binding.
    pub(super) fn create(socket_path: &Path, pane_id: &str, token: MarkerToken) -> Result<Self> {
        let parent = socket_path.parent().ok_or_else(|| {
            MezError::invalid_state("control socket has no parent for managed Bash startup")
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
            ".mez-bash-{pane_component}-{}",
            &token.as_str()[..16]
        ));
        fs::create_dir(&directory).map_err(|error| {
            MezError::new(
                MezErrorKind::Io,
                format!(
                    "failed to create managed Bash startup directory `{}`: {error}",
                    directory.display()
                ),
            )
        })?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|error| {
            let _ = fs::remove_dir_all(&directory);
            MezError::new(
                MezErrorKind::Io,
                format!(
                    "failed to restrict managed Bash startup directory `{}`: {error}",
                    directory.display()
                ),
            )
        })?;
        let rcfile = directory.join("bashrc");
        let source = format!(
            "if [[ -r ${{MEZ_BASH_USER_RCFILE:-$HOME/.bashrc}} ]]; then builtin source -- \"${{MEZ_BASH_USER_RCFILE:-$HOME/.bashrc}}\"; fi\nMEZ_BASH_RECEIVER_TOKEN={}\n__mez_bash_receiver() {{ [[ -z ${{READLINE_LINE-}} ]] || return 1; IFS= builtin read -r MEZ_BASH_RECEIVER_FRAME || return 1; case $MEZ_BASH_RECEIVER_FRAME in \"MEZ_BASH_RX $MEZ_BASH_RECEIVER_TOKEN \"*) ;; *) return 1;; esac; MEZ_BASH_RECEIVER_B64=${{MEZ_BASH_RECEIVER_FRAME#MEZ_BASH_RX $MEZ_BASH_RECEIVER_TOKEN }}; MEZ_BASH_RECEIVER_SOURCE=$(printf '%s' \"$MEZ_BASH_RECEIVER_B64\" | base64 -d 2>/dev/null || printf '%s' \"$MEZ_BASH_RECEIVER_B64\" | base64 -D 2>/dev/null) || return 1; eval \"$MEZ_BASH_RECEIVER_SOURCE\"; unset MEZ_BASH_RECEIVER_SOURCE MEZ_BASH_RECEIVER_B64 MEZ_BASH_RECEIVER_FRAME; }}\nbind -x '\"\\C-g\":__mez_bash_receiver'\n",
            token.as_str()
        );
        if let Err(error) = write_private_file(&rcfile, &source) {
            let _ = fs::remove_dir_all(&directory);
            return Err(error);
        }
        Ok(Self {
            token,
            directory,
            rcfile,
        })
    }

    /// Starts interactive Bash with the managed rcfile after normal user rc
    /// loading has been delegated to that private file.
    pub(super) fn configure_launch(&self, launch: PaneProcessLaunch) -> PaneProcessLaunch {
        launch
            .with_interactive_arguments([
                "--noprofile",
                "--rcfile",
                self.rcfile.to_string_lossy().as_ref(),
                "-i",
            ])
            .with_environment_variable("BASH_ENV", "")
            .with_environment_variable("MEZ_BASH_RECEIVER_RCFILE", self.rcfile.as_os_str())
    }

    /// Returns the token required by the private receiver frame.
    pub(super) fn token(&self) -> &MarkerToken {
        &self.token
    }

    /// Returns the private rcfile retained for an agent subshell handoff.
    pub(super) fn rcfile(&self) -> &Path {
        &self.rcfile
    }
}

impl Drop for ManagedBashCompatibility {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
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
                    "failed to create managed Bash startup file `{}`: {error}",
                    path.display()
                ),
            )
        })?;
    file.write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            MezError::new(
                MezErrorKind::Io,
                format!(
                    "failed to write managed Bash startup file `{}`: {error}",
                    path.display()
                ),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use mez_mux::process::pane_command_plan;
    use std::process::{Command, Stdio};

    /// Verifies the managed Bash receiver evaluates an authenticated frame
    /// without placing its trigger, metadata, or source in Bash history.
    #[test]
    fn managed_bash_receiver_keeps_generated_source_out_of_history() {
        let bash = Path::new("/bin/bash");
        if !bash.exists() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "mez-bash-receiver-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let history = home.join("history");
        fs::write(
            home.join(".bashrc"),
            "shopt -s histappend\ntrap 'history -a' DEBUG\nPS1=\n",
        )
        .unwrap();
        let token = MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap();
        let compatibility =
            ManagedBashCompatibility::create(&root.join("control.sock"), "%1", token.clone())
                .unwrap();
        let launch = compatibility.configure_launch(PaneProcessLaunch::new(bash.to_path_buf()));
        let plan = pane_command_plan(&launch, None).unwrap();
        let source = "printf '__MEZ_BASH_RECEIVER_EXECUTED__\\n'";
        let frame = format!(
            "\x07MEZ_BASH_RX {} {}\n",
            token.as_str(),
            base64::engine::general_purpose::STANDARD.encode(source)
        );
        let mut command = Command::new(plan.program);
        command
            .args(plan.args)
            .env("HOME", &home)
            .env("HISTFILE", &history)
            .env("HISTSIZE", "1000")
            .env("HISTFILESIZE", "1000")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in launch.environment() {
            command.env(key, value);
        }
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.as_mut().unwrap();
        stdin
            .write_all(b"history -c\nprintf '__MEZ_USER_COMMAND__\\n'\n")
            .unwrap();
        stdin.write_all(frame.as_bytes()).unwrap();
        stdin.write_all(b"printf '__MEZ_HISTORY_BEGIN__\\n'; history; printf '__MEZ_HISTORY_END__\\n'; history -w; exit\n").unwrap();
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "stdout={stdout:?} stderr={:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("__MEZ_BASH_RECEIVER_EXECUTED__"),
            "{stdout:?}"
        );
        let persisted = fs::read_to_string(&history).unwrap_or_default();
        for observed in [stdout.as_ref(), persisted.as_str()] {
            assert!(!observed.contains("MEZ_BASH_RX"), "{observed:?}");
            assert!(
                !observed.contains("__MEZ_BASH_RECEIVER_EXECUTED__") || observed == stdout,
                "{observed:?}"
            );
        }
        drop(compatibility);
        fs::remove_dir_all(root).unwrap();
    }
}
