//! Validated typed child launches and bounded owner-only artifacts.
//!
//! This contract accepts literal argv and explicit materialized-file references,
//! never raw shell fragments. Renderers consume the same validated artifact set;
//! product adapters retain responsibility for approval and process execution.

use super::{AgentShellValidationError, AgentShellValidationResult, Path};
use std::collections::BTreeSet;

/// Maximum number of bounded artifacts accepted by one typed child launch.
pub const SHELL_LAUNCH_MAX_ARTIFACTS: usize = 16;
/// Maximum bytes accepted in one typed child-launch artifact.
pub const SHELL_LAUNCH_MAX_ARTIFACT_BYTES: usize = 256 * 1024;
/// Maximum aggregate artifact bytes accepted by one typed child launch.
pub const SHELL_LAUNCH_MAX_TOTAL_ARTIFACT_BYTES: usize = 1024 * 1024;

/// Stable identifier for one materialized typed child-launch artifact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ShellLaunchArtifactId(String);

impl ShellLaunchArtifactId {
    /// Validates one bounded identifier used only for typed artifact lookup.
    pub fn new(value: impl Into<String>) -> AgentShellValidationResult<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
        if !valid {
            return Err(AgentShellValidationError::invalid_args(
                "typed child artifact ids must be 1-32 ASCII alphanumeric, underscore, or hyphen bytes",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the validated identifier without exposing a shell variable.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One bounded owner-only file materialized for a typed child launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellLaunchArtifact {
    /// Stable identifier referenced by typed child arguments.
    pub id: ShellLaunchArtifactId,
    /// Inert file bytes written without shell evaluation.
    pub content: Vec<u8>,
    /// Owner-only file mode, restricted to read-only or read-write data.
    pub mode: u32,
}

impl ShellLaunchArtifact {
    /// Builds one bounded owner-only launch artifact.
    pub fn new(
        id: ShellLaunchArtifactId,
        content: Vec<u8>,
        mode: u32,
    ) -> AgentShellValidationResult<Self> {
        if content.len() > SHELL_LAUNCH_MAX_ARTIFACT_BYTES {
            return Err(AgentShellValidationError::invalid_args(
                "typed child artifact exceeds the per-artifact byte limit",
            ));
        }
        if !matches!(mode, 0o400 | 0o600) {
            return Err(AgentShellValidationError::invalid_args(
                "typed child artifact mode must be owner-only 0400 or 0600",
            ));
        }
        Ok(Self { id, content, mode })
    }
}

/// One argument in a typed isolated-child process launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellChildArgument {
    /// A literal argv element rendered with shell-specific quoting.
    Literal(String),
    /// The temporary command file materialized by the transaction wrapper.
    MaterializedCommandFile,
    /// Exact path of one launch artifact materialized by the transport.
    MaterializedArtifact(ShellLaunchArtifactId),
    /// One validated `NAME=<materialized path>` argv element.
    MaterializedPathBinding {
        /// Validated non-secret binding name.
        name: String,
        /// Artifact whose canonical materialized path supplies the value.
        artifact: ShellLaunchArtifactId,
    },
}

/// Typed executable and argv for a child process.
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
    /// Bounded inert files materialized before this child starts.
    pub artifacts: Vec<ShellLaunchArtifact>,
    /// Optional runtime-owned descriptor used to capture trusted child status.
    ///
    /// The transaction wrapper redirects this descriptor to a private temporary
    /// file and emits that file through a framing channel separate from child
    /// stdout and stderr after the process exits.
    pub status_fd: Option<u8>,
    /// Whether the child inherits the pane terminal under shell job control.
    ///
    /// The default remains an isolated session with stdin detached. Blocking
    /// terminal applications opt in so the interactive shell can make the
    /// child the foreground PTY process and wait for its exit.
    pub inherited_terminal: bool,
}

impl ShellChildLaunch {
    /// Validates one typed child launch before shell rendering.
    pub fn new(
        executable: impl Into<String>,
        arguments: Vec<ShellChildArgument>,
    ) -> AgentShellValidationResult<Self> {
        Self::new_with_artifacts(executable, arguments, Vec::new())
    }

    /// Validates one typed child launch and its bounded artifact set.
    pub fn new_with_artifacts(
        executable: impl Into<String>,
        arguments: Vec<ShellChildArgument>,
        artifacts: Vec<ShellLaunchArtifact>,
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
        if artifacts.len() > SHELL_LAUNCH_MAX_ARTIFACTS {
            return Err(AgentShellValidationError::invalid_args(
                "typed child launch exceeds the artifact count limit",
            ));
        }
        let mut artifact_ids = BTreeSet::new();
        let mut total_artifact_bytes = 0usize;
        for artifact in &artifacts {
            ShellLaunchArtifactId::new(artifact.id.as_str())?;
            if artifact.content.len() > SHELL_LAUNCH_MAX_ARTIFACT_BYTES
                || !matches!(artifact.mode, 0o400 | 0o600)
            {
                return Err(AgentShellValidationError::invalid_args(
                    "typed child launch contains an invalid artifact",
                ));
            }
            total_artifact_bytes = total_artifact_bytes.saturating_add(artifact.content.len());
            if !artifact_ids.insert(artifact.id.clone()) {
                return Err(AgentShellValidationError::invalid_args(
                    "typed child launch artifact ids must be unique",
                ));
            }
        }
        if total_artifact_bytes > SHELL_LAUNCH_MAX_TOTAL_ARTIFACT_BYTES {
            return Err(AgentShellValidationError::invalid_args(
                "typed child launch exceeds the aggregate artifact byte limit",
            ));
        }
        for argument in &arguments {
            let artifact = match argument {
                ShellChildArgument::MaterializedArtifact(artifact) => Some(artifact),
                ShellChildArgument::MaterializedPathBinding { name, artifact } => {
                    let valid_name = !name.is_empty()
                        && name.len() <= 32
                        && name.bytes().enumerate().all(|(index, byte)| {
                            byte == b'_'
                                || byte.is_ascii_uppercase()
                                || (index > 0 && byte.is_ascii_digit())
                        });
                    if !valid_name {
                        return Err(AgentShellValidationError::invalid_args(
                            "typed child artifact binding names must match [A-Z_][A-Z0-9_]{0,31}",
                        ));
                    }
                    Some(artifact)
                }
                ShellChildArgument::Literal(_) | ShellChildArgument::MaterializedCommandFile => {
                    None
                }
            };
            if artifact.is_some_and(|artifact| !artifact_ids.contains(artifact)) {
                return Err(AgentShellValidationError::invalid_args(
                    "typed child argument references an unknown artifact id",
                ));
            }
        }
        Ok(Self {
            executable,
            arguments,
            artifacts,
            status_fd: None,
            inherited_terminal: false,
        })
    }

    /// Selects inherited pane-terminal ownership for a blocking child.
    pub fn with_inherited_terminal(mut self) -> Self {
        self.inherited_terminal = true;
        self
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
