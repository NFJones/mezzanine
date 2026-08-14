//! Runtime support for command-backed window status pills.
//!
//! This module owns the configuration model, active-template detection, bounded
//! command execution, and cache state for `#{pill.<name>}` window status fields.
//! Rendering receives only cached text and schedules generation-stamped work so
//! terminal frame rendering stays pure. A supervised worker executes commands
//! only for pills referenced by the active `frames.window.right_status`
//! template, and the actor applies typed completions to this cache.

use super::{BTreeMap, Duration, MezError, Result, Value, current_unix_millis};
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Default timeout for one status pill command execution.
pub(super) const DEFAULT_STATUS_PILL_TIMEOUT_MS: u64 = 750;
/// Default maximum number of Unicode scalar values retained from command output.
pub(super) const DEFAULT_STATUS_PILL_MAX_OUTPUT_CHARS: usize = 32;
/// Maximum bytes retained from one status pill stdout stream.
const STATUS_PILL_OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;
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
    /// Refresh currently owned by the asynchronous worker.
    pending_generation: Option<u64>,
}

/// Immutable external work for one command-backed status pill refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStatusPillRefreshPlan {
    /// Configured pill name.
    name: String,
    /// Monotonic cache generation used to reject stale completions.
    generation: u64,
    /// Definition snapshot that produced this refresh.
    definition: RuntimeStatusPillDefinition,
}

#[cfg(test)]
impl RuntimeStatusPillRefreshPlan {
    /// Builds one deterministic command plan for async-runtime tests.
    pub(crate) fn for_tests(
        name: &str,
        generation: u64,
        command: &str,
        timeout_ms: u64,
        max_output_chars: usize,
    ) -> Self {
        Self {
            name: name.to_string(),
            generation,
            definition: RuntimeStatusPillDefinition {
                label: None,
                command: command.to_string(),
                interval_ms: 1_000,
                initial: None,
                timeout_ms,
                empty_behavior: RuntimeStatusPillEmptyBehavior::Hide,
                error_behavior: RuntimeStatusPillErrorBehavior::Hide,
                max_output_chars,
                style: None,
            },
        }
    }
}

/// Result of one bounded status pill command execution.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RuntimeStatusPillRefreshOutcome {
    /// Command exited successfully with normalized bounded stdout.
    Succeeded(String),
    /// Command failed, timed out, emitted invalid UTF-8, or could not be read.
    Failed,
}

/// Typed completion emitted by the asynchronous status pill worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStatusPillEvent {
    /// Original immutable refresh request.
    plan: RuntimeStatusPillRefreshPlan,
    /// Bounded command outcome.
    outcome: RuntimeStatusPillRefreshOutcome,
}

/// Cache and scheduler for command-backed status pills.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RuntimeStatusPillCache {
    states: BTreeMap<String, RuntimeStatusPillState>,
    pending_refreshes: Vec<RuntimeStatusPillRefreshPlan>,
    /// Cache-wide generation preventing reuse after pill removal and re-addition.
    next_generation: u64,
}

impl RuntimeStatusPillCache {
    /// Returns cached display strings and schedules due refreshes as external work.
    pub(super) fn render_active(
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
            if state.next_refresh_at_ms <= now_ms && state.pending_generation.is_none() {
                self.next_generation = self.next_generation.wrapping_add(1).max(1);
                let generation = self.next_generation;
                state.pending_generation = Some(generation);
                state.next_refresh_at_ms = now_ms.saturating_add(definition.interval_ms.max(1_000));
                self.pending_refreshes.push(RuntimeStatusPillRefreshPlan {
                    name: name.clone(),
                    generation,
                    definition: definition.clone(),
                });
            }
            if let Some(display) = state.display.as_ref().filter(|value| !value.is_empty()) {
                output.insert(name.clone(), display.clone());
            }
        }
        output
    }

    /// Drains scheduled refresh plans for the supervised external worker.
    pub(super) fn drain_refresh_plans(&mut self) -> Vec<RuntimeStatusPillRefreshPlan> {
        std::mem::take(&mut self.pending_refreshes)
    }

    /// Applies one current completion and reports whether visible text changed.
    pub(super) fn apply_event(
        &mut self,
        definitions: &BTreeMap<String, RuntimeStatusPillDefinition>,
        template: &str,
        event: RuntimeStatusPillEvent,
    ) -> Option<bool> {
        let active_names = runtime_status_pill_names_from_template(template);
        if !active_names.contains_key(event.plan.name.as_str()) {
            return None;
        }
        let definition = definitions.get(event.plan.name.as_str())?;
        let state = self.states.get_mut(event.plan.name.as_str())?;
        if state.pending_generation != Some(event.plan.generation) {
            return None;
        }
        state.pending_generation = None;
        if definition != &event.plan.definition {
            state.next_refresh_at_ms = 0;
            return Some(false);
        }
        let previous = state.display.clone();
        state.display = runtime_status_pill_display_from_outcome(
            definition,
            previous.as_deref(),
            event.outcome,
        );
        Some(state.display != previous)
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

/// Applies configured display policy to one external command outcome.
fn runtime_status_pill_display_from_outcome(
    definition: &RuntimeStatusPillDefinition,
    previous: Option<&str>,
    outcome: RuntimeStatusPillRefreshOutcome,
) -> Option<String> {
    match outcome {
        RuntimeStatusPillRefreshOutcome::Succeeded(output) if output.is_empty() => {
            match definition.empty_behavior {
                RuntimeStatusPillEmptyBehavior::Hide => None,
                RuntimeStatusPillEmptyBehavior::ShowEmpty => Some(definition.display_text(None)),
                RuntimeStatusPillEmptyBehavior::KeepPrevious => previous.map(ToOwned::to_owned),
            }
        }
        RuntimeStatusPillRefreshOutcome::Succeeded(output) => {
            Some(definition.display_text(Some(&output)))
        }
        RuntimeStatusPillRefreshOutcome::Failed => match definition.error_behavior {
            RuntimeStatusPillErrorBehavior::Hide => None,
            RuntimeStatusPillErrorBehavior::ShowError => {
                Some(definition.display_text(Some(STATUS_PILL_ERROR_TEXT)))
            }
            RuntimeStatusPillErrorBehavior::KeepPrevious => previous.map(ToOwned::to_owned),
        },
    }
}

/// Executes one status pill refresh outside serialized runtime ownership.
#[cfg(test)]
pub async fn execute_runtime_status_pill_refresh_plan_async(
    plan: RuntimeStatusPillRefreshPlan,
) -> RuntimeStatusPillEvent {
    execute_runtime_status_pill_refresh_plan_with_cancellation(plan, std::future::pending())
        .await
        .expect("a pending cancellation source cannot cancel status-pill execution")
}

/// Executes one refresh until completion, timeout, or lifecycle cancellation.
pub async fn execute_runtime_status_pill_refresh_plan_with_cancellation<C>(
    plan: RuntimeStatusPillRefreshPlan,
    cancellation: C,
) -> Option<RuntimeStatusPillEvent>
where
    C: std::future::Future<Output = ()>,
{
    let outcome = runtime_status_pill_command_output_async(&plan.definition, cancellation).await?;
    Some(RuntimeStatusPillEvent { plan, outcome })
}

/// Runs the configured command with bounded time and concurrently drained pipes.
async fn runtime_status_pill_command_output_async<C>(
    definition: &RuntimeStatusPillDefinition,
    cancellation: C,
) -> Option<RuntimeStatusPillRefreshOutcome>
where
    C: std::future::Future<Output = ()>,
{
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(&definition.command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let Ok(mut child) = command.spawn() else {
        return Some(RuntimeStatusPillRefreshOutcome::Failed);
    };
    let mut process_group = RuntimeStatusPillProcessGroupGuard::new(&child);
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        terminate_runtime_status_pill_process(&mut child, &process_group).await;
        process_group.disarm();
        return Some(RuntimeStatusPillRefreshOutcome::Failed);
    };
    let deadline = tokio::time::Instant::now() + Duration::from_millis(definition.timeout_ms);
    tokio::pin!(cancellation);
    let completed = tokio::select! {
        completed = async {
            tokio::join!(
                child.wait(),
                read_bounded_status_pill_pipe(stdout, STATUS_PILL_OUTPUT_LIMIT_BYTES),
                read_bounded_status_pill_pipe(stderr, 0),
            )
        } => completed,
        _ = tokio::time::sleep_until(deadline) => {
            terminate_runtime_status_pill_process(&mut child, &process_group).await;
            process_group.disarm();
            return Some(RuntimeStatusPillRefreshOutcome::Failed);
        }
        _ = &mut cancellation => {
            terminate_runtime_status_pill_process(&mut child, &process_group).await;
            process_group.disarm();
            return None;
        }
    };
    process_group.disarm();
    let (Ok(status), Ok(stdout), Ok(_stderr)) = completed else {
        return Some(RuntimeStatusPillRefreshOutcome::Failed);
    };
    if !status.success() {
        return Some(RuntimeStatusPillRefreshOutcome::Failed);
    }
    let Ok(stdout) = String::from_utf8(stdout) else {
        return Some(RuntimeStatusPillRefreshOutcome::Failed);
    };
    Some(RuntimeStatusPillRefreshOutcome::Succeeded(
        runtime_status_pill_normalize_output(&stdout, definition.max_output_chars),
    ))
}

/// Drains one child stream while retaining no more than `max_bytes`.
async fn read_bounded_status_pill_pipe<R>(mut pipe: R, max_bytes: usize) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut retained = Vec::with_capacity(max_bytes.min(8192));
    let mut chunk = [0u8; 8192];
    loop {
        let read = pipe.read(&mut chunk).await?;
        if read == 0 {
            return Ok(retained);
        }
        let accepted = max_bytes.saturating_sub(retained.len()).min(read);
        retained.extend_from_slice(&chunk[..accepted]);
    }
}

/// Best-effort private-process-group cleanup for status pill commands.
struct RuntimeStatusPillProcessGroupGuard {
    #[cfg(unix)]
    process_group_id: Option<i32>,
    armed: bool,
}

impl RuntimeStatusPillProcessGroupGuard {
    /// Arms cleanup for one spawned child process group.
    fn new(child: &tokio::process::Child) -> Self {
        Self {
            #[cfg(unix)]
            process_group_id: child.id().and_then(|id| i32::try_from(id).ok()),
            armed: true,
        }
    }

    /// Prevents cleanup after the direct child has been reaped.
    fn disarm(&mut self) {
        self.armed = false;
    }

    /// Terminates the private process group when supported.
    fn terminate(&self) {
        if !self.armed {
            return;
        }
        #[cfg(unix)]
        if let Some(process_group_id) = self.process_group_id {
            // SAFETY: the pid belongs to a child started in its own process group.
            unsafe {
                libc::kill(-process_group_id, libc::SIGKILL);
            }
        }
    }
}

impl Drop for RuntimeStatusPillProcessGroupGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// Terminates descendants and reaps the direct status pill child.
async fn terminate_runtime_status_pill_process(
    child: &mut tokio::process::Child,
    process_group: &RuntimeStatusPillProcessGroupGuard,
) {
    process_group.terminate();
    let _ = child.start_kill();
    let _ = child.wait().await;
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
        RuntimeStatusPillErrorBehavior, RuntimeStatusPillEvent, RuntimeStatusPillRefreshOutcome,
        execute_runtime_status_pill_refresh_plan_async, runtime_status_pill_names_from_template,
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
        let values = cache.render_active(&definitions, "#{pill.used}");

        assert!(!values.contains_key("used"));
        assert!(!values.contains_key("unused"));
        let plans = cache.drain_refresh_plans();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].name, "used");

        let repeated = cache.render_active(&definitions, "#{pill.used}");
        assert!(repeated.is_empty());
        assert!(cache.drain_refresh_plans().is_empty());
    }

    /// Verifies stale generations are ignored and current completions apply
    /// configured visible output policy exactly once.
    #[test]
    fn status_pill_cache_rejects_stale_completions() {
        let mut definitions = BTreeMap::new();
        definitions.insert(
            "used".to_string(),
            RuntimeStatusPillDefinition {
                label: Some("USED".to_string()),
                command: "printf ok".to_string(),
                interval_ms: 1_000,
                initial: Some("initial".to_string()),
                timeout_ms: DEFAULT_STATUS_PILL_TIMEOUT_MS,
                empty_behavior: RuntimeStatusPillEmptyBehavior::Hide,
                error_behavior: RuntimeStatusPillErrorBehavior::Hide,
                max_output_chars: DEFAULT_STATUS_PILL_MAX_OUTPUT_CHARS,
                style: None,
            },
        );
        let mut cache = RuntimeStatusPillCache::default();
        let visible = cache.render_active(&definitions, "#{pill.used}");
        assert_eq!(
            visible.get("used").map(String::as_str),
            Some("USED initial")
        );
        let plan = cache.drain_refresh_plans().remove(0);
        let mut stale = plan.clone();
        stale.generation = stale.generation.saturating_add(1);

        assert_eq!(
            cache.apply_event(
                &definitions,
                "#{pill.used}",
                RuntimeStatusPillEvent {
                    plan: stale,
                    outcome: RuntimeStatusPillRefreshOutcome::Succeeded("stale".to_string()),
                },
            ),
            None
        );
        assert_eq!(
            cache.apply_event(
                &definitions,
                "#{pill.used}",
                RuntimeStatusPillEvent {
                    plan,
                    outcome: RuntimeStatusPillRefreshOutcome::Succeeded("ready".to_string()),
                },
            ),
            Some(true)
        );
        let visible = cache.render_active(&definitions, "#{pill.used}");
        assert_eq!(visible.get("used").map(String::as_str), Some("USED ready"));
    }

    /// Verifies stdout and stderr are drained concurrently under one deadline,
    /// output is normalized, and timed-out helpers fail without hanging.
    #[tokio::test(flavor = "current_thread")]
    async fn status_pill_executor_bounds_output_and_timeout() {
        let successful = super::RuntimeStatusPillRefreshPlan::for_tests(
            "pipe-fill",
            1,
            "printf '  ready  \\nignored'; head -c 2097152 /dev/zero >&2",
            1_000,
            5,
        );
        let completed = execute_runtime_status_pill_refresh_plan_async(successful).await;
        assert_eq!(
            completed.outcome,
            RuntimeStatusPillRefreshOutcome::Succeeded("ready".to_string())
        );

        let timed_out =
            super::RuntimeStatusPillRefreshPlan::for_tests("timeout", 1, "sleep 1", 20, 32);
        let completed = execute_runtime_status_pill_refresh_plan_async(timed_out).await;
        assert_eq!(completed.outcome, RuntimeStatusPillRefreshOutcome::Failed);
    }
}
