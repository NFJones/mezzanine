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
    /// Shell wrapper source that defines and invokes the transaction receiver.
    pub wrapper: String,
    /// Base64 command payload consumed by the receiver after it starts.
    pub payload: String,
}

impl ShellTransactionInput {
    /// Returns the total byte length of all pane input for this transaction.
    pub fn len(&self) -> usize {
        self.wrapper.len().saturating_add(self.payload.len())
    }

    /// Reports whether this rendered transaction contains no bytes.
    pub fn is_empty(&self) -> bool {
        self.wrapper.is_empty() && self.payload.is_empty()
    }

    /// Combines wrapper and payload into one interactive-shell input string.
    pub fn combined(&self) -> String {
        let mut combined = String::with_capacity(self.len());
        combined.push_str(&self.wrapper);
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
    ("HISTFILE", "/dev/null"),
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
/// remote panes.
pub const SHELL_TRANSACTION_COMMAND_BASE64_LINE_BYTES: usize = 768;
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
            "    command python3 -c 'import os,sys; os.setsid(); os.execvp(sys.argv[1], sys.argv[1:])'",
            child_env,
            shell_invocation,
            transport,
            status_fd,
        ),
        "  elif command -v perl >/dev/null 2>&1; then".to_string(),
        posix_child_command_line(
            "    command perl -MPOSIX=setsid -e 'setsid(); exec @ARGV'",
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
                SHELL_OUTPUT_BASE64_MAX_RAW_BYTES,
                SHELL_OUTPUT_BASE64_MAX_RAW_BYTES
            ),
            format!(
                "    dd if=\"$MEZ_OUTPUT_FILE\" bs={} count=1 2>/dev/null | base64",
                SHELL_OUTPUT_BASE64_MAX_RAW_BYTES
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
            "    command python3 -c 'import os,sys; os.setsid(); os.execvp(sys.argv[1], sys.argv[1:])' env",
            noninteractive_env,
            shell_invocation,
            transport,
            status_fd,
        ),
        "else if command -q perl".to_string(),
        fish_child_command_line(
            "    command perl -MPOSIX=setsid -e 'setsid(); exec @ARGV' env",
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
                SHELL_OUTPUT_BASE64_MAX_RAW_BYTES
            ),
            format!(
                "set MEZ_OUTPUT_DROPPED (math \"$MEZ_OUTPUT_BYTES - {}\")",
                SHELL_OUTPUT_BASE64_MAX_RAW_BYTES
            ),
            "else".to_string(),
            "set MEZ_OUTPUT_DROPPED 0".to_string(),
            "end".to_string(),
            format!(
                "command dd if=\"$MEZ_OUTPUT_FILE\" bs={} count=1 2>/dev/null | base64",
                SHELL_OUTPUT_BASE64_MAX_RAW_BYTES
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
            ShellChildArgument::Literal(value) => shell_quote(value),
            ShellChildArgument::MaterializedCommandFile => "\"$MEZ_COMMAND_FILE\"".to_string(),
        }))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Renders one typed child launch as Fish shell words.
fn fish_typed_child_launch_words(launch: &ShellChildLaunch) -> String {
    std::iter::once(fish_quote(&launch.executable))
        .chain(launch.arguments.iter().map(|argument| match argument {
            ShellChildArgument::Literal(value) => fish_quote(value),
            ShellChildArgument::MaterializedCommandFile => "\"$MEZ_COMMAND_FILE\"".to_string(),
        }))
        .collect::<Vec<_>>()
        .join(" ")
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
            child_launch: None,
            output_transport: ShellTransactionOutputTransport::Raw,
        })
    }

    /// Selects a validated typed child process launch for this transaction.
    pub fn with_child_launch(mut self, child_launch: ShellChildLaunch) -> Self {
        self.child_launch = Some(child_launch);
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
        let function_name = transaction_function_name(self.marker.as_str());
        let command_materialization = posix_command_file_materialization(
            &self.command,
            self.marker.as_str(),
            "command printf '\\033]133;C;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \"$MEZ_MARKER_TOKEN\" \"$MEZ_TURN\" \"$MEZ_AGENT\" \"$MEZ_PANE\"",
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
            &child_env,
            &shell_invocation,
            self.child_launch
                .as_ref()
                .and_then(|launch| launch.status_fd),
        );
        let wrapper = format!(
            "{history_start}\
{function_name}() {{\n\
MEZ_MARKER_TOKEN={marker}\n\
MEZ_TURN={turn}\n\
MEZ_AGENT={agent}\n\
MEZ_PANE={pane}\n\
{command_file_lines}\
{child_invocation}\
if [ -n \"$MEZ_COMMAND_FILE\" ]; then command rm -f -- \"$MEZ_COMMAND_FILE\" >/dev/null 2>&1 || :; fi\n\
if [ -n \"$MEZ_COMMAND_B64\" ]; then command rm -f -- \"$MEZ_COMMAND_B64\" >/dev/null 2>&1 || :; fi\n\
if [ -n \"$MEZ_OUTPUT_FILE\" ]; then command rm -f -- \"$MEZ_OUTPUT_FILE\" >/dev/null 2>&1 || :; fi\n\
if [ -n \"$MEZ_STATUS_FILE\" ]; then command rm -f -- \"$MEZ_STATUS_FILE\" >/dev/null 2>&1 || :; fi\n\
unset MEZ_COMMAND_FILE MEZ_COMMAND_B64 MEZ_COMMAND_END MEZ_COMMAND_LINE MEZ_COMMAND_SEEN_END MEZ_OUTPUT_FILE MEZ_STATUS_FILE MEZ_STTY_STATE MEZ_WRITE_STATUS\n\
unset -f {function_name} 2>/dev/null || :\n\
{history_restore}\
{history_marker_finish}command printf '\\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
\"$MEZ_STATUS\" \"$MEZ_MARKER_TOKEN\" \"$MEZ_TURN\" \"$MEZ_AGENT\" \"$MEZ_PANE\"; \
unset MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS; {errexit_restore}\n\
}}\n\
{function_name}\n",
            history_start = posix_shell_history_suppression_start(),
            history_restore = posix_shell_history_file_restore(),
            history_marker_finish = posix_shell_history_marker_finish_prefix(),
            errexit_restore = posix_shell_errexit_restore_suffix(),
            function_name = function_name,
            marker = shell_quote(self.marker.as_str()),
            turn = shell_quote(&self.turn_id),
            agent = shell_quote(&self.agent_id),
            pane = shell_quote(&self.pane_id),
            command_file_lines = command_materialization.setup,
            child_invocation = child_invocation,
        );
        ShellTransactionInput {
            wrapper,
            payload: command_materialization.payload,
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
        let function_name = transaction_function_name(self.marker.as_str());
        format!(
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
            history_start = posix_shell_history_suppression_start(),
            history_restore = posix_shell_history_file_restore(),
            history_marker_finish = posix_shell_history_marker_finish_prefix(),
            errexit_restore = posix_shell_errexit_restore_suffix(),
            function_name = function_name,
            marker = shell_quote(self.marker.as_str()),
            turn = shell_quote(&self.turn_id),
            agent = shell_quote(&self.agent_id),
            pane = shell_quote(&self.pane_id),
            command = self.command,
        )
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
        if classification == ShellClassification::Fish {
            self.render_fish_stateful()
        } else {
            self.render_stateful()
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
        let command_materialization = fish_command_file_materialization(
            &self.command,
            self.marker.as_str(),
            "printf '\\033]133;C;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' $MEZ_MARKER_TOKEN $MEZ_TURN $MEZ_AGENT $MEZ_PANE",
        );
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
            &child_env,
            &shell_invocation,
            self.child_launch
                .as_ref()
                .and_then(|launch| launch.status_fd),
        );
        let wrapper = format!(
            "{history_start}\
begin\n\
set -l MEZ_MARKER_TOKEN {marker}\n\
set -l MEZ_TURN {turn}\n\
set -l MEZ_AGENT {agent}\n\
set -l MEZ_PANE {pane}\n\
{command_file_lines}\
set -l MEZ_STATUS 0\n\
{child_invocation}\
if test -n \"$MEZ_COMMAND_FILE\"; command rm -f -- \"$MEZ_COMMAND_FILE\" >/dev/null 2>&1; or true; end\n\
if test -n \"$MEZ_COMMAND_B64\"; command rm -f -- \"$MEZ_COMMAND_B64\" >/dev/null 2>&1; or true; end\n\
if test -n \"$MEZ_OUTPUT_FILE\"; command rm -f -- \"$MEZ_OUTPUT_FILE\" >/dev/null 2>&1; or true; end\n\
if test -n \"$MEZ_STATUS_FILE\"; command rm -f -- \"$MEZ_STATUS_FILE\" >/dev/null 2>&1; or true; end\n\
set -e MEZ_COMMAND_FILE MEZ_COMMAND_B64 MEZ_COMMAND_END MEZ_COMMAND_LINE MEZ_COMMAND_SEEN_END MEZ_OUTPUT_FILE MEZ_STATUS_FILE MEZ_STTY_STATE MEZ_WRITE_STATUS\n\
printf '\\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
$MEZ_STATUS $MEZ_MARKER_TOKEN $MEZ_TURN $MEZ_AGENT $MEZ_PANE\n\
{history_restore}\
set -e MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS\n\
end\n",
            history_start = fish_shell_history_suppression_start(),
            history_restore = fish_shell_history_restore(),
            marker = fish_quote(self.marker.as_str()),
            turn = fish_quote(&self.turn_id),
            agent = fish_quote(&self.agent_id),
            pane = fish_quote(&self.pane_id),
            command_file_lines = command_materialization.setup,
            child_invocation = child_invocation,
        );
        ShellTransactionInput {
            wrapper,
            payload: command_materialization.payload,
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
printf '\\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
$MEZ_STATUS $MEZ_MARKER_TOKEN $MEZ_TURN $MEZ_AGENT $MEZ_PANE\n\
{history_restore}\
set -e MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS\n\
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
    marker: &str,
    start_marker_line: &str,
) -> CommandMaterialization {
    let end_marker = command_payload_end_marker(marker);
    let mut lines = vec![
        "MEZ_COMMAND_FILE=$(mktemp) || MEZ_COMMAND_FILE=".to_string(),
        "MEZ_COMMAND_B64=".to_string(),
        format!("MEZ_COMMAND_END={}", shell_quote(&end_marker)),
        "MEZ_COMMAND_SEEN_END=0".to_string(),
        "MEZ_STTY_STATE=".to_string(),
        "MEZ_WRITE_STATUS=0".to_string(),
        "if [ -n \"$MEZ_COMMAND_FILE\" ]; then".to_string(),
        "command -v base64 >/dev/null 2>&1 || { printf '%s\\n' 'base64 is required for Mezzanine shell transaction wrappers' >&2; MEZ_WRITE_STATUS=127; }".to_string(),
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then MEZ_COMMAND_B64=$(mktemp) || MEZ_WRITE_STATUS=1; fi".to_string(),
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then : > \"$MEZ_COMMAND_B64\" || MEZ_WRITE_STATUS=$?; fi".to_string(),
    ];
    lines.extend([
        "MEZ_STTY_STATE=$(stty -g 2>/dev/null) || MEZ_STTY_STATE=".to_string(),
        "if [ -n \"$MEZ_STTY_STATE\" ]; then stty -echo 2>/dev/null || :; fi".to_string(),
        start_marker_line.to_string(),
        "while IFS= read -r MEZ_COMMAND_LINE; do".to_string(),
        "if [ \"$MEZ_COMMAND_LINE\" = \"$MEZ_COMMAND_END\" ]; then MEZ_COMMAND_SEEN_END=1; break; fi".to_string(),
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then printf '%s\\n' \"$MEZ_COMMAND_LINE\" >> \"$MEZ_COMMAND_B64\" || { MEZ_WRITE_STATUS=$?; break; }; fi".to_string(),
        "done".to_string(),
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ] && [ \"$MEZ_COMMAND_SEEN_END\" != 1 ]; then printf '%s\\n' 'Mezzanine shell transaction command payload ended before sentinel' >&2; MEZ_WRITE_STATUS=1; fi".to_string(),
        "if [ -n \"$MEZ_STTY_STATE\" ]; then stty \"$MEZ_STTY_STATE\" 2>/dev/null || :; MEZ_STTY_STATE=; fi".to_string(),
        "if [ \"$MEZ_WRITE_STATUS\" -eq 0 ]; then if base64 -d < \"$MEZ_COMMAND_B64\" > \"$MEZ_COMMAND_FILE\" 2>/dev/null; then MEZ_WRITE_STATUS=0; else base64 -D < \"$MEZ_COMMAND_B64\" > \"$MEZ_COMMAND_FILE\"; MEZ_WRITE_STATUS=$?; fi; fi".to_string(),
        "else".to_string(),
        "MEZ_WRITE_STATUS=1".to_string(),
        "fi".to_string(),
    ]);
    CommandMaterialization {
        setup: lines.join("\n") + "\n",
        payload: command_payload_lines(command, &end_marker),
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
    let mut words = vec![shell_quote(shell_path)];
    words.extend(
        startup_suppression_args(classification)
            .iter()
            .map(|arg| (*arg).to_string()),
    );
    words.join(" ")
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
    words.extend(
        startup_suppression_args(classification)
            .iter()
            .map(|arg| (*arg).to_string()),
    );
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
    marker: &str,
    start_marker_line: &str,
) -> CommandMaterialization {
    let end_marker = command_payload_end_marker(marker);
    let mut lines = vec![
        "set -l MEZ_COMMAND_FILE (mktemp); or set -l MEZ_COMMAND_FILE ''".to_string(),
        "set -l MEZ_COMMAND_B64 ''".to_string(),
        format!("set -l MEZ_COMMAND_END {}", fish_quote(&end_marker)),
        "set -l MEZ_COMMAND_SEEN_END 0".to_string(),
        "set -l MEZ_STTY_STATE ''".to_string(),
        "set -l MEZ_WRITE_STATUS 0".to_string(),
        "if test -n \"$MEZ_COMMAND_FILE\"".to_string(),
        "command -q base64; or begin; printf '%s\\n' 'base64 is required for Mezzanine shell transaction wrappers' >&2; set MEZ_WRITE_STATUS 127; end".to_string(),
        "if test \"$MEZ_WRITE_STATUS\" -eq 0; set MEZ_COMMAND_B64 (mktemp); or set MEZ_WRITE_STATUS 1; end".to_string(),
        "if test \"$MEZ_WRITE_STATUS\" -eq 0; : > \"$MEZ_COMMAND_B64\"; or set MEZ_WRITE_STATUS $status; end".to_string(),
    ];
    lines.extend([
        "set MEZ_STTY_STATE (stty -g 2>/dev/null); or set MEZ_STTY_STATE ''".to_string(),
        "if test -n \"$MEZ_STTY_STATE\"".to_string(),
        "stty -echo 2>/dev/null; or true".to_string(),
        "end".to_string(),
        start_marker_line.to_string(),
        "while read -l MEZ_COMMAND_LINE".to_string(),
        "if test \"$MEZ_COMMAND_LINE\" = \"$MEZ_COMMAND_END\"".to_string(),
        "set MEZ_COMMAND_SEEN_END 1".to_string(),
        "break".to_string(),
        "end".to_string(),
        "if test \"$MEZ_WRITE_STATUS\" -eq 0".to_string(),
        "printf '%s\\n' \"$MEZ_COMMAND_LINE\" >> \"$MEZ_COMMAND_B64\"; or begin; set MEZ_WRITE_STATUS $status; break; end".to_string(),
        "end".to_string(),
        "end".to_string(),
        "if test \"$MEZ_WRITE_STATUS\" -eq 0; and test \"$MEZ_COMMAND_SEEN_END\" != 1".to_string(),
        "printf '%s\\n' 'Mezzanine shell transaction command payload ended before sentinel' >&2".to_string(),
        "set MEZ_WRITE_STATUS 1".to_string(),
        "end".to_string(),
        "if test -n \"$MEZ_STTY_STATE\"".to_string(),
        "stty \"$MEZ_STTY_STATE\" 2>/dev/null; or true".to_string(),
        "set MEZ_STTY_STATE ''".to_string(),
        "end".to_string(),
        "if test \"$MEZ_WRITE_STATUS\" -eq 0".to_string(),
        "if base64 -d < \"$MEZ_COMMAND_B64\" > \"$MEZ_COMMAND_FILE\" 2>/dev/null".to_string(),
        "set MEZ_WRITE_STATUS 0".to_string(),
        "else".to_string(),
        "base64 -D < \"$MEZ_COMMAND_B64\" > \"$MEZ_COMMAND_FILE\"".to_string(),
        "set MEZ_WRITE_STATUS $status".to_string(),
        "end".to_string(),
        "else".to_string(),
        "set MEZ_WRITE_STATUS 1".to_string(),
        "end".to_string(),
    ]);
    CommandMaterialization {
        setup: lines.join("\n") + "\n",
        payload: command_payload_lines(command, &end_marker),
    }
}

/// Returns a sentinel line that cannot be mistaken for standard base64 data.
fn command_payload_end_marker(marker: &str) -> String {
    format!("__MEZ_COMMAND_PAYLOAD_END_{marker}__")
}

/// Renders the base64 command payload consumed by the transaction receiver.
fn command_payload_lines(command: &str, end_marker: &str) -> String {
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
        payload.push_str(chunk);
        payload.push('\n');
    }
    payload.push_str(end_marker);
    payload.push('\n');
    payload
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
fn posix_agent_subshell_env_words() -> String {
    let mut words = AGENT_SHELL_STARTUP_ENV_UNSETS
        .iter()
        .map(|key| format!("-u {key}"))
        .collect::<Vec<_>>();
    words.extend(
        AGENT_SUBSHELL_PROMPT_ENV
            .iter()
            .map(|(key, value)| format!("{key}={}", shell_quote(value))),
    );
    words.join(" ")
}

/// Formats transaction-local environment words for a Fish persistent agent
/// subshell.
fn fish_agent_subshell_env_words() -> String {
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
    words.join(" ")
}

/// Formats transaction-local environment assignments for Fish shell wrappers.
/// Returns a POSIX-compatible prologue that suppresses shell history and
/// preserves `errexit` before Mezzanine injects wrapper lines into a pane shell.
///
/// The first command is deliberately a single line: Bash-like shells add a line
/// to history before executing it, so the prologue disables history and deletes
/// that current history entry before later wrapper lines are read.
pub fn posix_shell_history_suppression_start() -> &'static str {
    "MEZ_RESTORE_ERREXIT=0; case $- in *e*) MEZ_RESTORE_ERREXIT=1; set +e;; esac; MEZ_RESTORE_NOUNSET=0; case $- in *u*) MEZ_RESTORE_NOUNSET=1; set +u;; esac; MEZ_HISTORY_RESTORE=0; case \"$(set -o 2>/dev/null | command awk '$1==\"history\"{print $2; exit}')\" in on) MEZ_HISTORY_RESTORE=1; set +o history 2>/dev/null || :; history -d $((HISTCMD-1)) 2>/dev/null || :;; esac\n\
MEZ_HISTORY_HISTFILE_WAS_SET=0\n\
if [ \"${HISTFILE+x}\" = x ]; then MEZ_HISTORY_HISTFILE_WAS_SET=1; MEZ_HISTORY_HISTFILE_SAVED=$HISTFILE; fi\n\
HISTFILE=/dev/null\n"
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
if [ \"$MEZ_RESTORE_HISTORY_NOW\" = 1 ]; then set -o history 2>/dev/null || :; fi; "
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

/// Returns a Fish-native prologue that asks Fish to avoid writing Mez-injected
/// wrapper commands to the user's normal fish history.
///
/// Fish history behavior differs by version, so this uses private-mode state
/// plus later best-effort deletion of wrapper prefixes.
pub(crate) fn fish_shell_history_suppression_start() -> &'static str {
    "set -l MEZ_FISH_PRIVATE_WAS_SET 0\n\
if set -q fish_private_mode\n\
  set MEZ_FISH_PRIVATE_WAS_SET 1\n\
  set -l MEZ_FISH_PRIVATE_SAVED $fish_private_mode\n\
end\n\
set -g fish_private_mode 1\n"
}

/// Returns Fish-native cleanup that removes known Mez wrapper prefixes from
/// Fish history and restores the previous private-mode variable state.
pub(crate) fn fish_shell_history_restore() -> &'static str {
    "history delete --prefix --case-sensitive 'set -l MEZ_MARKER_TOKEN' >/dev/null 2>&1\n\
history delete --prefix --case-sensitive 'set -l MEZ_TURN' >/dev/null 2>&1\n\
history delete --prefix --case-sensitive 'set -l MEZ_AGENT' >/dev/null 2>&1\n\
history delete --prefix --case-sensitive 'set -l MEZ_PANE' >/dev/null 2>&1\n\
history delete --prefix --case-sensitive \"printf '\\\\033]133;\" >/dev/null 2>&1\n\
history delete --prefix --case-sensitive 'history delete --' >/dev/null 2>&1\n\
if test \"$MEZ_FISH_PRIVATE_WAS_SET\" = 1\n\
  set -g fish_private_mode $MEZ_FISH_PRIVATE_SAVED\n\
else\n\
  set -e fish_private_mode\n\
end\n\
set -e MEZ_FISH_PRIVATE_WAS_SET MEZ_FISH_PRIVATE_SAVED\n"
}

/// Validates model-authored shell input before Mezzanine wraps it for pane
/// execution.
///
/// Model-authored heredoc and here-string redirections are disabled because
/// they are easy to leave unterminated and can strand the shell transaction.
/// Runtime-generated wrappers use bounded shell syntax and base64 command
/// materialization instead. Models should use `apply_patch` for structured file
/// content changes.
pub fn validate_agent_authored_shell_command(command: &str) -> AgentShellValidationResult<()> {
    if shell_command_contains_unquoted_heredoc(command) {
        return Err(AgentShellValidationError::invalid_args(
            "shell_command heredoc redirection is disabled for agent-authored commands; use apply_patch for file content changes",
        ));
    }
    if let Some(tool) = shell_command_invokes_semantic_action(command) {
        return Err(AgentShellValidationError::invalid_args(format!(
            "shell_command must not invoke MAAP action `{tool}` as a shell program; emit a `{tool}` action instead. MAAP actions such as apply_patch are not available as pane shell commands"
        )));
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

/// Returns a semantic MAAP action name when a model-authored shell command tries
/// to invoke that action as an unquoted or quoted command word.
///
/// The scan intentionally focuses on command position rather than ordinary
/// arguments, so commands like `rg apply_patch` remain valid while
/// `printf ... | apply_patch` is rejected before it can reach the pane shell.
pub fn shell_command_invokes_semantic_action(command: &str) -> Option<&'static str> {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ScanState {
        Normal,
        SingleQuoted,
        DoubleQuoted,
    }

    fn semantic_action_name(token: &str) -> Option<&'static str> {
        match token {
            "apply_patch" => Some("apply_patch"),
            _ => None,
        }
    }

    fn is_assignment_word(token: &str) -> bool {
        let Some((name, _)) = token.split_once('=') else {
            return false;
        };
        !name.is_empty()
            && name.chars().enumerate().all(|(index, ch)| {
                ch == '_'
                    || ch.is_ascii_alphanumeric() && index > 0
                    || ch.is_ascii_alphabetic() && index == 0
            })
    }

    fn keeps_command_position(token: &str) -> bool {
        matches!(token, "command" | "builtin" | "env")
            || token.starts_with('-')
            || is_assignment_word(token)
    }

    let mut state = ScanState::Normal;
    let mut token = String::new();
    let mut command_position = true;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        match state {
            ScanState::Normal => match ch {
                '\\' => {
                    if let Some(next) = chars.next() {
                        token.push(next);
                    }
                }
                '\'' => state = ScanState::SingleQuoted,
                '"' => state = ScanState::DoubleQuoted,
                '#' => {
                    if !token.is_empty() {
                        if command_position
                            && let Some(action) = semantic_action_name(token.as_str())
                        {
                            return Some(action);
                        }
                        command_position = command_position && keeps_command_position(&token);
                        token.clear();
                    }
                    for comment_ch in chars.by_ref() {
                        if comment_ch == '\n' {
                            command_position = true;
                            break;
                        }
                    }
                }
                '\n' | ';' | '|' | '&' => {
                    if !token.is_empty() {
                        if command_position
                            && let Some(action) = semantic_action_name(token.as_str())
                        {
                            return Some(action);
                        }
                        token.clear();
                    }
                    command_position = true;
                    if ch == '&' && chars.peek() == Some(&'&') {
                        let _ = chars.next();
                    }
                    if ch == '|' && chars.peek() == Some(&'|') {
                        let _ = chars.next();
                    }
                }
                ch if ch.is_whitespace() => {
                    if !token.is_empty() {
                        if command_position
                            && let Some(action) = semantic_action_name(token.as_str())
                        {
                            return Some(action);
                        }
                        command_position = command_position && keeps_command_position(&token);
                        token.clear();
                    }
                }
                _ => token.push(ch),
            },
            ScanState::SingleQuoted => {
                if ch == '\'' {
                    state = ScanState::Normal;
                } else {
                    token.push(ch);
                }
            }
            ScanState::DoubleQuoted => match ch {
                '\\' => {
                    if let Some(next) = chars.next() {
                        token.push(next);
                    }
                }
                '"' => state = ScanState::Normal,
                _ => token.push(ch),
            },
        }
    }

    if command_position {
        semantic_action_name(token.as_str())
    } else {
        None
    }
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
/// Mezzanine-owned handoff line does not persist in the user's normal history.
pub fn agent_subshell_enter_command(
    shell_path: &Path,
    classification: ShellClassification,
) -> AgentShellValidationResult<String> {
    if !shell_path.is_absolute() {
        return Err(AgentShellValidationError::invalid_args(
            "agent subshell requires an absolute resolved shell path",
        ));
    }
    let shell = shell_path.to_string_lossy();
    if classification == ShellClassification::Fish {
        let env_words = fish_agent_subshell_env_words();
        let shell_invocation = fish_shell_interactive_invocation_words(&shell, classification);
        Ok(format!(
            "set -l MEZ_FISH_PRIVATE_WAS_SET 0; \
if set -q fish_private_mode; set MEZ_FISH_PRIVATE_WAS_SET 1; set -l MEZ_FISH_PRIVATE_SAVED $fish_private_mode; end; \
set -g fish_private_mode 1; \
command env {env_words} {shell_invocation}; \
set -l MEZ_SUBSHELL_STATUS $status; \
history delete --prefix --case-sensitive 'command env -u BASH_ENV' >/dev/null 2>&1; \
history delete --prefix --case-sensitive 'set -l MEZ_FISH_PRIVATE_WAS_SET' >/dev/null 2>&1; \
if test \"$MEZ_FISH_PRIVATE_WAS_SET\" = 1; set -g fish_private_mode $MEZ_FISH_PRIVATE_SAVED; else; set -e fish_private_mode; end; \
set -e MEZ_FISH_PRIVATE_WAS_SET MEZ_FISH_PRIVATE_SAVED MEZ_SUBSHELL_STATUS\n",
            env_words = env_words,
            shell_invocation = shell_invocation,
        ))
    } else {
        let env_words = posix_agent_subshell_env_words();
        let shell_invocation = posix_shell_interactive_invocation_words(&shell, classification);
        Ok(format!(
            "MEZ_RESTORE_ERREXIT=0; case $- in *e*) MEZ_RESTORE_ERREXIT=1; set +e;; esac; \
MEZ_RESTORE_NOUNSET=0; case $- in *u*) MEZ_RESTORE_NOUNSET=1; set +u;; esac; \
MEZ_HISTORY_RESTORE=0; case \"$(set -o 2>/dev/null | command awk '$1==\"history\"{{print $2; exit}}')\" in on) MEZ_HISTORY_RESTORE=1; set +o history 2>/dev/null || :; history -d $((HISTCMD-1)) 2>/dev/null || :;; esac; \
MEZ_HISTORY_HISTFILE_WAS_SET=0; if [ \"${{HISTFILE+x}}\" = x ]; then MEZ_HISTORY_HISTFILE_WAS_SET=1; MEZ_HISTORY_HISTFILE_SAVED=$HISTFILE; fi; \
HISTFILE=/dev/null; command env {env_words} {shell_invocation}; MEZ_SUBSHELL_STATUS=$?; \
if [ \"$MEZ_HISTORY_HISTFILE_WAS_SET\" = 1 ]; then HISTFILE=$MEZ_HISTORY_HISTFILE_SAVED; else unset HISTFILE; fi; \
MEZ_RESTORE_HISTORY_NOW=$MEZ_HISTORY_RESTORE; MEZ_RESTORE_ERREXIT_NOW=$MEZ_RESTORE_ERREXIT; MEZ_RESTORE_NOUNSET_NOW=$MEZ_RESTORE_NOUNSET; \
unset MEZ_HISTORY_RESTORE MEZ_HISTORY_HISTFILE_WAS_SET MEZ_HISTORY_HISTFILE_SAVED MEZ_RESTORE_ERREXIT MEZ_RESTORE_NOUNSET MEZ_SUBSHELL_STATUS; \
if [ \"${{MEZ_RESTORE_HISTORY_NOW:-0}}\" = 1 ]; then set -o history 2>/dev/null || :; fi; \
MEZ_RESTORE_ERREXIT_APPLY=${{MEZ_RESTORE_ERREXIT_NOW:-0}}; MEZ_RESTORE_NOUNSET_APPLY=${{MEZ_RESTORE_NOUNSET_NOW:-0}}; \
unset MEZ_RESTORE_HISTORY_NOW MEZ_RESTORE_ERREXIT_NOW MEZ_RESTORE_NOUNSET_NOW; \
case \"$MEZ_RESTORE_ERREXIT_APPLY\" in 1) set -e;; esac; case \"$MEZ_RESTORE_NOUNSET_APPLY\" in 1) set -u;; esac; \
unset MEZ_RESTORE_ERREXIT_APPLY MEZ_RESTORE_NOUNSET_APPLY; :\n",
            env_words = env_words,
            shell_invocation = shell_invocation,
        ))
    }
}
