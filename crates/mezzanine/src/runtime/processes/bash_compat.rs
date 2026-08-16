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
    if [[ ${MEZ_BASH_RECEIVER_SAVED_LINE_SET-} == 1 ]]; then
        READLINE_LINE=$MEZ_BASH_RECEIVER_SAVED_LINE
        READLINE_POINT=$MEZ_BASH_RECEIVER_SAVED_POINT
        if [[ ${MEZ_BASH_RECEIVER_SAVED_MARK_SET-} == 1 ]]; then
            READLINE_MARK=$MEZ_BASH_RECEIVER_SAVED_MARK
        else
            unset READLINE_MARK
        fi
    fi
    unset MEZ_BASH_RECEIVER_FRAME MEZ_BASH_RECEIVER_KIND MEZ_BASH_RECEIVER_FRAME_TOKEN MEZ_BASH_RECEIVER_FRAME_MARKER MEZ_BASH_RECEIVER_FRAME_MARKER_READ MEZ_BASH_RECEIVER_LENGTH MEZ_BASH_RECEIVER_LENGTH_READ MEZ_BASH_RECEIVER_DIGEST MEZ_BASH_RECEIVER_DIGEST_READ MEZ_BASH_RECEIVER_CHUNKS MEZ_BASH_RECEIVER_SEQUENCE MEZ_BASH_RECEIVER_SEQUENCE_READ MEZ_BASH_RECEIVER_B64 MEZ_BASH_RECEIVER_SOURCE MEZ_BASH_RECEIVER_ACTUAL_LENGTH MEZ_BASH_RECEIVER_ACTUAL_DIGEST MEZ_BASH_RECEIVER_STATUS MEZ_BASH_RECEIVER_VERSION MEZ_BASH_RECEIVER_PARENT_PROOF MEZ_BASH_RECEIVER_OUTCOME MEZ_BASH_RECEIVER_REASON MEZ_BASH_RECEIVER_SAVED_LINE MEZ_BASH_RECEIVER_SAVED_LINE_SET MEZ_BASH_RECEIVER_SAVED_POINT MEZ_BASH_RECEIVER_SAVED_MARK MEZ_BASH_RECEIVER_SAVED_MARK_SET
}
__mez_bash_receiver() {
    MEZ_BASH_RECEIVER_SAVED_LINE=$READLINE_LINE
    MEZ_BASH_RECEIVER_SAVED_LINE_SET=1
    MEZ_BASH_RECEIVER_SAVED_POINT=$READLINE_POINT
    if [[ ${READLINE_MARK+x} ]]; then
        MEZ_BASH_RECEIVER_SAVED_MARK=$READLINE_MARK
        MEZ_BASH_RECEIVER_SAVED_MARK_SET=1
    else
        MEZ_BASH_RECEIVER_SAVED_MARK_SET=0
    fi
    READLINE_LINE=
    READLINE_POINT=0
    READLINE_MARK=0
    IFS=' ' builtin read -r MEZ_BASH_RECEIVER_KIND MEZ_BASH_RECEIVER_FRAME_TOKEN MEZ_BASH_RECEIVER_FRAME_MARKER MEZ_BASH_RECEIVER_LENGTH MEZ_BASH_RECEIVER_DIGEST MEZ_BASH_RECEIVER_CHUNKS MEZ_BASH_RECEIVER_PARENT_PROOF || { __mez_bash_receiver_reset; return 1; }
    MEZ_BASH_RECEIVER_KIND=${MEZ_BASH_RECEIVER_KIND#$'\a'}
    if [[ $MEZ_BASH_RECEIVER_KIND == MEZ_BASH_RX1_BEGIN && -z $MEZ_BASH_RECEIVER_PARENT_PROOF ]]; then
        MEZ_BASH_RECEIVER_VERSION=RX1
    elif [[ $MEZ_BASH_RECEIVER_KIND == MEZ_BASH_RX2_BEGIN && $MEZ_BASH_RECEIVER_PARENT_PROOF =~ ^[0-9a-f]{32,}$ ]]; then
        MEZ_BASH_RECEIVER_VERSION=RX2
    else
        __mez_bash_receiver_reset
        return 1
    fi
    [[ $MEZ_BASH_RECEIVER_FRAME_TOKEN == "$MEZ_BASH_RECEIVER_TOKEN" ]] || { __mez_bash_receiver_reset; return 1; }
    [[ $MEZ_BASH_RECEIVER_FRAME_MARKER && $MEZ_BASH_RECEIVER_LENGTH =~ ^[0-9]+$ && $MEZ_BASH_RECEIVER_LENGTH -le 16777216 && $MEZ_BASH_RECEIVER_DIGEST =~ ^[0-9a-f]{64}$ && $MEZ_BASH_RECEIVER_CHUNKS =~ ^[0-9]+$ && $MEZ_BASH_RECEIVER_CHUNKS -le 34953 ]] || { __mez_bash_receiver_reset; return 1; }
    command printf '\033]133;R;mez_protocol=2;mez_shell=bash;mez_token=%s;mez_event=editor-held;mez_marker=%s\033\\' "$MEZ_BASH_RECEIVER_TOKEN" "$MEZ_BASH_RECEIVER_FRAME_MARKER"
    command printf '\033]133;R;mez_protocol=2;mez_shell=bash;mez_token=%s;mez_event=frame-admitted;mez_marker=%s\033\\' "$MEZ_BASH_RECEIVER_TOKEN" "$MEZ_BASH_RECEIVER_FRAME_MARKER"
    MEZ_BASH_RECEIVER_SEQUENCE=0
    MEZ_BASH_RECEIVER_B64=
    MEZ_BASH_RECEIVER_OUTCOME=
    while (( MEZ_BASH_RECEIVER_SEQUENCE < MEZ_BASH_RECEIVER_CHUNKS )); do
        IFS=' ' builtin read -r MEZ_BASH_RECEIVER_KIND MEZ_BASH_RECEIVER_FRAME_TOKEN MEZ_BASH_RECEIVER_FRAME_MARKER_READ MEZ_BASH_RECEIVER_SEQUENCE_READ MEZ_BASH_RECEIVER_FRAME || { __mez_bash_receiver_reset; return 1; }
        if [[ $MEZ_BASH_RECEIVER_VERSION == RX2 && $MEZ_BASH_RECEIVER_SEQUENCE == 0 && $MEZ_BASH_RECEIVER_KIND == MEZ_BASH_RX2_CANCEL && $MEZ_BASH_RECEIVER_FRAME_TOKEN == "$MEZ_BASH_RECEIVER_TOKEN" && $MEZ_BASH_RECEIVER_FRAME_MARKER_READ == "$MEZ_BASH_RECEIVER_FRAME_MARKER" && $MEZ_BASH_RECEIVER_SEQUENCE_READ == "$MEZ_BASH_RECEIVER_PARENT_PROOF" && -z $MEZ_BASH_RECEIVER_FRAME ]]; then
            MEZ_BASH_RECEIVER_STATUS=130
            MEZ_BASH_RECEIVER_OUTCOME=cancelled
            command printf '\036'
            break
        fi
        if [[ $MEZ_BASH_RECEIVER_KIND == MEZ_BASH_${MEZ_BASH_RECEIVER_VERSION}_DATA && $MEZ_BASH_RECEIVER_FRAME_TOKEN == "$MEZ_BASH_RECEIVER_TOKEN" && $MEZ_BASH_RECEIVER_FRAME_MARKER_READ == "$MEZ_BASH_RECEIVER_FRAME_MARKER" && $MEZ_BASH_RECEIVER_SEQUENCE_READ == "$MEZ_BASH_RECEIVER_SEQUENCE" && $MEZ_BASH_RECEIVER_FRAME =~ ^[A-Za-z0-9+/]*={0,2}$ ]]; then
            if [[ -z $MEZ_BASH_RECEIVER_OUTCOME ]]; then
                MEZ_BASH_RECEIVER_B64+=$MEZ_BASH_RECEIVER_FRAME
            fi
        elif [[ -z $MEZ_BASH_RECEIVER_OUTCOME ]]; then
            MEZ_BASH_RECEIVER_OUTCOME=frame-rejected
            MEZ_BASH_RECEIVER_REASON=malformed-data
        fi
        (( MEZ_BASH_RECEIVER_SEQUENCE += 1 ))
        command printf '\036'
    done
    if [[ $MEZ_BASH_RECEIVER_OUTCOME != cancelled ]]; then
        IFS=' ' builtin read -r MEZ_BASH_RECEIVER_KIND MEZ_BASH_RECEIVER_FRAME_TOKEN MEZ_BASH_RECEIVER_FRAME_MARKER_READ MEZ_BASH_RECEIVER_SEQUENCE_READ MEZ_BASH_RECEIVER_LENGTH_READ MEZ_BASH_RECEIVER_DIGEST_READ || { __mez_bash_receiver_reset; return 1; }
        if [[ $MEZ_BASH_RECEIVER_KIND != MEZ_BASH_${MEZ_BASH_RECEIVER_VERSION}_END || $MEZ_BASH_RECEIVER_FRAME_TOKEN != "$MEZ_BASH_RECEIVER_TOKEN" || $MEZ_BASH_RECEIVER_FRAME_MARKER_READ != "$MEZ_BASH_RECEIVER_FRAME_MARKER" || $MEZ_BASH_RECEIVER_SEQUENCE_READ != "$MEZ_BASH_RECEIVER_CHUNKS" || $MEZ_BASH_RECEIVER_LENGTH_READ != "$MEZ_BASH_RECEIVER_LENGTH" || $MEZ_BASH_RECEIVER_DIGEST_READ != "$MEZ_BASH_RECEIVER_DIGEST" ]]; then
            MEZ_BASH_RECEIVER_OUTCOME=frame-rejected
            MEZ_BASH_RECEIVER_REASON=malformed-end
        fi
        command printf '\036'
        if [[ $MEZ_BASH_RECEIVER_OUTCOME != frame-rejected ]]; then
            MEZ_BASH_RECEIVER_SOURCE=$({ command printf '%s' "$MEZ_BASH_RECEIVER_B64" | base64 -d 2>/dev/null || command printf '%s' "$MEZ_BASH_RECEIVER_B64" | base64 -D 2>/dev/null; } && command printf x) || {
                MEZ_BASH_RECEIVER_OUTCOME=frame-rejected
                MEZ_BASH_RECEIVER_REASON=invalid-base64
            }
        fi
    fi
    if [[ $MEZ_BASH_RECEIVER_OUTCOME == cancelled ]]; then
        MEZ_BASH_RECEIVER_COMPLETE_MARKER=$MEZ_BASH_RECEIVER_FRAME_MARKER
        MEZ_BASH_RECEIVER_COMPLETE_STATUS=$MEZ_BASH_RECEIVER_STATUS
        MEZ_BASH_RECEIVER_COMPLETE_PROOF=$MEZ_BASH_RECEIVER_PARENT_PROOF
        __mez_bash_receiver_reset
        command printf '\033]133;R;mez_protocol=2;mez_shell=bash;mez_token=%s;mez_event=parent-ready;mez_marker=%s;mez_outcome=cancelled;mez_status=%s;mez_proof=%s\033\\' "$MEZ_BASH_RECEIVER_TOKEN" "$MEZ_BASH_RECEIVER_COMPLETE_MARKER" "$MEZ_BASH_RECEIVER_COMPLETE_STATUS" "$MEZ_BASH_RECEIVER_COMPLETE_PROOF"
        unset MEZ_BASH_RECEIVER_COMPLETE_MARKER MEZ_BASH_RECEIVER_COMPLETE_STATUS MEZ_BASH_RECEIVER_COMPLETE_PROOF
        return 130
    fi
    if [[ $MEZ_BASH_RECEIVER_OUTCOME != frame-rejected ]]; then
        MEZ_BASH_RECEIVER_SOURCE=${MEZ_BASH_RECEIVER_SOURCE%x}
        MEZ_BASH_RECEIVER_ACTUAL_LENGTH=$(command printf '%s' "$MEZ_BASH_RECEIVER_SOURCE" | LC_ALL=C command wc -c | command tr -d '[:space:]') || {
            MEZ_BASH_RECEIVER_OUTCOME=frame-rejected
            MEZ_BASH_RECEIVER_REASON=length-unavailable
        }
    fi
    if [[ $MEZ_BASH_RECEIVER_OUTCOME != frame-rejected ]]; then
        if command -v sha256sum >/dev/null 2>&1; then
            MEZ_BASH_RECEIVER_ACTUAL_DIGEST=$(command printf '%s' "$MEZ_BASH_RECEIVER_SOURCE" | sha256sum)
        elif command -v shasum >/dev/null 2>&1; then
            MEZ_BASH_RECEIVER_ACTUAL_DIGEST=$(command printf '%s' "$MEZ_BASH_RECEIVER_SOURCE" | shasum -a 256)
        else
            MEZ_BASH_RECEIVER_OUTCOME=frame-rejected
            MEZ_BASH_RECEIVER_REASON=digest-unavailable
        fi
    fi
    if [[ $MEZ_BASH_RECEIVER_OUTCOME != frame-rejected ]]; then
        MEZ_BASH_RECEIVER_ACTUAL_DIGEST=${MEZ_BASH_RECEIVER_ACTUAL_DIGEST%%[[:space:]]*}
        if [[ $MEZ_BASH_RECEIVER_ACTUAL_LENGTH != "$MEZ_BASH_RECEIVER_LENGTH" || $MEZ_BASH_RECEIVER_ACTUAL_DIGEST != "$MEZ_BASH_RECEIVER_DIGEST" ]]; then
            MEZ_BASH_RECEIVER_OUTCOME=frame-rejected
            MEZ_BASH_RECEIVER_REASON=integrity-mismatch
        fi
    fi
    if [[ $MEZ_BASH_RECEIVER_OUTCOME == frame-rejected ]]; then
        MEZ_BASH_RECEIVER_COMPLETE_MARKER=$MEZ_BASH_RECEIVER_FRAME_MARKER
        MEZ_BASH_RECEIVER_COMPLETE_VERSION=$MEZ_BASH_RECEIVER_VERSION
        MEZ_BASH_RECEIVER_COMPLETE_PROOF=$MEZ_BASH_RECEIVER_PARENT_PROOF
        MEZ_BASH_RECEIVER_COMPLETE_REASON=$MEZ_BASH_RECEIVER_REASON
        __mez_bash_receiver_reset
        if [[ $MEZ_BASH_RECEIVER_COMPLETE_VERSION == RX2 ]]; then
            command printf '\033]133;R;mez_protocol=2;mez_shell=bash;mez_token=%s;mez_event=parent-ready;mez_marker=%s;mez_outcome=frame-rejected;mez_status=65;mez_proof=%s\033\\' "$MEZ_BASH_RECEIVER_TOKEN" "$MEZ_BASH_RECEIVER_COMPLETE_MARKER" "$MEZ_BASH_RECEIVER_COMPLETE_PROOF"
        else
            command printf '\033]133;R;mez_protocol=2;mez_shell=bash;mez_token=%s;mez_event=receiver-rejected;mez_marker=%s;mez_reason=%s\033\\' "$MEZ_BASH_RECEIVER_TOKEN" "$MEZ_BASH_RECEIVER_COMPLETE_MARKER" "$MEZ_BASH_RECEIVER_COMPLETE_REASON"
        fi
        unset MEZ_BASH_RECEIVER_COMPLETE_MARKER MEZ_BASH_RECEIVER_COMPLETE_VERSION MEZ_BASH_RECEIVER_COMPLETE_PROOF MEZ_BASH_RECEIVER_COMPLETE_REASON
        return 65
    fi
    eval "$MEZ_BASH_RECEIVER_SOURCE"
    MEZ_BASH_RECEIVER_STATUS=$?
    MEZ_BASH_RECEIVER_COMPLETE_MARKER=$MEZ_BASH_RECEIVER_FRAME_MARKER
    MEZ_BASH_RECEIVER_COMPLETE_STATUS=$MEZ_BASH_RECEIVER_STATUS
    MEZ_BASH_RECEIVER_COMPLETE_VERSION=$MEZ_BASH_RECEIVER_VERSION
    MEZ_BASH_RECEIVER_COMPLETE_PROOF=$MEZ_BASH_RECEIVER_PARENT_PROOF
    __mez_bash_receiver_reset
    MEZ_BASH_RECEIVER_COMPLETE_OUTCOME=completed
    [[ $MEZ_BASH_RECEIVER_COMPLETE_STATUS == 0 ]] || MEZ_BASH_RECEIVER_COMPLETE_OUTCOME=source-failed
    if [[ $MEZ_BASH_RECEIVER_COMPLETE_VERSION == RX2 ]]; then
        command printf '\033]133;R;mez_protocol=2;mez_shell=bash;mez_token=%s;mez_event=parent-ready;mez_marker=%s;mez_outcome=%s;mez_status=%s;mez_proof=%s\033\\' "$MEZ_BASH_RECEIVER_TOKEN" "$MEZ_BASH_RECEIVER_COMPLETE_MARKER" "$MEZ_BASH_RECEIVER_COMPLETE_OUTCOME" "$MEZ_BASH_RECEIVER_COMPLETE_STATUS" "$MEZ_BASH_RECEIVER_COMPLETE_PROOF"
    else
        command printf '\033]133;R;mez_protocol=2;mez_shell=bash;mez_token=%s;mez_event=parent-ready;mez_marker=%s;mez_outcome=%s;mez_status=%s\033\\' "$MEZ_BASH_RECEIVER_TOKEN" "$MEZ_BASH_RECEIVER_COMPLETE_MARKER" "$MEZ_BASH_RECEIVER_COMPLETE_OUTCOME" "$MEZ_BASH_RECEIVER_COMPLETE_STATUS"
    fi
    unset MEZ_BASH_RECEIVER_COMPLETE_MARKER MEZ_BASH_RECEIVER_COMPLETE_STATUS MEZ_BASH_RECEIVER_COMPLETE_VERSION MEZ_BASH_RECEIVER_COMPLETE_PROOF MEZ_BASH_RECEIVER_COMPLETE_OUTCOME
}
bind -m emacs-standard -x '"\C-g":__mez_bash_receiver'
bind -m vi-insert -x '"\C-g":__mez_bash_receiver'
bind -m vi-command -x '"\C-g":__mez_bash_receiver'
if [[ -n ${MEZ_BASH_RECEIVER_INSTALL_MARKER-} ]]; then
    MEZ_BASH_RECEIVER_INSTALLED_MARKER=$MEZ_BASH_RECEIVER_INSTALL_MARKER
    unset MEZ_BASH_RECEIVER_INSTALL_MARKER
    command printf '\033]133;R;mez_protocol=2;mez_shell=bash;mez_token=%s;mez_event=child-installed;mez_marker=%s\033\\' "$MEZ_BASH_RECEIVER_TOKEN" "$MEZ_BASH_RECEIVER_INSTALLED_MARKER"
    unset MEZ_BASH_RECEIVER_INSTALLED_MARKER
    __mez_bash_receiver
fi
command printf '\033]133;R;mez_protocol=2;mez_shell=bash;mez_token=%s;mez_event=adapter-available\033\\' "$MEZ_BASH_RECEIVER_TOKEN"
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

    /// Runs one complete managed Bash receiver exchange against a real shell.
    ///
    /// The caller supplies the private trigger, protocol records, a follow-up
    /// parent command, and `exit`. The helper keeps the startup artifact alive
    /// through shell exit and returns all control and command output.
    fn run_managed_bash_receiver_exchange(test_name: &str, input: &[u8]) -> std::process::Output {
        let bash = Path::new("/bin/bash");
        let root = std::env::temp_dir().join(format!(
            "mez-bash-{test_name}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join(".bashrc"), "PS1=\n").unwrap();
        let token = MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap();
        let compatibility =
            ManagedBashCompatibility::create(&root.join("control.sock"), "%1", token).unwrap();
        let launch = compatibility.configure_launch(PaneProcessLaunch::new(bash.to_path_buf()));
        let plan = pane_command_plan(&launch, None).unwrap();
        let mut command = Command::new(plan.program);
        command
            .args(plan.args)
            .env("HOME", &home)
            .env("HISTFILE", "/dev/null")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in launch.environment() {
            command.env(key, value);
        }
        let mut child = command.spawn().unwrap();
        child.stdin.as_mut().unwrap().write_all(input).unwrap();
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        drop(compatibility);
        fs::remove_dir_all(root).unwrap();
        output
    }

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

    /// Verifies authenticated RX2 cancellation is acknowledged before source
    /// delivery, evaluates no source, and returns the parent shell immediately.
    ///
    /// The callback must emit proof-bearing typed readiness only after its
    /// Readline cleanup and must leave the parent able to execute the very next
    /// command without an intervening prompt heuristic.
    #[test]
    fn managed_bash_receiver_cancels_rx2_before_source_delivery() {
        if !Path::new("/bin/bash").exists() {
            return;
        }
        let token = "0123456789abcdef0123456789abcdef";
        let proof = "fedcba9876543210fedcba9876543210";
        let marker = "rx2-cancel-marker";
        let source = "printf '__MEZ_CANCELLED_SOURCE_RAN__\\n'";
        let digest = Sha256::digest(source.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let input = format!(
            "\x07MEZ_BASH_RX2_BEGIN {token} {marker} {} {digest} 1 {proof}\n\
MEZ_BASH_RX2_CANCEL {token} {marker} {proof}\n\
printf '__MEZ_PARENT_AFTER_CANCEL__\\n'\n\
exit\n",
            source.len()
        );

        let output = run_managed_bash_receiver_exchange("rx2-cancel", input.as_bytes());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            output.status.success(),
            "stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            !stdout.contains("__MEZ_CANCELLED_SOURCE_RAN__"),
            "{stdout:?}"
        );
        assert!(stdout.contains("__MEZ_PARENT_AFTER_CANCEL__"), "{stdout:?}");
        assert!(
            stdout.contains(&format!(
                "mez_event=parent-ready;mez_marker={marker};mez_outcome=cancelled;mez_status=130;mez_proof={proof}"
            )),
            "{stdout:?}"
        );
        assert_eq!(stdout.bytes().filter(|byte| *byte == 0x1e).count(), 1);
    }

    /// Verifies an admitted malformed RX2 frame is drained through its END
    /// record, acknowledged record-by-record, and never evaluated.
    ///
    /// Typed rejection must retain the parent-only proof and callback cleanup
    /// must leave the original parent responsive to an immediate command.
    #[test]
    fn managed_bash_receiver_drains_malformed_rx2_frame_before_rejection() {
        if !Path::new("/bin/bash").exists() {
            return;
        }
        let token = "0123456789abcdef0123456789abcdef";
        let proof = "fedcba9876543210fedcba9876543210";
        let marker = "rx2-malformed-marker";
        let source = "printf '__MEZ_MALFORMED_SOURCE_RAN__\\n'";
        let digest = Sha256::digest(source.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let encoded = base64::engine::general_purpose::STANDARD.encode(source);
        let input = format!(
            "\x07MEZ_BASH_RX2_BEGIN {token} {marker} {} {digest} 2 {proof}\n\
MEZ_BASH_RX2_DATA {token} {marker} 0 !\n\
MEZ_BASH_RX2_DATA {token} {marker} 1 {encoded}\n\
MEZ_BASH_RX2_END {token} {marker} 2 {} {digest}\n\
printf '__MEZ_PARENT_AFTER_REJECTION__\\n'\n\
exit\n",
            source.len(),
            source.len()
        );

        let output = run_managed_bash_receiver_exchange("rx2-malformed", input.as_bytes());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            output.status.success(),
            "stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            !stdout.contains("__MEZ_MALFORMED_SOURCE_RAN__"),
            "{stdout:?}"
        );
        assert!(
            stdout.contains("__MEZ_PARENT_AFTER_REJECTION__"),
            "{stdout:?}"
        );
        assert!(
            stdout.contains(&format!(
                "mez_event=parent-ready;mez_marker={marker};mez_outcome=frame-rejected;mez_status=65;mez_proof={proof}"
            )),
            "{stdout:?}"
        );
        assert_eq!(stdout.bytes().filter(|byte| *byte == 0x1e).count(), 3);
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
