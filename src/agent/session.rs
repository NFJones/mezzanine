//! Agent Session implementation.
//!
//! This module owns the agent session boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    BTreeMap, McpRegistry, MezError, PermissionPolicy, Result, baseline_slash_commands,
    validate_non_empty,
};
use crate::mcp::{
    McpApprovalSetting, McpServerKind, McpServerState, McpServerStatus, McpToolEffects,
    McpToolState,
};

// Agent shell sessions, stores, and shell display helpers.

/// Carries Agent Turn Trigger state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTurnTrigger {
    /// Represents the User Prompt case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    UserPrompt,
    /// Represents the Local Message case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    LocalMessage,
    /// Represents the Scheduled Task case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ScheduledTask,
    /// Represents the Subagent Event case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    SubagentEvent,
    /// Represents the Approved Continuation case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ApprovedContinuation,
}

/// Carries Agent Turn State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTurnState {
    /// Represents the Queued case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Queued,
    /// Represents the Running case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Running,
    /// Represents the Blocked case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Blocked,
    /// Represents the Completed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Completed,
    /// Represents the Failed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Failed,
    /// Represents the Interrupted case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Interrupted,
}

/// Carries Agent Shell Visibility state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentShellVisibility {
    /// Represents the Hidden case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Hidden,
    /// Represents the Visible case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Visible,
    /// Represents the Hide Pending Task Completion case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HidePendingTaskCompletion,
}

/// Carries Agent Log Level state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentLogLevel {
    /// Represents the Normal case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Normal,
    /// Represents the Verbose case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Verbose,
    /// Represents the Debug case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Debug,
    /// Represents the Trace case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Trace,
}

impl AgentLogLevel {
    /// Runs the as str operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentLogLevel::Normal => "normal",
            AgentLogLevel::Verbose => "verbose",
            AgentLogLevel::Debug => "debug",
            AgentLogLevel::Trace => "trace",
        }
    }

    /// Runs the parse operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "normal" | "default" | "info" | "off" => Some(AgentLogLevel::Normal),
            "verbose" => Some(AgentLogLevel::Verbose),
            "debug" => Some(AgentLogLevel::Debug),
            "trace" => Some(AgentLogLevel::Trace),
            _ => None,
        }
    }

    /// Runs the shows verbose status operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn shows_verbose_status(self) -> bool {
        matches!(
            self,
            AgentLogLevel::Verbose | AgentLogLevel::Debug | AgentLogLevel::Trace
        )
    }

    /// Runs the shows thinking operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn shows_thinking(self) -> bool {
        true
    }

    /// Runs the shows debug operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn shows_debug(self) -> bool {
        matches!(self, AgentLogLevel::Debug | AgentLogLevel::Trace)
    }

    /// Runs the shows trace operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn shows_trace(self) -> bool {
        self == AgentLogLevel::Trace
    }

    /// Runs the shows shell view operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn shows_shell_view(self) -> bool {
        matches!(self, AgentLogLevel::Verbose | AgentLogLevel::Trace)
    }
}

/// Carries Agent Shell Session state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentShellSession {
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the visibility value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub visibility: AgentShellVisibility,
    /// Stores the running turn id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub running_turn_id: Option<String>,
    /// Stores the transcript entries value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub transcript_entries: u64,
    /// Stores the log level value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub log_level: AgentLogLevel,
}

/// Carries Agent Shell Store state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentShellStore {
    /// Stores the sessions by pane value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) sessions_by_pane: BTreeMap<String, AgentShellSession>,
}

impl AgentShellStore {
    /// Runs the ensure session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn ensure_session(&mut self, pane_id: impl Into<String>) -> Result<&mut AgentShellSession> {
        let pane_id = pane_id.into();
        validate_non_empty("pane id", &pane_id)?;
        if !self.sessions_by_pane.contains_key(&pane_id) {
            self.sessions_by_pane.insert(
                pane_id.clone(),
                AgentShellSession {
                    session_id: new_agent_session_uuid(),
                    pane_id: pane_id.clone(),
                    visibility: AgentShellVisibility::Hidden,
                    running_turn_id: None,
                    transcript_entries: 0,
                    log_level: AgentLogLevel::Normal,
                },
            );
        }
        self.session_mut(&pane_id)
    }

    /// Runs the enter or resume operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn enter_or_resume(&mut self, pane_id: impl Into<String>) -> Result<&AgentShellSession> {
        let pane_id = pane_id.into();
        let session = self.ensure_session(pane_id)?;
        session.visibility = AgentShellVisibility::Visible;
        Ok(session)
    }

    /// Runs the request exit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn request_exit(&mut self, pane_id: &str) -> Result<&AgentShellSession> {
        let session = self.session_mut(pane_id)?;
        session.visibility = AgentShellVisibility::Hidden;
        Ok(session)
    }

    /// Requests that an agent shell hide as soon as its active turn completes.
    ///
    /// # Parameters
    /// - `pane_id`: The pane-local agent shell session to hide after stopping.
    pub fn request_hide_pending_task_completion(
        &mut self,
        pane_id: &str,
    ) -> Result<&AgentShellSession> {
        let session = self.session_mut(pane_id)?;
        session.visibility = AgentShellVisibility::HidePendingTaskCompletion;
        Ok(session)
    }

    /// Runs the start turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn start_turn(&mut self, pane_id: &str, turn_id: impl Into<String>) -> Result<()> {
        let turn_id = turn_id.into();
        validate_non_empty("turn id", &turn_id)?;
        let session = self.session_mut(pane_id)?;
        if session.running_turn_id.is_some() {
            return Err(MezError::conflict(
                "agent shell session already has a running turn",
            ));
        }
        session.running_turn_id = Some(turn_id);
        Ok(())
    }

    /// Runs the finish turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn finish_turn(&mut self, pane_id: &str, turn_id: &str) -> Result<&AgentShellSession> {
        let session = self.session_mut(pane_id)?;
        if session.running_turn_id.as_deref() != Some(turn_id) {
            return Err(MezError::invalid_args(
                "finished turn does not match running agent shell turn",
            ));
        }
        session.running_turn_id = None;
        if session.visibility == AgentShellVisibility::HidePendingTaskCompletion {
            session.visibility = AgentShellVisibility::Hidden;
        }
        Ok(session)
    }

    /// Records newly persisted transcript entries for the pane conversation.
    ///
    /// The counter tracks durable transcript entries in the active raw replay
    /// window, not completed turns. Compaction may retain a bounded recent
    /// transcript tail, and context assembly uses this count as the exact raw
    /// replay window after the compacted summary.
    pub fn record_transcript_entries(
        &mut self,
        pane_id: &str,
        entries: usize,
    ) -> Result<&AgentShellSession> {
        let session = self.session_mut(pane_id)?;
        session.transcript_entries = session
            .transcript_entries
            .saturating_add(u64::try_from(entries).unwrap_or(u64::MAX));
        Ok(session)
    }

    /// Removes and returns the agent shell session for a pane that has left the
    /// runtime layout.
    ///
    /// Closing panes must drop their prompt/session state so later pane ids,
    /// agent listings, and snapshot capture do not retain a stale agent shell
    /// for a pane the user can no longer inspect.
    pub fn remove_session(&mut self, pane_id: &str) -> Option<AgentShellSession> {
        self.sessions_by_pane.remove(pane_id)
    }

    /// Retains a bounded count of recent transcript entries for raw replay.
    ///
    /// The transcript file remains append-only; this method only moves the
    /// model-facing replay boundary so older transcript content is represented
    /// by compact memory while the recent tail stays exact for follow-up
    /// references.
    pub fn retain_recent_transcript_entries(
        &mut self,
        pane_id: &str,
        entries: u64,
    ) -> Result<&AgentShellSession> {
        let session = self.session_mut(pane_id)?;
        session.transcript_entries = entries;
        Ok(session)
    }

    /// Runs the bind conversation operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn bind_conversation(
        &mut self,
        pane_id: &str,
        conversation_id: impl Into<String>,
        transcript_entries: u64,
    ) -> Result<&AgentShellSession> {
        let conversation_id = conversation_id.into();
        validate_non_empty("conversation id", &conversation_id)?;
        let session = self.session_mut(pane_id)?;
        if session.running_turn_id.is_some() {
            return Err(MezError::conflict(
                "cannot switch conversations while an agent turn is running",
            ));
        }
        session.session_id = conversation_id;
        session.transcript_entries = transcript_entries;
        session.visibility = AgentShellVisibility::Visible;
        Ok(session)
    }

    /// Runs the set log level operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_log_level(
        &mut self,
        pane_id: &str,
        level: AgentLogLevel,
    ) -> Result<&AgentShellSession> {
        let session = self.session_mut(pane_id)?;
        session.log_level = level;
        Ok(session)
    }

    /// Starts a fresh visible conversation for a pane with no transcript entries.
    ///
    /// The command refuses to switch away from a pane that still has a running
    /// turn so the caller cannot orphan active provider, scheduler, or shell
    /// work under an unreferenced conversation id.
    pub fn start_new_conversation(&mut self, pane_id: &str) -> Result<&AgentShellSession> {
        validate_non_empty("pane id", pane_id)?;
        let log_level = self
            .sessions_by_pane
            .get(pane_id)
            .map(|session| session.log_level)
            .unwrap_or(AgentLogLevel::Normal);
        if let Some(session) = self.sessions_by_pane.get(pane_id)
            && session.running_turn_id.is_some()
        {
            return Err(MezError::conflict(
                "cannot start a new conversation while an agent turn is running",
            ));
        }
        self.sessions_by_pane.insert(
            pane_id.to_string(),
            AgentShellSession {
                session_id: new_agent_session_uuid(),
                pane_id: pane_id.to_string(),
                visibility: AgentShellVisibility::Visible,
                running_turn_id: None,
                transcript_entries: 0,
                log_level,
            },
        );
        self.get(pane_id)
            .ok_or_else(|| MezError::invalid_state("new agent shell conversation was not retained"))
    }

    /// Runs the get operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn get(&self, pane_id: &str) -> Option<&AgentShellSession> {
        self.sessions_by_pane.get(pane_id)
    }

    /// Runs the sessions operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn sessions(&self) -> impl Iterator<Item = &AgentShellSession> {
        self.sessions_by_pane.values()
    }

    /// Runs the session mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn session_mut(&mut self, pane_id: &str) -> Result<&mut AgentShellSession> {
        self.sessions_by_pane.get_mut(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent shell session not found for pane",
            )
        })
    }
}

/// Runs the new agent session uuid operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn new_agent_session_uuid() -> String {
    let mut bytes: [u8; 16] = rand::random();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

/// Runs the agent shell help display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_shell_help_display() -> String {
    let mut specs = baseline_slash_commands();
    specs.sort_by(|left, right| {
        agent_shell_command_category(left.name)
            .cmp(agent_shell_command_category(right.name))
            .then_with(|| left.name.cmp(right.name))
    });
    let name_width = specs
        .iter()
        .map(|spec| {
            let aliases = agent_shell_help_alias_suffix(spec.aliases);
            format!("/{}{}", spec.name, aliases).len()
        })
        .max()
        .unwrap_or(0);
    let mut lines = vec![
        "agent shell commands".to_string(),
        String::new(),
        "commands run inside the pane-local agent shell.".to_string(),
    ];
    let mut current_category = "";
    for spec in specs {
        let category = agent_shell_command_category(spec.name);
        if category != current_category {
            lines.push(String::new());
            lines.push(category.to_string());
            current_category = category;
        }
        let aliases = agent_shell_help_alias_suffix(spec.aliases);
        let name = format!("/{}{}", spec.name, aliases);
        lines.push(format!(
            "  {name:<name_width$}  {}",
            agent_shell_command_description(spec.name)
        ));
    }
    lines.join("\n")
}

/// Returns the help category for one slash command.
fn agent_shell_command_category(name: &str) -> &'static str {
    match name {
        "copy"
        | "copy-context"
        | "copy-patches"
        | "copy-trace-log"
        | "debug-config"
        | "diff"
        | "list-modified-files" => "copy and diagnostics",
        "approval" | "approve" | "auto-reasoning" | "init" | "list-mcp" | "log-level"
        | "logout" | "model" | "permissions" | "personality" | "trust" => "configuration",
        "help" | "list-sessions" | "list-skills" => "discovery",
        _ => "work control",
    }
}

/// Formats aliases for one slash-command help row.
fn agent_shell_help_alias_suffix(aliases: &[&str]) -> String {
    if aliases.is_empty() {
        String::new()
    } else {
        format!(" ({})", aliases.join(", "))
    }
}

/// Returns the human-readable description for one slash command.
fn agent_shell_command_description(name: &str) -> &'static str {
    match name {
        "help" => "show this command guide.",
        "permissions" => "inspect permission preset and approval policy.",
        "approval" => "inspect or change the session approval mode.",
        "approve" => "approve a pending pane-local agent action.",
        "trust" => "inspect or decide pending project trust requests.",
        "list-sessions" => "list resumable saved agent conversations.",
        "list-skills" => "list available skills and their $skill prompt names.",
        "list-modified-files" => "list files modified by this agent conversation.",
        "copy-context" => "copy the current model request context.",
        "copy-trace-log" => "copy the retained pane agent trace log.",
        "copy-patches" => "copy retained apply_patch payloads and statuses.",
        "clear" => "clear the visible conversation and terminal view.",
        "compact" => "summarize older transcript context and keep a raw tail.",
        "copy" => "copy the latest model say text.",
        "diff" => "show the current working tree diff.",
        "exit" => "hide the agent shell after stopping active work.",
        "init" => "generate a project instruction scaffold.",
        "logout" => "log out of a provider account.",
        "list-mcp" => "list configured MCP servers and tools.",
        "model" => "inspect or change model and reasoning settings.",
        "auto-reasoning" => "toggle pane-local automatic model sizing.",
        "personality" => "inspect or change response personality.",
        "resume" => "resume a saved conversation.",
        "fork" => "fork the current conversation into a new thread.",
        "new" => "start a fresh conversation in this pane.",
        "status" => "show the current agent shell session status.",
        "stop" => "stop the active agent turn.",
        "statusline" => "inspect or change pane agent statusline behavior.",
        "title" => "set or clear the pane title.",
        "log-level" => "inspect or change pane agent log verbosity.",
        "debug-config" => "inspect parsed slash-command config behavior.",
        _ => "run the slash command.",
    }
}

/// Runs the agent shell status display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_shell_status_display(session: &AgentShellSession) -> String {
    format!(
        "pane: {}\nsession: {}\nvisibility: {}\nrunning turn: {}\ntranscript entries: {}\nlog level: {}",
        session.pane_id,
        session.session_id,
        agent_shell_visibility_name(session.visibility),
        session.running_turn_id.as_deref().unwrap_or("none"),
        session.transcript_entries,
        session.log_level.as_str()
    )
}

/// Runs the agent shell permissions display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_shell_permissions_display(policy: &PermissionPolicy) -> String {
    format!(
        "preset={} approval_policy={} bypass={} command_rules={} source=runtime-policy",
        permission_preset_name(policy.preset),
        approval_policy_name(policy.approval_policy),
        policy.approval_bypass(),
        policy.rules().len()
    )
}

/// Runs the permission preset name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn permission_preset_name(preset: crate::permissions::PermissionPreset) -> &'static str {
    match preset {
        crate::permissions::PermissionPreset::ReadOnly => "read-only",
        crate::permissions::PermissionPreset::Auto => "auto",
    }
}

/// Runs the approval policy name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn approval_policy_name(policy: crate::permissions::ApprovalPolicy) -> &'static str {
    match policy {
        crate::permissions::ApprovalPolicy::Ask => "ask",
        crate::permissions::ApprovalPolicy::AutoAllow => "auto-allow",
        crate::permissions::ApprovalPolicy::FullAccess => "full-access",
    }
}

/// Runs the agent shell mcp display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_shell_mcp_display(registry: &McpRegistry) -> String {
    let server_states = registry.list_servers();
    let tools = server_states
        .iter()
        .map(|server| server.tools.len())
        .sum::<usize>();
    let mut lines = Vec::new();
    lines.push("## MCP Servers".to_string());
    lines.push(String::new());
    lines.push(format!("Servers: {}", server_states.len()));
    lines.push(format!("Tools: {tools}"));
    lines.push("Source: runtime-mcp".to_string());
    if server_states.is_empty() {
        lines.push(String::new());
        lines.push("No MCP servers are configured.".to_string());
    } else {
        for server in server_states {
            lines.push(String::new());
            agent_shell_mcp_server_lines(server, &mut lines);
        }
    }
    lines.join("\n")
}

/// Appends one MCP server as human-readable `/list-mcp` display lines.
fn agent_shell_mcp_server_lines(server: &McpServerState, lines: &mut Vec<String>) {
    let status = agent_shell_mcp_server_status_name(server.status);
    let state = agent_shell_mcp_server_state_name(server);
    let session_blacklisted = server.status == McpServerStatus::Blacklisted;
    let blacklisted = session_blacklisted || server.blacklist_reason.is_some();
    let retryable = server.configured.enabled
        && matches!(
            server.status,
            McpServerStatus::Unavailable | McpServerStatus::Blacklisted | McpServerStatus::Failed
        );
    lines.push(format!(
        "### `{}` - {}",
        server.configured.id,
        agent_shell_mcp_display_text(&server.configured.name)
    ));
    lines.push(format!("- State: {state}"));
    lines.push(format!("- Status: {status}"));
    lines.push(format!("- Enabled: {}", server.configured.enabled));
    lines.push(format!(
        "- Transport: {}",
        agent_shell_mcp_server_kind_name(server.configured.kind)
    ));
    lines.push(format!("- Blacklisted: {blacklisted}"));
    lines.push(format!("- Session blacklisted: {session_blacklisted}"));
    lines.push(format!("- Retryable: {retryable}"));
    if let Some(reason) = server.blacklist_reason.as_deref() {
        lines.push(format!(
            "- Reason: {}",
            agent_shell_mcp_display_text(reason)
        ));
    } else if !server.configured.enabled {
        lines.push("- Reason: disabled".to_string());
    }
    agent_shell_mcp_tool_lines(server, lines);
}

/// Appends one MCP server's tools as readable `/list-mcp` display lines.
fn agent_shell_mcp_tool_lines(server: &McpServerState, lines: &mut Vec<String>) {
    if server.tools.is_empty() {
        lines.push("- Tools: none".to_string());
        return;
    }
    lines.push("- Tools:".to_string());
    for tool in &server.tools {
        lines.push(format!(
            "  - `{}`: state={}, approval={}, permission_required={}, effects={}, description={}",
            tool.name,
            agent_shell_mcp_tool_state_name(server, tool),
            agent_shell_mcp_approval_name(tool.approval),
            tool.permission_required,
            agent_shell_mcp_effects_summary(tool.effects),
            agent_shell_mcp_display_text(&tool.description)
        ));
    }
}

/// Normalizes free-form MCP display text for single-line markdown output.
fn agent_shell_mcp_display_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Returns the normalized MCP transport kind name used in command output.
fn agent_shell_mcp_server_kind_name(kind: McpServerKind) -> &'static str {
    match kind {
        McpServerKind::Stdio => "stdio",
        McpServerKind::Http => "streamable-http",
    }
}

/// Returns the user-facing MCP server state, with disabled config taking precedence.
fn agent_shell_mcp_server_state_name(server: &McpServerState) -> &'static str {
    if !server.configured.enabled {
        return "disabled";
    }
    match server.status {
        McpServerStatus::Configured => "enabled",
        McpServerStatus::Starting => "starting",
        McpServerStatus::Available => "available",
        McpServerStatus::Unavailable => "unavailable",
        McpServerStatus::Blacklisted => "blacklisted",
        McpServerStatus::Failed => "failed",
    }
}

/// Returns the normalized MCP status name used in command output.
fn agent_shell_mcp_server_status_name(status: McpServerStatus) -> &'static str {
    match status {
        McpServerStatus::Configured => "configured",
        McpServerStatus::Starting => "starting",
        McpServerStatus::Available => "available",
        McpServerStatus::Unavailable => "unavailable",
        McpServerStatus::Blacklisted => "blacklisted",
        McpServerStatus::Failed => "failed",
    }
}

/// Returns the effective MCP tool state, with configured disables before runtime state.
fn agent_shell_mcp_tool_state_name(server: &McpServerState, tool: &McpToolState) -> &'static str {
    if !server.configured.tool_allowed_by_config(&tool.name) {
        "disabled"
    } else if tool.blacklisted {
        "blacklisted"
    } else if tool.available {
        "available"
    } else {
        "unavailable"
    }
}

/// Returns the normalized MCP approval setting name used in command output.
fn agent_shell_mcp_approval_name(approval: McpApprovalSetting) -> &'static str {
    match approval {
        McpApprovalSetting::Inherit => "inherit",
        McpApprovalSetting::Prompt => "prompt",
        McpApprovalSetting::Allow => "allow",
        McpApprovalSetting::Deny => "deny",
    }
}

/// Summarizes MCP tool effects as a compact comma-separated command field.
fn agent_shell_mcp_effects_summary(effects: McpToolEffects) -> String {
    let mut names = Vec::new();
    if effects.reads_filesystem {
        names.push("read-fs");
    }
    if effects.mutates_filesystem {
        names.push("mutate-fs");
    }
    if effects.executes_processes {
        names.push("execute-process");
    }
    if effects.accesses_credentials {
        names.push("credential-access");
    }
    if effects.uses_network {
        names.push("network");
    }
    if effects.has_side_effects {
        names.push("side-effects");
    }
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(",")
    }
}

/// Runs the agent shell visibility name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_shell_visibility_name(visibility: AgentShellVisibility) -> &'static str {
    match visibility {
        AgentShellVisibility::Hidden => "hidden",
        AgentShellVisibility::Visible => "visible",
        AgentShellVisibility::HidePendingTaskCompletion => "hide-pending-task-completion",
    }
}
