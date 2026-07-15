//! Agent Shell implementation.
//!
//! This module owns the agent shell boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{BTreeMap, MezError, Path, Result};
use base64::Engine;
use mez_agent::instructions::{DiscoveredInstructionFile, parse_instruction_discovery_output};
use mez_agent::{validate_resolved_shell_path, validate_shell_marker_token};
use sha2::Digest;

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
fn classify_version_probe(version: &str) -> Option<ShellClassification> {
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
    pub fn new(token: impl Into<String>) -> Result<Self> {
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
    /// Stores the output transport used by isolated child command execution.
    ///
    /// Stateful commands always remain raw because they intentionally execute
    /// in the active pane shell. Isolated action commands can encode output so
    /// terminal-control bytes stay inert until runtime result processing.
    pub output_transport: ShellTransactionOutputTransport,
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
pub(super) const SHELL_TRANSACTION_COMMAND_BASE64_LINE_BYTES: usize = 768;
/// Maximum raw output bytes emitted through one base64 shell-output transport.
pub(super) const SHELL_OUTPUT_BASE64_MAX_RAW_BYTES: usize = 256 * 1024;
/// Marker that begins one base64-encoded shell-output transport block.
pub(super) const SHELL_OUTPUT_BASE64_BEGIN_MARKER: &str = "__MEZ_SHELL_OUTPUT_BASE64_BEGIN__";
/// Marker that ends one base64-encoded shell-output transport block.
pub(super) const SHELL_OUTPUT_BASE64_END_MARKER: &str = "__MEZ_SHELL_OUTPUT_BASE64_END__";
/// Marker that reports raw bytes dropped before base64 output emission.
pub(super) const SHELL_OUTPUT_BASE64_DROPPED_BYTES_MARKER: &str =
    "__MEZ_SHELL_OUTPUT_BASE64_DROPPED_BYTES__";

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
) -> String {
    let mut lines = Vec::new();
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
        posix_child_command_line("    command setsid -w", child_env, shell_invocation, transport),
        "  elif command -v python3 >/dev/null 2>&1; then".to_string(),
        posix_child_command_line(
            "    command python3 -c 'import os,sys; os.setsid(); os.execvp(sys.argv[1], sys.argv[1:])'",
            child_env,
            shell_invocation,
            transport,
        ),
        "  elif command -v perl >/dev/null 2>&1; then".to_string(),
        posix_child_command_line(
            "    command perl -MPOSIX=setsid -e 'setsid(); exec @ARGV'",
            child_env,
            shell_invocation,
            transport,
        ),
        "  else".to_string(),
        posix_child_command_line("    command", child_env, shell_invocation, transport),
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
) -> String {
    let redirect = if transport == ShellTransactionOutputTransport::Base64 {
        " > \"$MEZ_OUTPUT_FILE\" 2>&1"
    } else {
        ""
    };
    format!("{prefix} {child_env} {shell_invocation} </dev/null{redirect}")
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
) -> String {
    let mut lines = Vec::new();
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
        ),
        "else if command -q python3".to_string(),
        fish_child_command_line(
            "    command python3 -c 'import os,sys; os.setsid(); os.execvp(sys.argv[1], sys.argv[1:])' env",
            noninteractive_env,
            shell_invocation,
            transport,
        ),
        "else if command -q perl".to_string(),
        fish_child_command_line(
            "    command perl -MPOSIX=setsid -e 'setsid(); exec @ARGV' env",
            noninteractive_env,
            shell_invocation,
            transport,
        ),
        "else".to_string(),
        fish_child_command_line("    command env", noninteractive_env, shell_invocation, transport),
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
) -> String {
    let redirect = if transport == ShellTransactionOutputTransport::Base64 {
        " > \"$MEZ_OUTPUT_FILE\" 2>&1"
    } else {
        ""
    };
    format!("{prefix} {noninteractive_env} {shell_invocation} </dev/null{redirect}")
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
    ) -> Result<Self> {
        validate_resolved_shell_path(shell_path)?;
        Ok(Self {
            marker,
            turn_id: turn_id.into(),
            agent_id: agent_id.into(),
            pane_id: pane_id.into(),
            shell_path: shell_path.to_string_lossy().into_owned(),
            command: command.into(),
            output_transport: ShellTransactionOutputTransport::Raw,
        })
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
        let shell_invocation = posix_shell_script_invocation_words(
            &self.shell_path,
            classification,
            "\"$MEZ_COMMAND_FILE\"",
        );
        let child_invocation = posix_child_command_invocation_lines(
            self.output_transport,
            &posix_noninteractive_agent_env_command_words(),
            &shell_invocation,
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
unset MEZ_COMMAND_FILE MEZ_COMMAND_B64 MEZ_COMMAND_END MEZ_COMMAND_LINE MEZ_COMMAND_SEEN_END MEZ_OUTPUT_FILE MEZ_STTY_STATE MEZ_WRITE_STATUS\n\
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
        let shell_invocation = fish_shell_script_invocation_words(
            &self.shell_path,
            ShellClassification::Fish,
            "\"$MEZ_COMMAND_FILE\"",
        );
        let child_invocation = fish_child_command_invocation_lines(
            self.output_transport,
            &fish_noninteractive_agent_env_words(),
            &shell_invocation,
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
set -e MEZ_COMMAND_FILE MEZ_COMMAND_B64 MEZ_COMMAND_END MEZ_COMMAND_LINE MEZ_COMMAND_SEEN_END MEZ_OUTPUT_FILE MEZ_STTY_STATE MEZ_WRITE_STATUS\n\
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
pub(crate) fn posix_shell_history_suppression_start() -> &'static str {
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
pub(crate) fn posix_shell_history_suppression_finish() -> &'static str {
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

use mez_agent::shell_quote;

/// Validates model-authored shell input before Mezzanine wraps it for pane
/// execution.
///
/// Model-authored heredoc and here-string redirections are disabled because
/// they are easy to leave unterminated and can strand the shell transaction.
/// Runtime-generated wrappers use bounded shell syntax and base64 command
/// materialization instead. Models should use `apply_patch` for structured file
/// content changes.
pub fn validate_agent_authored_shell_command(command: &str) -> Result<()> {
    if shell_command_contains_unquoted_heredoc(command) {
        return Err(MezError::invalid_args(
            "shell_command heredoc redirection is disabled for agent-authored commands; use apply_patch for file content changes",
        ));
    }
    if let Some(tool) = shell_command_invokes_semantic_action(command) {
        return Err(MezError::invalid_args(format!(
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
) -> Result<String> {
    if !shell_path.is_absolute() {
        return Err(MezError::invalid_args(
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

/// Carries Environment Signature state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnvironmentSignature {
    /// Stores the os value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub os: String,
    /// Stores the arch value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub arch: String,
    /// Stores the kernel version value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub kernel_version: Option<String>,
    /// Stores the host value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub host: String,
    /// Stores the user value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub user: String,
    /// Stores the shell path value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub shell_path: String,
    /// Stores the shell classification value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub shell_classification: ShellClassification,
    /// Stores the shell version value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub shell_version: Option<String>,
    /// Stores the path value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub path: Option<String>,
    /// Stores the working directory value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub working_directory: String,
    /// Stores the project root value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub project_root: Option<String>,
    /// Stores the git repo value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub git_repo: bool,
    /// Stores the container value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub container: Option<String>,
    /// Stores the environment managers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub environment_managers: Vec<String>,
}

impl EnvironmentSignature {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        os: impl Into<String>,
        arch: impl Into<String>,
        kernel_version: Option<String>,
        host: impl Into<String>,
        user: impl Into<String>,
        shell_path: impl Into<String>,
        shell_classification: ShellClassification,
        shell_version: Option<String>,
        path: Option<String>,
        working_directory: impl Into<String>,
        project_root: Option<String>,
        git_repo: bool,
        container: Option<String>,
        environment_managers: Vec<String>,
    ) -> Result<Self> {
        let signature = Self {
            os: os.into(),
            arch: arch.into(),
            kernel_version,
            host: host.into(),
            user: user.into(),
            shell_path: shell_path.into(),
            shell_classification,
            shell_version,
            path,
            working_directory: working_directory.into(),
            project_root,
            git_repo,
            container,
            environment_managers,
        };
        if signature.os.is_empty()
            || signature.arch.is_empty()
            || signature.host.is_empty()
            || signature.user.is_empty()
            || signature.shell_path.is_empty()
            || signature.working_directory.is_empty()
        {
            return Err(MezError::invalid_args(
                "environment signature core fields must not be empty",
            ));
        }
        Ok(signature)
    }

    /// Runs the unknown operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn unknown() -> Self {
        Self {
            os: "unknown".to_string(),
            arch: "unknown".to_string(),
            kernel_version: None,
            host: "unknown".to_string(),
            user: "unknown".to_string(),
            shell_path: "/bin/sh".to_string(),
            shell_classification: ShellClassification::UnknownUnix,
            shell_version: None,
            path: None,
            working_directory: "/".to_string(),
            project_root: None,
            git_repo: false,
            container: None,
            environment_managers: Vec::new(),
        }
    }

    /// Reports whether this signature is the unknown sentinel used before the
    /// runtime can collect real environment details.
    ///
    /// Unknown signatures are intentionally treated as uncached bootstrap
    /// requests so a previously-recorded sentinel cannot suppress discovery for
    /// later sessions that still lack concrete environment identity.
    pub fn is_unknown(&self) -> bool {
        self.os == "unknown"
            && self.arch == "unknown"
            && self.host == "unknown"
            && self.user == "unknown"
            && self.shell_path == "/bin/sh"
            && self.shell_classification == ShellClassification::UnknownUnix
            && self.shell_version.is_none()
            && self.path.is_none()
            && self.working_directory == "/"
            && self.project_root.is_none()
            && !self.git_repo
            && self.container.is_none()
            && self.environment_managers.is_empty()
    }

    /// Runs the known fields operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn known_fields(&self) -> Vec<String> {
        let mut fields = Vec::new();
        fields.push(format!("os={}", self.os));
        fields.push(format!("arch={}", self.arch));
        if let Some(ref kv) = self.kernel_version {
            fields.push(format!("kernel_version={kv}"));
        }
        fields.push(format!("host={}", self.host));
        fields.push(format!("user={}", self.user));
        fields.push(format!("shell_path={}", self.shell_path));
        fields.push(format!(
            "shell_classification={}",
            self.shell_classification.as_str()
        ));
        if let Some(ref sv) = self.shell_version {
            fields.push(format!("shell_version={sv}"));
        }
        if let Some(ref p) = self.path {
            fields.push(format!("path={p}"));
        }
        fields.push(format!("working_directory={}", self.working_directory));
        if let Some(ref pr) = self.project_root {
            fields.push(format!("project_root={pr}"));
        }
        fields.push(format!(
            "git_repo={}",
            if self.git_repo { "1" } else { "0" }
        ));
        if let Some(ref c) = self.container {
            fields.push(format!("container={c}"));
        }
        for manager in &self.environment_managers {
            fields.push(format!("environment_manager={manager}"));
        }
        fields
    }

    /// Returns a stable SHA-256 digest of the full canonical signature.
    ///
    /// The digest lets model-facing context identify the current environment
    /// without copying large or sensitive host details such as `PATH`, host
    /// names, user names, or shell version banners into every request.
    pub fn stable_hash(&self) -> String {
        sha256_hex(self.canonical_hash_fields().join("\n").as_bytes())
    }

    /// Returns compact fields intended for model prompt context.
    ///
    /// This projection keeps execution-critical facts visible while replacing
    /// noisy host details with a fixed-width hash. Runtime caches and audits can
    /// still use the full typed signature.
    pub fn model_context_fields(&self) -> Vec<String> {
        let mut fields = Vec::new();
        fields.push(format!("env_signature=sha256:{}", self.stable_hash()));
        fields.push(format!("cwd={}", self.working_directory));
        fields.push(format!("shell={}", self.shell_classification.as_str()));
        fields.push(format!("shell_path={}", self.shell_path));
        fields.push(format!(
            "git_repo={}",
            if self.git_repo { "1" } else { "0" }
        ));
        if let Some(ref pr) = self.project_root {
            fields.push(format!("project_root={pr}"));
        }
        if let Some(ref container) = self.container {
            fields.push(format!("container={container}"));
        }
        if !self.environment_managers.is_empty() {
            fields.push(format!(
                "environment_managers={}",
                self.environment_managers.join(",")
            ));
        }
        if let Some(ref path) = self.path {
            fields.push(format!(
                "path_entries={}",
                path.split(':').filter(|entry| !entry.is_empty()).count()
            ));
        }
        fields
    }

    /// Returns deterministic full-signature fields for hashing.
    fn canonical_hash_fields(&self) -> Vec<String> {
        let mut fields = self.known_fields();
        let mut managers = self.environment_managers.clone();
        managers.sort();
        fields.retain(|field| !field.starts_with("environment_manager="));
        for manager in managers {
            fields.push(format!("environment_manager={manager}"));
        }
        fields
    }
}

/// Returns a lowercase hex SHA-256 digest for stable model-visible IDs.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

/// Carries Tool Probe state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolProbe {
    /// Tool name requested by the bootstrap probe.
    pub name: String,
    /// Whether the lookup command found an executable.
    pub available: bool,
    /// Resolved executable path returned by the pane shell, when available.
    pub path: Option<String>,
    /// First line of version output, when the tool supports a version probe.
    pub version: Option<String>,
    /// Lookup command used for the probe.
    pub lookup_command: String,
    /// Exit status from the lookup command.
    pub lookup_exit_status: Option<i32>,
    /// Version command used after a successful lookup.
    pub version_command: Option<String>,
    /// Exit status from the version command.
    pub version_exit_status: Option<i32>,
    /// Unix timestamp reported by the pane shell for the discovery run.
    pub discovered_at_unix_seconds: Option<u64>,
}

/// Carries Tool Inventory state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInventory {
    /// Stores the sed value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub sed: bool,
    /// Stores the grep value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub grep: bool,
    /// Stores the python value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub python: bool,
    /// Stores the rg value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub rg: bool,
    /// Stores the modern tools value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub modern_tools: Vec<String>,
    /// Stores the tools value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub tools: BTreeMap<String, ToolProbe>,
}

impl ToolInventory {
    /// Runs the parse bootstrap output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn parse_bootstrap_output(output: &str) -> Self {
        let mut inventory = Self {
            sed: false,
            grep: false,
            python: false,
            rg: false,
            modern_tools: Vec::new(),
            tools: BTreeMap::new(),
        };

        for line in output.lines() {
            if let Some(probe) = tool_probe_from_structured_line(line) {
                inventory.record_tool_probe(probe);
                continue;
            }
            let Some((name, present)) = line.split_once('=') else {
                continue;
            };
            let present = present.trim() == "1";
            inventory.record_legacy_probe(name.trim(), present);
        }

        inventory.modern_tools.sort();
        inventory.modern_tools.dedup();
        inventory
    }

    /// Runs the record legacy probe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn record_legacy_probe(&mut self, name: &str, available: bool) {
        self.record_tool_probe(ToolProbe {
            name: name.to_string(),
            available,
            path: None,
            version: None,
            lookup_command: format!("command -v {name}"),
            lookup_exit_status: Some(if available { 0 } else { 1 }),
            version_command: None,
            version_exit_status: None,
            discovered_at_unix_seconds: None,
        });
    }

    /// Runs the record tool probe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn record_tool_probe(&mut self, probe: ToolProbe) {
        match probe.name.as_str() {
            "sed" => self.sed = probe.available,
            "grep" => self.grep = probe.available,
            "python" => self.python = probe.available,
            "rg" => self.rg = probe.available,
            tool if probe.available => self.modern_tools.push(tool.to_string()),
            _ => {}
        }
        self.tools.insert(probe.name.clone(), probe);
    }
}

/// Runs the tool probe from structured line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn tool_probe_from_structured_line(line: &str) -> Option<ToolProbe> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 10 || fields[0] != "tool" {
        return None;
    }

    let name = fields[1].trim();
    if name.is_empty() {
        return None;
    }
    Some(ToolProbe {
        name: name.to_string(),
        available: fields[2] == "1",
        path: optional_tool_field(fields[3]),
        version: optional_tool_field(fields[4]),
        lookup_command: fields[5].to_string(),
        lookup_exit_status: optional_i32_field(fields[6]),
        version_command: optional_tool_field(fields[7]),
        version_exit_status: optional_i32_field(fields[8]),
        discovered_at_unix_seconds: optional_u64_field(fields[9]),
    })
}

/// Runs the optional tool field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_tool_field(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Runs the optional i32 field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_i32_field(value: &str) -> Option<i32> {
    (!value.is_empty())
        .then(|| value.parse::<i32>().ok())
        .flatten()
}

/// Runs the optional u64 field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_u64_field(value: &str) -> Option<u64> {
    (!value.is_empty())
        .then(|| value.parse::<u64>().ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::{optional_tool_field, tool_probe_from_structured_line};

    /// Verifies whitespace-only optional tool metadata normalizes to `None`
    /// so the discovery cache does not preserve meaningless placeholder text.
    #[test]
    fn optional_tool_field_rejects_whitespace_only_values() {
        assert_eq!(optional_tool_field("   \t  "), None);
    }

    /// Verifies structured tool probe parsing trims blank optional fields to
    /// `None` while preserving the required probe metadata.
    #[test]
    fn tool_probe_from_structured_line_normalizes_blank_optional_fields() {
        let probe = tool_probe_from_structured_line(
            "tool\trg\t0\t \t \tcommand -v rg\t127\t \t\t1710000000",
        )
        .expect("tool probe line should parse");

        assert_eq!(probe.name, "rg");
        assert!(!probe.available);
        assert_eq!(probe.path, None);
        assert_eq!(probe.version, None);
        assert_eq!(probe.lookup_command, "command -v rg");
        assert_eq!(probe.lookup_exit_status, Some(127));
        assert_eq!(probe.version_command, None);
        assert_eq!(probe.version_exit_status, None);
        assert_eq!(probe.discovered_at_unix_seconds, Some(1710000000));
    }
}

/// Carries Tool Discovery Cache state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default)]
pub struct ToolDiscoveryCache {
    /// Stores the inventories value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) inventories: BTreeMap<EnvironmentSignature, ToolInventory>,
}

impl ToolDiscoveryCache {
    /// Runs the requires bootstrap operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn requires_bootstrap(&self, signature: &EnvironmentSignature) -> bool {
        signature.is_unknown() || !self.inventories.contains_key(signature)
    }

    /// Runs the record operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn record(&mut self, signature: EnvironmentSignature, inventory: ToolInventory) {
        if signature.is_unknown() {
            return;
        }
        self.inventories.insert(signature, inventory);
    }

    /// Runs the get operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn get(&self, signature: &EnvironmentSignature) -> Option<&ToolInventory> {
        self.inventories.get(signature)
    }
}

/// Runs the tool discovery script operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn tool_discovery_script() -> &'static str {
    "mez_discovered_at=$(date +%s 2>/dev/null || printf '0')\n\
mez_probe_tool() {\n\
  mez_tool=\"$1\"\n\
  mez_lookup_command=\"command -v $mez_tool\"\n\
  mez_path=$(command -v \"$mez_tool\" 2>/dev/null)\n\
  mez_lookup_status=$?\n\
  mez_version=\"\"\n\
  mez_version_command=\"\"\n\
  mez_version_status=\"\"\n\
  if [ \"$mez_lookup_status\" -eq 0 ]; then\n\
    mez_version_command=\"$mez_path --version\"\n\
    mez_version_output=$(\"$mez_path\" --version 2>/dev/null)\n\
    mez_version_status=$?\n\
    mez_version=$(printf '%s\\n' \"$mez_version_output\" | { IFS= read -r mez_first_line; printf '%s' \"$mez_first_line\"; })\n\
  fi\n\
  printf 'tool\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\n' \"$mez_tool\" \"$([ \"$mez_lookup_status\" -eq 0 ] && printf '1' || printf '0')\" \"$mez_path\" \"$mez_version\" \"$mez_lookup_command\" \"$mez_lookup_status\" \"$mez_version_command\" \"$mez_version_status\" \"$mez_discovered_at\"\n\
}\n\
for mez_tool in sed grep rg fd bat jq git; do\n\
  mez_probe_tool \"$mez_tool\"\n\
done\n\
mez_python_path=$(command -v python3 2>/dev/null)\n\
mez_python_lookup_status=$?\n\
if [ \"$mez_python_lookup_status\" -ne 0 ]; then\n\
  mez_python_path=$(command -v python 2>/dev/null)\n\
  mez_python_lookup_status=$?\n\
fi\n\
mez_python_version=\"\"\n\
mez_python_version_command=\"\"\n\
mez_python_version_status=\"\"\n\
if [ \"$mez_python_lookup_status\" -eq 0 ]; then\n\
  mez_python_version_command=\"$mez_python_path --version\"\n\
  mez_python_version_output=$(\"$mez_python_path\" --version 2>/dev/null)\n\
  mez_python_version_status=$?\n\
  mez_python_version=$(printf '%s\\n' \"$mez_python_version_output\" | { IFS= read -r mez_first_line; printf '%s' \"$mez_first_line\"; })\n\
fi\n\
printf 'tool\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\n' \"python\" \"$([ \"$mez_python_lookup_status\" -eq 0 ] && printf '1' || printf '0')\" \"$mez_python_path\" \"$mez_python_version\" \"command -v python3 || command -v python\" \"$mez_python_lookup_status\" \"$mez_python_version_command\" \"$mez_python_version_status\" \"$mez_discovered_at\"\n"
}

/// Runs the bootstrap script operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn bootstrap_script() -> String {
    let mut script = "mez_discovered_at=$(date +%s 2>/dev/null || printf '0')\n\
mez_bootstrap_field() {\n\
  mez_key=\"$1\"\n\
  mez_value=\"$2\"\n\
  printf 'env\\t%s\\t%s\\n' \"$mez_key\" \"$mez_value\"\n\
}\n\
\n\
mez_bootstrap_field os \"$(uname -s 2>/dev/null || printf 'unknown')\"\n\
mez_bootstrap_field arch \"$(uname -m 2>/dev/null || printf 'unknown')\"\n\
mez_kernel=$(uname -r 2>/dev/null)\n\
if [ -n \"$mez_kernel\" ]; then\n\
  mez_bootstrap_field kernel_version \"$mez_kernel\"\n\
fi\n\
\n\
mez_bootstrap_field host \"$(hostname 2>/dev/null || printf 'unknown')\"\n\
mez_bootstrap_field user \"$(whoami 2>/dev/null || printf 'unknown')\"\n\
mez_bootstrap_field shell_path \"$SHELL\"\n\
\n\
mez_shell_name=$(printf '%s' \"$SHELL\" | { IFS=/ read -r _ _ _ _ _ _ _ _ _ _ _ mez_stem; printf '%s' \"$mez_stem\"; });\n\
mez_shell_name=${mez_shell_name:-sh}\n\
mez_bootstrap_field shell_class \"$mez_shell_name\"\n\
\n\
if command -v \"$SHELL\" >/dev/null 2>&1; then\n\
  mez_shell_ver=$(\"$SHELL\" --version 2>/dev/null | { IFS= read -r mez_first_line; printf '%s' \"$mez_first_line\"; })\n\
  if [ -n \"$mez_shell_ver\" ]; then\n\
    mez_bootstrap_field shell_version \"$mez_shell_ver\"\n\
  fi\n\
fi\n\
\n\
mez_bootstrap_field path \"$PATH\"\n\
mez_bootstrap_field cwd \"$(pwd 2>/dev/null || printf '/')\"\n\
\n\
mez_project_root=\"\"\n\
mez_search_dir=\"$(pwd 2>/dev/null)\"\n\
while [ -n \"$mez_search_dir\" ] && [ \"$mez_search_dir\" != \"/\" ]; do\n\
  if [ -d \"$mez_search_dir/.git\" ]; then\n\
    mez_project_root=\"$mez_search_dir\"\n\
    break\n\
  fi\n\
  mez_search_dir=$(dirname \"$mez_search_dir\" 2>/dev/null)\n\
done\n\
mez_bootstrap_field project_root \"$mez_project_root\"\n\
mez_bootstrap_field git_repo \"$([ -n \"$mez_project_root\" ] && printf '1' || printf '0')\"\n\
\n\
if [ -f /proc/1/cgroup ] 2>/dev/null; then\n\
  mez_container=$(grep -Eo 'docker|lxc|kubepods|libpod' /proc/1/cgroup 2>/dev/null | head -n1)\n\
  if [ -n \"$mez_container\" ]; then\n\
    mez_bootstrap_field container \"$mez_container\"\n\
  fi\n\
elif [ -f /.dockerenv ] 2>/dev/null; then\n\
  mez_bootstrap_field container docker\n\
fi\n\
\n\
if [ -n \"$VIRTUAL_ENV\" ]; then\n\
  mez_bootstrap_field env_manager \"virtualenv:$VIRTUAL_ENV\"\n\
fi\n\
if [ -n \"$CONDA_PREFIX\" ]; then\n\
  mez_bootstrap_field env_manager \"conda:$CONDA_PREFIX\"\n\
fi\n\
if [ -n \"$NIX_PROFILES\" ]; then\n\
  mez_bootstrap_field env_manager \"nix:$NIX_PROFILES\"\n\
fi\n\
if [ -n \"$NODE_VIRTUAL_ENV\" ]; then\n\
  mez_bootstrap_field env_manager \"node:$NODE_VIRTUAL_ENV\"\n\
fi\n\
if [ -n \"$RUSTUP_HOME\" ]; then\n\
  mez_bootstrap_field env_manager \"rustup\"\n\
fi\n\
if [ -n \"$GOPATH\" ]; then\n\
  mez_bootstrap_field env_manager \"go\"\n\
fi\n\
\n\
mez_inst_max=32768\n\
mez_inst_cwd=\"$(pwd 2>/dev/null || printf '/')\"\n\
mez_inst_current=\"$mez_inst_cwd\"\n\
mez_inst_done=false\n\
while [ \"$mez_inst_done\" = \"false\" ]; do\n\
  if [ -f \"$mez_inst_current/AGENTS.md\" ]; then\n\
    mez_inst_file=\"$mez_inst_current/AGENTS.md\"\n\
    mez_inst_bytes=$(wc -c < \"$mez_inst_file\" 2>/dev/null | tr -d ' ')\n\
    [ -z \"$mez_inst_bytes\" ] && mez_inst_bytes=0\n\
    mez_inst_trunc=false; [ \"$mez_inst_bytes\" -gt \"$mez_inst_max\" ] && mez_inst_trunc=true\n\
    mez_inst_content=$(head -c \"$mez_inst_max\" \"$mez_inst_file\" 2>/dev/null | sed 's/\\\\/\\\\\\\\/g; s/\\t/\\\\t/g; s/\\r/\\\\r/g; s/$/\\\\n/' | tr -d '\\n')\n\
    printf 'instruction\\tpath=%s\\tscope=%s\\tbytes=%s\\ttruncated=%s\\tcontent=%s\\n' \"$mez_inst_file\" \"$mez_inst_current\" \"$mez_inst_bytes\" \"$mez_inst_trunc\" \"$mez_inst_content\"\n\
  fi\n\
  if [ \"$mez_inst_current\" = \"$mez_project_root\" ] || [ \"$mez_inst_current\" = \"/\" ] || [ -z \"$mez_project_root\" ]; then\n\
    mez_inst_done=true\n\
  else\n\
    mez_inst_current=$(dirname \"$mez_inst_current\" 2>/dev/null || printf '/')\n\
  fi\n\
done\n\
\n\
printf 'bootstrap\\tcomplete\\t%s\\n' \"$mez_discovered_at\"\n"
        .to_string();
    script.push_str(tool_discovery_script());
    script
}

/// Runs the fish bootstrap script operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn fish_bootstrap_script() -> String {
    let mut script = "set -l mez_discovered_at (date +%s 2>/dev/null; or printf '0')\n\
function mez_bootstrap_field\n\
  set -l mez_key $argv[1]\n\
  set -l mez_value $argv[2]\n\
  printf 'env\\t%s\\t%s\\n' \"$mez_key\" \"$mez_value\"\n\
end\n\
\n\
mez_bootstrap_field os (uname -s 2>/dev/null; or printf 'unknown')\n\
mez_bootstrap_field arch (uname -m 2>/dev/null; or printf 'unknown')\n\
set -l mez_kernel (uname -r 2>/dev/null)\n\
if test -n \"$mez_kernel\"\n\
  mez_bootstrap_field kernel_version \"$mez_kernel\"\n\
end\n\
\n\
mez_bootstrap_field host (hostname 2>/dev/null; or printf 'unknown')\n\
mez_bootstrap_field user (whoami 2>/dev/null; or printf 'unknown')\n\
set -l mez_shell_path (status fish-path 2>/dev/null; or command -v fish 2>/dev/null; or printf '%s' \"$SHELL\")\n\
if test -z \"$mez_shell_path\"\n\
  set mez_shell_path \"$SHELL\"\n\
end\n\
mez_bootstrap_field shell_path \"$mez_shell_path\"\n\
mez_bootstrap_field shell_class fish\n\
set -l mez_shell_ver ($mez_shell_path --version 2>/dev/null | head -n 1)\n\
if test -n \"$mez_shell_ver\"\n\
  mez_bootstrap_field shell_version \"$mez_shell_ver\"\n\
end\n\
\n\
mez_bootstrap_field path \"$PATH\"\n\
set -l mez_cwd (pwd 2>/dev/null; or printf '/')\n\
mez_bootstrap_field cwd \"$mez_cwd\"\n\
\n\
set -l mez_project_root ''\n\
set -l mez_search_dir \"$mez_cwd\"\n\
while test -n \"$mez_search_dir\"; and test \"$mez_search_dir\" != '/'\n\
  if test -d \"$mez_search_dir/.git\"; or test -f \"$mez_search_dir/.git\"\n\
    set mez_project_root \"$mez_search_dir\"\n\
    break\n\
  end\n\
  set mez_search_dir (dirname \"$mez_search_dir\" 2>/dev/null; or printf '/')\n\
end\n\
mez_bootstrap_field project_root \"$mez_project_root\"\n\
if test -n \"$mez_project_root\"\n\
  mez_bootstrap_field git_repo 1\n\
else\n\
  mez_bootstrap_field git_repo 0\n\
end\n\
\n\
if test -f /proc/1/cgroup\n\
  set -l mez_container (grep -Eo 'docker|lxc|kubepods|libpod' /proc/1/cgroup 2>/dev/null | head -n 1)\n\
  if test -n \"$mez_container\"\n\
    mez_bootstrap_field container \"$mez_container\"\n\
  end\n\
else if test -f /.dockerenv\n\
  mez_bootstrap_field container docker\n\
end\n\
\n\
if test -n \"$VIRTUAL_ENV\"\n\
  mez_bootstrap_field env_manager \"virtualenv:$VIRTUAL_ENV\"\n\
end\n\
if test -n \"$CONDA_PREFIX\"\n\
  mez_bootstrap_field env_manager \"conda:$CONDA_PREFIX\"\n\
end\n\
if test -n \"$NIX_PROFILES\"\n\
  mez_bootstrap_field env_manager \"nix:$NIX_PROFILES\"\n\
end\n\
if test -n \"$NODE_VIRTUAL_ENV\"\n\
  mez_bootstrap_field env_manager \"node:$NODE_VIRTUAL_ENV\"\n\
end\n\
if test -n \"$RUSTUP_HOME\"\n\
  mez_bootstrap_field env_manager rustup\n\
end\n\
if test -n \"$GOPATH\"\n\
  mez_bootstrap_field env_manager go\n\
end\n\
\n\
set -l mez_inst_max 32768\n\
set -l mez_inst_current \"$mez_cwd\"\n\
while true\n\
  if test -f \"$mez_inst_current/AGENTS.md\"\n\
    set -l mez_inst_file \"$mez_inst_current/AGENTS.md\"\n\
    set -l mez_inst_bytes (wc -c < \"$mez_inst_file\" 2>/dev/null | tr -d ' ')\n\
    if test -z \"$mez_inst_bytes\"\n\
      set mez_inst_bytes 0\n\
    end\n\
    set -l mez_inst_trunc false\n\
    if test \"$mez_inst_bytes\" -gt \"$mez_inst_max\"\n\
      set mez_inst_trunc true\n\
    end\n\
    set -l mez_inst_content (head -c \"$mez_inst_max\" \"$mez_inst_file\" 2>/dev/null | sed 's/\\\\/\\\\\\\\/g; s/\\t/\\\\t/g; s/\\r/\\\\r/g; s/$/\\\\n/' | tr -d '\\n')\n\
    printf 'instruction\\tpath=%s\\tscope=%s\\tbytes=%s\\ttruncated=%s\\tcontent=%s\\n' \"$mez_inst_file\" \"$mez_inst_current\" \"$mez_inst_bytes\" \"$mez_inst_trunc\" \"$mez_inst_content\"\n\
  end\n\
  if test \"$mez_inst_current\" = \"$mez_project_root\"; or test \"$mez_inst_current\" = '/'; or test -z \"$mez_project_root\"\n\
    break\n\
  end\n\
  set mez_inst_current (dirname \"$mez_inst_current\" 2>/dev/null; or printf '/')\n\
end\n\
\n\
printf 'bootstrap\\tcomplete\\t%s\\n' \"$mez_discovered_at\"\n"
        .to_string();
    script.push_str(fish_tool_discovery_script());
    script
}

/// Runs the fish tool discovery script operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn fish_tool_discovery_script() -> &'static str {
    "set -l mez_discovered_at (date +%s 2>/dev/null; or printf '0')\n\
function mez_probe_tool\n\
  set -l mez_tool $argv[1]\n\
  set -l mez_lookup_command \"command -v $mez_tool\"\n\
  set -l mez_path (command -v \"$mez_tool\" 2>/dev/null)\n\
  set -l mez_lookup_status $status\n\
  set -l mez_version ''\n\
  set -l mez_version_command ''\n\
  set -l mez_version_status ''\n\
  if test \"$mez_lookup_status\" -eq 0\n\
    set mez_version_command \"$mez_path --version\"\n\
    set -l mez_version_output ($mez_path --version 2>/dev/null | head -n 1)\n\
    set mez_version_status $status\n\
    set mez_version \"$mez_version_output\"\n\
  end\n\
  set -l mez_available 0\n\
  if test \"$mez_lookup_status\" -eq 0\n\
    set mez_available 1\n\
  end\n\
  printf 'tool\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\n' \"$mez_tool\" \"$mez_available\" \"$mez_path\" \"$mez_version\" \"$mez_lookup_command\" \"$mez_lookup_status\" \"$mez_version_command\" \"$mez_version_status\" \"$mez_discovered_at\"\n\
end\n\
for mez_tool in sed grep rg fd bat jq git\n\
  mez_probe_tool \"$mez_tool\"\n\
end\n\
set -l mez_python_path (command -v python3 2>/dev/null)\n\
set -l mez_python_lookup_status $status\n\
if test \"$mez_python_lookup_status\" -ne 0\n\
  set mez_python_path (command -v python 2>/dev/null)\n\
  set mez_python_lookup_status $status\n\
end\n\
set -l mez_python_version ''\n\
set -l mez_python_version_command ''\n\
set -l mez_python_version_status ''\n\
if test \"$mez_python_lookup_status\" -eq 0\n\
  set mez_python_version_command \"$mez_python_path --version\"\n\
  set -l mez_python_version_output ($mez_python_path --version 2>/dev/null | head -n 1)\n\
  set mez_python_version_status $status\n\
  set mez_python_version \"$mez_python_version_output\"\n\
end\n\
set -l mez_python_available 0\n\
if test \"$mez_python_lookup_status\" -eq 0\n\
  set mez_python_available 1\n\
end\n\
printf 'tool\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\n' python \"$mez_python_available\" \"$mez_python_path\" \"$mez_python_version\" 'command -v python3 || command -v python' \"$mez_python_lookup_status\" \"$mez_python_version_command\" \"$mez_python_version_status\" \"$mez_discovered_at\"\n"
}

/// Runs the bootstrap script for classification operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn bootstrap_script_for_classification(classification: ShellClassification) -> String {
    if classification == ShellClassification::Fish {
        fish_bootstrap_script()
    } else {
        bootstrap_script()
    }
}

/// Runs the readiness probe command for classification operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn readiness_probe_command_for_classification(
    classification: ShellClassification,
) -> &'static str {
    if classification == ShellClassification::Fish {
        "true"
    } else {
        ":"
    }
}

/// Runs the parse bootstrap env output operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn parse_bootstrap_env_output(
    output: &str,
    resolved_shell_path: &Path,
) -> (
    Option<EnvironmentSignature>,
    Option<ToolInventory>,
    Vec<DiscoveredInstructionFile>,
) {
    let mut os = String::new();
    let mut arch = String::new();
    let mut kernel_version: Option<String> = None;
    let mut host = String::new();
    let mut user = String::new();
    let mut shell_path = String::new();
    let mut shell_class: Option<String> = None;
    let mut shell_version: Option<String> = None;
    let mut path: Option<String> = None;
    let mut working_directory = String::new();
    let mut project_root: Option<String> = None;
    let mut git_repo = false;
    let mut container: Option<String> = None;
    let mut environment_managers: Vec<String> = Vec::new();
    let mut tool_output = String::new();
    let mut instruction_lines: Vec<String> = Vec::new();
    let mut in_tool_section = false;

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("tool\t") {
            in_tool_section = true;
        }
        if in_tool_section || line.starts_with("tool\t") {
            if !tool_output.is_empty() {
                tool_output.push('\n');
            }
            tool_output.push_str(line);
            continue;
        }
        if let Some(rest) = line.strip_prefix("instruction\t") {
            instruction_lines.push(rest.to_string());
            continue;
        }
        let Some((prefix, rest)) = line.split_once('\t') else {
            continue;
        };
        if prefix != "env" && prefix != "bootstrap" {
            continue;
        }
        let Some((key, value)) = rest.split_once('\t') else {
            continue;
        };
        match key {
            "os" => os = value.to_string(),
            "arch" => arch = value.to_string(),
            "kernel_version" => kernel_version = Some(value.to_string()),
            "host" => host = value.to_string(),
            "user" => user = value.to_string(),
            "shell_path" => shell_path = value.to_string(),
            "shell_class" => shell_class = Some(value.to_string()),
            "shell_version" => shell_version = Some(value.to_string()),
            "path" => path = Some(value.to_string()),
            "cwd" => working_directory = value.to_string(),
            "project_root" if !value.is_empty() => {
                project_root = Some(value.to_string());
            }
            "git_repo" => git_repo = value == "1",
            "container" => container = Some(value.to_string()),
            "env_manager" if !value.is_empty() => {
                environment_managers.push(value.to_string());
            }
            _ => {}
        }
    }

    environment_managers.sort();
    environment_managers.dedup();

    let shell_metadata_matches_resolved =
        shell_path.is_empty() || Path::new(&shell_path) == resolved_shell_path;
    if shell_path.is_empty() {
        shell_path = resolved_shell_path.to_string_lossy().into_owned();
    }
    let trusted_shell_version = shell_metadata_matches_resolved
        .then_some(shell_version.as_deref())
        .flatten();
    let trusted_shell_class = shell_metadata_matches_resolved
        .then_some(shell_class.as_deref())
        .flatten();
    let probe_classification = trusted_shell_version.and_then(classify_version_probe);
    let resolved_shell_classification =
        ShellClassification::classify_with_probe(resolved_shell_path, trusted_shell_version);
    let shell_classification = probe_classification
        .or_else(|| trusted_shell_class.map(ShellClassification::classify))
        .unwrap_or(resolved_shell_classification);

    let signature = if os.is_empty() && arch.is_empty() && host.is_empty() {
        None
    } else {
        if os.is_empty() {
            os = "unknown".to_string();
        }
        if arch.is_empty() {
            arch = "unknown".to_string();
        }
        if host.is_empty() {
            host = "unknown".to_string();
        }
        if user.is_empty() {
            user = "unknown".to_string();
        }
        if working_directory.is_empty() {
            working_directory = "/".to_string();
        }
        EnvironmentSignature::new(
            os,
            arch,
            kernel_version,
            host,
            user,
            shell_path,
            shell_classification,
            shell_version,
            path,
            working_directory,
            project_root,
            git_repo,
            container,
            environment_managers,
        )
        .ok()
    };

    let inventory = if tool_output.is_empty() {
        None
    } else {
        Some(ToolInventory::parse_bootstrap_output(&tool_output))
    };

    let instruction_files = if instruction_lines.is_empty() {
        Vec::new()
    } else {
        parse_instruction_discovery_output(&instruction_lines.join("\n")).unwrap_or_default()
    };

    (signature, inventory, instruction_files)
}
