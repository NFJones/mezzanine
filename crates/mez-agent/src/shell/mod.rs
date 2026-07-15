//! Provider-independent shell transaction, environment, and bootstrap contracts.
//!
//! This module owns deterministic shell classification, transaction rendering,
//! bootstrap parsing, and tool-discovery state. It does not read product
//! configuration, inspect the filesystem, or execute a process; the product
//! crate supplies those effects through its runtime adapters.

use std::fmt;
use std::path::Path;

mod bootstrap;
mod environment;
mod transaction;

pub use bootstrap::{
    bootstrap_script, bootstrap_script_for_classification, fish_bootstrap_script,
    fish_tool_discovery_script, parse_bootstrap_env_output,
    readiness_probe_command_for_classification, tool_discovery_script,
};
pub use environment::{EnvironmentSignature, ToolDiscoveryCache, ToolInventory, ToolProbe};
pub use transaction::{
    DEFAULT_BOOTSTRAP_TIMEOUT_MS, DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS, MarkerToken,
    SHELL_OUTPUT_BASE64_MAX_RAW_BYTES, SHELL_TRANSACTION_COMMAND_BASE64_LINE_BYTES,
    ShellClassification, ShellTransaction, ShellTransactionInput, ShellTransactionOutputTransport,
    agent_subshell_enter_command, fish_quote, posix_shell_history_suppression_finish,
    posix_shell_history_suppression_start, shell_command_contains_unquoted_heredoc,
    shell_command_invokes_semantic_action, validate_agent_authored_shell_command,
};

/// Categorizes deterministic shell-source validation failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentShellValidationErrorKind {
    /// A caller supplied invalid shell transaction input.
    InvalidArgs,
}

/// Reports invalid provider-independent shell transaction input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentShellValidationError {
    kind: AgentShellValidationErrorKind,
    message: String,
}

impl AgentShellValidationError {
    /// Creates an invalid-arguments shell validation failure.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            kind: AgentShellValidationErrorKind::InvalidArgs,
            message: message.into(),
        }
    }

    /// Returns the stable failure category.
    pub fn kind(&self) -> AgentShellValidationErrorKind {
        self.kind
    }

    /// Returns the diagnostic message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentShellValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AgentShellValidationError {}

/// Result returned by provider-independent shell-source validation.
pub type AgentShellValidationResult<T> = Result<T, AgentShellValidationError>;

/// Validates the hexadecimal marker used to delimit one shell transaction.
pub fn validate_shell_marker_token(token: &str) -> AgentShellValidationResult<()> {
    if token.len() < 32 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AgentShellValidationError::invalid_args(
            "marker token must contain at least 128 bits encoded as 32 or more hex characters",
        ));
    }
    Ok(())
}

/// Validates that a transaction uses an absolute resolved shell path.
pub fn validate_resolved_shell_path(shell_path: &Path) -> AgentShellValidationResult<()> {
    if !shell_path.is_absolute() {
        return Err(AgentShellValidationError::invalid_args(
            "shell transaction wrapper requires an absolute resolved shell path",
        ));
    }
    Ok(())
}

/// Quotes one value as a POSIX shell word.
///
/// The returned text is safe to embed as one literal shell argument. Empty
/// values remain explicit empty arguments, and embedded single quotes use the
/// standard close-double-quote-reopen sequence.
pub fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    static NEXT_SHELL_TEST_TEMP_ID: AtomicU64 = AtomicU64::new(1);

    /// Builds the stable transaction marker shared by shell contract tests.
    fn marker() -> MarkerToken {
        MarkerToken::new("0123456789abcdef0123456789abcdef")
            .expect("the test marker should be valid")
    }

    /// Creates one unique temporary directory for an executing shell test.
    fn test_temp_dir(label: &str) -> PathBuf {
        let unique = NEXT_SHELL_TEST_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mez-agent-{label}-{}-{nanos}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("the shell test temp directory should be created");
        path
    }

    /// Runs a POSIX shell script through stdin.
    fn run_sh_stdin(script: &str) -> Output {
        let mut command = Command::new("/bin/sh");
        run_command_stdin(&mut command, script)
    }

    /// Streams one transaction wrapper and payload through a POSIX shell.
    fn run_sh_transaction(input: &ShellTransactionInput, suffix: &str) -> Output {
        let mut command = Command::new("/bin/sh");
        run_command_transaction_stdin(&mut command, input, suffix)
    }

    /// Streams one transaction to a spawned shell process in protocol order.
    fn run_command_transaction_stdin(
        command: &mut Command,
        input: &ShellTransactionInput,
        suffix: &str,
    ) -> Output {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the shell test process should spawn");
        let stdin = child
            .stdin
            .as_mut()
            .expect("the child stdin should be piped");
        stdin
            .write_all(input.wrapper.as_bytes())
            .expect("the transaction wrapper should be written");
        thread::sleep(Duration::from_millis(50));
        stdin
            .write_all(input.payload.as_bytes())
            .expect("the transaction payload should be written");
        stdin
            .write_all(suffix.as_bytes())
            .expect("the transaction suffix should be written");
        drop(child.stdin.take());
        child
            .wait_with_output()
            .expect("the shell test process should finish")
    }

    /// Writes one complete script to a spawned shell process.
    fn run_command_stdin(command: &mut Command, script: &str) -> Output {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the shell test process should spawn");
        child
            .stdin
            .as_mut()
            .expect("the child stdin should be piped")
            .write_all(script.as_bytes())
            .expect("the shell test script should be written");
        drop(child.stdin.take());
        child
            .wait_with_output()
            .expect("the shell test process should finish")
    }

    /// Builds a representative known environment signature for cache tests.
    fn test_env_signature(
        host: &str,
        user: &str,
        shell_path: &str,
        working_directory: &str,
    ) -> EnvironmentSignature {
        EnvironmentSignature::new(
            "linux",
            "x86_64",
            None,
            host,
            user,
            shell_path,
            ShellClassification::classify(shell_path),
            None,
            None,
            working_directory,
            None,
            false,
            None,
            Vec::new(),
        )
        .expect("the test environment signature should be valid")
    }

    mod shell_bootstrap;
    mod shell_transport;
    mod tool_discovery;

    /// Verifies shell quoting preserves empty values and embedded single
    /// quotes as one literal POSIX shell argument.
    #[test]
    fn shell_quote_preserves_literal_arguments() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("plain value"), "'plain value'");
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }

    /// Shell transaction validation accepts strong markers and absolute
    /// shell paths while rejecting malformed product inputs.
    #[test]
    fn shell_transaction_inputs_are_validated() {
        validate_shell_marker_token("0123456789abcdef0123456789abcdef")
            .expect("a 128-bit hexadecimal marker should be valid");
        validate_resolved_shell_path(Path::new("/bin/sh"))
            .expect("an absolute shell path should be valid");

        let marker_error = validate_shell_marker_token("not-hex")
            .expect_err("a short non-hexadecimal marker should fail");
        assert_eq!(
            marker_error.kind(),
            AgentShellValidationErrorKind::InvalidArgs
        );
        let path_error = validate_resolved_shell_path(Path::new("bin/sh"))
            .expect_err("a relative shell path should fail");
        assert_eq!(
            path_error.kind(),
            AgentShellValidationErrorKind::InvalidArgs
        );
    }
}
