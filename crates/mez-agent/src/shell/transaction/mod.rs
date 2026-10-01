//! Facade for typed shell transactions and deterministic dialect composition.
//!
//! One marker and staged-input contract connects validated child launches,
//! dialect renderers, canonical payload/materialization, private receivers,
//! history restoration and persistent subshell handoff. Child modules construct
//! source but never execute it: product adapters own authenticated admission,
//! approval, pane writes, process lifetime, timeouts and output observation.
//! Platform-specific physical line limits remain part of the shared contract.

mod authored_input;
pub use authored_input::{
    shell_command_contains_unquoted_heredoc, validate_agent_authored_shell_command,
};

mod payload;

mod renderers;

mod subshell;
pub use subshell::{
    agent_subshell_enter_command, agent_subshell_enter_command_with_shell_compatibility,
    agent_subshell_enter_command_with_shell_compatibility_and_exit_marker,
    agent_subshell_enter_command_with_zsh_history_token, agent_subshell_exit_marker_bytes,
    fish_agent_subshell_exit_input,
};

mod materialization;
use materialization::{
    CommandMaterialization, fish_command_file_materialization, posix_command_file_materialization,
};

mod bash_receiver;
use bash_receiver::bash_private_receiver_transport;
pub use bash_receiver::{
    bash_private_handoff_cancel_input, bash_private_handoff_source_input, bash_private_source_input,
};

mod private_source;
pub use private_source::{
    FishPrivateSourceInput, ZshPrivateSourceInput, fish_private_source_cancel_input,
    fish_private_source_input, zsh_private_source_cancel_input, zsh_private_source_input,
};

mod launch;
pub use launch::{
    SHELL_LAUNCH_MAX_ARTIFACT_BYTES, SHELL_LAUNCH_MAX_ARTIFACTS,
    SHELL_LAUNCH_MAX_TOTAL_ARTIFACT_BYTES, ShellChildArgument, ShellChildLaunch,
    ShellLaunchArtifact, ShellLaunchArtifactId,
};

mod history;
pub(crate) use history::{fish_shell_history_restore, fish_shell_history_suppression_start};
use history::{
    posix_shell_errexit_restore_suffix, posix_shell_history_file_restore,
    posix_shell_history_marker_finish_prefix_for_classification,
    posix_shell_history_suppression_start_for_classification,
    posix_shell_history_transport_fallback, posix_shell_state_marker_finish_prefix,
    posix_shell_state_suppression_start, zsh_history_transport_start,
    zsh_shell_history_marker_finish_prefix, zsh_shell_history_suppression_start,
};
pub use history::{posix_shell_history_suppression_finish, posix_shell_history_suppression_start};

use super::{AgentShellValidationError, AgentShellValidationResult, shell_quote};
use crate::{
    SHELL_OUTPUT_BASE64_BEGIN_MARKER, SHELL_OUTPUT_BASE64_DROPPED_BYTES_MARKER,
    SHELL_OUTPUT_BASE64_END_MARKER, SHELL_STATUS_BASE64_BEGIN_MARKER,
    SHELL_STATUS_BASE64_END_MARKER,
};
use base64::Engine;
use std::path::{Path, PathBuf};

use super::{validate_resolved_shell_path, validate_shell_marker_token};

// Shell transactions, quoting, tool discovery, environment signatures, and bootstrap.

/// Defines the DEFAULT TOOL DISCOVERY TIMEOUT MS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS: u64 = 10_000;
/// Defines the DEFAULT BOOTSTRAP TIMEOUT MS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_BOOTSTRAP_TIMEOUT_MS: u64 = 15_000;

/// Python fallback that emulates `setsid -w` from an interactive pane shell.
///
/// Job-control shells can start the interpreter as a process-group leader,
/// which makes a direct `setsid` call fail with `EPERM`. Forking only in that
/// state lets the child create a session while the foreground parent waits and
/// propagates the child's exit status.
const PYTHON_SETSID_WAIT_COMMAND: &str = "command python3 -c 'import os,sys;p=os.getpid()==os.getpgrp() and os.fork();p and sys.exit(os.waitstatus_to_exitcode(os.waitpid(p,0)[1]));os.setsid();os.execvp(sys.argv[1],sys.argv[1:])'";

/// Perl fallback with the same group-leader fork and wait behavior as Python.
const PERL_SETSID_WAIT_COMMAND: &str = "command perl -MPOSIX=setsid -e '$p=getpgrp()==$$&&fork();$p&&waitpid($p,0)&&exit($?&127?128+($?&127):$?>>8);setsid();exec @ARGV'";

/// Carries Shell Classification state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShellClassification {
    /// Represents the Bash case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Bash,
    /// Represents the Zsh case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Zsh,
    /// Represents the Fish case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Fish,
    /// Represents the Posix Sh case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PosixSh,
    /// Represents the Unknown Unix case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    UnknownUnix,
}

impl ShellClassification {
    /// Runs the classify operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn classify(shell_path: impl AsRef<Path>) -> Self {
        let file_stem = shell_path
            .as_ref()
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("");
        classify_by_name(file_stem)
    }

    /// Runs the as str operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn as_str(&self) -> &'static str {
        match self {
            ShellClassification::Bash => "bash",
            ShellClassification::Zsh => "zsh",
            ShellClassification::Fish => "fish",
            ShellClassification::PosixSh => "posix-sh",
            ShellClassification::UnknownUnix => "unknown-unix",
        }
    }
}

/// Runs the classify by name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn classify_by_name(file_stem: &str) -> ShellClassification {
    match file_stem {
        "bash" => ShellClassification::Bash,
        "zsh" => ShellClassification::Zsh,
        "fish" => ShellClassification::Fish,
        "sh" | "dash" | "ash" | "ksh" | "posix-sh" => ShellClassification::PosixSh,
        _ => ShellClassification::UnknownUnix,
    }
}

/// Carries Marker Token state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerToken(String);

impl MarkerToken {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(token: impl Into<String>) -> AgentShellValidationResult<Self> {
        let token = token.into();
        validate_shell_marker_token(&token)?;
        Ok(Self(token))
    }

    /// Runs the as str operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One fixed terminal sequence reserved for managed zsh private admission.
///
/// Runtime accepts only these identifiers from authenticated startup events,
/// preventing shell-provided bytes from becoming arbitrary pane input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedZshTrigger {
    /// Modified function-key sequence ending in the `m` key code.
    EscapeM,
    /// Modified function-key sequence ending in the `n` key code.
    EscapeN,
}

impl ManagedZshTrigger {
    /// Parses one protocol identifier emitted by the managed startup shim.
    pub fn from_protocol_str(value: &str) -> Option<Self> {
        match value {
            "escape-m" => Some(Self::EscapeM),
            "escape-n" => Some(Self::EscapeN),
            _ => None,
        }
    }

    /// Returns the bounded protocol identifier for this trigger.
    pub fn as_protocol_str(self) -> &'static str {
        match self {
            Self::EscapeM => "escape-m",
            Self::EscapeN => "escape-n",
        }
    }

    /// Returns the exact terminal bytes consumed by the managed zsh widget.
    pub fn input(self) -> &'static str {
        match self {
            Self::EscapeM => "\x1b[27;9;109~",
            Self::EscapeN => "\x1b[27;9;110~",
        }
    }
}

/// Immutable runtime-owned startup state for one managed zsh pane.
///
/// The descriptor keeps the child launch independent from mutable variables in
/// the already-running parent shell. In particular, the managed startup
/// directory is rendered directly into the handoff instead of being recovered
/// from `MEZ_ZSH_MANAGED_ZDOTDIR` after startup cleanup has unset it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedZshShell {
    /// Pane-scoped token authenticating private receiver events.
    token: MarkerToken,
    /// Owner-only directory containing the managed zsh startup files.
    startup_directory: PathBuf,
    /// Fixed trigger selected without replacing user key bindings.
    trigger: ManagedZshTrigger,
}

impl ManagedZshShell {
    /// Creates a managed-zsh launch descriptor from runtime-owned state.
    ///
    /// Returns an invalid-arguments error when the startup directory is not
    /// absolute because a relative `ZDOTDIR` would depend on mutable pane state.
    pub fn new(
        token: MarkerToken,
        startup_directory: impl Into<PathBuf>,
        trigger: ManagedZshTrigger,
    ) -> AgentShellValidationResult<Self> {
        let startup_directory = startup_directory.into();
        if !startup_directory.is_absolute() {
            return Err(AgentShellValidationError::invalid_args(
                "managed zsh startup directory must be absolute",
            ));
        }
        Ok(Self {
            token,
            startup_directory,
            trigger,
        })
    }

    /// Returns the pane-scoped private receiver token.
    pub fn token(&self) -> &MarkerToken {
        &self.token
    }

    /// Returns the runtime-owned startup directory used by managed children.
    pub fn startup_directory(&self) -> &Path {
        &self.startup_directory
    }

    /// Returns the fixed private trigger selected by managed startup.
    pub fn trigger(&self) -> ManagedZshTrigger {
        self.trigger
    }
}

/// Carries Shell Transaction state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellTransaction {
    /// Stores the marker value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub marker: MarkerToken,
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub turn_id: String,
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the shell path value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub shell_path: String,
    /// Stores the command value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub command: String,
    /// Optional Base64 records appended to the materialized command file as
    /// inert comments after the executable shell source.
    ///
    /// Large semantic-write bytes use this channel so they cross the pane PTY
    /// only once instead of being embedded in source that is encoded again.
    input_sidecar: Option<String>,
    /// Pane-scoped token authenticated by the managed zsh history hook.
    ///
    /// The token is only used when rendering for zsh. Other shell
    /// classifications retain their native history-suppression paths.
    zsh_history_token: Option<MarkerToken>,
    bash_receiver_token: Option<MarkerToken>,
    /// Optional typed process launch that receives the materialized command
    /// file as one argv element instead of executing it directly in a child
    /// shell.
    pub child_launch: Option<ShellChildLaunch>,
    /// Stores the output transport used by isolated child command execution.
    ///
    /// Stateful commands always remain raw because they intentionally execute
    /// in the active pane shell. Isolated action commands can encode output so
    /// terminal-control bytes stay inert until runtime result processing.
    pub output_transport: ShellTransactionOutputTransport,
    /// Maximum raw child-output bytes retained by the encoded transport.
    ///
    /// Ordinary actions use the global default. Internal protocols whose
    /// complete output is required for correctness may select a larger bounded
    /// limit before rendering the transaction wrapper.
    pub output_max_raw_bytes: usize,
    /// Whether the streamed payload receiver acknowledges each consumed record.
    ///
    /// Strict PTY pacing uses this opt-in contract to distinguish receiver
    /// progress from unrelated child output. Ordinary unpaced transactions
    /// leave it disabled.
    payload_receiver_acknowledgements: bool,
}

/// Rendered shell input for one non-stateful shell transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellTransactionInput {
    /// Admission trigger submitted to the interactive shell.
    pub wrapper: String,
    /// Authenticated Bash source frames sent only after receiver admission.
    ///
    /// Other shell classifications leave this stage empty because their
    /// existing private transport performs admission and source delivery in
    /// one shell-specific operation.
    pub receiver_payload: String,
    /// Base64 command payload consumed by the receiver after it starts.
    pub payload: String,
    /// Whether the rendered receiver emits one raw record-separator byte after
    /// every data record and after the authenticated sentinel.
    pub payload_receiver_acknowledgements: bool,
}

impl ShellTransactionInput {
    /// Returns the total byte length of all pane input for this transaction.
    pub fn len(&self) -> usize {
        self.wrapper
            .len()
            .saturating_add(self.receiver_payload.len())
            .saturating_add(self.payload.len())
    }

    /// Reports whether this rendered transaction contains no bytes.
    pub fn is_empty(&self) -> bool {
        self.wrapper.is_empty() && self.receiver_payload.is_empty() && self.payload.is_empty()
    }

    /// Combines wrapper and payload into one interactive-shell input string.
    pub fn combined(&self) -> String {
        let mut combined = String::with_capacity(self.len());
        combined.push_str(&self.wrapper);
        combined.push_str(&self.receiver_payload);
        combined.push_str(&self.payload);
        combined
    }
}

/// Environment overrides applied to isolated non-interactive agent command
/// shells.
///
/// Pane output still travels through a PTY, so many child programs would
/// otherwise assume they can launch a pager, editor, or terminal prompt. These
/// values are scoped to the child transaction shell and keep the parent pane
/// shell untouched.
const NONINTERACTIVE_AGENT_ENV: &[(&str, &str)] = &[
    ("TERM", "dumb"),
    ("PAGER", "cat"),
    ("MANPAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("SYSTEMD_PAGER", "cat"),
    ("BAT_PAGER", "cat"),
    ("DELTA_PAGER", "cat"),
    ("LESS", "FRX"),
    ("LESSSECURE", "1"),
    ("SYSTEMD_LESS", "FRXMK"),
    ("SYSTEMD_PAGERSECURE", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_EDITOR", "true"),
    ("GIT_SEQUENCE_EDITOR", "true"),
    ("EDITOR", "true"),
    ("VISUAL", "true"),
    ("DEBIAN_FRONTEND", "noninteractive"),
    ("APT_LISTCHANGES_FRONTEND", "none"),
];
/// Environment variables removed from Mezzanine-owned shell launches.
///
/// The rest of the pane environment remains inherited. These variables are
/// startup and prompt hook entry points that can run arbitrary commands before
/// or after an agent shell transaction reaches its marker.
const AGENT_SHELL_STARTUP_ENV_UNSETS: &[&str] = &[
    "BASH_ENV",
    "ENV",
    "ZDOTDIR",
    "PROMPT_COMMAND",
    "PS0",
    "PS1",
    "PS2",
    "PS3",
    "PS4",
    "PROMPT",
    "RPROMPT",
    "RPS1",
];
/// Prompt-related environment assignments for persistent agent shells.
///
/// These values keep a child agent shell prompt cheap and deterministic when
/// the parent pane exported prompt variables. Non-stateful action commands run
/// in further child shells and do not rely on these prompt values.
const AGENT_SUBSHELL_PROMPT_ENV: &[(&str, &str)] = &[
    ("PROMPT_COMMAND", ""),
    ("PS0", ""),
    ("PS1", "$ "),
    ("PS2", "> "),
    ("PS3", ""),
    ("PS4", "+ "),
    ("PROMPT", "$ "),
    ("RPROMPT", ""),
    ("RPS1", ""),
];

/// Maximum base64 payload bytes emitted on one generated shell-source line.
///
/// Shell transaction wrappers are delivered through a PTY, so command scripts
/// are materialized from short base64 chunks instead of heredocs. Keeping each
/// generated line modest avoids shell line-editor and transport edge cases on
/// remote panes. These payload lines are consumed by the wrapper's `read`
/// loop, after the interactive shell has relinquished its line editor.
pub const SHELL_TRANSACTION_COMMAND_BASE64_LINE_BYTES: usize = 768;
/// Maximum exact sidecar bytes protected by one logical acknowledgement.
///
/// One logical frame is still transported as canonical-safe physical lines.
/// The receiver validates its sequence, byte count, and SHA-256 digest before
/// acknowledging the frame, which preserves bounded flow control while
/// avoiding one stop-and-wait round trip per physical line.
pub const SHELL_TRANSACTION_SIDECAR_FRAME_BYTES: usize = 32 * 1024;
/// Maximum encoded Bash RX2 bytes protected by one logical acknowledgement.
///
/// Physical DATA records remain portable terminal lines. The receiver validates
/// one frame's sequence, encoded length, and SHA-256 digest before acknowledging
/// its FRAME_END record, avoiding one SSH round trip per physical DATA record.
pub const BASH_PRIVATE_SOURCE_FRAME_BYTES: usize = 32 * 1024;
/// Maximum raw source bytes accepted by one managed zsh private admission.
pub const ZSH_PRIVATE_SOURCE_MAX_BYTES: usize = 1024 * 1024;
/// Maximum base64 bytes representing one bounded managed zsh source.
pub const ZSH_PRIVATE_SOURCE_MAX_BASE64_BYTES: usize = ZSH_PRIVATE_SOURCE_MAX_BYTES.div_ceil(3) * 4;
/// Maximum encoded source bytes protected by one managed zsh logical frame.
///
/// Physical DATA records remain within the portable PTY line bound, while a
/// validated frame end supplies one bounded receiver acknowledgement.
pub const ZSH_PRIVATE_SOURCE_FRAME_BYTES: usize = 32 * 1024;
/// Maximum logical frames accepted by one managed zsh private admission.
pub const ZSH_PRIVATE_SOURCE_MAX_FRAMES: usize =
    ZSH_PRIVATE_SOURCE_MAX_BASE64_BYTES.div_ceil(ZSH_PRIVATE_SOURCE_FRAME_BYTES);
/// Maximum physical receiver-record bytes accepted by managed zsh.
pub const ZSH_PRIVATE_SOURCE_MAX_RECORD_BYTES: usize = 1024;
/// Maximum base64 bytes appended by one shell wrapper transport command.
///
/// Wrapper source is reconstructed through interactive shell input before the
/// ordinary command-payload receiver exists. A smaller bound leaves ample
/// room for assignment syntax in Darwin's constrained terminal input buffers.
#[cfg(target_os = "macos")]
pub(crate) const SHELL_WRAPPER_BASE64_LINE_BYTES: usize = 64;
#[cfg(not(target_os = "macos"))]
pub(crate) const SHELL_WRAPPER_BASE64_LINE_BYTES: usize = 640;
/// Maximum Base64 data bytes accepted in one managed zsh DATA record.
pub const ZSH_PRIVATE_SOURCE_DATA_MAX_BYTES: usize = SHELL_WRAPPER_BASE64_LINE_BYTES;
/// Maximum DATA records accepted by one managed zsh private admission.
pub const ZSH_PRIVATE_SOURCE_MAX_CHUNKS: usize =
    ZSH_PRIVATE_SOURCE_MAX_BASE64_BYTES.div_ceil(SHELL_WRAPPER_BASE64_LINE_BYTES);
/// Maximum raw output bytes emitted through one base64 shell-output transport.
pub const SHELL_OUTPUT_BASE64_MAX_RAW_BYTES: usize = 256 * 1024;
/// Output transport used by isolated shell transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellTransactionOutputTransport {
    /// Child command output is emitted unchanged.
    Raw,
    /// Child command output is emitted as printable base64.
    Base64,
}

/// Renders the isolated POSIX child-shell execution block.
///
/// # Parameters
/// - `transport`: Output transport selected for the child command.
/// - `child_env`: Shell words that apply non-interactive child environment.
/// - `shell_invocation`: Shell words that invoke the materialized command file.
fn posix_child_command_invocation_lines(
    transport: ShellTransactionOutputTransport,
    output_max_raw_bytes: usize,
    child_env: &str,
    shell_invocation: &str,
    status_fd: Option<u8>,
    inherited_terminal: bool,
) -> String {
    let mut lines = Vec::new();
    if status_fd.is_some() {
        lines.push("MEZ_STATUS_FILE=".to_string());
        lines.push(
            "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then MEZ_STATUS_FILE=$(mktemp) || MEZ_WRITE_STATUS=1; fi"
                .to_string(),
        );
    } else {
        lines.push("MEZ_STATUS_FILE=".to_string());
    }
    if transport == ShellTransactionOutputTransport::Base64 {
        lines.push("MEZ_OUTPUT_FILE=".to_string());
        lines.push("MEZ_OUTPUT_DROPPED=0".to_string());
        lines.push(
            "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then MEZ_OUTPUT_FILE=$(mktemp) || MEZ_WRITE_STATUS=1; fi"
                .to_string(),
        );
    } else {
        lines.push("MEZ_OUTPUT_FILE=".to_string());
    }
    if inherited_terminal {
        lines.extend([
            "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then".to_string(),
            posix_inherited_terminal_child_command_line(
                "    command",
                child_env,
                shell_invocation,
                transport,
                status_fd,
            ),
            "  MEZ_STATUS=$?".to_string(),
        ]);
    } else {
        lines.extend([
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then".to_string(),
        "  if command -v setsid >/dev/null 2>&1 && command setsid -w true >/dev/null 2>&1; then"
            .to_string(),
        posix_child_command_line(
            "    command setsid -w",
            child_env,
            shell_invocation,
            transport,
            status_fd,
        ),
        "  elif command -v python3 >/dev/null 2>&1; then".to_string(),
        posix_child_command_line(
            PYTHON_SETSID_WAIT_COMMAND,
            child_env,
            shell_invocation,
            transport,
            status_fd,
        ),
        "  elif command -v perl >/dev/null 2>&1; then".to_string(),
        posix_child_command_line(
            PERL_SETSID_WAIT_COMMAND,
            child_env,
            shell_invocation,
            transport,
            status_fd,
        ),
        "  else".to_string(),
        posix_child_command_line(
            "    command",
            child_env,
            shell_invocation,
            transport,
            status_fd,
        ),
        "  fi".to_string(),
        "  MEZ_STATUS=$?".to_string(),
        ]);
    }
    if transport == ShellTransactionOutputTransport::Base64 {
        lines.extend([
            format!(
                "  printf '\\n%s\\n' {}",
                shell_quote(SHELL_OUTPUT_BASE64_BEGIN_MARKER)
            ),
            "  if [ -n \"$MEZ_OUTPUT_FILE\" ]; then".to_string(),
            "    MEZ_OUTPUT_BYTES=$(wc -c < \"$MEZ_OUTPUT_FILE\" 2>/dev/null || printf 0)".to_string(),
            format!(
                "    if [ \"$MEZ_OUTPUT_BYTES\" -gt {} ] 2>/dev/null; then MEZ_OUTPUT_DROPPED=$((MEZ_OUTPUT_BYTES - {})); else MEZ_OUTPUT_DROPPED=0; fi",
                output_max_raw_bytes,
                output_max_raw_bytes
            ),
            format!(
                "    dd if=\"$MEZ_OUTPUT_FILE\" bs={} count=1 2>/dev/null | base64",
                output_max_raw_bytes
            ),
            "  fi".to_string(),
            format!(
                "  printf '%s\\n' {}",
                shell_quote(SHELL_OUTPUT_BASE64_END_MARKER)
            ),
            format!(
                "  if [ \"${{MEZ_OUTPUT_DROPPED:-0}}\" -gt 0 ] 2>/dev/null; then printf '%s %s\\n' {} \"$MEZ_OUTPUT_DROPPED\"; fi",
                shell_quote(SHELL_OUTPUT_BASE64_DROPPED_BYTES_MARKER)
            ),
        ]);
    }
    if status_fd.is_some() {
        lines.extend([
            format!(
                "  printf '\\n%s\\n' {}",
                shell_quote(SHELL_STATUS_BASE64_BEGIN_MARKER)
            ),
            "  if [ -n \"$MEZ_STATUS_FILE\" ]; then base64 < \"$MEZ_STATUS_FILE\"; fi".to_string(),
            format!(
                "  printf '%s\\n' {}",
                shell_quote(SHELL_STATUS_BASE64_END_MARKER)
            ),
        ]);
    }
    lines.extend([
        "else".to_string(),
        "  MEZ_STATUS=$MEZ_WRITE_STATUS".to_string(),
        "fi".to_string(),
    ]);
    lines.join("\n") + "\n"
}

/// Renders one POSIX child command line with optional output redirection.
///
/// # Parameters
/// - `prefix`: Already-indented command prefix.
/// - `child_env`: Shell words that apply non-interactive child environment.
/// - `shell_invocation`: Shell words that invoke the materialized command file.
/// - `transport`: Output transport selected for the child command.
fn posix_child_command_line(
    prefix: &str,
    child_env: &str,
    shell_invocation: &str,
    transport: ShellTransactionOutputTransport,
    status_fd: Option<u8>,
) -> String {
    let redirect = if transport == ShellTransactionOutputTransport::Base64 {
        " > \"$MEZ_OUTPUT_FILE\" 2>&1"
    } else {
        ""
    };
    let status_redirect = status_fd
        .map(|fd| format!(" {fd}>\"$MEZ_STATUS_FILE\""))
        .unwrap_or_default();
    format!("{prefix} {child_env} {shell_invocation} </dev/null{redirect}{status_redirect}")
}

/// Renders one typed child command that inherits the pane terminal.
fn posix_inherited_terminal_child_command_line(
    prefix: &str,
    child_env: &str,
    shell_invocation: &str,
    transport: ShellTransactionOutputTransport,
    status_fd: Option<u8>,
) -> String {
    let redirect = if transport == ShellTransactionOutputTransport::Base64 {
        " > \"$MEZ_OUTPUT_FILE\" 2>&1"
    } else {
        ""
    };
    let status_redirect = status_fd
        .map(|fd| format!(" {fd}>\"$MEZ_STATUS_FILE\""))
        .unwrap_or_default();
    let child_env = if child_env.is_empty() {
        String::new()
    } else {
        format!("{child_env} ")
    };
    format!("{prefix} {child_env}{shell_invocation}{redirect}{status_redirect}")
}

/// Renders the isolated Fish child-shell execution block.
///
/// # Parameters
/// - `transport`: Output transport selected for the child command.
/// - `noninteractive_env`: Fish words that apply child environment.
/// - `shell_invocation`: Fish words that invoke the materialized command file.
fn fish_child_command_invocation_lines(
    transport: ShellTransactionOutputTransport,
    output_max_raw_bytes: usize,
    noninteractive_env: &str,
    shell_invocation: &str,
    status_fd: Option<u8>,
    inherited_terminal: bool,
) -> String {
    let mut lines = Vec::new();
    if status_fd.is_some() {
        lines.push("set -l MEZ_STATUS_FILE ''".to_string());
        lines.push("if test \"$MEZ_WRITE_STATUS\" -eq 0".to_string());
        lines.push("set MEZ_STATUS_FILE (mktemp); or set MEZ_WRITE_STATUS 1".to_string());
        lines.push("end".to_string());
    } else {
        lines.push("set -l MEZ_STATUS_FILE ''".to_string());
    }
    if transport == ShellTransactionOutputTransport::Base64 {
        lines.push("set -l MEZ_OUTPUT_FILE ''".to_string());
        lines.push("set -l MEZ_OUTPUT_DROPPED 0".to_string());
        lines.push("if test \"$MEZ_WRITE_STATUS\" -eq 0".to_string());
        lines.push("set MEZ_OUTPUT_FILE (mktemp); or set MEZ_WRITE_STATUS 1".to_string());
        lines.push("end".to_string());
    } else {
        lines.push("set -l MEZ_OUTPUT_FILE ''".to_string());
    }
    lines.push("if test \"$MEZ_WRITE_STATUS\" -eq 0".to_string());
    if inherited_terminal {
        lines.push(fish_child_command_line(
            "    command",
            noninteractive_env,
            shell_invocation,
            transport,
            status_fd,
            true,
        ));
    } else {
        lines.extend([
            "if command -q setsid; and command setsid -w true >/dev/null 2>&1".to_string(),
            fish_child_command_line(
                "    command setsid -w env",
                noninteractive_env,
                shell_invocation,
                transport,
                status_fd,
                false,
            ),
            "else if command -q python3".to_string(),
            fish_child_command_line(
                &format!("    {PYTHON_SETSID_WAIT_COMMAND} env"),
                noninteractive_env,
                shell_invocation,
                transport,
                status_fd,
                false,
            ),
            "else if command -q perl".to_string(),
            fish_child_command_line(
                &format!("    {PERL_SETSID_WAIT_COMMAND} env"),
                noninteractive_env,
                shell_invocation,
                transport,
                status_fd,
                false,
            ),
            "else".to_string(),
            fish_child_command_line(
                "    command env",
                noninteractive_env,
                shell_invocation,
                transport,
                status_fd,
                false,
            ),
            "end".to_string(),
        ]);
    }
    lines.push("set MEZ_STATUS $status".to_string());
    if transport == ShellTransactionOutputTransport::Base64 {
        lines.extend([
            format!(
                "printf '\\n%s\\n' {}",
                fish_quote(SHELL_OUTPUT_BASE64_BEGIN_MARKER)
            ),
            "if test -n \"$MEZ_OUTPUT_FILE\"".to_string(),
            "set -l MEZ_OUTPUT_BYTES (wc -c < \"$MEZ_OUTPUT_FILE\" 2>/dev/null); or set MEZ_OUTPUT_BYTES 0".to_string(),
            format!(
                "if test \"$MEZ_OUTPUT_BYTES\" -gt {} 2>/dev/null",
                output_max_raw_bytes
            ),
            format!(
                "set MEZ_OUTPUT_DROPPED (math \"$MEZ_OUTPUT_BYTES - {}\")",
                output_max_raw_bytes
            ),
            "else".to_string(),
            "set MEZ_OUTPUT_DROPPED 0".to_string(),
            "end".to_string(),
            format!(
                "command dd if=\"$MEZ_OUTPUT_FILE\" bs={} count=1 2>/dev/null | base64",
                output_max_raw_bytes
            ),
            "end".to_string(),
            format!(
                "printf '%s\\n' {}",
                fish_quote(SHELL_OUTPUT_BASE64_END_MARKER)
            ),
            format!(
                "if test \"$MEZ_OUTPUT_DROPPED\" -gt 0 2>/dev/null; printf '%s %s\\n' {} \"$MEZ_OUTPUT_DROPPED\"; end",
                fish_quote(SHELL_OUTPUT_BASE64_DROPPED_BYTES_MARKER)
            ),
        ]);
    }
    if status_fd.is_some() {
        lines.extend([
            format!(
                "printf '\\n%s\\n' {}",
                fish_quote(SHELL_STATUS_BASE64_BEGIN_MARKER)
            ),
            "if test -n \"$MEZ_STATUS_FILE\"; base64 < \"$MEZ_STATUS_FILE\"; end".to_string(),
            format!(
                "printf '%s\\n' {}",
                fish_quote(SHELL_STATUS_BASE64_END_MARKER)
            ),
        ]);
    }
    lines.extend([
        "else".to_string(),
        "set MEZ_STATUS $MEZ_WRITE_STATUS".to_string(),
        "end".to_string(),
    ]);
    lines.join("\n") + "\n"
}

/// Renders one Fish child command line with optional output redirection.
///
/// # Parameters
/// - `prefix`: Already-indented command prefix.
/// - `noninteractive_env`: Fish words that apply child environment.
/// - `shell_invocation`: Fish words that invoke the materialized command file.
/// - `transport`: Output transport selected for the child command.
fn fish_child_command_line(
    prefix: &str,
    noninteractive_env: &str,
    shell_invocation: &str,
    transport: ShellTransactionOutputTransport,
    status_fd: Option<u8>,
    inherited_terminal: bool,
) -> String {
    let redirect = if transport == ShellTransactionOutputTransport::Base64 {
        " > \"$MEZ_OUTPUT_FILE\" 2>&1"
    } else {
        ""
    };
    let status_redirect = status_fd
        .map(|fd| format!(" {fd}>\"$MEZ_STATUS_FILE\""))
        .unwrap_or_default();
    let noninteractive_env = if noninteractive_env.is_empty() {
        String::new()
    } else {
        format!("{noninteractive_env} ")
    };
    let input_redirect = if inherited_terminal {
        ""
    } else {
        " </dev/null"
    };
    format!(
        "{prefix} {noninteractive_env}{shell_invocation}{input_redirect}{redirect}{status_redirect}"
    )
}

/// Renders one typed child launch as POSIX shell words.
fn posix_typed_child_launch_words(launch: &ShellChildLaunch) -> String {
    std::iter::once(shell_quote(&launch.executable))
        .chain(launch.arguments.iter().map(|argument| match argument {
            ShellChildArgument::Literal(value) => posix_shell_quoted_argument(value),
            ShellChildArgument::MaterializedCommandFile => "\"$MEZ_COMMAND_FILE\"".to_string(),
            ShellChildArgument::MaterializedArtifact(artifact) => {
                format!(
                    "\"$MEZ_ARTIFACT_DIR/{}\"",
                    launch_artifact_index(launch, artifact)
                )
            }
            ShellChildArgument::MaterializedPathBinding { name, artifact } => format!(
                "\"{name}=$MEZ_ARTIFACT_DIR/{}\"",
                launch_artifact_index(launch, artifact)
            ),
        }))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Returns the validated positional index for one referenced launch artifact.
fn launch_artifact_index(launch: &ShellChildLaunch, artifact: &ShellLaunchArtifactId) -> usize {
    launch
        .artifacts
        .iter()
        .position(|candidate| &candidate.id == artifact)
        .expect("typed launch validation guarantees every artifact reference exists")
}

/// Renders one POSIX argument without creating an oversized physical source line.
///
/// The transaction wrapper travels through a PTY before the shell reads it. Long
/// forwarded environment values must therefore remain below conservative line
/// discipline limits. Adjacent quoted words preserve one argument, while the
/// escaped newline separates the generated source into bounded physical lines.
fn posix_shell_quoted_argument(value: &str) -> String {
    const MAX_QUOTED_ARGUMENT_LINE_BYTES: usize = 512;

    if shell_quote(value).len() <= MAX_QUOTED_ARGUMENT_LINE_BYTES {
        return shell_quote(value);
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        current.push(character);
        if shell_quote(&current).len() > MAX_QUOTED_ARGUMENT_LINE_BYTES {
            let split_at = current.len() - character.len_utf8();
            let remainder = current.split_off(split_at);
            chunks.push(shell_quote(&current));
            current = remainder;
        }
    }
    if !current.is_empty() {
        chunks.push(shell_quote(&current));
    }
    chunks.join("\\\n")
}

/// Renders one Fish argument without creating an oversized physical source line.
///
/// Adjacent quoted fragments remain one Fish word across escaped newlines, so
/// chunking preserves the argv element without evaluating any literal content.
fn fish_shell_quoted_argument(value: &str) -> String {
    const MAX_QUOTED_ARGUMENT_LINE_BYTES: usize = 512;

    if fish_quote(value).len() <= MAX_QUOTED_ARGUMENT_LINE_BYTES {
        return fish_quote(value);
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        current.push(character);
        if fish_quote(&current).len() >= MAX_QUOTED_ARGUMENT_LINE_BYTES {
            let remainder = current.pop().map(|character| character.to_string());
            chunks.push(fish_quote(&current));
            current = remainder.unwrap_or_default();
        }
    }
    if !current.is_empty() {
        chunks.push(fish_quote(&current));
    }
    chunks.join("\\\n")
}

/// Renders one typed child launch as Fish shell words.
pub(super) fn fish_typed_child_launch_words(launch: &ShellChildLaunch) -> String {
    std::iter::once(fish_quote(&launch.executable))
        .chain(launch.arguments.iter().map(|argument| match argument {
            ShellChildArgument::Literal(value) => fish_shell_quoted_argument(value),
            ShellChildArgument::MaterializedCommandFile => "\"$MEZ_COMMAND_FILE\"".to_string(),
            ShellChildArgument::MaterializedArtifact(artifact) => {
                format!(
                    "\"$MEZ_ARTIFACT_DIR/{}\"",
                    launch_artifact_index(launch, artifact)
                )
            }
            ShellChildArgument::MaterializedPathBinding { name, artifact } => format!(
                "\"{name}=$MEZ_ARTIFACT_DIR/{}\"",
                launch_artifact_index(launch, artifact)
            ),
        }))
        .collect::<Vec<_>>()
        .join(" \\\n")
}

impl ShellTransaction {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(
        marker: MarkerToken,
        turn_id: impl Into<String>,
        agent_id: impl Into<String>,
        pane_id: impl Into<String>,
        shell_path: &Path,
        command: impl Into<String>,
    ) -> AgentShellValidationResult<Self> {
        validate_resolved_shell_path(shell_path)?;
        Ok(Self {
            marker,
            turn_id: turn_id.into(),
            agent_id: agent_id.into(),
            pane_id: pane_id.into(),
            shell_path: shell_path.to_string_lossy().into_owned(),
            command: command.into(),
            input_sidecar: None,
            zsh_history_token: None,
            bash_receiver_token: None,
            child_launch: None,
            output_transport: ShellTransactionOutputTransport::Raw,
            output_max_raw_bytes: SHELL_OUTPUT_BASE64_MAX_RAW_BYTES,
            payload_receiver_acknowledgements: false,
        })
    }

    /// Selects a validated typed child process launch for this transaction.
    pub fn with_child_launch(mut self, child_launch: ShellChildLaunch) -> Self {
        self.child_launch = Some(child_launch);
        self
    }

    /// Selects separately streamed Base64 records for the materialized script.
    pub fn with_input_sidecar(mut self, input_sidecar: Option<String>) -> Self {
        self.input_sidecar = input_sidecar;
        self
    }

    /// Returns the separately streamed Base64 records for the materialized
    /// script, when one was selected.
    pub fn input_sidecar(&self) -> Option<&str> {
        self.input_sidecar.as_deref()
    }

    /// Selects the pane-scoped token used by managed zsh history isolation.
    ///
    /// The pane startup compatibility hook recognizes only the exact control
    /// record carrying this token. That record pushes a private zsh history
    /// context before any transaction transport records are submitted.
    pub fn with_zsh_history_token(mut self, token: MarkerToken) -> Self {
        self.zsh_history_token = Some(token);
        self
    }

    /// Selects the pane-scoped private Bash receiver for generated transport.
    pub fn with_bash_receiver_token(mut self, token: MarkerToken) -> Self {
        self.bash_receiver_token = Some(token);
        self
    }

    /// Selects the output transport for isolated shell rendering.
    ///
    /// # Parameters
    /// - `output_transport`: Transport mode used when rendering non-stateful
    ///   command wrappers.
    pub fn with_output_transport(
        mut self,
        output_transport: ShellTransactionOutputTransport,
    ) -> Self {
        self.output_transport = output_transport;
        self
    }

    /// Selects the bounded raw-output limit used by encoded shell transport.
    ///
    /// A zero value is promoted to one byte so generated `dd` commands remain
    /// valid and every transaction preserves a deterministic finite bound.
    pub fn with_output_max_raw_bytes(mut self, output_max_raw_bytes: usize) -> Self {
        self.output_max_raw_bytes = output_max_raw_bytes.max(1);
        self
    }

    /// Selects receiver acknowledgements for strict streamed-payload pacing.
    ///
    /// When enabled, the shell receiver emits one raw `0x1e` byte after each
    /// consumed base64 record and after the sentinel. The rendered input
    /// advertises this capability so runtime delivery layers do not infer it.
    pub fn with_payload_receiver_acknowledgements(mut self, enabled: bool) -> Self {
        self.payload_receiver_acknowledgements = enabled;
        self
    }
}

/// Builds a shell-safe function name for one transaction wrapper.
///
/// # Parameters
/// - `marker`: The transaction marker token used to distinguish OSC events.
fn transaction_function_name(marker: &str) -> String {
    let suffix = marker
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(16)
        .collect::<String>();
    if suffix.is_empty() {
        "__mez_tx".to_string()
    } else {
        format!("__mez_tx_{suffix}")
    }
}

/// Returns shell flags that skip common startup files for one classification.
fn startup_suppression_args(classification: ShellClassification) -> &'static [&'static str] {
    match classification {
        ShellClassification::Bash => &["--noprofile", "--norc"],
        ShellClassification::Zsh => &["-f"],
        ShellClassification::Fish => &["--no-config"],
        ShellClassification::PosixSh | ShellClassification::UnknownUnix => &[],
    }
}

/// Renders a POSIX-shell command word sequence that invokes a script file
/// through a startup-suppressed child shell.
///
/// # Parameters
/// - `shell_path`: Absolute resolved shell path.
/// - `classification`: Shell classification used to choose safe startup flags.
/// - `script_word`: Already-rendered shell word for the script path.
fn posix_shell_script_invocation_words(
    shell_path: &str,
    classification: ShellClassification,
    script_word: &str,
) -> String {
    let mut words = vec![shell_quote(shell_path)];
    words.extend(
        startup_suppression_args(classification)
            .iter()
            .map(|arg| (*arg).to_string()),
    );
    words.push(script_word.to_string());
    words.join(" ")
}

/// Renders a Fish command word sequence that invokes a script file through a
/// startup-suppressed child shell.
///
/// # Parameters
/// - `shell_path`: Absolute resolved shell path.
/// - `classification`: Shell classification used to choose safe startup flags.
/// - `script_word`: Already-rendered Fish word for the script path.
fn fish_shell_script_invocation_words(
    shell_path: &str,
    classification: ShellClassification,
    script_word: &str,
) -> String {
    let mut words = vec![fish_quote(shell_path)];
    words.extend(
        startup_suppression_args(classification)
            .iter()
            .map(|arg| (*arg).to_string()),
    );
    words.push(script_word.to_string());
    words.join(" ")
}

/// Renders a POSIX-shell command word sequence that starts the persistent
/// agent-mode child shell without user startup files.
///
/// # Parameters
/// - `shell_path`: Absolute resolved shell path.
/// - `classification`: Shell classification used to choose safe startup flags.
fn posix_shell_interactive_invocation_words(
    shell_path: &str,
    classification: ShellClassification,
) -> String {
    posix_shell_interactive_invocation_words_with_startup_suppression(
        shell_path,
        classification,
        true,
    )
}

/// Renders a persistent shell invocation with an optional managed Bash rcfile.
fn posix_shell_interactive_invocation_words_with_bash_receiver(
    shell_path: &str,
    classification: ShellClassification,
    bash_receiver_rcfile: Option<&Path>,
    bash_receiver_install_marker: Option<&str>,
) -> String {
    if classification != ShellClassification::Bash {
        return posix_shell_interactive_invocation_words(shell_path, classification);
    }
    let Some(rcfile) = bash_receiver_rcfile else {
        return posix_shell_interactive_invocation_words(shell_path, classification);
    };
    let shell = shell_quote(shell_path);
    let rcfile = shell_quote(&rcfile.to_string_lossy());
    let install_marker = shell_quote(bash_receiver_install_marker.unwrap_or_default());
    format!(
        "MEZ_BASH_RECEIVER_INSTALL_MARKER={install_marker} {shell} --noprofile --rcfile {rcfile} -i"
    )
}

/// Renders a persistent POSIX-shell child invocation with optional startup
/// suppression. Managed zsh children retain their pane-scoped startup shim so
/// ordinary user commands keep the user's history configuration.
fn posix_shell_interactive_invocation_words_with_startup_suppression(
    shell_path: &str,
    classification: ShellClassification,
    suppress_startup: bool,
) -> String {
    let mut words = vec![shell_quote(shell_path)];
    let startup_args = if suppress_startup {
        startup_suppression_args(classification)
    } else {
        &[]
    };
    words.extend(startup_args.iter().map(|arg| (*arg).to_string()));
    let mut exec_words = vec!["exec".to_string(), shell_quote(shell_path)];
    exec_words.extend(startup_args.iter().map(|arg| (*arg).to_string()));
    exec_words.push("-i".to_string());
    let readiness_source = format!(
        "command printf '\\033]133;B\\033\\\\'; {}",
        exec_words.join(" ")
    );
    words.push("-c".to_string());
    words.push(shell_quote(&readiness_source));
    words.join(" ")
}

/// Formats persistent-shell environment words while retaining managed zsh
/// startup state for a token-authenticated agent child.
fn posix_agent_subshell_env_word_list_for_classification(
    classification: ShellClassification,
) -> Vec<String> {
    let mut words = AGENT_SHELL_STARTUP_ENV_UNSETS
        .iter()
        .filter(|key| classification != ShellClassification::Zsh || **key != "ZDOTDIR")
        .map(|key| format!("-u {key}"))
        .collect::<Vec<_>>();
    if classification == ShellClassification::Zsh {
        words.push("ZDOTDIR=\"$MEZ_ZSH_MANAGED_ZDOTDIR\"".to_string());
        words.push("MEZ_ZSH_PRESERVE_STARTUP_CONTEXT=1".to_string());
        words
            .push("MEZ_ZSH_ORIGINAL_ZDOTDIR_WAS_SET=\"$MEZ_ZSH_USER_ZDOTDIR_WAS_SET\"".to_string());
        words.push("MEZ_ZSH_ORIGINAL_ZDOTDIR=\"$MEZ_ZSH_USER_ZDOTDIR\"".to_string());
    }
    words.extend(
        AGENT_SUBSHELL_PROMPT_ENV
            .iter()
            .map(|(key, value)| format!("{key}={}", shell_quote(value))),
    );
    words
}

/// Formats environment words for a managed zsh child from immutable runtime state.
fn managed_zsh_agent_subshell_env_word_list(managed_zsh: &ManagedZshShell) -> Vec<String> {
    let mut words = AGENT_SHELL_STARTUP_ENV_UNSETS
        .iter()
        .filter(|key| **key != "ZDOTDIR")
        .map(|key| format!("-u {key}"))
        .collect::<Vec<_>>();
    words.push(format!(
        "ZDOTDIR={}",
        shell_quote(&managed_zsh.startup_directory().to_string_lossy())
    ));
    words.push("MEZ_ZSH_PRESERVE_STARTUP_CONTEXT=1".to_string());
    words.push("MEZ_ZSH_ORIGINAL_ZDOTDIR_WAS_SET=\"$MEZ_ZSH_USER_ZDOTDIR_WAS_SET\"".to_string());
    words.push("MEZ_ZSH_ORIGINAL_ZDOTDIR=\"$MEZ_ZSH_USER_ZDOTDIR\"".to_string());
    words.extend(
        AGENT_SUBSHELL_PROMPT_ENV
            .iter()
            .map(|(key, value)| format!("{key}={}", shell_quote(value))),
    );
    words
}

/// Renders one direct interactive managed zsh child invocation.
///
/// The parent pane already owns the login environment. Replaying `.zprofile`
/// and `.zlogin` for every persistent child is both wasteful and observably
/// different from an ordinary interactive subshell. Global startup files are
/// also suppressed because they can present interactive prompts before the
/// managed receiver installs, while the managed `ZDOTDIR` chain remains active.
fn managed_zsh_interactive_invocation_words(shell_path: &str) -> String {
    format!("{} -d -i", shell_quote(shell_path))
}

/// Renders a Fish command word sequence that starts the persistent agent-mode
/// child shell without user startup files.
///
/// # Parameters
/// - `shell_path`: Absolute resolved shell path.
/// - `classification`: Shell classification used to choose safe startup flags.
fn fish_shell_interactive_invocation_words(
    shell_path: &str,
    classification: ShellClassification,
    receiver_install: Option<(&MarkerToken, &str)>,
) -> String {
    let mut words = vec![fish_quote(shell_path)];
    let startup_args = startup_suppression_args(classification);
    words.extend(startup_args.iter().map(|arg| (*arg).to_string()));
    let mut exec_words = vec!["exec".to_string(), fish_quote(shell_path)];
    exec_words.extend(startup_args.iter().map(|arg| (*arg).to_string()));
    exec_words.push("--init-command".to_string());
    let mut init_command = fish_wrapper_receiver_init_command().to_string();
    if let Some((token, marker)) = receiver_install {
        init_command.push_str(&format!(
            "\nfunction __mez_agent_child_exit\n    commandline --replace 'exit'\n    commandline -f execute\nend\nbind -M default \\cx __mez_agent_child_exit\nbind -M insert \\cx __mez_agent_child_exit\nbind -M visual \\cx __mez_agent_child_exit\nbind -M replace_one \\cx __mez_agent_child_exit\nfunction fish_prompt\n    bind -M default \\cx __mez_agent_child_exit\n    bind -M insert \\cx __mez_agent_child_exit\n    bind -M visual \\cx __mez_agent_child_exit\n    bind -M replace_one \\cx __mez_agent_child_exit\n    fish_default_prompt\n    if not set -q __MEZ_AGENT_CHILD_INSTALLED\n        set -g __MEZ_AGENT_CHILD_INSTALLED 1\n        builtin printf '\\e]133;R;mez_protocol=2;mez_shell=fish;mez_token=%s;mez_event=child-installed;mez_marker=%s\\e\\\\' {} {}\n    else\n        builtin printf '\\e]133;B\\e\\\\'\n        builtin printf '\\e]133;R;mez_protocol=2;mez_shell=fish;mez_token=%s;mez_event=child-prompt-ready;mez_marker=%s\\e\\\\' {} {}\n    end\nend",
            fish_quote(token.as_str()),
            fish_quote(marker),
            fish_quote(token.as_str()),
            fish_quote(marker)
        ));
    }
    exec_words.push(fish_quote(&init_command));
    exec_words.push("-i".to_string());
    let readiness_source = format!(
        "command printf '\\e]133;B\\e\\\\'; {}",
        exec_words.join(" ")
    );
    words.push("-c".to_string());
    words.push(fish_quote(&readiness_source));
    words.join(" ")
}

/// Fish function installed before interactive transaction delivery begins.
///
/// The interactive reader sees only one short function invocation. The
/// function delegates stdin ownership to a non-interactive POSIX reader while
/// it receives bounded base64 records. This avoids entering Fish's line editor
/// and its terminal-query handshake between records. The reader writes the
/// decoded wrapper to a temporary file, acknowledges each consumed record for
/// paced Darwin PTY delivery, and Fish sources the file without command
/// substitution so physical newlines remain intact.
pub fn fish_wrapper_receiver_init_command() -> &'static str {
    r#"function __mez_agent_wrapper_receive --argument-names sentinel
    set -l source_file (command mktemp); or return 1
    set -l encoded_file "$source_file.b64"
    set -l receiver_stty (command stty -g 2>/dev/null); or set receiver_stty ''
    if test -n "$receiver_stty"
        command stty -echo 2>/dev/null; or true
    end
    set -l receive_status 0
    command printf '' > "$encoded_file"; or set receive_status $status
    builtin history delete --exact --case-sensitive "__mez_agent_wrapper_receive '$sentinel'" >/dev/null 2>&1
    builtin printf '\036'
    if test "$receive_status" -eq 0
        command /bin/sh -c 'sentinel=$1; encoded_file=$2; while IFS= read -r record; do payload=${record%%;*}; if [ "$payload" = "$sentinel" ]; then printf "\036"; exit 0; fi; printf %s "$payload" >>"$encoded_file" || exit 1; printf "\036"; done; exit 1' sh "$sentinel" "$encoded_file"
        set receive_status $status
    end
    set -l decode_status 1
    if test "$receive_status" -eq 0
        if command base64 -d < "$encoded_file" > "$source_file" 2>/dev/null
            set decode_status 0
        else
            command base64 -D < "$encoded_file" > "$source_file"
            set decode_status $status
        end
    end
    if test -n "$receiver_stty"
        command stty "$receiver_stty" 2>/dev/null; or true
    end
    set -l source_status $decode_status
    if test "$decode_status" -eq 0
        source "$source_file"
        set source_status $status
    end
    command rm -f -- "$source_file" "$encoded_file" >/dev/null 2>&1; or true
    return $source_status
end"#
}

/// Syntax-neutral rendezvous command and deferred source for one foreign loader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignShellLoaderInput {
    /// One bounded `/bin/sh` command that takes ownership of terminal input.
    pub command: String,
    /// Base64 records released only after the correlated loader-ready event.
    pub payload: String,
}

/// Renders the fixed syntax-neutral rendezvous command for one foreign loader.
///
/// Runtime may queue this command immediately behind its identity probe because
/// it contains no generated child source or shell-specific assumptions. The
/// loader publishes its nonce before accepting payload records, preserving the
/// admission boundary while avoiding a host round trip between identity
/// discovery and loader startup.
pub fn dependency_free_foreign_shell_loader_command(
    marker: &str,
) -> AgentShellValidationResult<String> {
    validate_shell_marker_token(marker)?;
    let record_acknowledgement = if cfg!(target_os = "macos") {
        "printf '\\036';"
    } else {
        ""
    };
    let loader_source = format!(
        "umask 077;p=${{TMPDIR:-/tmp}}/.mez-$1;mkdir -m 700 \"$p\"||exit 70;trap 'r=$?;rm -rf \"$p\";exit $r' 0;f=$p/p;printf '\\033]133;R;mez_foreign_loader=ready;mez_marker=%s\\033\\\\' \"$1\";z=;while IFS= read -r x;do if [ \"$x\" = \"MEZ_LOADER_END_$1\" ];then z=1;printf '\\036';break;fi;printf %s \"$x\">>\"$f\"||exit 71;{record_acknowledgement}done;[ \"$z\" ]||exit 72;q=-d;printf ''|base64 -d>/dev/null 2>&1||q=-D;base64 \"$q\"<\"$f\">\"$p/e\"||exit 73;chmod 600 \"$p/e\"||exit 74;MEZ_FOREIGN_LOADER_DIR=$p /bin/sh \"$p/e\";r=$?;printf '\\033]133;R;mez_foreign_loader=exited;mez_marker=%s;mez_status=%s\\033\\\\' \"$1\" \"$r\";exit \"$r\""
    );
    Ok(format!(
        "/bin/sh -c {} sh {}\n",
        shell_quote(&loader_source),
        shell_quote(marker)
    ))
}

/// Renders a dependency-free loader for generated source inside a foreign shell.
///
/// The syntax-neutral `/bin/sh` command publishes a marker-correlated ready
/// event before reading any payload. Runtime withholds the bounded base64
/// records until that event, so an interactive parent editor cannot consume
/// staged source as typeahead. The loader materializes a shell-specific script
/// in an owner-only temporary directory, executes it with startup files
/// suppressed, publishes its final status, and removes every loader artifact
/// after the synchronous managed child returns.
pub fn dependency_free_foreign_shell_loader_input(
    source: &str,
    shell_path: &Path,
    classification: ShellClassification,
    child_token: Option<&MarkerToken>,
    marker: &str,
) -> AgentShellValidationResult<ForeignShellLoaderInput> {
    validate_resolved_shell_path(shell_path)?;
    validate_shell_marker_token(marker)?;
    if matches!(
        classification,
        ShellClassification::Bash | ShellClassification::Fish | ShellClassification::Zsh
    ) && child_token.is_none()
    {
        return Err(AgentShellValidationError::invalid_args(
            "managed foreign child loader requires a child token",
        ));
    }

    let shell = shell_quote(&shell_path.to_string_lossy());
    let invocation = match classification {
        ShellClassification::Bash => format!("{shell} --noprofile --norc \"$MEZ_FOREIGN_STAGE\""),
        ShellClassification::Fish => format!("{shell} --no-config \"$MEZ_FOREIGN_STAGE\""),
        ShellClassification::Zsh => format!("{shell} -f \"$MEZ_FOREIGN_STAGE\""),
        ShellClassification::PosixSh | ShellClassification::UnknownUnix => {
            format!("{shell} \"$MEZ_FOREIGN_STAGE\"")
        }
    };
    let encoded_source = base64::engine::general_purpose::STANDARD.encode(source.as_bytes());
    let entry_source = format!(
        "umask 077\n\
MEZ_FOREIGN_STAGE=$MEZ_FOREIGN_LOADER_DIR/stage\n\
MEZ_FOREIGN_BASE64_FLAG=-d\n\
printf '' | base64 -d >/dev/null 2>&1 || MEZ_FOREIGN_BASE64_FLAG=-D\n\
printf '%s' {} | base64 \"$MEZ_FOREIGN_BASE64_FLAG\" >\"$MEZ_FOREIGN_STAGE\" || exit 71\n\
chmod 600 \"$MEZ_FOREIGN_STAGE\" || exit 72\n\
{invocation}\n\
MEZ_FOREIGN_STATUS=$?\n\
exit \"$MEZ_FOREIGN_STATUS\"\n",
        shell_quote(&encoded_source),
    );
    let encoded_entry = base64::engine::general_purpose::STANDARD.encode(entry_source.as_bytes());
    let nonce = marker;
    let command = dependency_free_foreign_shell_loader_command(nonce)?;
    let mut payload = String::new();
    for chunk in encoded_entry
        .as_bytes()
        .chunks(SHELL_WRAPPER_BASE64_LINE_BYTES)
    {
        payload.push_str(
            std::str::from_utf8(chunk)
                .expect("standard base64 output should always be valid UTF-8"),
        );
        payload.push('\n');
    }
    payload.push_str(&format!("MEZ_LOADER_END_{nonce}\n"));
    Ok(ForeignShellLoaderInput { command, payload })
}

/// Encodes a generated Fish wrapper as receiver-consumed base64 records.
fn fish_shell_wrapper_transport(source: &str, marker: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(source.as_bytes());
    let end_marker = format!("__MEZ_WRAPPER_SOURCE_END_{marker}__");
    let mut transport = format!("__mez_agent_wrapper_receive {}\n", fish_quote(&end_marker));
    for chunk in encoded.as_bytes().chunks(SHELL_WRAPPER_BASE64_LINE_BYTES) {
        let chunk = std::str::from_utf8(chunk)
            .expect("standard base64 output should always be valid UTF-8");
        transport.push_str(chunk);
        transport.push_str("; printf '\\036'\n");
    }
    transport.push_str(&end_marker);
    transport.push_str("; printf '\\036'\n");
    transport
}

/// Encodes a generated POSIX wrapper as bounded shell-owned assignments.
///
/// Each physical input line is a complete command that appends one base64
/// chunk. This avoids overflowing small Darwin PTY and Readline typeahead
/// buffers while preserving the requirement that every action reaches the pane
/// as shell input. The final complete command decodes and evaluates the source.
pub(super) fn posix_shell_wrapper_transport(
    source: &str,
    classification: ShellClassification,
    zsh_history_token: Option<&MarkerToken>,
) -> String {
    const ACK: &str = "printf '\\036'";
    let source = format!("unset MEZ_WRAPPER_SOURCE\n{source}");
    let encoded = base64::engine::general_purpose::STANDARD.encode(source.as_bytes());
    let mut chunks = encoded.as_bytes().chunks(SHELL_WRAPPER_BASE64_LINE_BYTES);
    let first = chunks
        .next()
        .and_then(|chunk| std::str::from_utf8(chunk).ok())
        .unwrap_or_default();
    let mut transport = zsh_history_transport_start(classification, zsh_history_token);
    transport.push_str(&format!(
        "MEZ_WRAPPER_STTY=$(stty -g 2>/dev/null) || MEZ_WRAPPER_STTY=; {ACK}\n\
MEZ_WRAPPER_PS1=${{PS1-}}; PS1=; stty -echo 2>/dev/null || :; {ACK}\n\
MEZ_WRAPPER_BASE64_FLAG=-d; printf '' | base64 -d >/dev/null 2>&1 || MEZ_WRAPPER_BASE64_FLAG=-D; {ACK}\n\
MEZ_WRAPPER_B64={first}; {ACK}\n",
        first = shell_quote(first),
    ));
    for chunk in chunks {
        let chunk = std::str::from_utf8(chunk)
            .expect("standard base64 output should always be valid UTF-8");
        transport.push_str("MEZ_WRAPPER_B64=$MEZ_WRAPPER_B64");
        transport.push_str(&shell_quote(chunk));
        transport.push_str("; ");
        transport.push_str(ACK);
        transport.push('\n');
    }
    transport.push_str(&format!(
        "if [ -n \"$MEZ_WRAPPER_STTY\" ]; then stty \"$MEZ_WRAPPER_STTY\" 2>/dev/null || :; fi; {ACK}\n\
MEZ_WRAPPER_SOURCE=$(printf '%s' \"$MEZ_WRAPPER_B64\" | base64 \"$MEZ_WRAPPER_BASE64_FLAG\"); {ACK}\n\
unset MEZ_WRAPPER_B64 MEZ_WRAPPER_STTY MEZ_WRAPPER_BASE64_FLAG; {ACK}\n\
PS1=$MEZ_WRAPPER_PS1; unset MEZ_WRAPPER_PS1; {ACK}\n\
eval \"$MEZ_WRAPPER_SOURCE\"; {}\n",
        posix_shell_history_transport_fallback(classification, zsh_history_token),
    ));
    transport
}

/// Formats the transaction-local environment command used to launch isolated
/// POSIX-compatible child shells.
fn posix_noninteractive_agent_env_command_words() -> String {
    let mut words = vec![
        "env".to_string(),
        "-u MEZ_MARKER_TOKEN".to_string(),
        "-u MEZ_TURN".to_string(),
        "-u MEZ_AGENT".to_string(),
        "-u MEZ_PANE".to_string(),
        "-u MEZ_RESTORE_ERREXIT".to_string(),
        "-u MEZ_RESTORE_NOUNSET".to_string(),
        "-u MEZ_HISTORY_RESTORE".to_string(),
        "-u MEZ_HISTORY_HISTFILE_WAS_SET".to_string(),
        "-u MEZ_HISTORY_HISTFILE_SAVED".to_string(),
    ];
    words.extend(
        AGENT_SHELL_STARTUP_ENV_UNSETS
            .iter()
            .map(|key| format!("-u {key}")),
    );
    words.extend(
        NONINTERACTIVE_AGENT_ENV
            .iter()
            .map(|(key, value)| format!("{key}={}", shell_quote(value))),
    );
    words.join(" ")
}

/// Formats transaction-local non-interactive environment words for Fish shell
/// wrappers.
fn fish_noninteractive_agent_env_words() -> String {
    let mut words = AGENT_SHELL_STARTUP_ENV_UNSETS
        .iter()
        .map(|key| format!("-u {key}"))
        .collect::<Vec<_>>();
    words.extend(
        NONINTERACTIVE_AGENT_ENV
            .iter()
            .map(|(key, value)| format!("{key}={}", fish_quote(value))),
    );
    words.join(" ")
}

/// Formats transaction-local environment words for a POSIX persistent agent
/// subshell.
fn posix_agent_subshell_env_word_list() -> Vec<String> {
    posix_agent_subshell_env_word_list_for_classification(ShellClassification::PosixSh)
}

/// Formats transaction-local environment words for a Fish persistent agent
/// subshell.
fn fish_agent_subshell_env_word_list() -> Vec<String> {
    let mut words = AGENT_SHELL_STARTUP_ENV_UNSETS
        .iter()
        .map(|key| format!("-u {key}"))
        .collect::<Vec<_>>();
    words.extend(
        AGENT_SUBSHELL_PROMPT_ENV
            .iter()
            .map(|(key, value)| format!("{key}={}", fish_quote(value))),
    );
    words.push("fish_private_mode=1".to_string());
    words
}

/// Runs the fish quote operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn fish_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{escaped}'")
}
