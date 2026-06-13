//! Agent memory and git-diff helper routines for runtime commands.
//!
//! The parent command module owns live command execution. This child module
//! keeps `/remember` model-request shaping, model-output validation, memory
//! record construction, and git snapshot helpers together so durable-memory
//! command behavior is isolated from unrelated command families.

use super::compaction;
use super::*;
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
                issue_actions_enabled: true,
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

impl RuntimeSessionService {
    /// Executes `/memory` against the persisted persistent-memory enablement flag.
    pub(super) fn execute_agent_shell_memory_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("memory command must be a slash command"))?;
        let mode = runtime_single_mode_arg(&invocation.args, "memory", "status")?;
        let enabled_before = self.runtime_persistent_memory_enabled();
        if matches!(mode.as_str(), "status" | "show") {
            return Ok(AgentShellCommandOutcome::Display {
                command: "memory".to_string(),
                body: format!(
                    "pane={} enabled={} source=memory.enabled",
                    json_escape(pane_id),
                    enabled_before
                ),
            });
        }
        let enabled = match mode.as_str() {
            "on" => true,
            "off" => false,
            "toggle" => !enabled_before,
            _ => {
                return Err(MezError::invalid_args(
                    "memory slash command expects on, off, toggle, status, or no argument",
                ));
            }
        };
        let report = self.persist_memory_enabled_config(enabled)?;
        let visibility = self.agent_shell_visibility_for_pane(pane_id)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "memory".to_string(),
            body: format!(
                "pane={} enabled={} changed={} source=memory.enabled config_path={} deferred={}",
                json_escape(pane_id),
                enabled,
                enabled != enabled_before || report.changed,
                json_escape(&report.path.to_string_lossy()),
                report.deferred
            ),
            visibility,
        })
    }

    /// Executes `/remember` by queuing durable memory generation.
    pub(super) async fn execute_agent_shell_remember_command_async(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        self.queue_agent_shell_remember_command_with_model(pane_id, input)
    }

    /// Queues model-backed durable memory generation and marks the pane active.
    pub(super) fn queue_agent_shell_remember_command_with_model(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("remember command must be a slash command"))?;
        if !self.runtime_persistent_memory_enabled() {
            return Err(MezError::invalid_state(
                "remember requires persistent memory to be enabled; run /memory on first",
            ));
        }
        let visibility = self.agent_shell_visibility_for_pane(pane_id)?;
        let Some(_) = self.config_root.as_ref() else {
            return Err(MezError::invalid_state(
                "remember requires a configured Mezzanine config root",
            ));
        };
        if self.agent_remembering_panes.contains_key(pane_id) {
            return Err(MezError::conflict(format!(
                "cannot remember while pane {pane_id} is already memorizing"
            )));
        }
        if self.agent_compacting_panes.contains_key(pane_id) {
            return Err(MezError::conflict(format!(
                "cannot remember while pane {pane_id} is compacting"
            )));
        }
        let _ = self.runtime_prune_expired_persistent_memory_best_effort();
        let source_text = if invocation.args.trim().is_empty() {
            let context = self.agent_context_for_pane_prompt(
                pane_id,
                "[durable memory generation requested]",
                100,
            )?;
            let context = self.apply_agent_shell_preference_context(pane_id, context)?;
            runtime_remember_context_source(pane_id, &context)
        } else {
            format!(
                "Source statement supplied by the user:\n{}",
                invocation.args.trim()
            )
        };
        let agent_id = format!("agent-{pane_id}");
        let (model_profile_name, model_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let request = runtime_model_remember_request(
            &model_profile,
            pane_id,
            &self.session.id.to_string(),
            invocation.args.trim().is_empty(),
            &source_text,
            self.runtime_memory_default_ttl_days(),
        )?;
        self.agent_remembering_panes
            .insert(pane_id.to_string(), current_unix_seconds().max(1));
        self.pending_agent_remember_tasks.insert(
            pane_id.to_string(),
            RuntimeAgentRememberTask {
                pane_id: pane_id.to_string(),
                model_profile_name: model_profile_name.clone(),
                model_profile: model_profile.clone(),
                scope: self.runtime_remember_scope_for_pane(pane_id),
                request,
            },
        );
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!(
                "agent: memorizing durable context provider={} model={}",
                model_profile.provider, model_profile.model
            ),
        )?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "remember".to_string(),
            body: format!(
                "pane={} memorizing=true state=queued source=agent-remember model_profile={} provider={} model={}",
                json_escape(pane_id),
                json_escape(&model_profile_name),
                json_escape(&model_profile.provider),
                json_escape(&model_profile.model),
            ),
            visibility,
        })
    }

    /// Applies one completed `/remember` model response through actor-owned state.
    pub fn apply_agent_remember_completed_event(
        &mut self,
        pane_id: &str,
        response: ModelResponse,
    ) -> Result<bool> {
        let Some(task) = self.claimed_agent_remember_tasks.remove(pane_id) else {
            self.agent_remembering_panes.remove(pane_id);
            return Ok(false);
        };
        self.agent_remembering_panes.remove(pane_id);
        self.record_agent_provider_token_usage_with_profile(
            pane_id,
            response.usage,
            response.usage,
            Some(&task.model_profile),
        );
        self.record_agent_provider_quota_usage(pane_id, &response.quota_usage);
        let candidates = runtime_remember_candidates_from_response(&response)?;
        let Some(config_root) = self.config_root.clone() else {
            return Err(MezError::invalid_state(
                "remember requires a configured Mezzanine config root",
            ));
        };
        let store = crate::memory::PersistentMemoryStore::under_config_root(&config_root);
        let now = current_unix_seconds().max(1);
        let mut stored = Vec::new();
        for (index, candidate) in candidates.into_iter().take(6).enumerate() {
            let id = format!(
                "remember-{}-{}",
                now,
                runtime_remember_slug(&candidate.summary, index + 1)
            );
            let record = candidate.into_record(
                id,
                task.scope.clone(),
                now,
                self.runtime_memory_default_ttl_days(),
            )?;
            self.upsert_session_memory(record.clone())?;
            store.upsert(record.clone())?;
            stored.push(record);
        }
        if stored.is_empty() {
            return Err(MezError::invalid_state(
                "remember model response did not contain any acceptable memories",
            ));
        }
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!(
                "agent: memorized durable context stored={} scope={} source=agent-remember model_profile={} provider={} model={} ids={}",
                stored.len(),
                runtime_remember_scope_display(&task.scope),
                task.model_profile_name,
                task.model_profile.provider,
                task.model_profile.model,
                runtime_string_array_json(
                    &stored
                        .iter()
                        .map(|record| record.id.clone())
                        .collect::<Vec<_>>()
                )
            ),
        )?;
        Ok(true)
    }

    /// Applies a failed model-backed `/remember` worker result.
    pub fn apply_agent_remember_failed_event(
        &mut self,
        pane_id: &str,
        message: &str,
    ) -> Result<bool> {
        let had_task = self.pending_agent_remember_tasks.remove(pane_id).is_some()
            || self.claimed_agent_remember_tasks.remove(pane_id).is_some()
            || self.agent_remembering_panes.remove(pane_id).is_some();
        if had_task {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!("agent: remember failed during provider request: {message}"),
            )?;
        }
        Ok(had_task)
    }

    /// Returns pane ids with queued model-backed durable memory tasks.
    pub fn pending_agent_remember_tasks(&self) -> Vec<String> {
        self.pending_agent_remember_tasks.keys().cloned().collect()
    }

    /// Claims one queued durable memory task for execution outside the actor.
    pub fn claim_agent_remember_task(
        &mut self,
        pane_id: &str,
    ) -> Result<Option<RuntimeAgentRememberDispatch>> {
        let Some(task) = self.pending_agent_remember_tasks.remove(pane_id) else {
            return Ok(None);
        };
        if !self.agent_remembering_panes.contains_key(pane_id) {
            return Ok(None);
        }
        let provider =
            self.runtime_model_provider_for_profile(&task.model_profile, "provider_remember")?;
        self.claimed_agent_remember_tasks
            .insert(pane_id.to_string(), task.clone());
        Ok(Some(RuntimeAgentRememberDispatch { task, provider }))
    }

    /// Builds the durable memory scope used for `/remember` records.
    fn runtime_remember_scope_for_pane(&self, pane_id: &str) -> MemoryScope {
        self.pane_current_working_directory(pane_id)
            .map(|path| MemoryScope::Project {
                root: discover_project_root(&path).to_string_lossy().into_owned(),
            })
            .unwrap_or(MemoryScope::Global)
    }

    /// Builds an async provider dispatch suitable for one runtime-owned model command.
    pub(super) fn runtime_model_provider_for_profile(
        &mut self,
        model_profile: &ModelProfile,
        audit_operation: &str,
    ) -> Result<RuntimeAgentProviderDispatchProvider> {
        let provider_config = self
            .provider_registry
            .provider(&model_profile.provider)
            .cloned()
            .ok_or_else(|| {
                MezError::config(format!(
                    "provider `{}` for active model profile is not configured",
                    model_profile.provider
                ))
            })?;
        let api = effective_provider_api(&provider_config.kind, provider_config.api.as_deref())?;
        self.append_credential_access_audit(
            &model_profile.provider,
            &provider_config.auth_profile,
            audit_operation,
            "requested",
        )?;
        let Some(auth_store) = self.auth_store.as_ref() else {
            self.append_credential_access_audit(
                &model_profile.provider,
                &provider_config.auth_profile,
                audit_operation,
                "denied",
            )?;
            return Err(MezError::invalid_state(
                "remember requires an attached provider auth store",
            ));
        };
        let endpoint_override = provider_config
            .base_url
            .as_deref()
            .filter(|endpoint| !endpoint.is_empty());
        let provider_result: Result<RuntimeAgentProviderDispatchProvider> = match api {
            ProviderApiCompatibility::OpenAiResponses => {
                openai_responses_provider_from_auth_store_with_provider_options(
                    auth_store,
                    &model_profile.provider,
                    endpoint_override,
                    &provider_config.options,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(RuntimeAgentProviderDispatchProvider::OpenAi)
            }
            ProviderApiCompatibility::OpenAiChatCompletions => {
                openai_compatible_provider_from_auth_store_with_provider_options(
                    auth_store,
                    &model_profile.provider,
                    endpoint_override,
                    &provider_config.options,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(RuntimeAgentProviderDispatchProvider::OpenAiCompatible)
            }
            ProviderApiCompatibility::DeepSeekChatCompletions => {
                deepseek_chat_completions_provider_from_auth_store_with_provider_options(
                    auth_store,
                    &model_profile.provider,
                    endpoint_override,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(RuntimeAgentProviderDispatchProvider::DeepSeek)
            }
        };
        match provider_result {
            Ok(provider) => {
                self.append_credential_access_audit(
                    &model_profile.provider,
                    &provider_config.auth_profile,
                    audit_operation,
                    "granted",
                )?;
                Ok(provider)
            }
            Err(error) => {
                self.append_credential_access_audit(
                    &model_profile.provider,
                    &provider_config.auth_profile,
                    audit_operation,
                    "denied",
                )?;
                Err(error)
            }
        }
    }

    /// Returns whether persistent memory is enabled in the live effective config.
    pub(in crate::runtime) fn runtime_persistent_memory_enabled(&self) -> bool {
        runtime_effective_config_value(&self.config_layers)
            .ok()
            .and_then(|root| {
                root.get("memory")
                    .and_then(|memory| memory.get("enabled"))
                    .and_then(serde_json::Value::as_bool)
            })
            .unwrap_or(true)
    }

    /// Returns the configured default memory TTL in days.
    pub(in crate::runtime) fn runtime_memory_default_ttl_days(&self) -> u64 {
        runtime_effective_config_value(&self.config_layers)
            .ok()
            .and_then(|root| {
                root.get("memory")
                    .and_then(|memory| memory.get("default_ttl_days"))
                    .and_then(serde_json::Value::as_u64)
            })
            .filter(|days| *days > 0)
            .unwrap_or(180)
    }

    /// Best-effort prunes expired persistent memory records from disk and session state.
    pub(in crate::runtime) fn runtime_prune_expired_persistent_memory_best_effort(
        &mut self,
    ) -> usize {
        if !self.runtime_persistent_memory_enabled() {
            return 0;
        }
        let Some(config_root) = self.config_root.clone() else {
            return 0;
        };
        let store = crate::memory::PersistentMemoryStore::under_config_root(&config_root);
        let now = current_unix_seconds().max(1);
        let Ok(pruned) = store.prune_expired(now, false) else {
            return 0;
        };
        for record in &pruned {
            let _ = self.session_memory_mut().delete(&record.id);
        }
        pruned.len()
    }

    /// Persists and applies the configured persistent-memory enablement flag.
    fn persist_memory_enabled_config(
        &mut self,
        enabled: bool,
    ) -> Result<crate::runtime::commands_support::RuntimePersistedConfigMutationBatchReport> {
        let path = self.runtime_primary_config_path_for_agent_command()?;
        runtime_apply_persisted_config_mutation_batch(
            self,
            path,
            &[ConfigMutation {
                path: "memory.enabled".to_string(),
                operation: ConfigMutationOperation::Set(ConfigMutationValue::Boolean(enabled)),
            }],
            "agent/slash:memory",
        )
    }

    /// Finds or creates the private primary config file for slash-command persistence.
    fn runtime_primary_config_path_for_agent_command(&mut self) -> Result<PathBuf> {
        if let Some(path) = self
            .config_layers
            .iter()
            .find(|layer| layer.scope == ConfigScope::Primary && layer.path.is_some())
            .and_then(|layer| layer.path.clone())
        {
            return Ok(path);
        }
        let root = self.config_root.clone().ok_or_else(|| {
            MezError::config(
                "memory slash command requires a configured config root or primary config file",
            )
        })?;
        let path = ConfigPaths::from_root(root).ensure_default_config()?;
        let format = ConfigFormat::from_path(&path)?;
        let text = fs::read_to_string(&path)?;
        self.config_layers.push(crate::config::ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format,
            scope: ConfigScope::Primary,
            trusted: true,
            text,
        });
        Ok(path)
    }
}
