//! One concrete exchange's native authorization and original receipt custody.
//!
//! The existing journal retains this factory before awaiting the original
//! blocking admission worker. A new permit is consumed once at the adapter's
//! transport boundary. Cancellation/reconciliation never reconstruct permission.
//! Receipt-only factories remain separate descriptive ports. Configured dispatch
//! installs this source only when it already owns activated budget membership;
//! monitored ordinary workers retain that same future through paced holds and
//! actor-owned timer adoption. Titles use their actual claim-local fence; fair
//! auxiliary requeue and membership activation remain staged.
//! Qualified scoped retirement commits a separate terminal receipt after subtotals,
//! with unknown dimensions unless independent dialect evidence qualifies them.
//!
//! A compaction wave has several exchange journals but only one actor claim and
//! one active clock. Its request-local fair read/write lane permits concurrent
//! unlimited sends; only a positively refused node upgrades to exclusive park
//! custody, after issued siblings transfer durable terminal receipts. Competing
//! nodes cannot install different wait captures on that same claim or park its
//! clock twice. The journal owns the guard across cancellation, not the send
//! future. Native root admission remains the sole authority to spend; this lane
//! neither grants permits nor coordinates unrelated requests or roots.

use super::*;
use crate::integrations::agent::provider::{
    ProviderFinalUsage, ProviderObservationReceipt, ProviderObservationReceiptFactory,
    ProviderObservationReceiptSource, ProviderTransportRetirement,
};
use crate::runtime::{
    RuntimeBudgetAttemptAdmission, RuntimeBudgetAttemptWork, RuntimeBudgetIssuedAttempt,
    RuntimeBudgetReceiptWork, RuntimeBudgetRequestOwner, RuntimeBudgetUnusedAttempt,
};
use std::sync::{Arc, Mutex};

/// Frozen request-local allocation, not another attempt ledger. The existing
/// context serializes planning and its journal retains each allocated source.
/// No descriptor is reconstructed from a result or shared with a later retry.
pub(super) struct BudgetRequestAdmission {
    owner: RuntimeBudgetRequestOwner,
    waiter: Option<AsyncRuntimeSessionHandle>,
    deadline: Option<crate::integrations::agent::provider::budget_deadline::ProviderBudgetDeadline>,
    completion: Arc<Mutex<RequestCompletion>>,
    // Shared only by this original compaction dispatch's bounded node wave.
    // Readers preserve unlimited concurrency; a held node takes exclusive
    // custody only after all earlier issued siblings have durably retired.
    compaction_lane: Arc<tokio::sync::RwLock<()>>,
}

/// At most one original terminal descriptor, not another observation ledger.
/// Planning a newer exchange clears it; late old checkpoints cannot restore it.
#[derive(Default)]
struct RequestCompletion {
    sequence: u64,
    receipt: Option<RuntimeBudgetReceiptWork>,
}

struct CompletionSlot {
    owner: Arc<Mutex<RequestCompletion>>,
    sequence: u64,
}

impl CompletionSlot {
    fn publish(&self, receipt: RuntimeBudgetReceiptWork) -> Result<()> {
        let mut owner = self
            .owner
            .lock()
            .map_err(|_| suspended("budget completion custody unavailable"))?;
        if owner.sequence == self.sequence {
            owner.receipt = Some(receipt);
        }
        Ok(())
    }
}

impl BudgetRequestAdmission {
    pub(super) fn new(owner: RuntimeBudgetRequestOwner) -> Self {
        Self {
            owner,
            waiter: None,
            deadline: None,
            completion: Default::default(),
            compaction_lane: Default::default(),
        }
    }

    /// Pure access to the exact latest issued retirement descriptor, available
    /// only after its durable checkpoint. This does not validate live claims,
    /// current policy, response publication or effect authority. None includes
    /// no exchange, an unsettled newer exchange and a never-issued refusal.
    pub(super) fn completed_receipt(&self) -> Result<Option<RuntimeBudgetReceiptWork>> {
        Ok(self
            .completion
            .lock()
            .map_err(|_| suspended("budget completion custody unavailable"))?
            .receipt
            .clone())
    }

    pub(super) fn with_waiter(
        mut self,
        handle: AsyncRuntimeSessionHandle,
        deadline: crate::integrations::agent::provider::budget_deadline::ProviderBudgetDeadline,
    ) -> Self {
        self.waiter = Some(handle);
        self.deadline = Some(deadline);
        self
    }

    /// Retain the result in its ORIGINAL producer before Completed can escape.
    /// This is a staged completion boundary, not atomic root-wide effect
    /// admission. No request/repair/fallback is issued by policy observation.
    pub(super) async fn wait_for_completion(
        &self,
        agent: &str,
        turn: &str,
        generation: u64,
    ) -> Result<crate::runtime::RuntimeBudgetProviderSettlement> {
        use crate::storage::token_usage::budget::receipts::BudgetSettlementState;
        let handle = self
            .waiter
            .as_ref()
            .ok_or_else(|| suspended("original completion waiter unavailable"))?;
        let agent = AgentId::opaque(agent.to_owned())
            .ok_or_else(|| suspended("original completion agent unavailable"))?;
        let receipt = self
            .completed_receipt()?
            .ok_or_else(|| suspended("original completion retirement unavailable"))?;
        let mut read_refused = false;
        let mut first_observation = None;
        loop {
            // One strong episode witness, owned only by this producer retaining
            // its execution. A fresh episode after resumed-policy recrossing
            // cannot renew active time or borrow an earlier wait allocation.
            let future = Arc::new(());
            let capture = receipt
                .capture_settled_response(Arc::downgrade(&future))
                .await?;
            if let Some(snapshot) = completion_observation(&receipt, &mut read_refused).await {
                let first = first_observation.get_or_insert_with(|| snapshot.clone());
                if snapshot.state() == BudgetSettlementState::WithinKnownLimits {
                    return capture.into_provider_settlement(future, first.clone(), snapshot);
                }
            }
            let capture = crate::runtime::RuntimeBudgetProviderCapture::from(capture);
            let mut delay = 100u64;
            while !handle
                .park_budget_provider_claim(
                    agent.clone(),
                    turn.to_owned(),
                    generation,
                    capture.clone(),
                )
                .await?
            {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                delay = delay.saturating_mul(2).min(2000);
            }
            let inactive = self
                .deadline
                .as_ref()
                .ok_or_else(|| suspended("original completion clock unavailable"))?
                .park()?;
            loop {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                delay = delay.saturating_mul(2).min(2000);
                // Revalidate actual claim/weak-future custody even when policy
                // is unchanged. Receipt expense alone cannot outlive Stop.
                handle
                    .park_budget_provider_claim(
                        agent.clone(),
                        turn.to_owned(),
                        generation,
                        capture.clone(),
                    )
                    .await?;
                let within = completion_observation(&receipt, &mut read_refused)
                    .await
                    .is_some_and(|snapshot| {
                        first_observation.get_or_insert_with(|| snapshot.clone());
                        snapshot.state() == BudgetSettlementState::WithinKnownLimits
                    });
                if within
                    && handle
                        .reacquire_budget_provider_claim(
                            agent.clone(),
                            turn.to_owned(),
                            generation,
                            capture.clone(),
                        )
                        .await?
                {
                    break;
                }
            }
            drop(inactive);
            #[cfg(test)]
            self.owner.after_completion_resume_for_tests().await;
            // Actor selection is not a policy grant. Reobserve after adoption;
            // another crossing parks this same result, never a new inference.
        }
    }
}

/// Read failure remains an operational hold, never numeric exhaustion or an
/// excuse to discard/replay the response. Only transition-level diagnostics
/// escape; the original query handle retains its existing cancellation custody.
async fn completion_observation(
    receipt: &RuntimeBudgetReceiptWork,
    refused: &mut bool,
) -> Option<crate::storage::token_usage::budget::receipts::BudgetReceiptSettlement> {
    match receipt.commit_owner().observe_settlement().await {
        Ok(Some(snapshot)) => {
            *refused = false;
            Some(snapshot)
        }
        result => {
            if !*refused {
                eprintln!(
                    "mezzanine: original budget completion observation unavailable ({:?}); response retained",
                    result.err().map(|error| error.kind())
                );
                *refused = true;
            }
            None
        }
    }
}

impl ProviderObservationReceiptSource for BudgetRequestAdmission {
    fn for_exchange(
        &self,
        observation: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceiptFactory>> {
        self.owner.validate_request(&observation.conversation_id)?;
        use crate::storage::token_usage::budget::receipts::BudgetAttemptPurpose as Purpose;
        let purpose = match observation.purpose {
            ProviderRequestPurpose::Execution => Purpose::Execution,
            ProviderRequestPurpose::Routing => Purpose::Routing,
            ProviderRequestPurpose::Compaction => Purpose::Compaction,
            ProviderRequestPurpose::Memory => Purpose::Memory,
            ProviderRequestPurpose::Title => Purpose::Title,
            ProviderRequestPurpose::MacroJudge => Purpose::MacroJudge,
            ProviderRequestPurpose::SandboxAssessment => Purpose::SandboxAssessment,
            ProviderRequestPurpose::FailureSummary => Purpose::FailureSummary,
        };
        // The code-owned pre-send observation freezes this actual request, not
        // a worker's router/execution profile or the provider-reported model.
        let work = self.owner.prepare_attempt(
            mez_agent::ModelTokenUsageKey::new(&observation.provider, &observation.model),
            purpose,
        )?;
        let sequence = {
            let mut completion = self
                .completion
                .lock()
                .map_err(|_| suspended("budget completion custody unavailable"))?;
            completion.sequence = completion
                .sequence
                .checked_add(1)
                .ok_or_else(|| suspended("budget request sequence exhausted"))?;
            completion.receipt = None;
            completion.sequence
        };
        Ok(Arc::new(BudgetAttemptAdmission::retaining(
            work,
            self.waiter.clone(),
            self.deadline.clone(),
            Some(CompletionSlot {
                owner: self.completion.clone(),
                sequence,
            }),
            (observation.purpose == ProviderRequestPurpose::Compaction)
                .then(|| self.compaction_lane.clone()),
        )))
    }
}

/// Install before any logical request; the existing observer journal owns every
/// allocated exchange through producer retirement. Absence preserves the
/// unactivated path, never manufactures a fresh accounting root or fallback.
pub(super) fn install_request_source(
    observer: ProviderWireRequestObserver,
    owner: Option<&RuntimeBudgetRequestOwner>,
) -> Result<ProviderWireRequestObserver> {
    match owner {
        Some(owner) => observer
            .with_budget_receipt_source(Arc::new(BudgetRequestAdmission::new(owner.clone()))),
        None => Ok(observer),
    }
}

#[derive(Default)]
struct Phase {
    started: bool,
    finalized: Option<ProviderWireRequestObservation>,
    retired: Option<ProviderTransportRetirement>,
    final_usage: Option<ProviderFinalUsage>,
    // Retain the actual consumed grant independently of a canceled send future.
    issued: Option<RuntimeBudgetIssuedAttempt>,
    // Original never-consumed permit withdrawn after its actual writer joins.
    unused: Option<RuntimeBudgetUnusedAttempt>,
    // The journal retains these guards even if before_send is cancelled while
    // its original native authorization writer is still running. Only joined
    // absence or a durable terminal receipt can release them.
    compaction_reader: Option<tokio::sync::OwnedRwLockReadGuard<()>>,
    compaction_writer: Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
}

struct Source {
    work: RuntimeBudgetAttemptWork,
    phase: Mutex<Phase>,
    waiter: Option<AsyncRuntimeSessionHandle>,
    deadline: Option<crate::integrations::agent::provider::budget_deadline::ProviderBudgetDeadline>,
    completion: Option<CompletionSlot>,
    compaction_lane: Option<Arc<tokio::sync::RwLock<()>>>,
}

fn reconsider_changed_hold(
    state: &crate::storage::token_usage::budget::admission::BudgetAdmissionWaitState,
    previous: Option<&crate::storage::token_usage::budget::admission::BudgetAdmissionWaitState>,
    reason: crate::storage::token_usage::budget::admission::BudgetAdmissionHold,
) -> bool {
    use crate::storage::token_usage::budget::admission::{
        BudgetAdmissionHold, MAX_OUTSTANDING_BUDGET_ATTEMPTS,
    };
    // A global count can cycle while this root's monotonic revisions do not.
    // The previous observation predates this fresh native Capacity refusal,
    // so equality cannot inhibit a currently available slot forever.
    state.outstanding_attempts < MAX_OUTSTANDING_BUDGET_ATTEMPTS
        && (previous != Some(state) || reason == BudgetAdmissionHold::Capacity)
}

#[cfg(test)]
mod wait_races;

impl Source {
    /// Stay inside the original adapter future and journal. Descriptive changes
    /// or a currently available slot after a fresh Capacity refusal permit fair
    /// reacquisition, never a send. Polling is paced 100ms..2s and cancellable;
    /// it never creates a request or retries uncertain native authorization.
    async fn wait_for_reacquisition(
        &self,
        handle: &AsyncRuntimeSessionHandle,
        owner: &crate::runtime::RuntimeBudgetAdmissionWork,
        capture: crate::runtime::RuntimeBudgetHeldAttempt,
        reason: crate::storage::token_usage::budget::admission::BudgetAdmissionHold,
        previous: Option<&crate::storage::token_usage::budget::admission::BudgetAdmissionWaitState>,
    ) -> Result<(
        crate::storage::token_usage::budget::admission::BudgetAdmissionWaitState,
        Option<Arc<()>>,
    )> {
        let ordinary = if capture.is_title()
            || capture.is_remember()
            || capture.compaction_subject().is_some()
        {
            None
        } else {
            let (agent, turn, generation) = self
                .work
                .ordinary_subject()
                .ok_or_else(|| suspended("original budget claim unavailable"))?;
            Some((
                AgentId::opaque(agent.to_owned())
                    .ok_or_else(|| suspended("original budget agent unavailable"))?,
                turn.to_owned(),
                generation,
            ))
        };
        let auxiliary_waiter = Arc::new(());
        let mut delay = 100u64;
        loop {
            let parked = match &ordinary {
                Some((agent, turn, generation)) => {
                    handle
                        .park_budget_provider_claim(
                            agent.clone(),
                            turn.clone(),
                            *generation,
                            capture.clone(),
                        )
                        .await?
                }
                None => {
                    handle
                        .park_budget_auxiliary_claim(
                            capture.clone(),
                            Arc::downgrade(&auxiliary_waiter),
                        )
                        .await?
                }
            };
            if parked {
                break;
            }
            tokio::time::sleep(Duration::from_millis(delay)).await;
            delay = delay.saturating_mul(2).min(2000);
        }
        let _inactive = self
            .deadline
            .as_ref()
            .ok_or_else(|| suspended("original budget active clock unavailable"))?
            .park()?;
        // Even a repeatedly raced fresh Capacity refusal is paced. This time
        // belongs to the acknowledged original park, not active execution.
        tokio::time::sleep(Duration::from_millis(delay)).await;
        loop {
            // Turn-less auxiliaries have no ordinary-turn monitor. Revalidate the
            // actual claim on every paced read, even while policy stays zero;
            // retained late-expense custody cannot keep cancelled work alive.
            if ordinary.is_none() {
                handle
                    .park_budget_auxiliary_claim(capture.clone(), Arc::downgrade(&auxiliary_waiter))
                    .await?;
            }
            let state = owner.observe_held().await?;
            // Avoid pointless writer churn while numerically exhausted, while
            // projection is updating, or finite roots still drain exposure.
            let may_reconsider = state.limits.is_some_and(|limits| {
                !limits.input.exhausted(state.known.input)
                    && !limits.output.exhausted(state.known.output)
                    && (!limits.requires_serial_inference() || !state.root_outstanding)
            });
            if reconsider_changed_hold(&state, previous, reason) && may_reconsider {
                let window = Arc::new(());
                let finite = state
                    .limits
                    .is_some_and(|limits| limits.requires_serial_inference());
                if !finite
                    || handle
                        .select_budget_reconsideration(
                            capture.clone(),
                            Arc::downgrade(&auxiliary_waiter),
                            Arc::downgrade(&window),
                        )
                        .await?
                {
                    let ready = match &ordinary {
                        Some((agent, turn, generation)) => {
                            handle
                                .reacquire_budget_provider_claim(
                                    agent.clone(),
                                    turn.clone(),
                                    *generation,
                                    capture.clone(),
                                )
                                .await?
                        }
                        None => {
                            handle
                                .resume_budget_auxiliary_claim(capture.clone())
                                .await?
                        }
                    };
                    if ready {
                        return Ok((state, finite.then_some(window)));
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(delay)).await;
            delay = delay.saturating_mul(2).min(2000);
        }
    }
}

/// One factory per original attempt, never shared as authority for a retry.
pub(super) struct BudgetAttemptAdmission(Arc<Source>);

impl BudgetAttemptAdmission {
    #[cfg(test)]
    pub(super) fn new(work: RuntimeBudgetAttemptWork) -> Self {
        Self::retaining(work, None, None, None, None)
    }

    fn retaining(
        work: RuntimeBudgetAttemptWork,
        waiter: Option<AsyncRuntimeSessionHandle>,
        deadline: Option<
            crate::integrations::agent::provider::budget_deadline::ProviderBudgetDeadline,
        >,
        completion: Option<CompletionSlot>,
        compaction_lane: Option<Arc<tokio::sync::RwLock<()>>>,
    ) -> Self {
        Self(Arc::new(Source {
            work,
            phase: Mutex::new(Phase::default()),
            waiter,
            deadline,
            completion,
            compaction_lane,
        }))
    }

    #[cfg(test)]
    pub(super) fn has_transport_retirement(
        &self,
        observation: &ProviderWireRequestObservation,
    ) -> bool {
        self.0
            .phase
            .lock()
            .unwrap()
            .retired
            .as_ref()
            .is_some_and(|proof| proof.matches(observation))
    }

    /// Expose the same original immutable work to connected fault fixtures;
    /// these captures do no I/O and cannot create transport or release proof.
    #[cfg(test)]
    pub(super) fn unused_retirement_for_test(
        &self,
        observation: &ProviderWireRequestObservation,
    ) -> Result<RuntimeBudgetReceiptWork> {
        self.0
            .phase
            .lock()
            .unwrap()
            .unused
            .as_ref()
            .unwrap()
            .freeze_retirement_receipt(observation)
    }

    #[cfg(test)]
    pub(super) fn receipt_stages_for_test(
        &self,
        observation: &ProviderWireRequestObservation,
    ) -> (RuntimeBudgetReceiptWork, Result<RuntimeBudgetReceiptWork>) {
        let phase = self.0.phase.lock().unwrap();
        let issued = phase.issued.as_ref().unwrap();
        (
            issued
                .registered()
                .freeze_wire_receipt(observation)
                .unwrap(),
            issued.freeze_retirement_receipt(
                observation,
                phase.retired.as_ref().unwrap(),
                phase.final_usage.as_ref(),
            ),
        )
    }
}

fn suspended(message: &'static str) -> MezError {
    MezError::new(MezErrorKind::BudgetSuspended, message)
}

impl ProviderObservationReceiptFactory for BudgetAttemptAdmission {
    fn plan(&self, observation: &ProviderWireRequestObservation) -> Result<()> {
        let phase = self
            .0
            .phase
            .lock()
            .map_err(|_| suspended("budget attempt custody unavailable"))?;
        if phase.finalized.is_some() {
            return Err(suspended("budget attempt already finalized"));
        }
        self.0.work.plan_wire_receipt_subject(observation)
    }

    fn before_send<'a>(
        &'a self,
        observation: &'a ProviderWireRequestObservation,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            {
                let mut phase = self
                    .0
                    .phase
                    .lock()
                    .map_err(|_| suspended("budget attempt custody unavailable"))?;
                self.0.work.plan_wire_receipt_subject(observation)?;
                if phase.started || phase.finalized.is_some() {
                    return Err(suspended("budget attempt cannot issue another exchange"));
                }
                phase.started = true;
            }
            if let Some(lane) = &self.0.compaction_lane {
                let reader = lane.clone().read_owned().await;
                let mut phase = self
                    .0
                    .phase
                    .lock()
                    .map_err(|_| suspended("budget attempt custody unavailable"))?;
                if phase.finalized.is_some() {
                    return Err(suspended(
                        "compaction attempt finalized before authorization",
                    ));
                }
                phase.compaction_reader = Some(reader);
            }
            let owner = self.0.work.admission_owner();
            let mut admission = owner
                .claim()
                .await
                .map_err(|_| suspended("original budget authorization unavailable"))?;
            let mut previous = None;
            while let RuntimeBudgetAttemptAdmission::Held(reason) = &admission {
                use crate::storage::token_usage::budget::admission::BudgetAdmissionHold;
                let Some(handle) = &self.0.waiter else { break };
                if *reason == BudgetAdmissionHold::RegistrationOnly {
                    break;
                }
                if let Some(lane) = &self.0.compaction_lane {
                    let needs_writer = {
                        let mut phase = self
                            .0
                            .phase
                            .lock()
                            .map_err(|_| suspended("budget attempt custody unavailable"))?;
                        // claim/retry has joined and positively refused this
                        // unissued request. Never upgrade while its writer may
                        // still authorize a send. Queued siblings hold no read
                        // guard, so Tokio's fair lock cannot deadlock the wave.
                        phase.compaction_reader.take();
                        phase.compaction_writer.is_none()
                    };
                    if needs_writer {
                        let writer = lane.clone().write_owned().await;
                        self.0
                            .phase
                            .lock()
                            .map_err(|_| suspended("budget attempt custody unavailable"))?
                            .compaction_writer = Some(writer);
                    }
                }
                let capture = owner
                    .capture_held()
                    .await
                    .map_err(|_| suspended("original budget hold custody unavailable"))?;
                let (state, reconsideration_window) = self
                    .0
                    .wait_for_reacquisition(handle, &owner, capture, *reason, previous.as_ref())
                    .await
                    .map_err(|_| suspended("original budget wait unavailable"))?;
                previous = Some(state);
                // Fair selection and actor timer adoption precede this exact
                // original positive hold's fresh native authorization.
                #[cfg(test)]
                self.0.work.await_title_grant_for_tests().await;
                admission = owner
                    .retry_held_with_window(reconsideration_window.clone())
                    .await
                    .map_err(|_| suspended("original budget reconsideration unavailable"))?;
                drop(reconsideration_window);
            }
            let RuntimeBudgetAttemptAdmission::Authorized(permit) = admission else {
                return Err(suspended(
                    "original budget authorization held or already issued",
                ));
            };
            let mut phase = self
                .0
                .phase
                .lock()
                .map_err(|_| suspended("budget attempt custody unavailable"))?;
            if phase.finalized.is_some() {
                phase.unused = Some(permit.retire_unused());
                return Err(suspended("budget attempt finalized during authorization"));
            }
            // No await between consumption and retaining its original custody.
            // The adapter then polls only this exchange's transport future.
            match permit
                .consume_if_current()
                .map_err(|_| suspended("original budget send custody invalidated"))?
            {
                Ok(issued) => phase.issued = Some(issued),
                Err(unused) => {
                    // A native grant may have completed after title retirement.
                    // Retain withdrawal/receipt custody; never silently drop it.
                    phase.unused = Some(unused);
                    return Err(suspended("original budget send custody invalidated"));
                }
            }
            Ok(())
        })
    }

    fn transport_retired(
        &self,
        observation: &ProviderWireRequestObservation,
        retirement: ProviderTransportRetirement,
    ) {
        // A failed/poisoned or foreign transfer remains unproven. Never turn
        // cleanup into permission, disk I/O, or a second async owner.
        if let Ok(mut phase) = self.0.phase.lock()
            && phase.issued.is_some()
            && phase.retired.is_none()
            && phase.finalized.is_none()
            && retirement.matches(observation)
            && self.0.work.plan_wire_receipt_subject(observation).is_ok()
        {
            phase.retired = Some(retirement);
        }
    }

    fn usage_finalized(
        &self,
        observation: &ProviderWireRequestObservation,
        usage: ProviderFinalUsage,
    ) -> Result<()> {
        let mut phase = self
            .0
            .phase
            .lock()
            .map_err(|_| suspended("budget attempt custody unavailable"))?;
        if phase.issued.is_none()
            || phase.finalized.is_some()
            || !usage.matches_subject(observation)
        {
            return Err(MezError::conflict("budget final usage custody changed"));
        }
        self.0.work.plan_wire_receipt_subject(observation)?;
        if let Some(original) = &phase.final_usage {
            if original != &usage {
                return Err(MezError::conflict("budget final usage already captured"));
            }
        } else {
            phase.final_usage = Some(usage);
        }
        Ok(())
    }

    fn freeze(
        &self,
        observation: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceipt>> {
        let mut phase = self
            .0
            .phase
            .lock()
            .map_err(|_| suspended("budget attempt custody unavailable"))?;
        if let Some(original) = &phase.finalized {
            if original != observation {
                return Err(MezError::conflict("budget attempt final evidence changed"));
            }
        } else {
            phase.finalized = Some(observation.clone());
        }
        Ok(Arc::new(Receipt {
            source: self.0.clone(),
            observation: observation.clone(),
        }))
    }
}

struct Receipt {
    source: Arc<Source>,
    observation: ProviderWireRequestObservation,
}

impl Receipt {
    async fn work(&self) -> Result<Option<RuntimeBudgetReceiptWork>> {
        // Observe the actual original authorization worker before querying.
        // Recovery supplies receipt evidence ONLY, never a replacement permit.
        let outcome = self.source.work.admission_owner().reconcile().await?;
        if let Some(outcome) = outcome {
            let registered = self.source.work.registered_from_admission(&outcome)?;
            return registered.freeze_wire_receipt(&self.observation).map(Some);
        }
        if self.observation.usage.is_some()
            || self
                .observation
                .usage_presence
                .is_some_and(|presence| presence.any())
        {
            return Err(MezError::conflict(
                "unissued budget attempt reported expense",
            ));
        }
        // A positively absent, non-issued attempt has no receipt to write. This
        // is not fabricated zero usage or an authority to repeat authorization.
        Ok(None)
    }
}

impl ProviderObservationReceipt for Receipt {
    fn checkpoint(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let Some(work) = self.work().await? else {
                // Original authorization has joined with positive absence.
                let mut phase = self
                    .source
                    .phase
                    .lock()
                    .map_err(|_| suspended("budget attempt custody unavailable"))?;
                phase.compaction_reader.take();
                phase.compaction_writer.take();
                return Ok(());
            };
            checkpoint_work(work).await?;
            // Reconciliation above joined the original writer, but descriptive
            // evidence alone is insufficient. Only a still-retained permit can
            // be withdrawn, and transfer into Phase has no intervening await.
            let unused = self.source.work.admission_owner().retire_unused().await?;
            let retirement = {
                let mut phase = self
                    .source
                    .phase
                    .lock()
                    .map_err(|_| suspended("budget attempt custody unavailable"))?;
                if let Some(unused) = unused {
                    phase.unused = Some(unused);
                }
                match (&phase.issued, &phase.retired) {
                    (Some(issued), Some(proof)) => Some(issued.freeze_retirement_receipt(
                        &self.observation,
                        proof,
                        phase.final_usage.as_ref(),
                    )?),
                    _ => phase
                        .unused
                        .as_ref()
                        .map(|unused| unused.freeze_retirement_receipt(&self.observation))
                        .transpose()?,
                }
            };
            if let Some(work) = retirement {
                checkpoint_work(work.clone()).await?;
                // An unused permit receipt never qualifies provider output.
                // The exact issued descriptor is published synchronously after
                // commit/reconciliation, with no intervening cancellation gap.
                if let Some(completion) = &self.source.completion
                    && self
                        .source
                        .phase
                        .lock()
                        .map_err(|_| suspended("budget attempt custody unavailable"))?
                        .issued
                        .is_some()
                {
                    completion.publish(work)?;
                }
                // All original native writers and transport custody are now
                // observed and the terminal receipt is durable. Prefix-only or
                // failed checkpoints must not let another node park the lease.
                let mut phase = self
                    .source
                    .phase
                    .lock()
                    .map_err(|_| suspended("budget attempt custody unavailable"))?;
                phase.compaction_reader.take();
                phase.compaction_writer.take();
            }
            Ok(())
        })
    }
}

/// One immutable writer/query owner per stage; cancellation never starts a
/// replacement, and failure does not automatically repeat an uncertain write.
async fn checkpoint_work(work: RuntimeBudgetReceiptWork) -> Result<()> {
    let owner = work.commit_owner();
    match owner.commit().await {
        Ok(_) => Ok(()),
        Err(original) => match owner.reconcile().await? {
            Some(_) => Ok(()),
            None => Err(original),
        },
    }
}
