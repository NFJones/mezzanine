//! Shell classification, transaction rendering, and authored-command policy.
//!
//! This module constructs shell source but never executes it. Runtime adapters
//! own pane writes, process lifetime, timeouts, and output observation.

use super::{AgentShellValidationError, AgentShellValidationResult, shell_quote};
use crate::{
    SHELL_OUTPUT_BASE64_BEGIN_MARKER, SHELL_OUTPUT_BASE64_DROPPED_BYTES_MARKER,
    SHELL_OUTPUT_BASE64_END_MARKER, SHELL_STATUS_BASE64_BEGIN_MARKER,
    SHELL_STATUS_BASE64_END_MARKER,
};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::path::Path;

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

    /// Classifies the shell using the file stem plus optional runtime probe
    /// data (version output from `$SHELL --version`). The version probe takes
    /// precedence over the file stem when it identifies a known shell.
    pub fn classify_with_probe(shell_path: impl AsRef<Path>, shell_version: Option<&str>) -> Self {
        let file_stem = shell_path
            .as_ref()
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("");
        if let Some(classification) = shell_version.and_then(classify_version_probe) {
            return classification;
        }
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

/// Runs the classify version probe operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn classify_version_probe(version: &str) -> Option<ShellClassification> {
    let lower = version.to_ascii_lowercase();
    if lower.contains("bash") {
        return Some(ShellClassification::Bash);
    }
    if lower.contains("zsh") {
        return Some(ShellClassification::Zsh);
    }
    if lower.contains("fish") {
        return Some(ShellClassification::Fish);
    }
    if lower.contains("dash") || lower.contains("debian almquist") {
        return Some(ShellClassification::PosixSh);
    }
    if lower.contains("ksh") || lower.contains("kornshell") {
        return Some(ShellClassification::PosixSh);
    }
    None
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

/// One argument in a typed isolated-child process launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellChildArgument {
    /// A literal argv element rendered with shell-specific quoting.
    Literal(String),
    /// The temporary command file materialized by the transaction wrapper.
    MaterializedCommandFile,
}

/// Typed executable and argv for an isolated child process.
///
/// The contract deliberately excludes raw shell fragments. Renderers quote
/// every literal and substitute the wrapper-owned command-file variable only
/// for the dedicated argument variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellChildLaunch {
    /// Absolute executable path resolved in the pane environment.
    pub executable: String,
    /// Ordered argv elements excluding argv[0].
    pub arguments: Vec<ShellChildArgument>,
    /// Optional runtime-owned descriptor used to capture trusted child status.
    ///
    /// The transaction wrapper redirects this descriptor to a private temporary
    /// file and emits that file through a framing channel separate from child
    /// stdout and stderr after the process exits.
    pub status_fd: Option<u8>,
}

impl ShellChildLaunch {
    /// Validates one typed child launch before shell rendering.
    pub fn new(
        executable: impl Into<String>,
        arguments: Vec<ShellChildArgument>,
    ) -> AgentShellValidationResult<Self> {
        let executable = executable.into();
        if !Path::new(&executable).is_absolute()
            || executable.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(AgentShellValidationError::invalid_args(
                "typed child executable must be an absolute printable path",
            ));
        }
        if arguments.iter().any(|argument| {
            matches!(argument, ShellChildArgument::Literal(value) if value.contains('\0') || value.bytes().any(|byte| byte.is_ascii_control()))
        }) {
            return Err(AgentShellValidationError::invalid_args(
                "typed child arguments must not contain NUL or control bytes",
            ));
        }
        if arguments
            .iter()
            .filter(|argument| matches!(argument, ShellChildArgument::MaterializedCommandFile))
            .count()
            > 1
        {
            return Err(AgentShellValidationError::invalid_args(
                "typed child launch accepts at most one materialized command-file argument",
            ));
        }
        Ok(Self {
            executable,
            arguments,
            status_fd: None,
        })
    }

    /// Selects one inherited descriptor for runtime-owned child status.
    pub fn with_status_fd(mut self, status_fd: u8) -> AgentShellValidationResult<Self> {
        if !(3..=9).contains(&status_fd) {
            return Err(AgentShellValidationError::invalid_args(
                "typed child status fd must be between 3 and 9",
            ));
        }
        self.status_fd = Some(status_fd);
        Ok(self)
    }
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
/// Maximum base64 bytes appended by one shell wrapper transport command.
///
/// Wrapper source is reconstructed through interactive shell input before the
/// ordinary command-payload receiver exists. A smaller bound leaves ample
/// room for assignment syntax in Darwin's constrained terminal input buffers.
#[cfg(target_os = "macos")]
pub(crate) const SHELL_WRAPPER_BASE64_LINE_BYTES: usize = 64;
#[cfg(not(target_os = "macos"))]
pub(crate) const SHELL_WRAPPER_BASE64_LINE_BYTES: usize = 640;
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
    lines.extend([
        "if test \"$MEZ_WRITE_STATUS\" -eq 0".to_string(),
        "if command -q setsid; and command setsid -w true >/dev/null 2>&1".to_string(),
        fish_child_command_line(
            "    command setsid -w env",
            noninteractive_env,
            shell_invocation,
            transport,
            status_fd,
        ),
        "else if command -q python3".to_string(),
        fish_child_command_line(
            &format!("    {PYTHON_SETSID_WAIT_COMMAND} env"),
            noninteractive_env,
            shell_invocation,
            transport,
            status_fd,
        ),
        "else if command -q perl".to_string(),
        fish_child_command_line(
            &format!("    {PERL_SETSID_WAIT_COMMAND} env"),
            noninteractive_env,
            shell_invocation,
            transport,
            status_fd,
        ),
        "else".to_string(),
        fish_child_command_line(
            "    command env",
            noninteractive_env,
            shell_invocation,
            transport,
            status_fd,
        ),
        "end".to_string(),
        "set MEZ_STATUS $status".to_string(),
    ]);
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
) -> String {
    let redirect = if transport == ShellTransactionOutputTransport::Base64 {
        " > \"$MEZ_OUTPUT_FILE\" 2>&1"
    } else {
        ""
    };
    let status_redirect = status_fd
        .map(|fd| format!(" {fd}>\"$MEZ_STATUS_FILE\""))
        .unwrap_or_default();
    format!(
        "{prefix} {noninteractive_env} {shell_invocation} </dev/null{redirect}{status_redirect}"
    )
}

/// Renders one typed child launch as POSIX shell words.
fn posix_typed_child_launch_words(launch: &ShellChildLaunch) -> String {
    std::iter::once(shell_quote(&launch.executable))
        .chain(launch.arguments.iter().map(|argument| match argument {
            ShellChildArgument::Literal(value) => posix_shell_quoted_argument(value),
            ShellChildArgument::MaterializedCommandFile => "\"$MEZ_COMMAND_FILE\"".to_string(),
        }))
        .collect::<Vec<_>>()
        .join(" ")
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

    /// Runs the render posix operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn render_posix(&self) -> String {
        self.render_posix_input_for_classification(ShellClassification::PosixSh)
            .combined()
    }

    /// Renders a POSIX-compatible shell transaction wrapper for one resolved
    /// shell classification.
    ///
    /// The wrapper is parsed by the persistent agent shell, then starts a
    /// startup-suppressed child shell to execute the materialized command file.
    fn render_posix_input_for_classification(
        &self,
        classification: ShellClassification,
    ) -> ShellTransactionInput {
        if classification == ShellClassification::Bash && self.bash_receiver_token.is_none() {
            return ShellTransactionInput {
                wrapper: String::new(),
                receiver_payload: String::new(),
                payload: String::new(),
                payload_receiver_acknowledgements: false,
            };
        }
        let function_name = transaction_function_name(self.marker.as_str());
        let command_materialization = posix_command_file_materialization(
            &self.command,
            self.input_sidecar.as_deref(),
            self.marker.as_str(),
            "command printf '\\033]133;C;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \"$MEZ_MARKER_TOKEN\" \"$MEZ_TURN\" \"$MEZ_AGENT\" \"$MEZ_PANE\"",
            self.payload_receiver_acknowledgements,
        );
        let shell_invocation = self.child_launch.as_ref().map_or_else(
            || {
                posix_shell_script_invocation_words(
                    &self.shell_path,
                    classification,
                    "\"$MEZ_COMMAND_FILE\"",
                )
            },
            posix_typed_child_launch_words,
        );
        let child_env = if self.child_launch.is_some() {
            String::new()
        } else {
            posix_noninteractive_agent_env_command_words()
        };
        let child_invocation = posix_child_command_invocation_lines(
            self.output_transport,
            self.output_max_raw_bytes,
            &child_env,
            &shell_invocation,
            self.child_launch
                .as_ref()
                .and_then(|launch| launch.status_fd),
        );
        let (history_start, history_restore, history_marker_finish) =
            if classification == ShellClassification::Bash && self.bash_receiver_token.is_some() {
                (
                    posix_shell_state_suppression_start().to_string(),
                    String::new(),
                    posix_shell_state_marker_finish_prefix().to_string(),
                )
            } else if classification == ShellClassification::Zsh && self.zsh_history_token.is_some()
            {
                (
                    zsh_shell_history_suppression_start().to_string(),
                    String::new(),
                    zsh_shell_history_marker_finish_prefix(self.zsh_history_token.as_ref()),
                )
            } else {
                (
                    posix_shell_history_suppression_start_for_classification(classification),
                    posix_shell_history_file_restore().to_string(),
                    posix_shell_history_marker_finish_prefix_for_classification(classification),
                )
            };
        let sidecar_frame_cleanup = if self.input_sidecar.is_some() {
            "if [ -n \"$MEZ_SIDECAR_FRAME\" ]; then command rm -f -- \"$MEZ_SIDECAR_FRAME\" >/dev/null 2>&1 || :; fi\n\\
unset MEZ_SIDECAR_FRAME MEZ_SIDECAR_FRAME_SEQUENCE MEZ_SIDECAR_FRAME_LENGTH MEZ_SIDECAR_FRAME_DIGEST MEZ_SIDECAR_FRAME_COUNT MEZ_SIDECAR_FRAME_ACTUAL MEZ_SIDECAR_SHA256\n"
        } else {
            ""
        };
        let wrapper = format!(
            "{history_start}\
{function_name}() {{\n\
MEZ_MARKER_TOKEN={marker}\n\
MEZ_TURN={turn}\n\
MEZ_AGENT={agent}\n\
MEZ_PANE={pane}\n\
{command_file_lines}\
{child_invocation}\
command rm -f -- \"$MEZ_COMMAND_FILE\" \"$MEZ_COMMAND_B64\" \"$MEZ_SIDECAR_DATA\" >/dev/null 2>&1 || :\n\
{sidecar_frame_cleanup}\
if [ -n \"$MEZ_OUTPUT_FILE\" ]; then command rm -f -- \"$MEZ_OUTPUT_FILE\" >/dev/null 2>&1 || :; fi\n\
if [ -n \"$MEZ_STATUS_FILE\" ]; then command rm -f -- \"$MEZ_STATUS_FILE\" >/dev/null 2>&1 || :; fi\n\
unset MEZ_COMMAND_FILE MEZ_COMMAND_B64 MEZ_SIDECAR_DATA MEZ_COMMAND_END MEZ_COMMAND_LINE MEZ_COMMAND_SEEN_END MEZ_OUTPUT_FILE MEZ_STATUS_FILE MEZ_STTY_STATE MEZ_WRITE_STATUS\n\
unset -f {function_name} 2>/dev/null || :\n\
{history_restore}\
{history_marker_finish}command printf '\\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
\"$MEZ_STATUS\" \"$MEZ_MARKER_TOKEN\" \"$MEZ_TURN\" \"$MEZ_AGENT\" \"$MEZ_PANE\"; \
unset MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS; {errexit_restore}\n\
}}\n\
{function_name}\n",
            history_start = history_start,
            history_restore = history_restore,
            history_marker_finish = history_marker_finish,
            sidecar_frame_cleanup = sidecar_frame_cleanup,
            errexit_restore = posix_shell_errexit_restore_suffix(),
            function_name = function_name,
            marker = shell_quote(self.marker.as_str()),
            turn = shell_quote(&self.turn_id),
            agent = shell_quote(&self.agent_id),
            pane = shell_quote(&self.pane_id),
            command_file_lines = command_materialization.setup,
            child_invocation = child_invocation,
        );
        let bash_transport = bash_private_receiver_transport(
            &wrapper,
            classification,
            self.bash_receiver_token.as_ref(),
            self.marker.as_str(),
        );
        ShellTransactionInput {
            wrapper: bash_transport.as_ref().map_or_else(
                || {
                    posix_shell_wrapper_transport(
                        &wrapper,
                        classification,
                        self.zsh_history_token.as_ref(),
                    )
                },
                |transport| transport.trigger.clone(),
            ),
            receiver_payload: bash_transport
                .map(|transport| transport.payload)
                .unwrap_or_default(),
            payload: command_materialization.payload,
            payload_receiver_acknowledgements: self.payload_receiver_acknowledgements,
        }
    }

    /// Runs the render for classification operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn render_for_classification(&self, classification: ShellClassification) -> String {
        self.render_for_classification_input(classification)
            .combined()
    }

    /// Renders the non-stateful shell transaction as a wrapper plus streamed
    /// payload.
    pub fn render_for_classification_input(
        &self,
        classification: ShellClassification,
    ) -> ShellTransactionInput {
        if classification == ShellClassification::Fish {
            self.render_fish_input()
        } else {
            self.render_posix_input_for_classification(classification)
        }
    }

    /// Renders a stateful shell command wrapper that executes directly in the
    /// interactive pane shell, preserving `cd`, environment, aliases, and
    /// shell options after the command completes.
    ///
    /// Stateful actions disclose in structured content that they may change
    /// the pane shell state. This wrapper skips the child-shell isolation so
    /// mutations persist in the interactive shell context.
    pub fn render_stateful(&self) -> String {
        self.render_stateful_for_classification_input(ShellClassification::PosixSh)
            .combined()
    }

    /// Renders one stateful POSIX-compatible transaction for a known shell.
    ///
    /// Zsh uses the bounded wrapper transport so its authenticated history
    /// record can push a private frame before any generated source is read.
    fn render_posix_stateful_for_classification(
        &self,
        classification: ShellClassification,
    ) -> String {
        let function_name = transaction_function_name(self.marker.as_str());
        let zsh_history_isolation =
            classification == ShellClassification::Zsh && self.zsh_history_token.is_some();
        let (history_start, history_restore, history_marker_finish) =
            if classification == ShellClassification::Bash && self.bash_receiver_token.is_some() {
                (
                    posix_shell_state_suppression_start().to_string(),
                    String::new(),
                    posix_shell_state_marker_finish_prefix().to_string(),
                )
            } else if zsh_history_isolation {
                (
                    zsh_shell_history_suppression_start().to_string(),
                    String::new(),
                    zsh_shell_history_marker_finish_prefix(self.zsh_history_token.as_ref()),
                )
            } else {
                (
                    posix_shell_history_suppression_start_for_classification(classification),
                    posix_shell_history_file_restore().to_string(),
                    posix_shell_history_marker_finish_prefix_for_classification(classification),
                )
            };
        let source = format!(
            "{history_start}\
{function_name}() {{\n\
command printf '\\033]133;C;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
{marker} {turn} {agent} {pane}\n\
{{\n\
{command}\n\
}}\n\
MEZ_STATUS=$?\n\
unset -f {function_name} 2>/dev/null || :\n\
{history_restore}\
{history_marker_finish}command printf '\\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
\"$MEZ_STATUS\" {marker} {turn} {agent} {pane}; unset MEZ_STATUS; {errexit_restore}\n\
}}\n\
{function_name}\n",
            history_start = history_start,
            history_restore = history_restore,
            history_marker_finish = history_marker_finish,
            errexit_restore = posix_shell_errexit_restore_suffix(),
            function_name = function_name,
            marker = shell_quote(self.marker.as_str()),
            turn = shell_quote(&self.turn_id),
            agent = shell_quote(&self.agent_id),
            pane = shell_quote(&self.pane_id),
            command = self.command,
        );
        if zsh_history_isolation {
            posix_shell_wrapper_transport(&source, classification, self.zsh_history_token.as_ref())
        } else {
            source
        }
    }

    /// Runs the render stateful for classification operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn render_stateful_for_classification(
        &self,
        classification: ShellClassification,
    ) -> String {
        self.render_stateful_for_classification_input(classification)
            .combined()
    }

    /// Renders stateful shell input with a separately gated Bash receiver stage.
    pub fn render_stateful_for_classification_input(
        &self,
        classification: ShellClassification,
    ) -> ShellTransactionInput {
        if classification == ShellClassification::Bash && self.bash_receiver_token.is_none() {
            return ShellTransactionInput {
                wrapper: String::new(),
                receiver_payload: String::new(),
                payload: String::new(),
                payload_receiver_acknowledgements: false,
            };
        }
        let source = if classification == ShellClassification::Fish {
            self.render_fish_stateful()
        } else {
            self.render_posix_stateful_for_classification(classification)
        };
        let bash_transport = bash_private_receiver_transport(
            &source,
            classification,
            self.bash_receiver_token.as_ref(),
            self.marker.as_str(),
        );
        ShellTransactionInput {
            wrapper: bash_transport
                .as_ref()
                .map_or(source, |transport| transport.trigger.clone()),
            receiver_payload: bash_transport
                .map(|transport| transport.payload)
                .unwrap_or_default(),
            payload: String::new(),
            payload_receiver_acknowledgements: self.payload_receiver_acknowledgements,
        }
    }

    /// Renders a Fish shell transaction wrapper with fish-native block syntax
    /// (`begin`/`end`), `set` variable assignment, and `$status` for exit code
    /// capture. This preserves the same OSC 133 marker convention used by the
    /// POSIX wrapper.
    pub fn render_fish(&self) -> String {
        self.render_fish_input().combined()
    }

    /// Renders a Fish shell transaction as a wrapper plus streamed payload.
    pub fn render_fish_input(&self) -> ShellTransactionInput {
        let start_marker_line = "printf '\\033]133;C;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' $MEZ_MARKER_TOKEN $MEZ_TURN $MEZ_AGENT $MEZ_PANE";
        let typed_child_uses_command_file = self.child_launch.as_ref().is_some_and(|launch| {
            launch
                .arguments
                .iter()
                .any(|argument| matches!(argument, ShellChildArgument::MaterializedCommandFile))
        });
        let command_materialization = if self.child_launch.is_some()
            && !typed_child_uses_command_file
            && self.input_sidecar.is_none()
        {
            CommandMaterialization {
                setup: format!(
                    "set -l MEZ_COMMAND_FILE ''\n\
set -l MEZ_COMMAND_B64 ''\n\
set -l MEZ_SIDECAR_DATA ''\n\
set -l MEZ_STTY_STATE ''\n\
set -l MEZ_WRITE_STATUS 0\n\
{start_marker_line}\n"
                ),
                payload: String::new(),
            }
        } else {
            fish_command_file_materialization(
                &self.command,
                self.input_sidecar.as_deref(),
                self.marker.as_str(),
                start_marker_line,
                "printf '\\033]133;R;mez_payload_receiver=ready;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' $MEZ_MARKER_TOKEN $MEZ_TURN $MEZ_AGENT $MEZ_PANE",
                self.payload_receiver_acknowledgements,
            )
        };
        let shell_invocation = self.child_launch.as_ref().map_or_else(
            || {
                fish_shell_script_invocation_words(
                    &self.shell_path,
                    ShellClassification::Fish,
                    "\"$MEZ_COMMAND_FILE\"",
                )
            },
            fish_typed_child_launch_words,
        );
        let child_env = if self.child_launch.is_some() {
            String::new()
        } else {
            fish_noninteractive_agent_env_words()
        };
        let child_invocation = fish_child_command_invocation_lines(
            self.output_transport,
            self.output_max_raw_bytes,
            &child_env,
            &shell_invocation,
            self.child_launch
                .as_ref()
                .and_then(|launch| launch.status_fd),
        );
        let child_output_separator = if self.child_launch.is_some() {
            ""
        } else {
            "printf '\\n'\n"
        };
        let sidecar_frame_cleanup = if self.input_sidecar.is_some() {
            "if test -n \"$MEZ_SIDECAR_FRAME\"; command rm -f -- \"$MEZ_SIDECAR_FRAME\" >/dev/null 2>&1; or true; end\n\\
set -e MEZ_SIDECAR_FRAME MEZ_SIDECAR_FRAME_SEQUENCE MEZ_SIDECAR_FRAME_LENGTH MEZ_SIDECAR_FRAME_DIGEST MEZ_SIDECAR_FRAME_COUNT MEZ_SIDECAR_FRAME_ACTUAL MEZ_SIDECAR_SHA256\n"
        } else {
            ""
        };
        let wrapper = format!(
            "{history_start}\
begin\n\
set -l MEZ_MARKER_TOKEN {marker}\n\
set -l MEZ_TURN {turn}\n\
set -l MEZ_AGENT {agent}\n\
set -l MEZ_PANE {pane}\n\
{command_file_lines}\
set -l MEZ_STATUS 0\n\
{child_output_separator}\
{child_invocation}\
if test -n \"$MEZ_COMMAND_FILE\"; command rm -f -- \"$MEZ_COMMAND_FILE\" >/dev/null 2>&1; or true; end\n\
if test -n \"$MEZ_COMMAND_B64\"; command rm -f -- \"$MEZ_COMMAND_B64\" >/dev/null 2>&1; or true; end\n\
if test -n \"$MEZ_SIDECAR_DATA\"; command rm -f -- \"$MEZ_SIDECAR_DATA\" >/dev/null 2>&1; or true; end\n\
{sidecar_frame_cleanup}\
if test -n \"$MEZ_OUTPUT_FILE\"; command rm -f -- \"$MEZ_OUTPUT_FILE\" >/dev/null 2>&1; or true; end\n\
if test -n \"$MEZ_STATUS_FILE\"; command rm -f -- \"$MEZ_STATUS_FILE\" >/dev/null 2>&1; or true; end\n\
set -e MEZ_COMMAND_FILE MEZ_COMMAND_B64 MEZ_SIDECAR_DATA MEZ_COMMAND_END MEZ_COMMAND_LINE MEZ_COMMAND_SEEN_END MEZ_OUTPUT_FILE MEZ_STATUS_FILE MEZ_STTY_STATE MEZ_WRITE_STATUS\n\
{history_restore}\
printf '\\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
$MEZ_STATUS $MEZ_MARKER_TOKEN $MEZ_TURN $MEZ_AGENT $MEZ_PANE\n\
end\n",
            history_start = fish_shell_history_suppression_start(),
            history_restore = fish_shell_history_restore(),
            sidecar_frame_cleanup = sidecar_frame_cleanup,
            marker = fish_quote(self.marker.as_str()),
            turn = fish_quote(&self.turn_id),
            agent = fish_quote(&self.agent_id),
            pane = fish_quote(&self.pane_id),
            command_file_lines = command_materialization.setup,
            child_output_separator = child_output_separator,
            child_invocation = child_invocation,
        );
        ShellTransactionInput {
            wrapper: fish_shell_wrapper_transport(&wrapper, self.marker.as_str()),
            receiver_payload: String::new(),
            payload: command_materialization.payload,
            payload_receiver_acknowledgements: self.payload_receiver_acknowledgements,
        }
    }

    /// Renders a stateful Fish shell command wrapper that executes directly in
    /// the interactive pane shell using fish-native `begin`/`end` block syntax
    /// and `$status` for exit capture. Mutations persist in the interactive
    /// context.
    pub fn render_fish_stateful(&self) -> String {
        format!(
            "{history_start}\
begin\n\
set -l MEZ_MARKER_TOKEN {marker}\n\
set -l MEZ_TURN {turn}\n\
set -l MEZ_AGENT {agent}\n\
set -l MEZ_PANE {pane}\n\
printf '\\033]133;C;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
$MEZ_MARKER_TOKEN $MEZ_TURN $MEZ_AGENT $MEZ_PANE\n\
begin\n\
eval {command}\n\
end\n\
set -l MEZ_STATUS $status\n\
{history_restore}\
printf '\\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
$MEZ_STATUS $MEZ_MARKER_TOKEN $MEZ_TURN $MEZ_AGENT $MEZ_PANE\n\
end\n",
            history_start = fish_shell_history_suppression_start(),
            history_restore = fish_shell_history_restore(),
            marker = fish_quote(self.marker.as_str()),
            turn = fish_quote(&self.turn_id),
            agent = fish_quote(&self.agent_id),
            pane = fish_quote(&self.pane_id),
            command = fish_quote(&self.command),
        )
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

/// Shell-source setup plus data payload used to materialize one command file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandMaterialization {
    /// Shell code that starts the payload receiver and decodes the command.
    setup: String,
    /// Base64 command payload lines consumed by the setup receiver.
    payload: String,
}

/// Renders POSIX shell lines that materialize a transaction command file.
///
/// The generated code avoids heredocs entirely. It writes standard-base64
/// chunks to a temporary sidecar file through a receiver that starts before the
/// payload bytes are sent. This keeps large action payloads out of the
/// persistent pane shell's parsed source and lets the shell drain PTY input as
/// data instead of waiting for an entire generated wrapper to arrive.
fn posix_command_file_materialization(
    command: &str,
    input_sidecar: Option<&str>,
    marker: &str,
    start_marker_line: &str,
    acknowledge_payload_records: bool,
) -> CommandMaterialization {
    let end_marker = command_payload_end_marker(marker);
    let acknowledge = if acknowledge_payload_records {
        "command printf '\\036'"
    } else {
        ":"
    };
    let receive_record = if input_sidecar.is_some() {
        format!(
            "case \"$MEZ_COMMAND_LINE\" in C\\ *) if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then printf '%s\\n' \"${{MEZ_COMMAND_LINE#C }}\" >> \"$MEZ_COMMAND_B64\" || MEZ_WRITE_STATUS=$?; fi; {acknowledge} ;; S1B\\ *) if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then MEZ_SIDECAR_FRAME_HEADER=${{MEZ_COMMAND_LINE#S1B }}; MEZ_SIDECAR_FRAME_HEADER_SEQUENCE=${{MEZ_SIDECAR_FRAME_HEADER%% *}}; MEZ_SIDECAR_FRAME_HEADER=${{MEZ_SIDECAR_FRAME_HEADER#* }}; MEZ_SIDECAR_FRAME_LENGTH=${{MEZ_SIDECAR_FRAME_HEADER%% *}}; MEZ_SIDECAR_FRAME_DIGEST=${{MEZ_SIDECAR_FRAME_HEADER#* }}; case \"$MEZ_SIDECAR_FRAME_LENGTH\" in ''|*[!0-9]*) MEZ_WRITE_STATUS=1;; esac; case \"$MEZ_SIDECAR_FRAME_DIGEST\" in *[!0-9a-f]*|???????????????????????????????????????????????????????????????|?????????????????????????????????????????????????????????????????*) MEZ_WRITE_STATUS=1;; esac; if [ \"$MEZ_SIDECAR_FRAME_OPEN\" != 0 ] || [ \"$MEZ_SIDECAR_FRAME_HEADER_SEQUENCE\" != \"$MEZ_SIDECAR_FRAME_SEQUENCE\" ] || [ \"$MEZ_SIDECAR_FRAME_LENGTH\" -gt {frame_bytes} ] 2>/dev/null || [ \"$MEZ_SIDECAR_FRAME_DIGEST\" != \"${{MEZ_SIDECAR_FRAME_DIGEST%% *}}\" ]; then MEZ_WRITE_STATUS=1; fi; if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then MEZ_SIDECAR_FRAME_OPEN=1; : > \"$MEZ_SIDECAR_FRAME\" || MEZ_WRITE_STATUS=$?; fi; fi ;; S1D\\ *) if [ \"$MEZ_WRITE_STATUS\" -eq 0 ] && [ \"$MEZ_SIDECAR_FRAME_OPEN\" = 1 ]; then printf '%s\\n' \"${{MEZ_COMMAND_LINE#S1D }}\" >> \"$MEZ_SIDECAR_FRAME\" || MEZ_WRITE_STATUS=$?; else MEZ_WRITE_STATUS=1; fi ;; S1E\\ *) if [ \"$MEZ_WRITE_STATUS\" -eq 0 ] && [ \"$MEZ_SIDECAR_FRAME_OPEN\" = 1 ]; then MEZ_SIDECAR_FRAME_END_SEQUENCE=${{MEZ_COMMAND_LINE#S1E }}; MEZ_SIDECAR_FRAME_COUNT=$(wc -c < \"$MEZ_SIDECAR_FRAME\" | tr -d '[:space:]') || MEZ_WRITE_STATUS=$?; if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then if [ \"$MEZ_SIDECAR_SHA256\" = sha256sum ]; then MEZ_SIDECAR_FRAME_ACTUAL=$(sha256sum -- \"$MEZ_SIDECAR_FRAME\"); else MEZ_SIDECAR_FRAME_ACTUAL=$(shasum -a 256 -- \"$MEZ_SIDECAR_FRAME\"); fi; MEZ_SIDECAR_FRAME_ACTUAL=${{MEZ_SIDECAR_FRAME_ACTUAL%%[[:space:]]*}}; fi; if [ \"$MEZ_SIDECAR_FRAME_END_SEQUENCE\" != \"$MEZ_SIDECAR_FRAME_SEQUENCE\" ] || [ \"$MEZ_SIDECAR_FRAME_END_SEQUENCE\" != \"${{MEZ_SIDECAR_FRAME_END_SEQUENCE%% *}}\" ] || [ \"$MEZ_SIDECAR_FRAME_COUNT\" != \"$MEZ_SIDECAR_FRAME_LENGTH\" ] || [ \"$MEZ_SIDECAR_FRAME_ACTUAL\" != \"$MEZ_SIDECAR_FRAME_DIGEST\" ]; then MEZ_WRITE_STATUS=1; else sed 's/^/# __MEZ_INPUT_SIDECAR_V1__ /' \"$MEZ_SIDECAR_FRAME\" >> \"$MEZ_SIDECAR_DATA\" || MEZ_WRITE_STATUS=$?; MEZ_SIDECAR_FRAME_SEQUENCE=$((MEZ_SIDECAR_FRAME_SEQUENCE + 1)); MEZ_SIDECAR_FRAME_OPEN=0; fi; else MEZ_WRITE_STATUS=1; fi; {acknowledge} ;; *) MEZ_WRITE_STATUS=1; {acknowledge} ;; esac",
            frame_bytes = SHELL_TRANSACTION_SIDECAR_FRAME_BYTES,
        )
    } else {
        format!(
            "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then case \"$MEZ_COMMAND_LINE\" in C\\ *) printf '%s\\n' \"${{MEZ_COMMAND_LINE#C }}\" >> \"$MEZ_COMMAND_B64\" || MEZ_WRITE_STATUS=$? ;; *) MEZ_WRITE_STATUS=1 ;; esac; fi; {acknowledge}"
        )
    };
    let terminal_mode = if input_sidecar.is_some() {
        "stty -icanon min 1 time 0 -echo 2>/dev/null || :"
    } else {
        "stty -echo 2>/dev/null || :"
    };
    let mut lines = vec![
        "MEZ_COMMAND_FILE=$(mktemp) || MEZ_COMMAND_FILE=".to_string(),
        "MEZ_COMMAND_B64=".to_string(),
        "MEZ_SIDECAR_DATA=".to_string(),
        "MEZ_SIDECAR_FRAME=".to_string(),
        "MEZ_SIDECAR_FRAME_SEQUENCE=0".to_string(),
        "MEZ_SIDECAR_FRAME_OPEN=0".to_string(),
        format!("MEZ_COMMAND_END={}", shell_quote(&end_marker)),
        "MEZ_COMMAND_SEEN_END=0".to_string(),
        "MEZ_STTY_STATE=".to_string(),
        "MEZ_WRITE_STATUS=0".to_string(),
        "if [ -n \"$MEZ_COMMAND_FILE\" ]; then".to_string(),
        "command -v base64 >/dev/null 2>&1 || { printf '%s\\n' 'base64 is required for Mezzanine shell transaction wrappers' >&2; MEZ_WRITE_STATUS=127; }".to_string(),
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then MEZ_COMMAND_B64=$(mktemp) || MEZ_WRITE_STATUS=1; fi".to_string(),
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then : > \"$MEZ_COMMAND_B64\" || MEZ_WRITE_STATUS=$?; fi".to_string(),
    ];
    if input_sidecar.is_some() {
        lines.push(
            "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then MEZ_SIDECAR_DATA=$(mktemp) || MEZ_WRITE_STATUS=1; fi"
                .to_string(),
        );
        lines.push(
            "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then MEZ_SIDECAR_FRAME=$(mktemp) || MEZ_WRITE_STATUS=1; fi"
                .to_string(),
        );
        lines.push(
            "if command -v sha256sum >/dev/null 2>&1; then MEZ_SIDECAR_SHA256=sha256sum; elif command -v shasum >/dev/null 2>&1; then MEZ_SIDECAR_SHA256=shasum; else MEZ_WRITE_STATUS=127; fi"
                .to_string(),
        );
    }
    lines.extend([
        "MEZ_STTY_STATE=$(stty -g 2>/dev/null) || MEZ_STTY_STATE=".to_string(),
        format!("if [ -n \"$MEZ_STTY_STATE\" ]; then {terminal_mode}; fi"),
        start_marker_line.to_string(),
        "while IFS= read -r MEZ_COMMAND_LINE; do".to_string(),
        format!("if [ \"$MEZ_COMMAND_LINE\" = \"$MEZ_COMMAND_END\" ]; then if [ \"${{MEZ_SIDECAR_FRAME_OPEN:-0}}\" != 0 ]; then MEZ_WRITE_STATUS=1; fi; MEZ_COMMAND_SEEN_END=1; {acknowledge}; break; fi"),
        receive_record,
        "done".to_string(),
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ] && [ \"$MEZ_COMMAND_SEEN_END\" != 1 ]; then printf '%s\\n' 'Mezzanine shell transaction command payload ended before sentinel' >&2; MEZ_WRITE_STATUS=1; fi".to_string(),
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then if base64 -d < \"$MEZ_COMMAND_B64\" > \"$MEZ_COMMAND_FILE\" 2>/dev/null; then MEZ_WRITE_STATUS=0; else base64 -D < \"$MEZ_COMMAND_B64\" > \"$MEZ_COMMAND_FILE\"; MEZ_WRITE_STATUS=$?; fi; fi".to_string(),
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ] && [ -n \"$MEZ_SIDECAR_DATA\" ]; then cat \"$MEZ_SIDECAR_DATA\" >> \"$MEZ_COMMAND_FILE\" || MEZ_WRITE_STATUS=$?; fi".to_string(),
        "else".to_string(),
        "MEZ_WRITE_STATUS=1".to_string(),
        "fi".to_string(),
    ]);
    lines.push(
        "if [ -n \"$MEZ_STTY_STATE\" ]; then stty \"$MEZ_STTY_STATE\" 2>/dev/null || :; MEZ_STTY_STATE=; fi"
            .to_string(),
    );
    CommandMaterialization {
        setup: lines.join("\n") + "\n",
        payload: command_payload_lines(command, &end_marker, input_sidecar),
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

/// Renders a Fish command word sequence that starts the persistent agent-mode
/// child shell without user startup files.
///
/// # Parameters
/// - `shell_path`: Absolute resolved shell path.
/// - `classification`: Shell classification used to choose safe startup flags.
fn fish_shell_interactive_invocation_words(
    shell_path: &str,
    classification: ShellClassification,
) -> String {
    let mut words = vec![fish_quote(shell_path)];
    let startup_args = startup_suppression_args(classification);
    words.extend(startup_args.iter().map(|arg| (*arg).to_string()));
    let mut exec_words = vec!["exec".to_string(), fish_quote(shell_path)];
    exec_words.extend(startup_args.iter().map(|arg| (*arg).to_string()));
    exec_words.push("--init-command".to_string());
    exec_words.push(fish_quote(fish_wrapper_receiver_init_command()));
    exec_words.push("-i".to_string());
    let readiness_source = format!(
        "command printf '\\e]133;B\\e\\\\'; {}",
        exec_words.join(" ")
    );
    words.push("-c".to_string());
    words.push(fish_quote(&readiness_source));
    words.join(" ")
}

/// Renders Fish syntax that writes a shell transaction command through short
/// base64 chunks into a temporary script file.
///
/// Fish wrappers cannot safely embed model-authored or runtime-generated
/// scripts as one large `-c` argument. Materializing the script keeps payload
/// bytes inert until the configured Fish shell reads them from a file.
fn fish_command_file_materialization(
    command: &str,
    input_sidecar: Option<&str>,
    marker: &str,
    start_marker_line: &str,
    receiver_ready_marker_line: &str,
    acknowledge_payload_records: bool,
) -> CommandMaterialization {
    let end_marker = command_payload_end_marker(marker);
    let acknowledge = if acknowledge_payload_records {
        "printf '\\036'"
    } else {
        "true"
    };
    let acknowledge_command_record = format!(
        "string replace -r '^C ' '' -- \"$MEZ_COMMAND_LINE\" >> \"$MEZ_COMMAND_B64\"; or set MEZ_WRITE_STATUS $status; {acknowledge}"
    );
    let mut lines = vec![
        "set -l MEZ_COMMAND_FILE (mktemp); or set -l MEZ_COMMAND_FILE ''".to_string(),
        "set -l MEZ_COMMAND_B64 ''".to_string(),
        "set -l MEZ_SIDECAR_DATA ''".to_string(),
        format!("set -l MEZ_COMMAND_END {}", fish_quote(&end_marker)),
        "set -l MEZ_COMMAND_SEEN_END 0".to_string(),
        "set -l MEZ_STTY_STATE ''".to_string(),
        "set -l MEZ_WRITE_STATUS 0".to_string(),
        "if test -n \"$MEZ_COMMAND_FILE\"".to_string(),
        "command -q base64; or begin; printf '%s\\n' 'base64 is required for Mezzanine shell transaction wrappers' >&2; set MEZ_WRITE_STATUS 127; end".to_string(),
        "if test \"$MEZ_WRITE_STATUS\" -eq 0; set MEZ_COMMAND_B64 (mktemp); or set MEZ_WRITE_STATUS 1; end".to_string(),
        "if test \"$MEZ_WRITE_STATUS\" -eq 0; : > \"$MEZ_COMMAND_B64\"; or set MEZ_WRITE_STATUS $status; end".to_string(),
    ];
    if input_sidecar.is_some() {
        lines.push("set -l MEZ_SIDECAR_FRAME ''".to_string());
        lines.push("set -l MEZ_SIDECAR_FRAME_SEQUENCE 0".to_string());
        lines.push("set -l MEZ_SIDECAR_FRAME_OPEN 0".to_string());
        lines.push(
            "if test \"$MEZ_WRITE_STATUS\" -eq 0; set MEZ_SIDECAR_DATA (mktemp); or set MEZ_WRITE_STATUS 1; end"
                .to_string(),
        );
        lines.push(
            "if test \"$MEZ_WRITE_STATUS\" -eq 0; set MEZ_SIDECAR_FRAME (mktemp); or set MEZ_WRITE_STATUS 1; end"
                .to_string(),
        );
        lines.push(
            "if command -q sha256sum; set MEZ_SIDECAR_SHA256 sha256sum; else if command -q shasum; set MEZ_SIDECAR_SHA256 shasum; else; set MEZ_WRITE_STATUS 127; end"
                .to_string(),
        );
    }
    let terminal_mode = if input_sidecar.is_some() {
        "stty -icanon min 1 time 0 -echo 2>/dev/null; or true"
    } else {
        "stty -echo 2>/dev/null; or true"
    };
    let open_frame_check = input_sidecar
        .is_some()
        .then_some("if test \"$MEZ_SIDECAR_FRAME_OPEN\" -ne 0; set MEZ_WRITE_STATUS 1; end");
    lines.extend([
        "set MEZ_STTY_STATE (stty -g 2>/dev/null); or set MEZ_STTY_STATE ''".to_string(),
        "if test -n \"$MEZ_STTY_STATE\"".to_string(),
        terminal_mode.to_string(),
        "end".to_string(),
        start_marker_line.to_string(),
        receiver_ready_marker_line.to_string(),
        "while read -l MEZ_COMMAND_LINE".to_string(),
        "if test \"$MEZ_COMMAND_LINE\" = \"$MEZ_COMMAND_END\"".to_string(),
    ]);
    if let Some(open_frame_check) = open_frame_check {
        lines.push(open_frame_check.to_string());
    }
    lines.extend([
        "set MEZ_COMMAND_SEEN_END 1".to_string(),
        acknowledge.to_string(),
        "break".to_string(),
        "end".to_string(),
    ]);
    if input_sidecar.is_some() {
        lines.extend([
            "switch \"$MEZ_COMMAND_LINE\"".to_string(),
            "case 'C *'".to_string(),
            format!("if test \"$MEZ_WRITE_STATUS\" -eq 0; {acknowledge_command_record}; else; {acknowledge}; end"),
            "case 'S1B *'".to_string(),
            "if test \"$MEZ_WRITE_STATUS\" -eq 0; set MEZ_SIDECAR_FRAME_FIELDS (string split ' ' -- \"$MEZ_COMMAND_LINE\"); if test (count $MEZ_SIDECAR_FRAME_FIELDS) -ne 4; or test \"$MEZ_SIDECAR_FRAME_OPEN\" -ne 0; or test \"$MEZ_SIDECAR_FRAME_FIELDS[2]\" != \"$MEZ_SIDECAR_FRAME_SEQUENCE\"; or not string match -rq '^[0-9]+$' -- \"$MEZ_SIDECAR_FRAME_FIELDS[3]\"; or test \"$MEZ_SIDECAR_FRAME_FIELDS[3]\" -gt 32768; or not string match -rq '^[0-9a-f]{64}$' -- \"$MEZ_SIDECAR_FRAME_FIELDS[4]\"; set MEZ_WRITE_STATUS 1; else; set MEZ_SIDECAR_FRAME_LENGTH $MEZ_SIDECAR_FRAME_FIELDS[3]; set MEZ_SIDECAR_FRAME_DIGEST $MEZ_SIDECAR_FRAME_FIELDS[4]; set MEZ_SIDECAR_FRAME_OPEN 1; : > \"$MEZ_SIDECAR_FRAME\"; or set MEZ_WRITE_STATUS $status; end; end".to_string(),
            "case 'S1D *'".to_string(),
            "if test \"$MEZ_WRITE_STATUS\" -eq 0; and test \"$MEZ_SIDECAR_FRAME_OPEN\" -eq 1; string replace -r '^S1D ' '' -- \"$MEZ_COMMAND_LINE\" >> \"$MEZ_SIDECAR_FRAME\"; or set MEZ_WRITE_STATUS $status; else; set MEZ_WRITE_STATUS 1; end".to_string(),
            "case 'S1E *'".to_string(),
            "if test \"$MEZ_WRITE_STATUS\" -eq 0; set MEZ_SIDECAR_FRAME_FIELDS (string split ' ' -- \"$MEZ_COMMAND_LINE\"); set MEZ_SIDECAR_FRAME_COUNT (wc -c < \"$MEZ_SIDECAR_FRAME\" | string trim); if test \"$MEZ_SIDECAR_SHA256\" = sha256sum; set MEZ_SIDECAR_FRAME_ACTUAL (sha256sum -- \"$MEZ_SIDECAR_FRAME\" | string split -f 1 ' '); else; set MEZ_SIDECAR_FRAME_ACTUAL (shasum -a 256 -- \"$MEZ_SIDECAR_FRAME\" | string split -f 1 ' '); end; if test (count $MEZ_SIDECAR_FRAME_FIELDS) -ne 2; or test \"$MEZ_SIDECAR_FRAME_OPEN\" -ne 1; or test \"$MEZ_SIDECAR_FRAME_FIELDS[2]\" != \"$MEZ_SIDECAR_FRAME_SEQUENCE\"; or test \"$MEZ_SIDECAR_FRAME_COUNT\" != \"$MEZ_SIDECAR_FRAME_LENGTH\"; or test \"$MEZ_SIDECAR_FRAME_ACTUAL\" != \"$MEZ_SIDECAR_FRAME_DIGEST\"; set MEZ_WRITE_STATUS 1; else; sed 's/^/# __MEZ_INPUT_SIDECAR_V1__ /' \"$MEZ_SIDECAR_FRAME\" >> \"$MEZ_SIDECAR_DATA\"; or set MEZ_WRITE_STATUS $status; set MEZ_SIDECAR_FRAME_SEQUENCE (math $MEZ_SIDECAR_FRAME_SEQUENCE + 1); set MEZ_SIDECAR_FRAME_OPEN 0; end; end".to_string(),
            acknowledge.to_string(),
            "case '*'".to_string(),
            "set MEZ_WRITE_STATUS 1".to_string(),
            acknowledge.to_string(),
            "end".to_string(),
        ]);
    } else {
        lines.extend([
            "if test \"$MEZ_WRITE_STATUS\" -eq 0".to_string(),
            "switch \"$MEZ_COMMAND_LINE\"".to_string(),
            "case 'C *'".to_string(),
            "string replace -r '^C ' '' -- \"$MEZ_COMMAND_LINE\" >> \"$MEZ_COMMAND_B64\"; or set MEZ_WRITE_STATUS $status".to_string(),
            "case '*'".to_string(),
            "set MEZ_WRITE_STATUS 1".to_string(),
            "end".to_string(),
            "end".to_string(),
            acknowledge.to_string(),
        ]);
    }
    lines.extend([
        "end".to_string(),
        "if test \"$MEZ_WRITE_STATUS\" -eq 0; and test \"$MEZ_COMMAND_SEEN_END\" != 1".to_string(),
        "printf '%s\\n' 'Mezzanine shell transaction command payload ended before sentinel' >&2".to_string(),
        "set MEZ_WRITE_STATUS 1".to_string(),
        "end".to_string(),
        "if test \"$MEZ_WRITE_STATUS\" -eq 0".to_string(),
        "if base64 -d < \"$MEZ_COMMAND_B64\" > \"$MEZ_COMMAND_FILE\" 2>/dev/null".to_string(),
        "set MEZ_WRITE_STATUS 0".to_string(),
        "else".to_string(),
        "base64 -D < \"$MEZ_COMMAND_B64\" > \"$MEZ_COMMAND_FILE\"".to_string(),
        "set MEZ_WRITE_STATUS $status".to_string(),
        "end".to_string(),
        "if test \"$MEZ_WRITE_STATUS\" -eq 0; and test -n \"$MEZ_SIDECAR_DATA\"; cat \"$MEZ_SIDECAR_DATA\" >> \"$MEZ_COMMAND_FILE\"; or set MEZ_WRITE_STATUS $status; end".to_string(),
        "else".to_string(),
        "set MEZ_WRITE_STATUS 1".to_string(),
        "end".to_string(),
        "end".to_string(),
    ]);
    lines.extend([
        "if test -n \"$MEZ_STTY_STATE\"".to_string(),
        "stty \"$MEZ_STTY_STATE\" 2>/dev/null; or true".to_string(),
        "set MEZ_STTY_STATE ''".to_string(),
        "end".to_string(),
    ]);
    CommandMaterialization {
        setup: lines.join("\n") + "\n",
        payload: command_payload_lines(command, &end_marker, input_sidecar),
    }
}

/// Returns a sentinel line that cannot be mistaken for standard base64 data.
fn command_payload_end_marker(marker: &str) -> String {
    format!("__MEZ_COMMAND_PAYLOAD_END_{marker}__")
}

/// Appends version-one logical sidecar frames to a receiver payload.
fn append_framed_sidecar_payload(payload: &mut String, input_sidecar: &str) {
    let mut sequence = 0usize;
    let mut frame = String::new();
    for record in input_sidecar.split_inclusive('\n') {
        if !frame.is_empty()
            && frame.len().saturating_add(record.len()) > SHELL_TRANSACTION_SIDECAR_FRAME_BYTES
        {
            append_sidecar_frame(payload, sequence, &frame);
            sequence = sequence.saturating_add(1);
            frame.clear();
        }
        frame.push_str(record);
    }
    if !frame.is_empty() {
        append_sidecar_frame(payload, sequence, &frame);
    }
}

/// Appends one sequenced frame as canonical-safe physical records.
fn append_sidecar_frame(payload: &mut String, sequence: usize, frame: &str) {
    let digest = Sha256::digest(frame.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    payload.push_str(&format!("S1B {sequence} {} {digest}\n", frame.len()));
    for record in frame.lines() {
        payload.push_str("S1D ");
        payload.push_str(record);
        payload.push('\n');
    }
    payload.push_str(&format!("S1E {sequence}\n"));
}

/// Renders the base64 command payload consumed by the transaction receiver.
fn command_payload_lines(command: &str, end_marker: &str, input_sidecar: Option<&str>) -> String {
    let mut command_source = command.to_string();
    if !command_source.ends_with('\n') {
        command_source.push('\n');
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(command_source.as_bytes());
    let mut payload = String::new();
    for chunk in encoded
        .as_bytes()
        .chunks(SHELL_TRANSACTION_COMMAND_BASE64_LINE_BYTES)
    {
        let chunk = std::str::from_utf8(chunk)
            .expect("standard base64 output should always be valid UTF-8");
        payload.push_str("C ");
        payload.push_str(chunk);
        payload.push('\n');
    }
    if let Some(input_sidecar) = input_sidecar {
        append_framed_sidecar_payload(&mut payload, input_sidecar);
    }
    payload.push_str(end_marker);
    payload.push('\n');
    payload
}

/// Fish function installed before interactive transaction delivery begins.
///
/// The interactive reader sees only one short function invocation. The
/// function then owns stdin while it receives bounded base64 records, writes
/// the decoded wrapper to a temporary file, and sources that file without
/// command substitution so physical newlines remain intact. Each record emits
/// the acknowledgement byte used by paced Darwin PTY delivery.
pub fn fish_wrapper_receiver_init_command() -> &'static str {
    r#"function __mez_agent_wrapper_receive --argument-names sentinel
    set -l source_file (mktemp); or return 1
    set -l encoded_file "$source_file.b64"
    set -l receiver_stty (stty -g 2>/dev/null); or set receiver_stty ''
    if test -n "$receiver_stty"
        stty -echo 2>/dev/null; or true
    end
    set -l receive_status 0
    set -l seen_end 0
    command printf '' > "$encoded_file"; or set receive_status $status
    builtin history delete --exact --case-sensitive "__mez_agent_wrapper_receive '$sentinel'" >/dev/null 2>&1
    printf '\036'
    while read -l record
        set -l payload (string split -m 1 ';' -- "$record")[1]
        if test "$payload" = "$sentinel"
            set seen_end 1
            printf '\036'
            break
        end
        if test "$receive_status" -eq 0
            printf '%s' "$payload" >> "$encoded_file"; or set receive_status $status
        end
        printf '\036'
    end
    if test "$seen_end" != 1
        set receive_status 1
    end
    set -l decode_status 1
    if test "$receive_status" -eq 0
        if base64 -d < "$encoded_file" > "$source_file" 2>/dev/null
            set decode_status 0
        else
            base64 -D < "$encoded_file" > "$source_file"
            set decode_status $status
        end
    end
    if test -n "$receiver_stty"
        stty "$receiver_stty" 2>/dev/null; or true
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

/// Admission trigger and authenticated source frames for managed Bash.
struct BashPrivateReceiverTransport {
    /// Non-newline Readline trigger followed by source-free admission metadata.
    trigger: String,
    /// Bounded, sequenced source records delivered after receiver-ready.
    payload: String,
}

/// Renders Bash source for the managed private Readline receiver.
///
/// The trigger is a bound control byte rather than a newline-terminated
/// command, so Bash never admits generated source into ordinary history. The
/// admission record contains no generated source. Runtime must wait for the
/// receiver-ready event before delivering the bounded source records.
fn bash_private_receiver_transport(
    source: &str,
    classification: ShellClassification,
    token: Option<&MarkerToken>,
    marker: &str,
) -> Option<BashPrivateReceiverTransport> {
    if classification != ShellClassification::Bash {
        return None;
    }
    let token = token?;
    let source = format!("unset MEZ_WRAPPER_SOURCE\n{source}");
    let digest = Sha256::digest(source.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let encoded = base64::engine::general_purpose::STANDARD.encode(source.as_bytes());
    let chunks = encoded.as_bytes().chunks(SHELL_WRAPPER_BASE64_LINE_BYTES);
    let chunk_count = chunks.len();
    let trigger = format!(
        "\x07MEZ_BASH_RX1_BEGIN {} {} {} {} {}\n",
        token.as_str(),
        marker,
        source.len(),
        digest,
        chunk_count
    );
    let mut payload = String::new();
    for (sequence, chunk) in chunks.enumerate() {
        let chunk = std::str::from_utf8(chunk)
            .expect("standard base64 output should always be valid UTF-8");
        payload.push_str(&format!(
            "MEZ_BASH_RX1_DATA {} {} {} {}\n",
            token.as_str(),
            marker,
            sequence,
            chunk
        ));
    }
    payload.push_str(&format!(
        "MEZ_BASH_RX1_END {} {} {} {} {}\n",
        token.as_str(),
        marker,
        chunk_count,
        source.len(),
        digest
    ));
    Some(BashPrivateReceiverTransport { trigger, payload })
}

/// Renders arbitrary runtime-owned Bash source for private receiver admission.
///
/// The returned wrapper contains only the non-newline admission trigger. The
/// authenticated source frames remain separate in `receiver_payload` so the
/// runtime can retain them until receiver-ready while holding its input lease.
pub fn bash_private_source_input(
    source: &str,
    token: &MarkerToken,
    marker: &str,
) -> ShellTransactionInput {
    let transport =
        bash_private_receiver_transport(source, ShellClassification::Bash, Some(token), marker)
            .expect("explicit Bash private source rendering requires a receiver transport");
    ShellTransactionInput {
        wrapper: transport.trigger,
        receiver_payload: transport.payload,
        payload: String::new(),
        payload_receiver_acknowledgements: true,
    }
}

/// Starts a zsh-private history frame before any generated transport record.
///
/// A pane startup hook rejects this exact token-bearing record before zsh adds
/// it to immediate or shared history. The record itself then pushes the private
/// frame that owns all subsequent wrapper transport records.
fn zsh_history_transport_start(
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
fn posix_shell_history_transport_fallback(
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

/// Formats transaction-local environment assignments for Fish shell wrappers.
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
fn posix_shell_history_suppression_start_for_classification(
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
fn posix_shell_history_file_restore() -> &'static str {
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
fn posix_shell_history_marker_finish_prefix_for_classification(
    _classification: ShellClassification,
) -> String {
    posix_shell_history_marker_finish_prefix().to_string()
}

/// Preserves strict POSIX shell options and terminal echo state.
///
/// Managed Bash and zsh transports establish their history boundary before
/// this source executes, so transaction-local source must not mutate history.
fn posix_shell_state_suppression_start() -> &'static str {
    "MEZ_SHELL_STTY_STATE=$(stty -g 2>/dev/null) || MEZ_SHELL_STTY_STATE=; if [ -n \"$MEZ_SHELL_STTY_STATE\" ]; then stty -echo 2>/dev/null || :; fi; MEZ_RESTORE_ERREXIT=0; case $- in *e*) MEZ_RESTORE_ERREXIT=1; set +e;; esac; MEZ_RESTORE_NOUNSET=0; case $- in *u*) MEZ_RESTORE_NOUNSET=1; set +u;; esac\n"
}

/// Restores state-only transaction setup immediately before completion.
fn posix_shell_state_marker_finish_prefix() -> &'static str {
    "MEZ_RESTORE_ERREXIT_NOW=$MEZ_RESTORE_ERREXIT\n\
MEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET\n\
unset MEZ_RESTORE_ERREXIT MEZ_RESTORE_NOUNSET\n\
if [ -n \"$MEZ_SHELL_STTY_STATE\" ]; then stty \"$MEZ_SHELL_STTY_STATE\" 2>/dev/null || :; fi\n\
unset MEZ_SHELL_STTY_STATE\n"
}

/// Returns the zsh-compatible transaction prologue.
fn zsh_shell_history_suppression_start() -> &'static str {
    posix_shell_state_suppression_start()
}

/// Restores zsh's prior history context before the completion marker.
fn zsh_shell_history_marker_finish_prefix(token: Option<&MarkerToken>) -> String {
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
fn posix_shell_errexit_restore_suffix() -> &'static str {
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

/// Validates model-authored shell input before Mezzanine wraps it for pane
/// execution.
///
/// Model-authored heredoc and here-string redirections are disabled because
/// they are easy to leave unterminated and can strand the shell transaction.
/// Runtime-generated wrappers use bounded shell syntax and base64 command
/// materialization instead. Filesystem effects from other shell syntax are
/// evaluated by the permission policy and sandbox layers.
pub fn validate_agent_authored_shell_command(command: &str) -> AgentShellValidationResult<()> {
    if shell_command_contains_unquoted_heredoc(command) {
        return Err(AgentShellValidationError::invalid_args(
            "shell_command heredoc redirection is disabled for agent-authored commands",
        ));
    }
    Ok(())
}

/// Returns whether a shell command contains an unquoted heredoc or here-string
/// redirection token.
///
/// This is a conservative lexical scan. It ignores tokens inside single and
/// double quoted strings and comments, while treating any unquoted `<<`, `<<-`,
/// or `<<<` occurrence as disabled shell input.
pub fn shell_command_contains_unquoted_heredoc(command: &str) -> bool {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ScanState {
        Normal,
        SingleQuoted,
        DoubleQuoted,
    }

    let mut chars = command.chars().peekable();
    let mut state = ScanState::Normal;
    while let Some(ch) = chars.next() {
        match state {
            ScanState::Normal => match ch {
                '\\' => {
                    let _ = chars.next();
                }
                '\'' => state = ScanState::SingleQuoted,
                '"' => state = ScanState::DoubleQuoted,
                '#' => {
                    for comment_ch in chars.by_ref() {
                        if comment_ch == '\n' {
                            break;
                        }
                    }
                }
                '<' if chars.peek() == Some(&'<') => return true,
                _ => {}
            },
            ScanState::SingleQuoted => {
                if ch == '\'' {
                    state = ScanState::Normal;
                }
            }
            ScanState::DoubleQuoted => match ch {
                '\\' => {
                    let _ = chars.next();
                }
                '"' => state = ScanState::Normal,
                _ => {}
            },
        }
    }
    false
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

/// Renders the shell line used to enter an agent-mode subshell.
///
/// The command starts the configured shell in the pane's current working
/// directory and resumes the parent shell only after the child exits. The
/// parent wrapper suppresses shell history before launching the child so the
/// Mezzanine-owned handoff does not persist in the user's normal history. It
/// defines the handoff as a multiline function before invoking it, keeping
/// physical terminal lines within the canonical-input limits of Unix PTYs
/// while ensuring the parent parses cleanup before the child can consume input.
pub fn agent_subshell_enter_command(
    shell_path: &Path,
    classification: ShellClassification,
) -> AgentShellValidationResult<String> {
    agent_subshell_enter_command_with_zsh_history_token(shell_path, classification, None)
}

/// Renders an agent-mode subshell handoff with optional managed zsh history isolation.
///
/// The token must match the pane startup hook. When present for zsh, the
/// complete parent-shell transport enters a private history frame before any
/// wrapper records are submitted and restores the prior frame after the child
/// shell exits.
pub fn agent_subshell_enter_command_with_zsh_history_token(
    shell_path: &Path,
    classification: ShellClassification,
    zsh_history_token: Option<&MarkerToken>,
) -> AgentShellValidationResult<String> {
    agent_subshell_enter_command_with_shell_compatibility(
        shell_path,
        classification,
        zsh_history_token,
        None,
        None,
    )
}

/// Renders an agent-mode subshell handoff with managed shell startup state.
///
/// The optional Bash receiver rcfile is retained only for a pane that owns the
/// private receiver. Callers without that pane-scoped compatibility state keep
/// the normal startup-suppressed Bash child behavior.
pub fn agent_subshell_enter_command_with_shell_compatibility(
    shell_path: &Path,
    classification: ShellClassification,
    zsh_history_token: Option<&MarkerToken>,
    bash_receiver_rcfile: Option<&Path>,
    bash_receiver_install_marker: Option<&str>,
) -> AgentShellValidationResult<String> {
    if !shell_path.is_absolute() {
        return Err(AgentShellValidationError::invalid_args(
            "agent subshell requires an absolute resolved shell path",
        ));
    }
    let shell = shell_path.to_string_lossy();
    let source = if classification == ShellClassification::Fish {
        let env_words = fish_agent_subshell_env_word_list().join(" \\\n  ");
        let shell_invocation = fish_shell_interactive_invocation_words(&shell, classification);
        format!(
            "begin
{history_start}
if test -n \"$MEZ_SHELL_STTY_STATE\"; stty \"$MEZ_SHELL_STTY_STATE\" 2>/dev/null; or true; end
command env \\
  {env_words} \\
  {shell_invocation}
{history_restore}
end
",
            history_start = fish_shell_history_suppression_start(),
            history_restore = fish_shell_history_restore(),
            env_words = env_words,
            shell_invocation = shell_invocation,
        )
    } else if classification == ShellClassification::Zsh && zsh_history_token.is_some() {
        let env_words =
            posix_agent_subshell_env_word_list_for_classification(classification).join(" \\\n  ");
        let shell_invocation = posix_shell_interactive_invocation_words_with_startup_suppression(
            &shell,
            classification,
            false,
        );
        format!(
            "{history_start}__mez_agent_subshell_handoff() {{
if [ -n \"$MEZ_SHELL_STTY_STATE\" ]; then stty \"$MEZ_SHELL_STTY_STATE\" 2>/dev/null || :; fi
unset MEZ_SHELL_STTY_STATE
command env \\
  {env_words} \\
  {shell_invocation}
:
}}
__mez_agent_subshell_handoff; unset -f __mez_agent_subshell_handoff 2>/dev/null || :
{history_finish}{errexit_restore}
",
            history_start = zsh_shell_history_suppression_start(),
            history_finish = zsh_shell_history_marker_finish_prefix(zsh_history_token),
            errexit_restore = posix_shell_errexit_restore_suffix(),
            env_words = env_words,
            shell_invocation = shell_invocation,
        )
    } else {
        let env_words = posix_agent_subshell_env_word_list().join(" \\\n  ");
        let shell_invocation = posix_shell_interactive_invocation_words_with_bash_receiver(
            &shell,
            classification,
            bash_receiver_rcfile,
            bash_receiver_install_marker,
        );
        let managed_bash =
            classification == ShellClassification::Bash && bash_receiver_rcfile.is_some();
        let history_start = if managed_bash {
            posix_shell_state_suppression_start()
        } else {
            posix_shell_history_suppression_start()
        };
        let history_cleanup = if managed_bash {
            "MEZ_RESTORE_ERREXIT_NOW=$MEZ_RESTORE_ERREXIT\n\
MEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET\n\
unset MEZ_RESTORE_ERREXIT MEZ_RESTORE_NOUNSET"
        } else {
            "if [ \"$MEZ_HISTORY_HISTFILE_WAS_SET\" = 1 ]; then HISTFILE=$MEZ_HISTORY_HISTFILE_SAVED; else unset HISTFILE; fi\n\
MEZ_RESTORE_HISTORY_NOW=$MEZ_HISTORY_RESTORE\n\
MEZ_RESTORE_ERREXIT_NOW=$MEZ_RESTORE_ERREXIT\n\
MEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET\n\
unset MEZ_HISTORY_RESTORE MEZ_HISTORY_HISTFILE_WAS_SET MEZ_HISTORY_HISTFILE_SAVED MEZ_RESTORE_ERREXIT MEZ_RESTORE_NOUNSET"
        };
        let history_restore = if managed_bash {
            ""
        } else {
            "if [ \"${MEZ_RESTORE_HISTORY_NOW:-0}\" = 1 ]; then set -o history 2>/dev/null || :; fi\n"
        };
        format!(
            "{history_start}__mez_agent_subshell_handoff() {{
if [ -n \"$MEZ_SHELL_STTY_STATE\" ]; then stty \"$MEZ_SHELL_STTY_STATE\" 2>/dev/null || :; fi
unset MEZ_SHELL_STTY_STATE
command env \\
  {env_words} \\
  {shell_invocation}
:
}}
__mez_agent_subshell_cleanup() {{
{history_cleanup}
:
}}
__mez_agent_subshell_restore_options() {{
{history_restore}MEZ_RESTORE_ERREXIT_APPLY=${{MEZ_RESTORE_ERREXIT_NOW:-0}}
MEZ_RESTORE_NOUNSET_APPLY=${{MEZ_RESTORE_NOUNSET_NOW:-0}}
unset MEZ_RESTORE_HISTORY_NOW MEZ_RESTORE_ERREXIT_NOW MEZ_RESTORE_NOUNSET_NOW
case \"$MEZ_RESTORE_ERREXIT_APPLY\" in 1) set -e;; esac
case \"$MEZ_RESTORE_NOUNSET_APPLY\" in 1) set -u;; esac
unset MEZ_RESTORE_ERREXIT_APPLY MEZ_RESTORE_NOUNSET_APPLY
:
}}
__mez_agent_subshell_handoff; __mez_agent_subshell_cleanup; unset -f __mez_agent_subshell_handoff __mez_agent_subshell_cleanup 2>/dev/null || :; __mez_agent_subshell_restore_options; unset -f __mez_agent_subshell_restore_options 2>/dev/null || :
",
            history_start = history_start,
            history_cleanup = history_cleanup,
            history_restore = history_restore,
            env_words = env_words,
            shell_invocation = shell_invocation,
        )
    };
    if classification == ShellClassification::Fish
        || (classification == ShellClassification::Bash && bash_receiver_rcfile.is_some())
    {
        Ok(source)
    } else {
        Ok(posix_shell_wrapper_transport(
            &source,
            classification,
            zsh_history_token,
        ))
    }
}
