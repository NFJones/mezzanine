//! Frozen request expense custody shared by ordinary and turn-less inference.
//!
//! Registration remains bookkeeping only. Native authorization additionally
//! checks current durable policy and exposure in one transaction; only a new
//! grant returns a one-use permit. Physical-send/retirement driver assembly is
//! still downstream. Neither ACK recovery nor receipt replay reissues inference.

use super::*;
use crate::storage::token_usage::AccountingOrigin;
use crate::storage::token_usage::budget::admission::{
    BudgetAdmissionHold, BudgetAdmissionOutcome, BudgetAttemptAdmission, BudgetIssuedAttempt,
    BudgetSendPermit,
};
use crate::storage::token_usage::budget::receipts::{
    BudgetAttempt, BudgetAttemptPurpose, BudgetReceipt, BudgetReceiptOutcome,
};
use crate::storage::token_usage::budget::reporting::{
    BudgetReportingObservation, BudgetReportingOrigin,
};
use mez_agent::token_budget::{BudgetAttemptId, UsageSnapshot};

/// Actual issued-request custody, independent of later actor/pane replacement.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBudgetRequestOwner {
    pub(crate) member: BudgetMembership,
    pub(crate) lease: BudgetBindingLease,
    pub(super) binding_owner: String,
    pub(super) store: TokenUsageStore,
    pub(super) origin: AccountingOrigin,
    /// Retained through every concrete attempt/journal clone. This narrows
    /// expense allocation only; task liveness requires separate actor checks.
    pub(super) auxiliary: Option<super::auxiliary::RuntimeBudgetAuxiliaryClaim>,
    /// Current title claim only; never keeps publication authority alive.
    pub(in crate::runtime::agent) title_fence:
        Option<std::sync::Weak<std::sync::atomic::AtomicBool>>,
    #[cfg(test)]
    pub(super) title_grant_gate: Option<Arc<(tokio::sync::Notify, tokio::sync::Notify)>>,
}

impl RuntimeBudgetRequestOwner {
    /// Request-local acceptance seam: gate after actor resume but before fresh
    /// native reconsideration, without retaining an unobserved local permit.
    #[cfg(test)]
    pub(crate) fn gate_title_grant_for_tests(
        &mut self,
        gate: Arc<(tokio::sync::Notify, tokio::sync::Notify)>,
    ) {
        self.title_grant_gate = Some(gate);
    }

    /// Pure exact-subject check. This is neither a policy permit nor disk I/O.
    pub(crate) fn validate_request(&self, conversation: &str) -> Result<()> {
        if self.member.conversation != conversation
            || !self.lease.matches(&self.member, &self.binding_owner)
        {
            return Err(MezError::conflict("budget request subject changed"));
        }
        Ok(())
    }

    /// Capture once for ONE planned concrete attempt, before any provider send.
    /// Retry of registration must reuse this value, not allocate another ID.
    pub(crate) fn prepare_attempt(
        &self,
        model: mez_agent::ModelTokenUsageKey,
        purpose: BudgetAttemptPurpose,
    ) -> Result<RuntimeBudgetAttemptWork> {
        self.validate_request(&self.member.conversation)?;
        if self
            .auxiliary
            .as_ref()
            .is_some_and(|claim| claim.purpose() != purpose)
        {
            return Err(MezError::conflict(
                "auxiliary budget request purpose changed",
            ));
        }
        let attempt = BudgetAttempt {
            namespace: self.member.namespace.clone(),
            id: BudgetAttemptId::new(crate::storage::token_usage::new_token_usage_event_id())
                .map_err(|error| MezError::invalid_state(error.to_string()))?,
            root: self.member.root.clone(),
            conversation: self.member.conversation.clone(),
            model,
            purpose,
            reporting_origin: Some(BudgetReportingOrigin::capture(&self.origin)),
        };
        Ok(RuntimeBudgetAttemptWork(Arc::new(AttemptSource {
            owner: self.clone(),
            attempt,
            wire_subject: std::sync::OnceLock::new(),
            wire: std::sync::OnceLock::new(),
            retirement: std::sync::OnceLock::new(),
            admission: std::sync::OnceLock::new(),
        })))
    }
}

#[derive(Debug)]
struct AttemptSource {
    owner: RuntimeBudgetRequestOwner,
    attempt: BudgetAttempt,
    wire_subject:
        std::sync::OnceLock<crate::integrations::agent::provider::ProviderWireRequestObservation>,
    wire: std::sync::OnceLock<WireReceiptCapture>,
    retirement: std::sync::OnceLock<WireRetirementCapture>,
    admission: std::sync::OnceLock<super::admission_work::SharedAdmission>,
}

/// Data and shared writer state only: no reverse reference to AttemptSource.
#[derive(Debug)]
struct WireReceiptCapture {
    observation: crate::integrations::agent::provider::ProviderWireRequestObservation,
    receipt: Result<BudgetReceipt>,
    commit: super::receipt_commit::SharedReceiptCommit,
}

/// Data only, without a reverse AttemptSource reference or an idle Arc cycle.
#[derive(Debug)]
struct WireRetirementCapture {
    receipt: Result<BudgetReceipt>,
    evidence: crate::storage::token_usage::budget::retirement::BudgetExposureRetirement,
    commit: super::receipt_commit::SharedReceiptCommit,
}

impl AttemptSource {
    fn plan_wire_receipt_subject(
        &self,
        observation: &crate::integrations::agent::provider::ProviderWireRequestObservation,
    ) -> Result<()> {
        if self.wire.get().is_some() {
            return Err(MezError::conflict(
                "registered budget attempt already owns finalized evidence",
            ));
        }
        self.bind_wire_receipt_subject(observation)
    }

    fn bind_wire_receipt_subject(
        &self,
        observation: &crate::integrations::agent::provider::ProviderWireRequestObservation,
    ) -> Result<()> {
        use crate::integrations::agent::provider::ProviderRequestPurpose as Purpose;
        let original = &self.attempt;
        let purpose = match original.purpose {
            BudgetAttemptPurpose::Execution => Purpose::Execution,
            BudgetAttemptPurpose::Routing => Purpose::Routing,
            BudgetAttemptPurpose::Compaction => Purpose::Compaction,
            BudgetAttemptPurpose::Memory => Purpose::Memory,
            BudgetAttemptPurpose::Title => Purpose::Title,
            BudgetAttemptPurpose::MacroJudge => Purpose::MacroJudge,
            BudgetAttemptPurpose::SandboxAssessment => Purpose::SandboxAssessment,
            BudgetAttemptPurpose::FailureSummary => Purpose::FailureSummary,
        };
        if observation.conversation_id != original.conversation
            || observation.provider != original.model.provider
            || observation.model != original.model.model
            || observation.purpose != purpose
            || observation.request_id.is_empty()
            || observation.request_id.len() > 256
        {
            return Err(MezError::conflict(
                "budget wire receipt original identity changed",
            ));
        }
        let bound = self.wire_subject.get_or_init(|| observation.clone());
        if !bound.same_wire_subject(observation) {
            return Err(MezError::conflict(
                "registered budget attempt already owns another exchange",
            ));
        }
        Ok(())
    }
}

/// Immutable original registration work; keep it until the ACK is reconciled.
#[derive(Debug, Clone)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "concrete attempt admission is downstream")
)]
pub(crate) struct RuntimeBudgetAttemptWork(Arc<AttemptSource>);

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "concrete attempt admission is downstream")
)]
impl RuntimeBudgetAttemptWork {
    /// Bind the original subject before authorization. This pure operation
    /// neither registers an attempt nor returns permission to send.
    pub(crate) fn plan_wire_receipt_subject(
        &self,
        observation: &crate::integrations::agent::provider::ProviderWireRequestObservation,
    ) -> Result<()> {
        self.0.plan_wire_receipt_subject(observation)
    }

    /// Original positive admission evidence can recover a receipt descriptor,
    /// never a send permit. The caller obtains it by exact read-only recovery.
    pub(crate) fn registered_from_admission(
        &self,
        outcome: &BudgetAdmissionOutcome,
    ) -> Result<RuntimeBudgetRegisteredAttempt> {
        let original = &self.0.attempt;
        if outcome.namespace != original.namespace
            || outcome.attempt != original.id
            || outcome.root != original.root
            || outcome.conversation != original.conversation
            || outcome.owner != self.0.owner.binding_owner
        {
            return Err(MezError::conflict(
                "budget admission evidence changed owner",
            ));
        }
        Ok(RuntimeBudgetRegisteredAttempt(self.0.clone()))
    }

    /// Capture before a cancellable admission wait. The existing request or
    /// journal owner must retain this source through actual worker observation.
    pub(crate) fn admission_owner(&self) -> super::admission_work::RuntimeBudgetAdmissionWork {
        super::admission_work::RuntimeBudgetAdmissionWork::new(
            self.clone(),
            self.0.admission.get_or_init(Default::default).clone(),
        )
    }

    /// Pure comparison with a current actor capture. This original request's
    /// native lease and binding remain mandatory; equal root labels alone do
    /// not authorize parking or attach another request's expense owner.
    pub(super) fn matches_claim(&self, agent: &str, turn: &str, generation: u64) -> bool {
        self.0.wire_subject.get().is_some_and(|subject| {
            subject.agent_id == agent
                && subject.turn_id == turn
                && subject.claim_generation == Some(generation)
        })
    }

    pub(super) fn matches_owner(&self, owner: &RuntimeBudgetRequestOwner) -> bool {
        let original = &self.0.owner;
        original.member == owner.member
            && original.binding_owner == owner.binding_owner
            && original.store.path() == owner.store.path()
            && original.auxiliary == owner.auxiliary
            && original
                .validate_request(&owner.member.conversation)
                .is_ok()
            && owner.validate_request(&owner.member.conversation).is_ok()
    }

    pub(super) fn auxiliary_claim(&self) -> Option<&super::auxiliary::RuntimeBudgetAuxiliaryClaim> {
        self.0.owner.auxiliary.as_ref()
    }

    /// Called only after the native binding validation. This atomic admission
    /// point orders against claim retirement; no await or I/O follows before
    /// the original permit is consumed. Prior admission remains issued exposure.
    fn title_is_live(&self) -> bool {
        if !matches!(
            self.0.owner.auxiliary,
            Some(super::auxiliary::RuntimeBudgetAuxiliaryClaim::Title { .. })
        ) {
            return true;
        }
        self.0
            .owner
            .title_fence
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .is_some_and(|live| live.load(std::sync::atomic::Ordering::SeqCst))
    }

    #[cfg(test)]
    pub(crate) async fn await_title_grant_for_tests(&self) {
        if let Some(gate) = &self.0.owner.title_grant_gate {
            gate.0.notify_one();
            gate.1.notified().await;
        }
    }

    pub(super) fn wrap_permit(&self, permit: BudgetSendPermit) -> RuntimeBudgetSendPermit {
        RuntimeBudgetSendPermit {
            source: self.0.clone(),
            permit,
        }
    }

    /// Worker-only actual-admission port, not preparation-time permission. The
    /// existing frozen identity survives a held result or uncertain grant ACK.
    pub(crate) fn authorize(&self) -> Result<RuntimeBudgetAttemptAdmission> {
        let owner = &self.0.owner;
        match owner.store.authorize_budget_attempt(
            &self.0.attempt,
            &owner.member,
            &owner.binding_owner,
            &owner.lease,
        )? {
            BudgetAttemptAdmission::Authorized(permit) => Ok(
                RuntimeBudgetAttemptAdmission::Authorized(RuntimeBudgetSendPermit {
                    source: self.0.clone(),
                    permit,
                }),
            ),
            BudgetAttemptAdmission::PreviouslyAuthorized(outcome) => {
                Ok(RuntimeBudgetAttemptAdmission::PreviouslyAuthorized(outcome))
            }
            BudgetAttemptAdmission::Held(reason) => Ok(RuntimeBudgetAttemptAdmission::Held(reason)),
        }
    }

    /// Existing-only read of original authorization evidence, never a permit.
    pub(crate) fn reconcile_admission(&self) -> Result<Option<BudgetAdmissionOutcome>> {
        self.0.owner.store.budget_attempt_admission(&self.0.attempt)
    }

    /// Worker-only wait dependency observation from this original request's
    /// store and native binding. A changed snapshot cannot authorize any send.
    pub(crate) fn observe_admission_wait(
        &self,
    ) -> Result<crate::storage::token_usage::budget::admission::BudgetAdmissionWaitState> {
        let owner = &self.0.owner;
        owner
            .store
            .budget_admission_wait_state(&owner.member, &owner.binding_owner, &owner.lease)
    }

    /// Blocking-worker only. A successful database registration is NOT a spend
    /// permit, proof of a send, or authority to repeat an uncertain remote call.
    pub(crate) fn execute(&self) -> Result<RuntimeBudgetRegisteredAttempt> {
        self.0
            .owner
            .store
            .register_budget_attempt(&self.0.attempt)?;
        Ok(RuntimeBudgetRegisteredAttempt(self.0.clone()))
    }

    /// Blocking-worker read-only recovery; absence grants no retry authority.
    pub(crate) fn reconcile_registration(&self) -> Result<Option<RuntimeBudgetRegisteredAttempt>> {
        let exists = self
            .0
            .owner
            .store
            .budget_attempt_registration(&self.0.attempt)?;
        Ok(exists.then(|| RuntimeBudgetRegisteredAttempt(self.0.clone())))
    }
}

/// Native grant result; replay evidence cannot be consumed as another permit.
#[derive(Debug)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "physical-send driver integration is downstream")
)]
pub(crate) enum RuntimeBudgetAttemptAdmission {
    Authorized(RuntimeBudgetSendPermit),
    PreviouslyAuthorized(BudgetAdmissionOutcome),
    Held(BudgetAdmissionHold),
}

#[derive(Debug)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "physical-send driver integration is downstream")
)]
pub(crate) struct RuntimeBudgetSendPermit {
    source: Arc<AttemptSource>,
    permit: BudgetSendPermit,
}

impl RuntimeBudgetSendPermit {
    /// Outer errors are native binding failures; an inner refusal retains the
    /// exact unused permit for withdrawal and durable terminal settlement.
    pub(crate) fn consume_if_current(
        self,
    ) -> Result<std::result::Result<RuntimeBudgetIssuedAttempt, RuntimeBudgetUnusedAttempt>> {
        if !matches!(
            self.source.owner.auxiliary,
            Some(super::auxiliary::RuntimeBudgetAuxiliaryClaim::Title { .. })
        ) {
            return Ok(Ok(self.consume()?));
        }
        let source = self.source;
        match self
            .permit
            .consume_if_live(|| RuntimeBudgetAttemptWork(source.clone()).title_is_live())?
        {
            Ok(issued) => Ok(Ok(RuntimeBudgetIssuedAttempt {
                registered: RuntimeBudgetRegisteredAttempt(source),
                issued,
            })),
            Err(permit) => Ok(Err(RuntimeBudgetUnusedAttempt {
                registered: RuntimeBudgetRegisteredAttempt(source),
                permit,
            })),
        }
    }

    /// The retained worker stores only the native permit, never a reverse Arc
    /// to the AttemptSource that owns that worker's shared state.
    pub(super) fn into_native(self) -> BudgetSendPermit {
        self.permit
    }

    /// Withdraw original send authority without consuming or revalidating it.
    /// The resulting owner exposes settlement only, never a send operation.
    pub(crate) fn retire_unused(self) -> RuntimeBudgetUnusedAttempt {
        RuntimeBudgetUnusedAttempt {
            registered: RuntimeBudgetRegisteredAttempt(self.source),
            permit: self.permit,
        }
    }

    /// Consume once off actor immediately before the physical send, retaining
    /// original request/native custody for settlement and actual retirement.
    pub(crate) fn consume(self) -> Result<RuntimeBudgetIssuedAttempt> {
        Ok(RuntimeBudgetIssuedAttempt {
            registered: RuntimeBudgetRegisteredAttempt(self.source),
            issued: self.permit.consume()?,
        })
    }
}

/// Non-cloneable consumed grant. Its original expense descriptor may be cloned
/// for receipt work, but neither value supplies another send permission.
#[derive(Debug)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "physical-send driver integration is downstream")
)]
pub(crate) struct RuntimeBudgetIssuedAttempt {
    registered: RuntimeBudgetRegisteredAttempt,
    issued: BudgetIssuedAttempt,
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "physical-send driver integration is downstream")
)]
impl RuntimeBudgetIssuedAttempt {
    /// Pure capture from consumed original custody AND sealed local retirement.
    /// Only separate source-qualified final evidence completes dimensions;
    /// absent evidence never upgrades partial counters. Its writer follows the
    /// original subtotal's ACK and retains independent transport qualification.
    pub(crate) fn freeze_retirement_receipt(
        &self,
        observation: &crate::integrations::agent::provider::ProviderWireRequestObservation,
        proof: &crate::integrations::agent::provider::ProviderTransportRetirement,
        final_usage: Option<&crate::integrations::agent::provider::ProviderFinalUsage>,
    ) -> Result<RuntimeBudgetReceiptWork> {
        if !proof.matches(observation) {
            return Err(MezError::conflict(
                "budget transport retirement subject changed",
            ));
        }
        let terminal = final_usage
            .map(|usage| usage.terminal_snapshot(observation))
            .transpose()?
            .unwrap_or(UsageSnapshot {
                terminal: true,
                ..Default::default()
            });
        self.registered
            .freeze_exposure_retirement(observation, self.outcome(), terminal)
    }

    pub(crate) fn registered(&self) -> &RuntimeBudgetRegisteredAttempt {
        &self.registered
    }

    pub(crate) fn outcome(&self) -> &BudgetAdmissionOutcome {
        self.issued.outcome()
    }
}

/// Positive registration ACK bound to the same original request descriptor.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBudgetRegisteredAttempt(Arc<AttemptSource>);

/// Irreversibly withdrawn original permit. Not Clone, reconstructible from
/// evidence, or consumable by a transport. Retains its original native lease.
#[derive(Debug)]
pub(crate) struct RuntimeBudgetUnusedAttempt {
    registered: RuntimeBudgetRegisteredAttempt,
    permit: BudgetSendPermit,
}

impl RuntimeBudgetUnusedAttempt {
    pub(crate) fn freeze_retirement_receipt(
        &self,
        observation: &crate::integrations::agent::provider::ProviderWireRequestObservation,
    ) -> Result<RuntimeBudgetReceiptWork> {
        if observation.usage.is_some()
            || observation
                .usage_presence
                .is_some_and(|presence| presence.any())
        {
            return Err(MezError::conflict("unused budget attempt reported expense"));
        }
        self.registered.freeze_exposure_retirement(
            observation,
            self.permit.outcome(),
            UsageSnapshot {
                terminal: true,
                ..Default::default()
            },
        )
    }
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "receipt driver integration is downstream")
)]
impl RuntimeBudgetRegisteredAttempt {
    /// Shared immutable terminal stage, called only by consumed-and-retired or
    /// withdrawn-unused original custody. Descriptive grant evidence is not a
    /// public entry point to qualify exposure release.
    fn freeze_exposure_retirement(
        &self,
        observation: &crate::integrations::agent::provider::ProviderWireRequestObservation,
        outcome: &BudgetAdmissionOutcome,
        terminal: UsageSnapshot,
    ) -> Result<RuntimeBudgetReceiptWork> {
        let prefix = self.freeze_wire_receipt(observation)?;
        let evidence =
            crate::storage::token_usage::budget::retirement::BudgetExposureRetirement::with_terminal(
                outcome.clone(),
                prefix.0.receipt.clone(),
                terminal,
            );
        let captured = self.0.retirement.get_or_init(|| WireRetirementCapture {
            receipt: BudgetReceiptId::new(crate::storage::token_usage::new_token_usage_event_id())
                .map_err(|e| MezError::invalid_state(e.to_string()))
                .map(|id| BudgetReceipt {
                    id,
                    attempt: self.0.attempt.clone(),
                    snapshot: terminal,
                    reporting: None,
                }),
            evidence: evidence.clone(),
            commit: Default::default(),
        });
        if captured.evidence != evidence {
            return Err(MezError::conflict(
                "budget original retirement evidence changed",
            ));
        }
        Ok(RuntimeBudgetReceiptWork(Arc::new(ReceiptSource {
            attempt: self.clone(),
            receipt: captured.receipt.clone()?,
            retirement: Some(captured.evidence.clone()),
            commit: std::sync::OnceLock::from(captured.commit.clone()),
        })))
    }

    /// Pure original-subject binding before transport. Descriptor clones share
    /// this slot; neither an exact repeat nor a new factory can mint permission.
    pub(crate) fn plan_wire_receipt_subject(
        &self,
        observation: &crate::integrations::agent::provider::ProviderWireRequestObservation,
    ) -> Result<()> {
        self.0.plan_wire_receipt_subject(observation)
    }

    fn bind_wire_receipt_subject(
        &self,
        observation: &crate::integrations::agent::provider::ProviderWireRequestObservation,
    ) -> Result<()> {
        self.0.bind_wire_receipt_subject(observation)
    }

    /// Pure capture of a native wire subtotal, not final coverage or retirement.
    /// Cache-only/reasoning-only evidence is a lower bound, never an invented
    /// missing primary zero. Original reporting remains on its existing path.
    pub(crate) fn freeze_wire_receipt(
        &self,
        observation: &crate::integrations::agent::provider::ProviderWireRequestObservation,
    ) -> Result<RuntimeBudgetReceiptWork> {
        let captured = self.0.wire.get_or_init(|| WireReceiptCapture {
            observation: observation.clone(),
            receipt: self.capture_wire_receipt(observation),
            commit: Default::default(),
        });
        if captured.observation != *observation {
            return Err(MezError::conflict(
                "registered budget attempt already owns another exchange",
            ));
        }
        Ok(RuntimeBudgetReceiptWork(Arc::new(ReceiptSource {
            attempt: self.clone(),
            receipt: captured.receipt.clone()?,
            retirement: None,
            commit: std::sync::OnceLock::from(captured.commit.clone()),
        })))
    }

    fn capture_wire_receipt(
        &self,
        observation: &crate::integrations::agent::provider::ProviderWireRequestObservation,
    ) -> Result<BudgetReceipt> {
        self.bind_wire_receipt_subject(observation)?;
        self.capture_wire_usage(observation)
    }

    fn capture_wire_usage(
        &self,
        observation: &crate::integrations::agent::provider::ProviderWireRequestObservation,
    ) -> Result<BudgetReceipt> {
        let presence = observation.usage_presence.ok_or_else(|| {
            MezError::invalid_state("budget wire receipt requires qualified counter evidence")
        })?;
        let mut snapshot = UsageSnapshot::default();
        if let Some(usage) = observation.usage {
            let cache =
                usage.cached_input_tokens.is_some() || usage.cache_write_input_tokens.is_some();
            let cache_lower_bound = usage
                .cached_input_tokens
                .unwrap_or(0)
                .checked_add(usage.cache_write_input_tokens.unwrap_or(0))
                .ok_or_else(|| MezError::conflict("budget cache subtotal overflow"))?;
            if presence.input && cache_lower_bound > usage.input_tokens {
                return Err(MezError::conflict("budget input contradicts cache subsets"));
            }
            if !presence.input && usage.input_tokens > cache_lower_bound {
                return Err(MezError::conflict(
                    "budget input lacks original counter evidence",
                ));
            }
            if !presence.output && usage.output_tokens != 0 {
                return Err(MezError::conflict(
                    "budget output lacks original counter evidence",
                ));
            }
            if presence.output && usage.reasoning_tokens > usage.output_tokens {
                return Err(MezError::conflict(
                    "budget output contradicts reasoning subset",
                ));
            }
            if presence.input || cache {
                snapshot.input = Some(usage.input_tokens.max(cache_lower_bound));
            }
            snapshot.output = if presence.output {
                Some(usage.output_tokens)
            } else {
                (usage.reasoning_tokens != 0).then_some(usage.reasoning_tokens)
            };
        } else if presence.input || presence.output {
            return Err(MezError::conflict(
                "budget reported counters lost their values",
            ));
        }
        Ok(BudgetReceipt {
            id: BudgetReceiptId::new(crate::storage::token_usage::new_token_usage_event_id())
                .map_err(|error| MezError::invalid_state(error.to_string()))?,
            attempt: self.0.attempt.clone(),
            snapshot,
            // Actor ingress still owns the existing rolling-reporting event.
            // Do not project a second expense event for the same exchange.
            reporting: None,
        })
    }

    /// Freeze reported knowledge once before cancellable delivery. Reporting
    /// time/counters and receipt ID survive retries byte-for-byte; missing
    /// dimensions remain missing rather than becoming an invented zero.
    pub(crate) fn freeze_receipt(
        &self,
        snapshot: UsageSnapshot,
        reporting: Option<BudgetReportingObservation>,
    ) -> Result<RuntimeBudgetReceiptWork> {
        let receipt = BudgetReceipt {
            id: BudgetReceiptId::new(crate::storage::token_usage::new_token_usage_event_id())
                .map_err(|error| MezError::invalid_state(error.to_string()))?,
            attempt: self.0.attempt.clone(),
            snapshot,
            reporting,
        };
        Ok(RuntimeBudgetReceiptWork(Arc::new(ReceiptSource {
            attempt: self.clone(),
            receipt,
            retirement: None,
            commit: std::sync::OnceLock::new(),
        })))
    }
}

#[derive(Debug)]
struct ReceiptSource {
    attempt: RuntimeBudgetRegisteredAttempt,
    receipt: BudgetReceipt,
    retirement: Option<crate::storage::token_usage::budget::retirement::BudgetExposureRetirement>,
    commit: std::sync::OnceLock<super::receipt_commit::SharedReceiptCommit>,
}

/// Exact known usage plus original custody retained until real durable ACK.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBudgetReceiptWork(Arc<ReceiptSource>);

impl RuntimeBudgetReceiptWork {
    /// Pure capture; every clone of this immutable receipt shares one writer.
    /// The service must retain this owner outside cancellable ACK waiters.
    pub(crate) fn commit_owner(&self) -> super::receipt_commit::RuntimeBudgetReceiptCommit {
        super::receipt_commit::RuntimeBudgetReceiptCommit::new(
            self.clone(),
            self.0.commit.get_or_init(Default::default).clone(),
        )
    }

    /// Blocking-worker only; repeat ONLY this immutable bookkeeping work after
    /// reconciliation, never the provider/effects that produced its counters.
    pub(crate) fn execute(&self) -> Result<BudgetReceiptOutcome> {
        if let Some(evidence) = &self.0.retirement {
            return self
                .0
                .attempt
                .0
                .owner
                .store
                .commit_budget_retirement_receipt(&self.0.receipt, evidence);
        }
        self.0
            .attempt
            .0
            .owner
            .store
            .commit_budget_receipt(&self.0.receipt)
    }

    /// Query the original committed outcome without repeating settlement.
    pub(crate) fn reconcile(&self) -> Result<Option<BudgetReceiptOutcome>> {
        if let Some(evidence) = &self.0.retirement {
            return self
                .0
                .attempt
                .0
                .owner
                .store
                .budget_retirement_receipt_outcome(&self.0.receipt, evidence);
        }
        self.0
            .attempt
            .0
            .owner
            .store
            .budget_receipt_outcome(&self.0.receipt)
    }
}
