//! Runtime agent and subagent option readers.
//!
//! This module owns `[agents]`, `[agents.auto_sizing]`, `[subagents]`, and
//! `[personalities]` materialization from effective runtime config. Keeping
//! these readers together separates agent scheduling/profile policy from
//! terminal, frame, provider, MCP, permission, and hook config domains.

use super::*;

/// Parses the maximum number of concurrently scheduled agent turns.
pub(in crate::runtime) fn runtime_max_concurrent_agents_from_config(root: &Value) -> Result<usize> {
    runtime_positive_agents_usize_from_config(
        root,
        "max_concurrent_agents",
        DEFAULT_MAX_CONCURRENT_AGENTS,
    )
}

/// Parses the retained raw-tail percentage used during context compaction.
pub(in crate::runtime) fn runtime_agent_compaction_raw_retention_percent_from_config(
    root: &Value,
) -> Result<usize> {
    let Some(agents) = runtime_json_object(root, "agents") else {
        return Ok(DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT);
    };
    let Some(value) = agents.get("compaction_raw_retention_percent") else {
        return Ok(DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT);
    };
    let percent = value.as_u64().ok_or_else(|| {
        MezError::config("agents.compaction_raw_retention_percent must be an integer from 1 to 100")
    })?;
    if !(1..=100).contains(&percent) {
        return Err(MezError::config(
            "agents.compaction_raw_retention_percent must be an integer from 1 to 100",
        ));
    }
    Ok(percent as usize)
}

/// Parses whether routing model and reasoning sizing is enabled.
pub(in crate::runtime) fn runtime_agent_routing_from_config(root: &Value) -> Result<bool> {
    let Some(agents) = runtime_json_object(root, "agents") else {
        return Ok(DEFAULT_AGENT_ROUTING);
    };
    let Some(value) = agents.get("routing") else {
        return Ok(DEFAULT_AGENT_ROUTING);
    };
    runtime_json_bool(Some(value))
        .ok_or_else(|| MezError::config("agents.routing must be a boolean"))
}

/// Parses user-configured system prompt text appended to the base prompt.
pub(in crate::runtime) fn runtime_agent_custom_system_prompt_from_config(
    root: &Value,
) -> Result<Option<String>> {
    let Some(agents) = runtime_json_object(root, "agents") else {
        return Ok(None);
    };
    let Some(value) = agents.get("custom_system_prompt") else {
        return Ok(None);
    };
    let prompt = value
        .as_str()
        .ok_or_else(|| MezError::config("agents.custom_system_prompt must be a string"))?;
    Ok((!prompt.trim().is_empty()).then(|| prompt.to_string()))
}

/// Parses the configured default personality profile id.
pub(in crate::runtime) fn runtime_default_agent_personality_from_config(
    root: &Value,
) -> Result<Option<String>> {
    let Some(agents) = runtime_json_object(root, "agents") else {
        return Ok(None);
    };
    let Some(value) = agents.get("default_personality") else {
        return Ok(None);
    };
    let profile = value
        .as_str()
        .ok_or_else(|| MezError::config("agents.default_personality must be a string"))?;
    if profile.trim().is_empty() {
        return Ok(None);
    }
    validate_agent_personality_profile_id(profile)?;
    Ok(Some(profile.to_string()))
}

/// Parses the model-correctable action failure retry budget.
pub(in crate::runtime) fn runtime_agent_action_failure_retry_limit_from_config(
    root: &Value,
) -> Result<usize> {
    runtime_positive_agents_usize_from_config(
        root,
        "action_failure_retry_limit",
        DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT,
    )
}

/// Parses the shell-command streak that triggers implementation-pressure hints.
pub(in crate::runtime) fn runtime_agent_implementation_pressure_after_shell_actions_from_config(
    root: &Value,
) -> Result<usize> {
    runtime_positive_agents_usize_from_config(
        root,
        "implementation_pressure_after_shell_actions",
        DEFAULT_AGENT_IMPLEMENTATION_PRESSURE_AFTER_SHELL_ACTIONS,
    )
}

/// Parses the `/loop` work-iteration budget from `[agents]`.
pub(in crate::runtime) fn runtime_agent_loop_limit_from_config(root: &Value) -> Result<usize> {
    runtime_positive_agents_usize_from_config(root, "loop_limit", DEFAULT_AGENT_LOOP_LIMIT)
}

/// Parses the configured local action executor from `[agents]`.
pub(in crate::runtime) fn runtime_agent_local_action_executor_from_config(
    root: &Value,
) -> Result<RuntimeLocalActionExecutor> {
    let Some(agents) = runtime_json_object(root, "agents") else {
        return Ok(DEFAULT_AGENT_LOCAL_ACTION_EXECUTOR);
    };
    let Some(value) = agents.get("local_action_executor") else {
        return Ok(DEFAULT_AGENT_LOCAL_ACTION_EXECUTOR);
    };
    let value = value
        .as_str()
        .ok_or_else(|| MezError::config("agents.local_action_executor must be a string"))?;
    match value {
        "pane_shell" => Ok(RuntimeLocalActionExecutor::PaneShell),
        "native" => Ok(RuntimeLocalActionExecutor::Native),
        _ => Err(MezError::config(
            "agents.local_action_executor must be pane_shell or native",
        )),
    }
}

/// Parses automatic turn model-sizing configuration from `[agents.auto_sizing]`.
pub(in crate::runtime) fn runtime_agent_auto_sizing_from_config(
    root: &Value,
) -> Result<RuntimeAutoSizingConfig> {
    let Some(agents) = runtime_json_object(root, "agents") else {
        return Ok(RuntimeAutoSizingConfig::default());
    };
    let Some(auto_sizing) = agents.get("auto_sizing").and_then(Value::as_object) else {
        return Ok(RuntimeAutoSizingConfig::default());
    };
    let mut config = RuntimeAutoSizingConfig::default();
    if let Some(profile) = runtime_json_string(auto_sizing.get("router_model_profile")) {
        config.router_model_profile = profile.to_string();
    }
    if let Some(profile) = runtime_json_string(auto_sizing.get("small_model_profile")) {
        config.small_model_profile = profile.to_string();
    }
    if let Some(profile) = runtime_json_string(auto_sizing.get("medium_model_profile")) {
        config.medium_model_profile = profile.to_string();
    }
    if let Some(profile) = runtime_json_string(auto_sizing.get("large_model_profile")) {
        config.large_model_profile = profile.to_string();
    }
    if let Some(value) = auto_sizing.get("allowed_reasoning_efforts") {
        config.allowed_reasoning_efforts =
            runtime_json_string_array(Some(value))?.ok_or_else(|| {
                MezError::config("agents.auto_sizing.allowed_reasoning_efforts must be an array")
            })?;
        if config.allowed_reasoning_efforts.is_empty() {
            return Err(MezError::config(
                "agents.auto_sizing.allowed_reasoning_efforts must not be empty",
            ));
        }
    }
    if let Some(policy) = runtime_json_string(auto_sizing.get("fallback_policy")) {
        config.fallback_policy = match policy {
            DEFAULT_AUTO_SIZING_FALLBACK_POLICY => {
                RuntimeAutoSizingFallbackPolicy::UseDefaultProfile
            }
            other => {
                return Err(MezError::config(format!(
                    "agents.auto_sizing.fallback_policy `{other}` is not supported"
                )));
            }
        };
    }
    for (path, value) in [
        (
            "agents.auto_sizing.router_model_profile",
            config.router_model_profile.as_str(),
        ),
        (
            "agents.auto_sizing.small_model_profile",
            config.small_model_profile.as_str(),
        ),
        (
            "agents.auto_sizing.medium_model_profile",
            config.medium_model_profile.as_str(),
        ),
        (
            "agents.auto_sizing.large_model_profile",
            config.large_model_profile.as_str(),
        ),
    ] {
        if value.trim().is_empty() {
            return Err(MezError::config(format!("{path} must not be empty")));
        }
    }
    for effort in &config.allowed_reasoning_efforts {
        if !matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh") {
            return Err(MezError::config(format!(
                "agents.auto_sizing.allowed_reasoning_efforts contains unsupported effort `{effort}`"
            )));
        }
    }
    Ok(config)
}

/// Parses one positive integer setting from the `[agents]` table.
fn runtime_positive_agents_usize_from_config(
    root: &Value,
    key: &str,
    default: usize,
) -> Result<usize> {
    let Some(agents) = runtime_json_object(root, "agents") else {
        return Ok(default);
    };
    let Some(value) = agents.get(key) else {
        return Ok(default);
    };
    let Some(limit) = value.as_u64() else {
        return Err(MezError::config(format!(
            "agents.{key} must be a positive integer"
        )));
    };
    let limit = usize::try_from(limit)
        .map_err(|_| MezError::config(format!("agents.{key} is too large")))?;
    if limit == 0 {
        return Err(MezError::config(format!(
            "agents.{key} must be greater than zero"
        )));
    }
    Ok(limit)
}

/// Parses the maximum number of subagent panes that may share one window.
pub(in crate::runtime) fn runtime_max_subagent_panes_per_window_from_config(
    root: &Value,
) -> Result<usize> {
    runtime_positive_agents_usize_from_config(
        root,
        "max_subagent_panes_per_window",
        DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW,
    )
}

/// Parses the maximum direct subagents available to a root pane agent.
pub(in crate::runtime) fn runtime_max_root_subagents_from_config(root: &Value) -> Result<usize> {
    runtime_positive_agents_usize_from_config(
        root,
        "max_root_subagents",
        DEFAULT_MAX_ROOT_SUBAGENTS,
    )
}

/// Parses the maximum direct subagents available to a spawned subagent.
pub(in crate::runtime) fn runtime_max_subagents_per_subagent_from_config(
    root: &Value,
) -> Result<usize> {
    runtime_positive_agents_usize_from_config(
        root,
        "max_subagents_per_subagent",
        DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT,
    )
}

/// Parses the maximum nested subagent delegation depth.
pub(in crate::runtime) fn runtime_max_subagent_depth_from_config(root: &Value) -> Result<usize> {
    runtime_positive_agents_usize_from_config(root, "max_depth", DEFAULT_MAX_SUBAGENT_DEPTH)
}

/// Parses how parent agent turns wait for MAAP-spawned child subagents.
pub(in crate::runtime) fn runtime_subagent_wait_policy_from_config(
    root: &Value,
) -> Result<SubagentWaitPolicy> {
    let Some(agents) = runtime_json_object(root, "agents") else {
        return Ok(DEFAULT_SUBAGENT_WAIT_POLICY);
    };
    let Some(value) = agents.get("subagent_wait_policy") else {
        return Ok(DEFAULT_SUBAGENT_WAIT_POLICY);
    };
    let Some(policy) = runtime_json_string(Some(value)) else {
        return Err(MezError::config(
            "agents.subagent_wait_policy must be a string",
        ));
    };
    match policy {
        "join" | "join-and-wait" | "wait" => Ok(SubagentWaitPolicy::Join),
        "detach" | "fire-and-forget" => Ok(SubagentWaitPolicy::Detach),
        _ => Err(MezError::config(
            "agents.subagent_wait_policy must be join or detach",
        )),
    }
}

/// Runs the runtime subagent profiles from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_subagent_profiles_from_config(
    root: &Value,
) -> Result<BTreeMap<String, SubagentProfile>> {
    let mut profiles = builtin_subagent_profiles();
    let Some(configured) = runtime_json_object(root, "subagents") else {
        return Ok(profiles);
    };
    for (profile_id, value) in configured {
        validate_subagent_profile_id(profile_id)?;
        let object = value
            .as_object()
            .ok_or_else(|| MezError::config("subagent profile must be an object"))?;
        let name = runtime_json_string(object.get("name"))
            .unwrap_or(profile_id)
            .to_string();
        let description = runtime_json_string(object.get("description"))
            .unwrap_or("")
            .to_string();
        let developer_instructions = runtime_json_string(object.get("developer_instructions"))
            .or_else(|| runtime_json_string(object.get("developer_prompt")))
            .map(ToOwned::to_owned);
        let model_profile = runtime_json_string(object.get("model_profile"))
            .or_else(|| runtime_json_string(object.get("model_profile_override")))
            .map(ToOwned::to_owned);
        let permission_preset = runtime_json_string(object.get("permission_preset"))
            .or_else(|| runtime_json_string(object.get("permission_override")))
            .map(runtime_config_permission_preset)
            .transpose()?;
        let mcp_servers = runtime_json_string_array(object.get("mcp_servers"))?.unwrap_or_default();
        let shell_env = runtime_json_string_map(object.get("shell_env"))?.unwrap_or_default();
        let default_cooperation_mode = runtime_json_string(object.get("default_cooperation_mode"))
            .or_else(|| runtime_json_string(object.get("default_mode")))
            .map(runtime_cooperation_mode)
            .transpose()?;
        let default_read_scopes =
            runtime_json_string_array(object.get("default_read_scopes"))?.unwrap_or_default();
        let default_write_scopes =
            runtime_json_string_array(object.get("default_write_scopes"))?.unwrap_or_default();
        profiles.insert(
            profile_id.clone(),
            SubagentProfile {
                id: profile_id.clone(),
                name,
                description,
                developer_instructions,
                model_profile,
                permission_preset,
                mcp_servers,
                shell_env,
                default_cooperation_mode,
                default_read_scopes,
                default_write_scopes,
            },
        );
    }
    Ok(profiles)
}

/// Parses user-defined agent personality profiles.
pub(in crate::runtime) fn runtime_agent_personality_profiles_from_config(
    root: &Value,
) -> Result<BTreeMap<String, RuntimeAgentPersonalityProfile>> {
    let mut profiles = BTreeMap::new();
    let Some(configured) = runtime_json_object(root, "personalities") else {
        return Ok(profiles);
    };
    for (profile_id, value) in configured {
        validate_agent_personality_profile_id(profile_id)?;
        let object = value
            .as_object()
            .ok_or_else(|| MezError::config("personality profile must be an object"))?;
        let profile = RuntimeAgentPersonalityProfile {
            id: profile_id.clone(),
            name: runtime_json_string(object.get("name")).map(ToOwned::to_owned),
            system_prompt: runtime_json_string(object.get("system_prompt"))
                .or_else(|| runtime_json_string(object.get("instructions")))
                .map(ToOwned::to_owned),
            response_style: runtime_json_string(object.get("response_style"))
                .or_else(|| runtime_json_string(object.get("style")))
                .map(ToOwned::to_owned),
            model_profile: runtime_json_string(object.get("model_profile")).map(ToOwned::to_owned),
            planning_enabled: runtime_json_bool(object.get("planning_enabled"))
                .or_else(|| runtime_json_bool(object.get("planning"))),
            routing_enabled: runtime_json_bool(object.get("routing_enabled"))
                .or_else(|| runtime_json_bool(object.get("routing"))),
        };
        profiles.insert(profile_id.clone(), profile);
    }
    Ok(profiles)
}

/// Validates one configured personality profile id.
///
/// # Parameters
/// - `profile_id`: The candidate profile id from config or a slash command.
fn validate_agent_personality_profile_id(profile_id: &str) -> Result<()> {
    if profile_id.is_empty()
        || !profile_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(MezError::config(
            "personality profile id must contain only ASCII letters, digits, hyphen, or underscore",
        ));
    }
    Ok(())
}

/// Runs the validate subagent profile id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_subagent_profile_id(profile_id: &str) -> Result<()> {
    if profile_id.trim().is_empty()
        || profile_id.chars().any(char::is_control)
        || profile_id.contains('/')
    {
        return Err(MezError::config("subagent profile name is invalid"));
    }
    Ok(())
}
