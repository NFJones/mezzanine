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
mod environment_resolution;
mod path_resolution;
mod transaction;

pub use bootstrap::{
    ShellIdentityProbeResult, bootstrap_script, bootstrap_script_for_classification,
    fish_bootstrap_script, fish_tool_discovery_script, parse_bootstrap_env_output,
    parse_shell_identity_probe_output, readiness_probe_command_for_classification,
    shell_identity_probe_command, tool_discovery_script,
};
pub use environment::{
    EnvironmentGroup, EnvironmentSignature, ToolDiscoveryCache, ToolInventory, ToolProbe,
};
pub use environment_resolution::{
    MAX_ENVIRONMENT_NAME_BYTES, MAX_ENVIRONMENT_TOTAL_VALUE_BYTES, MAX_ENVIRONMENT_VALUE_BYTES,
    MAX_ENVIRONMENT_VARIABLES, PaneEnvironmentEvidence, PaneEnvironmentRequest,
    pane_environment_evidence_command, parse_pane_environment_evidence,
};
pub use path_resolution::{
    PanePathResolutionRequest, PanePathResolutionResult, pane_path_resolution_command,
    parse_pane_path_resolution_output,
};
pub use transaction::{
    DEFAULT_BOOTSTRAP_TIMEOUT_MS, DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS, FishPrivateSourceInput,
    ForeignShellLoaderInput, ManagedZshShell, ManagedZshTrigger, MarkerToken,
    SHELL_OUTPUT_BASE64_MAX_RAW_BYTES, SHELL_TRANSACTION_COMMAND_BASE64_LINE_BYTES,
    SHELL_TRANSACTION_SIDECAR_FRAME_BYTES, ShellChildArgument, ShellChildLaunch,
    ShellClassification, ShellTransaction, ShellTransactionInput, ShellTransactionOutputTransport,
    ZSH_PRIVATE_SOURCE_DATA_MAX_BYTES, ZSH_PRIVATE_SOURCE_FRAME_BYTES,
    ZSH_PRIVATE_SOURCE_MAX_BASE64_BYTES, ZSH_PRIVATE_SOURCE_MAX_BYTES,
    ZSH_PRIVATE_SOURCE_MAX_CHUNKS, ZSH_PRIVATE_SOURCE_MAX_FRAMES,
    ZSH_PRIVATE_SOURCE_MAX_RECORD_BYTES, ZshPrivateSourceInput, agent_subshell_enter_command,
    agent_subshell_enter_command_with_shell_compatibility,
    agent_subshell_enter_command_with_shell_compatibility_and_exit_marker,
    agent_subshell_enter_command_with_zsh_history_token, agent_subshell_exit_marker_bytes,
    bash_private_handoff_cancel_input, bash_private_handoff_source_input,
    bash_private_source_input, dependency_free_foreign_shell_loader_command,
    dependency_free_foreign_shell_loader_input, fish_private_source_cancel_input,
    fish_private_source_input, fish_quote, fish_wrapper_receiver_init_command,
    posix_shell_history_suppression_finish, posix_shell_history_suppression_start,
    shell_command_contains_unquoted_heredoc, validate_agent_authored_shell_command,
    zsh_private_source_cancel_input, zsh_private_source_input,
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
    use base64::Engine as _;
    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    static NEXT_SHELL_TEST_TEMP_ID: AtomicU64 = AtomicU64::new(1);

    /// Re-encodes modified POSIX wrapper source for receiver failure tests.
    fn posix_shell_wrapper_transport(
        source: &str,
        classification: ShellClassification,
        zsh_history_token: Option<&MarkerToken>,
    ) -> String {
        super::transaction::posix_shell_wrapper_transport(source, classification, zsh_history_token)
    }

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

    /// Runs one dependency-free foreign loader exchange through a real shell.
    ///
    /// Dash script read-ahead consumes concatenated stdin records before the
    /// loader child can read them, so this mirrors production ordering: the
    /// rendezvous command is delivered alone and the payload is withheld until
    /// the loader publishes its ready event on stdout.
    fn run_foreign_loader_exchange(command: &str, payload: &str) -> Output {
        let root = test_temp_dir("foreign-loader");
        let stdout_path = root.join("stdout");
        let stderr_path = root.join("stderr");
        let mut child = Command::new("/bin/sh")
            .stdin(Stdio::piped())
            .stdout(Stdio::from(std::fs::File::create(&stdout_path).unwrap()))
            .stderr(Stdio::from(std::fs::File::create(&stderr_path).unwrap()))
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(command.as_bytes())
            .unwrap();
        let mut observed = false;
        for _ in 0..300 {
            if String::from_utf8_lossy(&std::fs::read(&stdout_path).unwrap_or_default())
                .contains("mez_foreign_loader=ready")
            {
                observed = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if !observed {
            drop(child.stdin.take());
            let _ = child.kill();
            let _ = child.wait();
            std::fs::remove_dir_all(&root).unwrap();
            panic!("dependency-free loader ready event was not observed");
        }
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        drop(child.stdin.take());
        let status = child.wait().unwrap();
        let output = Output {
            status,
            stdout: std::fs::read(&stdout_path).unwrap_or_default(),
            stderr: std::fs::read(&stderr_path).unwrap_or_default(),
        };
        std::fs::remove_dir_all(&root).unwrap();
        output
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

    /// Runs one stdin-fed command under a finite deadline, returning `None`
    /// when the requested executable is not installed.
    fn run_optional_command_stdin_bounded(
        command: &mut Command,
        script: &str,
        label: &str,
    ) -> Option<Output> {
        let mut child = match command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => panic!("the Fish parser process should spawn: {error}"),
        };
        child
            .stdin
            .as_mut()
            .unwrap_or_else(|| panic!("the {label} stdin should be piped"))
            .write_all(script.as_bytes())
            .unwrap_or_else(|error| panic!("the {label} input should be written: {error}"));
        drop(child.stdin.take());

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child
                .try_wait()
                .unwrap_or_else(|error| {
                    panic!("the {label} process should remain observable: {error}")
                })
                .is_some()
            {
                return Some(child.wait_with_output().unwrap_or_else(|error| {
                    panic!("the {label} output should be collected: {error}")
                }));
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the {label} process exceeded its five-second deadline");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Parses a complete generated wrapper with a real Fish process under a
    /// finite deadline, or returns `None` when Fish is not installed.
    fn parse_fish_wrapper(wrapper: &str) -> Option<Output> {
        let mut command = Command::new("fish");
        command.args(["--no-config", "--no-execute"]);
        run_optional_command_stdin_bounded(&mut command, wrapper, "Fish parser")
    }

    /// Resolves a real Fish executable for bounded execution tests.
    fn fish_path_for_tests() -> Option<PathBuf> {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
            .map(|directory| directory.join("fish"))
            .chain(
                [
                    "/usr/bin/fish",
                    "/usr/local/bin/fish",
                    "/opt/homebrew/bin/fish",
                ]
                .into_iter()
                .map(PathBuf::from),
            )
            .find(|candidate| candidate.is_file())
    }

    /// Waits until incremental child output satisfies one protocol predicate.
    fn wait_for_observed_shell_output(
        observed: &(Mutex<Vec<u8>>, Condvar),
        deadline: Instant,
        predicate: impl Fn(&[u8]) -> bool,
    ) -> bool {
        let (bytes, changed) = observed;
        let mut bytes = bytes
            .lock()
            .expect("the shell output observation lock should remain available");
        loop {
            if predicate(&bytes) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (next, timeout) = changed
                .wait_timeout(bytes, deadline.saturating_duration_since(now))
                .expect("the shell output observation wait should remain available");
            bytes = next;
            if timeout.timed_out() && !predicate(&bytes) {
                return false;
            }
        }
    }

    /// Executes a complete Fish transaction in marker-paced protocol order.
    ///
    /// The complete wrapper and observation suffix are supplied as the Fish
    /// `-c` program because pipe-fed Fish defers parsing stdin source until
    /// end-of-file. Stdin therefore remains exclusively available for inert
    /// payload records. The runner waits for the transaction start marker and,
    /// when negotiated, for each record-separator acknowledgement before
    /// sending the next record. Stdout and stderr are drained concurrently and
    /// the whole process has one five-second deadline.
    fn run_fish_transaction_bounded(
        command: &mut Command,
        input: &ShellTransactionInput,
        suffix: &str,
        label: &str,
    ) -> Output {
        let transport_preamble = input
            .wrapper
            .split_once("__mez_agent_wrapper_receive ")
            .map_or("", |(preamble, _)| preamble);
        let wrapper = decoded_fish_wrapper_source(&input.wrapper);
        let source = format!("{transport_preamble}{wrapper}\n{suffix}");
        let mut child = command
            .args(["-c", &source])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("the {label} process should spawn: {error}"));
        let stdout = child
            .stdout
            .take()
            .unwrap_or_else(|| panic!("the {label} stdout should be piped"));
        let stderr = child
            .stderr
            .take()
            .unwrap_or_else(|| panic!("the {label} stderr should be piped"));
        let observed = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let reader_observed = Arc::clone(&observed);
        let stdout_reader = thread::spawn(move || {
            let mut stdout = stdout;
            let mut chunk = [0_u8; 1024];
            loop {
                let count = stdout
                    .read(&mut chunk)
                    .expect("Fish transaction stdout should remain readable");
                if count == 0 {
                    break;
                }
                let (bytes, changed) = &*reader_observed;
                bytes
                    .lock()
                    .expect("the Fish stdout lock should remain available")
                    .extend_from_slice(&chunk[..count]);
                changed.notify_all();
            }
        });
        let stderr_reader = thread::spawn(move || {
            let mut stderr = stderr;
            let mut bytes = Vec::new();
            stderr
                .read_to_end(&mut bytes)
                .expect("Fish transaction stderr should remain readable");
            bytes
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let start_marker = b"\x1b]133;C;";
        if !wait_for_observed_shell_output(&observed, deadline, |bytes| {
            bytes
                .windows(start_marker.len())
                .any(|window| window == start_marker)
        }) {
            let _ = child.kill();
            let _ = child.wait();
            let observed = observed
                .0
                .lock()
                .expect("the Fish stdout lock should remain available")
                .clone();
            panic!(
                "the {label} process did not emit its start marker before the deadline: stdout={:?}",
                String::from_utf8_lossy(&observed)
            );
        }
        for (index, record) in input.payload.split_inclusive('\n').enumerate() {
            let stdin = child
                .stdin
                .as_mut()
                .unwrap_or_else(|| panic!("the {label} stdin should remain piped"));
            stdin
                .write_all(record.as_bytes())
                .unwrap_or_else(|error| panic!("the {label} payload should be written: {error}"));
            stdin
                .flush()
                .unwrap_or_else(|error| panic!("the {label} payload should be flushed: {error}"));
            if input.payload_receiver_acknowledgements
                && !wait_for_observed_shell_output(&observed, deadline, |bytes| {
                    bytes.iter().filter(|byte| **byte == 0x1e).count() > index
                })
            {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "the {label} process did not acknowledge payload record {} before the deadline",
                    index + 1
                );
            }
        }
        drop(child.stdin.take());

        let status = loop {
            if let Some(status) = child.try_wait().unwrap_or_else(|error| {
                panic!("the {label} process should remain observable: {error}")
            }) {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the {label} process exceeded its five-second deadline");
            }
            thread::sleep(Duration::from_millis(10));
        };
        stdout_reader
            .join()
            .expect("the Fish stdout reader should finish");
        let stderr = stderr_reader
            .join()
            .expect("the Fish stderr reader should finish");
        let stdout = observed
            .0
            .lock()
            .expect("the Fish stdout lock should remain available")
            .clone();
        Output {
            status,
            stdout,
            stderr,
        }
    }

    /// Decodes the generated POSIX wrapper source from its bounded interactive
    /// assignment transport.
    ///
    /// Structural tests should assert shell semantics against the reconstructed
    /// source while separate bounds assertions cover its physical delivery
    /// records. Standard base64 contains no single quotes, so the transport's
    /// quoted assignment values can be extracted without a shell parser.
    fn decoded_posix_wrapper_source(transport: &str) -> String {
        const FIRST_PREFIX: &str = "MEZ_WRAPPER_B64='";
        const APPEND_PREFIX: &str = "MEZ_WRAPPER_B64=$MEZ_WRAPPER_B64'";
        let mut encoded = String::new();
        for line in transport.lines() {
            let chunk = line
                .strip_prefix(FIRST_PREFIX)
                .or_else(|| line.strip_prefix(APPEND_PREFIX));
            if let Some(chunk) = chunk {
                encoded.push_str(
                    chunk
                        .split_once('\'')
                        .map(|(chunk, _)| chunk)
                        .expect("wrapper base64 assignments should remain shell quoted"),
                );
            }
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("wrapper transport should contain valid standard base64");
        String::from_utf8(decoded).expect("generated wrapper source should be valid UTF-8")
    }

    /// Decodes generated Fish wrapper source from its bounded interactive
    /// assignment transport.
    ///
    /// Structural tests assert Fish semantics against this reconstructed
    /// source while transport tests independently enforce physical line bounds.
    fn decoded_fish_wrapper_source(transport: &str) -> String {
        const RECORD_SUFFIX: &str = "; printf '\\036'";
        let mut encoded = String::new();
        for line in transport.lines() {
            if let Some(chunk) = line.strip_suffix(RECORD_SUFFIX)
                && !chunk.starts_with("__MEZ_WRAPPER_SOURCE_END_")
            {
                encoded.push_str(chunk);
            }
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("Fish wrapper transport should contain valid standard base64");
        String::from_utf8(decoded).expect("generated Fish wrapper source should be valid UTF-8")
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
            None,
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

    mod environment_resolution;
    mod path_resolution;
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

    /// Verifies dependency-free foreign bootstrap uses one portable rendezvous
    /// line, waits for deferred source, executes it, and reports both lifecycle
    /// records without requiring a Mezzanine executable in the target shell.
    #[test]
    fn dependency_free_foreign_loader_is_portable_and_executes_deferred_source() {
        let marker = marker();
        let input = dependency_free_foreign_shell_loader_input(
            "printf 'dependency-free-loader-ok\\n'",
            Path::new("/bin/sh"),
            ShellClassification::PosixSh,
            None,
            marker.as_str(),
        )
        .expect("the dependency-free loader should render");

        assert_eq!(input.command.lines().count(), 1);
        assert!(
            input.command.trim_end().len() <= 700,
            "rendezvous command exceeded portable PTY input: {} bytes",
            input.command.trim_end().len()
        );
        assert!(!input.command.contains("dependency-free-loader-ok"));
        assert!(input.payload.lines().all(|line| line.len() <= 700));

        let output = run_foreign_loader_exchange(&input.command, &input.payload);
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("mez_foreign_loader=ready"), "{stdout:?}");
        assert!(stdout.contains("dependency-free-loader-ok"), "{stdout:?}");
        assert!(stdout.contains("mez_foreign_loader=exited"), "{stdout:?}");
    }

    /// Verifies multi-frame Bash RX2 transport declares the exact emitted
    /// DATA record count.
    ///
    /// Frame boundaries need not align with the base64 line record size, so
    /// each frame's record count rounds up independently. The admission and
    /// END records must declare the sum of per-frame counts rather than the
    /// whole-string ceiling, otherwise the receiver's chunk loop exits before
    /// the final frame and misreads a frame header as the END record.
    #[test]
    fn bash_private_handoff_transport_declares_emitted_data_record_count() {
        let token = marker();
        let proof = MarkerToken::new("00112233445566778899aabbccddeeff")
            .expect("the handoff proof should be valid");
        let source = "# comment\n".repeat(64 * 1024);
        let input =
            bash_private_handoff_source_input(&source, &token, "multi-frame-accounting", &proof);
        let declared_chunks: usize = input
            .wrapper
            .split_whitespace()
            .nth(5)
            .expect("the admission record should declare a chunk count")
            .parse()
            .expect("the declared chunk count should be numeric");
        let frame_lines = input
            .receiver_payload
            .lines()
            .filter(|line| line.starts_with("MEZ_BASH_RX2_FRAME "))
            .collect::<Vec<_>>();
        assert!(
            frame_lines.len() >= 2,
            "the probe source should span multiple frames"
        );
        let frame_chunk_sum: usize = frame_lines
            .iter()
            .map(|line| {
                line.split_whitespace()
                    .nth(6)
                    .expect("a frame record should declare its chunk count")
                    .parse::<usize>()
                    .expect("the frame chunk count should be numeric")
            })
            .sum();
        assert_eq!(
            frame_chunk_sum, declared_chunks,
            "per-frame accounting must match the admission record"
        );
        let data_records = input
            .receiver_payload
            .lines()
            .filter(|line| line.starts_with("MEZ_BASH_RX2_DATA"))
            .count();
        assert_eq!(
            data_records, declared_chunks,
            "the admission record must match the emitted DATA records"
        );
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

    /// Verifies shell commands that use ordinary file-editing programs remain
    /// valid shell source so permission policy and sandbox enforcement decide
    /// whether they may execute.
    #[test]
    fn agent_shell_validation_allows_file_editing_commands() {
        for command in [
            "sed -i 's/old/new/' README.md",
            "printf '%s\\n' updated > README.md",
            "python3 -c \"from pathlib import Path; Path('README.md').write_text('updated\\n')\"",
            "git apply change.patch",
        ] {
            validate_agent_authored_shell_command(command)
                .unwrap_or_else(|error| panic!("{command}: {error:?}"));
        }
    }

    /// Verifies semantic action names remain valid inert data when passed as
    /// arguments to ordinary tools or included in quoted documentation text.
    #[test]
    fn agent_shell_validation_allows_semantic_action_names_as_data() {
        for command in [
            "rg apply_patch",
            "printf '%s' apply_patch",
            "printf '%s' 'run apply_patch through MAAP'",
            "rg 'apply_patch' crates/mez-agent",
            "value='apply_patch'; printf '%s' \"$value\"",
        ] {
            validate_agent_authored_shell_command(command)
                .unwrap_or_else(|error| panic!("{command}: {error:?}"));
        }
    }
}
