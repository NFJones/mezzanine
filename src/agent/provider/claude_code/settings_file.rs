//! Temporary settings and system-prompt files for Claude Code subprocesses.

use super::*;

/// Owns a per-invocation Claude Code settings file and removes it when the
/// subprocess invocation completes or fails before spawn.
pub(super) struct ClaudeCodeSettingsFile {
    path: PathBuf,
}

impl ClaudeCodeSettingsFile {
    /// Writes the Claude settings backed fields for one subprocess invocation.
    pub(super) fn write(model: &str, structured_output_allowed: bool) -> Result<Self> {
        let suffix = CLAUDE_CODE_SETTINGS_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mez-claude-code-settings-{}-{suffix}.json",
            std::process::id()
        ));
        let mut allowed_tools = Vec::new();
        if structured_output_allowed {
            allowed_tools.push(CLAUDE_CODE_STRUCTURED_OUTPUT_TOOL);
        }
        let settings = serde_json::json!({
            "model": model,
            "permissions": {
                "allow": allowed_tools,
                "deny": CLAUDE_CODE_DISALLOWED_NATIVE_TOOLS
                    .split(',')
                    .collect::<Vec<_>>(),
            },
        });
        let bytes = serde_json::to_vec_pretty(&settings).map_err(|error| {
            MezError::invalid_state(format!(
                "Claude Code settings serialization failed: {}",
                redact_claude_code_text(&error.to_string())
            ))
        })?;
        std::fs::write(&path, bytes).map_err(|error| {
            MezError::invalid_state(format!(
                "Claude Code settings file write failed: {}",
                redact_claude_code_text(&error.to_string())
            ))
        })?;
        Ok(Self { path })
    }

    /// Returns the filesystem path passed to Claude Code with `--settings`.
    pub(super) fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for ClaudeCodeSettingsFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Owns a per-invocation Claude Code system prompt file and removes it when
/// the subprocess invocation completes or fails before spawn.
pub(super) struct ClaudeCodeSystemPromptFile {
    path: PathBuf,
}

impl ClaudeCodeSystemPromptFile {
    /// Writes the generated system prompt for one subprocess invocation.
    pub(super) fn write(system_prompt: &str) -> Result<Option<Self>> {
        if system_prompt.is_empty() {
            return Ok(None);
        }
        let suffix = CLAUDE_CODE_SETTINGS_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mez-claude-code-system-prompt-{}-{suffix}.md",
            std::process::id()
        ));
        std::fs::write(&path, system_prompt).map_err(|error| {
            MezError::invalid_state(format!(
                "Claude Code system prompt file write failed: {}",
                redact_claude_code_text(&error.to_string())
            ))
        })?;
        Ok(Some(Self { path }))
    }

    /// Returns the filesystem path passed to Claude Code with
    /// `--append-system-prompt-file`.
    pub(super) fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for ClaudeCodeSystemPromptFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
