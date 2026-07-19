//! Provider-independent agent-shell session state and display policy.
//!
//! This module owns the agent session boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use std::collections::BTreeMap;

use crate::{
    AgentShellMcpServerSummary, AgentShellMcpSummary, AgentShellPermissionSummary,
    AgentShellSessionError, AgentShellSessionResult, baseline_slash_commands,
    validate_agent_shell_required,
};

// Agent shell sessions, stores, and shell display helpers.

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
    /// Stores the prompt-cache lineage id value for this data structure.
    ///
    /// The lineage remains stable across resume or inherited fork flows so
    /// provider prompt caching can continue from the same observed prefix even
    /// when the runtime conversation or session id changes.
    pub prompt_cache_lineage_id: String,
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
    /// Stores the session directive value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub directive: Option<String>,
    /// Whether this pane binding is temporary runtime-only state.
    ///
    /// Ephemeral conversations are used by loop-owned fork attempts. They may
    /// receive live model context and terminal output, but must not be saved as
    /// resumable agent sessions or checkpointed as active pane bindings.
    pub ephemeral: bool,
    /// Durable conversation whose transcript should seed an ephemeral turn.
    pub ephemeral_transcript_source_conversation_id: Option<String>,
    /// Number of durable source transcript entries available for ephemeral context.
    pub ephemeral_transcript_source_entries: u64,
}

/// Persistence policy for one pane conversation binding.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentShellConversationPersistence {
    /// Whether this binding is runtime-only.
    ephemeral: bool,
    /// Durable conversation id used to seed ephemeral transcript context.
    transcript_source_conversation_id: Option<String>,
    /// Number of durable transcript entries available from the source.
    transcript_source_entries: u64,
}

impl AgentShellConversationPersistence {
    /// Builds the policy for a durable, resumable pane conversation binding.
    fn durable() -> Self {
        Self {
            ephemeral: false,
            transcript_source_conversation_id: None,
            transcript_source_entries: 0,
        }
    }

    /// Builds the policy for a runtime-only conversation binding.
    fn ephemeral(
        transcript_source_conversation_id: Option<String>,
        transcript_source_entries: u64,
    ) -> Self {
        Self {
            ephemeral: true,
            transcript_source_conversation_id,
            transcript_source_entries,
        }
    }
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
    pub fn ensure_session(
        &mut self,
        pane_id: impl Into<String>,
    ) -> AgentShellSessionResult<&mut AgentShellSession> {
        let pane_id = pane_id.into();
        validate_agent_shell_required("pane id", &pane_id)?;
        if !self.sessions_by_pane.contains_key(&pane_id) {
            self.sessions_by_pane.insert(
                pane_id.clone(),
                AgentShellSession {
                    session_id: new_agent_session_uuid(),
                    pane_id: pane_id.clone(),
                    prompt_cache_lineage_id: new_agent_session_uuid(),
                    visibility: AgentShellVisibility::Hidden,
                    running_turn_id: None,
                    transcript_entries: 0,
                    log_level: AgentLogLevel::Normal,
                    directive: None,
                    ephemeral: false,
                    ephemeral_transcript_source_conversation_id: None,
                    ephemeral_transcript_source_entries: 0,
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
    pub fn enter_or_resume(
        &mut self,
        pane_id: impl Into<String>,
    ) -> AgentShellSessionResult<&AgentShellSession> {
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
    pub fn request_exit(&mut self, pane_id: &str) -> AgentShellSessionResult<&AgentShellSession> {
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
    ) -> AgentShellSessionResult<&AgentShellSession> {
        let session = self.session_mut(pane_id)?;
        session.visibility = AgentShellVisibility::HidePendingTaskCompletion;
        Ok(session)
    }

    /// Runs the start turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn start_turn(
        &mut self,
        pane_id: &str,
        turn_id: impl Into<String>,
    ) -> AgentShellSessionResult<()> {
        let turn_id = turn_id.into();
        validate_agent_shell_required("turn id", &turn_id)?;
        let session = self.session_mut(pane_id)?;
        if session.running_turn_id.is_some() {
            return Err(AgentShellSessionError::conflict(
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
    pub fn finish_turn(
        &mut self,
        pane_id: &str,
        turn_id: &str,
    ) -> AgentShellSessionResult<&AgentShellSession> {
        let session = self.session_mut(pane_id)?;
        if session.running_turn_id.as_deref() != Some(turn_id) {
            return Err(AgentShellSessionError::invalid_args(
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
    ) -> AgentShellSessionResult<&AgentShellSession> {
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
    ) -> AgentShellSessionResult<&AgentShellSession> {
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
    ) -> AgentShellSessionResult<&AgentShellSession> {
        self.bind_conversation_with_lineage(pane_id, conversation_id, transcript_entries, None)
    }

    /// Binds a pane to one conversation while optionally overriding prompt-cache
    /// lineage for inherited fork or restore flows.
    pub fn bind_conversation_with_lineage(
        &mut self,
        pane_id: &str,
        conversation_id: impl Into<String>,
        transcript_entries: u64,
        prompt_cache_lineage_id: Option<String>,
    ) -> AgentShellSessionResult<&AgentShellSession> {
        self.bind_conversation_with_lineage_and_persistence(
            pane_id,
            conversation_id,
            transcript_entries,
            prompt_cache_lineage_id,
            AgentShellConversationPersistence::durable(),
        )
    }

    /// Restores a durable conversation while preserving one known running turn.
    ///
    /// Runtime-managed routed loops temporarily bind their invoking pane to an
    /// ephemeral attempt before transferring execution to a worker. The
    /// blocked parent turn remains the shell owner for final presentation, so
    /// restoration must preserve that exact turn id while replacing only the
    /// conversation binding. Any missing or different running turn is rejected
    /// to prevent callers from orphaning unrelated provider work.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose managed parent turn owns the shell session.
    /// - `expected_running_turn_id`: Exact routed parent turn that must remain bound.
    /// - `conversation_id`: Durable parent conversation to restore.
    /// - `transcript_entries`: Parent transcript high-water mark.
    /// - `prompt_cache_lineage_id`: Optional lineage captured before the attempt.
    ///
    /// # Errors
    /// Returns an error when identifiers are empty, the pane session is absent,
    /// or a different (or no) running turn owns the pane.
    pub fn restore_conversation_for_running_turn_with_lineage(
        &mut self,
        pane_id: &str,
        expected_running_turn_id: &str,
        conversation_id: impl Into<String>,
        transcript_entries: u64,
        prompt_cache_lineage_id: Option<String>,
    ) -> AgentShellSessionResult<&AgentShellSession> {
        validate_agent_shell_required("running turn id", expected_running_turn_id)?;
        let conversation_id = conversation_id.into();
        validate_agent_shell_required("conversation id", &conversation_id)?;
        if let Some(lineage_id) = prompt_cache_lineage_id.as_deref() {
            validate_agent_shell_required("prompt cache lineage id", lineage_id)?;
        }
        let session = self.session_mut(pane_id)?;
        if session.running_turn_id.as_deref() != Some(expected_running_turn_id) {
            return Err(AgentShellSessionError::conflict(
                "cannot restore a running conversation for a different turn",
            ));
        }
        session.session_id = conversation_id;
        if let Some(lineage_id) = prompt_cache_lineage_id {
            session.prompt_cache_lineage_id = lineage_id;
        }
        session.transcript_entries = transcript_entries;
        session.ephemeral = false;
        session.ephemeral_transcript_source_conversation_id = None;
        session.ephemeral_transcript_source_entries = 0;
        session.visibility = AgentShellVisibility::Visible;
        Ok(session)
    }

    /// Binds a pane to one runtime-only conversation while optionally
    /// inheriting prompt-cache lineage from its parent.
    ///
    /// Ephemeral bindings are intentionally excluded from durable transcript
    /// and active-session metadata persistence. They are suitable for
    /// throw-away loop attempts whose visible parent conversation should remain
    /// the resumable session.
    pub fn bind_ephemeral_conversation_with_lineage(
        &mut self,
        pane_id: &str,
        conversation_id: impl Into<String>,
        transcript_entries: u64,
        prompt_cache_lineage_id: Option<String>,
    ) -> AgentShellSessionResult<&AgentShellSession> {
        self.bind_ephemeral_conversation_with_lineage_and_transcript_source(
            pane_id,
            conversation_id,
            transcript_entries,
            prompt_cache_lineage_id,
            None,
            0,
        )
    }

    /// Binds a pane to a runtime-only conversation seeded from a durable source
    /// transcript.
    pub fn bind_ephemeral_conversation_with_lineage_and_transcript_source(
        &mut self,
        pane_id: &str,
        conversation_id: impl Into<String>,
        transcript_entries: u64,
        prompt_cache_lineage_id: Option<String>,
        transcript_source_conversation_id: Option<String>,
        transcript_source_entries: u64,
    ) -> AgentShellSessionResult<&AgentShellSession> {
        self.bind_conversation_with_lineage_and_persistence(
            pane_id,
            conversation_id,
            transcript_entries,
            prompt_cache_lineage_id,
            AgentShellConversationPersistence::ephemeral(
                transcript_source_conversation_id,
                transcript_source_entries,
            ),
        )
    }

    /// Binds a pane to one conversation and records whether the binding is
    /// durable or runtime-only.
    fn bind_conversation_with_lineage_and_persistence(
        &mut self,
        pane_id: &str,
        conversation_id: impl Into<String>,
        transcript_entries: u64,
        prompt_cache_lineage_id: Option<String>,
        persistence: AgentShellConversationPersistence,
    ) -> AgentShellSessionResult<&AgentShellSession> {
        let conversation_id = conversation_id.into();
        validate_agent_shell_required("conversation id", &conversation_id)?;
        if let Some(source_conversation_id) = persistence.transcript_source_conversation_id.as_ref()
        {
            validate_agent_shell_required(
                "transcript source conversation id",
                source_conversation_id,
            )?;
        }
        let session = self.session_mut(pane_id)?;
        if session.running_turn_id.is_some() {
            return Err(AgentShellSessionError::conflict(
                "cannot switch conversations while an agent turn is running",
            ));
        }
        session.session_id = conversation_id;
        if let Some(lineage_id) = prompt_cache_lineage_id {
            validate_agent_shell_required("prompt cache lineage id", &lineage_id)?;
            session.prompt_cache_lineage_id = lineage_id;
        }
        session.transcript_entries = transcript_entries;
        session.ephemeral = persistence.ephemeral;
        session.ephemeral_transcript_source_conversation_id = if persistence.ephemeral {
            persistence.transcript_source_conversation_id
        } else {
            None
        };
        session.ephemeral_transcript_source_entries = if persistence.ephemeral {
            persistence.transcript_source_entries
        } else {
            0
        };
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
    ) -> AgentShellSessionResult<&AgentShellSession> {
        let session = self.session_mut(pane_id)?;
        session.log_level = level;
        Ok(session)
    }

    /// Sets or clears the pane-local session directive.
    ///
    /// # Parameters
    /// - `pane_id`: The pane owning the agent shell session.
    /// - `directive`: The optional directive text appended to developer
    ///   instructions for future turns.
    pub fn set_directive(
        &mut self,
        pane_id: &str,
        directive: Option<String>,
    ) -> AgentShellSessionResult<&AgentShellSession> {
        if let Some(text) = directive.as_deref() {
            validate_agent_shell_required("directive", text)?;
        }
        let session = self.session_mut(pane_id)?;
        session.directive = directive;
        Ok(session)
    }

    /// Starts a fresh visible conversation for a pane with no transcript entries.
    ///
    /// The command refuses to switch away from a pane that still has a running
    /// turn so the caller cannot orphan active provider, scheduler, or shell
    /// work under an unreferenced conversation id.
    pub fn start_new_conversation(
        &mut self,
        pane_id: &str,
    ) -> AgentShellSessionResult<&AgentShellSession> {
        validate_agent_shell_required("pane id", pane_id)?;
        let log_level = self
            .sessions_by_pane
            .get(pane_id)
            .map(|session| session.log_level)
            .unwrap_or(AgentLogLevel::Normal);
        if let Some(session) = self.sessions_by_pane.get(pane_id)
            && session.running_turn_id.is_some()
        {
            return Err(AgentShellSessionError::conflict(
                "cannot start a new conversation while an agent turn is running",
            ));
        }
        self.sessions_by_pane.insert(
            pane_id.to_string(),
            AgentShellSession {
                session_id: new_agent_session_uuid(),
                pane_id: pane_id.to_string(),
                prompt_cache_lineage_id: new_agent_session_uuid(),
                visibility: AgentShellVisibility::Visible,
                running_turn_id: None,
                transcript_entries: 0,
                log_level,
                directive: None,
                ephemeral: false,
                ephemeral_transcript_source_conversation_id: None,
                ephemeral_transcript_source_entries: 0,
            },
        );
        self.get(pane_id).ok_or_else(|| {
            AgentShellSessionError::invalid_state("new agent shell conversation was not retained")
        })
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
    pub(super) fn session_mut(
        &mut self,
        pane_id: &str,
    ) -> AgentShellSessionResult<&mut AgentShellSession> {
        self.sessions_by_pane.get_mut(pane_id).ok_or_else(|| {
            AgentShellSessionError::not_found("agent shell session not found for pane")
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
pub fn agent_shell_help_display() -> String {
    let mut specs = baseline_slash_commands();
    specs.sort_by(|left, right| {
        agent_shell_command_category(left.name)
            .cmp(agent_shell_command_category(right.name))
            .then_with(|| left.name.cmp(right.name))
    });
    let mut lines = vec![
        "# Agent shell commands".to_string(),
        String::new(),
        "Commands run inside the pane-local agent shell.".to_string(),
        String::new(),
        "| Category | Command | Description |".to_string(),
        "| --- | --- | --- |".to_string(),
    ];
    let mut current_category = "";
    for spec in specs {
        let category = agent_shell_command_category(spec.name);
        if category != current_category {
            lines.push(format!(
                "| {} |  |  |",
                agent_shell_help_title_case(category)
            ));
            current_category = category;
        }
        let aliases = agent_shell_help_alias_suffix(spec.aliases);
        let name = format!("/{}{}", spec.name, aliases);
        lines.push(format!(
            "|  | `{name}` | {} |",
            agent_shell_command_description(spec.name)
        ));
    }
    lines.join("\n")
}

/// Returns a display heading for one lower-case agent-shell help category.
fn agent_shell_help_title_case(category: &str) -> String {
    category
        .split_whitespace()
        .enumerate()
        .map(|(index, word)| {
            if index > 0 {
                return word.to_string();
            }
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
        "approval" | "approve" | "routing" | "init" | "latency" | "list-mcp" | "log-level"
        | "memory" | "directive" | "logout" | "model" | "permissions" | "personality" | "trust" => {
            "configuration"
        }
        "help" | "list-macros" | "list-sessions" | "list-skills" => "discovery",
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
        "list-macros" => "list available macros and their #macro prompt names.",
        "list-sessions" => "list resumable saved agent conversations.",
        "list-skills" => "list available skills and their $skill prompt names.",
        "sync-builtin-skills" => {
            "synchronize managed built-in skills into the user configuration root."
        }
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
        "issue" => "create, inspect, update, or delete project issue records.",
        "show-context" => "browse and delete entries in the current pane conversation.",
        "show-issues" => "browse project issue records and open issue details.",
        "memory" => "inspect or change persistent memory enablement.",
        "show-memories" => "browse durable memory records and open memory details.",
        "remember" => "generate durable memories from the current context or a statement.",
        "model" => "inspect or change model and reasoning settings.",
        "thinking" => "inspect or toggle pane-local model reasoning visibility.",
        "latency" => "inspect or change latency/cost preference.",
        "routing" => "toggle pane-local automatic model sizing.",
        "directive" => "inspect or set a session-scoped developer-instruction addendum.",
        "personality" => "inspect or change response personality.",
        "loop" => {
            "iterate on a prompt until an iteration completes without apply_patch actions or the loop limit is reached; pass --fork to use fresh parent-conversation forks, --new to use fresh empty conversations, or --limit <int> to override the loop limit for this command."
        }
        "resume" => "resume a saved conversation.",
        "fork" => "fork the current conversation into a new thread.",
        "new" => "start a fresh conversation in this pane.",
        "status" => "show the current agent shell session status.",
        "reset-status" => "reset pane token-accounting statistics.",
        "stop" => "stop the active agent turn.",
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
pub fn agent_shell_status_display(session: &AgentShellSession) -> String {
    format!(
        "pane: {}\nsession: {}\nvisibility: {}\nrunning turn: {}\ntranscript entries: {}\nlog level: {}\ndirective: {}",
        session.pane_id,
        session.session_id,
        agent_shell_visibility_name(session.visibility),
        session.running_turn_id.as_deref().unwrap_or("none"),
        session.transcript_entries,
        session.log_level.as_str(),
        session
            .directive
            .as_deref()
            .map(|directive| directive.replace('\n', "\\n"))
            .unwrap_or_else(|| "none".to_string())
    )
}

/// Runs the agent shell permissions display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn agent_shell_permissions_display(summary: AgentShellPermissionSummary) -> String {
    format!(
        "preset={} approval_policy={} bypass={} command_rules={} source=runtime-policy",
        permission_preset_name(summary.preset),
        approval_policy_name(summary.approval_policy),
        summary.approval_bypass,
        summary.command_rule_count
    )
}

/// Runs the permission preset name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn permission_preset_name(preset: crate::PermissionPreset) -> &'static str {
    preset.as_str()
}

/// Runs the approval policy name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn approval_policy_name(policy: crate::ApprovalPolicy) -> &'static str {
    policy.as_str()
}

/// Runs the agent shell mcp display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn agent_shell_mcp_display(summary: &AgentShellMcpSummary) -> String {
    let tools = summary
        .servers
        .iter()
        .map(|server| server.tools.len())
        .sum::<usize>();
    let mut lines = Vec::new();
    lines.push("## MCP Servers".to_string());
    lines.push(String::new());
    lines.push(format!("Servers: {}", summary.servers.len()));
    lines.push(format!("Tools: {tools}"));
    lines.push("Source: runtime-mcp".to_string());
    if summary.servers.is_empty() {
        lines.push(String::new());
        lines.push("No MCP servers are configured.".to_string());
    } else {
        for server in &summary.servers {
            lines.push(String::new());
            agent_shell_mcp_server_lines(server, &mut lines);
        }
    }
    lines.join("\n")
}

/// Appends one MCP server as human-readable `/list-mcp` display lines.
fn agent_shell_mcp_server_lines(server: &AgentShellMcpServerSummary, lines: &mut Vec<String>) {
    lines.push(format!(
        "### `{}` - {}",
        server.server_id,
        agent_shell_mcp_display_text(&server.display_name)
    ));
    lines.push(format!("- State: {}", server.state));
    lines.push(format!("- Status: {}", server.status));
    lines.push(format!("- Enabled: {}", server.enabled));
    lines.push(format!("- Transport: {}", server.transport));
    lines.push(format!("- Blacklisted: {}", server.blacklisted));
    lines.push(format!(
        "- Session blacklisted: {}",
        server.session_blacklisted
    ));
    lines.push(format!("- Retryable: {}", server.retryable));
    if let Some(reason) = server.reason.as_deref() {
        lines.push(format!(
            "- Reason: {}",
            agent_shell_mcp_display_text(reason)
        ));
    }
    agent_shell_mcp_tool_lines(server, lines);
}

/// Appends one MCP server's tools as readable `/list-mcp` display lines.
fn agent_shell_mcp_tool_lines(server: &AgentShellMcpServerSummary, lines: &mut Vec<String>) {
    if server.tools.is_empty() {
        lines.push("- Tools: none".to_string());
        return;
    }
    lines.push(String::new());
    lines.push("| Tool | State | Approval | Permission | Effects | Description |".to_string());
    lines.push("| --- | --- | --- | --- | --- | --- |".to_string());
    for tool in &server.tools {
        lines.push(format!(
            "| `{}` | {} | {} | {} | {} | {} |",
            tool.name,
            tool.state,
            tool.approval,
            tool.permission_required,
            tool.effects,
            agent_shell_mcp_display_text(&tool.description)
        ));
    }
}

/// Normalizes free-form MCP display text for single-line markdown output.
fn agent_shell_mcp_display_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Runs the agent shell visibility name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn agent_shell_visibility_name(visibility: AgentShellVisibility) -> &'static str {
    match visibility {
        AgentShellVisibility::Hidden => "hidden",
        AgentShellVisibility::Visible => "visible",
        AgentShellVisibility::HidePendingTaskCompletion => "hide-pending-task-completion",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies agent shell rejects mismatched turn completion.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn agent_shell_rejects_mismatched_turn_completion() {
        let mut store = AgentShellStore::default();
        store.enter_or_resume("%1").unwrap();
        store.start_turn("%1", "turn-1").unwrap();

        let error = store.finish_turn("%1", "turn-2").unwrap_err();

        assert_eq!(error.kind(), crate::AgentShellSessionErrorKind::InvalidArgs);
    }

    /// Verifies managed restoration changes only conversation metadata while
    /// preserving the exact routed parent turn that owns the shell session.
    #[test]
    fn agent_shell_restores_conversation_for_expected_running_turn() {
        let mut store = AgentShellStore::default();
        store.enter_or_resume("%1").unwrap();
        store.start_turn("%1", "turn-routed-parent").unwrap();
        let original_lineage = store.get("%1").unwrap().prompt_cache_lineage_id.clone();

        let restored = store
            .restore_conversation_for_running_turn_with_lineage(
                "%1",
                "turn-routed-parent",
                "conversation-parent",
                7,
                Some(original_lineage.clone()),
            )
            .unwrap();

        assert_eq!(
            restored.running_turn_id.as_deref(),
            Some("turn-routed-parent")
        );
        assert_eq!(restored.session_id, "conversation-parent");
        assert_eq!(restored.transcript_entries, 7);
        assert_eq!(restored.prompt_cache_lineage_id, original_lineage);
        assert!(!restored.ephemeral);
        assert!(
            restored
                .ephemeral_transcript_source_conversation_id
                .is_none()
        );
    }

    /// Verifies managed restoration cannot replace a conversation owned by a
    /// different live turn, preserving the ordinary anti-orphaning guard.
    #[test]
    fn agent_shell_rejects_running_conversation_restore_for_wrong_turn() {
        let mut store = AgentShellStore::default();
        store.enter_or_resume("%1").unwrap();
        store.start_turn("%1", "turn-current").unwrap();
        let original_conversation = store.get("%1").unwrap().session_id.clone();

        let error = store
            .restore_conversation_for_running_turn_with_lineage(
                "%1",
                "turn-stale",
                "conversation-parent",
                3,
                None,
            )
            .unwrap_err();

        assert_eq!(error.kind(), crate::AgentShellSessionErrorKind::Conflict);
        assert_eq!(store.get("%1").unwrap().session_id, original_conversation);
        assert_eq!(
            store.get("%1").unwrap().running_turn_id.as_deref(),
            Some("turn-current")
        );
    }

    /// Verifies that hiding an agent shell immediately returns pane input focus
    /// to the user even when a turn continues in the background. Finishing the
    /// turn keeps the same session while transcript state remains tied to
    /// durable transcript writes.
    #[test]
    fn agent_shell_resumes_per_pane_and_hides_immediately_during_running_turn() {
        let mut store = AgentShellStore::default();
        let first_session_id = store.enter_or_resume("%1").unwrap().session_id.to_string();
        assert!(looks_like_uuid_v4(&first_session_id));

        store.start_turn("%1", "turn-1").unwrap();
        let pending = store.request_exit("%1").unwrap();
        assert_eq!(pending.visibility, AgentShellVisibility::Hidden);

        let hidden = store.finish_turn("%1", "turn-1").unwrap();
        assert_eq!(hidden.visibility, AgentShellVisibility::Hidden);
        assert_eq!(hidden.transcript_entries, 0);
        let recorded = store.record_transcript_entries("%1", 3).unwrap();
        assert_eq!(recorded.transcript_entries, 3);

        let resumed = store.enter_or_resume("%1").unwrap();
        assert_eq!(resumed.session_id, first_session_id);
        assert_eq!(resumed.visibility, AgentShellVisibility::Visible);
        assert_eq!(resumed.transcript_entries, 3);

        let other = store.enter_or_resume("%2").unwrap();
        assert!(looks_like_uuid_v4(&other.session_id));
        assert_ne!(other.session_id, first_session_id);
    }

    /// Reports whether one string is a lowercase RFC 4122 UUIDv4.
    fn looks_like_uuid_v4(value: &str) -> bool {
        let bytes = value.as_bytes();
        bytes.len() == 36
            && bytes[8] == b'-'
            && bytes[13] == b'-'
            && bytes[18] == b'-'
            && bytes[23] == b'-'
            && bytes[14] == b'4'
            && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
            && bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_hexdigit())
    }
}
