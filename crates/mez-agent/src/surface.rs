//! Provider interaction and MAAP action-surface types.
//!
//! This module owns the small, shared vocabulary that describes what kind of
//! model interaction is in progress and which concrete MAAP actions are exposed
//! for that interaction. Keeping these types together avoids mixing action
//! routing rules with context block storage and provider message assembly.

use std::collections::BTreeSet;

/// Describes the kind of provider interaction Mezzanine is requesting.
///
/// The interaction kind is controller-owned state. It tells providers whether
/// the model is currently deciding which capability it needs or emitting
/// executable MAAP actions after a capability has been granted. Runtime-owned
/// default gates may still widen a capability-decision request with already
/// available diagnostic or integration actions such as MCP and memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelInteractionKind {
    /// The model may speak to the user or request a coarse capability. The base
    /// surface is non-executing, but runtime-owned default gates can add
    /// already-available MCP or memory actions before provider submission.
    CapabilityDecision,
    /// The model may emit only the executable MAAP actions exposed through the
    /// request's allowed-action set.
    ActionExecution,
    /// The controller has settled capability decisions and the model is
    /// continuing on the resulting concrete action surface.
    CapabilityContinuation,
    /// The model is repairing malformed MAAP for the same interaction surface.
    MaapRepair,
    /// The model is producing an internal automatic sizing decision. The
    /// response is parsed as structured JSON and is not replayed as ordinary
    /// conversation context.
    AutoSizing,
    /// The model is judging one completed agent-macro step. The response is a
    /// constrained JSON decision that the runtime validates and executes; it is
    /// not a MAAP action batch and is not replayed as conversation content.
    MacroJudge,
    /// The model is classifying an ambiguous Bubblewrap payload failure from
    /// bounded runtime evidence. The response is structured JSON and cannot
    /// grant execution authority.
    SandboxFailureAssessment,
    /// The model is retrying after provider output exhaustion and must return
    /// one minimal complete action batch or final answer.
    OutputLimitRetry,
    /// A routed worker is returning the structured JSON handoff requested by
    /// its controller task context.
    RoutedHandoff,
    /// A routed worker is correcting a rejected structured JSON handoff.
    RoutedHandoffRepair,
    /// The parent model is presenting completed routed-worker evidence to the
    /// original user.
    RoutedPresentation,
    /// The parent model is explaining a routed workflow failure to the user.
    RoutedFailureExplanation,
    /// The model is producing a bounded user-facing summary of a terminal
    /// provider or controller failure.
    FailureSummary,
}

impl ModelInteractionKind {
    /// Returns the stable provider/debug name for the interaction kind.
    pub fn as_str(self) -> &'static str {
        match self {
            ModelInteractionKind::CapabilityDecision => "capability_decision",
            ModelInteractionKind::ActionExecution => "action_execution",
            ModelInteractionKind::CapabilityContinuation => "capability_continuation",
            ModelInteractionKind::MaapRepair => "maap_repair",
            ModelInteractionKind::AutoSizing => "auto_sizing",
            ModelInteractionKind::MacroJudge => "macro_judge",
            ModelInteractionKind::SandboxFailureAssessment => "sandbox_failure_assessment",
            ModelInteractionKind::OutputLimitRetry => "output_limit_retry",
            ModelInteractionKind::RoutedHandoff => "routed_handoff",
            ModelInteractionKind::RoutedHandoffRepair => "routed_handoff_repair",
            ModelInteractionKind::RoutedPresentation => "routed_presentation",
            ModelInteractionKind::RoutedFailureExplanation => "routed_failure_explanation",
            ModelInteractionKind::FailureSummary => "failure_summary",
        }
    }

    /// Reports whether this provider request expects a MAAP action batch.
    pub fn expects_maap_batch(self) -> bool {
        matches!(
            self,
            ModelInteractionKind::CapabilityDecision
                | ModelInteractionKind::ActionExecution
                | ModelInteractionKind::CapabilityContinuation
                | ModelInteractionKind::MaapRepair
                | ModelInteractionKind::OutputLimitRetry
                | ModelInteractionKind::RoutedPresentation
                | ModelInteractionKind::RoutedFailureExplanation
                | ModelInteractionKind::FailureSummary
        )
    }

    /// Reports whether this provider request expects runtime-owned JSON.
    pub fn expects_structured_json(self) -> bool {
        matches!(
            self,
            ModelInteractionKind::AutoSizing
                | ModelInteractionKind::MacroJudge
                | ModelInteractionKind::SandboxFailureAssessment
                | ModelInteractionKind::RoutedHandoff
                | ModelInteractionKind::RoutedHandoffRepair
        )
    }

    /// Reports whether this interaction returns a routed-worker handoff.
    pub fn is_routed_handoff(self) -> bool {
        matches!(self, Self::RoutedHandoff | Self::RoutedHandoffRepair)
    }

    /// Returns the stable mode-specific system instruction for exceptional
    /// interactions that share the ordinary MAAP response envelope.
    pub fn system_instruction(self) -> Option<&'static str> {
        match self {
            ModelInteractionKind::CapabilityContinuation => Some(
                "Continue the active task using the appended controller capability decisions and the currently allowed action surface. Do not repeat the capability request or describe the controller negotiation unless it affects the user-facing result.",
            ),
            ModelInteractionKind::MaapRepair => Some(
                "The previous provider response failed MAAP validation before any action executed. Return exactly one corrected MAAP action batch on the currently allowed surface. Do not mention the repair process to the user.",
            ),
            ModelInteractionKind::OutputLimitRetry => Some(
                "The previous response hit the provider output limit. Return one minimal complete MAAP batch when work remains or one short final answer when it does not. Omit progress prose, plans, evidence recaps, command logs, and explanations from this retry.",
            ),
            ModelInteractionKind::RoutedHandoff => Some(
                "Complete the routed handoff task from controller-origin context. Return only the requested structured JSON handoff; do not continue implementation or address the end user.",
            ),
            ModelInteractionKind::RoutedHandoffRepair => Some(
                "Correct the routed handoff using the appended invalid-output and validation evidence. Return only the requested corrected structured JSON handoff.",
            ),
            ModelInteractionKind::RoutedPresentation => Some(
                "Answer the original user from the appended routed-worker result and handoff evidence. Preserve the worker's facts, do not redo its work, and do not discuss internal routing unless it is necessary to explain the result.",
            ),
            ModelInteractionKind::RoutedFailureExplanation => Some(
                "Give the original user one concise, accurate explanation of the routed workflow failure using the appended evidence. Do not claim success, invent missing results, or retry the routed work.",
            ),
            ModelInteractionKind::FailureSummary => Some(
                "Return one concise user-facing summary of the terminal failure evidence. State what failed and any concrete next step without inventing completion or emitting executable actions.",
            ),
            ModelInteractionKind::CapabilityDecision
            | ModelInteractionKind::ActionExecution
            | ModelInteractionKind::AutoSizing
            | ModelInteractionKind::MacroJudge
            | ModelInteractionKind::SandboxFailureAssessment => None,
        }
    }

    /// Returns the diagnostic reason when this mode intentionally changes the
    /// request's stable instruction profile.
    pub fn expected_cache_break_reason(self) -> Option<&'static str> {
        match self {
            ModelInteractionKind::CapabilityDecision | ModelInteractionKind::ActionExecution => {
                None
            }
            ModelInteractionKind::CapabilityContinuation
            | ModelInteractionKind::MaapRepair
            | ModelInteractionKind::AutoSizing
            | ModelInteractionKind::MacroJudge
            | ModelInteractionKind::SandboxFailureAssessment
            | ModelInteractionKind::OutputLimitRetry
            | ModelInteractionKind::RoutedHandoff
            | ModelInteractionKind::RoutedHandoffRepair
            | ModelInteractionKind::RoutedPresentation
            | ModelInteractionKind::RoutedFailureExplanation
            | ModelInteractionKind::FailureSummary => Some(self.as_str()),
        }
    }
}

/// Coarse capabilities the model may request before executable actions are
/// exposed.
///
/// Capabilities are intentionally broader than individual MAAP actions. The
/// controller can grant or deny them with simple policy and runtime-context
/// checks, while the model still chooses the concrete action once a capability
/// is granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentCapability {
    /// Respond to the user without external effects.
    RespondOnly,
    /// Execute a pane shell command.
    Shell,
    /// Search external HTTP(S) information.
    NetworkSearch,
    /// Fetch an external HTTP(S) URL.
    NetworkFetch,
    /// Call an available MCP tool.
    Mcp,
    /// Send a local agent message or spawn a subagent.
    Subagent,
    /// Request a Mezzanine configuration change.
    ConfigChange,
    /// Search or store persistent memory records.
    Memory,
    /// Add, query, or delete local project issue records.
    Issues,
}

impl AgentCapability {
    /// Returns the stable schema/debug name for the capability.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentCapability::RespondOnly => "respond_only",
            AgentCapability::Shell => "shell",
            AgentCapability::NetworkSearch => "network_search",
            AgentCapability::NetworkFetch => "network_fetch",
            AgentCapability::Mcp => "mcp",
            AgentCapability::Subagent => "subagent",
            AgentCapability::ConfigChange => "config_change",
            AgentCapability::Memory => "memory",
            AgentCapability::Issues => "issues",
        }
    }

    /// Parses a model-authored capability name.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "respond_only" => Some(AgentCapability::RespondOnly),
            "shell" => Some(AgentCapability::Shell),
            "network_search" => Some(AgentCapability::NetworkSearch),
            "network_fetch" => Some(AgentCapability::NetworkFetch),
            "mcp" => Some(AgentCapability::Mcp),
            "subagent" => Some(AgentCapability::Subagent),
            "config_change" => Some(AgentCapability::ConfigChange),
            "memory" => Some(AgentCapability::Memory),
            "issues" => Some(AgentCapability::Issues),
            _ => None,
        }
    }

    /// Returns every provider-visible capability name.
    pub fn all_names() -> &'static [&'static str] {
        &[
            "respond_only",
            "shell",
            "network_search",
            "network_fetch",
            "mcp",
            "subagent",
            "config_change",
            "memory",
            "issues",
        ]
    }
}

/// Concrete MAAP action kinds that may be exposed in one provider request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AllowedAction {
    /// User-facing text.
    Say,
    /// Non-executing capability request.
    RequestCapability,
    /// Skill catalog request.
    RequestSkills,
    /// Skill context loading.
    CallSkill,
    /// Pane shell command.
    ShellCommand,
    /// Apply a patch.
    ApplyPatch,
    /// External web search.
    WebSearch,
    /// External URL fetch.
    FetchUrl,
    /// Local agent message.
    SendMessage,
    /// Subagent spawn.
    SpawnAgent,
    /// Configuration change.
    ConfigChange,
    /// MCP tool call.
    McpCall,
    /// Search persistent memory records.
    MemorySearch,
    /// Store one persistent memory record.
    MemoryStore,
    /// Add one local project issue.
    IssueAdd,
    /// Update one local project issue.
    IssueUpdate,
    /// Query local project issues.
    IssueQuery,
    /// Delete one local project issue.
    IssueDelete,
}

impl AllowedAction {
    /// Returns the stable MAAP action type for this allowed action.
    pub fn action_type(self) -> &'static str {
        match self {
            AllowedAction::Say => "say",
            AllowedAction::RequestCapability => "request_capability",
            AllowedAction::RequestSkills => "request_skills",
            AllowedAction::CallSkill => "call_skill",
            AllowedAction::ShellCommand => "shell_command",
            AllowedAction::ApplyPatch => "apply_patch",
            AllowedAction::WebSearch => "web_search",
            AllowedAction::FetchUrl => "fetch_url",
            AllowedAction::SendMessage => "send_message",
            AllowedAction::SpawnAgent => "spawn_agent",
            AllowedAction::ConfigChange => "config_change",
            AllowedAction::McpCall => "mcp_call",
            AllowedAction::MemorySearch => "memory_search",
            AllowedAction::MemoryStore => "memory_store",
            AllowedAction::IssueAdd => "issue_add",
            AllowedAction::IssueUpdate => "issue_update",
            AllowedAction::IssueQuery => "issue_query",
            AllowedAction::IssueDelete => "issue_delete",
        }
    }

    /// Maps a MAAP action type to the corresponding allowed-action value.
    pub fn from_action_type(action_type: &str) -> Option<Self> {
        match action_type {
            "say" => Some(AllowedAction::Say),
            "request_capability" => Some(AllowedAction::RequestCapability),
            "request_skills" => Some(AllowedAction::RequestSkills),
            "call_skill" => Some(AllowedAction::CallSkill),
            "shell_command" => Some(AllowedAction::ShellCommand),
            "apply_patch" => Some(AllowedAction::ApplyPatch),
            "web_search" => Some(AllowedAction::WebSearch),
            "fetch_url" => Some(AllowedAction::FetchUrl),
            "send_message" => Some(AllowedAction::SendMessage),
            "spawn_agent" => Some(AllowedAction::SpawnAgent),
            "config_change" => Some(AllowedAction::ConfigChange),
            "mcp_call" => Some(AllowedAction::McpCall),
            "memory_search" => Some(AllowedAction::MemorySearch),
            "memory_store" => Some(AllowedAction::MemoryStore),
            "issue_add" => Some(AllowedAction::IssueAdd),
            "issue_update" => Some(AllowedAction::IssueUpdate),
            "issue_query" => Some(AllowedAction::IssueQuery),
            "issue_delete" => Some(AllowedAction::IssueDelete),
            _ => None,
        }
    }
}

/// Controller-owned concrete action surface for one provider request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedActionSet {
    /// Stores the allowed action values.
    pub actions: BTreeSet<AllowedAction>,
    /// Product-provided setting-path guidance for config-change actions.
    config_change_setting_path_description: Option<String>,
}

impl AllowedActionSet {
    /// Builds the initial non-executing capability-decision surface.
    pub fn capability_decision() -> Self {
        Self::from_actions([AllowedAction::Say, AllowedAction::RequestCapability])
    }

    /// Builds a response-only action surface.
    pub fn respond_only() -> Self {
        Self::from_actions([AllowedAction::Say])
    }

    /// Builds the non-effecting base surface for action-execution requests.
    pub fn action_execution_base() -> Self {
        Self::from_actions([AllowedAction::Say, AllowedAction::RequestCapability])
    }

    /// Builds an action surface that can only emit user-facing text.
    pub fn say_only() -> Self {
        Self::from_actions([AllowedAction::Say])
    }

    /// Builds the executable action surface exposed after a capability grant.
    pub fn for_capability(capability: AgentCapability) -> Self {
        let mut output = Self::action_execution_base();
        match capability {
            AgentCapability::RespondOnly => {}
            AgentCapability::Shell => {
                output.extend([AllowedAction::ShellCommand, AllowedAction::ApplyPatch])
            }
            AgentCapability::NetworkSearch => output.extend([AllowedAction::WebSearch]),
            AgentCapability::NetworkFetch => output.extend([AllowedAction::FetchUrl]),
            AgentCapability::Mcp => output.extend([AllowedAction::McpCall]),
            AgentCapability::Subagent => {
                output.extend([AllowedAction::SendMessage, AllowedAction::SpawnAgent])
            }
            AgentCapability::ConfigChange => output.extend([AllowedAction::ConfigChange]),
            AgentCapability::Memory => {
                output.extend([AllowedAction::MemorySearch, AllowedAction::MemoryStore])
            }
            AgentCapability::Issues => output.extend([
                AllowedAction::IssueAdd,
                AllowedAction::IssueUpdate,
                AllowedAction::IssueQuery,
                AllowedAction::IssueDelete,
            ]),
        }
        output
    }

    /// Builds a set from a sequence of actions.
    pub fn from_actions(actions: impl IntoIterator<Item = AllowedAction>) -> Self {
        Self {
            actions: actions.into_iter().collect(),
            config_change_setting_path_description: None,
        }
    }

    /// Attaches product-specific setting-path guidance to this action surface.
    pub fn with_config_change_setting_path_description(
        mut self,
        description: impl Into<String>,
    ) -> Self {
        self.config_change_setting_path_description = Some(description.into());
        self
    }

    /// Returns product-specific config-change setting-path guidance, if set.
    pub fn config_change_setting_path_description(&self) -> Option<&str> {
        self.config_change_setting_path_description.as_deref()
    }

    /// Adds actions to the set.
    pub fn extend(&mut self, actions: impl IntoIterator<Item = AllowedAction>) {
        self.actions.extend(actions);
    }

    /// Adds all actions from another set.
    pub fn extend_set(&mut self, other: &AllowedActionSet) {
        self.actions.extend(other.actions.iter().copied());
        if other.config_change_setting_path_description.is_some() {
            self.config_change_setting_path_description =
                other.config_change_setting_path_description.clone();
        }
    }

    /// Removes one action from the exposed action surface.
    pub fn remove(&mut self, action: AllowedAction) {
        self.actions.remove(&action);
    }

    /// Returns true when the given action is exposed in this set.
    pub fn contains(&self, action: AllowedAction) -> bool {
        self.actions.contains(&action)
    }

    /// Returns action type names in stable order for trace and debug output.
    pub fn action_type_names(&self) -> Vec<&'static str> {
        self.actions
            .iter()
            .map(|action| action.action_type())
            .collect()
    }
}
