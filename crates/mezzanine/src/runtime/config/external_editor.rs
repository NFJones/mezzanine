//! Runtime external-editor configuration.
//!
//! This module converts the schema-validated structured editor command into a
//! typed runtime value. It never invokes a shell or resolves executables; the
//! external-editor session subsystem owns path resolution and process launch.

use serde_json::Value;

use crate::error::{MezError, Result};

/// Structured blocking-editor command candidates in preference order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeExternalEditorConfig {
    /// Preferred editor argv before draft-path substitution.
    pub(crate) command: Vec<String>,
    /// Fallback argv candidates used only after lookup or spawn failure.
    pub(crate) fallback: Vec<Vec<String>>,
}

impl Default for RuntimeExternalEditorConfig {
    fn default() -> Self {
        Self {
            command: vec!["editor".to_string(), "{file}".to_string()],
            fallback: vec![
                vec!["vim".to_string(), "{file}".to_string()],
                vec!["nano".to_string(), "{file}".to_string()],
                vec!["vi".to_string(), "{file}".to_string()],
            ],
        }
    }
}

/// Parses the structured external-editor configuration from effective config.
pub(crate) fn runtime_external_editor_config_from_config(
    root: &Value,
) -> Result<RuntimeExternalEditorConfig> {
    let defaults = RuntimeExternalEditorConfig::default();
    let Some(editor) = root.get("external_editor").and_then(Value::as_object) else {
        return Ok(defaults);
    };
    Ok(RuntimeExternalEditorConfig {
        command: editor
            .get("command")
            .map(|value| runtime_editor_argv(value, "external_editor.command"))
            .transpose()?
            .unwrap_or(defaults.command),
        fallback: editor
            .get("fallback")
            .map(runtime_editor_fallback)
            .transpose()?
            .unwrap_or(defaults.fallback),
    })
}

/// Parses one non-empty string argv candidate defensively at runtime.
fn runtime_editor_argv(value: &Value, path: &str) -> Result<Vec<String>> {
    let argv = value
        .as_array()
        .filter(|argv| !argv.is_empty())
        .ok_or_else(|| MezError::config(format!("{path} must be a non-empty argv string array")))?;
    argv.iter()
        .map(|argument| {
            argument
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| MezError::config(format!("{path} must contain only strings")))
        })
        .collect()
}

/// Parses the ordered fallback candidate array defensively at runtime.
fn runtime_editor_fallback(value: &Value) -> Result<Vec<Vec<String>>> {
    value
        .as_array()
        .ok_or_else(|| {
            MezError::config("external_editor.fallback must be an array of argv string arrays")
        })?
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            runtime_editor_argv(candidate, &format!("external_editor.fallback[{index}]"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies runtime parsing preserves argv boundaries and ordered fallback
    /// candidates instead of flattening them into shell command strings.
    #[test]
    fn parses_structured_editor_candidates() {
        let config = runtime_external_editor_config_from_config(&serde_json::json!({
            "external_editor": {
                "command": ["hx", "--working-dir", ".", "{file}"],
                "fallback": [["nvim", "{file}"], ["vi"]]
            }
        }))
        .unwrap();

        assert_eq!(config.command, ["hx", "--working-dir", ".", "{file}"]);
        assert_eq!(
            config.fallback,
            vec![
                vec!["nvim".to_string(), "{file}".to_string()],
                vec!["vi".to_string()],
            ]
        );
    }
}
