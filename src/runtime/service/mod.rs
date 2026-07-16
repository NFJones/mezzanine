//! Runtime Service implementation.
//!
//! This module owns the runtime service boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use crate::terminal::AGENT_STATUS_ANIMATION_REFRESH_INTERVAL_MS;

use super::{
    AgentLogLevel, AgentSessionMetadata, AgentShellStore, AgentShellVisibility,
    AgentTranscriptStore, AgentTurnLedger, AgentTurnRecord, AgentTurnState, AgentTurnTrigger,
    AuditActor, AuditDeferredWrite, AuditLog, AuditRecord, AuthStore, BTreeMap, BTreeSet,
    BlockedApprovalQueue, BlockedApprovalRequest, ConfigFormat, ConfigLayer, ConfigScope,
    ControlIdempotencyCache, DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT,
    DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT,
    DEFAULT_AGENT_IMPLEMENTATION_PRESSURE_AFTER_SHELL_ACTIONS, DEFAULT_AGENT_LOOP_LIMIT,
    DEFAULT_AGENT_ROUTING, EventKind, EventLog, FocusedShellHookQueue, MEZ_ENV_FIELD_SEPARATOR,
    McpRegistry, McpServerStatus, McpStartupTransportPlan, MemoryRecord, MessageService, MezError,
    ModelProfile, ModelTokenUsage, ModelTokenUsageKey, PaneProcessManager, Path, PathBuf,
    PermissionAuthorityChange, PermissionPolicy, ProjectTrustStore, RenderInvalidationReason,
    Result, RuntimeConfigApplyReport, RuntimeHttpMcpTransportState, RuntimeLifecycleState,
    RuntimeMcpRetryReport, RuntimeMcpTransportSet, RuntimeModelProfileOverrideStore,
    RuntimePresentationSettings, RuntimePresetRegistry, RuntimeProviderConfig,
    RuntimeProviderRegistry, RuntimeRegistryUpdatePlan, RuntimeSessionService, RuntimeSideEffect,
    RuntimeTimerKey, RuntimeTimerKind, RuntimeTransition, Session, SessionApprovalStore,
    SessionMemoryStore, SessionRegistry, SnapshotRepository, TerminalClientLoopConfig,
    TrustDecision, Value, agent_shell_visibility_json_name, apply_registry_update,
    builtin_subagent_profiles, compare_approval_policy_authority, compose_effective_config,
    current_unix_seconds, discover_existing_overlays, discover_project_root,
    discover_streamable_http_mcp_server_with_auth_token, ensure_absolute, ensure_no_mez_separator,
    fs, json_escape, runtime_agent_action_failure_retry_limit_from_config,
    runtime_agent_auto_sizing_from_config,
    runtime_agent_compaction_raw_retention_percent_from_config,
    runtime_agent_custom_system_prompt_from_config,
    runtime_agent_implementation_pressure_after_shell_actions_from_config,
    runtime_agent_loop_limit_from_config, runtime_agent_personality_profiles_from_config,
    runtime_agent_routing_from_config, runtime_approval_policy_name, runtime_audit_config_present,
    runtime_audit_log_from_config, runtime_default_agent_personality_from_config,
    runtime_default_models_for_provider, runtime_effective_config_value,
    runtime_history_limit_from_config, runtime_history_rotate_lines_from_config,
    runtime_hook_definitions_from_config, runtime_host_clipboard_from_config,
    runtime_max_concurrent_agents_from_config, runtime_max_root_subagents_from_config,
    runtime_max_subagent_depth_from_config, runtime_max_subagent_panes_per_window_from_config,
    runtime_max_subagents_per_subagent_from_config, runtime_mcp_registry_from_config,
    runtime_pane_by_id, runtime_parse_approval_policy, runtime_permission_policy_from_config,
    runtime_preset_registry_from_config, runtime_provider_auth_refresh_leeway_seconds_from_config,
    runtime_provider_registry_from_config, runtime_saved_agent_session_limit_from_config,
    runtime_subagent_profiles_from_config, runtime_subagent_wait_policy_from_config,
    runtime_terminal_emoji_width_from_config,
    runtime_terminal_shell_output_preview_lines_from_config, runtime_terminal_term_from_config,
    spawn_stdio_mcp_connection,
};

// RuntimeSessionService construction, accessors, and live config application.

/// Default interval for status-only terminal refreshes.
const DEFAULT_STATUS_REFRESH_INTERVAL_MS: u64 = 1_000;

/// Returns whether the resolved terminal configuration needs periodic status refreshes.
fn runtime_status_refresh_required_by_config(config: &TerminalClientLoopConfig) -> bool {
    let window_status_requires_refresh = config.window_frames_enabled
        && config
            .frame_context
            .window_status
            .as_ref()
            .is_some_and(|status| !status.template.trim().is_empty());
    let agent_status_requires_refresh = config.frame_context.panes.values().any(|pane| {
        let active = matches!(
            pane.agent_status.as_deref(),
            Some("queued" | "running" | "thinking" | "executing" | "waiting" | "compacting")
        );
        let visible_surface = config.pane_frames_enabled
            || pane.agent_prompt.is_some()
            || pane.mode.as_deref() == Some("agent");
        active && visible_surface
    });
    window_status_requires_refresh || agent_status_requires_refresh
}

/// Returns the periodic status-refresh interval for one terminal configuration.
fn runtime_status_refresh_interval_ms_for_config(config: &TerminalClientLoopConfig) -> u64 {
    if config.frame_context.animation_tick_ms > 0 {
        AGENT_STATUS_ANIMATION_REFRESH_INTERVAL_MS
    } else {
        DEFAULT_STATUS_REFRESH_INTERVAL_MS
    }
}

/// Keeps only the final replacement effect for each configuration target.
pub(crate) fn coalesce_config_persistence_effects(
    effects: Vec<RuntimeSideEffect>,
) -> Vec<RuntimeSideEffect> {
    let mut coalesced = Vec::<RuntimeSideEffect>::new();
    for effect in effects {
        let RuntimeSideEffect::Persist {
            target,
            path,
            mode: crate::runtime::PersistenceWriteMode::Replace,
            ..
        } = &effect
        else {
            coalesced.push(effect);
            continue;
        };
        if let Some(existing) = coalesced.iter_mut().find(|existing| {
            matches!(
                existing,
                RuntimeSideEffect::Persist {
                    target: existing_target,
                    path: existing_path,
                    mode: crate::runtime::PersistenceWriteMode::Replace,
                    ..
                } if existing_target == target && existing_path == path
            )
        }) {
            *existing = effect;
        } else {
            coalesced.push(effect);
        }
    }
    coalesced
}

/// Converts one encoded audit write into its canonical persistence effect.
fn audit_persistence_effect(write: AuditDeferredWrite) -> RuntimeSideEffect {
    RuntimeSideEffect::PersistAuditLog {
        path: write.path,
        bytes: write.bytes,
        retention: write.retention,
    }
}

/// Returns persisted per-model token accounting with legacy aggregate fallback.
fn runtime_agent_token_usage_by_model_from_metadata(
    metadata: &AgentSessionMetadata,
) -> BTreeMap<ModelTokenUsageKey, ModelTokenUsage> {
    if !metadata.token_usage_by_model.is_empty() {
        return metadata.token_usage_by_model.clone();
    }
    if metadata.token_usage.is_zero() {
        return BTreeMap::new();
    }
    BTreeMap::from([(ModelTokenUsageKey::unknown(), metadata.token_usage)])
}

/// Aggregates per-model token accounting for legacy metadata readers.
fn runtime_agent_total_token_usage_by_model(
    usage_by_model: &BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
) -> ModelTokenUsage {
    let mut total = ModelTokenUsage::default();
    for usage in usage_by_model.values() {
        total.add_assign(*usage);
    }
    total
}

mod accessors;
mod config_apply;
mod construction;
mod mcp_helpers;
mod mcp_runtime;
mod persistence;
mod stores;
mod transcript_restore;
