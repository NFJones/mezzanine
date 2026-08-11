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
        let source = managed_bash_receiver_source(&token);
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

/// Renders the pane-private Bash receiver installed after the user's rcfile.
///
/// Admission metadata never contains generated source. The callback confirms
/// an empty Readline edit buffer, emits an authenticated ready event, then
/// validates bounded sequence, length, digest, and end framing before eval.
fn managed_bash_receiver_source(token: &MarkerToken) -> String {
    const TEMPLATE: &str = r#"if [[ -r ${MEZ_BASH_USER_RCFILE:-$HOME/.bashrc} ]]; then builtin source -- "${MEZ_BASH_USER_RCFILE:-$HOME/.bashrc}"; fi
MEZ_BASH_RECEIVER_TOKEN=__MEZ_BASH_RECEIVER_TOKEN__
__mez_bash_receiver_reset() {
    unset MEZ_BASH_RECEIVER_FRAME MEZ_BASH_RECEIVER_KIND MEZ_BASH_RECEIVER_FRAME_TOKEN MEZ_BASH_RECEIVER_FRAME_MARKER MEZ_BASH_RECEIVER_LENGTH MEZ_BASH_RECEIVER_DIGEST MEZ_BASH_RECEIVER_CHUNKS MEZ_BASH_RECEIVER_SEQUENCE MEZ_BASH_RECEIVER_B64 MEZ_BASH_RECEIVER_SOURCE MEZ_BASH_RECEIVER_ACTUAL_LENGTH MEZ_BASH_RECEIVER_ACTUAL_DIGEST MEZ_BASH_RECEIVER_STATUS
}
__mez_bash_receiver() {
    IFS=' ' builtin read -r MEZ_BASH_RECEIVER_KIND MEZ_BASH_RECEIVER_FRAME_TOKEN MEZ_BASH_RECEIVER_FRAME_MARKER MEZ_BASH_RECEIVER_LENGTH MEZ_BASH_RECEIVER_DIGEST MEZ_BASH_RECEIVER_CHUNKS || return 1
    MEZ_BASH_RECEIVER_KIND=${MEZ_BASH_RECEIVER_KIND#$'\a'}
    [[ $MEZ_BASH_RECEIVER_KIND == MEZ_BASH_RX1_BEGIN && $MEZ_BASH_RECEIVER_FRAME_TOKEN == "$MEZ_BASH_RECEIVER_TOKEN" ]] || { __mez_bash_receiver_reset; return 1; }
    [[ $MEZ_BASH_RECEIVER_FRAME_MARKER && $MEZ_BASH_RECEIVER_LENGTH =~ ^[0-9]+$ && $MEZ_BASH_RECEIVER_LENGTH -le 16777216 && $MEZ_BASH_RECEIVER_DIGEST =~ ^[0-9a-f]{64}$ && $MEZ_BASH_RECEIVER_CHUNKS =~ ^[0-9]+$ && $MEZ_BASH_RECEIVER_CHUNKS -le 34953 ]] || { __mez_bash_receiver_reset; return 1; }
    [[ -z ${READLINE_LINE-} ]] || { __mez_bash_receiver_reset; return 1; }
    command printf '\033]133;R;mez_receiver=ready;mez_token=%s;mez_marker=%s\033\\' "$MEZ_BASH_RECEIVER_TOKEN" "$MEZ_BASH_RECEIVER_FRAME_MARKER"
    MEZ_BASH_RECEIVER_SEQUENCE=0
    MEZ_BASH_RECEIVER_B64=
    while (( MEZ_BASH_RECEIVER_SEQUENCE < MEZ_BASH_RECEIVER_CHUNKS )); do
        IFS=' ' builtin read -r MEZ_BASH_RECEIVER_KIND MEZ_BASH_RECEIVER_FRAME_TOKEN MEZ_BASH_RECEIVER_FRAME_MARKER_READ MEZ_BASH_RECEIVER_SEQUENCE_READ MEZ_BASH_RECEIVER_FRAME || { __mez_bash_receiver_reset; return 1; }
        [[ $MEZ_BASH_RECEIVER_KIND == MEZ_BASH_RX1_DATA && $MEZ_BASH_RECEIVER_FRAME_TOKEN == "$MEZ_BASH_RECEIVER_TOKEN" && $MEZ_BASH_RECEIVER_FRAME_MARKER_READ == "$MEZ_BASH_RECEIVER_FRAME_MARKER" && $MEZ_BASH_RECEIVER_SEQUENCE_READ == "$MEZ_BASH_RECEIVER_SEQUENCE" && $MEZ_BASH_RECEIVER_FRAME =~ ^[A-Za-z0-9+/]*={0,2}$ ]] || { __mez_bash_receiver_reset; return 1; }
        MEZ_BASH_RECEIVER_B64+=$MEZ_BASH_RECEIVER_FRAME
        (( MEZ_BASH_RECEIVER_SEQUENCE += 1 ))
        command printf '\036'
    done
    IFS=' ' builtin read -r MEZ_BASH_RECEIVER_KIND MEZ_BASH_RECEIVER_FRAME_TOKEN MEZ_BASH_RECEIVER_FRAME_MARKER_READ MEZ_BASH_RECEIVER_SEQUENCE_READ MEZ_BASH_RECEIVER_LENGTH_READ MEZ_BASH_RECEIVER_DIGEST_READ || { __mez_bash_receiver_reset; return 1; }
    [[ $MEZ_BASH_RECEIVER_KIND == MEZ_BASH_RX1_END && $MEZ_BASH_RECEIVER_FRAME_TOKEN == "$MEZ_BASH_RECEIVER_TOKEN" && $MEZ_BASH_RECEIVER_FRAME_MARKER_READ == "$MEZ_BASH_RECEIVER_FRAME_MARKER" && $MEZ_BASH_RECEIVER_SEQUENCE_READ == "$MEZ_BASH_RECEIVER_CHUNKS" && $MEZ_BASH_RECEIVER_LENGTH_READ == "$MEZ_BASH_RECEIVER_LENGTH" && $MEZ_BASH_RECEIVER_DIGEST_READ == "$MEZ_BASH_RECEIVER_DIGEST" ]] || { __mez_bash_receiver_reset; return 1; }
    command printf '\036'
    MEZ_BASH_RECEIVER_SOURCE=$({ command printf '%s' "$MEZ_BASH_RECEIVER_B64" | base64 -d 2>/dev/null || command printf '%s' "$MEZ_BASH_RECEIVER_B64" | base64 -D 2>/dev/null; } && command printf x) || { __mez_bash_receiver_reset; return 1; }
    MEZ_BASH_RECEIVER_SOURCE=${MEZ_BASH_RECEIVER_SOURCE%x}
    MEZ_BASH_RECEIVER_ACTUAL_LENGTH=$(command printf '%s' "$MEZ_BASH_RECEIVER_SOURCE" | LC_ALL=C command wc -c | command tr -d '[:space:]') || { __mez_bash_receiver_reset; return 1; }
    if command -v sha256sum >/dev/null 2>&1; then
        MEZ_BASH_RECEIVER_ACTUAL_DIGEST=$(command printf '%s' "$MEZ_BASH_RECEIVER_SOURCE" | sha256sum)
    elif command -v shasum >/dev/null 2>&1; then
        MEZ_BASH_RECEIVER_ACTUAL_DIGEST=$(command printf '%s' "$MEZ_BASH_RECEIVER_SOURCE" | shasum -a 256)
    else
        __mez_bash_receiver_reset
        return 1
    fi
    MEZ_BASH_RECEIVER_ACTUAL_DIGEST=${MEZ_BASH_RECEIVER_ACTUAL_DIGEST%%[[:space:]]*}
    [[ $MEZ_BASH_RECEIVER_ACTUAL_LENGTH == "$MEZ_BASH_RECEIVER_LENGTH" && $MEZ_BASH_RECEIVER_ACTUAL_DIGEST == "$MEZ_BASH_RECEIVER_DIGEST" ]] || { __mez_bash_receiver_reset; return 1; }
    eval "$MEZ_BASH_RECEIVER_SOURCE"
    MEZ_BASH_RECEIVER_STATUS=$?
    MEZ_BASH_RECEIVER_COMPLETE_MARKER=$MEZ_BASH_RECEIVER_FRAME_MARKER
    MEZ_BASH_RECEIVER_COMPLETE_STATUS=$MEZ_BASH_RECEIVER_STATUS
    __mez_bash_receiver_reset
    command printf '\033]133;R;mez_receiver=complete;mez_token=%s;mez_marker=%s;mez_status=%s\033\\' "$MEZ_BASH_RECEIVER_TOKEN" "$MEZ_BASH_RECEIVER_COMPLETE_MARKER" "$MEZ_BASH_RECEIVER_COMPLETE_STATUS"
    unset MEZ_BASH_RECEIVER_COMPLETE_MARKER MEZ_BASH_RECEIVER_COMPLETE_STATUS
}
bind -m emacs-standard -x '"\C-g":__mez_bash_receiver'
bind -m vi-insert -x '"\C-g":__mez_bash_receiver'
bind -m vi-command -x '"\C-g":__mez_bash_receiver'
if [[ -n ${MEZ_BASH_RECEIVER_INSTALL_MARKER-} ]]; then
    MEZ_BASH_RECEIVER_INSTALLED_MARKER=$MEZ_BASH_RECEIVER_INSTALL_MARKER
    unset MEZ_BASH_RECEIVER_INSTALL_MARKER
    command printf '\033]133;R;mez_receiver=installed;mez_token=%s;mez_marker=%s\033\\' "$MEZ_BASH_RECEIVER_TOKEN" "$MEZ_BASH_RECEIVER_INSTALLED_MARKER"
    unset MEZ_BASH_RECEIVER_INSTALLED_MARKER
    __mez_bash_receiver
fi
"#;
    TEMPLATE.replace("__MEZ_BASH_RECEIVER_TOKEN__", token.as_str())
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
    use sha2::{Digest as _, Sha256};
    use std::process::{Command, Stdio};

    /// Verifies the generated receiver binds its trigger in Readline's real vi
    /// insertion map so ordinary Bash startup does not emit a keymap diagnostic.
    #[test]
    fn managed_bash_receiver_uses_valid_readline_keymaps() {
        let token = MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap();
        let source = managed_bash_receiver_source(&token);

        assert!(source.contains("bind -m emacs-standard"), "{source}");
        assert!(source.contains("bind -m vi-insert"), "{source}");
        assert!(source.contains("bind -m vi-command"), "{source}");
        assert!(!source.contains("vi-insertion"), "{source}");
    }

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
        let source = "printf '__MEZ_BASH_RECEIVER_EXECUTED_π__\\n'";
        let marker = "receiver-test-marker";
        let encoded = base64::engine::general_purpose::STANDARD.encode(source);
        let digest = Sha256::digest(source.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let trigger = format!(
            "\x07MEZ_BASH_RX1_BEGIN {} {} {} {} 1\n",
            token.as_str(),
            marker,
            source.len(),
            digest
        );
        let frame = format!(
            "MEZ_BASH_RX1_DATA {} {} 0 {}\nMEZ_BASH_RX1_END {} {} 1 {} {}\n",
            token.as_str(),
            marker,
            encoded,
            token.as_str(),
            marker,
            source.len(),
            digest
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
        stdin.write_all(trigger.as_bytes()).unwrap();
        stdin.write_all(frame.as_bytes()).unwrap();
        stdin.write_all(b"printf '__MEZ_HISTORY_BEGIN__\\n'; history; printf '__MEZ_HISTORY_END__\\n'; history -w; exit\n").unwrap();
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "stdout={stdout:?} stderr={stderr:?}",
        );
        assert!(!stderr.contains("invalid keymap name"), "{stderr:?}");
        assert!(
            stdout.contains("__MEZ_BASH_RECEIVER_EXECUTED_π__"),
            "{stdout:?}"
        );
        let persisted = fs::read_to_string(&history).unwrap_or_default();
        for observed in [stdout.as_ref(), persisted.as_str()] {
            assert!(!observed.contains("MEZ_BASH_RX"), "{observed:?}");
            assert!(
                !observed.contains("__MEZ_BASH_RECEIVER_EXECUTED_π__") || observed == stdout,
                "{observed:?}"
            );
        }
        drop(compatibility);
        fs::remove_dir_all(root).unwrap();
    }
}
