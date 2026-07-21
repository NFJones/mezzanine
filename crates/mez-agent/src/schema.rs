//! Provider MAAP tool and strict-schema construction.
//!
//! This module owns provider-neutral MAAP action schemas, MCP argument schema
//! normalization, and provider-facing schema descriptions shared by multiple
//! provider adapters.

use crate::{
    AgentCapability, AllowedAction, AllowedActionSet, CONFIG_CHANGE_OPERATION_NAMES,
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
    /// Shared capability map for provider-local MAAP tool descriptions.
    const CAPABILITY_MAP: &str = "Capability map: shell=local files, rg/sed/cat, git, builds, tests, shell_command, and apply_patch; network_search=web_search; network_fetch=fetch_url; mcp=mcp_call; subagent=send_message or spawn_agent; config_change=config_change; memory=memory_search or memory_store; issues=issue_add, issue_update, issue_query, or issue_delete; respond_only=final text only.";
    /// Shared anti-pattern corrections for provider-local MAAP tool descriptions.
    const ANTI_EXAMPLES: &str = "Wrong: say(blocked, \"Need shell capability\"). Right: request_capability(capability=\"shell\", reason=\"Need to inspect repository files\"). Wrong: *** Replace File. Right: *** Update File with anchored hunks. Wrong: inferred apply_patch old context. Right: copy old/context lines verbatim from read file evidence.";

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
    allowed_actions: &AllowedActionSet,
    available_mcp_tools: &[McpPromptTool],
) -> String {
    let mcp_manifest = if allowed_actions.contains(AllowedAction::McpCall) {
        mcp_tool_manifest_for_description(available_mcp_tools)
    } else {
        "No mcp_call action is active on this request surface.".to_string()
    };
    maap_action_batch_description_with_mcp_manifest(&mcp_manifest)
}

/// Returns the request-independent OpenAI Responses MAAP tool description.
pub fn maap_cache_stable_action_batch_description() -> String {
    maap_action_batch_description_with_mcp_manifest(
        "The schema includes a generic mcp_call action. The OpenAI request-state suffix identifies the currently allowed actions, and injected MCP context identifies callable server/tool pairs when MCP is active; runtime validation rejects unavailable tools and invalid arguments.",
    )
}

/// Builds shared MAAP tool guidance with the selected MCP routing contract.
fn maap_action_batch_description_with_mcp_manifest(mcp_manifest: &str) -> String {
    format!(
        "Submit one validated Mezzanine MAAP action batch for the currently allowed actions. {} {} Use only the action objects in this function schema. The function call is only the transport envelope for the chosen action batch, not a prerequisite task step; do not put required-function-call, current-actions-call, or schema-wrapper compliance language in rationale or thought fields. Choose the smallest action that makes concrete progress: direct inspection or execution beats placeholder setup. If an executable action is available and useful, put that action in this function call now. If the needed action family is absent and request_capability is available, emit request_capability for that capability instead of say(blocked), final text, or prose asking for access. If missing information, parameters, or identifiers can be safely gathered from current context, action results, local artifacts, web results, MCP results, or another available or requestable action, request or use the relevant capability instead of asking the user. Do not ask for task-local facts such as identifiers, URLs, versions, paths, command forms, config names, repo owner/name, branch, commit, remote URL, issue/PR numbers, or CI targets when they can be safely discovered. Do not use memory_search or memory_store to rehydrate, preserve, or look up facts already present in current action results. Model-selected skill lookup/loading is disabled; request_skills and call_skill are never valid actions. {} {} {}",
        OpenAiMaapToolSurface::FUNCTION_CALL_DISCIPLINE,
        OpenAiMaapToolSurface::ACTION_BATCH_ENVELOPE_RULE,
        mcp_manifest,
        OpenAiMaapToolSurface::CAPABILITY_MAP,
        OpenAiMaapToolSurface::ANTI_EXAMPLES
    )
}

/// Returns a compact model-facing manifest for MCP tools in one provider schema.
pub fn mcp_tool_manifest_for_description(tools: &[McpPromptTool]) -> String {
    const MAX_TOOL_DESCRIPTION_COUNT: usize = 20;
    const MAX_SERVER_DESCRIPTION_COUNT: usize = 20;
    if tools.is_empty() {
        return "No MCP tools are currently callable in this schema.".to_string();
    }
    let sorted_tools = sorted_mcp_prompt_tools(tools);
    if sorted_tools.is_empty() {
        return "No MCP tools are currently callable in this schema.".to_string();
    }
    let total_servers = mcp_server_manifest_entries(&sorted_tools).len();
    let mut server_entries = mcp_server_manifest_entries(&sorted_tools)
        .into_iter()
        .take(MAX_SERVER_DESCRIPTION_COUNT)
        .collect::<Vec<_>>();
    if total_servers > MAX_SERVER_DESCRIPTION_COUNT {
        server_entries.push(format!(
            "... plus {} more MCP servers listed in the schema",
            total_servers - MAX_SERVER_DESCRIPTION_COUNT
        ));
    }
    let total = sorted_tools.len();
    let mut entries = sorted_tools
        .into_iter()
        .take(MAX_TOOL_DESCRIPTION_COUNT)
        .map(|tool| {
            format!(
                "{}/{}: {}",
                tool.server_id,
                tool.tool_name,
                mcp_schema_description(&tool.description)
            )
        })
        .collect::<Vec<_>>();
    if total > MAX_TOOL_DESCRIPTION_COUNT {
        entries.push(format!(
            "... plus {} more MCP tools listed in the schema",
            total - MAX_TOOL_DESCRIPTION_COUNT
        ));
    }
    format!(
        "Available MCP servers callable with mcp_call: {}. Available MCP tools callable with mcp_call: {}.",
        server_entries.join("; "),
        entries.join("; ")
    )
}

/// Returns compact server-level routing entries synthesized from MCP tools.
fn mcp_server_manifest_entries(tools: &[&McpPromptTool]) -> Vec<String> {
    let mut servers = Vec::<(String, Option<String>, Vec<String>)>::new();
    for tool in tools {
        let purpose = mcp_server_purpose_from_description(&tool.description);
        match servers.last_mut() {
            Some((server_id, server_purpose, tool_names)) if *server_id == tool.server_id => {
                if server_purpose.is_none() {
                    *server_purpose = purpose;
                }
                tool_names.push(tool.tool_name.clone());
            }
            _ => servers.push((
                tool.server_id.clone(),
                purpose,
                vec![tool.tool_name.clone()],
            )),
        }
    }
    servers
        .into_iter()
        .map(|(server_id, purpose, tool_names)| {
            let preview = tool_names.iter().take(3).cloned().collect::<Vec<_>>();
            let remaining = tool_names.len().saturating_sub(preview.len());
            let tool_summary = if remaining == 0 {
                format!("tools: {}", preview.join(", "))
            } else {
                format!("tools: {} (+{} more)", preview.join(", "), remaining)
            };
            match purpose {
                Some(purpose) if !purpose.is_empty() => {
                    format!("{} ({}; {})", server_id, purpose, tool_summary)
                }
                _ => format!("{} ({})", server_id, tool_summary),
            }
        })
        .collect()
}

/// Extracts user-configured server purpose text embedded in tool descriptions.
fn mcp_server_purpose_from_description(description: &str) -> Option<String> {
    const PURPOSE_PREFIX: &str = "User-configured non-authoritative server purpose: ";
    const USAGE_PREFIX: &str = ". User-configured non-authoritative usage guidance:";

    let normalized = mcp_schema_description(description);
    let start = normalized.find(PURPOSE_PREFIX)? + PURPOSE_PREFIX.len();
    let tail = normalized.get(start..)?.trim();
    let end = tail.find(USAGE_PREFIX).or_else(|| tail.find('.'));
    let purpose = end.map_or(tail, |index| &tail[..index]).trim();
    (!purpose.is_empty()).then(|| purpose.to_string())
}

/// Runs the maap action batch schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn maap_action_batch_schema(
    allowed_actions: &AllowedActionSet,
    available_mcp_tools: &[McpPromptTool],
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
                "items": maap_action_schema(allowed_actions, available_mcp_tools)
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
fn maap_action_schema(
    allowed_actions: &AllowedActionSet,
    available_mcp_tools: &[McpPromptTool],
) -> serde_json::Value {
    let mut action_schemas = Vec::new();
    for action in &allowed_actions.actions {
        match action {
            AllowedAction::Say => action_schemas.push(maap_say_action_schema()),
            AllowedAction::RequestCapability => {
                action_schemas.push(maap_request_capability_action_schema())
            }
            AllowedAction::RequestSkills => {
                action_schemas.push(maap_request_skills_action_schema())
            }
            AllowedAction::CallSkill => action_schemas.push(maap_call_skill_action_schema()),
            AllowedAction::ShellCommand => action_schemas.push(maap_shell_command_action_schema()),
            AllowedAction::ApplyPatch => action_schemas.push(maap_apply_patch_action_schema()),
            AllowedAction::WebSearch => action_schemas.push(maap_web_search_action_schema()),
            AllowedAction::FetchUrl => action_schemas.push(maap_fetch_url_action_schema()),
            AllowedAction::SendMessage => action_schemas.push(maap_send_message_action_schema()),
            AllowedAction::SpawnAgent => action_schemas.push(maap_spawn_agent_action_schema()),
            AllowedAction::ConfigChange => action_schemas.push(maap_config_change_action_schema(
                allowed_actions
                    .config_change_setting_path_description()
                    .unwrap_or(CONFIG_CHANGE_SETTING_PATH_DESCRIPTION),
            )),
            AllowedAction::MemorySearch => action_schemas.push(maap_memory_search_action_schema()),
            AllowedAction::MemoryStore => action_schemas.push(maap_memory_store_action_schema()),
            AllowedAction::IssueAdd => action_schemas.push(maap_issue_add_action_schema()),
            AllowedAction::IssueUpdate => action_schemas.push(maap_issue_update_action_schema()),
            AllowedAction::IssueQuery => action_schemas.push(maap_issue_query_action_schema()),
            AllowedAction::IssueDelete => action_schemas.push(maap_issue_delete_action_schema()),
            AllowedAction::McpCall => action_schemas.extend(
                sorted_mcp_prompt_tools(available_mcp_tools)
                    .into_iter()
                    .filter_map(maap_mcp_call_action_schema_for_tool),
            ),
        }
    }
    if action_schemas.is_empty() {
        action_schemas.push(maap_say_action_schema());
    }
    serde_json::json!({
        "anyOf": action_schemas
    })
}

/// Returns MCP prompt tools in deterministic provider-visible order.
fn sorted_mcp_prompt_tools(tools: &[McpPromptTool]) -> Vec<&McpPromptTool> {
    let mut tools = tools
        .iter()
        .filter(|tool| !tool.server_id.is_empty() && !tool.tool_name.is_empty())
        .collect::<Vec<_>>();
    tools.sort_by(|left, right| {
        left.server_id
            .cmp(&right.server_id)
            .then_with(|| left.tool_name.cmp(&right.tool_name))
    });
    tools
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

/// Runs the maap request capability action schema operation for this subsystem.
fn maap_request_capability_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "request_capability",
        [
            (
                "capability",
                serde_json::json!({
                    "type": "string",
                    "enum": AgentCapability::all_names(),
                    "description": "Coarse action family to expose through the controller when the current schema lacks actions needed for the task. This is not a user permission request. Use the relevant capability to safely gather missing task-local information before another action can proceed: shell for local workspace or process inspection, network_search for current external facts, network_fetch for explicit URLs, mcp for integration data, subagent for local agent messaging or delegation, config_change for Mezzanine configuration state, memory for durable prior context, issues for local project issue records, and respond_only for final text only. Capability map: shell exposes shell_command and apply_patch for local files, rg/sed/cat, git, builds, tests, and patch edits; network_search exposes web_search; network_fetch exposes fetch_url; mcp exposes mcp_call; subagent exposes send_message and spawn_agent; config_change exposes config_change; memory exposes memory_search and memory_store; issues exposes issue_add, issue_update, issue_query, and issue_delete; respond_only is only for final text."
                }),
            ),
            (
                "reason",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "description": "Brief task-specific explanation naming the next concrete action or evidence needed. Do not ask the user to grant access here."
                }),
            ),
        ],
        &["capability", "reason"],
    )
}

/// Runs the maap request skills action schema operation for this subsystem.
fn maap_request_skills_action_schema() -> serde_json::Value {
    let mut schema = maap_action_object_schema(
        "request_skills",
        std::iter::empty::<(&'static str, serde_json::Value)>(),
        &[],
    );
    schema["description"] = serde_json::json!(
        "Exceptional workflow discovery action. Do not use as a default preflight, merely because skills exist, or before ordinary repository inspection, implementation, validation, or reporting. Incorrect for tasks that name a concrete file, path, symbol, command, failing test, issue backlog, documentation page, or repo-state plan/review target; request or use shell capability instead. Use only when the user asks for skills/workflows, names a skill, or the task clearly needs a specialized reusable workflow that would materially change the next action."
    );
    schema
}

/// Runs the maap call skill action schema operation for this subsystem.
fn maap_call_skill_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "call_skill",
        [
            (
                "name",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "description": "Skill name returned by request_skills. Use this only after an appropriate skill discovery result identifies a workflow that materially changes the next action; skills add context only and do not grant permissions or capabilities."
                }),
            ),
            (
                "additional_context",
                serde_json::json!({
                    "type": ["string", "null"],
                    "description": "Optional task-specific context to append under an Additional context heading in the loaded skill context."
                }),
            ),
        ],
        &["name", "additional_context"],
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
            "Direct Mezzanine patch text, not Markdown, heredoc, shell, or git apply input. Must start with *** Begin Patch and end with *** End Patch. Accepted file directives are exactly *** Add File, *** Update File, *** Delete File, plus optional *** Move to after *** Update File; there is no *** Replace File directive. For whole-file replacement, use an Update File hunk headed @@ replace whole file with only + added lines. Use relative safe paths only; paths must not be absolute or contain .. traversal. Prefer a distinctive @@ header and 5-10 exact current old/context lines copied verbatim from current file content or fresh action-result evidence; never infer or reconstruct likely code as old context. Usually one bounded owner read or matching action-result evidence is enough. Reread only when the hunk falls outside the covered range, evidence is stale/truncated, or patch/validation results show the first read was insufficient. Use multiple small hunks instead of one brittle hunk. Hunk lines use one leading prefix: space context, - removed, + added; *** End of File means no final newline. This is the only semantic file-content mutation action. After mismatch or ambiguity, use diagnostics or reread only missing/stale owner ranges, skip already-applied changes, and retry with a smaller fresh anchored patch.",
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
        &["kind", "title", "body", "notes", "depends_on"],
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
                    "enum": ["open", "resolved", null],
                    "description": "Optional replacement issue state. Use null to leave unchanged."
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
                    "enum": ["open", "resolved", null],
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
    maap_action_object_schema(
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
    )
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

/// Runs the maap mcp call action schema for tool operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn maap_mcp_call_action_schema_for_tool(tool: &McpPromptTool) -> Option<serde_json::Value> {
    let mut schema = maap_action_object_schema(
        "mcp_call",
        [
            (
                "server",
                serde_json::json!({
                    "type": "string",
                    "enum": [tool.server_id],
                    "description": format!("MCP server id exposed for this tool: {}", tool.server_id)
                }),
            ),
            (
                "tool",
                serde_json::json!({
                    "type": "string",
                    "enum": [tool.tool_name],
                    "description": format!("MCP tool exposed by server {}: {}. Tool description: {}", tool.server_id, tool.tool_name, mcp_schema_description(&tool.description))
                }),
            ),
            (
                "arguments",
                mcp_tool_arguments_schema_with_description(tool)?,
            ),
        ],
        &["server", "tool", "arguments"],
    );
    if let Some(object) = schema.as_object_mut() {
        object.insert(
            "description".to_string(),
            serde_json::json!(format!(
                "Call MCP tool {}/{}. Description: {}. If the user named this MCP server or the task matches this tool description, use this as a direct action when it is the smallest action that makes concrete progress; do not use shell, capability requests, memory_search, or memory_store merely as setup before it. If required arguments such as identifiers, URLs, paths, repo owner/name, branch, commit, issue/PR number, or CI target are already present in current prompt or action results, use them directly; if they must be safely derived from local, web, or integration context and the needed capability is absent, request that capability instead of asking the user.",
                tool.server_id,
                tool.tool_name,
                mcp_schema_description(&tool.description)
            )),
        );
    }
    Some(schema)
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

/// Returns a compact provider-facing description for MCP schema metadata.
fn mcp_schema_description(description: &str) -> String {
    let normalized = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        "No tool description was provided by the MCP server.".to_string()
    } else {
        normalized
    }
}

/// Returns the MCP tool arguments schema with tool-specific guidance attached.
fn mcp_tool_arguments_schema_with_description(tool: &McpPromptTool) -> Option<serde_json::Value> {
    let mut schema = mcp_tool_arguments_schema(tool)?;
    if let Some(object) = schema.as_object_mut() {
        object.insert(
            "description".to_string(),
            serde_json::json!(format!(
                "Arguments for MCP tool {}/{}. Use this action when the task matches this tool description or the user named this MCP server/tool. Fill task-local arguments from current prompt, action results, or safely gatherable context when available instead of searching memory or asking the user: {}",
                tool.server_id,
                tool.tool_name,
                mcp_schema_description(&tool.description)
            )),
        );
    }
    Some(schema)
}

/// Runs the mcp tool arguments schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mcp_tool_arguments_schema(tool: &McpPromptTool) -> Option<serde_json::Value> {
    crate::mcp::validate_mcp_tool_input_schema(&tool.input_schema_json)
        .ok()
        .map(normalize_openai_strict_schema)
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

    /// Verifies provider-neutral action-batch construction follows the active
    /// lower-crate action surface rather than silently exposing product actions.
    #[test]
    fn action_batch_schema_tracks_allowed_actions() {
        let schema = maap_action_batch_schema(
            &AllowedActionSet::from_actions([AllowedAction::Say, AllowedAction::ShellCommand]),
            &[],
        );
        let variants = schema["properties"]["actions"]["items"]["anyOf"]
            .as_array()
            .expect("action variants should be an array");
        let action_types = variants
            .iter()
            .filter_map(|variant| variant["properties"]["type"]["enum"][0].as_str())
            .collect::<Vec<_>>();

        assert_eq!(action_types, ["say", "shell_command"]);
    }

    /// Verifies MCP argument schemas are normalized to the strict provider
    /// object shape while unsupported format annotations are removed.
    #[test]
    fn mcp_argument_schema_is_normalized_for_strict_providers() {
        let tool = McpPromptTool {
            server_id: "example".to_string(),
            tool_name: "lookup".to_string(),
            description: "Look up one resource".to_string(),
            approval_required: false,
            input_schema_json: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "format": "uri"}
                }
            })
            .to_string(),
        };

        let schema = maap_mcp_call_action_schema_for_tool(&tool).unwrap();
        let arguments = &schema["properties"]["arguments"];
        assert_eq!(arguments["required"], serde_json::json!(["url"]));
        assert_eq!(arguments["additionalProperties"], false);
        assert!(arguments["properties"]["url"].get("format").is_none());
    }

    /// Verifies malformed and non-object MCP schemas never produce callable
    /// action variants with synthesized empty arguments.
    #[test]
    fn invalid_mcp_argument_schemas_are_omitted_from_action_construction() {
        for input_schema_json in ["{", "[]", r#"{"type":"string"}"#] {
            let tool = McpPromptTool {
                server_id: "example".to_string(),
                tool_name: "broken".to_string(),
                description: "Broken schema fixture".to_string(),
                approval_required: false,
                input_schema_json: input_schema_json.to_string(),
            };

            assert!(maap_mcp_call_action_schema_for_tool(&tool).is_none());
        }
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

    /// Verifies an MCP tool schema containing `format: uri` can be embedded in
    /// the OpenAI MCP action schema without leaking the rejected annotation.
    ///
    /// This protects the provider request path where a configured MCP server
    /// advertises a nested `arguments.data.uri` field.
    #[test]
    fn openai_mcp_action_tool_schema_omits_rejected_uri_format() {
        let tool = McpPromptTool {
            server_id: "everything".to_string(),
            tool_name: "echo".to_string(),
            description: "Echo test input".to_string(),
            approval_required: false,
            input_schema_json: serde_json::json!({
                "type": "object",
                "properties": {
                    "data": {
                        "type": "object",
                        "properties": {
                            "uri": {"type": "string", "format": "uri"}
                        }
                    }
                }
            })
            .to_string(),
        };
        let schema = maap_mcp_call_action_schema_for_tool(&tool).unwrap();

        assert_eq!(
            schema.pointer("/properties/arguments/properties/data/properties/uri/format"),
            None
        );
        assert_eq!(
            schema.pointer("/properties/arguments/properties/data/required"),
            Some(&serde_json::json!(["uri"]))
        );
        assert_eq!(
            schema.pointer("/properties/arguments/required"),
            Some(&serde_json::json!(["data"]))
        );
    }
}
