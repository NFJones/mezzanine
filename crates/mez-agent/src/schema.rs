//! Provider MAAP tool and strict-schema construction.
//!
//! This module owns provider-neutral MAAP action schemas, MCP argument schema
//! normalization, and provider-facing schema descriptions shared by multiple
//! provider adapters.

use crate::{
    AllowedAction, AllowedActionSet, CONFIG_CHANGE_OPERATION_NAMES,
    CONFIG_CHANGE_SETTING_PATH_DESCRIPTION, CONFIG_CHANGE_VALUE_DESCRIPTION, McpPromptTool,
};

/// Legacy OpenAI MAAP function-tool surfaces.
///
/// Current OpenAI requests use the canonical `submit_maap_action_batch`
/// function. These names remain accepted while parsing older provider events
/// and persisted transcripts produced during rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiMaapToolSurface {
    /// Initial capability selection surface.
    CapabilityDecision,
    /// Response-only continuation surface.
    RespondOnly,
    /// Shell and patch execution surface.
    Shell,
    /// Network search execution surface.
    NetworkSearch,
    /// Network fetch execution surface.
    NetworkFetch,
    /// MCP call execution surface.
    Mcp,
    /// Local messaging and subagent execution surface.
    Subagent,
    /// Configuration mutation request surface.
    ConfigChange,
    /// Persistent memory search and storage surface.
    Memory,
    /// Local project issue tracking surface.
    Issues,
    /// Narrow fallback for uncommon composite capability grants.
    CurrentRequest,
}

impl OpenAiMaapToolSurface {
    /// Shared provider-local instruction that forbids prose responses when a
    /// MAAP function tool is available.
    const FUNCTION_CALL_DISCIPLINE: &str = "Return a function call, not prose.";
    /// Shared provider-local instruction that treats the function call as the
    /// current action envelope rather than a separate setup step.
    const ACTION_BATCH_ENVELOPE_RULE: &str = "The function call is only the transport envelope for the action batch, not a prerequisite task step; do not emit a say-only or progress batch claiming that an initial or schema-valid batch is needed before the executable action, and do not put required-function-call compliance language in rationale or thought fields. If an executable action is available and useful, put that action in this function call now.";
    /// Shared anti-pattern corrections for provider-local MAAP tool descriptions.
    const ANTI_EXAMPLES: &str = "Wrong: *** Replace File. Right: *** Update File with anchored hunks. Wrong: inferred apply_patch old context. Right: copy old/context lines verbatim from read file evidence.";

    /// Returns legacy surface names accepted while parsing provider output.
    pub fn stable_surfaces() -> &'static [Self] {
        &[
            Self::CapabilityDecision,
            Self::RespondOnly,
            Self::Shell,
            Self::NetworkSearch,
            Self::NetworkFetch,
            Self::Mcp,
            Self::Subagent,
            Self::ConfigChange,
            Self::Memory,
            Self::Issues,
        ]
    }

    /// Returns the function-tool name for this surface.
    pub fn tool_name(self) -> &'static str {
        match self {
            Self::CapabilityDecision => "submit_maap_capability_decision",
            Self::RespondOnly => "submit_maap_respond_only_actions",
            Self::Shell => "submit_maap_shell_actions",
            Self::NetworkSearch => "submit_maap_network_search_actions",
            Self::NetworkFetch => "submit_maap_network_fetch_actions",
            Self::Mcp => "submit_maap_mcp_actions",
            Self::Subagent => "submit_maap_subagent_actions",
            Self::ConfigChange => "submit_maap_config_change_actions",
            Self::Memory => "submit_maap_memory_actions",
            Self::Issues => "submit_maap_issues_actions",
            Self::CurrentRequest => "submit_maap_current_actions",
        }
    }
}

/// Returns the provider-facing description for the current MAAP action-batch tool.
pub fn maap_current_action_batch_description(
    _allowed_actions: &AllowedActionSet,
    _available_mcp_tools: &[McpPromptTool],
) -> String {
    maap_cache_stable_action_batch_description()
}

/// Returns the request-independent OpenAI Responses MAAP tool description.
pub fn maap_cache_stable_action_batch_description() -> String {
    maap_action_batch_description_with_mcp_manifest(
        "The schema includes fixed mcp_server_search and mcp_server_get actions plus a generic mcp_call action. Search configured MCP metadata, retrieve a selected server's safe metadata, then use mcp_call only when the current action surface and MCP context identify a callable server/tool pair; runtime validation rejects unavailable tools and invalid arguments.",
    )
}

/// Builds shared MAAP tool guidance with the selected MCP routing contract.
fn maap_action_batch_description_with_mcp_manifest(mcp_manifest: &str) -> String {
    format!(
        "Submit one validated Mezzanine MAAP action batch. {} {} The schema is a static catalog of every valid action; runtime configuration determines which catalog actions are enabled and runtime validation rejects disabled actions, unavailable integrations, or invalid arguments. Use only action objects in this function schema and use enabled actions directly without capability negotiation. The function call is only the transport envelope for the chosen action batch, not a prerequisite task step; do not put required-function-call or schema-wrapper compliance language in rationale or thought fields. Choose the smallest action that makes concrete progress: direct inspection or execution beats placeholder setup. If an executable action is useful, put that action in this function call now. Safely gather task-local facts from current context, action results, local artifacts, web results, MCP results, or another enabled action instead of asking the user. Do not ask for identifiers, URLs, versions, paths, command forms, config names, repository metadata, or CI targets when they can be safely discovered. Do not use memory actions to rehydrate facts already present in current action results. Model-selected skill lookup/loading and capability negotiation are not valid actions. {} {}",
        OpenAiMaapToolSurface::FUNCTION_CALL_DISCIPLINE,
        OpenAiMaapToolSurface::ACTION_BATCH_ENVELOPE_RULE,
        mcp_manifest,
        OpenAiMaapToolSurface::ANTI_EXAMPLES
    )
}

/// Runs the maap action batch schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn maap_action_batch_schema(
    _allowed_actions: &AllowedActionSet,
    _available_mcp_tools: &[McpPromptTool],
) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "rationale": {
                "type": "string",
                "minLength": 1,
                "description": "Terse additive reason these actions are next. Name why the selected action directly advances the user task. Do not say you are complying with a required function call, tool call, current-actions call, schema wrapper, or action wrapper. Do not restate the user request, prior rationale, progress say, or action summaries."
            },
            "thought": {
                "type": ["string", "null"],
                "description": "Optional longer durable work note for future context. Use only for substantive learning, decisions, invariants, or recovery details; otherwise null. Do not include secrets or private chain-of-thought."
            },
            "actions": {
                "type": "array",
                "minItems": 1,
                "description": "At least one visible or executable action from this function tool's currently active MAAP action surface.",
                "items": maap_action_schema()
            }
        },
        "required": ["rationale", "thought", "actions"],
        "additionalProperties": false
    })
}

/// Runs the maap action schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_action_schema() -> serde_json::Value {
    let mut action_schemas = Vec::new();
    for action in &AllowedActionSet::all_enabled().actions {
        match action {
            AllowedAction::Say => action_schemas.push(maap_say_action_schema()),
            AllowedAction::RequestCapability => {}
            AllowedAction::RequestSkills => {
                // Model-selected skill discovery is not part of the static
                // provider action surface.
            }
            AllowedAction::CallSkill => {}
            AllowedAction::ShellCommand => action_schemas.push(maap_shell_command_action_schema()),
            AllowedAction::ApplyPatch => action_schemas.push(maap_apply_patch_action_schema()),
            AllowedAction::WebSearch => action_schemas.push(maap_web_search_action_schema()),
            AllowedAction::FetchUrl => action_schemas.push(maap_fetch_url_action_schema()),
            AllowedAction::SendMessage => action_schemas.push(maap_send_message_action_schema()),
            AllowedAction::SpawnAgent => action_schemas.push(maap_spawn_agent_action_schema()),
            AllowedAction::ConfigChange => action_schemas.push(maap_config_change_action_schema(
                CONFIG_CHANGE_SETTING_PATH_DESCRIPTION,
            )),
            AllowedAction::MemorySearch => action_schemas.push(maap_memory_search_action_schema()),
            AllowedAction::MemoryStore => action_schemas.push(maap_memory_store_action_schema()),
            AllowedAction::IssueAdd => action_schemas.push(maap_issue_add_action_schema()),
            AllowedAction::IssueUpdate => action_schemas.push(maap_issue_update_action_schema()),
            AllowedAction::IssueQuery => action_schemas.push(maap_issue_query_action_schema()),
            AllowedAction::IssueDelete => action_schemas.push(maap_issue_delete_action_schema()),
            AllowedAction::McpServerSearch => {
                action_schemas.push(maap_mcp_server_search_action_schema())
            }
            AllowedAction::McpServerGet => action_schemas.push(maap_mcp_server_get_action_schema()),
            AllowedAction::McpCall => action_schemas.push(maap_generic_mcp_call_action_schema()),
        }
    }
    if action_schemas.is_empty() {
        action_schemas.push(maap_say_action_schema());
    }
    serde_json::json!({
        "anyOf": action_schemas
    })
}

/// Runs the maap common action properties operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_common_action_properties(action_type: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "type".to_string(),
        serde_json::json!({
            "type": "string",
            "enum": [action_type]
        }),
    );
    properties
}

/// Runs the maap action object schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_action_object_schema(
    action_type: &str,
    extra_properties: impl IntoIterator<Item = (&'static str, serde_json::Value)>,
    extra_required: &[&str],
) -> serde_json::Value {
    let mut properties = maap_common_action_properties(action_type);
    for (name, schema) in extra_properties {
        properties.insert(name.to_string(), schema);
    }

    let mut required = vec!["type"];
    required.extend(extra_required);

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

/// Returns a compact required string property schema with provider-facing usage
/// guidance for fields whose action semantics are otherwise easy to overuse.
fn described_string_property(
    name: &'static str,
    description: &'static str,
) -> (&'static str, serde_json::Value) {
    (
        name,
        serde_json::json!({
            "type": "string",
            "minLength": 1,
            "description": description
        }),
    )
}

/// Runs the maap say action schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_say_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "say",
        [
            (
                "status",
                serde_json::json!({
                    "type": "string",
                    "enum": ["progress", "final", "blocked"],
                    "description": "progress for a new sequence-point update, final when the user goal is complete, blocked when external input/state is required. Do not pair final or blocked with executable actions."
                }),
            ),
            (
                "content_type",
                serde_json::json!({
                    "type": "string",
                    "enum": ["text/plain; charset=utf-8", "text/markdown; charset=utf-8", "text/x-diff; charset=utf-8"],
                    "description": "HTTP-style media type for text. Use text/markdown; charset=utf-8 when the text uses Markdown presentation syntax, text/x-diff; charset=utf-8 when the text is a unified diff, otherwise use text/plain; charset=utf-8."
                }),
            ),
            (
                "text",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "description": "User-visible text. Display-only: commands and patch blocks here do not execute. Progress text must be a compact new learning, decision, phase change, validation outcome, or blocker delta; omit it if it repeats prior progress, rationale, summaries, thinking, or action results."
                }),
            ),
        ],
        &["status", "content_type", "text"],
    )
}

/// Runs the maap shell command action schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_shell_command_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "shell_command",
        [
            (
                "summary",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "description": "Concise user-facing progress summary to display before the command runs. Do not include the raw shell command; describe what will happen or what output will be used."
                }),
            ),
            (
                "command",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "description": "Exact bounded, noninteractive pane shell input for one logical inspection, command, build, test, format, validation, filesystem, or git action. Prefer one focused command; use separate shell_command actions for independent work. Do not run apply_patch as a shell program; use the apply_patch action. Heredocs and here-strings are disabled."
                }),
            ),
        ],
        &["summary", "command"],
    )
}

/// Runs the maap apply patch action schema operation for this subsystem.
fn maap_apply_patch_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "apply_patch",
        [described_string_property(
            "patch",
            "Direct Mezzanine patch text; no Markdown, heredoc, shell, or git apply input. Must start with *** Begin Patch and end with *** End Patch. Accepted file directives are exactly *** Add File, *** Update File, *** Delete File, plus optional *** Move to after *** Update File; there is no *** Replace File directive. For whole-file replacement, use an Update File hunk headed @@ replace whole file with only + added lines. Prefer relative safe paths. With active non-bypassed Bubblewrap, absolute paths are allowed only inside effective configured sandbox write scopes; otherwise paths must be relative. Forbid .. traversal. Prefer a distinctive @@ header and 5-10 exact current old/context lines copied verbatim from current file content or fresh action evidence; never infer or reconstruct likely code as old context. Usually one bounded owner read or matching result is enough. Reread only for uncovered hunks, stale/truncated evidence, or failed validation. Use multiple small hunks. Hunk lines use one prefix: space context, - removed, + added; *** End of File means no final newline. This is the only semantic file-content mutation action. After mismatch or ambiguity, reread only missing/stale owner ranges, skip already-applied changes, and retry with a smaller fresh anchored patch.",
        )],
        &["patch"],
    )
}

/// Runs the maap web search action schema operation for this subsystem.
fn maap_web_search_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "web_search",
        [described_string_property(
            "query",
            "Use only when the user asks for web search or current external information; not for local filesystem work or random/test/generated local content.",
        )],
        &["query"],
    )
}

/// Runs the maap fetch url action schema operation for this subsystem.
fn maap_fetch_url_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "fetch_url",
        [described_string_property(
            "url",
            "Use only for explicit http:// or https:// external URLs. For file://, local paths, or created outputs use shell_command; not for random/test/generated local data or replacing apply_patch/shell_command.",
        )],
        &["url"],
    )
}

/// Runs the maap issue add action schema operation for this subsystem.
fn maap_issue_add_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "issue_add",
        [
            (
                "kind",
                serde_json::json!({
                    "type": "string",
                    "enum": ["defect", "task"],
                    "description": "Issue kind to create: defect for bugs or task for planned work."
                }),
            ),
            (
                "state",
                serde_json::json!({
                    "type": ["string", "null"],
                    "enum": ["open", "in-progress", "resolved", null],
                    "description": "Optional initial issue state. Use null to create an open issue."
                }),
            ),
            (
                "priority",
                serde_json::json!({
                    "type": ["integer", "null"],
                    "minimum": 0,
                    "maximum": 100,
                    "description": "Optional issue priority from 0 to 100. Use null for the default priority."
                }),
            ),
            described_string_property("title", "Single-line issue title."),
            (
                "body",
                serde_json::json!({
                    "type": ["string", "null"],
                    "description": "Optional issue details. Use null when no body is needed."
                }),
            ),
            (
                "notes",
                serde_json::json!({
                    "type": ["string", "null"],
                    "description": "Optional mutable progress or handoff notes. Use null when no notes are needed."
                }),
            ),
            (
                "depends_on",
                serde_json::json!({
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Issue ids this new issue depends on. Use [] when there are no dependencies."
                }),
            ),
        ],
        &[
            "kind",
            "state",
            "priority",
            "title",
            "body",
            "notes",
            "depends_on",
        ],
    )
}

/// Runs the maap issue update action schema operation for this subsystem.
fn maap_issue_update_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "issue_update",
        [
            described_string_property("id", "Issue id to update in the current project."),
            (
                "kind",
                serde_json::json!({
                    "type": ["string", "null"],
                    "enum": ["defect", "task", null],
                    "description": "Optional replacement issue kind. Use null to leave unchanged."
                }),
            ),
            (
                "state",
                serde_json::json!({
                    "type": ["string", "null"],
                    "enum": ["open", "in-progress", "resolved", null],
                    "description": "Optional replacement issue state. Use null to leave unchanged."
                }),
            ),
            (
                "priority",
                serde_json::json!({
                    "type": ["integer", "null"],
                    "minimum": 0,
                    "maximum": 100,
                    "description": "Optional replacement priority from 0 to 100. Use null to leave unchanged."
                }),
            ),
            (
                "title",
                serde_json::json!({
                    "type": ["string", "null"],
                    "description": "Optional replacement single-line title. Use null to leave unchanged."
                }),
            ),
            (
                "body",
                serde_json::json!({
                    "type": ["string", "null"],
                    "description": "Optional replacement issue details. Use null to leave unchanged."
                }),
            ),
            (
                "clear_body",
                serde_json::json!({
                    "type": "boolean",
                    "description": "Whether to clear existing issue details. Cannot be true when body is set."
                }),
            ),
            (
                "notes",
                serde_json::json!({
                    "type": ["string", "null"],
                    "description": "Optional replacement progress or handoff notes. Use null to leave unchanged."
                }),
            ),
            (
                "clear_notes",
                serde_json::json!({
                    "type": "boolean",
                    "description": "Whether to clear existing progress or handoff notes. Cannot be true when notes is set."
                }),
            ),
            (
                "depends_on",
                serde_json::json!({
                    "type": ["array", "null"],
                    "items": {"type": "string"},
                    "description": "Optional replacement dependency issue ids. Use null to leave unchanged."
                }),
            ),
            (
                "clear_depends_on",
                serde_json::json!({
                    "type": "boolean",
                    "description": "Whether to clear existing dependency issue ids. Cannot be true when depends_on is set."
                }),
            ),
        ],
        &[
            "id",
            "kind",
            "state",
            "priority",
            "title",
            "body",
            "clear_body",
            "notes",
            "clear_notes",
            "depends_on",
            "clear_depends_on",
        ],
    )
}

/// Runs the maap issue query action schema operation for this subsystem.
fn maap_issue_query_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "issue_query",
        [
            (
                "kind",
                serde_json::json!({
                    "type": ["string", "null"],
                    "enum": ["defect", "task", null],
                    "description": "Optional issue kind filter. Use null for both defects and tasks."
                }),
            ),
            (
                "state",
                serde_json::json!({
                    "type": ["string", "null"],
                    "enum": ["open", "in-progress", "resolved", null],
                    "description": "Optional issue state filter. Use null for open issues by default."
                }),
            ),
            (
                "text",
                serde_json::json!({
                    "type": ["string", "null"],
                    "description": "Optional title/body substring filter. Use null for no text filter."
                }),
            ),
            (
                "limit",
                serde_json::json!({
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "maximum": 200,
                    "description": "Optional maximum issue records to return."
                }),
            ),
            (
                "refresh",
                serde_json::json!({
                    "type": "boolean",
                    "description": "Whether to bypass same-turn query freshness after concrete evidence that the issue store changed externally. Use false for ordinary discovery and continuations."
                }),
            ),
        ],
        &["kind", "state", "text", "limit", "refresh"],
    )
}

/// Runs the maap issue delete action schema operation for this subsystem.
fn maap_issue_delete_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "issue_delete",
        [described_string_property(
            "id",
            "Issue id to delete from the current project.",
        )],
        &["id"],
    )
}

/// Runs the maap memory search action schema operation for this subsystem.
fn maap_memory_search_action_schema() -> serde_json::Value {
    let mut schema = maap_action_object_schema(
        "memory_search",
        [
            described_string_property(
                "query",
                "Search durable prior context only when a specific missing prior-context question exists and current prompt, action results, MCP, shell, web, or another direct artifact cannot answer it. Do not use memory_search by default, as a startup ritual, or as a generic way to make progress. Treat it as optional support, not a required first step. Use at most one focused search in ordinary turns and never more than two memory_search actions in one user turn. Never search memory for facts already present in current action results, including identifiers, URLs, versions, paths, command forms, config names, repo owner/name, branch, commit, remotes, issue/PR numbers, or CI targets. Visible MCP schema and manifest metadata can be direct current-turn evidence for a callable integration, but it is not a reason to search memory first. If a direct path is unclear, use current action results, adjust or broaden a direct integration query, inspect a direct artifact with shell/web/MCP, or report a bounded blocker instead of searching memory. Do not use memory_search as placeholder setup before another direct action. If runtime skips or rejects a memory action, move on with direct artifacts, current action results, MCP, shell, web, or a bounded report instead of searching memory again unless a new specific prior-context gap appears. Lack of useful results is not a reason to paraphrase and search again.",
            ),
            (
                "limit",
                serde_json::json!({
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "maximum": 20,
                    "description": "Optional maximum records to return. Use small limits; omit for the runtime default."
                }),
            ),
        ],
        &["query", "limit"],
    );
    if let Some(description) = schema["properties"]["query"]["description"].as_str() {
        schema["properties"]["query"]["description"] = serde_json::Value::String(format!(
            "{description} A valid memory UUID query retrieves that record exactly when it is visible to the current runtime scopes and satisfies the requested active-state filter."
        ));
    }
    schema
}

/// Runs the maap memory store action schema operation for this subsystem.
fn maap_memory_store_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "memory_store",
        [
            (
                "kind",
                serde_json::json!({
                    "type": "string",
                    "enum": ["preference", "fact", "procedure", "documentation", "research", "warning"],
                    "description": "Durable memory kind. Use documentation for reusable reference material or docs content that should inform future tasks; use research for durable research findings that should inform future planning; use memory_store only for stable reusable information that is almost certain to help future sessions. Do not store prompt-specific, current-turn, action-result, tool-output, repo-state, issue-state, CI-state, plan, progress, MCP-output, episodic transcript, scratch, or other transient notes."
                }),
            ),
            (
                "priority",
                serde_json::json!({
                    "type": ["integer", "null"],
                    "minimum": 0,
                    "maximum": 100,
                    "description": "Optional retrieval priority from 0 to 100. Use high priority only when the memory is almost certain to be useful in future sessions; omit when unsure."
                }),
            ),
            (
                "scope",
                serde_json::json!({
                    "type": ["string", "null"],
                    "enum": ["global", "project", null],
                    "description": "Optional durable scope hint. Prefer project for repository-specific facts and global only for cross-project user preferences or stable facts."
                }),
            ),
            (
                "keywords",
                serde_json::json!({
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Search anchors or aliases to embed with the memory content for later retrieval. Use a short focused list."
                }),
            ),
            described_string_property(
                "content",
                "Durable memory body to store. Store only information that is stable, reusable beyond the current task, not already present in current context, not user-provided only for this task, almost certain to be useful in future sessions, and unlikely to be cheaply rediscovered. Do not store secrets, credentials, tokens, sensitive personal data, current-task-only summaries, plans, action results, tool outputs, transient terminal noise, no-op placeholders, current-actions markers, current checkout repo slugs, owner/repo, git remotes, branches, commits, paths, CI results, or MCP results unless the user explicitly instructed storing that exact content.",
            ),
            (
                "expires_in_days",
                serde_json::json!({
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Optional retention period in days. Omit to use memory.default_ttl_days."
                }),
            ),
        ],
        &[
            "kind",
            "priority",
            "scope",
            "keywords",
            "content",
            "expires_in_days",
        ],
    )
}

/// Runs the maap send message action schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_send_message_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "send_message",
        [
            (
                "recipient",
                serde_json::json!({
                    "type": "string"
                }),
            ),
            (
                "content_type",
                serde_json::json!({
                    "type": "string",
                    "enum": ["text/plain; charset=utf-8", "application/json"],
                    "description": "Use text/plain; charset=utf-8 for plain-text coordination messages and application/json for compact JSON-string payloads."
                }),
            ),
            (
                "payload",
                serde_json::json!({
                    "type": "string",
                    "description": "Model-readable payload, with JSON payloads encoded as a compact JSON string."
                }),
            ),
        ],
        &["recipient", "content_type", "payload"],
    )
}

/// Runs the maap spawn agent action schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_spawn_agent_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "spawn_agent",
        [
            (
                "role",
                serde_json::json!({
                    "type": "string",
                    "description": "Subagent role/profile. Use explorer for read-only search and inspection, worker for implementation, or a configured custom role."
                }),
            ),
            (
                "task_prompt",
                serde_json::json!({
                    "type": "string"
                }),
            ),
        ],
        &["role", "task_prompt"],
    )
}

/// Runs the maap config change action schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_config_change_action_schema(setting_path_description: &str) -> serde_json::Value {
    maap_action_object_schema(
        "config_change",
        [
            (
                "setting_path",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "description": setting_path_description
                }),
            ),
            (
                "operation",
                serde_json::json!({
                    "type": "string",
                    "enum": CONFIG_CHANGE_OPERATION_NAMES,
                    "description": "Configuration mutation operation. Use this action, not prose or config-file edits, for explicit requests such as changing the mez theme, approval mode, model, reasoning, or other supported settings. Config changes follow the active approval policy like other privileged actions. Once approved or policy-allowed, the runtime persists the change to the user config target and applies it immediately. A theme.active set uses set-theme behavior, including materialized theme aliases/colors. Use set to assign a scalar/string-array value, unset to remove a scalar override, and reset when the user's intent is to return a field to its lower-precedence or default value."
                }),
            ),
            (
                "value",
                serde_json::json!({
                    "type": ["string", "null"],
                    "description": CONFIG_CHANGE_VALUE_DESCRIPTION
                }),
            ),
        ],
        &["setting_path", "operation", "value"],
    )
}

/// Returns the request-independent MCP server discovery action schema.
pub fn maap_mcp_server_search_action_schema() -> serde_json::Value {
    let mut schema = maap_action_object_schema(
        "mcp_server_search",
        [
            (
                "query",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 512,
                    "description": "Search query matched against configured MCP server identity and purpose."
                }),
            ),
            (
                "limit",
                serde_json::json!({
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "maximum": 20,
                    "description": "Maximum matching servers to return in deterministic rank order."
                }),
            ),
        ],
        &["query", "limit"],
    );
    if let Some(object) = schema.as_object_mut() {
        object.insert(
            "description".to_string(),
            serde_json::json!("Search configured MCP server metadata without invoking an external tool. Retrieve a matching server with mcp_server_get before calling it."),
        );
    }
    schema
}

/// Returns the request-independent MCP server metadata retrieval action schema.
pub fn maap_mcp_server_get_action_schema() -> serde_json::Value {
    let mut schema = maap_action_object_schema(
        "mcp_server_get",
        [(
            "server",
            serde_json::json!({
                "type": "string",
                "minLength": 1,
                "description": "Configured MCP server id returned by mcp_server_search or durable MCP directory context."
            }),
        )],
        &["server"],
    );
    if let Some(object) = schema.as_object_mut() {
        object.insert(
            "description".to_string(),
            serde_json::json!("Retrieve safe metadata for one configured MCP server before deciding whether an external mcp_call is useful."),
        );
    }
    schema
}

/// Returns the request-independent MCP action variant used by OpenAI Responses.
///
/// Server and tool identity remain unconstrained in the provider schema so
/// injected catalogs cannot alter the cached tool bytes. The arguments field
/// carries compact JSON object text that canonical MAAP parsing normalizes
/// before the active MCP registry validates identity and tool-specific shape.
pub fn maap_generic_mcp_call_action_schema() -> serde_json::Value {
    let mut schema = maap_action_object_schema(
        "mcp_call",
        [
            (
                "server",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "description": "MCP server id from the active injected MCP context."
                }),
            ),
            (
                "tool",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "description": "MCP tool name from the active injected MCP context."
                }),
            ),
            (
                "arguments",
                serde_json::json!({
                    "type": "string",
                    "minLength": 2,
                    "description": "Compact JSON text encoding one object that conforms to the active injected MCP tool schema. Use {} when the tool takes no arguments."
                }),
            ),
        ],
        &["server", "tool", "arguments"],
    );
    if let Some(object) = schema.as_object_mut() {
        object.insert(
            "description".to_string(),
            serde_json::json!("Call an MCP tool listed in the active injected MCP context. Encode arguments as compact JSON object text; canonical parsing normalizes the object before runtime validation checks the active server, tool, and advertised input schema."),
        );
    }
    schema
}

/// Runs the normalize openai strict schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn normalize_openai_strict_schema(mut value: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(schema) = &mut value else {
        return value;
    };

    schema.remove("format");

    if let Some(serde_json::Value::Object(properties)) = schema.get_mut("properties") {
        let mut required = properties
            .keys()
            .cloned()
            .map(serde_json::Value::String)
            .collect::<Vec<_>>();
        required.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
        for property_schema in properties.values_mut() {
            *property_schema = normalize_openai_strict_schema(std::mem::take(property_schema));
        }
        schema
            .entry("type")
            .or_insert_with(|| serde_json::json!("object"));
        schema.insert("required".to_string(), serde_json::Value::Array(required));
        schema.insert(
            "additionalProperties".to_string(),
            serde_json::Value::Bool(false),
        );
    } else if schema
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| kind == "object")
    {
        schema.insert(
            "properties".to_string(),
            serde_json::Value::Object(serde_json::Map::new()),
        );
        schema.insert("required".to_string(), serde_json::Value::Array(Vec::new()));
        schema.insert(
            "additionalProperties".to_string(),
            serde_json::Value::Bool(false),
        );
    }

    if let Some(items) = schema.get_mut("items") {
        *items = normalize_openai_strict_schema(std::mem::take(items));
    }
    if let Some(serde_json::Value::Array(variants)) = schema.get_mut("anyOf") {
        for variant in variants {
            *variant = normalize_openai_strict_schema(std::mem::take(variant));
        }
    }

    value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies provider-neutral action-batch construction is byte-stable and
    /// exposes every executable action independently of request-local state.
    #[test]
    fn action_batch_schema_is_static_across_allowed_action_inputs() {
        let narrow = maap_action_batch_schema(
            &AllowedActionSet::from_actions([AllowedAction::Say, AllowedAction::ShellCommand]),
            &[],
        );
        let complete = maap_action_batch_schema(&AllowedActionSet::all_enabled(), &[]);
        assert_eq!(narrow, complete);

        let variants = narrow["properties"]["actions"]["items"]["anyOf"]
            .as_array()
            .expect("action variants should be an array");
        let action_types = variants
            .iter()
            .filter_map(|variant| variant["properties"]["type"]["enum"][0].as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            action_types,
            [
                "say",
                "shell_command",
                "apply_patch",
                "web_search",
                "fetch_url",
                "send_message",
                "spawn_agent",
                "config_change",
                "mcp_server_search",
                "mcp_server_get",
                "mcp_call",
                "memory_search",
                "memory_store",
                "issue_add",
                "issue_update",
                "issue_query",
                "issue_delete",
            ]
        );
    }

    /// Verifies third-party MCP input schemas are normalized into the OpenAI
    /// strict-schema subset before they are embedded in MAAP function tools.
    ///
    /// Some MCP servers advertise ordinary JSON Schema `format` annotations
    /// such as `uri`. The OpenAI validator rejects at least some of those
    /// values, so normalization must recurse through objects, arrays, and
    /// unions while preserving strict required-field expansion.
    #[test]
    fn normalize_openai_strict_schema_strips_nested_format_annotations() {
        let normalized = normalize_openai_strict_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "data": {
                    "type": "object",
                    "properties": {
                        "uri": {"type": "string", "format": "uri"}
                    }
                },
                "items": {
                    "type": "array",
                    "items": {"type": "string", "format": "uri-reference"}
                },
                "choice": {
                    "anyOf": [
                        {"type": "string", "format": "email"},
                        {"type": "null"}
                    ]
                }
            }
        }));

        assert_eq!(
            normalized.pointer("/properties/data/properties/uri/format"),
            None
        );
        assert_eq!(normalized.pointer("/properties/items/items/format"), None);
        assert_eq!(
            normalized.pointer("/properties/choice/anyOf/0/format"),
            None
        );
        assert_eq!(
            normalized.pointer("/properties/data/required"),
            Some(&serde_json::json!(["uri"]))
        );
        assert_eq!(
            normalized.pointer("/required"),
            Some(&serde_json::json!(["choice", "data", "items"]))
        );
        assert_eq!(
            normalized.pointer("/properties/data/additionalProperties"),
            Some(&serde_json::json!(false))
        );
    }
}
