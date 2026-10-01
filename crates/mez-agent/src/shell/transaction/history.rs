//! Dialect-specific history isolation and terminal/option restoration source.
//!
//! Managed receivers establish authenticated private history before rendering.
//! Completion restores the parent's history and terminal state before emitting
//! its marker, and restores strict shell options last. These helpers only build
//! source; runtime owns admission, PTY writes and process lifetime.

use super::{MarkerToken, ShellClassification, fish_quote, shell_quote};

/// Starts a zsh-private history frame before any generated transport record.
///
/// A pane startup hook rejects this exact token-bearing record before zsh adds
/// it to immediate or shared history. The record itself then pushes the private
/// frame that owns all subsequent wrapper transport records.
pub(super) fn zsh_history_transport_start(
    classification: ShellClassification,
    token: Option<&MarkerToken>,
) -> String {
    if classification != ShellClassification::Zsh {
        return String::new();
    }
    let Some(token) = token else {
        return String::new();
    };
    format!("{}\n", zsh_history_control_record(token))
}

/// Restores outer history isolation when evaluated source skipped cleanup.
pub(super) fn posix_shell_history_transport_fallback(
    classification: ShellClassification,
    token: Option<&MarkerToken>,
) -> String {
    if classification != ShellClassification::Zsh {
        return "unset MEZ_WRAPPER_SOURCE".to_string();
    }
    let Some(token) = token else {
        return "unset MEZ_WRAPPER_SOURCE".to_string();
    };
    format!(
        "if [ \"${{MEZ_ZSH_HISTORY_ACTIVE-}}\" = {} ]; then unset MEZ_ZSH_HISTORY_ACTIVE; fc -P; fi; unset MEZ_WRAPPER_SOURCE",
        shell_quote(token.as_str())
    )
}

/// Renders the exact authenticated record accepted by the managed zsh hook.
///
/// Zsh calls `zshaddhistory` before executing an interactive record. The
/// startup compatibility layer rejects only this token-bearing record, which
/// then pushes the private history frame used by the remaining transport.
pub fn zsh_history_control_record(token: &MarkerToken) -> String {
    format!(
        "fc -p && MEZ_ZSH_HISTORY_ACTIVE={}; printf '\\036'",
        shell_quote(token.as_str())
    )
}

/// Returns a POSIX-compatible prologue that suppresses shell history and
/// preserves `errexit` before Mezzanine injects wrapper lines into a pane shell.
///
/// The first command is deliberately a single line: Bash-like shells add a line
/// to history before executing it, so the prologue disables history and deletes
/// that current history entry before later wrapper lines are read.
pub fn posix_shell_history_suppression_start() -> &'static str {
    "MEZ_SHELL_STTY_STATE=$(stty -g 2>/dev/null) || MEZ_SHELL_STTY_STATE=; if [ -n \"$MEZ_SHELL_STTY_STATE\" ]; then stty -echo 2>/dev/null || :; fi; MEZ_RESTORE_ERREXIT=0; case $- in *e*) MEZ_RESTORE_ERREXIT=1; set +e;; esac; MEZ_RESTORE_NOUNSET=0; case $- in *u*) MEZ_RESTORE_NOUNSET=1; set +u;; esac; MEZ_HISTORY_RESTORE=0; case \"$(set -o 2>/dev/null | command awk '$1==\"history\"{print $2; exit}')\" in on) MEZ_HISTORY_RESTORE=1; set +o history 2>/dev/null || :; history -d $((HISTCMD-1)) 2>/dev/null || :;; esac\n\
MEZ_HISTORY_HISTFILE_WAS_SET=0\n\
if [ \"${HISTFILE+x}\" = x ]; then MEZ_HISTORY_HISTFILE_WAS_SET=1; MEZ_HISTORY_HISTFILE_SAVED=$HISTFILE; fi\n\
HISTFILE=/dev/null\n"
}

/// Returns history setup for source evaluated inside an existing shell.
pub(super) fn posix_shell_history_suppression_start_for_classification(
    _classification: ShellClassification,
) -> String {
    posix_shell_history_suppression_start().to_string()
}

/// Returns POSIX-compatible cleanup that restores `HISTFILE`, shell history,
/// and `errexit` for non-transaction shell injections.
///
/// History and `errexit` are restored together on the final line so the cleanup
/// itself is read while history is still disabled and cannot become the next
/// persisted shell-history entry.
pub fn posix_shell_history_suppression_finish() -> &'static str {
    "if [ \"$MEZ_HISTORY_HISTFILE_WAS_SET\" = 1 ]; then HISTFILE=$MEZ_HISTORY_HISTFILE_SAVED; else unset HISTFILE; fi\n\
MEZ_RESTORE_HISTORY_NOW=$MEZ_HISTORY_RESTORE\n\
MEZ_RESTORE_ERREXIT_NOW=$MEZ_RESTORE_ERREXIT\n\
MEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET\n\
unset MEZ_HISTORY_RESTORE MEZ_HISTORY_HISTFILE_WAS_SET MEZ_HISTORY_HISTFILE_SAVED MEZ_RESTORE_ERREXIT MEZ_RESTORE_NOUNSET\n\
if [ -n \"$MEZ_SHELL_STTY_STATE\" ]; then stty \"$MEZ_SHELL_STTY_STATE\" 2>/dev/null || :; fi\n\
unset MEZ_SHELL_STTY_STATE\n\
if [ \"${MEZ_RESTORE_HISTORY_NOW:-0}\" = 1 ]; then set -o history 2>/dev/null || :; fi; MEZ_RESTORE_ERREXIT_APPLY=${MEZ_RESTORE_ERREXIT_NOW:-0}; MEZ_RESTORE_NOUNSET_APPLY=${MEZ_RESTORE_NOUNSET_NOW:-0}; unset MEZ_RESTORE_HISTORY_NOW MEZ_RESTORE_ERREXIT_NOW MEZ_RESTORE_NOUNSET_NOW; case \"$MEZ_RESTORE_ERREXIT_APPLY\" in 1) set -e;; esac; case \"$MEZ_RESTORE_NOUNSET_APPLY\" in 1) set -u;; esac; unset MEZ_RESTORE_ERREXIT_APPLY MEZ_RESTORE_NOUNSET_APPLY; :\n"
}

/// Returns the POSIX-compatible `HISTFILE` restore segment used before
/// transaction-local variable cleanup.
///
/// Shell transaction wrappers keep this segment separate because the OSC
/// transaction-end marker is emitted from the final option-restore line.
pub(super) fn posix_shell_history_file_restore() -> &'static str {
    "if [ \"$MEZ_HISTORY_HISTFILE_WAS_SET\" = 1 ]; then HISTFILE=$MEZ_HISTORY_HISTFILE_SAVED; else unset HISTFILE; fi\n"
}

/// Returns the POSIX-compatible final restoration prefix used immediately before
/// the transaction completion marker.
///
/// The returned string deliberately leaves the final shell line open. The caller
/// appends the OSC transaction-end marker on that same physical line, so the
/// runtime only observes transaction completion after Mezzanine has restored
/// history state. `errexit` restoration remains a suffix step so a restored
/// `set -e` cannot terminate the pane during marker emission or cleanup.
fn posix_shell_history_marker_finish_prefix() -> &'static str {
    "MEZ_RESTORE_HISTORY_NOW=$MEZ_HISTORY_RESTORE\n\
MEZ_RESTORE_ERREXIT_NOW=$MEZ_RESTORE_ERREXIT\n\
MEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET\n\
unset MEZ_HISTORY_RESTORE MEZ_HISTORY_HISTFILE_WAS_SET MEZ_HISTORY_HISTFILE_SAVED MEZ_RESTORE_ERREXIT MEZ_RESTORE_NOUNSET\n\
if [ -n \"$MEZ_SHELL_STTY_STATE\" ]; then stty \"$MEZ_SHELL_STTY_STATE\" 2>/dev/null || :; fi\n\
unset MEZ_SHELL_STTY_STATE\n\
if [ \"$MEZ_RESTORE_HISTORY_NOW\" = 1 ]; then set -o history 2>/dev/null || :; fi; "
}

/// Returns completion cleanup for POSIX-compatible transaction state.
pub(super) fn posix_shell_history_marker_finish_prefix_for_classification(
    _classification: ShellClassification,
) -> String {
    posix_shell_history_marker_finish_prefix().to_string()
}

/// Preserves strict POSIX shell options and terminal echo state.
///
/// Managed Bash and zsh transports establish their history boundary before
/// this source executes, so transaction-local source must not mutate history.
pub(super) fn posix_shell_state_suppression_start() -> &'static str {
    "MEZ_SHELL_STTY_STATE=$(stty -g 2>/dev/null) || MEZ_SHELL_STTY_STATE=; if [ -n \"$MEZ_SHELL_STTY_STATE\" ]; then stty -echo 2>/dev/null || :; fi; MEZ_RESTORE_ERREXIT=0; case $- in *e*) MEZ_RESTORE_ERREXIT=1; set +e;; esac; MEZ_RESTORE_NOUNSET=0; case $- in *u*) MEZ_RESTORE_NOUNSET=1; set +u;; esac\n"
}

/// Restores state-only transaction setup immediately before completion.
pub(super) fn posix_shell_state_marker_finish_prefix() -> &'static str {
    "MEZ_RESTORE_ERREXIT_NOW=$MEZ_RESTORE_ERREXIT\n\
MEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET\n\
unset MEZ_RESTORE_ERREXIT MEZ_RESTORE_NOUNSET\n\
if [ -n \"$MEZ_SHELL_STTY_STATE\" ]; then stty \"$MEZ_SHELL_STTY_STATE\" 2>/dev/null || :; fi\n\
unset MEZ_SHELL_STTY_STATE\n"
}

/// Returns the zsh-compatible transaction prologue.
pub(super) fn zsh_shell_history_suppression_start() -> &'static str {
    posix_shell_state_suppression_start()
}

/// Restores zsh's prior history context before the completion marker.
pub(super) fn zsh_shell_history_marker_finish_prefix(token: Option<&MarkerToken>) -> String {
    let token = token.unwrap_or_else(|| {
        panic!("zsh transaction rendering requires a pane-scoped history token")
    });
    format!(
        "MEZ_RESTORE_ERREXIT_NOW=$MEZ_RESTORE_ERREXIT\nMEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET\nunset MEZ_RESTORE_ERREXIT MEZ_RESTORE_NOUNSET\nif [ -n \"$MEZ_SHELL_STTY_STATE\" ]; then stty \"$MEZ_SHELL_STTY_STATE\" 2>/dev/null || :; fi\nunset MEZ_SHELL_STTY_STATE\nif [ \"${{MEZ_ZSH_HISTORY_ACTIVE-}}\" = {} ]; then unset MEZ_ZSH_HISTORY_ACTIVE; fc -P; fi; ",
        shell_quote(token.as_str())
    )
}

/// Returns POSIX-compatible suffix cleanup for restoring `errexit` after the
/// transaction completion marker has been emitted.
///
/// `errexit` is intentionally restored last. If the parent shell had `set -e`
/// enabled, restoring it before the marker or wrapper cleanup can make a minor
/// cleanup failure terminate the interactive pane immediately after an agent
/// command preview.
pub(super) fn posix_shell_errexit_restore_suffix() -> &'static str {
    "MEZ_RESTORE_ERREXIT_APPLY=${MEZ_RESTORE_ERREXIT_NOW:-0}; MEZ_RESTORE_NOUNSET_APPLY=${MEZ_RESTORE_NOUNSET_NOW:-0}; unset MEZ_RESTORE_HISTORY_NOW MEZ_RESTORE_ERREXIT_NOW MEZ_RESTORE_NOUNSET_NOW; case \"$MEZ_RESTORE_ERREXIT_APPLY\" in 1) set -e;; esac; case \"$MEZ_RESTORE_NOUNSET_APPLY\" in 1) set -u;; esac; unset MEZ_RESTORE_ERREXIT_APPLY MEZ_RESTORE_NOUNSET_APPLY; :"
}

/// Complete Fish input record that enters transaction-owned history isolation.
///
/// Fish records complete physical input lines. Keeping all setup that precedes
/// private mode on one stable line lets cleanup delete that exact owned record
/// without matching similarly prefixed user commands.
const FISH_HISTORY_ISOLATION_RECORD: &str = "set -l MEZ_SHELL_STTY_STATE (stty -g 2>/dev/null); or set -l MEZ_SHELL_STTY_STATE ''; if test -n \"$MEZ_SHELL_STTY_STATE\"; stty -echo 2>/dev/null; or true; end; set -l MEZ_FISH_PRIVATE_WAS_SET 0; set -l MEZ_FISH_PRIVATE_SAVED; if set -q fish_private_mode; set MEZ_FISH_PRIVATE_WAS_SET 1; set MEZ_FISH_PRIVATE_SAVED $fish_private_mode; end; set -g fish_private_mode 1";

/// Returns a Fish-native prologue that asks Fish to avoid writing Mez-injected
/// wrapper commands to the user's normal fish history.
pub(crate) fn fish_shell_history_suppression_start() -> String {
    format!("{FISH_HISTORY_ISOLATION_RECORD}\n")
}

/// Returns Fish-native cleanup that removes exact Mez wrapper records from
/// Fish history and restores the previous private-mode variable state.
pub(crate) fn fish_shell_history_restore() -> String {
    format!(
        "builtin history delete --exact --case-sensitive {isolation_record} >/dev/null 2>&1\n\
if test -n \"$MEZ_SHELL_STTY_STATE\"; stty \"$MEZ_SHELL_STTY_STATE\" 2>/dev/null; or true; end\n\
if test \"$MEZ_FISH_PRIVATE_WAS_SET\" = 1\n\
  set -g fish_private_mode $MEZ_FISH_PRIVATE_SAVED\n\
else\n\
  set -e fish_private_mode\n\
end\n\
set -e MEZ_SHELL_STTY_STATE MEZ_FISH_PRIVATE_WAS_SET MEZ_FISH_PRIVATE_SAVED\n",
        isolation_record = fish_quote(FISH_HISTORY_ISOLATION_RECORD),
    )
}
