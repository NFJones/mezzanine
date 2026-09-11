//! Bounded generated session-title scheduling for agent conversations.
//!
//! Title generation is a side channel: it never creates or claims a turn, never
//! appends to the live transcript, never moves prompt-cache lineage, and never
//! changes turn state. This module owns the controller-side policy: when one
//! conversation may spend a provider request, how many may be in flight, how many
//! attempts are allowed, and which bounded reason is recorded when generation
//! fails so the row falls back to the objective-derived title.
//!
//! Admission is a pure function so every refusal rule is testable without a
//! provider, and the per-conversation bookkeeping is a plain bounded map keyed by
//! conversation id. Provider execution itself belongs to the host provider worker,
//! which receives the prepared request through the runtime dispatch path; the
//! actor keeps ownership of scheduling, the attempt limit, settlement, and
//! cancellation.

use std::collections::BTreeMap;

use mez_agent::session_title::{SessionTitleGenerationInputs, session_title_request};

use crate::error::Result as MezResult;
use crate::runtime::runtime_effective_config_value;
use crate::runtime::{
    AgentSessionTitleEvent, AgentSessionTitleOutcome, RuntimeAgentSessionTitleDispatch,
    RuntimeSideEffect, RuntimeTransition,
};
use crate::session_title::{SessionTitleFailureReason, SessionTitlePolicy};

use super::{
    EventKind, ModelProfile, RuntimeAgentSessionTitleClaim, RuntimeAgentSessionTitleTask,
    RuntimeSessionService, current_unix_millis, current_unix_seconds, json_escape,
};

/// Hard cap on concurrently in-flight generated-title tasks.
///
/// Title generation is strictly optional work, so it is capped far below the
/// agent concurrency limit and never competes with turn machinery.
pub(crate) const MAX_CONCURRENT_SESSION_TITLE_TASKS: usize = 2;

/// Maximum provider attempts for one conversation title, so one retry is allowed.
pub(crate) const SESSION_TITLE_MAX_ATTEMPTS: u32 = 2;

/// Provider-worker lease applied to one claimed generated-title task.
///
/// The lease is far above a bounded title reply, so a live worker is never reaped
/// while it is still waiting, and it is bounded so a lost worker cannot hold a
/// concurrency slot for the life of the process.
pub(crate) const SESSION_TITLE_CLAIM_TIMEOUT_MS: u64 = 300_000;

/// Maximum conversations whose last admission denial is remembered.
///
/// The marker only deduplicates routine denial traces, so a bounded map of the
/// most recent conversations is enough.
pub(crate) const SESSION_TITLE_DENIAL_MARKER_CAP: usize = 256;

/// Provider-task backlog at which title generation stops being scheduled.
///
/// A saturated queue means real turns are waiting, and a display title must never
/// delay or fail one.
pub(crate) const SESSION_TITLE_TURN_SATURATION: usize = 4;

/// Number of accepted inbound prompt turns one generated title covers.
///
/// A generated title is display state, so it is produced after the first accepted
/// prompt and refreshed once this many further prompt turns have been accepted.
/// The interval counts prompts rather than time, so a busy conversation refreshes
/// and an idle one stays as it is.
pub(crate) const SESSION_TITLE_REFRESH_PROMPT_INTERVAL: u32 = 5;

/// Reports whether one accepted prompt turn reached the next title refresh.
///
/// Prompt ordinals are 1-based accepted-inbound-prompt ticks. A conversation this
/// daemon has never started a window for is always due, and one whose window
/// started at an earlier prompt is due again once the interval elapsed, so a
/// settled conversation is refreshed on a bounded cadence instead of never.
pub(crate) fn session_title_refresh_due(
    prompt_ordinal: u32,
    generated_at_prompt_ordinal: u32,
) -> bool {
    generated_at_prompt_ordinal == 0
        || prompt_ordinal.saturating_sub(generated_at_prompt_ordinal)
            >= SESSION_TITLE_REFRESH_PROMPT_INTERVAL
}

/// Stable bounded reason one conversation was refused a title request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionTitleDenial {
    /// The configured policy is not `generated`, which is the opt-out.
    PolicyOptOut,
    /// A durable manual name already wins over any generated title.
    ManualName,
    /// A generated title is already stored for this conversation.
    AlreadyGenerated,
    /// This conversation already has a finished or in-flight title task.
    AlreadyScheduled,
    /// No provider is available, so no request may be dispatched.
    ProviderUnavailable,
    /// Turn machinery is saturated and takes priority over display work.
    TurnMachinerySaturated,
    /// The configured concurrent title-task cap is already in use.
    ConcurrencyCap,
    /// No bounded input exists to build a request from.
    NoInputs,
    /// The conversation store is unavailable.
    StorageUnavailable,
}

impl SessionTitleDenial {
    /// Returns the stable bounded reason name.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PolicyOptOut => "policy_opt_out",
            Self::ManualName => "manual_name",
            Self::AlreadyGenerated => "already_generated",
            Self::AlreadyScheduled => "already_scheduled",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::TurnMachinerySaturated => "turn_machinery_saturated",
            Self::ConcurrencyCap => "concurrency_cap",
            Self::NoInputs => "no_inputs",
            Self::StorageUnavailable => "storage_unavailable",
        }
    }
}

/// Bounded generation progress for one conversation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SessionTitleTaskState {
    /// Provider attempts started in the current refresh window.
    pub(crate) attempts: u32,
    /// Whether one request is currently in flight for this conversation.
    pub(crate) in_flight: bool,
    /// Whether the current refresh window is closed.
    ///
    /// A window closes when it generated a title, exhausted its attempts, or was
    /// retired. The first prompt tick that reaches the refresh interval re-arms
    /// it, so this flag no longer means "never asked again": the cadence, not
    /// this flag, decides when the next window opens.
    pub(crate) completed: bool,
    /// Accepted inbound prompt turns counted for this conversation, 1-based.
    pub(crate) prompt_ordinal: u32,
    /// Prompt ordinal the current refresh window started at.
    ///
    /// Zero means no window started in this daemon lifetime, which is what makes
    /// the first generation of a conversation different from a refresh.
    pub(crate) generated_at_prompt_ordinal: u32,
    /// Unix milliseconds at which the current claim was installed.
    ///
    /// A claimed task leaves the pending queue, so this lease is what makes a
    /// lost worker observable and recoverable instead of holding a concurrency
    /// slot forever.
    pub(crate) claimed_at_unix_ms: Option<u64>,
}

/// Bounded observed state one generated-title admission decision depends on.
///
/// The values are gathered by the caller so admission stays a pure, fully
/// testable predicate. They are split in two because the store-free half has to
/// be decided before the transcript store is cloned or any index is probed: a
/// settled conversation repeats this path on every turn, and it must never read
/// a store index merely to recompute the same denial.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SessionTitleTaskAdmission {
    /// Configured saved-session title policy in force for this session.
    pub(crate) policy: SessionTitlePolicy,
    /// Whether this conversation already has queued or claimed title work.
    pub(crate) scheduled: bool,
    /// Whether a configured provider can carry the request.
    pub(crate) provider_available: bool,
    /// Whether turn machinery is saturated and takes priority.
    pub(crate) turn_machinery_saturated: bool,
    /// Accepted inbound prompt turns counted for this conversation, 1-based.
    pub(crate) prompt_ordinal: u32,
    /// Prompt ordinal the current refresh window started at, 0 when none did.
    pub(crate) generated_at_prompt_ordinal: u32,
    /// Retained progress for this conversation, when one exists.
    pub(crate) state: Option<SessionTitleTaskState>,
    /// Number of conversations with a request currently in flight.
    pub(crate) in_flight: usize,
}

/// Bounded store observations one generated-title admission decision depends on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SessionTitleStoreAdmission {
    /// Whether a durable user-assigned name already wins over any title.
    pub(crate) has_manual_name: bool,
    /// Whether a generated title is already stored for this conversation.
    pub(crate) has_stored_generated_title: bool,
    /// Whether this daemon already has a generation epoch for the conversation.
    ///
    /// A stored generated title only makes a request pointless while this daemon
    /// has no epoch of its own: with an epoch the stored title was already
    /// adopted and the cadence decides when it is refreshed.
    pub(crate) has_generation_epoch: bool,
    /// Whether any bounded input can seed a title request.
    pub(crate) has_inputs: bool,
}

/// Decides every store-free refusal for one generated-title request.
///
/// The policy opt-out short-circuits first, queued or in-flight work is never
/// duplicated, a window that is closed and not yet due is not re-opened, and
/// queue pressure always wins over optional display work. Nothing here touches
/// the transcript store, so a conversation that is settled, refused, or
/// provider-less performs no index read.
pub(crate) fn session_title_task_admission(
    request: SessionTitleTaskAdmission,
) -> Result<(), SessionTitleDenial> {
    let SessionTitleTaskAdmission {
        policy,
        scheduled,
        provider_available,
        turn_machinery_saturated,
        prompt_ordinal,
        generated_at_prompt_ordinal,
        state,
        in_flight,
    } = request;
    if policy != SessionTitlePolicy::Generated {
        return Err(SessionTitleDenial::PolicyOptOut);
    }
    if scheduled || state.is_some_and(|state| state.in_flight) {
        return Err(SessionTitleDenial::AlreadyScheduled);
    }
    // The cadence decides only a closed window. An open window that is not in
    // flight is the retry continuation inside the attempt budget, and its second
    // attempt belongs to the prompt that spent the first one, so the cadence must
    // not refuse it.
    if state.is_some_and(|state| state.completed)
        && !session_title_refresh_due(prompt_ordinal, generated_at_prompt_ordinal)
    {
        return Err(SessionTitleDenial::AlreadyScheduled);
    }
    if !provider_available {
        return Err(SessionTitleDenial::ProviderUnavailable);
    }
    if turn_machinery_saturated {
        return Err(SessionTitleDenial::TurnMachinerySaturated);
    }
    if in_flight >= MAX_CONCURRENT_SESSION_TITLE_TASKS {
        return Err(SessionTitleDenial::ConcurrencyCap);
    }
    Ok(())
}

/// Decides every store-dependent refusal for one generated-title request.
///
/// A durable manual name makes a generated title pointless, a stored generated
/// title refuses only while this daemon has no epoch to measure a refresh from,
/// and a request with no bounded input can never produce one.
pub(crate) fn session_title_store_admission(
    request: SessionTitleStoreAdmission,
) -> Result<(), SessionTitleDenial> {
    let SessionTitleStoreAdmission {
        has_manual_name,
        has_stored_generated_title,
        has_generation_epoch,
        has_inputs,
    } = request;
    if has_manual_name {
        return Err(SessionTitleDenial::ManualName);
    }
    if has_stored_generated_title && !has_generation_epoch {
        return Err(SessionTitleDenial::AlreadyGenerated);
    }
    if !has_inputs {
        return Err(SessionTitleDenial::NoInputs);
    }
    Ok(())
}

/// Composes both admission halves in the documented precedence order.
///
/// Production callers run the two halves directly so a store-free refusal can be
/// answered before any index read; this composition exists so the ordering itself
/// stays covered by one focused test.
#[cfg(test)]
pub(crate) fn session_title_admission(
    task: SessionTitleTaskAdmission,
    store: SessionTitleStoreAdmission,
) -> Result<(), SessionTitleDenial> {
    session_title_task_admission(task)?;
    session_title_store_admission(store)
}

/// Bounded generated-title task bookkeeping for one runtime service.
///
/// The map is keyed by conversation id, so at most one in-flight task can exist
/// per conversation, and a conversation is asked again only when a prompt tick
/// reaches the refresh interval and re-arms its window.
#[derive(Debug, Default)]
pub(crate) struct RuntimeSessionTitleTasks {
    tasks: BTreeMap<String, SessionTitleTaskState>,
    /// Last admission denial recorded per conversation, bounded by the cap.
    denials: BTreeMap<String, SessionTitleDenial>,
}

impl RuntimeSessionTitleTasks {
    /// Returns the retained state for one conversation.
    pub(crate) fn state(&self, conversation_id: &str) -> Option<SessionTitleTaskState> {
        self.tasks.get(conversation_id).copied()
    }

    /// Returns the number of conversations with an in-flight request.
    pub(crate) fn in_flight(&self) -> usize {
        self.tasks.values().filter(|state| state.in_flight).count()
    }

    /// Counts one accepted inbound prompt turn and opens a due refresh window.
    ///
    /// The ordinal is 1-based and saturating, so it can never wrap into the value
    /// that means "no window started yet". Only a due window is re-armed, so a
    /// settled conversation keeps its refusal and its zero index reads until the
    /// refresh interval is reached. Returns the new ordinal.
    pub(crate) fn note_prompt_turn(&mut self, conversation_id: &str) -> u32 {
        let state = self.tasks.entry(conversation_id.to_string()).or_default();
        state.prompt_ordinal = state.prompt_ordinal.saturating_add(1);
        if session_title_refresh_due(state.prompt_ordinal, state.generated_at_prompt_ordinal) {
            state.completed = false;
            state.attempts = 0;
        }
        state.prompt_ordinal
    }

    /// Adopts one stored generated title this daemon did not produce.
    ///
    /// A stored generated title outlives the daemon that wrote it, so the first
    /// prompt of a later daemon adopts it with its own prompt ordinal instead of
    /// refusing on every tick. The adopted window is closed, so the cadence then
    /// decides when the title is refreshed.
    pub(crate) fn adopt_stored_title(&mut self, conversation_id: &str, ordinal: u32) {
        let state = self.tasks.entry(conversation_id.to_string()).or_default();
        state.generated_at_prompt_ordinal = ordinal;
        state.completed = true;
        state.in_flight = false;
    }

    /// Marks one conversation request in flight and counts its attempt.
    ///
    /// Returns the attempt number that just started. The caller has already
    /// admitted the request, so an in-flight entry here is always a fresh one.
    /// The window starts at the prompt whose attempt this is, so both the cadence
    /// and the refresh inputs are measured from the prompt that spent a request.
    pub(crate) fn begin(&mut self, conversation_id: &str) -> u32 {
        let state = self.tasks.entry(conversation_id.to_string()).or_default();
        state.attempts = state.attempts.saturating_add(1);
        state.in_flight = true;
        state.generated_at_prompt_ordinal = state.prompt_ordinal;
        state.attempts
    }

    /// Settles one successful conversation title, closing the current window.
    pub(crate) fn settle_success(&mut self, conversation_id: &str) {
        let state = self.tasks.entry(conversation_id.to_string()).or_default();
        state.in_flight = false;
        state.completed = true;
        state.claimed_at_unix_ms = None;
    }

    /// Settles one failed attempt, reporting whether a retry is still allowed.
    ///
    /// When no retry remains the current window is closed, so the deterministic
    /// fallback title stands until a prompt tick reaches the refresh interval and
    /// opens a new window. A broken provider therefore costs one bounded attempt
    /// pair per window instead of one attempt per prompt.
    pub(crate) fn settle_failure(&mut self, conversation_id: &str) -> bool {
        let state = self.tasks.entry(conversation_id.to_string()).or_default();
        state.in_flight = false;
        state.claimed_at_unix_ms = None;
        let retry_allowed = state.attempts < SESSION_TITLE_MAX_ATTEMPTS;
        if !retry_allowed {
            state.completed = true;
        }
        retry_allowed
    }

    /// Retires the current refresh window so no further attempt starts in it.
    pub(crate) fn retire(&mut self, conversation_id: &str) {
        let state = self.tasks.entry(conversation_id.to_string()).or_default();
        state.in_flight = false;
        state.completed = true;
        state.claimed_at_unix_ms = None;
    }

    /// Records the lease for one claim that just left the pending queue.
    pub(crate) fn note_claim(&mut self, conversation_id: &str, claimed_at_unix_ms: u64) {
        let state = self.tasks.entry(conversation_id.to_string()).or_default();
        state.claimed_at_unix_ms = Some(claimed_at_unix_ms);
    }

    /// Clears one lease after a failed claim step returned the task to the queue.
    pub(crate) fn note_claim_cleared(&mut self, conversation_id: &str) {
        if let Some(state) = self.tasks.get_mut(conversation_id) {
            state.claimed_at_unix_ms = None;
        }
    }

    /// Returns the conversations whose claim lease expired at `now_unix_ms`.
    ///
    /// Expiry is measured from the claim, not from the attempt, so a worker that
    /// is still running is never reaped while its lease is live.
    pub(crate) fn expired_claims(&self, now_unix_ms: u64) -> Vec<String> {
        self.tasks
            .iter()
            .filter(|(_conversation_id, state)| {
                state.in_flight
                    && state.claimed_at_unix_ms.is_some_and(|claimed_at| {
                        now_unix_ms.saturating_sub(claimed_at) >= SESSION_TITLE_CLAIM_TIMEOUT_MS
                    })
            })
            .map(|(conversation_id, _state)| conversation_id.clone())
            .collect()
    }

    /// Drops one conversation's task state so nothing stays pending after close.
    pub(crate) fn cancel(&mut self, conversation_id: &str) {
        self.tasks.remove(conversation_id);
        self.denials.remove(conversation_id);
    }

    /// Records one admission denial and reports whether the reason changed.
    ///
    /// Routine refusals repeat on every turn, so callers trace a denial only when
    /// this reports a reason the conversation has not already recorded. The map is
    /// capped, and the oldest keys are dropped first, so remembered markers stay
    /// bounded even for conversations that are never cancelled.
    pub(crate) fn note_denial(
        &mut self,
        conversation_id: &str,
        denial: SessionTitleDenial,
    ) -> bool {
        let previous = self.denials.insert(conversation_id.to_string(), denial);
        let excess = self
            .denials
            .len()
            .saturating_sub(SESSION_TITLE_DENIAL_MARKER_CAP);
        if excess > 0 {
            let stale = self
                .denials
                .keys()
                .take(excess)
                .cloned()
                .collect::<Vec<_>>();
            for stale in stale {
                self.denials.remove(&stale);
            }
        }
        previous != Some(denial)
    }

    /// Clears one conversation's remembered denial after a task was queued.
    pub(crate) fn clear_denial(&mut self, conversation_id: &str) {
        self.denials.remove(conversation_id);
    }
}

/// Returns the bounded synthetic provider-task id for one conversation title.
///
/// Title work is not a turn, so it never borrows a turn id. The stable
/// `title:<conversation_id>` form keeps trace output bounded and makes a title
/// task distinguishable from every turn-backed provider task.
pub(crate) fn session_title_task_id(conversation_id: &str) -> String {
    format!("title:{conversation_id}")
}

/// Chooses the model profile one generated-title request must use.
///
/// A configured `agents.session_title_model_profile` override wins only while it
/// still resolves. An absent, empty, or no-longer-resolvable name degrades to the
/// conversation profile, so a stale override can never block a title or fail work.
/// The resolver is injected so the degradation branch stays testable even though
/// config validation rejects an unknown name up front.
pub(crate) fn session_title_profile_choice(
    conversation_profile: (String, ModelProfile),
    configured_override: Option<String>,
    resolve_override: impl FnOnce(&str) -> Option<ModelProfile>,
) -> (String, ModelProfile) {
    match configured_override {
        Some(name) => match resolve_override(&name) {
            Some(profile) => (name, profile),
            None => conversation_profile,
        },
        None => conversation_profile,
    }
}

impl RuntimeSessionService {
    /// Returns conversation ids whose generated-title request is still queued.
    pub fn pending_agent_session_title_tasks(&self) -> Vec<String> {
        self.agent
            .pending_agent_session_title_tasks
            .keys()
            .cloned()
            .collect()
    }

    /// Reports whether one conversation has queued or in-flight title work.
    pub fn agent_session_title_task_is_scheduled(&self, conversation_id: &str) -> bool {
        self.agent
            .pending_agent_session_title_tasks
            .contains_key(conversation_id)
            || self
                .agent
                .claimed_agent_session_title_tasks
                .contains_key(conversation_id)
    }

    /// Returns conversation ids whose title task a worker currently owns.
    ///
    /// A claim is a worker lease, so a test asserting that no claim was stranded
    /// needs to distinguish it from the queued task it was taken from.
    #[cfg(test)]
    pub(crate) fn claimed_agent_session_title_task_ids(&self) -> Vec<String> {
        self.agent
            .claimed_agent_session_title_tasks
            .keys()
            .cloned()
            .collect()
    }

    /// Reports whether any generated-title task is claimed by a worker.
    ///
    /// A claimed task is invisible to the pending queues, so the provider poll
    /// timer must keep ticking while one is outstanding: that tick is what reaps a
    /// claim whose worker lease expired.
    pub(crate) fn agent_session_title_claim_is_outstanding(&self) -> bool {
        !self.agent.claimed_agent_session_title_tasks.is_empty()
    }

    /// Queues at most one generated-title request for one conversation.
    ///
    /// Returns the stable bounded denial reason when nothing was queued. A
    /// request is queued only under the `generated` policy, for a prompt turn
    /// that reached the refresh interval, with no manual name or unadopted stored
    /// generated title, an available provider, unsaturated turn machinery, and
    /// the configured concurrency cap respected. The queued task is turn-less: it
    /// never creates, claims, or mutates a turn.
    ///
    /// Every store-free refusal is decided before the transcript store is cloned
    /// or any index is probed, so the ordinary per-turn refresh of a settled
    /// conversation that is not due yet reads no index at all.
    pub(crate) fn schedule_agent_session_title_for_conversation(
        &mut self,
        conversation_id: &str,
        pane_id: &str,
        objective: Option<&str>,
    ) -> std::result::Result<(), SessionTitleDenial> {
        // The documented opt-out is decided before any provider or store state is
        // consulted, so switching the policy away from `generated` costs nothing.
        if self.agent_session_title_policy() != SessionTitlePolicy::Generated {
            return Err(self.deny_session_title_generation(
                pane_id,
                conversation_id,
                SessionTitleDenial::PolicyOptOut,
            ));
        }
        let agent_id = format!("agent-{pane_id}");
        // The model profile comes from live pane state, never from a store index,
        // so it is resolved before anything touches the transcript store.
        let Some((model_profile_name, model_profile)) =
            self.session_title_model_profile(pane_id, &agent_id)
        else {
            return Err(self.deny_session_title_generation(
                pane_id,
                conversation_id,
                SessionTitleDenial::ProviderUnavailable,
            ));
        };
        if let Some(denial) =
            self.session_title_task_denial(conversation_id, &model_profile.provider)
        {
            return Err(self.deny_session_title_generation(pane_id, conversation_id, denial));
        }
        // The cadence, the trace reason, and the request inputs all read the same
        // retained ordinals, so they are taken once, before any store work.
        let retained = self.agent.session_title_tasks.state(conversation_id);
        let prompt_ordinal = retained.map_or(0, |state| state.prompt_ordinal);
        let generated_at_prompt_ordinal =
            retained.map_or(0, |state| state.generated_at_prompt_ordinal);
        // A non-zero epoch means this daemon already started a window for the
        // conversation, which makes this request a bounded refresh.
        let is_refresh = generated_at_prompt_ordinal != 0;
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Err(self.deny_session_title_generation(
                pane_id,
                conversation_id,
                SessionTitleDenial::StorageUnavailable,
            ));
        };
        // A conversation needs a generated title only when no durable manual
        // name already wins and no unadopted stored generated title exists. A
        // failed probe is its own bounded denial and is never mistaken for a
        // manual name.
        let probe = match store.session_title_generation_probe(conversation_id) {
            Ok(probe) => probe,
            Err(_) => {
                return Err(self.deny_session_title_generation(
                    pane_id,
                    conversation_id,
                    SessionTitleDenial::StorageUnavailable,
                ));
            }
        };
        // One summary read supplies both bounded prompt inputs: the first prompt
        // stays the stable opening of a conversation, and the latest prompt is
        // what lets a refresh follow the conversation instead of restating it.
        let conversation_summary = store.summary(conversation_id).ok().flatten();
        let first_prompt = conversation_summary
            .as_ref()
            .and_then(|summary| summary.initial_prompt.as_deref());
        let summary_line = if is_refresh {
            conversation_summary
                .as_ref()
                .and_then(|summary| summary.latest_user_prompt.as_deref())
        } else {
            None
        };
        let inputs = SessionTitleGenerationInputs {
            objective,
            first_prompt,
            summary_line,
        };
        if let Err(denial) = session_title_store_admission(SessionTitleStoreAdmission {
            has_manual_name: probe.has_manual_name,
            has_stored_generated_title: probe.has_stored_generated_title,
            has_generation_epoch: generated_at_prompt_ordinal != 0,
            has_inputs: inputs.has_input(),
        }) {
            if denial == SessionTitleDenial::AlreadyGenerated {
                // A stored title written outside this daemon lifetime is adopted
                // as the epoch the cadence measures from. This tick is still
                // refused, and the next due tick refreshes the adopted title.
                self.agent
                    .session_title_tasks
                    .adopt_stored_title(conversation_id, prompt_ordinal);
            }
            return Err(self.deny_session_title_generation(pane_id, conversation_id, denial));
        }
        let request = session_title_request(&model_profile, &agent_id, &inputs);
        self.agent.session_title_tasks.begin(conversation_id);
        // A queued task resets the denial marker so a later refusal traces again.
        self.agent.session_title_tasks.clear_denial(conversation_id);
        self.agent.pending_agent_session_title_tasks.insert(
            conversation_id.to_string(),
            RuntimeAgentSessionTitleTask {
                conversation_id: conversation_id.to_string(),
                pane_id: pane_id.to_string(),
                agent_id,
                model_profile_name,
                model_profile,
                request,
            },
        );
        // A diagnostic trace failure must never fail admission or turn work. The
        // reason stays bounded: a refresh line carries the prompt ordinal only,
        // never any prompt text.
        let reason = if is_refresh {
            format!("session_title queued reason=refresh prompt_ordinal={prompt_ordinal}")
        } else {
            "session_title queued reason=conversation_objective_published".to_string()
        };
        let _ = self.append_agent_trace_turn_event(
            pane_id,
            &session_title_task_id(conversation_id),
            &reason,
        );
        Ok(())
    }

    /// Runs every store-free admission gate for one conversation.
    ///
    /// Nothing here touches the transcript store, so a settled, refused, or
    /// provider-less conversation is answered without any index read.
    fn session_title_task_denial(
        &mut self,
        conversation_id: &str,
        provider_name: &str,
    ) -> Option<SessionTitleDenial> {
        let state = self.agent.session_title_tasks.state(conversation_id);
        session_title_task_admission(SessionTitleTaskAdmission {
            policy: self.agent_session_title_policy(),
            scheduled: self.agent_session_title_task_is_scheduled(conversation_id),
            provider_available: self.session_title_provider_available(provider_name),
            turn_machinery_saturated: self.pending_agent_provider_tasks().len()
                >= SESSION_TITLE_TURN_SATURATION,
            prompt_ordinal: state.map_or(0, |state| state.prompt_ordinal),
            generated_at_prompt_ordinal: state.map_or(0, |state| state.generated_at_prompt_ordinal),
            state,
            in_flight: self.agent.session_title_tasks.in_flight(),
        })
        .err()
    }

    /// Reports whether the configured provider can carry one title request.
    fn session_title_provider_available(&self, provider_name: &str) -> bool {
        self.provider_registry().provider(provider_name).is_some()
            && self.integration.auth_store().is_some()
    }

    /// Records one admission denial and reports it back to the caller.
    ///
    /// Routine refusals repeat on every turn, so a conversation traces a denial
    /// only when its reason changes. Only actionable reasons are traced at all,
    /// because an opt-out or an already-satisfied request needs no operator
    /// attention.
    fn deny_session_title_generation(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        denial: SessionTitleDenial,
    ) -> SessionTitleDenial {
        let changed = self
            .agent
            .session_title_tasks
            .note_denial(conversation_id, denial);
        // A diagnostic trace failure must never fail admission or turn work.
        if changed
            && matches!(
                denial,
                SessionTitleDenial::ProviderUnavailable
                    | SessionTitleDenial::TurnMachinerySaturated
                    | SessionTitleDenial::ConcurrencyCap
                    | SessionTitleDenial::NoInputs
                    | SessionTitleDenial::StorageUnavailable
            )
        {
            let _ = self.append_agent_trace_turn_event(
                pane_id,
                &session_title_task_id(conversation_id),
                &format!("session_title skipped reason={}", denial.as_str()),
            );
        }
        denial
    }

    /// Attempts one generated-title admission for a published conversation.
    ///
    /// The live pane that owns the conversation supplies the conversation model
    /// profile and the status line, so a conversation with no live pane is
    /// skipped instead of being queued. An ephemeral conversation is skipped for
    /// the same reason the objective mirror skips it: it never persists a
    /// transcript or a browsable title, so no request may be spent on it.
    pub(crate) fn schedule_runtime_agent_session_title(
        &mut self,
        conversation_id: &str,
        objective: Option<&str>,
    ) -> std::result::Result<(), SessionTitleDenial> {
        if self.runtime_agent_conversation_is_ephemeral(conversation_id) {
            return Ok(());
        }
        let Some(pane_id) = self
            .agent_shell_store()
            .sessions()
            .find(|session| session.session_id == conversation_id)
            .map(|session| session.pane_id.clone())
        else {
            return Err(SessionTitleDenial::StorageUnavailable);
        };
        self.schedule_agent_session_title_for_conversation(conversation_id, &pane_id, objective)
    }

    /// Resolves the model profile used for one conversation's title request.
    ///
    /// `agents.session_title_model_profile` selects an optional cheaper profile
    /// for display-only work. An absent or empty value, an unreadable effective
    /// config, or a name that no longer resolves degrades to the conversation
    /// model profile, so a stale override can never block a title or fail work.
    fn session_title_model_profile(
        &self,
        pane_id: &str,
        agent_id: &str,
    ) -> Option<(String, ModelProfile)> {
        let conversation = self
            .active_model_profile_for_pane(pane_id, agent_id, None)
            .ok()?;
        let configured_override = runtime_effective_config_value(self.integration.config_layers())
            .ok()
            .and_then(|root| {
                root.get("agents")
                    .and_then(|agents| agents.get("session_title_model_profile"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
            });
        Some(session_title_profile_choice(
            conversation,
            configured_override,
            |name| self.provider_registry().resolve_profile(name).ok(),
        ))
    }

    /// Claims one queued generated-title task for execution outside the actor.
    ///
    /// A claim never touches a turn: the returned dispatch carries only the
    /// bounded request and the provider that must execute it. When the provider
    /// cannot be constructed the task is retired with a bounded reason instead
    /// of being left queued, so a permanent provider failure cannot spin.
    ///
    /// Every fallible step runs before the claim is installed: a failed
    /// diagnostic must never leave a claim that no worker owns, because such a
    /// claim would consume a concurrency slot forever. The recorded claim also
    /// carries the worker lease the provider poll timer reaps.
    pub fn claim_agent_session_title_task(
        &mut self,
        conversation_id: &str,
    ) -> MezResult<Option<RuntimeAgentSessionTitleDispatch>> {
        let Some(task) = self
            .agent
            .pending_agent_session_title_tasks
            .remove(conversation_id)
        else {
            return Ok(None);
        };
        let provider = match self
            .runtime_model_provider_for_profile(&task.model_profile, "provider_session_title")
        {
            Ok(provider) => provider,
            Err(error) => {
                self.agent.session_title_tasks.retire(conversation_id);
                // A diagnostic write must never mask the provider failure or leave
                // a claim behind, so it is best effort here.
                let _ = self.append_agent_trace_turn_event(
                    &task.pane_id,
                    &session_title_task_id(conversation_id),
                    &format!(
                        "session_title unavailable reason={}",
                        SessionTitleFailureReason::ProviderError.as_str()
                    ),
                );
                return Err(error);
            }
        };
        // The diagnostic is written before the claim exists, so a failed trace
        // returns the task to the pending queue instead of stranding a claim.
        if let Err(error) = self.append_agent_trace_turn_event(
            &task.pane_id,
            &session_title_task_id(conversation_id),
            &format!(
                "session_title claimed agent={} reason=async_provider_worker",
                task.agent_id
            ),
        ) {
            self.restore_pending_agent_session_title_task(task);
            return Err(error);
        }
        let claimed_at_unix_ms = current_unix_millis();
        self.agent
            .session_title_tasks
            .note_claim(conversation_id, claimed_at_unix_ms);
        self.agent.claimed_agent_session_title_tasks.insert(
            conversation_id.to_string(),
            RuntimeAgentSessionTitleClaim {
                task: task.clone(),
                claimed_at_unix_ms,
                timeout_ms: SESSION_TITLE_CLAIM_TIMEOUT_MS,
            },
        );
        Ok(Some(RuntimeAgentSessionTitleDispatch { task, provider }))
    }

    /// Returns one claimed task to the pending queue after a failed claim step.
    ///
    /// The conversation keeps its in-flight marker, so the next provider poll can
    /// claim the same task again instead of losing it or holding a slot.
    pub(crate) fn restore_pending_agent_session_title_task(
        &mut self,
        task: RuntimeAgentSessionTitleTask,
    ) {
        self.agent
            .claimed_agent_session_title_tasks
            .remove(task.conversation_id.as_str());
        self.agent
            .session_title_tasks
            .note_claim_cleared(&task.conversation_id);
        self.agent
            .pending_agent_session_title_tasks
            .insert(task.conversation_id.clone(), task);
    }

    /// Cancels every generated-title task retained for one conversation.
    ///
    /// Closing a session or conversation must leave no pending task and no
    /// orphaned claim, so the queued task, the worker claim, and the retained
    /// attempt state are all dropped together.
    pub(crate) fn cancel_agent_session_title_task(&mut self, conversation_id: &str) -> bool {
        let pending = self
            .agent
            .pending_agent_session_title_tasks
            .remove(conversation_id)
            .is_some();
        let claimed = self
            .agent
            .claimed_agent_session_title_tasks
            .remove(conversation_id)
            .is_some();
        self.agent.session_title_tasks.cancel(conversation_id);
        pending || claimed
    }

    /// Applies one generated-title worker result through actor-owned state.
    ///
    /// The title is persisted only on success. Every other outcome records a
    /// bounded reason and settles the in-flight marker, honoring the configured
    /// retry limit before the row falls back to the objective-derived title.
    pub(crate) fn apply_agent_session_title_transition(
        &mut self,
        event: AgentSessionTitleEvent,
    ) -> MezResult<RuntimeTransition> {
        match event {
            AgentSessionTitleEvent::Settled {
                conversation_id,
                outcome,
            } => match outcome {
                AgentSessionTitleOutcome::Generated(title) => {
                    self.settle_agent_session_title_success(&conversation_id, &title)
                }
                AgentSessionTitleOutcome::Rejected(reason) => self
                    .settle_agent_session_title_failure(
                        &conversation_id,
                        bounded_session_title_reason(&reason),
                    ),
            },
            AgentSessionTitleEvent::Failed {
                conversation_id,
                kind,
                message: _,
            } => self.settle_agent_session_title_failure(
                &conversation_id,
                bounded_session_title_reason(&kind),
            ),
        }
    }

    /// Persists one successful generated title and reports it on the pane.
    fn settle_agent_session_title_success(
        &mut self,
        conversation_id: &str,
        title: &str,
    ) -> MezResult<RuntimeTransition> {
        let Some(claim) = self
            .agent
            .claimed_agent_session_title_tasks
            .remove(conversation_id)
        else {
            return Ok(RuntimeTransition::default());
        };
        let task = claim.task;
        self.agent
            .session_title_tasks
            .settle_success(conversation_id);
        let written = self.persist_generated_session_title(conversation_id, title);
        let task_id = session_title_task_id(conversation_id);
        let reason = if written {
            "written"
        } else {
            SessionTitleFailureReason::StorageUnavailable.as_str()
        };
        self.append_agent_trace_turn_event(
            &task.pane_id,
            &task_id,
            &format!(
                "session_title generated result={reason} model_profile={}",
                task.model_profile_name
            ),
        )?;
        if written {
            self.append_agent_status_text_to_terminal_buffer(
                &task.pane_id,
                &format!("agent: session title set: {title}"),
            )?;
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_session_title":"{}","conversation_id":"{}","model_profile":"{}"}}"#,
                json_escape(&task.pane_id),
                json_escape(reason),
                json_escape(conversation_id),
                json_escape(&task.model_profile_name),
            ),
        )?;
        Ok(RuntimeTransition {
            applied: true,
            side_effects: vec![],
        })
    }

    /// Settles one failed generated-title attempt with a bounded reason.
    ///
    /// A retry is queued while the configured attempt budget remains; once it is
    /// exhausted the conversation is retired so the row falls back to the
    /// objective-derived title and no further provider call is made. A retry is
    /// admitted again before it is queued, so a policy switch, a manual name, or
    /// turn saturation between the two attempts ends generation instead of
    /// spending a second provider call.
    fn settle_agent_session_title_failure(
        &mut self,
        conversation_id: &str,
        reason: &str,
    ) -> MezResult<RuntimeTransition> {
        let Some(claim) = self
            .agent
            .claimed_agent_session_title_tasks
            .get(conversation_id)
            .cloned()
        else {
            return Ok(RuntimeTransition::default());
        };
        let task = claim.task;
        // The attempt is settled before the retry gates run, so re-admission sees
        // the cleared claim rather than refusing the retry as already scheduled.
        self.agent
            .claimed_agent_session_title_tasks
            .remove(conversation_id);
        let retry_allowed = self
            .agent
            .session_title_tasks
            .settle_failure(conversation_id);
        let task_id = session_title_task_id(conversation_id);
        let retry_denial = if retry_allowed {
            self.session_title_retry_denial(conversation_id, &task)
        } else {
            None
        };
        if retry_allowed && retry_denial.is_none() {
            self.agent.session_title_tasks.begin(conversation_id);
            self.append_agent_trace_turn_event(
                &task.pane_id,
                &task_id,
                &format!("session_title retry_scheduled reason={reason}"),
            )?;
            self.agent
                .pending_agent_session_title_tasks
                .insert(conversation_id.to_string(), task);
            return Ok(RuntimeTransition {
                applied: true,
                side_effects: vec![RuntimeSideEffect::DispatchAgentSessionTitle {
                    conversation_id: conversation_id.to_string(),
                }],
            });
        }
        self.agent.session_title_tasks.retire(conversation_id);
        let outcome = match retry_denial {
            Some(denial) => format!("retry_denied={}", denial.as_str()),
            None => format!(
                "exhausted={}",
                SessionTitleFailureReason::AttemptsExhausted.as_str()
            ),
        };
        self.append_agent_trace_turn_event(
            &task.pane_id,
            &task_id,
            &format!("session_title degraded reason={reason} {outcome}"),
        )?;
        self.append_agent_status_text_to_terminal_buffer(
            &task.pane_id,
            &format!("agent: session title unavailable ({reason}); using the objective title"),
        )?;
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_session_title":"degraded","conversation_id":"{}","reason":"{}"}}"#,
                json_escape(&task.pane_id),
                json_escape(conversation_id),
                json_escape(reason),
            ),
        )?;
        Ok(RuntimeTransition {
            applied: true,
            side_effects: vec![],
        })
    }

    /// Returns the bounded reason one failed attempt may not spend a second call.
    ///
    /// The store-free gates are re-run first, so a policy opt-out or a saturated
    /// queue ends generation without reading any index. Only then is the store
    /// asked whether a manual name or a stored title appeared between attempts.
    fn session_title_retry_denial(
        &mut self,
        conversation_id: &str,
        task: &RuntimeAgentSessionTitleTask,
    ) -> Option<SessionTitleDenial> {
        if let Some(denial) =
            self.session_title_task_denial(conversation_id, &task.model_profile.provider)
        {
            return Some(denial);
        }
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Some(SessionTitleDenial::StorageUnavailable);
        };
        let Ok(probe) = store.session_title_generation_probe(conversation_id) else {
            return Some(SessionTitleDenial::StorageUnavailable);
        };
        session_title_store_admission(SessionTitleStoreAdmission {
            has_manual_name: probe.has_manual_name,
            has_stored_generated_title: probe.has_stored_generated_title,
            has_generation_epoch: self
                .agent
                .session_title_tasks
                .state(conversation_id)
                .is_some_and(|state| state.generated_at_prompt_ordinal != 0),
            has_inputs: true,
        })
        .err()
    }

    /// Reaps claimed generated-title tasks whose worker lease expired.
    ///
    /// A worker that never settles would otherwise hold a concurrency slot for
    /// the life of the process. An expired claim is settled through the ordinary
    /// failure path, so the bounded attempt budget still applies, the retry gates
    /// are still re-run, and the row degrades once the budget is exhausted.
    pub(crate) fn reap_expired_agent_session_title_claims(
        &mut self,
        now_unix_ms: u64,
    ) -> MezResult<Vec<RuntimeSideEffect>> {
        let mut side_effects = Vec::new();
        for conversation_id in self.agent.session_title_tasks.expired_claims(now_unix_ms) {
            let Some(claim) = self
                .agent
                .claimed_agent_session_title_tasks
                .get(&conversation_id)
                .cloned()
            else {
                self.agent
                    .session_title_tasks
                    .note_claim_cleared(&conversation_id);
                continue;
            };
            let _ = self.append_agent_trace_turn_event(
                &claim.task.pane_id,
                &session_title_task_id(&conversation_id),
                &format!(
                    "session_title claim_lease expired claimed_at_unix_ms={} timeout_ms={}",
                    claim.claimed_at_unix_ms, claim.timeout_ms
                ),
            );
            let transition = self.settle_agent_session_title_failure(
                &conversation_id,
                SessionTitleFailureReason::Timeout.as_str(),
            )?;
            side_effects.extend(transition.side_effects);
        }
        Ok(side_effects)
    }

    /// Persists one bounded generated title through the shared title mirror.
    fn persist_generated_session_title(&mut self, conversation_id: &str, title: &str) -> bool {
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return false;
        };
        let changed = store
            .mirror_session_generated_title(conversation_id, title, current_unix_seconds())
            .unwrap_or(false);
        if changed {
            self.invalidate_agent_prompt_selector_extra_candidates();
            let _ = self.refresh_saved_session_overlay_after_title_change();
        }
        changed
    }
}

/// Bounds one worker-supplied reason to a stable, known reason name.
///
/// Unknown or empty reasons degrade to `provider_error` so a trace line can
/// never carry an unbounded worker string.
fn bounded_session_title_reason(reason: &str) -> &'static str {
    match reason {
        "provider_error" => SessionTitleFailureReason::ProviderError.as_str(),
        "timeout" => SessionTitleFailureReason::Timeout.as_str(),
        "empty" => SessionTitleFailureReason::Empty.as_str(),
        "control_characters" => SessionTitleFailureReason::ControlCharacters.as_str(),
        "oversize" => SessionTitleFailureReason::Oversize.as_str(),
        "malformed" => SessionTitleFailureReason::Malformed.as_str(),
        "output_limit" => SessionTitleFailureReason::OutputLimit.as_str(),
        "storage_unavailable" => SessionTitleFailureReason::StorageUnavailable.as_str(),
        "attempts_exhausted" => SessionTitleFailureReason::AttemptsExhausted.as_str(),
        _ => SessionTitleFailureReason::ProviderError.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::ModelProfile;
    use super::{
        MAX_CONCURRENT_SESSION_TITLE_TASKS, RuntimeSessionTitleTasks,
        SESSION_TITLE_CLAIM_TIMEOUT_MS, SESSION_TITLE_DENIAL_MARKER_CAP,
        SESSION_TITLE_MAX_ATTEMPTS, SESSION_TITLE_REFRESH_PROMPT_INTERVAL, SessionTitleDenial,
        SessionTitleStoreAdmission, SessionTitleTaskAdmission, SessionTitleTaskState,
        session_title_admission, session_title_profile_choice, session_title_refresh_due,
        session_title_store_admission, session_title_task_admission,
    };
    use crate::session_title::{
        SessionTitleFailureReason, SessionTitlePolicy, SessionTitleRejection,
    };

    /// Builds one admission request that is otherwise acceptable.
    ///
    /// The default shape queues a first request under `generated`: no manual
    /// name, no stored title, bounded inputs, an available provider, unsaturated
    /// turn machinery, and a conversation whose window has never started, so the
    /// cadence considers it due.
    fn task_admission(
        policy: SessionTitlePolicy,
        state: Option<SessionTitleTaskState>,
        in_flight: usize,
    ) -> SessionTitleTaskAdmission {
        SessionTitleTaskAdmission {
            policy,
            scheduled: false,
            provider_available: true,
            turn_machinery_saturated: false,
            prompt_ordinal: 1,
            generated_at_prompt_ordinal: 0,
            state,
            in_flight,
        }
    }

    /// Builds one otherwise acceptable store admission request.
    fn store_admission() -> SessionTitleStoreAdmission {
        SessionTitleStoreAdmission {
            has_manual_name: false,
            has_stored_generated_title: false,
            has_generation_epoch: false,
            has_inputs: true,
        }
    }

    /// Builds one retained attempt state for admission ordering tests.
    ///
    /// The window starts at the first prompt, so only an explicit cadence input
    /// makes the state due again.
    fn task_state(
        attempts: u32,
        in_flight: bool,
        completed: bool,
    ) -> Option<SessionTitleTaskState> {
        Some(SessionTitleTaskState {
            attempts,
            in_flight,
            completed,
            prompt_ordinal: 1,
            generated_at_prompt_ordinal: 1,
            claimed_at_unix_ms: None,
        })
    }

    /// Verifies every policy other than `generated` opts out before any request.
    #[test]
    fn admission_denies_every_non_generated_policy() {
        for policy in [
            SessionTitlePolicy::Objective,
            SessionTitlePolicy::LastPrompt,
            SessionTitlePolicy::FirstPrompt,
        ] {
            assert_eq!(
                session_title_admission(task_admission(policy, None, 0), store_admission()),
                Err(SessionTitleDenial::PolicyOptOut),
                "policy {}",
                policy.as_str()
            );
        }
        assert_eq!(
            session_title_admission(
                task_admission(SessionTitlePolicy::Generated, None, 0),
                store_admission()
            ),
            Ok(())
        );
    }

    /// Verifies the store-free gates refuse without any store observation.
    #[test]
    fn task_admission_refuses_scheduled_unavailable_and_saturated_requests() {
        let first_request = || task_admission(SessionTitlePolicy::Generated, None, 0);
        let cases = [
            (
                SessionTitleTaskAdmission {
                    scheduled: true,
                    ..first_request()
                },
                SessionTitleDenial::AlreadyScheduled,
            ),
            (
                SessionTitleTaskAdmission {
                    state: task_state(1, true, false),
                    ..first_request()
                },
                SessionTitleDenial::AlreadyScheduled,
            ),
            (
                SessionTitleTaskAdmission {
                    state: task_state(1, false, true),
                    generated_at_prompt_ordinal: 1,
                    ..first_request()
                },
                SessionTitleDenial::AlreadyScheduled,
            ),
            (
                SessionTitleTaskAdmission {
                    state: task_state(1, false, true),
                    prompt_ordinal: SESSION_TITLE_REFRESH_PROMPT_INTERVAL,
                    generated_at_prompt_ordinal: 1,
                    ..first_request()
                },
                SessionTitleDenial::AlreadyScheduled,
            ),
            (
                SessionTitleTaskAdmission {
                    provider_available: false,
                    ..first_request()
                },
                SessionTitleDenial::ProviderUnavailable,
            ),
            (
                SessionTitleTaskAdmission {
                    turn_machinery_saturated: true,
                    ..first_request()
                },
                SessionTitleDenial::TurnMachinerySaturated,
            ),
            (
                SessionTitleTaskAdmission {
                    in_flight: MAX_CONCURRENT_SESSION_TITLE_TASKS,
                    ..first_request()
                },
                SessionTitleDenial::ConcurrencyCap,
            ),
        ];
        for (request, expected) in cases {
            assert_eq!(
                session_title_task_admission(request),
                Err(expected),
                "{}",
                expected.as_str()
            );
            assert!(!expected.as_str().is_empty());
        }
    }

    /// Verifies a closed window is refused until the cadence makes it due.
    #[test]
    fn admission_refuses_a_closed_window_until_it_is_due() {
        let closed_window = task_state(1, false, true);
        assert_eq!(
            session_title_task_admission(SessionTitleTaskAdmission {
                state: closed_window,
                prompt_ordinal: SESSION_TITLE_REFRESH_PROMPT_INTERVAL,
                generated_at_prompt_ordinal: 1,
                ..task_admission(SessionTitlePolicy::Generated, None, 0)
            }),
            Err(SessionTitleDenial::AlreadyScheduled)
        );
        assert_eq!(
            session_title_task_admission(SessionTitleTaskAdmission {
                state: closed_window,
                prompt_ordinal: 1 + SESSION_TITLE_REFRESH_PROMPT_INTERVAL,
                generated_at_prompt_ordinal: 1,
                ..task_admission(SessionTitlePolicy::Generated, None, 0)
            }),
            Ok(())
        );
    }

    /// Verifies the refresh interval boundaries of the cadence predicate.
    #[test]
    fn refresh_due_reports_never_generated_and_interval_boundaries() {
        assert_eq!(SESSION_TITLE_REFRESH_PROMPT_INTERVAL, 5);
        // A conversation with no window in this daemon lifetime is always due.
        for prompt_ordinal in [0, 1, 9] {
            assert!(
                session_title_refresh_due(prompt_ordinal, 0),
                "{prompt_ordinal}"
            );
        }
        // A window that started at prompt 1 is not due for the next four prompts.
        for prompt_ordinal in 1..=5 {
            assert!(
                !session_title_refresh_due(prompt_ordinal, 1),
                "{prompt_ordinal}"
            );
        }
        assert!(session_title_refresh_due(6, 1));
        assert!(session_title_refresh_due(11, 6));
        // A stale epoch saturates instead of wrapping into "never generated".
        assert!(!session_title_refresh_due(3, 5));
    }

    /// Verifies store answers refuse a pointless or input-less request.
    #[test]
    fn store_admission_refuses_manual_names_stored_titles_and_missing_inputs() {
        let cases = [
            ((true, false, true), SessionTitleDenial::ManualName),
            ((false, true, true), SessionTitleDenial::AlreadyGenerated),
            ((false, false, false), SessionTitleDenial::NoInputs),
        ];
        for ((manual, stored, inputs), expected) in cases {
            assert_eq!(
                session_title_store_admission(SessionTitleStoreAdmission {
                    has_manual_name: manual,
                    has_stored_generated_title: stored,
                    has_generation_epoch: false,
                    has_inputs: inputs,
                }),
                Err(expected),
                "{}",
                expected.as_str()
            );
        }
        assert_eq!(session_title_store_admission(store_admission()), Ok(()));

        // A manual name still wins first when a stored title and an epoch are
        // both present, so the opt-out precedence is unchanged.
        assert_eq!(
            session_title_store_admission(SessionTitleStoreAdmission {
                has_manual_name: true,
                has_stored_generated_title: true,
                has_generation_epoch: true,
                ..store_admission()
            }),
            Err(SessionTitleDenial::ManualName)
        );
    }

    /// Verifies the store-free half is decided before any store answer.
    #[test]
    fn admission_composition_prefers_store_free_refusals() {
        let manual_and_opted_out = session_title_admission(
            task_admission(SessionTitlePolicy::FirstPrompt, None, 0),
            SessionTitleStoreAdmission {
                has_manual_name: true,
                ..store_admission()
            },
        );
        assert_eq!(manual_and_opted_out, Err(SessionTitleDenial::PolicyOptOut));

        let manual_and_providerless = session_title_admission(
            SessionTitleTaskAdmission {
                provider_available: false,
                ..task_admission(SessionTitlePolicy::Generated, None, 0)
            },
            SessionTitleStoreAdmission {
                has_manual_name: true,
                ..store_admission()
            },
        );
        assert_eq!(
            manual_and_providerless,
            Err(SessionTitleDenial::ProviderUnavailable)
        );
    }

    /// Verifies one conversation never holds two in-flight title tasks.
    #[test]
    fn one_conversation_admits_only_one_in_flight_task() {
        let mut tasks = RuntimeSessionTitleTasks::default();
        assert_eq!(tasks.in_flight(), 0);
        assert_eq!(tasks.begin("conversation-a"), 1);
        assert_eq!(tasks.in_flight(), 1);
        assert_eq!(
            session_title_task_admission(task_admission(
                SessionTitlePolicy::Generated,
                tasks.state("conversation-a"),
                tasks.in_flight(),
            )),
            Err(SessionTitleDenial::AlreadyScheduled)
        );
        assert_eq!(tasks.begin("conversation-b"), 1);
        assert_eq!(tasks.in_flight(), 2);
        assert_eq!(
            session_title_task_admission(task_admission(
                SessionTitlePolicy::Generated,
                tasks.state("conversation-c"),
                tasks.in_flight(),
            )),
            Err(SessionTitleDenial::ConcurrencyCap)
        );
    }

    /// Verifies prompt ticks re-arm exactly one window per refresh interval.
    #[test]
    fn prompt_ticks_rearm_only_a_due_window() {
        let mut tasks = RuntimeSessionTitleTasks::default();
        assert_eq!(tasks.note_prompt_turn("conversation-a"), 1);
        tasks.begin("conversation-a");
        tasks.settle_success("conversation-a");
        let settled = tasks.state("conversation-a").expect("state");
        assert!(settled.completed);
        assert_eq!(settled.generated_at_prompt_ordinal, 1);

        // The prompts inside the window change nothing, so a settled
        // conversation keeps its refusal and its closed window.
        for expected_ordinal in 2..=SESSION_TITLE_REFRESH_PROMPT_INTERVAL {
            assert_eq!(tasks.note_prompt_turn("conversation-a"), expected_ordinal);
            let state = tasks.state("conversation-a").expect("state");
            assert!(state.completed, "prompt {expected_ordinal}");
            assert_eq!(state.attempts, 1);
            assert_eq!(state.generated_at_prompt_ordinal, 1);
        }

        // The next prompt reaches the interval and opens a fresh window.
        assert_eq!(
            tasks.note_prompt_turn("conversation-a"),
            1 + SESSION_TITLE_REFRESH_PROMPT_INTERVAL
        );
        let rearmed = tasks.state("conversation-a").expect("state");
        assert!(!rearmed.completed);
        assert_eq!(rearmed.attempts, 0);
        assert_eq!(rearmed.generated_at_prompt_ordinal, 1);
    }

    /// Verifies an adopted stored title becomes this daemon's cadence epoch.
    #[test]
    fn adoption_retires_a_stored_title_into_the_cadence() {
        let mut tasks = RuntimeSessionTitleTasks::default();
        assert_eq!(tasks.note_prompt_turn("conversation-a"), 1);
        tasks.adopt_stored_title("conversation-a", 1);
        let adopted = tasks.state("conversation-a").expect("state");
        assert_eq!(adopted.generated_at_prompt_ordinal, 1);
        assert!(adopted.completed);
        assert!(!adopted.in_flight);

        // The adopted window is closed and not due, so it refuses exactly like a
        // title this daemon generated itself.
        assert_eq!(
            session_title_task_admission(SessionTitleTaskAdmission {
                state: tasks.state("conversation-a"),
                prompt_ordinal: 1,
                generated_at_prompt_ordinal: adopted.generated_at_prompt_ordinal,
                ..task_admission(SessionTitlePolicy::Generated, None, 0)
            }),
            Err(SessionTitleDenial::AlreadyScheduled)
        );
        assert!(session_title_refresh_due(
            1 + SESSION_TITLE_REFRESH_PROMPT_INTERVAL,
            adopted.generated_at_prompt_ordinal
        ));

        // Without an epoch a stored title still refuses outright; with one the
        // cadence, not the stored title, decides the next request.
        assert_eq!(
            session_title_store_admission(SessionTitleStoreAdmission {
                has_stored_generated_title: true,
                ..store_admission()
            }),
            Err(SessionTitleDenial::AlreadyGenerated)
        );
        assert_eq!(
            session_title_store_admission(SessionTitleStoreAdmission {
                has_stored_generated_title: true,
                has_generation_epoch: true,
                ..store_admission()
            }),
            Ok(())
        );
    }

    /// Verifies a claim lease expires only after its timeout and only while live.
    #[test]
    fn claim_lease_expires_after_its_timeout() {
        let mut tasks = RuntimeSessionTitleTasks::default();
        tasks.begin("conversation-a");
        tasks.note_claim("conversation-a", 1_000);
        assert!(
            tasks
                .expired_claims(1_000 + SESSION_TITLE_CLAIM_TIMEOUT_MS - 1)
                .is_empty()
        );
        assert_eq!(
            tasks.expired_claims(1_000 + SESSION_TITLE_CLAIM_TIMEOUT_MS),
            vec!["conversation-a".to_string()]
        );

        // A cleared claim is never reaped, and neither is a task with no lease.
        tasks.note_claim_cleared("conversation-a");
        assert!(tasks.expired_claims(u64::MAX).is_empty());
        tasks.begin("conversation-b");
        assert!(tasks.expired_claims(u64::MAX).is_empty());
    }

    /// Verifies denial markers report only a changed reason and stay bounded.
    #[test]
    fn denial_markers_report_only_changed_reasons() {
        let mut tasks = RuntimeSessionTitleTasks::default();
        assert!(tasks.note_denial("conversation-a", SessionTitleDenial::ProviderUnavailable));
        assert!(!tasks.note_denial("conversation-a", SessionTitleDenial::ProviderUnavailable));
        assert!(tasks.note_denial("conversation-a", SessionTitleDenial::TurnMachinerySaturated));
        assert!(tasks.note_denial("conversation-b", SessionTitleDenial::ProviderUnavailable));

        tasks.clear_denial("conversation-a");
        assert!(tasks.note_denial("conversation-a", SessionTitleDenial::ProviderUnavailable));
        tasks.cancel("conversation-b");
        assert!(tasks.note_denial("conversation-b", SessionTitleDenial::ProviderUnavailable));

        for index in 0..SESSION_TITLE_DENIAL_MARKER_CAP + 8 {
            assert!(tasks.note_denial(
                &format!("conversation-{index}"),
                SessionTitleDenial::NoInputs
            ));
        }
        // The oldest markers were dropped, so the first conversation is new again.
        assert!(tasks.note_denial("conversation-0", SessionTitleDenial::NoInputs));
    }

    /// Verifies exactly one retry is allowed before the fallback is final.
    #[test]
    fn failure_settlement_allows_at_most_one_retry() {
        let mut tasks = RuntimeSessionTitleTasks::default();
        tasks.begin("conversation-a");
        assert!(tasks.settle_failure("conversation-a"));
        assert!(!tasks.state("conversation-a").expect("state").completed);

        tasks.begin("conversation-a");
        assert!(!tasks.settle_failure("conversation-a"));
        let state = tasks.state("conversation-a").expect("state");
        assert!(state.completed);
        assert!(!state.in_flight);
        assert_eq!(state.attempts, SESSION_TITLE_MAX_ATTEMPTS);
        assert_eq!(tasks.in_flight(), 0);
    }

    /// Verifies success retires the conversation and cancellation clears it.
    #[test]
    fn success_retires_the_conversation_and_cancel_clears_it() {
        let mut tasks = RuntimeSessionTitleTasks::default();
        tasks.begin("conversation-a");
        tasks.settle_success("conversation-a");
        assert_eq!(tasks.in_flight(), 0);
        assert!(tasks.state("conversation-a").expect("state").completed);

        tasks.begin("conversation-b");
        tasks.cancel("conversation-b");
        assert_eq!(tasks.state("conversation-b"), None);
        assert_eq!(tasks.in_flight(), 0);
        assert!(tasks.state("conversation-a").is_some());
    }

    /// Verifies a title profile override wins while it resolves and otherwise degrades.
    #[test]
    fn title_profile_choice_prefers_a_resolving_override() {
        let conversation = (
            "default".to_string(),
            ModelProfile {
                provider: "local-chat".to_string(),
                model: "conversation-model".to_string(),
                ..ModelProfile::default()
            },
        );
        let override_profile = ModelProfile {
            provider: "local-chat".to_string(),
            model: "title-model".to_string(),
            ..ModelProfile::default()
        };

        let chosen =
            session_title_profile_choice(conversation.clone(), Some("cheap".to_string()), |name| {
                (name == "cheap").then(|| override_profile.clone())
            });
        assert_eq!(chosen.0, "cheap");
        assert_eq!(chosen.1.model, "title-model");

        let degraded =
            session_title_profile_choice(conversation.clone(), Some("stale".to_string()), |_| None);
        assert_eq!(degraded.0, "default");
        assert_eq!(degraded.1.model, "conversation-model");

        let plain = session_title_profile_choice(conversation, None, |_| None);
        assert_eq!(plain.0, "default");
        assert_eq!(plain.1.model, "conversation-model");
    }

    /// Verifies every sanitizer rejection maps to its stable failure reason.
    #[test]
    fn sanitizer_rejections_map_to_bounded_failure_reasons() {
        let cases = [
            (SessionTitleRejection::Empty, "empty"),
            (
                SessionTitleRejection::ControlCharacters,
                "control_characters",
            ),
            (SessionTitleRejection::Oversize, "oversize"),
            (SessionTitleRejection::Malformed, "malformed"),
        ];
        for (rejection, expected) in cases {
            let reason = SessionTitleFailureReason::from_rejection(rejection);
            assert_eq!(reason.as_str(), expected);
        }
        assert_eq!(
            SessionTitleFailureReason::ProviderError.as_str(),
            "provider_error"
        );
        assert_eq!(SessionTitleFailureReason::Timeout.as_str(), "timeout");
        assert_eq!(
            SessionTitleFailureReason::OutputLimit.as_str(),
            "output_limit"
        );
        assert_eq!(
            SessionTitleFailureReason::StorageUnavailable.as_str(),
            "storage_unavailable"
        );
        assert_eq!(
            SessionTitleFailureReason::AttemptsExhausted.as_str(),
            "attempts_exhausted"
        );
    }
}
