//! Runtime model command and profile display helpers.
//!
//! This module owns parsing and display helpers for live `/model` command
//! configuration state. Keeping the command argument grammar and override-scope
//! formatting separate from the runtime config root leaves live config
//! application focused on materializing effective configuration.

use std::collections::BTreeMap;

use mez_agent::ModelProfile;

use crate::error::{MezError, Result};
use crate::runtime::service_state::RuntimeModelProfileOverrideScope;

use super::RuntimeSessionService;

/// Carries Runtime Model Command Args state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RuntimeModelCommandArgs {
    /// Stores the profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) profile: Option<String>,
    /// Stores the reasoning profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) reasoning_profile: Option<String>,
    /// Stores the scope value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) scope: Option<String>,
    /// Stores the target value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) target: Option<String>,
    /// Stores the clear value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) clear: bool,
    /// Stores the list value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) list: bool,
    /// Stores the show value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) show: bool,
    /// Stores whether the command targets the routing auto-sizing router.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(crate) routing: bool,
}

/// Runs the runtime model command args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_model_command_args(args: &str) -> Result<RuntimeModelCommandArgs> {
    let mut parsed = RuntimeModelCommandArgs::default();
    let mut words = args.split_whitespace().peekable();
    while let Some(word) = words.next() {
        match word {
            "--scope" => {
                let scope = words
                    .next()
                    .ok_or_else(|| MezError::invalid_args("/model --scope requires a value"))?;
                parsed.scope = Some(scope.to_string());
            }
            "--target" => {
                let target = words
                    .next()
                    .ok_or_else(|| MezError::invalid_args("/model --target requires a value"))?;
                parsed.target = Some(target.to_string());
            }
            "--routing" | "--router" => parsed.routing = true,
            "--reasoning" | "--reasoning-level" | "--reasoning-profile" => {
                let reasoning = words
                    .next()
                    .ok_or_else(|| MezError::invalid_args("/model --reasoning requires a value"))?;
                parsed.reasoning_profile = Some(reasoning.to_string());
            }
            "--clear" | "clear" => parsed.clear = true,
            "list" if parsed.profile.is_none() && parsed.reasoning_profile.is_none() => {
                parsed.list = true;
            }
            "--show" | "show" => parsed.show = true,
            value if value.starts_with("--") => {
                return Err(MezError::invalid_args(format!(
                    "unknown /model option `{value}`"
                )));
            }
            value => {
                if parsed.list {
                    return Err(MezError::invalid_args(
                        "/model list does not accept model or reasoning arguments",
                    ));
                }
                if parsed.profile.is_none() {
                    parsed.profile = Some(value.to_string());
                } else if parsed.reasoning_profile.is_none() {
                    parsed.reasoning_profile = Some(value.to_string());
                } else {
                    return Err(MezError::invalid_args(
                        "/model accepts at most a model name and optional reasoning level",
                    ));
                }
            }
        }
    }
    if parsed.clear
        && (parsed.profile.is_some() || parsed.reasoning_profile.is_some() || parsed.list)
    {
        return Err(MezError::invalid_args(
            "/model clear cannot be combined with list, model, or reasoning arguments",
        ));
    }
    if parsed.list && parsed.reasoning_profile.is_some() {
        return Err(MezError::invalid_args(
            "/model list cannot be combined with a reasoning level",
        ));
    }
    if parsed.routing && (parsed.scope.is_some() || parsed.target.is_some()) {
        return Err(MezError::invalid_args(
            "/model --routing cannot be combined with --scope or --target",
        ));
    }
    Ok(parsed)
}

/// Runs the runtime model override scope for args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_model_override_scope_for_args(
    service: &RuntimeSessionService,
    pane_id: &str,
    agent_id: &str,
    args: &RuntimeModelCommandArgs,
) -> Result<RuntimeModelProfileOverrideScope> {
    let scope = args.scope.as_deref().unwrap_or("pane");
    match scope {
        "session" => Ok(RuntimeModelProfileOverrideScope::Session),
        "window" => {
            let window_id = if let Some(target) = args.target.as_deref() {
                target.to_string()
            } else {
                service
                    .find_pane_descriptor(pane_id)
                    .ok_or_else(|| {
                        MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")
                    })?
                    .window_id
                    .to_string()
            };
            Ok(RuntimeModelProfileOverrideScope::Window(window_id))
        }
        "pane" => Ok(RuntimeModelProfileOverrideScope::Pane(
            args.target.as_deref().unwrap_or(pane_id).to_string(),
        )),
        "agent" => Ok(RuntimeModelProfileOverrideScope::Agent(
            args.target.as_deref().unwrap_or(agent_id).to_string(),
        )),
        "subagent" => {
            let target = args.target.as_deref().ok_or_else(|| {
                MezError::invalid_args("/model --scope subagent requires --target")
            })?;
            Ok(RuntimeModelProfileOverrideScope::Subagent(
                target.to_string(),
            ))
        }
        _ => Err(MezError::invalid_args(
            "/model --scope must be session, window, pane, agent, or subagent",
        )),
    }
}

/// Runs the runtime model override scope name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_model_override_scope_name(
    scope: &RuntimeModelProfileOverrideScope,
) -> String {
    match scope {
        RuntimeModelProfileOverrideScope::Session => "session".to_string(),
        RuntimeModelProfileOverrideScope::Window(id) => format!("window:{id}"),
        RuntimeModelProfileOverrideScope::Pane(id) => format!("pane:{id}"),
        RuntimeModelProfileOverrideScope::Agent(id) => format!("agent:{id}"),
        RuntimeModelProfileOverrideScope::Subagent(id) => format!("subagent:{id}"),
    }
}
/// Supported pane-local model latency preferences in display order.
pub(crate) const RUNTIME_LATENCY_PREFERENCES: &[&str] = &["slow", "default", "fast"];

/// Validates a user-facing latency preference value.
pub(crate) fn runtime_validate_latency_preference(value: &str) -> Result<&str> {
    let value = value.trim();
    if RUNTIME_LATENCY_PREFERENCES
        .iter()
        .any(|allowed| allowed == &value)
    {
        Ok(value)
    } else {
        Err(MezError::invalid_args(format!(
            "latency preference must be slow, default, or fast, got {value:?}"
        )))
    }
}

/// Runs the runtime model profile display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_model_profile_display(
    active_name: &str,
    active_profile: &ModelProfile,
    profiles: &BTreeMap<String, ModelProfile>,
) -> String {
    let available = profiles
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "active_profile={} provider={} model={} latency_preference={} profiles={}",
        active_name,
        active_profile.provider,
        active_profile.model,
        active_profile
            .latency_preference
            .as_deref()
            .unwrap_or("default"),
        available
    )
}
