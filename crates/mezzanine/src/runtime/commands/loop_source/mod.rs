//! Original loop history on the existing deferred-command worker.
//!
//! Acceptance captures a binding and finite source permit without source I/O.
//! The worker selects one leased epoch before any conversation switch. Only
//! the actor may restore a stale controller and admit the new loop, after exact
//! binding/config/process/controller checks. Logical cancellation releases no
//! physical read capacity while any worker still owns its captured work.

use super::*;
use crate::runtime::control::{
    RuntimeAgentPromptHistoryWork, RuntimeAgentTranscriptContext, RuntimeFrozenReplayHistory,
};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct LoopSourceWork {
    pub(crate) operation: u64,
    inner: Arc<Inputs>,
}

struct Inputs {
    pane: String,
    input: String,
    binding: Binding,
    frozen: Option<RuntimeFrozenReplayHistory>,
    stale: Option<RuntimeAgentLoopState>,
    config: u64,
    layers: Vec<crate::config::ConfigLayer>,
    profiles: crate::runtime::service_state::RuntimeModelProfileOverrideStore,
    objective_conversation: Option<String>,
    process: Option<crate::runtime::processes::RuntimePaneProcessIdentity>,
    store: Option<crate::storage::transcript::AgentTranscriptStore>,
    history: Option<RuntimeAgentPromptHistoryWork>,
    source_frontier: u64,
    /// Original parent and optional count-only source, at most two entries.
    /// A later binding cannot redirect the adoption-time chronology fence.
    admission_conversations: Vec<String>,
    _permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
}

/// Descriptive freshness fields only. The worker retains no shell task handles,
/// peer incarnation, prompt draft/history or unrelated session settings.
struct Binding {
    session_id: String,
    running_turn_id: Option<String>,
    ephemeral: bool,
    transcript_entries: u64,
    transcript_high_water: u64,
    ephemeral_transcript_source_conversation_id: Option<String>,
    ephemeral_transcript_source_entries: u64,
    directory_generation: u64,
    visibility: AgentShellVisibility,
    allowed_actions: Option<AllowedActionSet>,
}
impl Binding {
    fn capture(session: &mez_agent::AgentShellSession) -> Self {
        Self {
            session_id: session.session_id.clone(),
            running_turn_id: session.running_turn_id.clone(),
            ephemeral: session.ephemeral,
            transcript_entries: session.transcript_entries,
            transcript_high_water: session.transcript_high_water,
            ephemeral_transcript_source_conversation_id: session
                .ephemeral_transcript_source_conversation_id
                .clone(),
            ephemeral_transcript_source_entries: session.ephemeral_transcript_source_entries,
            directory_generation: session.native_working_directory_generation(),
            visibility: session.visibility,
            allowed_actions: session.allowed_actions.clone(),
        }
    }
}

/// Opaque worker result. A plain checkpoint or successful-looking response
/// cannot fabricate the original read's association with its accepted owner.
pub(crate) struct LoopSourceRead {
    owner: LoopSourceWork,
    frozen: Option<RuntimeFrozenReplayHistory>,
    history: Option<RuntimeAgentTranscriptContext>,
    objective: Option<String>,
}

/// Immutable legacy loop projection, not a synthetic typed head. The loop
/// compares the original allocation for stale-controller fencing and reuses
/// its exact history for isolated iterations without reading the parent again.
#[derive(Clone)]
pub(crate) struct RuntimeLoopHistory(Arc<RuntimeAgentTranscriptContext>);
impl std::fmt::Debug for RuntimeLoopHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RuntimeLoopHistory")
    }
}
impl PartialEq for RuntimeLoopHistory {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for RuntimeLoopHistory {}
impl RuntimeLoopHistory {
    pub(super) fn new(history: RuntimeAgentTranscriptContext) -> Self {
        Self(Arc::new(history))
    }
    pub(super) fn history(&self) -> RuntimeAgentTranscriptContext {
        (*self.0).clone()
    }
}

impl std::fmt::Debug for LoopSourceWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopSourceWork")
            .field("pane", &self.inner.pane)
            .field("operation", &self.operation)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for LoopSourceRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopSourceRead")
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

impl LoopSourceWork {
    pub(crate) fn execute(&self) -> Result<LoopSourceRead> {
        let objective = self
            .inner
            .objective_conversation
            .as_ref()
            .map(|conversation| {
                self.inner
                    .store
                    .as_ref()
                    .map(|store| store.user_objective(conversation))
                    .transpose()
                    .map(Option::flatten)
            })
            .transpose()?
            .flatten();
        let prepared = self
            .inner
            .history
            .clone()
            .map(|history| history.execute_loop_history(self.inner.source_frontier))
            .transpose()?;
        let (history, frozen) =
            prepared.map_or((None, None), |(history, frozen)| (Some(history), frozen));
        Ok(LoopSourceRead {
            owner: self.clone(),
            frozen,
            history,
            objective,
        })
    }

    /// Checks the retained allocation, not equality of mutable rendered text.
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        self.operation == other.operation && Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl RuntimeSessionService {
    /// Rechecks actor-owned chronology custody without any source I/O. These
    /// same original conversations fence both capture and late-result adoption.
    fn check_loop_source_conversations(&self, conversations: &[String]) -> Result<()> {
        if conversations.iter().any(|conversation| {
            self.persistence.bookkeeping_pending(conversation)
                || self.canonical_message_receipt_pending_for_conversation(conversation)
        }) {
            return Err(MezError::conflict(
                "conversation bookkeeping must settle before loop source admission",
            ));
        }
        Ok(())
    }

    /// Captures bounded actor-owned inputs. A synchronous direct-service caller
    /// supplies no permit because no detached worker outlives that call; every
    /// deferred admission reserves the existing source pool before acceptance.
    pub(crate) fn capture_loop_source_work(
        &self,
        pane: &str,
        input: &str,
        permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    ) -> Result<LoopSourceWork> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("loop requires slash input"))?;
        let parsed = parse_agent_loop_args(invocation.args.trim())?;
        if parsed.original_prompt.is_empty() {
            return Err(MezError::invalid_args("/loop requires a non-empty prompt"));
        }
        if self.pane_has_active_agent_loop_turn(pane) {
            return Err(MezError::conflict(
                "an agent loop is already active for this pane",
            ));
        }
        let binding = self
            .agent_shell_store()
            .get(pane)
            .cloned()
            .ok_or_else(|| MezError::conflict("loop parent session unavailable"))?;
        let frozen = self
            .frozen_replay_binding(pane, &binding.session_id)
            .cloned();
        let stale = self.agent_loop_state(pane).cloned();
        let mut parent = binding.clone();
        let mut parent_frozen = frozen.clone();
        if let Some(state) = &stale {
            parent.session_id = state.parent_conversation_id.clone();
            parent.transcript_entries = state.parent_transcript_entries;
            parent.transcript_high_water = state.parent_transcript_high_water;
            parent.ephemeral = state.parent_no_store_source.is_some();
            parent.ephemeral_transcript_source_conversation_id = state
                .parent_no_store_source
                .as_ref()
                .and_then(|source| source.conversation_id.clone());
            parent.ephemeral_transcript_source_entries = state
                .parent_no_store_source
                .as_ref()
                .map_or(0, |source| source.entries);
            parent_frozen = state
                .parent_frozen_replay
                .clone()
                .filter(|_| parent.ephemeral);
        }
        let mut admission_conversations = vec![parent.session_id.clone()];
        if parent_frozen.is_none()
            && let Some(source) = &parent.ephemeral_transcript_source_conversation_id
            && source != &parent.session_id
        {
            admission_conversations.push(source.clone());
        }
        self.check_loop_source_conversations(&admission_conversations)?;
        let source_frontier = if parent.ephemeral {
            parent.ephemeral_transcript_source_entries
        } else {
            parent.transcript_high_water
        };
        let history = (parsed.mode != RuntimeAgentLoopMode::NewEachIteration || parent.ephemeral)
            .then(|| self.prepare_prompt_history_for_binding(pane, Some(&parent), parent_frozen));
        let objective_conversation =
            (parsed.mode != RuntimeAgentLoopMode::ReuseCurrentConversation || stale.is_some())
                .then(|| parent.session_id.clone());
        Ok(LoopSourceWork {
            operation: 0,
            inner: Arc::new(Inputs {
                pane: pane.into(),
                input: input.into(),
                binding: Binding::capture(&binding),
                frozen,
                stale,
                config: self.session.config_generation,
                layers: self.integration.config_layers().to_vec(),
                profiles: self.integration.model_profile_overrides().clone(),
                objective_conversation,
                process: self.steering_process_binding(pane)?,
                store: self.persistence.cloned_transcript_store(),
                history,
                source_frontier,
                admission_conversations,
                _permit: permit,
            }),
        })
    }

    pub(crate) fn validate_loop_source_work(&self, work: &LoopSourceWork) -> Result<()> {
        let original = &work.inner;
        let current = self
            .agent_shell_store()
            .get(&original.pane)
            .ok_or_else(|| MezError::conflict("loop source pane was removed"))?;
        let expected = &original.binding;
        if current.session_id != expected.session_id
            || current.running_turn_id != expected.running_turn_id
            || current.ephemeral != expected.ephemeral
            || current.transcript_entries != expected.transcript_entries
            || current.transcript_high_water != expected.transcript_high_water
            || current.ephemeral_transcript_source_conversation_id
                != expected.ephemeral_transcript_source_conversation_id
            || current.ephemeral_transcript_source_entries
                != expected.ephemeral_transcript_source_entries
            || current.native_working_directory_generation() != expected.directory_generation
            || current.visibility != expected.visibility
            || current.allowed_actions != expected.allowed_actions
            || self.frozen_replay_binding(&original.pane, &expected.session_id)
                != original.frozen.as_ref()
            || self.agent_loop_state(&original.pane) != original.stale.as_ref()
            || self.session.config_generation != original.config
            || self.integration.config_layers() != original.layers.as_slice()
            || self.integration.model_profile_overrides() != &original.profiles
            || self.persistence.transcript_store() != original.store.as_ref()
        {
            return Err(MezError::conflict("loop source preparation became stale"));
        }
        self.check_loop_source_conversations(&original.admission_conversations)?;
        let current = self.steering_process_binding(&original.pane)?;
        if !match (original.process.as_ref(), current.as_ref()) {
            (Some(old), Some(current)) => old.same_incarnation(current),
            (None, None) => true,
            _ => false,
        } {
            return Err(MezError::conflict("loop source pane incarnation changed"));
        }
        Ok(())
    }

    /// No source lookup is allowed after the worker result reaches this owner.
    pub(crate) fn adopt_loop_source(
        &mut self,
        work: &LoopSourceWork,
        read: &LoopSourceRead,
    ) -> Result<AgentShellCommandOutcome> {
        if !work.same_owner(&read.owner) {
            return Err(MezError::conflict(
                "loop source result has a different owner",
            ));
        }
        if work.operation != 0
            && (!self.agent.agent_command_is_claimed(
                &work.inner.pane,
                &work.inner.binding.session_id,
                work.operation,
            ) || !self
                .agent
                .loop_source_work(&work.inner.pane, work.operation)
                .is_some_and(|actual| actual.same_owner(work)))
        {
            return Err(MezError::conflict(
                "loop source command was cancelled or replaced",
            ));
        }
        self.validate_loop_source_work(work)?;
        let owner = &work.inner;
        if let Some(state) = &owner.stale {
            self.restore_agent_loop_parent_conversation(
                &owner.pane,
                state,
                read.objective.as_deref(),
            )?;
            self.remove_agent_loop_state_by_id(&state.loop_id);
            self.clear_agent_loop_turns_for_pane(&owner.pane);
        }
        self.start_agent_loop_with_source(
            &owner.pane,
            &owner.input,
            read.frozen.clone(),
            read.history.clone(),
            read.objective.as_deref(),
        )
    }

    pub(crate) fn loop_source_command_response(
        &mut self,
        work: &crate::runtime::RuntimeAgentCommandAsyncWork,
        read: &LoopSourceRead,
    ) -> crate::runtime::RuntimeAgentCommandAsyncOutcome {
        use crate::runtime::{
            RuntimeAgentCommandAsyncOutcome as Outcome, RuntimeAgentCommandPrepared as Prepared,
        };
        let result = match &work.prepared {
            Prepared::LoopSource(source) => self.adopt_loop_source(source, read),
            _ => Err(MezError::conflict(
                "loop result reached another command family",
            )),
        };
        match result {
            Ok(outcome) => Outcome::Response {
                body: runtime_agent_shell_command_response_json(
                    &work.pane_id,
                    &work.input,
                    Some(&outcome),
                ),
            },
            Err(error) => Outcome::Failed {
                message: error.message().into(),
                kind: error.kind(),
            },
        }
    }
}
