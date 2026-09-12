//! Agent-session checkpoint validation tests.

use std::collections::BTreeMap;

use crate::transcript::AgentSessionMetadata;
use crate::{
    AllowedAction, AllowedActionSet, ModelProfile, ModelTokenUsage, ModelTokenUsageKey,
    SpawnAgentSizeOption, SpawnAgentSizing,
};

fn captured_execution_profile() -> ModelProfile {
    ModelProfile {
        provider: "test-provider".to_string(),
        model: "test-model".to_string(),
        ..Default::default()
    }
}

fn valid_checkpoint() -> AgentSessionMetadata {
    AgentSessionMetadata {
        mezzanine_session_id: "$1".to_string(),
        pane_id: "%1".to_string(),
        conversation_id: "conversation-1".to_string(),
        prompt_cache_lineage_id: "lineage-1".to_string(),
        visibility: "visible".to_string(),
        running_turn_id: None,
        running_turn_kind: None,
        transcript_entries: 1,
        log_level: "normal".to_string(),
        pane_model_profile: None,
        planning_enabled: false,
        response_style: None,
        directive: None,
        routing_enabled: None,
        root_routing_policy: None,
        approval_policy: Some("ask".to_string()),
        pane_permission_preset_override: None,
        pane_approval_policy_override: None,
        working_directory: None,
        project_root: None,
        token_usage: ModelTokenUsage::default(),
        token_usage_by_model: BTreeMap::<ModelTokenUsageKey, ModelTokenUsage>::new(),
        context_usage: None,
        context_usage_snapshot: None,
        latest_request_usage: None,
        allowed_actions: None,
    }
}

/// Verifies checkpoint enums and required identities share lower validation.
///
/// The product TSV adapter must not be the sole authority for persisted
/// visibility, log-level, and approval-policy spellings.
#[test]
fn agent_session_checkpoint_rejects_unknown_policy_values() {
    let mut checkpoint = valid_checkpoint();
    checkpoint.validate().unwrap();

    checkpoint.approval_policy = Some("host-access".to_string());
    checkpoint.validate().unwrap();

    checkpoint.approval_policy = Some("host-everything".to_string());
    assert!(checkpoint.validate().is_err());

    checkpoint.approval_policy = Some("ask".to_string());
    checkpoint.log_level = "unknown".to_string();
    assert!(checkpoint.validate().is_err());

    checkpoint.log_level = "normal".to_string();
    checkpoint.root_routing_policy = Some("in-place".to_string());
    checkpoint.validate().unwrap();

    checkpoint.root_routing_policy = Some("invalid".to_string());
    assert!(checkpoint.validate().is_err());
}

/// Verifies restored session catalogs accept configurable action subsets and
/// reject legacy controller-only actions that cannot appear in static MAAP.
#[test]
fn agent_session_checkpoint_validates_persisted_action_catalog() {
    let mut checkpoint = valid_checkpoint();
    checkpoint.allowed_actions = Some(AllowedActionSet::from_actions([
        AllowedAction::Say,
        AllowedAction::ShellCommand,
    ]));
    checkpoint.validate().unwrap();

    checkpoint.allowed_actions = Some(AllowedActionSet::from_actions([
        AllowedAction::RequestCapability,
    ]));
    assert!(checkpoint.validate().is_err());
}

/// Verifies durable catalogs reject empty surfaces and schema metadata that is
/// detached from its owning action or carries malformed routed-size values.
#[test]
fn agent_session_checkpoint_rejects_malformed_persisted_action_catalogs() {
    let mut checkpoint = valid_checkpoint();
    checkpoint.allowed_actions = Some(AllowedActionSet::from_actions([]));
    assert!(checkpoint.validate().is_err());

    checkpoint.allowed_actions = Some(
        AllowedActionSet::from_actions([AllowedAction::Say]).with_spawn_agent_sizing(
            SpawnAgentSizing {
                sizes: vec![SpawnAgentSizeOption {
                    size: "small".to_string(),
                    profile_name: "small-profile".to_string(),
                    execution_profile: None,
                    allowed_reasoning_efforts: vec!["medium".to_string()],
                }],
            },
        ),
    );
    assert!(checkpoint.validate().is_err());

    checkpoint.allowed_actions = Some(
        AllowedActionSet::from_actions([AllowedAction::SpawnAgent]).with_spawn_agent_sizing(
            SpawnAgentSizing {
                sizes: vec![
                    SpawnAgentSizeOption {
                        size: "small".to_string(),
                        profile_name: "small-profile".to_string(),
                        execution_profile: None,
                        allowed_reasoning_efforts: vec!["medium".to_string()],
                    },
                    SpawnAgentSizeOption {
                        size: "small".to_string(),
                        profile_name: "small-profile".to_string(),
                        execution_profile: None,
                        allowed_reasoning_efforts: vec!["unsupported".to_string()],
                    },
                ],
            },
        ),
    );
    assert!(checkpoint.validate().is_err());
}

/// Verifies distinct routed sizes may intentionally share one profile while
/// persisted validation still treats duplicate size names as malformed.
#[test]
fn agent_session_checkpoint_allows_shared_sizing_profile_names() {
    let mut checkpoint = valid_checkpoint();
    checkpoint.allowed_actions = Some(
        AllowedActionSet::from_actions([AllowedAction::SpawnAgent]).with_spawn_agent_sizing(
            SpawnAgentSizing {
                sizes: vec![
                    SpawnAgentSizeOption {
                        size: "small".to_string(),
                        profile_name: "shared-profile".to_string(),
                        execution_profile: Some(captured_execution_profile()),
                        allowed_reasoning_efforts: vec!["low".to_string()],
                    },
                    SpawnAgentSizeOption {
                        size: "medium".to_string(),
                        profile_name: "shared-profile".to_string(),
                        execution_profile: Some(captured_execution_profile()),
                        allowed_reasoning_efforts: vec!["high".to_string()],
                    },
                ],
            },
        ),
    );

    checkpoint.validate().unwrap();
}
