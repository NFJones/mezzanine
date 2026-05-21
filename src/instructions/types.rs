//! Instruction discovery data types.
//!
//! These types define the stable boundary between the harness that executes the
//! shell discovery command and the parser that consumes its escaped records.

use std::path::PathBuf;

/// Configuration for discovering project instruction files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionDiscoveryConfig {
    /// File names to check in each directory, in precedence order.
    pub project_filenames: Vec<String>,
    /// Maximum bytes to read from each discovered file.
    pub max_bytes: usize,
    /// Whether hidden directories participate in the ancestor walk.
    pub include_hidden_directories: bool,
}

/// Shell execution plan for discovering instruction files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionDiscoveryPlan {
    /// Absolute repository root used as the shell working directory.
    pub project_root: PathBuf,
    /// Absolute path to the task file or directory.
    pub task_path: PathBuf,
    /// POSIX-compatible command to execute through the pane shell.
    pub shell_command: String,
    /// Maximum bytes to read from each file.
    pub max_bytes: usize,
}

/// One instruction file decoded from discovery command output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredInstructionFile {
    /// Path emitted by the discovery command.
    pub path: String,
    /// Directory scope where the instruction file was found.
    pub scope_root: String,
    /// Full file size reported by the shell command.
    pub bytes: usize,
    /// Whether content was truncated to the configured byte limit.
    pub truncated: bool,
    /// Escaped and decoded file content.
    pub content: String,
}

impl Default for InstructionDiscoveryConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            project_filenames: vec!["AGENTS.md".to_string()],
            max_bytes: 32_768,
            include_hidden_directories: false,
        }
    }
}
