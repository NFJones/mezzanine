//! Runtime support for command-backed window status pills.
//!
//! This module owns the configuration model, active-template detection, bounded
//! command execution, and cache state for `#{pill.<name>}` window status fields.
//! Rendering receives only cached text and schedules generation-stamped work so
//! terminal frame rendering stays pure. A supervised worker executes commands
//! only for pills referenced by the active `frames.window.right_status`
//! template, and the actor applies typed completions to this cache.

use super::{BTreeMap, Duration, MezError, Result, Value, current_unix_millis};
use crate::host::terminal::{
    PaneStatusCondition, PaneStatusProviderDefinition, PaneStatusProviderEmptyBehavior,
    PaneStatusProviderErrorBehavior,
};
use crate::runtime::processes::{
    NativeBubblewrapActivityLease, NativeSandboxCapabilityProbe, NativeShellContext,
};
use mez_agent::ShellChildLaunch;
use std::collections::BTreeSet;
use std::process::Stdio;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Default timeout for one status pill command execution.
pub(super) const DEFAULT_STATUS_PILL_TIMEOUT_MS: u64 = 750;
/// Default maximum number of Unicode scalar values retained from command output.
pub(super) const DEFAULT_STATUS_PILL_MAX_OUTPUT_CHARS: usize = 32;
/// Maximum bytes retained from one status pill stdout stream.
pub(crate) const STATUS_PILL_OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;
/// Maximum pending pane-provider plans retained by the serialized runtime.
pub(super) const MAX_PENDING_PANE_STATUS_PROVIDER_REFRESHES: usize = 128;
/// Maximum exact pane/provider contexts retained for same-context restoration.
const MAX_CACHED_PANE_STATUS_PROVIDER_STATES: usize = 256;
/// Maximum pane-provider processes admitted concurrently by the shared worker.
pub(crate) const MAX_CONCURRENT_PANE_STATUS_PROVIDERS: usize = 4;
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

/// Fixed status surface carried in pane-provider cache identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RuntimeStatusPillSurface {
    /// Pane title/status frame.
    Pane,
}

/// Exact cache identity for one pane-local provider context.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RuntimePaneStatusProviderKey {
    /// Status surface that owns the provider.
    pub(crate) surface: RuntimeStatusPillSurface,
    /// Stable pane owner.
    pub(crate) pane_id: String,
    /// Configured pill name.
    pub(crate) name: String,
    /// Canonical pane working directory used for execution.
    pub(crate) cwd: String,
    /// Session configuration generation used during admission.
    pub(crate) config_generation: u64,
    /// Stable pane process/environment context generation.
    pub(crate) context_generation: u64,
}

/// Shared cancellation fence polled by the spawned-process executor.
#[derive(Debug, Clone)]
pub(crate) struct RuntimePaneStatusProviderCancellation {
    flag: Arc<AtomicBool>,
}

impl RuntimePaneStatusProviderCancellation {
    /// Creates one uncancelled provider fence.
    fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Requests prompt process-group termination.
    pub(crate) fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Reports whether cancellation was requested before worker dispatch.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Returns the exact shared flag consumed by the process executor.
    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }
}

impl PartialEq for RuntimePaneStatusProviderCancellation {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.flag, &other.flag)
    }
}

impl Eq for RuntimePaneStatusProviderCancellation {}

/// Immutable sandboxed execution template produced by actor-side admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimePaneStatusProviderLaunch {
    /// Pane-derived native process context; the child sandbox still receives a minimal environment.
    pub(crate) context: NativeShellContext,
    /// Optional exact backend capability probe that must pass before execution.
    pub(crate) capability_probe: Option<NativeSandboxCapabilityProbe>,
    /// Backend whose out-of-band lifecycle evidence must be validated.
    pub(crate) sandbox_backend: crate::runtime::SandboxBackend,
    /// Compiled typed sandbox process launch.
    pub(crate) child_launch: ShellChildLaunch,
    /// Bubblewrap managed-home exclusion retained through execution.
    pub(crate) bubblewrap_activity_lease: Option<NativeBubblewrapActivityLease>,
    /// Seatbelt private workload files retained through execution.
    pub(crate) seatbelt_workload_lease: Option<crate::security::sandbox::SeatbeltWorkloadLease>,
}

/// One lightweight provider candidate reconciled during render preparation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimePaneStatusProviderRequest {
    /// Exact pane/config/context cache key.
    pub(crate) key: RuntimePaneStatusProviderKey,
    /// Validated provider display and refresh policy.
    pub(crate) definition: PaneStatusProviderDefinition,
}

/// One due provider context claimed before heavyweight admission begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimePaneStatusProviderPreparation {
    /// Exact pane/config/context cache key.
    pub(crate) key: RuntimePaneStatusProviderKey,
    /// Monotonic cache generation.
    pub(crate) generation: u64,
    /// Definition snapshot fenced against current configuration.
    pub(crate) definition: PaneStatusProviderDefinition,
    /// Cancellation fence shared with lifecycle and worker ownership.
    pub(crate) cancellation: RuntimePaneStatusProviderCancellation,
}

/// Immutable pane-provider work transferred to the bounded worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaneStatusProviderRefreshPlan {
    /// Exact pane/config/context cache key.
    pub(crate) key: RuntimePaneStatusProviderKey,
    /// Monotonic cache generation.
    pub(crate) generation: u64,
    /// Definition snapshot used to reject stale completions.
    pub(crate) definition: PaneStatusProviderDefinition,
    /// Actor-compiled sandbox launch.
    pub(crate) launch: RuntimePaneStatusProviderLaunch,
    /// Cancellation fence shared with actor-owned lifecycle state.
    pub(crate) cancellation: RuntimePaneStatusProviderCancellation,
}

#[cfg(test)]
impl RuntimePaneStatusProviderRefreshPlan {
    /// Builds one directly executable provider plan for worker boundary tests.
    pub(crate) fn for_tests(
        name: &str,
        command: &str,
        working_directory: &std::path::Path,
        timeout_ms: u64,
    ) -> Self {
        let cwd = working_directory.to_string_lossy().into_owned();
        Self {
            key: RuntimePaneStatusProviderKey {
                surface: RuntimeStatusPillSurface::Pane,
                pane_id: "%1".to_string(),
                name: name.to_string(),
                cwd,
                config_generation: 1,
                context_generation: 1,
            },
            generation: 1,
            definition: PaneStatusProviderDefinition {
                command: command.to_string(),
                origin: None,
                interval_ms: 1_000,
                initial: None,
                timeout_ms,
                empty_behavior: PaneStatusProviderEmptyBehavior::Hide,
                error_behavior: PaneStatusProviderErrorBehavior::Hide,
                max_output_chars: DEFAULT_STATUS_PILL_MAX_OUTPUT_CHARS,
            },
            launch: RuntimePaneStatusProviderLaunch {
                context: NativeShellContext::for_test(
                    std::path::PathBuf::from("/bin/sh"),
                    Vec::new(),
                    working_directory.to_path_buf(),
                ),
                capability_probe: None,
                sandbox_backend: crate::runtime::SandboxBackend::Bubblewrap,
                child_launch: ShellChildLaunch::new(
                    "/bin/sh",
                    vec![
                        mez_agent::ShellChildArgument::Literal("-e".to_string()),
                        mez_agent::ShellChildArgument::MaterializedCommandFile,
                    ],
                )
                .expect("test child launch should be valid"),
                bubblewrap_activity_lease: None,
                seatbelt_workload_lease: None,
            },
            cancellation: RuntimePaneStatusProviderCancellation::new(),
        }
    }
}

/// Bounded pane-provider process result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimePaneStatusProviderOutcome {
    /// Successful normalized first-line output.
    Succeeded(String),
    /// Work was cancelled because its pane, context, config, or runtime ended.
    Cancelled,
    /// Spawn, sandbox, timeout, encoding, or exit failure.
    Failed,
}

/// Typed completion returned to serialized runtime ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaneStatusProviderEvent {
    /// Exact pane/config/context key from dispatch.
    pub(crate) key: RuntimePaneStatusProviderKey,
    /// Cache generation from dispatch.
    pub(crate) generation: u64,
    /// Definition snapshot from dispatch.
    pub(crate) definition: PaneStatusProviderDefinition,
    /// Bounded worker outcome.
    pub(crate) outcome: RuntimePaneStatusProviderOutcome,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RuntimePaneStatusProviderState {
    display: Option<String>,
    definition: Option<PaneStatusProviderDefinition>,
    next_refresh_at_ms: u64,
    pending_generation: Option<u64>,
    cancellation: Option<RuntimePaneStatusProviderCancellation>,
    blocked_reason: Option<String>,
    active: bool,
    last_used_sequence: u64,
}

/// Secret-safe diagnostic for one currently blocked pane provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimePaneStatusProviderBlockedState {
    /// Configured provider name, which is already constrained by the pane-pill schema.
    pub(crate) name: String,
    /// Finite product-owned reason code; internal admission text is never retained here.
    pub(crate) reason: &'static str,
}

/// Maps internal admission failures onto a finite, secret-safe operator vocabulary.
fn pane_status_provider_blocked_reason(reason: &str) -> &'static str {
    if reason.contains("requires explicit approval") {
        "approval-required"
    } else if reason.contains("denied by permission policy") {
        "permission-denied"
    } else if reason.contains("source provenance") || reason.contains("source layer is not trusted")
    {
        "untrusted-source"
    } else if reason.contains("configuration changed") || reason.contains("definition changed") {
        "configuration-changed"
    } else if reason.contains("pane owner is unavailable") {
        "owner-unavailable"
    } else if reason.contains("pane context")
        || reason.contains("working directory")
        || reason.contains("path authority")
    {
        "context-unavailable"
    } else if reason.contains("sandbox admission failed") {
        "sandbox-unavailable"
    } else {
        "unavailable"
    }
}

/// Actor-owned pane-provider cache and fair bounded scheduler.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RuntimePaneStatusProviderCache {
    states: BTreeMap<RuntimePaneStatusProviderKey, RuntimePaneStatusProviderState>,
    next_generation: u64,
    next_use_sequence: u64,
    last_scheduled: Option<RuntimePaneStatusProviderKey>,
}

impl RuntimePaneStatusProviderCache {
    /// Reconciles exact visible provider contexts without policy or filesystem work.
    pub(super) fn reconcile(&mut self, mut requests: Vec<RuntimePaneStatusProviderRequest>) {
        requests.sort_by(|left, right| left.key.cmp(&right.key));
        if let Some(last) = self.last_scheduled.as_ref()
            && let Some(split) = requests.iter().position(|request| request.key > *last)
        {
            requests.rotate_left(split);
        }
        let active = requests
            .iter()
            .map(|request| request.key.clone())
            .collect::<BTreeSet<_>>();
        for (key, state) in &mut self.states {
            state.active = active.contains(key);
            if !state.active {
                if let Some(cancellation) = state.cancellation.take() {
                    cancellation.cancel();
                }
                state.pending_generation = None;
            }
        }

        for request in requests {
            if !self.states.contains_key(&request.key)
                && self.states.len() >= MAX_CACHED_PANE_STATUS_PROVIDER_STATES
            {
                let eviction = self
                    .states
                    .iter()
                    .filter(|(_, state)| !state.active && state.pending_generation.is_none())
                    .min_by_key(|(_, state)| state.last_used_sequence)
                    .map(|(key, _)| key.clone());
                if let Some(eviction) = eviction {
                    self.states.remove(&eviction);
                } else {
                    continue;
                }
            }
            let state = self.states.entry(request.key.clone()).or_default();
            if state.definition.as_ref() != Some(&request.definition) {
                if let Some(cancellation) = state.cancellation.take() {
                    cancellation.cancel();
                }
                *state = RuntimePaneStatusProviderState::default();
            }
            self.next_use_sequence = self.next_use_sequence.wrapping_add(1).max(1);
            state.active = true;
            state.last_used_sequence = self.next_use_sequence;
            state.definition = Some(request.definition.clone());
            if state.display.is_none() {
                state.display = request
                    .definition
                    .initial
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned);
            }
        }
    }

    /// Claims due contexts only after enforcing queue and continuous in-flight bounds.
    pub(super) fn claim_due_preparations(
        &mut self,
        limit: usize,
    ) -> Vec<RuntimePaneStatusProviderPreparation> {
        let pending = self
            .states
            .values()
            .filter(|state| state.pending_generation.is_some())
            .count();
        let available = MAX_PENDING_PANE_STATUS_PROVIDER_REFRESHES
            .saturating_sub(pending)
            .min(limit);
        if available == 0 {
            return Vec::new();
        }
        let now_ms = current_unix_millis();
        let mut keys = self
            .states
            .iter()
            .filter(|(_, state)| {
                state.active
                    && state.pending_generation.is_none()
                    && state.blocked_reason.is_none()
                    && state.next_refresh_at_ms <= now_ms
                    && state.definition.is_some()
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        if let Some(last) = self.last_scheduled.as_ref()
            && let Some(split) = keys.iter().position(|key| key > last)
        {
            keys.rotate_left(split);
        }
        let mut preparations = Vec::with_capacity(available);
        for key in keys.into_iter().take(available) {
            let Some(state) = self.states.get_mut(&key) else {
                continue;
            };
            let Some(definition) = state.definition.clone() else {
                continue;
            };
            self.next_generation = self.next_generation.wrapping_add(1).max(1);
            let generation = self.next_generation;
            let cancellation = RuntimePaneStatusProviderCancellation::new();
            state.pending_generation = Some(generation);
            state.cancellation = Some(cancellation.clone());
            state.next_refresh_at_ms = now_ms.saturating_add(definition.interval_ms.max(1_000));
            self.last_scheduled = Some(key.clone());
            preparations.push(RuntimePaneStatusProviderPreparation {
                key,
                generation,
                definition,
                cancellation,
            });
        }
        preparations
    }

    /// Reports whether active due work needs a bounded actor-side preparation pass.
    pub(super) fn preparation_needed(&self) -> bool {
        if self
            .states
            .values()
            .filter(|state| state.pending_generation.is_some())
            .count()
            >= MAX_PENDING_PANE_STATUS_PROVIDER_REFRESHES
        {
            return false;
        }
        let now_ms = current_unix_millis();
        self.states.values().any(|state| {
            state.active
                && state.pending_generation.is_none()
                && state.blocked_reason.is_none()
                && state.next_refresh_at_ms <= now_ms
                && state.definition.is_some()
        })
    }

    /// Settles one bounded preparation and returns worker work only when still current.
    pub(super) fn complete_preparation(
        &mut self,
        preparation: RuntimePaneStatusProviderPreparation,
        result: std::result::Result<RuntimePaneStatusProviderLaunch, String>,
    ) -> Option<RuntimePaneStatusProviderRefreshPlan> {
        let state = self.states.get_mut(&preparation.key)?;
        if state.pending_generation != Some(preparation.generation)
            || state.definition.as_ref() != Some(&preparation.definition)
            || !state.active
        {
            preparation.cancellation.cancel();
            return None;
        }
        match result {
            Ok(launch) => {
                state.blocked_reason = None;
                Some(RuntimePaneStatusProviderRefreshPlan {
                    key: preparation.key,
                    generation: preparation.generation,
                    definition: preparation.definition,
                    launch,
                    cancellation: preparation.cancellation,
                })
            }
            Err(reason) => {
                state.pending_generation = None;
                state.cancellation = None;
                state.blocked_reason = Some(reason);
                None
            }
        }
    }

    /// Returns cached raw values for one stable pane owner without scheduling work.
    pub(super) fn values_for_pane(&self, pane_id: &str) -> BTreeMap<String, String> {
        self.states
            .iter()
            .filter(|(key, state)| key.pane_id == pane_id && state.active)
            .filter_map(|(key, state)| {
                state
                    .display
                    .as_ref()
                    .map(|display| (key.name.clone(), display.clone()))
            })
            .collect()
    }

    /// Returns one sanitized block only when the supplied exact context is current in the cache.
    fn blocked_provider_state(
        &self,
        key: &RuntimePaneStatusProviderKey,
    ) -> Option<RuntimePaneStatusProviderBlockedState> {
        let state = self.states.get(key)?;
        let reason = state.blocked_reason.as_deref()?;
        Some(RuntimePaneStatusProviderBlockedState {
            name: key.name.clone(),
            reason: pane_status_provider_blocked_reason(reason),
        })
    }

    /// Returns one sanitized block matching the current provider and pane context.
    ///
    /// Inspection deliberately ignores only the cache key's session-wide configuration
    /// generation so an unrelated zen-mode override cannot hide a retained block.
    fn blocked_provider_state_for_context(
        &self,
        key: &RuntimePaneStatusProviderKey,
        definition: &PaneStatusProviderDefinition,
    ) -> Option<RuntimePaneStatusProviderBlockedState> {
        self.states
            .iter()
            .rev()
            .find(|(candidate, state)| {
                candidate.surface == key.surface
                    && candidate.pane_id == key.pane_id
                    && candidate.name == key.name
                    && candidate.cwd == key.cwd
                    && candidate.context_generation == key.context_generation
                    && state.definition.as_ref() == Some(definition)
                    && state.blocked_reason.is_some()
            })
            .and_then(|(candidate, _)| self.blocked_provider_state(candidate))
    }

    /// Clears only an exact current block and marks that context due for normal admission.
    fn retry_blocked_provider(&mut self, key: &RuntimePaneStatusProviderKey) -> bool {
        let Some(state) = self.states.get_mut(key) else {
            return false;
        };
        if state.blocked_reason.take().is_none() {
            return false;
        }
        state.next_refresh_at_ms = 0;
        true
    }

    /// Applies only the exact current pane/config/context completion.
    pub(super) fn apply_event(&mut self, event: RuntimePaneStatusProviderEvent) -> Option<bool> {
        let state = self.states.get_mut(&event.key)?;
        if state.pending_generation != Some(event.generation)
            || !state.active
            || state.definition.as_ref() != Some(&event.definition)
        {
            return None;
        }
        state.pending_generation = None;
        state.cancellation = None;
        let previous = state.display.clone();
        state.display = match event.outcome {
            RuntimePaneStatusProviderOutcome::Succeeded(output) if output.is_empty() => {
                match event.definition.empty_behavior {
                    PaneStatusProviderEmptyBehavior::Hide => None,
                    PaneStatusProviderEmptyBehavior::ShowEmpty => Some(String::new()),
                    PaneStatusProviderEmptyBehavior::KeepPrevious => previous.clone(),
                }
            }
            RuntimePaneStatusProviderOutcome::Succeeded(output) => Some(output),
            RuntimePaneStatusProviderOutcome::Cancelled => previous.clone(),
            RuntimePaneStatusProviderOutcome::Failed => match event.definition.error_behavior {
                PaneStatusProviderErrorBehavior::Hide => None,
                PaneStatusProviderErrorBehavior::ShowError => {
                    Some(STATUS_PILL_ERROR_TEXT.to_string())
                }
                PaneStatusProviderErrorBehavior::KeepPrevious => previous.clone(),
            },
        };
        Some(state.display != previous)
    }

    /// Cancels and removes all work after config or visibility invalidation.
    pub(super) fn invalidate_all(&mut self) {
        for state in self.states.values() {
            if let Some(cancellation) = state.cancellation.as_ref() {
                cancellation.cancel();
            }
        }
        self.states.clear();
    }

    /// Cancels active work while retaining bounded same-context display values.
    pub(super) fn suspend_all(&mut self) {
        for state in self.states.values_mut() {
            if let Some(cancellation) = state.cancellation.take() {
                cancellation.cancel();
            }
            state.pending_generation = None;
            state.active = false;
        }
    }

    /// Cancels and removes every context owned by one closed or changed pane.
    pub(super) fn remove_pane(&mut self, pane_id: &str) {
        let retained = self
            .states
            .keys()
            .filter(|key| key.pane_id != pane_id)
            .cloned()
            .collect::<BTreeSet<_>>();
        self.states.retain(|key, state| {
            let keep = retained.contains(key);
            if !keep && let Some(cancellation) = state.cancellation.as_ref() {
                cancellation.cancel();
            }
            keep
        });
    }
}

impl crate::runtime::RuntimeSessionService {
    /// Cancels every pending or dispatched pane-status provider for lifecycle teardown.
    pub(crate) fn cancel_pane_status_provider_work(&self) {
        self.presentation
            .pane_status_provider_cache
            .borrow_mut()
            .suspend_all();
    }

    /// Derives the exact current cache identity for one configured pane provider.
    ///
    /// This lookup reads existing runtime and configuration state only. It does not
    /// reconcile providers, perform admission, or enqueue external work.
    fn current_pane_status_provider_key(
        &self,
        pane_id: &str,
        name: &str,
    ) -> Option<RuntimePaneStatusProviderKey> {
        self.find_pane_descriptor(pane_id)?;
        self.presentation
            .settings
            .pane_status
            .pills
            .get(name)?
            .provider
            .as_ref()?;
        let cwd = self
            .pane_current_working_directory(pane_id)
            .map(|cwd| cwd.to_string_lossy().into_owned())
            .unwrap_or_else(|| "<unavailable>".to_string());
        let context_generation = pane_provider_context_generation(
            pane_id,
            &cwd,
            self.primary_pid_for_live_pane_process(pane_id),
            self.pane_environment_signature(pane_id)
                .map(mez_agent::EnvironmentSignature::stable_hash)
                .as_deref(),
        );
        Some(RuntimePaneStatusProviderKey {
            surface: RuntimeStatusPillSurface::Pane,
            pane_id: pane_id.to_string(),
            name: name.to_string(),
            cwd,
            config_generation: self.session.config_generation,
            context_generation,
        })
    }

    /// Returns current secret-safe blocks for one pane without scheduling providers.
    pub(crate) fn pane_status_provider_blocked_states(
        &self,
        pane_id: &str,
    ) -> Vec<RuntimePaneStatusProviderBlockedState> {
        let providers = self
            .presentation
            .settings
            .pane_status
            .pills
            .iter()
            .filter_map(|(name, definition)| {
                definition
                    .provider
                    .as_ref()
                    .map(|provider| (name.clone(), provider.clone()))
            })
            .collect::<Vec<_>>();
        let cache = self.presentation.pane_status_provider_cache.borrow();
        providers
            .iter()
            .filter_map(|(name, provider)| {
                self.current_pane_status_provider_key(pane_id, name)
                    .and_then(|key| cache.blocked_provider_state_for_context(&key, provider))
            })
            .collect()
    }

    /// Clears one exact current block and marks it due for ordinary admission.
    ///
    /// Retry changes no trust, approval, permission, or sandbox state.
    pub(crate) fn retry_pane_status_provider(&self, pane_id: &str, name: &str) -> bool {
        let Some(key) = self.current_pane_status_provider_key(pane_id, name) else {
            return false;
        };
        self.presentation
            .pane_status_provider_cache
            .borrow_mut()
            .retry_blocked_provider(&key)
    }

    /// Reconciles pane-provider work for every window currently presented by an attached client.
    ///
    /// Configuration parsing retains exact source-layer provenance. This boundary additionally
    /// requires an attached presentation, a concrete pane context, a structured `Allow`
    /// permission decision, and a successfully compiled OS-sandbox launch. Prompt and deny
    /// decisions remain inert and never enqueue approval requests from the refresh timer.
    pub(crate) fn reconcile_pane_status_providers(&mut self) {
        if !self.effective_pane_frames_enabled() {
            self.presentation
                .pane_status_provider_cache
                .borrow_mut()
                .suspend_all();
            return;
        }

        let pane_status = self.presentation.settings.pane_status.clone();
        let mut referenced = runtime_status_pill_names_from_template(&pane_status.left_status);
        referenced.extend(runtime_status_pill_names_from_template(
            &pane_status.right_status,
        ));
        if referenced.is_empty() {
            self.presentation
                .pane_status_provider_cache
                .borrow_mut()
                .invalidate_all();
            return;
        }

        let panes = presented_pane_focus_states(&self.session);
        let frame_context = self.terminal_frame_context();
        let mut requests = Vec::new();

        for (pane_id, focus_states) in panes {
            for name in referenced.keys() {
                let Some(definition) = pane_status.pills.get(name).cloned() else {
                    continue;
                };
                let Some(provider) = definition.provider.clone() else {
                    continue;
                };
                if !pane_provider_is_eligible_in_any_view(
                    &definition.when,
                    &focus_states,
                    frame_context.panes.get(&pane_id),
                ) {
                    continue;
                }

                let cwd = self
                    .pane_current_working_directory(&pane_id)
                    .map(|cwd| cwd.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "<unavailable>".to_string());
                let context_generation = pane_provider_context_generation(
                    &pane_id,
                    &cwd,
                    self.primary_pid_for_live_pane_process(&pane_id),
                    self.pane_environment_signature(&pane_id)
                        .map(mez_agent::EnvironmentSignature::stable_hash)
                        .as_deref(),
                );
                let key = RuntimePaneStatusProviderKey {
                    surface: RuntimeStatusPillSurface::Pane,
                    pane_id: pane_id.clone(),
                    name: name.clone(),
                    cwd,
                    config_generation: self.session.config_generation,
                    context_generation,
                };
                requests.push(RuntimePaneStatusProviderRequest {
                    key,
                    definition: provider,
                });
            }
        }

        self.presentation
            .pane_status_provider_cache
            .borrow_mut()
            .reconcile(requests);
    }

    /// Claims due contexts, performs bounded actor-side admission, and returns worker plans.
    pub(crate) fn prepare_pane_status_provider_refreshes(
        &mut self,
        limit: usize,
    ) -> Vec<RuntimePaneStatusProviderRefreshPlan> {
        if limit == 0 {
            return Vec::new();
        }
        let preparations = self
            .presentation
            .pane_status_provider_cache
            .borrow_mut()
            .claim_due_preparations(limit);
        let mut plans = Vec::with_capacity(preparations.len());
        for preparation in preparations {
            let result = self.pane_status_provider_admission(&preparation);
            if let Some(plan) = self
                .presentation
                .pane_status_provider_cache
                .borrow_mut()
                .complete_preparation(preparation, result)
            {
                plans.push(plan);
            }
        }
        plans
    }

    /// Returns a fail-closed launch after revalidating the exact claimed context.
    fn pane_status_provider_admission(
        &mut self,
        preparation: &RuntimePaneStatusProviderPreparation,
    ) -> std::result::Result<RuntimePaneStatusProviderLaunch, String> {
        let pane_id = preparation.key.pane_id.as_str();
        let provider = &preparation.definition;
        if self.session.config_generation != preparation.key.config_generation {
            return Err("provider configuration changed before admission".to_string());
        }
        let configured = self
            .presentation
            .settings
            .pane_status
            .pills
            .get(&preparation.key.name)
            .and_then(|definition| definition.provider.as_ref());
        if configured != Some(provider) {
            return Err("provider definition changed before admission".to_string());
        }
        if !self
            .session
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .any(|pane| pane.id.as_str() == pane_id)
        {
            return Err("provider pane owner is unavailable".to_string());
        }
        let cwd = self
            .pane_current_working_directory(pane_id)
            .map(|cwd| cwd.to_string_lossy().into_owned())
            .unwrap_or_else(|| "<unavailable>".to_string());
        let context_generation = pane_provider_context_generation(
            pane_id,
            &cwd,
            self.primary_pid_for_live_pane_process(pane_id),
            self.pane_environment_signature(pane_id)
                .map(mez_agent::EnvironmentSignature::stable_hash)
                .as_deref(),
        );
        if cwd != preparation.key.cwd || context_generation != preparation.key.context_generation {
            return Err("provider pane context changed before admission".to_string());
        }
        let Some(origin) = provider.origin.as_ref() else {
            return Err("provider source provenance is unavailable".to_string());
        };
        if !origin.trusted {
            return Err("provider source layer is not trusted".to_string());
        }
        let context = match self.native_shell_context_for_pane(pane_id) {
            Ok(context) => context,
            Err(error) => {
                return Err(format!("pane context is unavailable: {}", error.message()));
            }
        };
        if std::fs::canonicalize(context.working_directory()).is_err() {
            return Err("pane working directory is unavailable".to_string());
        }
        let policy = self.permission_policy_for_pane(pane_id);
        let path_scopes = match self.native_path_scopes_for_pane_status_provider(pane_id, &context)
        {
            Ok(Some(path_scopes)) => path_scopes,
            Ok(None) => {
                return Err("provider path authority is unavailable".to_string());
            }
            Err(error) => {
                return Err(format!(
                    "provider path authority is unavailable: {}",
                    error.message()
                ));
            }
        };
        let evaluation = {
            let planning = crate::security::permissions::ProductPermissionPlanning::new(
                &policy,
                self.session_approvals(),
                Some(&path_scopes),
            )
            .with_shell_classification(context.classification().as_str());
            mez_agent::permissions::PermissionPlanning::evaluate_command_structured(
                &planning,
                &provider.command,
            )
        };
        match evaluation.decision {
            mez_agent::permissions::RuleDecision::Forbid => {
                return Err("provider command is denied by permission policy".to_string());
            }
            mez_agent::permissions::RuleDecision::Prompt => {
                return Err("provider command requires explicit approval".to_string());
            }
            mez_agent::permissions::RuleDecision::Allow => {}
        }
        self.compile_pane_status_provider_launch(
            pane_id,
            &provider.command,
            &context,
            &path_scopes,
            &evaluation,
        )
        .map_err(|error| format!("sandbox admission failed: {}", error.message()))
    }
}

/// Derives pane presentation and focus from each attached client's exact source-primary view.
fn presented_pane_focus_states(
    session: &mez_mux::session::Session,
) -> BTreeMap<String, BTreeSet<bool>> {
    let mut presented = BTreeMap::<String, BTreeSet<bool>>::new();
    for client in session
        .clients()
        .iter()
        .filter(|client| client.state == mez_mux::session::ClientState::Attached)
    {
        let source = match client.role {
            mez_mux::session::ClientRole::Primary => Some(&client.id),
            mez_mux::session::ClientRole::Observer => session
                .observer_attachments()
                .iter()
                .find(|observer| observer.client_id == client.id)
                .map(|observer| &observer.view_source_client_id),
            mez_mux::session::ClientRole::Agent | mez_mux::session::ClientRole::Automation => None,
        };
        let Some(source) = source else {
            continue;
        };
        let Ok(window) = session.active_window_for(source) else {
            continue;
        };
        let Ok(navigation) = session.navigation(source) else {
            continue;
        };
        let focused_pane = navigation
            .panes_by_window
            .get(&window.id)
            .and_then(|cursor| cursor.active.as_ref());
        let zoomed_pane = navigation.zoomed_panes_by_window.get(&window.id);
        for pane in window
            .panes()
            .iter()
            .filter(|pane| zoomed_pane.is_none_or(|zoomed| pane.id == *zoomed))
        {
            presented
                .entry(pane.id.to_string())
                .or_default()
                .insert(focused_pane == Some(&pane.id));
        }
    }
    presented
}

/// Reports whether at least one presented view of a pane satisfies its provider conditions.
fn pane_provider_is_eligible_in_any_view(
    conditions: &[PaneStatusCondition],
    focus_states: &BTreeSet<bool>,
    context: Option<&crate::host::terminal::TerminalPaneFrameContext>,
) -> bool {
    focus_states
        .iter()
        .any(|focused| pane_provider_conditions_match(conditions, *focused, context))
}

fn pane_provider_conditions_match(
    conditions: &[PaneStatusCondition],
    focused: bool,
    context: Option<&crate::host::terminal::TerminalPaneFrameContext>,
) -> bool {
    let agent_view = context.and_then(|context| context.mode.as_deref()) == Some("agent");
    let busy = context
        .and_then(|context| context.agent_status.as_deref())
        .is_some_and(|status| {
            matches!(
                status,
                "queued"
                    | "running"
                    | "thinking"
                    | "executing"
                    | "waiting"
                    | "bootstrapping"
                    | "certifying_sandbox"
                    | "compacting"
                    | "memorizing"
            )
        });
    let scrollback = context
        .and_then(|context| context.history_position.as_deref())
        .is_some_and(|value| !value.trim().is_empty());
    conditions.iter().all(|condition| match condition {
        PaneStatusCondition::AgentView => agent_view,
        PaneStatusCondition::ShellView => !agent_view,
        PaneStatusCondition::Focused => focused,
        PaneStatusCondition::Unfocused => !focused,
        PaneStatusCondition::Busy => busy,
        PaneStatusCondition::Idle => !busy,
        PaneStatusCondition::Supported | PaneStatusCondition::Nonempty => true,
        PaneStatusCondition::Scrollback => scrollback,
    })
}

fn pane_provider_context_generation(
    pane_id: &str,
    cwd: &str,
    primary_pid: Option<u32>,
    environment_signature: Option<&str>,
) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    pane_id.hash(&mut hasher);
    cwd.hash(&mut hasher);
    primary_pid.hash(&mut hasher);
    environment_signature.hash(&mut hasher);
    hasher.finish()
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
pub(crate) fn runtime_status_pill_normalize_output(output: &str, max_chars: usize) -> String {
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
        MAX_CACHED_PANE_STATUS_PROVIDER_STATES, MAX_PENDING_PANE_STATUS_PROVIDER_REFRESHES,
        RuntimePaneStatusProviderCache, RuntimePaneStatusProviderEvent,
        RuntimePaneStatusProviderKey, RuntimePaneStatusProviderLaunch,
        RuntimePaneStatusProviderOutcome, RuntimePaneStatusProviderRequest, RuntimeStatusPillCache,
        RuntimeStatusPillDefinition, RuntimeStatusPillEmptyBehavior,
        RuntimeStatusPillErrorBehavior, RuntimeStatusPillEvent, RuntimeStatusPillRefreshOutcome,
        RuntimeStatusPillSurface, execute_runtime_status_pill_refresh_plan_async,
        pane_provider_is_eligible_in_any_view, presented_pane_focus_states,
        runtime_status_pill_names_from_template,
    };
    use crate::host::terminal::{
        PaneStatusCondition, PaneStatusProviderDefinition, PaneStatusProviderEmptyBehavior,
        PaneStatusProviderErrorBehavior,
    };
    use crate::runtime::processes::NativeShellContext;
    use mez_agent::ShellChildLaunch;
    use mez_mux::layout::{Size, SplitDirection};
    use mez_mux::session::{ClientRole, Session, SessionShell};
    use std::path::PathBuf;

    fn pane_provider_definition(initial: Option<&str>) -> PaneStatusProviderDefinition {
        PaneStatusProviderDefinition {
            command: "printf ready".to_string(),
            origin: None,
            interval_ms: 1_000,
            initial: initial.map(ToOwned::to_owned),
            timeout_ms: DEFAULT_STATUS_PILL_TIMEOUT_MS,
            empty_behavior: PaneStatusProviderEmptyBehavior::Hide,
            error_behavior: PaneStatusProviderErrorBehavior::Hide,
            max_output_chars: DEFAULT_STATUS_PILL_MAX_OUTPUT_CHARS,
        }
    }

    fn pane_provider_key(pane_id: &str, name: &str, cwd: &str) -> RuntimePaneStatusProviderKey {
        RuntimePaneStatusProviderKey {
            surface: RuntimeStatusPillSurface::Pane,
            pane_id: pane_id.to_string(),
            name: name.to_string(),
            cwd: cwd.to_string(),
            config_generation: 1,
            context_generation: 1,
        }
    }

    fn pane_provider_launch(cwd: &str) -> RuntimePaneStatusProviderLaunch {
        RuntimePaneStatusProviderLaunch {
            context: NativeShellContext::for_test(
                PathBuf::from("/bin/sh"),
                Vec::new(),
                PathBuf::from(cwd),
            ),
            capability_probe: None,
            sandbox_backend: crate::runtime::SandboxBackend::Bubblewrap,
            child_launch: ShellChildLaunch::new("/bin/sh", Vec::new()).unwrap(),
            bubblewrap_activity_lease: None,
            seatbelt_workload_lease: None,
        }
    }

    fn pane_provider_request(
        key: RuntimePaneStatusProviderKey,
        definition: PaneStatusProviderDefinition,
    ) -> RuntimePaneStatusProviderRequest {
        RuntimePaneStatusProviderRequest { key, definition }
    }

    fn claim_and_admit(
        cache: &mut RuntimePaneStatusProviderCache,
        limit: usize,
    ) -> Vec<super::RuntimePaneStatusProviderRefreshPlan> {
        cache
            .claim_due_preparations(limit)
            .into_iter()
            .filter_map(|preparation| {
                let launch = pane_provider_launch(&preparation.key.cwd);
                cache.complete_preparation(preparation, Ok(launch))
            })
            .collect()
    }

    fn pane_provider_test_session() -> Session {
        Session::new_default(
            SessionShell::new(PathBuf::from("/bin/sh"), "fallback-bin-sh", true),
            Size::new(80, 24).unwrap(),
        )
    }

    /// Verifies provider presentation preserves each primary's exact pane
    /// focus instead of reading whichever focus was last projected into the window.
    #[test]
    fn pane_provider_presentation_unions_focus_across_primary_views() {
        let mut session = pane_provider_test_session();
        let first = session.attach_primary("first", true).unwrap();
        let first_pane = session.active_pane_for(&first).unwrap().id.clone();
        let second_pane = session
            .split_active_pane(&first, SplitDirection::Vertical)
            .unwrap();
        let second = session.attach_primary("second", true).unwrap();
        session.select_pane(&first, first_pane.as_str()).unwrap();

        let presented = presented_pane_focus_states(&session);

        assert_eq!(presented[&first_pane.to_string()], [false, true].into());
        assert_eq!(presented[&second_pane.to_string()], [false, true].into());
        assert!(pane_provider_is_eligible_in_any_view(
            &[PaneStatusCondition::Focused],
            &presented[&first_pane.to_string()],
            None,
        ));
        assert!(pane_provider_is_eligible_in_any_view(
            &[PaneStatusCondition::Unfocused],
            &presented[&first_pane.to_string()],
            None,
        ));
        assert_eq!(session.active_pane_for(&second).unwrap().id, second_pane);
    }

    /// Verifies an observer contributes only its source primary's zoomed
    /// presentation, so a sibling hidden by that exact view is never scheduled.
    #[test]
    fn pane_provider_presentation_uses_observer_source_zoom() {
        let mut session = pane_provider_test_session();
        let primary = session.attach_primary("primary", true).unwrap();
        let hidden_pane = session.active_pane_for(&primary).unwrap().id.clone();
        let zoomed_pane = session
            .split_active_pane(&primary, SplitDirection::Horizontal)
            .unwrap();
        session.toggle_active_pane_zoom(&primary).unwrap();
        let observer = session
            .attach_observer_with_terminal("observer", None, 1)
            .unwrap();

        let presented = presented_pane_focus_states(&session);

        assert_eq!(presented.len(), 1);
        assert_eq!(presented[&zoomed_pane.to_string()], [true].into());
        assert!(!presented.contains_key(hidden_pane.as_str()));
        assert_eq!(
            session
                .clients()
                .iter()
                .find(|client| client.id == observer)
                .map(|client| client.role),
            Some(ClientRole::Observer)
        );
    }

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

    /// Verifies identical configured names remain isolated by stable pane and
    /// canonical working-directory identity, including completion storage.
    #[test]
    fn pane_provider_cache_isolates_same_name_across_panes_and_cwds() {
        let definition = pane_provider_definition(Some("initial"));
        let first_key = pane_provider_key("%1", "branch", "/workspace/one");
        let second_key = pane_provider_key("%2", "branch", "/workspace/two");
        let mut cache = RuntimePaneStatusProviderCache::default();
        cache.reconcile(vec![
            pane_provider_request(first_key.clone(), definition.clone()),
            pane_provider_request(second_key.clone(), definition.clone()),
        ]);
        assert!(cache.preparation_needed());
        assert!(
            cache
                .states
                .values()
                .all(|state| state.pending_generation.is_none()),
            "render reconciliation must retain lightweight candidates only"
        );
        let plans = claim_and_admit(&mut cache, 8);
        assert_eq!(plans.len(), 2);

        for plan in plans {
            let output = if plan.key == first_key { "one" } else { "two" };
            assert_eq!(
                cache.apply_event(RuntimePaneStatusProviderEvent {
                    key: plan.key,
                    generation: plan.generation,
                    definition: plan.definition,
                    outcome: RuntimePaneStatusProviderOutcome::Succeeded(output.to_string()),
                }),
                Some(true)
            );
        }

        assert_eq!(
            cache
                .values_for_pane("%1")
                .get("branch")
                .map(String::as_str),
            Some("one")
        );
        assert_eq!(
            cache
                .values_for_pane("%2")
                .get("branch")
                .map(String::as_str),
            Some("two")
        );
    }

    /// Verifies suspension and pane removal cancel outstanding work and fence
    /// late completions, while same-context cached text remains restorable.
    #[test]
    fn pane_provider_cache_cancels_and_rejects_late_lifecycle_events() {
        let definition = pane_provider_definition(Some("initial"));
        let key = pane_provider_key("%1", "branch", "/workspace/one");
        let mut cache = RuntimePaneStatusProviderCache::default();
        cache.reconcile(vec![pane_provider_request(key.clone(), definition.clone())]);
        let suspended = claim_and_admit(&mut cache, 1).remove(0);

        cache.suspend_all();
        assert!(suspended.cancellation.is_cancelled());
        assert_eq!(
            cache.apply_event(RuntimePaneStatusProviderEvent {
                key: suspended.key,
                generation: suspended.generation,
                definition: suspended.definition,
                outcome: RuntimePaneStatusProviderOutcome::Succeeded("stale".to_string()),
            }),
            None
        );
        cache.states.get_mut(&key).unwrap().next_refresh_at_ms = 0;
        cache.reconcile(vec![pane_provider_request(key.clone(), definition)]);
        assert_eq!(
            cache
                .values_for_pane("%1")
                .get("branch")
                .map(String::as_str),
            Some("initial")
        );
        let removed = claim_and_admit(&mut cache, 1).remove(0);
        cache.remove_pane("%1");
        assert!(removed.cancellation.is_cancelled());
        assert_eq!(
            cache.apply_event(RuntimePaneStatusProviderEvent {
                key: removed.key,
                generation: removed.generation,
                definition: removed.definition,
                outcome: RuntimePaneStatusProviderOutcome::Succeeded("closed".to_string()),
            }),
            None
        );
        assert!(cache.values_for_pane("%1").is_empty());
    }

    /// Verifies failed admission settles the claimed generation without
    /// producing worker work or repeatedly claiming the same due context.
    #[test]
    fn pane_provider_cache_does_not_schedule_blocked_admission() {
        let key = pane_provider_key("%1", "branch", "/workspace/one");
        let request = pane_provider_request(key.clone(), pane_provider_definition(None));
        let mut cache = RuntimePaneStatusProviderCache::default();

        cache.reconcile(vec![request]);
        let preparation = cache.claim_due_preparations(1).remove(0);
        assert!(
            cache
                .complete_preparation(
                    preparation,
                    Err("provider command requires explicit approval".to_string()),
                )
                .is_none()
        );

        assert!(cache.claim_due_preparations(1).is_empty());
        assert_eq!(cache.next_generation, 1);
        cache.states.get_mut(&key).unwrap().next_refresh_at_ms = 0;
        assert!(!cache.preparation_needed());
        assert!(cache.claim_due_preparations(1).is_empty());
        assert_eq!(
            cache
                .states
                .get(&key)
                .and_then(|state| state.blocked_reason.as_deref()),
            Some("provider command requires explicit approval")
        );
    }

    /// Verifies blocked-provider inspection exposes only a finite reason code,
    /// never the internal admission diagnostic that may contain sensitive data.
    #[test]
    fn pane_provider_cache_inspection_sanitizes_blocked_reason() {
        let key = pane_provider_key("%1", "branch", "/secret/workspace");
        let request = pane_provider_request(key.clone(), pane_provider_definition(None));
        let mut cache = RuntimePaneStatusProviderCache::default();
        cache.reconcile(vec![request]);
        let preparation = cache.claim_due_preparations(1).remove(0);
        assert!(
            cache
                .complete_preparation(
                    preparation,
                    Err("sandbox admission failed: private path /secret/workspace".to_string()),
                )
                .is_none()
        );

        let blocked = cache
            .blocked_provider_state_for_context(&key, &pane_provider_definition(None))
            .expect("current blocked provider should remain inspectable");

        assert_eq!(blocked.name, "branch");
        assert_eq!(blocked.reason, "sandbox-unavailable");
        assert!(!format!("{blocked:?}").contains("/secret/workspace"));
    }

    /// Verifies retry clears only the exact current blocked provider and marks
    /// it due without changing sibling blocks or granting execution authority.
    #[test]
    fn pane_provider_cache_retry_requires_exact_blocked_key_and_marks_only_it_due() {
        let first_key = pane_provider_key("%1", "branch", "/workspace/one");
        let second_key = pane_provider_key("%1", "clock", "/workspace/one");
        let stale_key = pane_provider_key("%1", "branch", "/workspace/stale");
        let definition = pane_provider_definition(None);
        let mut cache = RuntimePaneStatusProviderCache::default();
        cache.reconcile(vec![
            pane_provider_request(first_key.clone(), definition.clone()),
            pane_provider_request(second_key.clone(), definition.clone()),
        ]);
        for preparation in cache.claim_due_preparations(2) {
            assert!(
                cache
                    .complete_preparation(
                        preparation,
                        Err("provider command requires explicit approval".to_string()),
                    )
                    .is_none()
            );
        }

        assert!(!cache.retry_blocked_provider(&stale_key));
        assert!(cache.retry_blocked_provider(&first_key));
        assert!(!cache.retry_blocked_provider(&first_key));
        let due = cache.claim_due_preparations(2);

        assert_eq!(due.len(), 1);
        assert_eq!(due[0].key, first_key);
        assert_eq!(
            cache
                .states
                .get(&second_key)
                .and_then(|state| state.blocked_reason.as_deref()),
            Some("provider command requires explicit approval")
        );
    }

    /// Ordinary visibility loss must invalidate a dispatched generation, even
    /// if its success arrives after the same context becomes visible again.
    #[test]
    fn pane_status_provider_deactivation_fences_late_success() {
        let key = pane_provider_key("%1", "branch", "/workspace/one");
        let request = pane_provider_request(key.clone(), pane_provider_definition(Some("initial")));
        let mut cache = RuntimePaneStatusProviderCache::default();
        cache.reconcile(vec![request.clone()]);
        let plan = claim_and_admit(&mut cache, 1).remove(0);
        cache.reconcile(Vec::new());
        assert!(plan.cancellation.is_cancelled());
        cache.reconcile(vec![request]);
        assert_eq!(
            cache.apply_event(RuntimePaneStatusProviderEvent {
                key,
                generation: plan.generation,
                definition: plan.definition,
                outcome: RuntimePaneStatusProviderOutcome::Succeeded("stale".to_string()),
            }),
            None
        );
    }

    /// A same-key definition replacement must cancel prior work and reset its
    /// value; a mismatched completion cannot settle the replacement generation.
    #[test]
    fn pane_status_provider_definition_changes_fence_results() {
        let key = pane_provider_key("%1", "branch", "/workspace/one");
        let mut definition = pane_provider_definition(Some("old"));
        let mut cache = RuntimePaneStatusProviderCache::default();
        cache.reconcile(vec![pane_provider_request(key.clone(), definition.clone())]);
        let old = claim_and_admit(&mut cache, 1).remove(0);
        definition.initial = Some("new".to_string());
        cache.reconcile(vec![pane_provider_request(key.clone(), definition)]);
        assert!(old.cancellation.is_cancelled());
        let current = claim_and_admit(&mut cache, 1).remove(0);
        assert_eq!(
            cache.apply_event(RuntimePaneStatusProviderEvent {
                key: key.clone(),
                generation: current.generation,
                definition: old.definition,
                outcome: RuntimePaneStatusProviderOutcome::Succeeded("stale".to_string()),
            }),
            None
        );
        assert_eq!(
            cache.states[&key].pending_generation,
            Some(current.generation)
        );
        assert_eq!(cache.values_for_pane("%1")["branch"], "new");
    }

    /// Verifies pending work is strictly bounded and the rotation cursor lets
    /// a deferred key claim the next available slot after a saturated batch.
    #[test]
    fn pane_provider_cache_bounds_pending_work_and_rotates_fairly() {
        let definition = pane_provider_definition(None);
        let request_count = MAX_PENDING_PANE_STATUS_PROVIDER_REFRESHES + 1;
        let requests = (0..request_count)
            .map(|index| {
                let key = pane_provider_key(
                    &format!("%{}", index + 1),
                    "branch",
                    &format!("/workspace/{index:03}"),
                );
                pane_provider_request(key, definition.clone())
            })
            .collect::<Vec<_>>();
        let mut cache = RuntimePaneStatusProviderCache::default();

        cache.reconcile(requests.clone());
        let mut first_batch =
            claim_and_admit(&mut cache, MAX_PENDING_PANE_STATUS_PROVIDER_REFRESHES);
        assert_eq!(
            first_batch.len(),
            MAX_PENDING_PANE_STATUS_PROVIDER_REFRESHES
        );
        let deferred = requests
            .iter()
            .map(|request| request.key.clone())
            .find(|key| !first_batch.iter().any(|plan| &plan.key == key))
            .expect("one key should be deferred by the pending bound");

        let completed = first_batch.remove(0);
        assert_eq!(
            cache.apply_event(RuntimePaneStatusProviderEvent {
                key: completed.key,
                generation: completed.generation,
                definition: completed.definition,
                outcome: RuntimePaneStatusProviderOutcome::Succeeded("ready".to_string()),
            }),
            Some(true)
        );

        cache.reconcile(requests);
        let second_batch = claim_and_admit(&mut cache, 1);
        assert_eq!(second_batch.len(), 1);
        assert_eq!(second_batch[0].key, deferred);
    }

    /// Verifies CWD churn cancels outstanding generations before eviction so
    /// inactive work cannot exhaust either the pending or retained-state bound.
    #[test]
    fn pane_provider_cache_evicts_inactive_lru_contexts_for_cwd_churn() {
        let definition = pane_provider_definition(Some("initial"));
        let mut cache = RuntimePaneStatusProviderCache::default();
        let mut previous: Option<super::RuntimePaneStatusProviderRefreshPlan> = None;
        for index in 0..=MAX_CACHED_PANE_STATUS_PROVIDER_STATES {
            let key = pane_provider_key("%1", "branch", &format!("/workspace/churn-{index:03}"));
            cache.reconcile(vec![pane_provider_request(key, definition.clone())]);
            if let Some(previous) = previous.take() {
                assert!(previous.cancellation.is_cancelled());
            }
            previous = Some(claim_and_admit(&mut cache, 1).remove(0));
        }

        let current = pane_provider_key(
            "%1",
            "branch",
            &format!(
                "/workspace/churn-{:03}",
                MAX_CACHED_PANE_STATUS_PROVIDER_STATES
            ),
        );
        assert_eq!(cache.states.len(), MAX_CACHED_PANE_STATUS_PROVIDER_STATES);
        assert!(cache.states.contains_key(&current));
        assert_eq!(
            cache
                .values_for_pane("%1")
                .get("branch")
                .map(String::as_str),
            Some("initial")
        );
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
