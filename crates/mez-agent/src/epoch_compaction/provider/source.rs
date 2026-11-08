//! Read-only semantic source rendering with redaction before fragmentation.

use crate::epoch_compaction::{FrozenEpochCompactionSource, invalid};
use crate::{AgentContextResult, ContextSourceKind, ReplayCompactionPlan};

/// Renders the original selected head/prefix as untrusted reference data. Whole
/// canonical ownership units remain attributed; original IDs never enter model
/// text. Stable policy, retained tail and later arrivals are outside this port.
/// The lexical redactor is a conservative safeguard, not secret classification.
pub fn render_epoch_source(
    plan: ReplayCompactionPlan,
    max_bytes: usize,
) -> AgentContextResult<FrozenEpochCompactionSource> {
    FrozenEpochCompactionSource::render(plan, max_bytes, |events| {
        let mut text = String::new();
        for event in events {
            let block = event.block();
            let label = serde_json::to_string(&redact_content(&block.label))
                .map_err(|_| invalid("compactor source label could not be encoded"))?;
            text.push_str(&format!(
                "[source={} label={label}]\n",
                source_kind_name(block.source)
            ));
            text.push_str(&redact_content(&block.content));
            text.push_str("\n[/source]\n");
        }
        Ok(text)
    })
}

/// Redacts suspicious lexical tokens while preserving every structural
/// whitespace byte. Always run before UTF-8 fragmentation, so a credential
/// cannot straddle two independently inspected fragments.
pub fn redact_content(content: &str) -> String {
    content
        .split_inclusive(char::is_whitespace)
        .map(|segment| {
            let token = segment.trim_end_matches(char::is_whitespace);
            let whitespace = &segment[token.len()..];
            format!("{}{}", redact_token(token), whitespace)
        })
        .collect()
}

/// Applies the established conservative compactor token redaction policy.
pub fn redact_token(token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    if lower.contains("private")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower == "api"
        || lower.contains("password")
        || lower.contains("token")
        || lower.contains("credential")
        || lower.contains("secret")
        || token.contains("sk-")
        || token.contains('@') && token.contains('.')
    {
        "[redacted]".to_string()
    } else {
        token.to_string()
    }
}

/// Returns descriptive authorship/source labels without granting instruction,
/// provider continuity, execution, or MCP authority to the rendered text.
pub fn source_kind_name(source: ContextSourceKind) -> &'static str {
    match source {
        ContextSourceKind::System => "system",
        ContextSourceKind::UserInstruction => "user-instruction",
        ContextSourceKind::SkillInstruction => "skill-instruction",
        ContextSourceKind::DeveloperInstruction => "developer-instruction",
        ContextSourceKind::Policy => "policy",
        ContextSourceKind::Configuration => "configuration",
        ContextSourceKind::LocalMessage => "local-message",
        ContextSourceKind::PeerMessage => "peer-message",
        ContextSourceKind::RuntimeHint => "runtime-hint",
        ContextSourceKind::ProjectGuidance => "project-guidance",
        ContextSourceKind::Memory
        | ContextSourceKind::CompactionPrefix
        | ContextSourceKind::CompactionSelective => "memory",
        ContextSourceKind::PersistedContextDocument => "persisted-context-document",
        ContextSourceKind::Transcript => "transcript",
        ContextSourceKind::TranscriptUser => "transcript-user",
        ContextSourceKind::TranscriptAssistant => "transcript-assistant",
        ContextSourceKind::TranscriptTool => "transcript-tool",
        ContextSourceKind::CommittedEvidence => "committed-evidence",
        ContextSourceKind::RoutedHandoff => "routed-handoff",
        ContextSourceKind::ActionResult => "action-result",
        ContextSourceKind::McpCatalogSnapshot => "mcp-catalog-snapshot",
        ContextSourceKind::McpServerReference => "mcp-server-reference",
        ContextSourceKind::McpServerSearchResult => "mcp-server-search-result",
        ContextSourceKind::McpRetrievedManifest => "mcp-retrieved-manifest",
    }
}
