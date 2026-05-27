//! OpenAI MAAP tool and strict-schema construction.
//!
//! This module owns cache-stable MAAP tool surfaces, action schemas, MCP
//! argument schema normalization, and provider-facing schema descriptions.

use super::{
    AgentCapability, AllowedAction, AllowedActionSet, McpPromptTool, ModelInteractionKind,
    ModelRequest,
};
use crate::config::{
    CONFIG_CHANGE_OPERATION_NAMES, CONFIG_CHANGE_VALUE_DESCRIPTION,
    config_change_setting_path_description,
};

/// Cache-stable OpenAI MAAP function-tool surfaces.
///
/// OpenAI can cache the complete tool list, while `tool_choice` can force the
/// one surface that is valid for the current turn. Keeping the action subset at
/// the function boundary lets strict schema generation remove disallowed action
/// variants instead of relying on prose inside the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OpenAiMaapToolSurface {
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
    /// Narrow fallback for uncommon composite capability grants.
    CurrentRequest,
}

impl OpenAiMaapToolSurface {
    /// Shared provider-local instruction that forbids prose responses when a
    /// MAAP function tool is available.
    const FUNCTION_CALL_DISCIPLINE: &str = "Return a function call, not prose.";
    /// Shared capability map for provider-local MAAP tool descriptions.
    const CAPABILITY_MAP: &str = "Capability map: shell=local files, rg/sed/cat, git, builds, tests, shell_command, and apply_patch; network_search=web_search; network_fetch=fetch_url; mcp=mcp_call; subagent=send_message or spawn_agent; config_change=config_change; respond_only=final text only.";
    /// Shared anti-pattern corrections for provider-local MAAP tool descriptions.
    const ANTI_EXAMPLES: &str = "Wrong: say(blocked, \"Need shell capability\"). Right: request_capability(capability=\"shell\", reason=\"Need to inspect repository files\"). Wrong: *** Replace File. Right: *** Update File with anchored hunks. Wrong: inferred apply_patch old context. Right: copy old/context lines verbatim from read file evidence.";

    /// Returns cache-stable surfaces that are always advertised to OpenAI.
    pub(super) fn stable_surfaces() -> &'static [Self] {
        &[
            Self::CapabilityDecision,
            Self::RespondOnly,
            Self::Shell,
            Self::NetworkSearch,
            Self::NetworkFetch,
            Self::Mcp,
            Self::Subagent,
            Self::ConfigChange,
        ]
    }

    /// Returns the function-tool name for this surface.
    pub(super) fn tool_name(self) -> &'static str {
        match self {
            Self::CapabilityDecision => "submit_maap_capability_decision",
            Self::RespondOnly => "submit_maap_respond_only_actions",
            Self::Shell => "submit_maap_shell_actions",
            Self::NetworkSearch => "submit_maap_network_search_actions",
            Self::NetworkFetch => "submit_maap_network_fetch_actions",
            Self::Mcp => "submit_maap_mcp_actions",
            Self::Subagent => "submit_maap_subagent_actions",
            Self::ConfigChange => "submit_maap_config_change_actions",
            Self::CurrentRequest => "submit_maap_current_actions",
        }
    }

    /// Returns provider-facing guidance for this function tool.
    fn description(self) -> String {
        match self {
            Self::CapabilityDecision => format!(
                "Submit one MAAP batch for deciding the next coarse capability. {} Only say and request_capability are valid. If any local or external action would help, emit request_capability only; missing shell, patch, web, MCP, messaging, subagent, or config action surface is not a blocker. Model-selected skill lookup/loading is disabled. {} {}",
                Self::FUNCTION_CALL_DISCIPLINE,
                Self::CAPABILITY_MAP,
                Self::ANTI_EXAMPLES
            ),
            Self::RespondOnly => format!(
                "Submit one MAAP batch for response-only progress or completion. {} Model-selected skill lookup/loading is disabled. Only non-executing say actions are valid.",
                Self::FUNCTION_CALL_DISCIPLINE
            ),
            Self::Shell => format!(
                "Submit one MAAP batch for local shell work or Mezzanine patch mutations. {} Use only the action objects in this function schema. If any useful next action is absent and request_capability is available, emit request_capability for that capability instead of say(blocked), final text, or prose asking for access. Shell and apply_patch are the only executable actions in this surface. When the current turn has enough evidence to start implementation, declare next_phase=edit_ready on the batch. After next_phase=edit_ready, any further discovery shell_command should use intent=read or intent=search and include one concrete missing_fact. {} {}",
                Self::FUNCTION_CALL_DISCIPLINE,
                Self::CAPABILITY_MAP,
                Self::ANTI_EXAMPLES
            ),
            Self::NetworkSearch => format!(
                "Submit one MAAP batch for external network search work. {} Use only the action objects in this function schema. If any useful next action is absent and request_capability is available, emit request_capability for that capability instead of say(blocked), final text, or prose asking for access. Web search is the only network action in this surface. {} {}",
                Self::FUNCTION_CALL_DISCIPLINE,
                Self::CAPABILITY_MAP,
                Self::ANTI_EXAMPLES
            ),
            Self::NetworkFetch => format!(
                "Submit one MAAP batch for external URL fetch work. {} Use only the action objects in this function schema. If any useful next action is absent and request_capability is available, emit request_capability for that capability instead of say(blocked), final text, or prose asking for access. Fetch URL is the only network action in this surface. {} {}",
                Self::FUNCTION_CALL_DISCIPLINE,
                Self::CAPABILITY_MAP,
                Self::ANTI_EXAMPLES
            ),
            Self::Mcp => format!(
                "Submit one MAAP batch for MCP tool work. {} Use only the action objects in this function schema. If any useful next action is absent and request_capability is available, emit request_capability for that capability instead of say(blocked), final text, or prose asking for access. MCP calls are limited to the tools listed in this function schema. {} {}",
                Self::FUNCTION_CALL_DISCIPLINE,
                Self::CAPABILITY_MAP,
                Self::ANTI_EXAMPLES
            ),
            Self::Subagent => format!(
                "Submit one MAAP batch for local agent messaging or spawning subagents. {} Use only the action objects in this function schema. If any useful next action is absent and request_capability is available, emit request_capability for that capability instead of say(blocked), final text, or prose asking for access. {} {}",
                Self::FUNCTION_CALL_DISCIPLINE,
                Self::CAPABILITY_MAP,
                Self::ANTI_EXAMPLES
            ),
            Self::ConfigChange => format!(
                "Submit one MAAP batch for proposing Mezzanine configuration changes. {} Use only the action objects in this function schema. If any useful next action is absent and request_capability is available, emit request_capability for that capability instead of say(blocked), final text, or prose asking for access. {} {}",
                Self::FUNCTION_CALL_DISCIPLINE,
                Self::CAPABILITY_MAP,
                Self::ANTI_EXAMPLES
            ),
            Self::CurrentRequest => format!(
                "Submit one MAAP batch for this request's current composite action surface. {} Use only the action objects in this function schema. If any useful next action is absent and request_capability is available, emit request_capability for that capability instead of say(blocked), final text, or prose asking for access. {} {}",
                Self::FUNCTION_CALL_DISCIPLINE,
                Self::CAPABILITY_MAP,
                Self::ANTI_EXAMPLES
            ),
        }
    }

    /// Returns the canonical action set for a cache-stable surface.
    fn allowed_actions(self) -> AllowedActionSet {
        match self {
            Self::CapabilityDecision => AllowedActionSet::capability_decision(),
            Self::RespondOnly => AllowedActionSet::for_capability(AgentCapability::RespondOnly),
            Self::Shell => AllowedActionSet::for_capability(AgentCapability::Shell),
            Self::NetworkSearch => AllowedActionSet::for_capability(AgentCapability::NetworkSearch),
            Self::NetworkFetch => AllowedActionSet::for_capability(AgentCapability::NetworkFetch),
            Self::Mcp => AllowedActionSet::for_capability(AgentCapability::Mcp),
            Self::Subagent => AllowedActionSet::for_capability(AgentCapability::Subagent),
            Self::ConfigChange => AllowedActionSet::for_capability(AgentCapability::ConfigChange),
            Self::CurrentRequest => AllowedActionSet::capability_decision(),
        }
    }
}

/// Returns the OpenAI MAAP tool surface that matches one request.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn openai_maap_tool_surface_for_request(
    request: &ModelRequest,
) -> OpenAiMaapToolSurface {
    let allowed_actions = &request.allowed_actions;
    if *allowed_actions == AllowedActionSet::capability_decision() {
        return if request.interaction_kind == ModelInteractionKind::CapabilityDecision {
            OpenAiMaapToolSurface::CapabilityDecision
        } else {
            OpenAiMaapToolSurface::RespondOnly
        };
    }
    if *allowed_actions == AllowedActionSet::say_only() {
        return OpenAiMaapToolSurface::RespondOnly;
    }
    for (capability, surface) in [
        (AgentCapability::Shell, OpenAiMaapToolSurface::Shell),
        (
            AgentCapability::NetworkSearch,
            OpenAiMaapToolSurface::NetworkSearch,
        ),
        (
            AgentCapability::NetworkFetch,
            OpenAiMaapToolSurface::NetworkFetch,
        ),
        (AgentCapability::Mcp, OpenAiMaapToolSurface::Mcp),
        (AgentCapability::Subagent, OpenAiMaapToolSurface::Subagent),
        (
            AgentCapability::ConfigChange,
            OpenAiMaapToolSurface::ConfigChange,
        ),
    ] {
        if *allowed_actions == AllowedActionSet::for_capability(capability) {
            return surface;
        }
    }
    OpenAiMaapToolSurface::CurrentRequest
}

/// Returns the current request's action set for an OpenAI MAAP tool surface.
fn openai_maap_allowed_actions_for_surface(
    surface: OpenAiMaapToolSurface,
    request: &ModelRequest,
) -> AllowedActionSet {
    if surface == OpenAiMaapToolSurface::CurrentRequest {
        request.allowed_actions.clone()
    } else {
        surface.allowed_actions()
    }
}

/// Builds the cache-stable OpenAI MAAP function-tool list.
pub(super) fn openai_maap_action_batch_tools(request: &ModelRequest) -> Vec<serde_json::Value> {
    let selected_surface = openai_maap_tool_surface_for_request(request);
    let mut tools = OpenAiMaapToolSurface::stable_surfaces()
        .iter()
        .copied()
        .map(|surface| openai_maap_action_batch_tool(surface, request))
        .collect::<Vec<_>>();
    if selected_surface == OpenAiMaapToolSurface::CurrentRequest {
        tools.push(openai_maap_action_batch_tool(selected_surface, request));
    }
    tools
}

/// Runs the openai maap action batch tool operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_maap_action_batch_tool(
    surface: OpenAiMaapToolSurface,
    request: &ModelRequest,
) -> serde_json::Value {
    let allowed_actions = openai_maap_allowed_actions_for_surface(surface, request);
    serde_json::json!({
        "type": "function",
        "name": surface.tool_name(),
        "description": surface.description(),
        "strict": true,
        "parameters": maap_action_batch_schema(&allowed_actions, &request.available_mcp_tools)
    })
}

/// Runs the maap action batch schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn maap_action_batch_schema(
    allowed_actions: &AllowedActionSet,
    available_mcp_tools: &[McpPromptTool],
) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "rationale": {
                "type": "string",
                "minLength": 1,
                "description": "Terse additive reason these actions are next. Do not restate the user request, prior rationale, progress say, or action summaries."
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
            },
            "next_phase": {
                "type": ["string", "null"],
                "enum": ["edit_ready", null],
                "description": "Optional explicit transition for the active turn. Use edit_ready once the current turn has enough evidence to begin implementation. After declaring edit_ready, any additional read/search shell_command should carry one concrete missing_fact."
            }
        },
        "required": ["rationale", "thought", "actions", "next_phase"],
        "additionalProperties": false
    })
}

/// Runs the maap action schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_action_schema(
    allowed_actions: &super::AllowedActionSet,
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
            AllowedAction::ConfigChange => action_schemas.push(maap_config_change_action_schema()),
            AllowedAction::McpCall => action_schemas.extend(
                sorted_mcp_prompt_tools(available_mcp_tools)
                    .into_iter()
                    .map(maap_mcp_call_action_schema_for_tool),
            ),
            AllowedAction::Abort => action_schemas.push(maap_abort_action_schema()),
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
    let mut tools = tools.iter().collect::<Vec<_>>();
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
                    "description": "Coarse action family to expose through the controller when the current schema lacks actions needed for the task. This is not a user permission request. Capability map: shell exposes shell_command and apply_patch for local files, rg/sed/cat, git, builds, tests, and patch edits; network_search exposes web_search; network_fetch exposes fetch_url; mcp exposes mcp_call; subagent exposes send_message and spawn_agent; config_change exposes config_change; respond_only is only for final text."
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
            (
                "intent",
                serde_json::json!({
                    "type": ["string", "null"],
                    "enum": ["read", "search", "build", "test", "format", "git", "other", null],
                    "description": "Optional coarse shell intent. Use read/search for discovery work, test/build/format/git for execution or validation work, and other when none of the listed intents fit."
                }),
            ),
            (
                "missing_fact",
                serde_json::json!({
                    "type": ["string", "null"],
                    "description": "Optional concrete missing fact that justifies an additional discovery shell command after next_phase=edit_ready has already been declared. Omit when not needed."
                }),
            ),
        ],
        &["summary", "command", "intent", "missing_fact"],
    )
}

/// Runs the maap apply patch action schema operation for this subsystem.
fn maap_apply_patch_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "apply_patch",
        [described_string_property(
            "patch",
            "Direct Mezzanine patch text, not Markdown, heredoc, shell, or git apply input. Must start with *** Begin Patch and end with *** End Patch. Accepted file directives are exactly *** Add File, *** Update File, *** Delete File, and optional *** Move to after *** Update File; there is no *** Replace File directive. For whole-file replacement, use *** Update File with hunks. Use relative safe paths only; paths must not be absolute or contain .. traversal. Prefer one anchored update with a distinctive @@ header and 1-6 exact current old/context lines copied verbatim from current file content or fresh action-result evidence; never infer or reconstruct likely code as old context. If the exact target line was not read, read that bounded region before patching. Use multiple small hunks instead of one brittle hunk. Hunk lines use one leading prefix: space context, - removed, + added; *** End of File means no final newline. This is the only semantic file-content mutation action. After mismatch or ambiguity, use diagnostics or reread only missing/stale owner ranges, skip already-applied changes, and retry with a smaller fresh anchored patch.",
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
fn maap_config_change_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "config_change",
        [
            (
                "setting_path",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "description": config_change_setting_path_description()
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
pub(super) fn maap_mcp_call_action_schema_for_tool(tool: &McpPromptTool) -> serde_json::Value {
    maap_action_object_schema(
        "mcp_call",
        [
            (
                "server",
                serde_json::json!({
                    "type": "string",
                    "enum": [tool.server_id]
                }),
            ),
            (
                "tool",
                serde_json::json!({
                    "type": "string",
                    "enum": [tool.tool_name]
                }),
            ),
            ("arguments", mcp_tool_arguments_schema(tool)),
        ],
        &["server", "tool", "arguments"],
    )
}

/// Runs the mcp tool arguments schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mcp_tool_arguments_schema(tool: &McpPromptTool) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(&tool.input_schema_json) {
        Ok(serde_json::Value::Object(schema)) => {
            normalize_openai_strict_schema(serde_json::Value::Object(schema))
        }
        _ => serde_json::json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }),
    }
}

/// Runs the normalize openai strict schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn normalize_openai_strict_schema(mut value: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(schema) = &mut value else {
        return value;
    };

    schema.remove("format");

    if let Some(serde_json::Value::Object(properties)) = schema.get_mut("properties") {
        let required = properties
            .keys()
            .cloned()
            .map(serde_json::Value::String)
            .collect::<Vec<_>>();
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

/// Runs the maap abort action schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_abort_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "abort",
        [(
            "reason",
            serde_json::json!({
                "type": "string"
            }),
        )],
        &["reason"],
    )
}
