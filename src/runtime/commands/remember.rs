//! Agent memory and git-diff helper routines for runtime commands.
//!
//! The parent command module owns live command execution. This child module
//! keeps `/remember` model-request shaping, model-output validation, memory
//! record construction, and git snapshot helpers together so durable-memory
//! command behavior is isolated from unrelated command families.

use super::super::{
    AgentContext, ContextSourceKind, MemoryRecord, MemoryScope, MemorySource, MezError,
    ModelProfile, PathBuf, Result, json_escape,
};
use super::compaction;
use crate::agent::{
    AgentActionPayload, AllowedActionSet, ModelInteractionKind, ModelMessage, ModelMessageRole,
    ModelRequest, ModelResponse,
};
use crate::memory::{MemoryKind, MemoryState};
use std::process::Command;

/// Normalized memory candidate returned by the `/remember` model request.
pub(super) struct RuntimeRememberCandidate {
    /// Durable memory taxonomy selected by the model and validated by runtime.
    pub(super) kind: MemoryKind,
    /// Retrieval priority clamped to the persistent-memory valid range.
    priority: u8,
    /// One-line stable summary used for ids and display.
    pub(super) summary: String,
    /// Search/index terms that should appear in stored content.
    keywords: Vec<String>,
    /// Relevance hints that help retrieval choose the record later.
    cues: Vec<String>,
    /// Durable body stored as the main memory content.
    content: String,
    /// Optional retention period requested by the model.
    expires_in_days: Option<u64>,
}

impl RuntimeRememberCandidate {
    /// Converts one validated model candidate into a persistent memory record.
    pub(super) fn into_record(
        self,
        id: String,
        scope: MemoryScope,
        now: u64,
        default_ttl_days: u64,
    ) -> Result<MemoryRecord> {
        let mut record = MemoryRecord::new_with_defaults(
            id,
            scope,
            now,
            now,
            MemorySource::Agent,
            self.priority,
            runtime_remember_memory_content(&self),
        );
        record.kind = self.kind;
        record.state = MemoryState::Active;
        let ttl_days = self.expires_in_days.unwrap_or(default_ttl_days).max(1);
        let duration = ttl_days.checked_mul(86_400).ok_or_else(|| {
            MezError::invalid_state("remember memory expiration duration overflow")
        })?;
        record.expiration_duration_seconds = Some(duration);
        record.expires_at_unix_seconds = now.checked_add(duration);
        if record.expires_at_unix_seconds.is_none() {
            return Err(MezError::invalid_state(
                "remember memory expiration timestamp overflow",
            ));
        }
        record.validate_for_persistence()?;
        Ok(record)
    }
}

/// Builds bounded context source text for no-argument `/remember`.
pub(super) fn runtime_remember_context_source(pane_id: &str, context: &AgentContext) -> String {
    let mut lines = vec![
        format!("Pane: {pane_id}"),
        format!("Context blocks supplied: {}", context.blocks.len()),
    ];
    for (index, block) in context.blocks.iter().enumerate() {
        lines.push(format!(
            "context_block={} source={} label={} content={}",
            index,
            compaction::runtime_context_source_kind_name(block.source),
            json_escape(&block.label),
            compaction::runtime_model_compaction_entry_content(&block.content)
        ));
    }
    lines.join("\n")
}

/// Builds the provider request that asks the active model for durable memories.
pub(super) fn runtime_model_remember_request(
    profile: &ModelProfile,
    pane_id: &str,
    session_id: &str,
    context_mode: bool,
    source_text: &str,
    default_ttl_days: u64,
) -> Result<ModelRequest> {
    let agent_id = format!("agent-{pane_id}");
    let turn_id = format!("remember-{session_id}-{pane_id}");
    let mode = if context_mode { "context" } else { "statement" };
    Ok(ModelRequest {
        provider: profile.provider.clone(),
        model: profile.model.clone(),
        reasoning_effort: profile
            .provider_options
            .get("reasoning_effort")
            .cloned()
            .or_else(|| profile.reasoning_profile.clone()),
        thinking_enabled: profile.thinking_enabled(),
        prompt_cache_retention: profile.provider_options.get("prompt_cache_retention").cloned(),
        latency_preference: profile.latency_preference.clone(),
        max_output_tokens: profile.max_output_tokens(),
        temperature: None,
        prompt_cache_session_id: Some(session_id.to_string()),
        prompt_cache_lineage_id: None,
        turn_id,
        agent_id,
        available_mcp_tools: Vec::new(),
        memory_actions_enabled: false,
        interaction_kind: ModelInteractionKind::ActionExecution,
        allowed_actions: AllowedActionSet::say_only(),
        stop: None,
        messages: vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                content: "You are Mezzanine's durable memory generator. Extract only stable, reusable memories and omit credentials, secrets, tokens, sensitive personal data, and transient terminal noise unless the user explicitly supplied the exact statement to remember."
                    .to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Developer,
                source: ContextSourceKind::DeveloperInstruction,
                content: format!("Return exactly one final say action whose text is strict JSON with a top-level `memories` array. Each memory object must contain: `kind` (`preference`, `fact`, `procedure`, `episode`, `warning`, or `scratch`), `priority` (1-255), `summary`, `keywords` array, `cues` array, and `content`. Include optional `expires_in_days` only when the source implies a more appropriate retention period; otherwise omit it and Mezzanine will use the configured default of {default_ttl_days} days. Include only durable information that will help future turns. Make `content` self-contained and retrieval-friendly. For statement mode, use only the supplied statement as source. For context mode, return at most six high-value memories.")
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::Transcript,
                content: format!("Remember mode: {mode}\n\n{source_text}"),
            },
        ],
    })
}

/// Parses and validates memory candidates from a model response.
pub(super) fn runtime_remember_candidates_from_response(
    response: &ModelResponse,
) -> Result<Vec<RuntimeRememberCandidate>> {
    let text = response
        .action_batch
        .as_ref()
        .and_then(|batch| {
            batch.actions.iter().find_map(|action| {
                if let AgentActionPayload::Say { text, .. } = &action.payload {
                    Some(text.trim())
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| response.raw_text.trim());
    let value = serde_json::from_str::<serde_json::Value>(text).map_err(|error| {
        MezError::invalid_state(format!(
            "remember model response was not strict JSON: {error}"
        ))
    })?;
    let memories = value
        .get("memories")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| MezError::invalid_state("remember JSON must contain a memories array"))?;
    memories
        .iter()
        .take(6)
        .map(runtime_remember_candidate_from_value)
        .collect()
}

/// Parses one model-authored memory candidate object.
fn runtime_remember_candidate_from_value(
    value: &serde_json::Value,
) -> Result<RuntimeRememberCandidate> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_state("remember memory candidate must be an object"))?;
    let kind = runtime_remember_kind(
        object
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("fact"),
    )?;
    let priority = object
        .get("priority")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(128)
        .clamp(1, 255) as u8;
    let summary = runtime_required_remember_string(object.get("summary"), "summary")?;
    let content = runtime_required_remember_string(object.get("content"), "content")?;
    Ok(RuntimeRememberCandidate {
        kind,
        priority,
        summary,
        keywords: runtime_remember_string_array(object.get("keywords")),
        cues: runtime_remember_string_array(object.get("cues")),
        content,
        expires_in_days: object
            .get("expires_in_days")
            .and_then(serde_json::Value::as_u64),
    })
}

/// Parses a required non-empty string from one `/remember` candidate field.
fn runtime_required_remember_string(
    value: Option<&serde_json::Value>,
    field: &str,
) -> Result<String> {
    let text = value
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| MezError::invalid_state(format!("remember candidate missing {field}")))?;
    Ok(text.to_string())
}

/// Parses a string array from one optional `/remember` candidate field.
fn runtime_remember_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .take(16)
        .collect()
}

/// Parses a memory kind name accepted by `/remember` model output.
fn runtime_remember_kind(kind: &str) -> Result<MemoryKind> {
    match kind {
        "preference" => Ok(MemoryKind::Preference),
        "fact" => Ok(MemoryKind::Fact),
        "procedure" => Ok(MemoryKind::Procedure),
        "episode" => Ok(MemoryKind::Episode),
        "warning" => Ok(MemoryKind::Warning),
        "scratch" => Ok(MemoryKind::Scratch),
        _ => Err(MezError::invalid_state(format!(
            "unknown remember memory kind `{kind}`"
        ))),
    }
}

/// Formats retrieval-friendly text for one stored `/remember` memory.
fn runtime_remember_memory_content(candidate: &RuntimeRememberCandidate) -> String {
    let mut lines = vec![format!("Summary: {}", candidate.summary)];
    if !candidate.keywords.is_empty() {
        lines.push(format!("Keywords: {}", candidate.keywords.join(", ")));
    }
    if !candidate.cues.is_empty() {
        lines.push(format!("Relevant when: {}", candidate.cues.join("; ")));
    }
    lines.push(format!("Memory: {}", candidate.content));
    lines.join("\n")
}

/// Builds a stable local id slug from a model-authored memory summary.
pub(super) fn runtime_remember_slug(summary: &str, index: usize) -> String {
    let mut slug = summary
        .chars()
        .filter_map(|character| {
            if character.is_ascii_alphanumeric() {
                Some(character.to_ascii_lowercase())
            } else if character.is_whitespace() || matches!(character, '-' | '_' | ':' | '/') {
                Some('-')
            } else {
                None
            }
        })
        .collect::<String>();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        return format!("memory-{index}");
    }
    slug.chars().take(48).collect()
}

/// Formats a memory scope for command output.
pub(super) fn runtime_remember_scope_display(scope: &MemoryScope) -> String {
    match scope {
        MemoryScope::Global => "global".to_string(),
        MemoryScope::Project { root } => format!("project:{root}"),
        MemoryScope::Session { session_id } => format!("session:{session_id}"),
        MemoryScope::Window {
            session_id,
            window_id,
        } => format!("window:{session_id}:{window_id}"),
        MemoryScope::Pane {
            session_id,
            pane_id,
        } => format!("pane:{session_id}:{pane_id}"),
        MemoryScope::Agent {
            session_id,
            agent_id,
        } => format!("agent:{session_id}:{agent_id}"),
    }
}

/// Runs the runtime git repository root operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_git_repository_root(working_directory: &PathBuf) -> Result<Option<PathBuf>> {
    let output = match runtime_git_output(working_directory, &["rev-parse", "--show-toplevel"]) {
        Ok(output) => output,
        Err(error) if error.kind() == crate::error::MezErrorKind::Io => return Ok(None),
        Err(error) => return Err(error),
    };
    if !output.status.success() {
        return Ok(None);
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(root)))
}

/// Runs the runtime git text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_git_text(repository_root: &PathBuf, args: &[&str]) -> Result<String> {
    let output = runtime_git_output(repository_root, args)?;
    if !output.status.success() {
        return Err(runtime_git_status_error(args, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Runs the runtime git untracked files operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_git_untracked_files(repository_root: &PathBuf) -> Result<Vec<String>> {
    let output = runtime_git_output(
        repository_root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    if !output.status.success() {
        return Err(runtime_git_status_error(
            &["ls-files", "--others", "--exclude-standard", "-z"],
            &output,
        ));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| String::from_utf8_lossy(bytes).to_string())
        .collect())
}

/// Runs the runtime git untracked diff operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_git_untracked_diff(repository_root: &PathBuf, file: &str) -> Result<String> {
    let file_path = repository_root.join(file);
    let output = Command::new("git")
        .args(["diff", "--no-index", "--no-ext-diff", "--no-color", "--"])
        .arg("/dev/null")
        .arg(&file_path)
        .current_dir(repository_root)
        .output()
        .map_err(|error| {
            MezError::new(
                crate::error::MezErrorKind::Io,
                format!("failed to run git diff for untracked file `{file}`: {error}"),
            )
        })?;
    match output.status.code() {
        Some(0 | 1) => Ok(String::from_utf8_lossy(&output.stdout).to_string()),
        _ => Err(runtime_git_status_error(
            &["diff", "--no-index", "--no-ext-diff", "--no-color"],
            &output,
        )),
    }
}

/// Runs the runtime git output operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_git_output(repository_root: &PathBuf, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(repository_root)
        .output()
        .map_err(|error| {
            MezError::new(
                crate::error::MezErrorKind::Io,
                format!("failed to run git {}: {error}", args.join(" ")),
            )
        })
}

/// Runs the runtime git status error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_git_status_error(args: &[&str], output: &std::process::Output) -> MezError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let detail = if stderr.is_empty() {
        "no stderr".to_string()
    } else {
        stderr
    };
    MezError::invalid_state(format!(
        "git {} exited with status {:?}: {}",
        args.join(" "),
        output.status.code(),
        detail
    ))
}
