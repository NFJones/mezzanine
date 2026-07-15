//! Agent-session checkpoint validation tests.

use std::collections::BTreeMap;

use crate::transcript::AgentSessionMetadata;
use crate::{ModelTokenUsage, ModelTokenUsageKey};

fn valid_checkpoint() -> AgentSessionMetadata {
    AgentSessionMetadata {
        mezzanine_session_id: "$1".to_string(),
        pane_id: "%1".to_string(),
        conversation_id: "conversation-1".to_string(),
        prompt_cache_lineage_id: "lineage-1".to_string(),
        visibility: "visible".to_string(),
        running_turn_id: None,
        transcript_entries: 1,
        log_level: "normal".to_string(),
        pane_model_profile: None,
        planning_enabled: false,
        response_style: None,
        directive: None,
        routing_enabled: None,
        approval_policy: Some("ask".to_string()),
        working_directory: None,
        project_root: None,
        token_usage: ModelTokenUsage::default(),
        token_usage_by_model: BTreeMap::<ModelTokenUsageKey, ModelTokenUsage>::new(),
        context_usage: None,
        context_usage_snapshot: None,
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

    checkpoint.log_level = "unknown".to_string();
    assert!(checkpoint.validate().is_err());
}
