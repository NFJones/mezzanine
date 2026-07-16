//! Runtime support for command-backed window status pills.
//!
//! This module owns the configuration model, active-template detection, bounded
//! command execution, and cache state for `#{pill.<name>}` window status fields.
//! Rendering receives only cached text so terminal frame rendering stays pure
//! and command execution only happens for pills referenced by the active
//! `frames.window.right_status` template.

use super::{BTreeMap, Command, Duration, MezError, Result, Stdio, Value, current_unix_millis};
use std::io::Read;
use wait_timeout::ChildExt;

/// Default timeout for one status pill command execution.
pub(super) const DEFAULT_STATUS_PILL_TIMEOUT_MS: u64 = 750;
/// Default maximum number of Unicode scalar values retained from command output.
pub(super) const DEFAULT_STATUS_PILL_MAX_OUTPUT_CHARS: usize = 32;
/// Text shown for failed pills when configured with `show_error`.
pub(super) const STATUS_PILL_ERROR_TEXT: &str = "error";

/// Defines how a status pill handles empty command output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum RuntimeStatusPillEmptyBehavior {
    /// Hide the pill when the command emits no usable text.
    #[default]
    Hide,
    /// Show the label-only or empty pill.
    ShowEmpty,
    /// Keep the previous non-empty value when possible.
    KeepPrevious,
}

/// Defines how a status pill handles non-zero exits and timeouts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum RuntimeStatusPillErrorBehavior {
    /// Hide the pill when execution fails.
    #[default]
    Hide,
    /// Show a compact `error` value.
    ShowError,
    /// Keep the previous value when possible.
    KeepPrevious,
}

/// Runtime configuration for one command-backed status pill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeStatusPillDefinition {
    /// Optional label rendered before command output inside the pill.
    pub(super) label: Option<String>,
    /// Shell command executed to refresh the pill value.
    pub(super) command: String,
    /// Minimum interval between command executions.
    pub(super) interval_ms: u64,
    /// Placeholder shown before the first command result.
    pub(super) initial: Option<String>,
    /// Per-command timeout.
    pub(super) timeout_ms: u64,
    /// Behavior for empty stdout after trimming and first-line selection.
    pub(super) empty_behavior: RuntimeStatusPillEmptyBehavior,
    /// Behavior for non-zero exits, spawn failures, and timeouts.
    pub(super) error_behavior: RuntimeStatusPillErrorBehavior,
    /// Maximum number of Unicode scalar values retained from output.
    pub(super) max_output_chars: usize,
    /// Optional style selector reserved for future theme differentiation.
    pub(super) style: Option<String>,
}

impl RuntimeStatusPillDefinition {
    /// Formats the display text for this pill from an optional value.
    fn display_text(&self, value: Option<&str>) -> String {
        let label = self.label.as_deref().unwrap_or_default().trim();
        let value = value.unwrap_or_default().trim();
        match (label.is_empty(), value.is_empty()) {
            (true, true) => String::new(),
            (true, false) => value.to_string(),
            (false, true) => label.to_string(),
            (false, false) => format!("{label} {value}"),
        }
    }
}

/// Cached runtime state for one command-backed status pill.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RuntimeStatusPillState {
    /// Last rendered pill text, including any configured label.
    display: Option<String>,
    /// Next Unix millisecond timestamp at which the command may be refreshed.
    next_refresh_at_ms: u64,
}

/// Cache and scheduler for command-backed status pills.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RuntimeStatusPillCache {
    states: BTreeMap<String, RuntimeStatusPillState>,
}

impl RuntimeStatusPillCache {
    /// Refreshes active pill definitions and returns display strings keyed by pill name.
    pub(super) fn refresh_active(
        &mut self,
        definitions: &BTreeMap<String, RuntimeStatusPillDefinition>,
        template: &str,
    ) -> BTreeMap<String, String> {
        let active_names = runtime_status_pill_names_from_template(template);
        self.states
            .retain(|name, _| active_names.contains_key(name.as_str()));
        let mut output = BTreeMap::new();
        let now_ms = current_unix_millis();
        for name in active_names.keys() {
            let Some(definition) = definitions.get(name.as_str()) else {
                continue;
            };
            let state = self.states.entry(name.clone()).or_default();
            if state.display.is_none() {
                state.display = definition
                    .initial
                    .as_deref()
                    .map(|initial| definition.display_text(Some(initial)))
                    .filter(|value| !value.is_empty());
            }
            if state.next_refresh_at_ms <= now_ms {
                let next_display =
                    runtime_status_pill_execute(definition, state.display.as_deref());
                state.display = next_display;
                state.next_refresh_at_ms = now_ms.saturating_add(definition.interval_ms.max(1_000));
            }
            if let Some(display) = state.display.as_ref().filter(|value| !value.is_empty()) {
                output.insert(name.clone(), display.clone());
            }
        }
        output
    }
}

/// Returns the command-backed pill names referenced by a status template.
pub(super) fn runtime_status_pill_names_from_template(template: &str) -> BTreeMap<String, ()> {
    let mut names = BTreeMap::new();
    let mut remaining = template;
    while let Some(start) = remaining.find("#{") {
        let after_start = &remaining[start + 2..];
        let Some(end) = after_start.find('}') else {
            break;
        };
        let field = &after_start[..end];
        if let Some(name) = field
            .strip_prefix("pill.")
            .and_then(runtime_status_pill_name)
        {
            names.insert(name.to_string(), ());
        }
        remaining = &after_start[end + 1..];
    }
    names
}

/// Parses status pill definitions from the effective runtime configuration.
pub(super) fn runtime_status_pill_definitions_from_config(
    root: &Value,
) -> Result<BTreeMap<String, RuntimeStatusPillDefinition>> {
    let Some(frames) = root.get("frames").and_then(Value::as_object) else {
        return Ok(BTreeMap::new());
    };
    let Some(window) = frames.get("window").and_then(Value::as_object) else {
        return Ok(BTreeMap::new());
    };
    let Some(pills) = window.get("pills") else {
        return Ok(BTreeMap::new());
    };
    let pills = pills
        .as_object()
        .ok_or_else(|| MezError::config("frames.window.pills must be a table"))?;
    let mut definitions = BTreeMap::new();
    for (name, value) in pills {
        let valid_name = runtime_status_pill_name(name).ok_or_else(|| {
            MezError::config(format!(
                "frames.window.pills.{name} name must contain only ASCII letters, digits, underscores, or hyphens"
            ))
        })?;
        let object = value.as_object().ok_or_else(|| {
            MezError::config(format!("frames.window.pills.{name} must be a table"))
        })?;
        for key in object.keys() {
            if !matches!(
                key.as_str(),
                "label"
                    | "command"
                    | "interval_seconds"
                    | "initial"
                    | "timeout_ms"
                    | "empty_behavior"
                    | "error_behavior"
                    | "max_output_chars"
                    | "style"
            ) {
                return Err(MezError::config(format!(
                    "frames.window.pills.{name}.{key} is not a supported status pill setting"
                )));
            }
        }
        let command = runtime_status_pill_string(object.get("command"), "command", name)?;
        let interval_seconds = object
            .get("interval_seconds")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                MezError::config(format!(
                    "frames.window.pills.{name}.interval_seconds must be a positive integer"
                ))
            })?;
        if interval_seconds == 0 {
            return Err(MezError::config(format!(
                "frames.window.pills.{name}.interval_seconds must be a positive integer"
            )));
        }
        let timeout_ms = match object.get("timeout_ms") {
            Some(value) => value.as_u64().filter(|value| *value > 0).ok_or_else(|| {
                MezError::config(format!(
                    "frames.window.pills.{name}.timeout_ms must be a positive integer"
                ))
            })?,
            None => DEFAULT_STATUS_PILL_TIMEOUT_MS,
        };
        let max_output_chars = match object.get("max_output_chars") {
            Some(value) => value
                .as_u64()
                .filter(|value| *value > 0)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    MezError::config(format!(
                        "frames.window.pills.{name}.max_output_chars must be a positive integer"
                    ))
                })?,
            None => DEFAULT_STATUS_PILL_MAX_OUTPUT_CHARS,
        };
        definitions.insert(
            valid_name.to_string(),
            RuntimeStatusPillDefinition {
                label: runtime_status_pill_optional_string(object.get("label"), "label", name)?,
                command,
                interval_ms: interval_seconds.saturating_mul(1_000),
                initial: runtime_status_pill_optional_string(
                    object.get("initial"),
                    "initial",
                    name,
                )?,
                timeout_ms,
                empty_behavior: runtime_status_pill_empty_behavior(
                    object.get("empty_behavior"),
                    name,
                )?,
                error_behavior: runtime_status_pill_error_behavior(
                    object.get("error_behavior"),
                    name,
                )?,
                max_output_chars,
                style: runtime_status_pill_optional_string(object.get("style"), "style", name)?,
            },
        );
    }
    Ok(definitions)
}

/// Returns a valid pill name, rejecting empty and non-identifier names.
fn runtime_status_pill_name(name: &str) -> Option<&str> {
    (!name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then_some(name)
}

/// Reads a required non-empty status pill string setting.
fn runtime_status_pill_string(value: Option<&Value>, key: &str, name: &str) -> Result<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            MezError::config(format!(
                "frames.window.pills.{name}.{key} must be a non-empty string"
            ))
        })
}

/// Reads an optional non-empty status pill string setting.
fn runtime_status_pill_optional_string(
    value: Option<&Value>,
    key: &str,
    name: &str,
) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(value) = value.as_str().filter(|value| !value.trim().is_empty()) else {
        return Err(MezError::config(format!(
            "frames.window.pills.{name}.{key} must be a non-empty string"
        )));
    };
    Ok(Some(value.to_string()))
}

/// Reads empty-output behavior from a pill definition.
fn runtime_status_pill_empty_behavior(
    value: Option<&Value>,
    name: &str,
) -> Result<RuntimeStatusPillEmptyBehavior> {
    match value.and_then(Value::as_str).unwrap_or("hide") {
        "hide" => Ok(RuntimeStatusPillEmptyBehavior::Hide),
        "show_empty" => Ok(RuntimeStatusPillEmptyBehavior::ShowEmpty),
        "keep_previous" => Ok(RuntimeStatusPillEmptyBehavior::KeepPrevious),
        _ => Err(MezError::config(format!(
            "frames.window.pills.{name}.empty_behavior must be hide, show_empty, or keep_previous"
        ))),
    }
}

/// Reads execution-error behavior from a pill definition.
fn runtime_status_pill_error_behavior(
    value: Option<&Value>,
    name: &str,
) -> Result<RuntimeStatusPillErrorBehavior> {
    match value.and_then(Value::as_str).unwrap_or("hide") {
        "hide" => Ok(RuntimeStatusPillErrorBehavior::Hide),
        "show_error" => Ok(RuntimeStatusPillErrorBehavior::ShowError),
        "keep_previous" => Ok(RuntimeStatusPillErrorBehavior::KeepPrevious),
        _ => Err(MezError::config(format!(
            "frames.window.pills.{name}.error_behavior must be hide, show_error, or keep_previous"
        ))),
    }
}

/// Executes one status pill command and returns the next rendered display state.
fn runtime_status_pill_execute(
    definition: &RuntimeStatusPillDefinition,
    previous: Option<&str>,
) -> Option<String> {
    match runtime_status_pill_command_output(definition) {
        Ok(output) if output.is_empty() => match definition.empty_behavior {
            RuntimeStatusPillEmptyBehavior::Hide => None,
            RuntimeStatusPillEmptyBehavior::ShowEmpty => Some(definition.display_text(None)),
            RuntimeStatusPillEmptyBehavior::KeepPrevious => previous.map(ToOwned::to_owned),
        },
        Ok(output) => Some(definition.display_text(Some(&output))),
        Err(()) => match definition.error_behavior {
            RuntimeStatusPillErrorBehavior::Hide => None,
            RuntimeStatusPillErrorBehavior::ShowError => {
                Some(definition.display_text(Some(STATUS_PILL_ERROR_TEXT)))
            }
            RuntimeStatusPillErrorBehavior::KeepPrevious => previous.map(ToOwned::to_owned),
        },
    }
}

/// Runs the configured command with bounded timeout and normalized stdout.
fn runtime_status_pill_command_output(
    definition: &RuntimeStatusPillDefinition,
) -> std::result::Result<String, ()> {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(&definition.command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())?;
    let status = match child
        .wait_timeout(Duration::from_millis(definition.timeout_ms))
        .map_err(|_| ())?
    {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(());
        }
    };
    if !status.success() {
        return Err(());
    }
    let mut stdout = String::new();
    child
        .stdout
        .as_mut()
        .ok_or(())?
        .read_to_string(&mut stdout)
        .map_err(|_| ())?;
    Ok(runtime_status_pill_normalize_output(
        &stdout,
        definition.max_output_chars,
    ))
}

/// Normalizes command stdout for single-line status rendering.
fn runtime_status_pill_normalize_output(output: &str, max_chars: usize) -> String {
    output
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(max_chars)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        BTreeMap, DEFAULT_STATUS_PILL_MAX_OUTPUT_CHARS, DEFAULT_STATUS_PILL_TIMEOUT_MS,
        RuntimeStatusPillCache, RuntimeStatusPillDefinition, RuntimeStatusPillEmptyBehavior,
        RuntimeStatusPillErrorBehavior, runtime_status_pill_names_from_template,
    };

    /// Verifies that active pill detection follows the same `#{...}` field
    /// boundary as status rendering and ignores malformed or unrelated fields.
    #[test]
    fn detects_only_named_status_pills_from_template() {
        let names = runtime_status_pill_names_from_template(
            "#{pill.cpu} #{datetime.local} #{pill.mem_1} #{pill.bad.name} #{pill.docker-running}",
        );

        assert!(names.contains_key("cpu"));
        assert!(names.contains_key("mem_1"));
        assert!(names.contains_key("docker-running"));
        assert!(!names.contains_key("bad.name"));
        assert_eq!(names.len(), 3);
    }

    /// Verifies that cached status pill refreshes are lazy: definitions that are
    /// not referenced by the active right-status template are not executed.
    #[test]
    fn refresh_active_skips_unreferenced_status_pills() {
        let mut definitions = BTreeMap::new();
        definitions.insert(
            "used".to_string(),
            RuntimeStatusPillDefinition {
                label: Some("USED".to_string()),
                command: "printf ok".to_string(),
                interval_ms: 1_000,
                initial: None,
                timeout_ms: DEFAULT_STATUS_PILL_TIMEOUT_MS,
                empty_behavior: RuntimeStatusPillEmptyBehavior::Hide,
                error_behavior: RuntimeStatusPillErrorBehavior::Hide,
                max_output_chars: DEFAULT_STATUS_PILL_MAX_OUTPUT_CHARS,
                style: None,
            },
        );
        definitions.insert(
            "unused".to_string(),
            RuntimeStatusPillDefinition {
                label: Some("UNUSED".to_string()),
                command: "exit 7".to_string(),
                interval_ms: 1_000,
                initial: None,
                timeout_ms: DEFAULT_STATUS_PILL_TIMEOUT_MS,
                empty_behavior: RuntimeStatusPillEmptyBehavior::Hide,
                error_behavior: RuntimeStatusPillErrorBehavior::ShowError,
                max_output_chars: DEFAULT_STATUS_PILL_MAX_OUTPUT_CHARS,
                style: None,
            },
        );

        let mut cache = RuntimeStatusPillCache::default();
        let values = cache.refresh_active(&definitions, "#{pill.used}");

        assert_eq!(values.get("used").map(String::as_str), Some("USED ok"));
        assert!(!values.contains_key("unused"));
    }
}
