//! Typed pane-status configuration and semantic identities.
//!
//! This module owns the product vocabulary for pane-local status fields. It
//! deliberately excludes row fitting, overflow menus, subprocess providers,
//! and command dispatch: renderers resolve these typed values into mux-owned
//! frame segments, while later presentation phases may consume the retained
//! width and priority metadata.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use mez_core::ids::PaneId;

use super::PaneAgentStatusField;

/// Default title-adjacent pane status rail.
pub const DEFAULT_PANE_FRAME_LEFT_STATUS_TEMPLATE: &str = "#{pane.progress}";

/// Default right-aligned pane status rail for new configurations.
pub const DEFAULT_PANE_FRAME_RIGHT_STATUS_TEMPLATE: &str = "#{pane.pwd} #{agent.model} #{agent.reasoning} #{agent.thinking} #{agent.planning} #{agent.routing} #{agent.latency} #{policy.mode} #{agent.context_usage} #{agent.status} #{history.position}";

/// One built-in pane-scoped status value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneStatusField {
    /// Determinate terminal progress.
    PaneProgress,
    /// Pane working directory.
    PaneWorkingDirectory,
    /// Harness-owned pane state.
    PaneStatus,
    /// Scrollback position.
    HistoryPosition,
    /// Active agent model.
    AgentModel,
    /// Active reasoning setting.
    AgentReasoning,
    /// Provider thinking mode.
    AgentThinking,
    /// Pane planning-only mode.
    AgentPlanning,
    /// Pane routing mode.
    AgentRouting,
    /// Latency preference.
    AgentLatency,
    /// Active model preset.
    AgentPreset,
    /// Human-readable agent name.
    AgentName,
    /// Context usage.
    AgentContextUsage,
    /// Agent lifecycle status.
    AgentStatus,
    /// Effective approval policy.
    PolicyMode,
}

impl PaneStatusField {
    /// Parses one public pane-status field name.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pane.progress" => Some(Self::PaneProgress),
            "pane.pwd" => Some(Self::PaneWorkingDirectory),
            "pane.status" => Some(Self::PaneStatus),
            "history.position" => Some(Self::HistoryPosition),
            "agent.model" => Some(Self::AgentModel),
            "agent.reasoning" => Some(Self::AgentReasoning),
            "agent.thinking" => Some(Self::AgentThinking),
            "agent.planning" => Some(Self::AgentPlanning),
            "agent.routing" => Some(Self::AgentRouting),
            "agent.latency" => Some(Self::AgentLatency),
            "agent.preset" => Some(Self::AgentPreset),
            "agent.name" => Some(Self::AgentName),
            "agent.context_usage" => Some(Self::AgentContextUsage),
            "agent.status" => Some(Self::AgentStatus),
            "policy.mode" => Some(Self::PolicyMode),
            _ => None,
        }
    }

    /// Returns the canonical public field name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PaneProgress => "pane.progress",
            Self::PaneWorkingDirectory => "pane.pwd",
            Self::PaneStatus => "pane.status",
            Self::HistoryPosition => "history.position",
            Self::AgentModel => "agent.model",
            Self::AgentReasoning => "agent.reasoning",
            Self::AgentThinking => "agent.thinking",
            Self::AgentPlanning => "agent.planning",
            Self::AgentRouting => "agent.routing",
            Self::AgentLatency => "agent.latency",
            Self::AgentPreset => "agent.preset",
            Self::AgentName => "agent.name",
            Self::AgentContextUsage => "agent.context_usage",
            Self::AgentStatus => "agent.status",
            Self::PolicyMode => "policy.mode",
        }
    }

    /// Returns the built-in selector/toggle action, when this field has one.
    pub const fn builtin_action(self) -> Option<PaneAgentStatusField> {
        match self {
            Self::AgentModel => Some(PaneAgentStatusField::Model),
            Self::AgentReasoning => Some(PaneAgentStatusField::Reasoning),
            Self::AgentThinking => Some(PaneAgentStatusField::Thinking),
            Self::AgentPlanning => Some(PaneAgentStatusField::Planning),
            Self::AgentRouting => Some(PaneAgentStatusField::Routing),
            Self::AgentLatency => Some(PaneAgentStatusField::Latency),
            Self::AgentPreset => Some(PaneAgentStatusField::Preset),
            Self::PolicyMode => Some(PaneAgentStatusField::ApprovalPolicy),
            _ => None,
        }
    }

    /// Returns whether this field may use percentage formatting.
    pub const fn supports_percent(self) -> bool {
        matches!(self, Self::PaneProgress | Self::AgentContextUsage)
    }

    /// Returns whether the field is scoped to an agent pane.
    pub const fn is_agent_scoped(self) -> bool {
        matches!(
            self,
            Self::AgentModel
                | Self::AgentReasoning
                | Self::AgentThinking
                | Self::AgentPlanning
                | Self::AgentRouting
                | Self::AgentLatency
                | Self::AgentPreset
                | Self::AgentName
                | Self::AgentContextUsage
                | Self::AgentStatus
                | Self::PolicyMode
        )
    }
}

/// Supported built-in display formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneStatusFormat {
    /// Preserve the resolved display value.
    Full,
    /// Use the registry's compact built-in label.
    Short,
    /// Normalize a numeric value as a percentage.
    Percent,
}

impl PaneStatusFormat {
    /// Parses one public format name.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "full" => Some(Self::Full),
            "short" => Some(Self::Short),
            "percent" => Some(Self::Percent),
            _ => None,
        }
    }
}

/// Finite conditions supported by pane-status definitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneStatusCondition {
    /// Show only in the agent view.
    AgentView,
    /// Show only outside the agent view.
    ShellView,
    /// Show only on the focused pane.
    Focused,
    /// Show only on an unfocused pane.
    Unfocused,
    /// Show only while pane-local agent work is active.
    Busy,
    /// Show only while pane-local agent work is inactive.
    Idle,
    /// Show only when the field capability is available.
    Supported,
    /// Show only when the resolved value is non-empty.
    Nonempty,
    /// Show only while viewing scrollback.
    Scrollback,
}

impl PaneStatusCondition {
    /// Parses one public condition name.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "agent-view" => Some(Self::AgentView),
            "shell-view" => Some(Self::ShellView),
            "focused" => Some(Self::Focused),
            "unfocused" => Some(Self::Unfocused),
            "busy" => Some(Self::Busy),
            "idle" => Some(Self::Idle),
            "supported" => Some(Self::Supported),
            "nonempty" => Some(Self::Nonempty),
            "scrollback" => Some(Self::Scrollback),
            _ => None,
        }
    }
}

/// Theme role used for one pane-status segment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PaneStatusStyle {
    /// Use the built-in field's state-aware style.
    #[default]
    Automatic,
    /// Scroll-position colors.
    ScrollIndicator,
    /// Terminal-progress colors.
    PaneProgress,
    /// Working-directory colors.
    PaneWorkingDirectory,
    /// Agent-model colors.
    AgentModel,
    /// Agent-reasoning colors.
    AgentReasoning,
    /// Idle agent-status colors.
    AgentStatusIdle,
    /// Running agent-status colors.
    AgentStatusRunning,
    /// Blocked agent-status colors.
    AgentStatusBlocked,
    /// Failed agent-status colors.
    AgentStatusFailed,
}

impl PaneStatusStyle {
    /// Parses one public style role.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "automatic" | "default" => Some(Self::Automatic),
            "scroll-indicator" => Some(Self::ScrollIndicator),
            "pane-progress" => Some(Self::PaneProgress),
            "pane-pwd" => Some(Self::PaneWorkingDirectory),
            "agent-model" => Some(Self::AgentModel),
            "agent-reasoning" => Some(Self::AgentReasoning),
            "agent-status-idle" => Some(Self::AgentStatusIdle),
            "agent-status-running" => Some(Self::AgentStatusRunning),
            "agent-status-blocked" => Some(Self::AgentStatusBlocked),
            "agent-status-failed" => Some(Self::AgentStatusFailed),
            _ => None,
        }
    }
}

/// Typed action attached to a rendered pane-status occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneStatusAction {
    /// The segment is read-only.
    None,
    /// Invoke the field's existing built-in selector or toggle.
    Builtin(PaneAgentStatusField),
}

/// Rail containing a pane-status occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneStatusRail {
    /// Title-adjacent status rail.
    Left,
    /// Right-aligned status rail.
    Right,
}

/// Stable identity of one template occurrence within a configured rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneStatusOccurrenceId {
    /// Rail containing the occurrence.
    pub rail: PaneStatusRail,
    /// Zero-based marker ordinal within that rail.
    pub ordinal: u16,
}

/// Semantic identity carried from resolution through styling and hit testing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneStatusSegmentIdentity {
    /// Stable pane that owns this occurrence.
    pub owner_pane_id: PaneId,
    /// Stable occurrence within the configured rail.
    pub occurrence: PaneStatusOccurrenceId,
    /// Built-in source field.
    pub field: PaneStatusField,
    /// Resolved theme role.
    pub style: PaneStatusStyle,
    /// Resolved typed interaction.
    pub action: PaneStatusAction,
    /// Compact display alternative retained for later whole-pill layout.
    pub compact_display: String,
    /// Minimum useful value width retained for later whole-pill layout.
    pub min_width: Option<usize>,
    /// Maximum configured value width.
    pub max_width: Option<usize>,
    /// Configured retention priority.
    pub priority: u8,
    /// Deterministic generation of the effective status configuration.
    pub config_generation: u64,
    /// Deterministic generation of relevant pane context.
    pub context_generation: u64,
}

/// Effective definition of one named or bare built-in pane pill.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneStatusPillDefinition {
    /// Built-in source field.
    pub field: PaneStatusField,
    /// Optional text prepended to the formatted value.
    pub label: Option<String>,
    /// Primary display format.
    pub format: PaneStatusFormat,
    /// Compact display format retained for the later overflow planner.
    pub compact_format: PaneStatusFormat,
    /// Finite AND-combined visibility conditions.
    pub when: Vec<PaneStatusCondition>,
    /// Minimum useful value width retained for later layout policy.
    pub min_width: Option<usize>,
    /// Maximum displayed value width.
    pub max_width: Option<usize>,
    /// Retention priority retained for later layout policy.
    pub priority: u8,
    /// Theme role.
    pub style: PaneStatusStyle,
    /// Typed built-in or read-only action.
    pub action: PaneStatusAction,
}

impl PaneStatusPillDefinition {
    /// Builds the registry defaults for one bare built-in field.
    pub fn builtin(field: PaneStatusField) -> Self {
        let format = if matches!(
            field,
            PaneStatusField::AgentThinking
                | PaneStatusField::AgentPlanning
                | PaneStatusField::AgentRouting
        ) {
            PaneStatusFormat::Short
        } else {
            PaneStatusFormat::Full
        };
        let when = if field.is_agent_scoped() {
            vec![
                PaneStatusCondition::AgentView,
                PaneStatusCondition::Supported,
                PaneStatusCondition::Nonempty,
            ]
        } else if matches!(
            field,
            PaneStatusField::HistoryPosition | PaneStatusField::PaneWorkingDirectory
        ) {
            vec![
                PaneStatusCondition::Scrollback,
                PaneStatusCondition::Nonempty,
            ]
        } else {
            vec![PaneStatusCondition::Nonempty]
        };
        Self {
            field,
            label: None,
            format,
            compact_format: PaneStatusFormat::Short,
            when,
            min_width: None,
            max_width: None,
            priority: 50,
            style: PaneStatusStyle::Automatic,
            action: field
                .builtin_action()
                .map(PaneStatusAction::Builtin)
                .unwrap_or(PaneStatusAction::None),
        }
    }
}

/// Complete effective pane-status configuration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneStatusConfig {
    /// Title-adjacent template.
    pub left_status: String,
    /// Right-aligned template.
    pub right_status: String,
    /// Named built-in definitions referenced as `pill.<name>`.
    pub pills: BTreeMap<String, PaneStatusPillDefinition>,
}

impl PaneStatusConfig {
    /// Returns a deterministic generation for stable segment identities.
    pub fn generation(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

impl Default for PaneStatusConfig {
    fn default() -> Self {
        Self {
            left_status: DEFAULT_PANE_FRAME_LEFT_STATUS_TEMPLATE.to_string(),
            right_status: DEFAULT_PANE_FRAME_RIGHT_STATUS_TEMPLATE.to_string(),
            pills: BTreeMap::new(),
        }
    }
}
