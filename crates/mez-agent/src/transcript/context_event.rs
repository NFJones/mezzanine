//! Typed model-context events stored inside durable transcripts.
//!
//! Ordinary transcript roles describe conversational messages, while some
//! provider-independent context must survive between turns without pretending
//! to be user or assistant speech. This module gives those records a reserved,
//! versioned system-entry encoding. Decoders reject malformed, unknown, and
//! unsupported payloads so durable audit records cannot become model context by
//! accident.

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    ContextExecutionGroupId, ContextSourceKind, ProviderContinuityOwner, ProviderTranscriptEvent,
};

/// Marker prefix for provider-independent transcript context events.
pub const TRANSCRIPT_CONTEXT_EVENT_MARKER: &str = "[mez-transcript-context-event/v1]\n";

/// Wire-format version for transcript context events.
const TRANSCRIPT_CONTEXT_EVENT_VERSION: &str = "mez-transcript-context-event/v1";
/// Event kind for a summarized routed-worker handoff.
const ROUTED_HANDOFF_KIND: &str = "routed_handoff";
/// Event kind for a user turn stopped before normal completion.
const INTERRUPTED_TURN_KIND: &str = "interrupted_turn";
/// Event kind for one immutable pane-environment projection.
const ENVIRONMENT_SNAPSHOT_KIND: &str = "environment_snapshot";
/// Event kind for one immutable configured MCP catalog projection.
const MCP_CATALOG_SNAPSHOT_KIND: &str = "mcp_catalog_snapshot";
/// Event kind for one durable MCP authorization-reset compaction boundary.
const MCP_COMPACTION_EPOCH_KIND: &str = "mcp_compaction_epoch";
/// Event kind for one exact context block immediately preceding a user event.
const PROMPT_BOUNDARY_KIND: &str = "prompt_boundary";
/// Event kind for one exact cache-visible execution block.
const EXECUTION_BLOCK_KIND: &str = "execution_block";
/// Maximum serialized environment projection accepted from durable storage.
const ENVIRONMENT_SNAPSHOT_CONTENT_LIMIT_BYTES: usize = 64 * 1024;
/// Maximum exact prompt-boundary content accepted from durable storage.
const PROMPT_BOUNDARY_CONTENT_LIMIT_BYTES: usize = 256 * 1024;
/// Maximum exact prompt-boundary label accepted from durable storage.
const PROMPT_BOUNDARY_LABEL_LIMIT_BYTES: usize = 4 * 1024;
/// Maximum exact execution-block content accepted from durable storage.
const EXECUTION_BLOCK_CONTENT_LIMIT_BYTES: usize = crate::http::DEFAULT_PROVIDER_MAX_RESPONSE_BYTES;
/// Maximum exact execution-block label accepted from durable storage.
const EXECUTION_BLOCK_LABEL_LIMIT_BYTES: usize = 4 * 1024;

/// Provider-independent context that is durable across conversation turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptContextEvent {
    /// Validated routed-worker summary presented through the parent model.
    RoutedHandoff {
        /// Serialized summarized handoff content.
        content: String,
    },
    /// Canonical pane-environment projection sampled at a user-prompt boundary.
    EnvironmentSnapshot {
        /// SHA-256 digest of the exact model-visible projection.
        projection_sha256: String,
        /// Exact model-visible environment projection.
        content: String,
    },
    /// Configured always-exposed MCP catalog sampled at a chronology boundary.
    McpCatalogSnapshot {
        /// SHA-256 digest of the exact model-visible catalog projection.
        projection_sha256: String,
        /// Original non-zero chronological event sequence.
        event_sequence: u64,
        /// Exact model-visible configured MCP catalog projection.
        content: String,
    },
    /// Durable compaction boundary that invalidates prior retrieved MCP manifests.
    McpCompactionEpoch,
    /// One exact pre-user context event retained in chronological order.
    PromptBoundary {
        /// Original provider-neutral context provenance.
        source: ContextSourceKind,
        /// SHA-256 digest of source, label, and exact model-visible content.
        projection_sha256: String,
        /// Exact model-visible block label.
        label: String,
        /// Exact model-visible block content.
        content: String,
    },
    /// One exact cache-visible block from a completed execution group.
    ExecutionBlock {
        /// Original provider-neutral context provenance.
        source: ContextSourceKind,
        /// SHA-256 digest of source, label, and exact model-visible content.
        projection_sha256: String,
        /// Stable causal execution owner for new exact records.
        execution_group_id: Option<ContextExecutionGroupId>,
        /// Non-zero ordinal within the causal execution group.
        ordinal: Option<u64>,
        /// Exclusive provider owner for opaque native continuity records.
        provider_owner: Option<ProviderContinuityOwner>,
        /// Exact model-visible block label.
        label: String,
        /// Exact canonical model-visible content.
        content: String,
    },
    /// Original intent and settled observations retained when a turn stops.
    InterruptedTurn {
        /// Original user prompt for the stopped turn.
        prompt: String,
        /// Runtime reason for the interruption.
        reason: String,
        /// Safely serializable action observations available at interruption.
        evidence: Vec<String>,
    },
}

impl TranscriptContextEvent {
    /// Builds one validated immutable environment-snapshot event.
    ///
    /// Empty or oversized projections return `None`; accepted content receives
    /// a digest over the exact bytes that durable replay will present.
    pub fn environment_snapshot(content: impl Into<String>) -> Option<Self> {
        let content = content.into();
        if content.trim().is_empty() || content.len() > ENVIRONMENT_SNAPSHOT_CONTENT_LIMIT_BYTES {
            return None;
        }
        let projection_sha256 = Sha256::digest(content.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Some(Self::EnvironmentSnapshot {
            projection_sha256,
            content,
        })
    }

    /// Builds one validated immutable configured MCP catalog snapshot.
    pub fn mcp_catalog_snapshot(content: impl Into<String>, event_sequence: u64) -> Option<Self> {
        let content = content.into();
        if content.trim().is_empty()
            || content.len() > EXECUTION_BLOCK_CONTENT_LIMIT_BYTES
            || event_sequence == 0
        {
            return None;
        }
        let projection_sha256 = mcp_catalog_snapshot_sha256(&content, event_sequence);
        Some(Self::McpCatalogSnapshot {
            projection_sha256,
            event_sequence,
            content,
        })
    }

    /// Builds one validated exact prompt-boundary event.
    pub fn prompt_boundary(
        source: ContextSourceKind,
        label: impl Into<String>,
        content: impl Into<String>,
    ) -> Option<Self> {
        let label = label.into();
        let content = content.into();
        if !valid_prompt_boundary(source, &label, &content) {
            return None;
        }
        Some(Self::PromptBoundary {
            source,
            projection_sha256: prompt_boundary_sha256(source, &label, &content),
            label,
            content,
        })
    }

    /// Builds one validated exact execution-block event.
    ///
    /// Only execution-group source kinds are accepted. Labels and canonical
    /// content are bounded before durable storage so replay never has to
    /// rewrite bytes that previously entered cache-eligible context.
    pub fn execution_block(
        source: ContextSourceKind,
        label: impl Into<String>,
        content: impl Into<String>,
    ) -> Option<Self> {
        let label = label.into();
        let content = content.into();
        if !valid_execution_block(source, &label, &content) {
            return None;
        }
        Some(Self::ExecutionBlock {
            source,
            projection_sha256: execution_block_sha256(source, &label, &content, None, None, None),
            execution_group_id: None,
            ordinal: None,
            provider_owner: None,
            label,
            content,
        })
    }

    /// Builds one validated exact execution block with durable causal metadata.
    pub fn execution_block_with_metadata(
        source: ContextSourceKind,
        label: impl Into<String>,
        content: impl Into<String>,
        execution_group_id: ContextExecutionGroupId,
        ordinal: u64,
        provider_owner: Option<ProviderContinuityOwner>,
    ) -> Option<Self> {
        let label = label.into();
        let content = content.into();
        if !valid_execution_block(source, &label, &content) || ordinal == 0 {
            return None;
        }
        if provider_owner
            .as_ref()
            .is_some_and(ProviderContinuityOwner::is_legacy)
        {
            return None;
        }
        if let Some(owner) = provider_owner.as_ref()
            && ProviderTranscriptEvent::from_transcript_content(&content)
                .is_none_or(|event| !owner.accepts_transcript_event(&event))
        {
            return None;
        }
        let projection_sha256 = execution_block_sha256(
            source,
            &label,
            &content,
            Some(execution_group_id.as_str()),
            Some(ordinal),
            provider_owner.as_ref(),
        );
        Some(Self::ExecutionBlock {
            source,
            projection_sha256,
            execution_group_id: Some(execution_group_id),
            ordinal: Some(ordinal),
            provider_owner,
            label,
            content,
        })
    }

    /// Encodes one event as a reserved system transcript entry.
    pub fn to_transcript_content(&self) -> String {
        let payload = match self {
            Self::RoutedHandoff { content } => serde_json::json!({
                "version": TRANSCRIPT_CONTEXT_EVENT_VERSION,
                "kind": ROUTED_HANDOFF_KIND,
                "content": content,
            }),
            Self::EnvironmentSnapshot {
                projection_sha256,
                content,
            } => serde_json::json!({
                "version": TRANSCRIPT_CONTEXT_EVENT_VERSION,
                "kind": ENVIRONMENT_SNAPSHOT_KIND,
                "projection_sha256": projection_sha256,
                "content": content,
            }),
            Self::McpCatalogSnapshot {
                projection_sha256,
                event_sequence,
                content,
            } => serde_json::json!({
                "version": TRANSCRIPT_CONTEXT_EVENT_VERSION,
                "kind": MCP_CATALOG_SNAPSHOT_KIND,
                "projection_sha256": projection_sha256,
                "event_sequence": event_sequence,
                "content": content,
            }),
            Self::McpCompactionEpoch => serde_json::json!({
                "version": TRANSCRIPT_CONTEXT_EVENT_VERSION,
                "kind": MCP_COMPACTION_EPOCH_KIND,
            }),
            Self::PromptBoundary {
                source,
                projection_sha256,
                label,
                content,
            } => serde_json::json!({
                "version": TRANSCRIPT_CONTEXT_EVENT_VERSION,
                "kind": PROMPT_BOUNDARY_KIND,
                "source": prompt_boundary_source_name(*source),
                "projection_sha256": projection_sha256,
                "label": label,
                "content": content,
            }),
            Self::ExecutionBlock {
                source,
                projection_sha256,
                execution_group_id,
                ordinal,
                provider_owner,
                label,
                content,
            } => serde_json::json!({
                "version": TRANSCRIPT_CONTEXT_EVENT_VERSION,
                "kind": EXECUTION_BLOCK_KIND,
                "source": execution_block_source_name(*source),
                "projection_sha256": projection_sha256,
                "execution_group_id": execution_group_id.as_ref().map(ContextExecutionGroupId::as_str),
                "ordinal": ordinal,
                "provider_owner": provider_owner.as_ref().map(provider_owner_json),
                "label": label,
                "content": content,
            }),
            Self::InterruptedTurn {
                prompt,
                reason,
                evidence,
            } => serde_json::json!({
                "version": TRANSCRIPT_CONTEXT_EVENT_VERSION,
                "kind": INTERRUPTED_TURN_KIND,
                "prompt": prompt,
                "reason": reason,
                "evidence": evidence,
            }),
        };
        format!(
            "{}{}",
            TRANSCRIPT_CONTEXT_EVENT_MARKER,
            serde_json::to_string(&payload)
                .expect("transcript context event payload contains only JSON values")
        )
    }

    /// Decodes a supported reserved transcript context event.
    ///
    /// Malformed payloads, unknown kinds, unsupported versions, and empty
    /// routed handoffs return `None` so callers never inject them into model
    /// context.
    pub fn from_transcript_content(content: &str) -> Option<Self> {
        let payload = content.strip_prefix(TRANSCRIPT_CONTEXT_EVENT_MARKER)?;
        let value: Value = serde_json::from_str(payload.trim()).ok()?;
        if value.get("version")?.as_str()? != TRANSCRIPT_CONTEXT_EVENT_VERSION {
            return None;
        }
        match value.get("kind")?.as_str()? {
            ROUTED_HANDOFF_KIND => {
                let content = value.get("content")?.as_str()?.trim();
                if content.is_empty() {
                    return None;
                }
                Some(Self::RoutedHandoff {
                    content: content.to_string(),
                })
            }
            ENVIRONMENT_SNAPSHOT_KIND => {
                let projection_sha256 = value.get("projection_sha256")?.as_str()?;
                let content = value.get("content")?.as_str()?;
                if !valid_environment_snapshot(projection_sha256, content) {
                    return None;
                }
                Some(Self::EnvironmentSnapshot {
                    projection_sha256: projection_sha256.to_string(),
                    content: content.to_string(),
                })
            }
            MCP_CATALOG_SNAPSHOT_KIND => {
                let projection_sha256 = value.get("projection_sha256")?.as_str()?;
                let event_sequence = value.get("event_sequence")?.as_u64()?;
                let content = value.get("content")?.as_str()?;
                if !valid_mcp_catalog_snapshot(projection_sha256, content, event_sequence) {
                    return None;
                }
                Some(Self::McpCatalogSnapshot {
                    projection_sha256: projection_sha256.to_string(),
                    event_sequence,
                    content: content.to_string(),
                })
            }
            MCP_COMPACTION_EPOCH_KIND => Some(Self::McpCompactionEpoch),
            PROMPT_BOUNDARY_KIND => {
                let source = prompt_boundary_source(value.get("source")?.as_str()?)?;
                let projection_sha256 = value.get("projection_sha256")?.as_str()?;
                let label = value.get("label")?.as_str()?;
                let content = value.get("content")?.as_str()?;
                if !valid_prompt_boundary(source, label, content)
                    || prompt_boundary_sha256(source, label, content) != projection_sha256
                {
                    return None;
                }
                Some(Self::PromptBoundary {
                    source,
                    projection_sha256: projection_sha256.to_string(),
                    label: label.to_string(),
                    content: content.to_string(),
                })
            }
            EXECUTION_BLOCK_KIND => {
                let source = execution_block_source(value.get("source")?.as_str()?)?;
                let label = value.get("label")?.as_str()?;
                let content = value.get("content")?.as_str()?;
                if !valid_execution_block(source, label, content) {
                    return None;
                }
                let execution_group_id = value
                    .get("execution_group_id")
                    .and_then(Value::as_str)
                    .map(ContextExecutionGroupId::new)
                    .transpose()
                    .ok()?;
                let ordinal = value.get("ordinal").and_then(Value::as_u64);
                if execution_group_id.is_some() != ordinal.is_some()
                    || ordinal.is_some_and(|ordinal| ordinal == 0)
                {
                    return None;
                }
                let provider_owner = match value.get("provider_owner") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(provider_owner_from_json(value)?),
                };
                if provider_owner.is_some() && execution_group_id.is_none() {
                    return None;
                }
                if let Some(owner) = provider_owner.as_ref()
                    && ProviderTranscriptEvent::from_transcript_content(content)
                        .is_none_or(|event| !owner.accepts_transcript_event(&event))
                {
                    return None;
                }
                let projection_sha256 = value.get("projection_sha256").and_then(Value::as_str);
                let expected = execution_block_sha256(
                    source,
                    label,
                    content,
                    execution_group_id
                        .as_ref()
                        .map(ContextExecutionGroupId::as_str),
                    ordinal,
                    provider_owner.as_ref(),
                );
                if (execution_group_id.is_some() && projection_sha256.is_none())
                    || projection_sha256.is_some_and(|digest| digest != expected)
                {
                    return None;
                }
                Some(Self::ExecutionBlock {
                    source,
                    projection_sha256: projection_sha256.unwrap_or(&expected).to_string(),
                    execution_group_id,
                    ordinal,
                    provider_owner,
                    label: label.to_string(),
                    content: content.to_string(),
                })
            }
            INTERRUPTED_TURN_KIND => {
                let prompt = value.get("prompt")?.as_str()?.trim();
                let reason = value.get("reason")?.as_str()?.trim();
                if prompt.is_empty() || reason.is_empty() {
                    return None;
                }
                let evidence = value
                    .get("evidence")?
                    .as_array()?
                    .iter()
                    .map(|entry| entry.as_str().map(str::to_string))
                    .collect::<Option<Vec<_>>>()?;
                Some(Self::InterruptedTurn {
                    prompt: prompt.to_string(),
                    reason: reason.to_string(),
                    evidence,
                })
            }
            _ => None,
        }
    }
}

/// Validates one environment snapshot before durable content becomes model-visible.
fn valid_environment_snapshot(projection_sha256: &str, content: &str) -> bool {
    if content.trim().is_empty()
        || content.len() > ENVIRONMENT_SNAPSHOT_CONTENT_LIMIT_BYTES
        || projection_sha256.len() != 64
        || !projection_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return false;
    }
    let digest = Sha256::digest(content.as_bytes());
    let expected = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    projection_sha256 == expected
}

/// Validates one configured MCP catalog snapshot before durable replay.
fn valid_mcp_catalog_snapshot(projection_sha256: &str, content: &str, event_sequence: u64) -> bool {
    if content.trim().is_empty()
        || content.len() > EXECUTION_BLOCK_CONTENT_LIMIT_BYTES
        || event_sequence == 0
        || projection_sha256.len() != 64
        || !projection_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return false;
    }
    projection_sha256 == mcp_catalog_snapshot_sha256(content, event_sequence)
}

/// Digests exact catalog bytes together with their chronological identity.
fn mcp_catalog_snapshot_sha256(content: &str, event_sequence: u64) -> String {
    let material = format!("{event_sequence}\0{content}");
    Sha256::digest(material.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Returns the durable wire name for an allowlisted prompt-boundary source.
fn prompt_boundary_source_name(source: ContextSourceKind) -> &'static str {
    match source {
        ContextSourceKind::SkillInstruction => "skill_instruction",
        ContextSourceKind::LocalMessage => "local_message",
        ContextSourceKind::Policy => "policy",
        ContextSourceKind::Configuration => "configuration",
        _ => "unsupported",
    }
}

/// Decodes one allowlisted prompt-boundary source.
fn prompt_boundary_source(source: &str) -> Option<ContextSourceKind> {
    match source {
        "skill_instruction" => Some(ContextSourceKind::SkillInstruction),
        "local_message" => Some(ContextSourceKind::LocalMessage),
        "policy" => Some(ContextSourceKind::Policy),
        "configuration" => Some(ContextSourceKind::Configuration),
        _ => None,
    }
}

/// Validates exact prompt-boundary fields before they become model-visible.
fn valid_prompt_boundary(source: ContextSourceKind, label: &str, content: &str) -> bool {
    prompt_boundary_source_name(source) != "unsupported"
        && !label.trim().is_empty()
        && label.len() <= PROMPT_BOUNDARY_LABEL_LIMIT_BYTES
        && !content.trim().is_empty()
        && content.len() <= PROMPT_BOUNDARY_CONTENT_LIMIT_BYTES
        && !label.bytes().any(|byte| byte == 0)
        && !content.bytes().any(|byte| byte == 0)
}

/// Digests the exact prompt-boundary identity without ambiguous concatenation.
fn prompt_boundary_sha256(source: ContextSourceKind, label: &str, content: &str) -> String {
    let material = format!(
        "{}\0{}\0{}",
        prompt_boundary_source_name(source),
        label,
        content
    );
    Sha256::digest(material.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Returns the durable wire name for an execution-block source.
fn execution_block_source_name(source: ContextSourceKind) -> &'static str {
    match source {
        ContextSourceKind::CommittedEvidence => "committed_evidence",
        ContextSourceKind::TranscriptAssistant => "transcript_assistant",
        ContextSourceKind::TranscriptTool => "transcript_tool",
        ContextSourceKind::ActionResult => "action_result",
        ContextSourceKind::McpServerReference => "mcp_server_reference",
        ContextSourceKind::McpServerSearchResult => "mcp_server_search_result",
        ContextSourceKind::McpRetrievedManifest => "mcp_retrieved_manifest",
        _ => "unsupported",
    }
}

/// Decodes one allowlisted execution-block source.
fn execution_block_source(source: &str) -> Option<ContextSourceKind> {
    match source {
        "committed_evidence" => Some(ContextSourceKind::CommittedEvidence),
        "transcript_assistant" => Some(ContextSourceKind::TranscriptAssistant),
        "transcript_tool" => Some(ContextSourceKind::TranscriptTool),
        "action_result" => Some(ContextSourceKind::ActionResult),
        "mcp_server_reference" => Some(ContextSourceKind::McpServerReference),
        "mcp_server_search_result" => Some(ContextSourceKind::McpServerSearchResult),
        "mcp_retrieved_manifest" => Some(ContextSourceKind::McpRetrievedManifest),
        _ => None,
    }
}

/// Validates exact execution-block fields before they become model-visible.
fn valid_execution_block(source: ContextSourceKind, label: &str, content: &str) -> bool {
    execution_block_source_name(source) != "unsupported"
        && !label.trim().is_empty()
        && label.len() <= EXECUTION_BLOCK_LABEL_LIMIT_BYTES
        && !content.trim().is_empty()
        && content.len() <= EXECUTION_BLOCK_CONTENT_LIMIT_BYTES
        && !label.bytes().any(|byte| byte == 0)
        && !content.bytes().any(|byte| byte == 0)
}

/// Digests one exact execution block without ambiguous field concatenation.
fn execution_block_sha256(
    source: ContextSourceKind,
    label: &str,
    content: &str,
    execution_group_id: Option<&str>,
    ordinal: Option<u64>,
    provider_owner: Option<&ProviderContinuityOwner>,
) -> String {
    let material = if execution_group_id.is_none() && ordinal.is_none() && provider_owner.is_none()
    {
        format!(
            "{}\0{}\0{}",
            execution_block_source_name(source),
            label,
            content
        )
    } else {
        format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            execution_block_source_name(source),
            label,
            content,
            execution_group_id.unwrap_or_default(),
            ordinal.map_or_else(String::new, |value| value.to_string()),
            provider_owner.map_or_else(String::new, provider_owner_hash_material),
        )
    };
    Sha256::digest(material.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Structured owner format emitted for exact configured-provider ownership.
const PROVIDER_CONTINUITY_OWNER_VERSION: &str = "mez-provider-continuity-owner/v1";

/// Projects one owner into its durable scalar or structured representation.
fn provider_owner_json(owner: &ProviderContinuityOwner) -> Value {
    if let Some(provider_id) = owner.legacy_provider_id() {
        return Value::String(provider_id.to_string());
    }
    serde_json::json!({
        "version": PROVIDER_CONTINUITY_OWNER_VERSION,
        "api": owner.api().as_str(),
        "provider_id": owner.provider_id(),
    })
}

/// Decodes legacy scalar owners and current structured exact owners.
fn provider_owner_from_json(value: &Value) -> Option<ProviderContinuityOwner> {
    if let Some(provider_id) = value.as_str() {
        return ProviderContinuityOwner::from_legacy_provider_id(provider_id);
    }
    let value = value.as_object()?;
    if value.len() != 3
        || !value.contains_key("version")
        || !value.contains_key("api")
        || !value.contains_key("provider_id")
    {
        return None;
    }
    if value.get("version")?.as_str()? != PROVIDER_CONTINUITY_OWNER_VERSION {
        return None;
    }
    let api = crate::ProviderApiCompatibility::from_id(value.get("api")?.as_str()?)?;
    ProviderContinuityOwner::new(api, value.get("provider_id")?.as_str()?)
}

/// Returns stable hash material while preserving historical scalar digests.
fn provider_owner_hash_material(owner: &ProviderContinuityOwner) -> String {
    owner.legacy_provider_id().map_or_else(
        || {
            format!(
                "{}\0{}\0{}",
                PROVIDER_CONTINUITY_OWNER_VERSION,
                owner.api().as_str(),
                owner.provider_id()
            )
        },
        str::to_string,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies routed-handoff context survives an exact typed transcript
    /// round trip without changing its serialized summary.
    #[test]
    fn routed_handoff_transcript_context_event_round_trips() {
        let event = TranscriptContextEvent::RoutedHandoff {
            content: r#"{"version":1,"result_summary":"done"}"#.to_string(),
        };

        let encoded = event.to_transcript_content();

        assert!(encoded.starts_with(TRANSCRIPT_CONTEXT_EVENT_MARKER));
        assert_eq!(
            TranscriptContextEvent::from_transcript_content(&encoded),
            Some(event)
        );
    }

    /// Verifies an environment snapshot retains its exact projection and
    /// digest across durable encoding and decoding.
    #[test]
    fn environment_snapshot_transcript_context_event_round_trips() {
        let content = "environment_state=known\nshell=posix-sh".to_string();
        let projection_sha256 = Sha256::digest(content.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let event = TranscriptContextEvent::EnvironmentSnapshot {
            projection_sha256,
            content,
        };

        let encoded = event.to_transcript_content();

        assert_eq!(
            TranscriptContextEvent::from_transcript_content(&encoded),
            Some(event)
        );
    }

    /// Verifies a configured MCP catalog snapshot retains exact manifest bytes
    /// and rejects content rewritten after its digest was committed.
    #[test]
    fn mcp_catalog_snapshot_transcript_context_event_round_trips() {
        let event = TranscriptContextEvent::mcp_catalog_snapshot(
            "available_servers=1 available_tools=1 unavailable_servers=0\nconfigured_exposure=\"state\" action=mcp_call\navailable_tool=state/list",
            42,
        )
        .unwrap();
        let encoded = event.to_transcript_content();

        assert_eq!(
            TranscriptContextEvent::from_transcript_content(&encoded),
            Some(event)
        );
        let tampered = encoded.replace("state/list", "state/delete");
        assert!(TranscriptContextEvent::from_transcript_content(&tampered).is_none());
        let tampered = encoded.replace("\"event_sequence\":42", "\"event_sequence\":43");
        assert!(TranscriptContextEvent::from_transcript_content(&tampered).is_none());
    }

    /// Verifies MCP compaction boundaries round-trip as durable control events.
    #[test]
    fn mcp_compaction_epoch_transcript_context_event_round_trips() {
        let event = TranscriptContextEvent::McpCompactionEpoch;

        let encoded = event.to_transcript_content();

        assert_eq!(
            TranscriptContextEvent::from_transcript_content(&encoded),
            Some(event)
        );
        let malformed = encoded.replace("mcp_compaction_epoch", "unknown_epoch");
        assert!(TranscriptContextEvent::from_transcript_content(&malformed).is_none());
    }

    /// Verifies every allowlisted pre-user source round-trips exactly and a
    /// tampered digest or unsupported source cannot become model context.
    #[test]
    fn prompt_boundary_transcript_context_event_validates_source_and_digest() {
        for source in [
            ContextSourceKind::SkillInstruction,
            ContextSourceKind::LocalMessage,
            ContextSourceKind::Policy,
            ContextSourceKind::Configuration,
        ] {
            let event = TranscriptContextEvent::prompt_boundary(
                source,
                "prompt boundary",
                "exact pre-user content",
            )
            .unwrap();
            let encoded = event.to_transcript_content();
            assert_eq!(
                TranscriptContextEvent::from_transcript_content(&encoded),
                Some(event)
            );

            let tampered = encoded.replace("exact pre-user content", "rewritten content");
            assert!(TranscriptContextEvent::from_transcript_content(&tampered).is_none());
        }
        assert!(
            TranscriptContextEvent::prompt_boundary(
                ContextSourceKind::UserInstruction,
                "user prompt",
                "must remain a direct user event",
            )
            .is_none()
        );
    }

    /// Verifies one canonical execution block preserves provenance, label, and
    /// model-visible bytes across durable transcript encoding and replay.
    #[test]
    fn execution_block_transcript_context_event_round_trips() {
        let group = ContextExecutionGroupId::new("execution-group-1").unwrap();
        let event = TranscriptContextEvent::execution_block_with_metadata(
            ContextSourceKind::ActionResult,
            "action result shell-1",
            "[action_result shell-1 shell_command succeeded]\noutput:\nexact output",
            group,
            2,
            None,
        )
        .unwrap();

        let encoded = event.to_transcript_content();

        assert_eq!(
            TranscriptContextEvent::from_transcript_content(&encoded),
            Some(event)
        );
        let tampered = encoded.replace("exact output", "rewritten output");
        assert!(TranscriptContextEvent::from_transcript_content(&tampered).is_none());
        let tampered = encoded.replace("\"ordinal\":2", "\"ordinal\":3");
        assert!(TranscriptContextEvent::from_transcript_content(&tampered).is_none());
        assert!(
            TranscriptContextEvent::execution_block(
                ContextSourceKind::UserInstruction,
                "user prompt",
                "must not become an execution block",
            )
            .is_none()
        );
    }

    /// Verifies historical scalar OpenAI and DeepSeek owners still decode and
    /// validate against the digest material emitted before exact ownership.
    #[test]
    fn execution_block_accepts_legacy_provider_owner_encoding_and_hashes() {
        for (provider_id, content, expected_digest) in [
            (
                "openai",
                concat!(
                    "[mez-provider-transcript-event/v1]\n",
                    r#"{"version":"mez-provider-transcript-event/v1","provider":"openai","kind":"response_output","items":[{"type":"reasoning","id":"reasoning-1"}]}"#,
                ),
                "4a3b0997056ce5a280a049cda41c4e41d7818400d2d2a08e535cb4882bca12c9",
            ),
            (
                "deepseek",
                concat!(
                    "[mez-provider-transcript-event/v1]\n",
                    r#"{"version":"mez-provider-transcript-event/v1","provider":"deepseek","kind":"tool_result","tool_call_id":"call-1","content":"exact output"}"#,
                ),
                "f0173280fcfb6ea6abc4d64951ef048477c1f07590b41e21d184cd7ab12ef305",
            ),
        ] {
            let encoded = format!(
                "{TRANSCRIPT_CONTEXT_EVENT_MARKER}{}",
                serde_json::json!({
                    "version": TRANSCRIPT_CONTEXT_EVENT_VERSION,
                    "kind": EXECUTION_BLOCK_KIND,
                    "source": "transcript_tool",
                    "projection_sha256": expected_digest,
                    "execution_group_id": "execution-group-legacy",
                    "ordinal": 1,
                    "provider_owner": provider_id,
                    "label": "legacy native event",
                    "content": content,
                })
            );
            let event = TranscriptContextEvent::from_transcript_content(&encoded).unwrap();
            let restored = event.to_transcript_content();

            assert!(restored.contains("\"provider_owner\":\""));
            assert!(restored.contains(expected_digest));
            let TranscriptContextEvent::ExecutionBlock {
                source,
                execution_group_id: Some(group),
                ordinal: Some(ordinal),
                provider_owner: Some(owner),
                label,
                content,
                ..
            } = &event
            else {
                panic!("legacy fixture should decode as a complete execution block");
            };
            assert!(
                TranscriptContextEvent::execution_block_with_metadata(
                    *source,
                    label.clone(),
                    content.clone(),
                    group.clone(),
                    *ordinal,
                    Some(owner.clone()),
                )
                .is_none()
            );
            assert_eq!(
                TranscriptContextEvent::from_transcript_content(&restored),
                Some(event)
            );
            let tampered = encoded.replace(expected_digest, &"0".repeat(64));
            assert!(TranscriptContextEvent::from_transcript_content(&tampered).is_none());
        }
    }

    /// Verifies current exact owners use a versioned structured encoding whose
    /// API and provider-id dimensions are both protected by the block digest.
    #[test]
    fn execution_block_exact_provider_owner_round_trips_and_rejects_tampering() {
        let content = ProviderTranscriptEvent::validated_openai_response_output(vec![
            serde_json::json!({"type":"reasoning","id":"reasoning-1"}),
        ])
        .unwrap()
        .to_transcript_content();
        let owner = ProviderContinuityOwner::new(
            crate::ProviderApiCompatibility::OpenAiResponses,
            "configured-openai",
        )
        .unwrap();
        let event = TranscriptContextEvent::execution_block_with_metadata(
            ContextSourceKind::TranscriptTool,
            "native response",
            content,
            ContextExecutionGroupId::new("execution-group-exact").unwrap(),
            1,
            Some(owner),
        )
        .unwrap();
        let encoded = event.to_transcript_content();

        assert!(encoded.contains("mez-provider-continuity-owner/v1"));
        assert!(encoded.contains("openai-responses"));
        assert!(encoded.contains("configured-openai"));
        assert_eq!(
            TranscriptContextEvent::from_transcript_content(&encoded),
            Some(event)
        );
        let provider_tamper = encoded.replace("configured-openai", "other-openai");
        assert!(TranscriptContextEvent::from_transcript_content(&provider_tamper).is_none());
        let api_tamper = encoded.replace("openai-responses", "openai-chat-completions");
        assert!(TranscriptContextEvent::from_transcript_content(&api_tamper).is_none());
        let version_tamper = encoded.replace(
            "mez-provider-continuity-owner/v1",
            "mez-provider-continuity-owner/v2",
        );
        assert!(TranscriptContextEvent::from_transcript_content(&version_tamper).is_none());
        let extra_field = encoded.replace("\"api\":", "\"unexpected\":true,\"api\":");
        assert!(TranscriptContextEvent::from_transcript_content(&extra_field).is_none());

        let chat_owner = ProviderContinuityOwner::new(
            crate::ProviderApiCompatibility::OpenAiChatCompletions,
            "configured-chat",
        )
        .unwrap();
        let chat_json = provider_owner_json(&chat_owner);
        assert_eq!(provider_owner_from_json(&chat_json), Some(chat_owner));
    }

    /// Verifies durable MCP reference and search evidence retain their typed
    /// provenance across transcript encoding without becoming manifests.
    #[test]
    fn mcp_reference_and_search_execution_blocks_round_trip() {
        for (source, label, content) in [
            (
                ContextSourceKind::McpServerReference,
                "MCP server reference fs",
                "mcp_server_reference={\"server_id\":\"fs\"}",
            ),
            (
                ContextSourceKind::McpServerSearchResult,
                "MCP server search result search-1",
                "mcp_server_search={\"servers\":[{\"server_id\":\"fs\"}]}",
            ),
        ] {
            let event = TranscriptContextEvent::execution_block(source, label, content)
                .expect("typed MCP evidence should be serializable");
            let encoded = event.to_transcript_content();

            assert_eq!(
                TranscriptContextEvent::from_transcript_content(&encoded),
                Some(event)
            );
        }
    }

    /// Verifies an interrupted turn retains its original intent and only the
    /// caller-provided safe action observations for subsequent continuation.
    #[test]
    fn interrupted_turn_transcript_context_event_round_trips() {
        let event = TranscriptContextEvent::InterruptedTurn {
            prompt: "repair the interrupted task".to_string(),
            reason: "agent turn stopped".to_string(),
            evidence: vec!["action_id=read-1 type=shell_command status=running".to_string()],
        };

        let encoded = event.to_transcript_content();

        assert_eq!(
            TranscriptContextEvent::from_transcript_content(&encoded),
            Some(event)
        );
    }

    /// Verifies malformed, unsupported, unknown, and empty context records are
    /// ignored rather than becoming model-visible durable context.
    #[test]
    fn transcript_context_event_rejects_unsupported_payloads() {
        for payload in [
            "not json",
            r#"{"version":"mez-transcript-context-event/v2","kind":"routed_handoff","content":"summary"}"#,
            r#"{"version":"mez-transcript-context-event/v1","kind":"unknown","content":"summary"}"#,
            r#"{"version":"mez-transcript-context-event/v1","kind":"routed_handoff","content":""}"#,
            r#"{"version":"mez-transcript-context-event/v1","kind":"environment_snapshot","projection_sha256":"invalid","content":"environment_state=known"}"#,
            r#"{"version":"mez-transcript-context-event/v1","kind":"environment_snapshot","projection_sha256":"0000000000000000000000000000000000000000000000000000000000000000","content":"environment_state=known"}"#,
        ] {
            let encoded = format!("{TRANSCRIPT_CONTEXT_EVENT_MARKER}{payload}");
            assert!(
                TranscriptContextEvent::from_transcript_content(&encoded).is_none(),
                "unexpectedly decoded {payload}"
            );
        }
        assert!(
            TranscriptContextEvent::from_transcript_content("ordinary system record").is_none()
        );
    }
}
