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

use base64::Engine as _;
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
    managed_bash_receiver_source_with_prelude(
        token,
        r#"if [[ -r ${MEZ_BASH_USER_RCFILE:-$HOME/.bashrc} ]]; then builtin source -- "${MEZ_BASH_USER_RCFILE:-$HOME/.bashrc}"; fi"#,
        r#"command printf '\033]133;R;mez_protocol=2;mez_shell=bash;mez_token=%s;mez_event=adapter-available\033\\' "$MEZ_BASH_RECEIVER_TOKEN""#,
    )
}

/// Renders private parent-receiver source that stages and runs one managed
/// Bash child entirely inside the active foreign filesystem.
///
/// The owner-only directory and rcfile are removed after the synchronous child
/// returns. The source itself is delivered only inside authenticated RX2 DATA
/// records; the trigger contains no child token, rcfile contents, or path.
pub(super) fn managed_foreign_bash_child_staging_source(
    shell_path: &Path,
    bootstrap_marker: &str,
    child_token: &MarkerToken,
) -> String {
    let rcfile_source = managed_bash_receiver_source(child_token);
    let encoded_rcfile = base64::engine::general_purpose::STANDARD.encode(rcfile_source.as_bytes());
    let directory_name = format!(".mez-bash-{}", &child_token.as_str()[..16]);
    let shell = mez_agent::shell_quote(&shell_path.to_string_lossy());
    let marker = mez_agent::shell_quote(bootstrap_marker);
    let encoded = mez_agent::shell_quote(&encoded_rcfile);
    format!(
        "__mez_foreign_bash_stage_child() {{\n\
    umask 077\n\
    MEZ_FOREIGN_BASH_DIR=${{TMPDIR:-/tmp}}/{directory_name}\n\
    command mkdir -m 700 \"$MEZ_FOREIGN_BASH_DIR\" || return 70\n\
    MEZ_FOREIGN_BASH_RCFILE=$MEZ_FOREIGN_BASH_DIR/bashrc\n\
    command printf '%s' {encoded} | command base64 -d > \"$MEZ_FOREIGN_BASH_RCFILE\" 2>/dev/null || command printf '%s' {encoded} | command base64 -D > \"$MEZ_FOREIGN_BASH_RCFILE\" 2>/dev/null || {{ command rm -rf -- \"$MEZ_FOREIGN_BASH_DIR\"; return 71; }}\n\
    command chmod 600 \"$MEZ_FOREIGN_BASH_RCFILE\" || {{ command rm -rf -- \"$MEZ_FOREIGN_BASH_DIR\"; return 72; }}\n\
    # child-token:{}\n\
    MEZ_BASH_RECEIVER_INSTALL_MARKER={marker} {shell} --noprofile --rcfile \"$MEZ_FOREIGN_BASH_RCFILE\" -i\n\
    MEZ_FOREIGN_BASH_STATUS=$?\n\
    command rm -rf -- \"$MEZ_FOREIGN_BASH_DIR\" || :\n\
    return \"$MEZ_FOREIGN_BASH_STATUS\"\n\
}}\n\
__mez_foreign_bash_stage_child\n\
MEZ_FOREIGN_BASH_STATUS=$?\n\
unset -f __mez_foreign_bash_stage_child\n\
(exit \"$MEZ_FOREIGN_BASH_STATUS\")",
        child_token.as_str()
    )
}

/// Renders dependency-free Bash staging with the runtime's parent-return boundary.
///
/// The loader owns the unmanaged foreign parent rather than a preinstalled
/// adapter, so it must publish this boundary before its correlated exit event.
pub(super) fn managed_dependency_free_foreign_bash_child_staging_source(
    shell_path: &Path,
    bootstrap_marker: &str,
    child_token: &MarkerToken,
    exit_marker: &MarkerToken,
) -> String {
    let staging =
        managed_foreign_bash_child_staging_source(shell_path, bootstrap_marker, child_token);
    let boundary = format!(
        "command printf '\\033]133;mez_agent_subshell_exit={}\\033\\\\'\n",
        exit_marker.as_str()
    );
    staging.replacen(
        "\n(exit \"$MEZ_FOREIGN_BASH_STATUS\")",
        &format!("\n{boundary}(exit \"$MEZ_FOREIGN_BASH_STATUS\")"),
        1,
    )
}

/// Renders the common authenticated Bash receiver with caller-owned startup
/// and readiness behavior.
fn managed_bash_receiver_source_with_prelude(
    token: &MarkerToken,
    prelude: &str,
    ready: &str,
) -> String {
    const TEMPLATE: &str = r#"__MEZ_BASH_RECEIVER_PRELUDE__
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
    unset MEZ_BASH_RECEIVER_FRAME MEZ_BASH_RECEIVER_KIND MEZ_BASH_RECEIVER_FRAME_TOKEN MEZ_BASH_RECEIVER_FRAME_MARKER MEZ_BASH_RECEIVER_FRAME_MARKER_READ MEZ_BASH_RECEIVER_LENGTH MEZ_BASH_RECEIVER_LENGTH_READ MEZ_BASH_RECEIVER_DIGEST MEZ_BASH_RECEIVER_DIGEST_READ MEZ_BASH_RECEIVER_CHUNKS MEZ_BASH_RECEIVER_SEQUENCE MEZ_BASH_RECEIVER_SEQUENCE_READ MEZ_BASH_RECEIVER_B64 MEZ_BASH_RECEIVER_SOURCE MEZ_BASH_RECEIVER_ACTUAL_LENGTH MEZ_BASH_RECEIVER_ACTUAL_DIGEST MEZ_BASH_RECEIVER_STATUS MEZ_BASH_RECEIVER_VERSION MEZ_BASH_RECEIVER_PARENT_PROOF MEZ_BASH_RECEIVER_OUTCOME MEZ_BASH_RECEIVER_REASON MEZ_BASH_RECEIVER_FRAME_SEQUENCE MEZ_BASH_RECEIVER_FRAME_SEQUENCE_READ MEZ_BASH_RECEIVER_FRAME_LENGTH MEZ_BASH_RECEIVER_FRAME_DIGEST MEZ_BASH_RECEIVER_FRAME_CHUNKS MEZ_BASH_RECEIVER_FRAME_B64 MEZ_BASH_RECEIVER_FRAME_ACTUAL_DIGEST MEZ_BASH_RECEIVER_FRAME_VALID MEZ_BASH_RECEIVER_SAVED_LINE MEZ_BASH_RECEIVER_SAVED_LINE_SET MEZ_BASH_RECEIVER_SAVED_POINT MEZ_BASH_RECEIVER_SAVED_MARK MEZ_BASH_RECEIVER_SAVED_MARK_SET
}
__mez_bash_receiver_inner() {
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
    if [[ $MEZ_BASH_RECEIVER_KIND == MEZ_BASH_FOREIGN_CHALLENGE && $MEZ_BASH_RECEIVER_FRAME_TOKEN == "$MEZ_BASH_RECEIVER_TOKEN" && $MEZ_BASH_RECEIVER_FRAME_MARKER == "${MEZ_BASH_FOREIGN_INSTANCE-}" && $MEZ_BASH_RECEIVER_LENGTH =~ ^[0-9a-f]{32,}$ && -z $MEZ_BASH_RECEIVER_DIGEST && -z $MEZ_BASH_RECEIVER_CHUNKS && -z $MEZ_BASH_RECEIVER_PARENT_PROOF ]]; then
        MEZ_BASH_FOREIGN_COMPLETED_INSTANCE=$MEZ_BASH_RECEIVER_FRAME_MARKER
        MEZ_BASH_FOREIGN_COMPLETED_CHALLENGE=$MEZ_BASH_RECEIVER_LENGTH
        __mez_bash_receiver_reset
        command printf '\033]133;R;mez_protocol=2;mez_shell=bash;mez_token=%s;mez_event=foreign-challenge-completed;mez_instance=%s;mez_challenge=%s\033\\' "$MEZ_BASH_RECEIVER_TOKEN" "$MEZ_BASH_FOREIGN_COMPLETED_INSTANCE" "$MEZ_BASH_FOREIGN_COMPLETED_CHALLENGE"
        unset MEZ_BASH_FOREIGN_COMPLETED_INSTANCE MEZ_BASH_FOREIGN_COMPLETED_CHALLENGE
        return 0
    fi
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
    if [[ $MEZ_BASH_RECEIVER_VERSION == RX1 ]]; then
        while (( MEZ_BASH_RECEIVER_SEQUENCE < MEZ_BASH_RECEIVER_CHUNKS )); do
            IFS=' ' builtin read -r MEZ_BASH_RECEIVER_KIND MEZ_BASH_RECEIVER_FRAME_TOKEN MEZ_BASH_RECEIVER_FRAME_MARKER_READ MEZ_BASH_RECEIVER_SEQUENCE_READ MEZ_BASH_RECEIVER_FRAME || { __mez_bash_receiver_reset; return 1; }
            if [[ $MEZ_BASH_RECEIVER_KIND == MEZ_BASH_RX1_DATA && $MEZ_BASH_RECEIVER_FRAME_TOKEN == "$MEZ_BASH_RECEIVER_TOKEN" && $MEZ_BASH_RECEIVER_FRAME_MARKER_READ == "$MEZ_BASH_RECEIVER_FRAME_MARKER" && $MEZ_BASH_RECEIVER_SEQUENCE_READ == "$MEZ_BASH_RECEIVER_SEQUENCE" && $MEZ_BASH_RECEIVER_FRAME =~ ^[A-Za-z0-9+/]*={0,2}$ ]]; then
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
    else
        MEZ_BASH_RECEIVER_FRAME_SEQUENCE=0
        while (( MEZ_BASH_RECEIVER_SEQUENCE < MEZ_BASH_RECEIVER_CHUNKS )); do
            IFS=' ' builtin read -r MEZ_BASH_RECEIVER_KIND MEZ_BASH_RECEIVER_FRAME_TOKEN MEZ_BASH_RECEIVER_FRAME_MARKER_READ MEZ_BASH_RECEIVER_FRAME_SEQUENCE_READ MEZ_BASH_RECEIVER_FRAME_LENGTH MEZ_BASH_RECEIVER_FRAME_DIGEST MEZ_BASH_RECEIVER_FRAME_CHUNKS || { __mez_bash_receiver_reset; return 1; }
            if [[ $MEZ_BASH_RECEIVER_SEQUENCE == 0 && $MEZ_BASH_RECEIVER_KIND == MEZ_BASH_RX2_CANCEL && $MEZ_BASH_RECEIVER_FRAME_TOKEN == "$MEZ_BASH_RECEIVER_TOKEN" && $MEZ_BASH_RECEIVER_FRAME_MARKER_READ == "$MEZ_BASH_RECEIVER_FRAME_MARKER" && $MEZ_BASH_RECEIVER_FRAME_SEQUENCE_READ == "$MEZ_BASH_RECEIVER_PARENT_PROOF" && -z $MEZ_BASH_RECEIVER_FRAME_LENGTH && -z $MEZ_BASH_RECEIVER_FRAME_DIGEST && -z $MEZ_BASH_RECEIVER_FRAME_CHUNKS ]]; then
                MEZ_BASH_RECEIVER_STATUS=130
                MEZ_BASH_RECEIVER_OUTCOME=cancelled
                command printf '\036'
                break
            fi
            MEZ_BASH_RECEIVER_FRAME_VALID=1
            if [[ $MEZ_BASH_RECEIVER_KIND != MEZ_BASH_RX2_FRAME || $MEZ_BASH_RECEIVER_FRAME_TOKEN != "$MEZ_BASH_RECEIVER_TOKEN" || $MEZ_BASH_RECEIVER_FRAME_MARKER_READ != "$MEZ_BASH_RECEIVER_FRAME_MARKER" || $MEZ_BASH_RECEIVER_FRAME_SEQUENCE_READ != "$MEZ_BASH_RECEIVER_FRAME_SEQUENCE" || ! $MEZ_BASH_RECEIVER_FRAME_LENGTH =~ ^[0-9]+$ || $MEZ_BASH_RECEIVER_FRAME_LENGTH -gt 32768 || ! $MEZ_BASH_RECEIVER_FRAME_DIGEST =~ ^[0-9a-f]{64}$ || ! $MEZ_BASH_RECEIVER_FRAME_CHUNKS =~ ^[0-9]+$ || $MEZ_BASH_RECEIVER_FRAME_CHUNKS -gt 512 || $((MEZ_BASH_RECEIVER_SEQUENCE + MEZ_BASH_RECEIVER_FRAME_CHUNKS)) -gt MEZ_BASH_RECEIVER_CHUNKS ]]; then
                MEZ_BASH_RECEIVER_FRAME_VALID=0
                MEZ_BASH_RECEIVER_OUTCOME=frame-rejected
                MEZ_BASH_RECEIVER_REASON=malformed-frame
            fi
            MEZ_BASH_RECEIVER_FRAME_B64=
            while (( MEZ_BASH_RECEIVER_FRAME_CHUNKS > 0 )); do
                IFS=' ' builtin read -r MEZ_BASH_RECEIVER_KIND MEZ_BASH_RECEIVER_FRAME_TOKEN MEZ_BASH_RECEIVER_FRAME_MARKER_READ MEZ_BASH_RECEIVER_SEQUENCE_READ MEZ_BASH_RECEIVER_FRAME || { __mez_bash_receiver_reset; return 1; }
                if [[ $MEZ_BASH_RECEIVER_KIND == MEZ_BASH_RX2_DATA && $MEZ_BASH_RECEIVER_FRAME_TOKEN == "$MEZ_BASH_RECEIVER_TOKEN" && $MEZ_BASH_RECEIVER_FRAME_MARKER_READ == "$MEZ_BASH_RECEIVER_FRAME_MARKER" && $MEZ_BASH_RECEIVER_SEQUENCE_READ == "$MEZ_BASH_RECEIVER_SEQUENCE" && $MEZ_BASH_RECEIVER_FRAME =~ ^[A-Za-z0-9+/]*={0,2}$ ]]; then
                    if [[ $MEZ_BASH_RECEIVER_FRAME_VALID == 1 ]]; then
                        MEZ_BASH_RECEIVER_FRAME_B64+=$MEZ_BASH_RECEIVER_FRAME
                    fi
                else
                    MEZ_BASH_RECEIVER_FRAME_VALID=0
                    MEZ_BASH_RECEIVER_OUTCOME=frame-rejected
                    MEZ_BASH_RECEIVER_REASON=malformed-data
                fi
                (( MEZ_BASH_RECEIVER_SEQUENCE += 1 ))
                (( MEZ_BASH_RECEIVER_FRAME_CHUNKS -= 1 ))
            done
            IFS=' ' builtin read -r MEZ_BASH_RECEIVER_KIND MEZ_BASH_RECEIVER_FRAME_TOKEN MEZ_BASH_RECEIVER_FRAME_MARKER_READ MEZ_BASH_RECEIVER_FRAME_SEQUENCE_READ MEZ_BASH_RECEIVER_SEQUENCE_READ MEZ_BASH_RECEIVER_FRAME || { __mez_bash_receiver_reset; return 1; }
            if [[ $MEZ_BASH_RECEIVER_KIND != MEZ_BASH_RX2_FRAME_END || $MEZ_BASH_RECEIVER_FRAME_TOKEN != "$MEZ_BASH_RECEIVER_TOKEN" || $MEZ_BASH_RECEIVER_FRAME_MARKER_READ != "$MEZ_BASH_RECEIVER_FRAME_MARKER" || $MEZ_BASH_RECEIVER_FRAME_SEQUENCE_READ != "$MEZ_BASH_RECEIVER_FRAME_SEQUENCE" || $MEZ_BASH_RECEIVER_SEQUENCE_READ != "$MEZ_BASH_RECEIVER_SEQUENCE" || -n $MEZ_BASH_RECEIVER_FRAME ]]; then
                MEZ_BASH_RECEIVER_FRAME_VALID=0
                MEZ_BASH_RECEIVER_OUTCOME=frame-rejected
                MEZ_BASH_RECEIVER_REASON=malformed-frame-end
            fi
            if [[ $MEZ_BASH_RECEIVER_FRAME_VALID == 1 ]]; then
                if command -v sha256sum >/dev/null 2>&1; then
                    MEZ_BASH_RECEIVER_FRAME_ACTUAL_DIGEST=$(command printf '%s' "$MEZ_BASH_RECEIVER_FRAME_B64" | sha256sum)
                elif command -v shasum >/dev/null 2>&1; then
                    MEZ_BASH_RECEIVER_FRAME_ACTUAL_DIGEST=$(command printf '%s' "$MEZ_BASH_RECEIVER_FRAME_B64" | shasum -a 256)
                else
                    MEZ_BASH_RECEIVER_FRAME_VALID=0
                    MEZ_BASH_RECEIVER_OUTCOME=frame-rejected
                    MEZ_BASH_RECEIVER_REASON=digest-unavailable
                fi
                MEZ_BASH_RECEIVER_FRAME_ACTUAL_DIGEST=${MEZ_BASH_RECEIVER_FRAME_ACTUAL_DIGEST%%[[:space:]]*}
                if [[ ${#MEZ_BASH_RECEIVER_FRAME_B64} != "$MEZ_BASH_RECEIVER_FRAME_LENGTH" || $MEZ_BASH_RECEIVER_FRAME_ACTUAL_DIGEST != "$MEZ_BASH_RECEIVER_FRAME_DIGEST" ]]; then
                    MEZ_BASH_RECEIVER_FRAME_VALID=0
                    MEZ_BASH_RECEIVER_OUTCOME=frame-rejected
                    MEZ_BASH_RECEIVER_REASON=frame-integrity-mismatch
                fi
            fi
            if [[ $MEZ_BASH_RECEIVER_FRAME_VALID == 1 ]]; then
                MEZ_BASH_RECEIVER_B64+=$MEZ_BASH_RECEIVER_FRAME_B64
            fi
            (( MEZ_BASH_RECEIVER_FRAME_SEQUENCE += 1 ))
            command printf '\036'
        done
    fi
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
__mez_bash_receiver() {
    local __mez_bash_callback_restore_errexit=0
    local __mez_bash_callback_restore_nounset=0
    case $- in *e*) __mez_bash_callback_restore_errexit=1; set +e;; esac
    case $- in *u*) __mez_bash_callback_restore_nounset=1; set +u;; esac
    __mez_bash_receiver_inner
    case "$__mez_bash_callback_restore_nounset" in 1) set -u;; esac
    case "$__mez_bash_callback_restore_errexit" in 1) set -e;; esac
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
__MEZ_BASH_RECEIVER_READY__
"#;
    TEMPLATE
        .replace("__MEZ_BASH_RECEIVER_PRELUDE__", prelude)
        .replace("__MEZ_BASH_RECEIVER_TOKEN__", token.as_str())
        .replace("__MEZ_BASH_RECEIVER_READY__", ready)
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
    use mez_mux::process::pane_command_plan;
    use sha2::{Digest as _, Sha256};
    use std::process::{Command, Stdio};

    /// Runs one complete managed Bash receiver exchange against a real shell.
    ///
    /// The caller supplies the private trigger, protocol records, a follow-up
    /// parent command, and `exit`. The helper keeps the startup artifact alive
    /// through shell exit and returns all control and command output.
    fn run_managed_bash_receiver_exchange(test_name: &str, input: &[u8]) -> std::process::Output {
        run_managed_bash_receiver_exchange_after_events(test_name, input, &[])
    }

    /// Runs one exchange while withholding staged input until receiver events.
    ///
    /// Production releases command payload after the transaction start marker
    /// and queues foreground input until authenticated callback completion.
    /// This helper mirrors that ordering instead of placing later records in
    /// Readline's input buffer while the callback owns it.
    fn run_managed_bash_receiver_exchange_after_events(
        test_name: &str,
        input: &[u8],
        stages: &[(&str, &[u8])],
    ) -> std::process::Output {
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
        let stdout_path = root.join("stdout");
        let stderr_path = root.join("stderr");
        let mut command = Command::new(plan.program);
        command
            .args(plan.args)
            .env("HOME", &home)
            .env("HISTFILE", "/dev/null")
            .stdin(Stdio::piped())
            .stdout(Stdio::from(fs::File::create(&stdout_path).unwrap()))
            .stderr(Stdio::from(fs::File::create(&stderr_path).unwrap()));
        for (key, value) in launch.environment() {
            command.env(key, value);
        }
        let mut child = command.spawn().unwrap();
        child.stdin.as_mut().unwrap().write_all(input).unwrap();
        for (expected_event, staged_input) in stages {
            let mut observed = false;
            for _ in 0..300 {
                let stdout = fs::read(&stdout_path).unwrap_or_default();
                if String::from_utf8_lossy(&stdout).contains(expected_event) {
                    observed = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if !observed {
                drop(child.stdin.take());
                let _ = child.kill();
                let status = child.wait().unwrap();
                let stdout = fs::read(&stdout_path).unwrap_or_default();
                let stderr = fs::read(&stderr_path).unwrap_or_default();
                drop(compatibility);
                let _ = fs::remove_dir_all(&root);
                panic!(
                    "managed Bash event was not observed: event={expected_event:?} status={status:?} stdout={:?} stderr={:?}",
                    String::from_utf8_lossy(&stdout),
                    String::from_utf8_lossy(&stderr),
                );
            }
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(staged_input)
                .unwrap();
        }
        drop(child.stdin.take());
        let status = child.wait().unwrap();
        let output = std::process::Output {
            status,
            stdout: fs::read(&stdout_path).unwrap_or_default(),
            stderr: fs::read(&stderr_path).unwrap_or_default(),
        };
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
            "set -eu\n\x07MEZ_BASH_RX2_BEGIN {token} {marker} {} {digest} 1 {proof}\n\
MEZ_BASH_RX2_CANCEL {token} {marker} {proof}\n\
printf '__MEZ_PARENT_AFTER_CANCEL__:%s\\n' \"$-\"\n\
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
        assert!(
            stdout
                .split("__MEZ_PARENT_AFTER_CANCEL__:")
                .nth(1)
                .and_then(|suffix| suffix.lines().next())
                .is_some_and(|flags| flags.contains('e') && flags.contains('u')),
            "{stdout:?}"
        );
        assert!(
            stdout.contains(&format!(
                "mez_event=parent-ready;mez_marker={marker};mez_outcome=cancelled;mez_status=130;mez_proof={proof}"
            )),
            "{stdout:?}"
        );
        assert_eq!(stdout.bytes().filter(|byte| *byte == 0x1e).count(), 1);
    }

    /// Verifies an admitted malformed RX2 frame is drained through its END
    /// record, acknowledged at bounded frame boundaries, and never evaluated.
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
        let malformed_frame = format!("!{encoded}");
        let frame_digest = Sha256::digest(malformed_frame.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let input = format!(
            "set -eu\n\x07MEZ_BASH_RX2_BEGIN {token} {marker} {} {digest} 2 {proof}\n\
MEZ_BASH_RX2_FRAME {token} {marker} 0 {} {frame_digest} 2\n\
MEZ_BASH_RX2_DATA {token} {marker} 0 !\n\
MEZ_BASH_RX2_DATA {token} {marker} 1 {encoded}\n\
MEZ_BASH_RX2_FRAME_END {token} {marker} 0 2\n\
MEZ_BASH_RX2_END {token} {marker} 2 {} {digest}\n\
printf '__MEZ_PARENT_AFTER_REJECTION__:%s\\n' \"$-\"\n\
exit\n",
            source.len(),
            malformed_frame.len(),
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
            stdout
                .split("__MEZ_PARENT_AFTER_REJECTION__:")
                .nth(1)
                .and_then(|suffix| suffix.lines().next())
                .is_some_and(|flags| flags.contains('e') && flags.contains('u')),
            "{stdout:?}"
        );
        assert!(
            stdout.contains(&format!(
                "mez_event=parent-ready;mez_marker={marker};mez_outcome=frame-rejected;mez_status=65;mez_proof={proof}"
            )),
            "{stdout:?}"
        );
        assert_eq!(stdout.bytes().filter(|byte| *byte == 0x1e).count(), 2);
    }

    /// Verifies the dependency-free loader, staged managed Bash child, and
    /// private bootstrap receiver complete their real wire exchange without
    /// requiring a preinstalled foreign adapter. This covers the generated
    /// process sequence that synthetic runtime event tests cannot exercise.
    #[test]
    fn dependency_free_loader_completes_real_managed_bash_exchange() {
        let bash = Path::new("/bin/bash");
        if !bash.exists() {
            return;
        }
        let marker = MarkerToken::new("00112233445566778899aabbccddeeff").unwrap();
        let child_token = MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap();
        let exit_marker = MarkerToken::new("abcdefabcdefabcdefabcdefabcdefab").unwrap();
        let loader_marker = "fedcba9876543210fedcba9876543210";
        let staging_source = managed_dependency_free_foreign_bash_child_staging_source(
            bash,
            marker.as_str(),
            &child_token,
            &exit_marker,
        );
        let loader = mez_agent::dependency_free_foreign_shell_loader_input(
            &staging_source,
            bash,
            mez_agent::ShellClassification::Bash,
            Some(&child_token),
            loader_marker,
        )
        .unwrap();
        let bootstrap_script =
            mez_agent::bootstrap_script_for_classification(mez_agent::ShellClassification::Bash);
        let bootstrap = mez_agent::ShellTransaction::new(
            marker.clone(),
            "turn-1",
            "agent-1",
            "pane-1",
            bash,
            bootstrap_script,
        )
        .unwrap()
        .with_bash_receiver_token(child_token.clone())
        .render_for_classification_input(mez_agent::ShellClassification::Bash);
        let input = format!(
            "{}{}{}{}{}exit\n",
            loader.command,
            loader.payload,
            bootstrap.wrapper,
            bootstrap.receiver_payload,
            bootstrap.payload
        );

        let mut command = Command::new("/bin/sh");
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            output.status.success(),
            "stdout={stdout:?} stderr={stderr:?}"
        );
        for expected in [
            "mez_foreign_loader=ready",
            "mez_event=child-installed",
            "mez_event=frame-admitted",
            "bootstrap\tcomplete\t",
            "mez_event=parent-ready",
            "mez_agent_subshell_exit=abcdefabcdefabcdefabcdefabcdefab",
            "mez_foreign_loader=exited",
        ] {
            assert!(
                stdout.contains(expected),
                "missing {expected:?}: stdout={stdout:?} stderr={stderr:?}"
            );
        }
        let loader_output = stdout
            .split("mez_event=child-installed")
            .next()
            .expect("loader ready output must precede child installation");
        assert_eq!(
            loader_output.bytes().filter(|byte| *byte == 0x1e).count(),
            1,
            "the loader must acknowledge only its terminating record: {loader_output:?}"
        );
    }

    /// Verifies foreign child staging reports a setup failure through the
    /// authenticated parent-ready event instead of returning from the receiver
    /// callback before its completion protocol can run.
    #[test]
    fn managed_bash_receiver_reports_foreign_child_staging_setup_failure() {
        if !Path::new("/bin/bash").exists() {
            return;
        }
        let token = MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap();
        let child_token = MarkerToken::new("fedcba9876543210fedcba9876543210").unwrap();
        let proof = MarkerToken::new("00112233445566778899aabbccddeeff").unwrap();
        let marker = "foreign-staging-failure-marker";
        let source = format!(
            "TMPDIR=/dev/null\n{}",
            managed_foreign_bash_child_staging_source(Path::new("/bin/bash"), marker, &child_token,)
        );
        let transport =
            mez_agent::bash_private_handoff_source_input(&source, &token, marker, &proof);
        let input = format!(
            "{}{}printf '__MEZ_PARENT_AFTER_STAGING_FAILURE__\\n'\nexit\n",
            transport.wrapper, transport.receiver_payload
        );

        let output =
            run_managed_bash_receiver_exchange("foreign-staging-failure", input.as_bytes());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            output.status.success(),
            "stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            stdout.contains("__MEZ_PARENT_AFTER_STAGING_FAILURE__"),
            "{stdout:?}"
        );
        assert!(
            stdout.contains(&format!(
                "mez_event=parent-ready;mez_marker={marker};mez_outcome=source-failed;mez_status=70;mez_proof={}",
                proof.as_str()
            )),
            "{stdout:?}"
        );
    }

    /// Verifies an exact managed-Bash transaction publishes completion before
    /// restoring strict options inherited from the interactive parent.
    ///
    /// The evaluated transaction intentionally reports a failed child status.
    /// Receiver cleanup must still emit its authenticated completion, leave the
    /// parent responsive, and restore both `errexit` and `nounset` afterward.
    #[test]
    fn managed_bash_receiver_completes_transaction_before_restoring_strict_options() {
        if !Path::new("/bin/bash").exists() {
            return;
        }
        let token = MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap();
        let marker = MarkerToken::new("abcdef0123456789abcdef0123456789").unwrap();
        let input = mez_agent::ShellTransaction::new(
            marker.clone(),
            "turn-strict",
            "agent-%1",
            "%1",
            Path::new("/bin/bash"),
            "printf '__MEZ_STRICT_ACTION_RAN__\\n'; false",
        )
        .unwrap()
        .with_bash_receiver_token(token)
        .render_for_classification_input(mez_agent::ShellClassification::Bash);
        let exchange = format!(
            "set -eu\nprintf '__MEZ_STRICT_PARENT_READY__\\n'\n{}{}",
            input.wrapper, input.receiver_payload
        );
        let completion = format!(
            "mez_event=parent-ready;mez_marker={};mez_outcome=completed;mez_status=0",
            marker.as_str()
        );
        let start = format!("133;C;mez_marker={}", marker.as_str());
        let parent_input = b"printf '__MEZ_STRICT_PARENT_AFTER__:%s\\n' \"$-\"\nexit\n";

        let output = run_managed_bash_receiver_exchange_after_events(
            "strict-options-completion",
            exchange.as_bytes(),
            &[
                (&start, input.payload.as_bytes()),
                (&completion, parent_input.as_slice()),
            ],
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            output.status.success(),
            "stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            stdout.contains(&format!(
                "133;D;1;mez_marker={};mez_turn=turn-strict;mez_agent=agent-%1;mez_pane=%1",
                marker.as_str()
            )),
            "{stdout:?}"
        );
        assert!(stdout.contains(&completion), "{stdout:?}");
        let strict_flags = stdout
            .split("__MEZ_STRICT_PARENT_AFTER__:")
            .nth(1)
            .and_then(|suffix| suffix.lines().next())
            .unwrap_or_default();
        assert!(
            strict_flags.contains('e') && strict_flags.contains('u'),
            "{stdout:?}"
        );
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
