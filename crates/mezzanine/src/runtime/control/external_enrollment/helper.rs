//! Qualified presentation helpers for an independently enrolled producer.
//!
//! A private current run handle or bounded daemon-owned selector index narrows
//! the target. The original public generation fences both paths, and native
//! direct-parent capture must match that run's retained producer anchor. Parent ancestry alone
//! never enrolls an owner. Observation stays on the existing bounded worker;
//! actor settlement rechecks exact authority before the existing presentation
//! reducer runs. Helpers acquire no role, accounting, lease or observer ownership.

use super::*;

/// Private callback data and an independently retained producer lifetime.
/// Secret payload formatting prevents credentials or title data in work Debug.
#[derive(Debug, Clone)]
pub(super) struct HelperPresentation {
    digest: [u8; 32],
    generation: u64,
    pub(super) producer: ProducerEvidence,
    params: SecretString,
    /// Explicit freshness proof is separate from generic presentation mutation.
    heartbeat: bool,
    /// Request origin is distinct from presentation/freshness and never expense.
    usage_begin: Option<super::helper_usage::HelperUsageBegin>,
    /// Shared ledger reservation; native observation must precede execution.
    pub(super) usage_end: Option<Box<super::super::external_usage::ExternalUsageWork>>,
    /// Metadata-only original Codex callback; daemon owns its sequence/replay fence.
    pub(super) codex: Option<super::codex::CodexEvent>,
    /// Holds this source's ingress ticket through complete actor settlement.
    pub(super) codex_order: Option<Arc<super::codex_order::CodexOrderLease>>,
}

impl HelperPresentation {
    /// Runs only after the original native direct-parent ancestry publication.
    pub(super) fn observe_usage(&self, work: &ExternalEnrollmentWork) -> Result<()> {
        if let Some(begin) = &self.usage_begin {
            begin.observe(work)?;
        }
        if let Some(end) = &self.usage_end {
            end.execute()?;
        }
        Ok(())
    }
}

impl RuntimeSessionService {
    /// Reserves bounded observation only for a current ordinary run candidate on
    /// uninitialized, same-user, sender-qualified Unix ingress. Supplied process
    /// IDs, pane hints and arbitrary callback methods are not accepted.
    pub(crate) fn prepare_external_helper_presentation(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
    ) -> Result<ExternalEnrollmentWork> {
        if request.method == "agent/external/codex-observe" {
            return self.prepare_external_codex_observation(request, connection);
        }
        self.require_live()?;
        if !matches!(
            request.method.as_str(),
            "agent/external/helper-presentation"
                | "agent/external/helper-observe"
                | "agent/external/curated-heartbeat"
                | "agent/external/curated-usage-begin"
                | "agent/external/curated-usage-end"
        ) || connection.initialized()
        {
            return Err(MezError::forbidden(
                "external helper requires uninitialized Unix ingress",
            ));
        }
        let Some(AuthenticatedPeer::UnixUser { uid }) = connection.authenticated_peer() else {
            return Err(MezError::forbidden(
                "external helper requires authenticated Unix transport",
            ));
        };
        crate::control::validate_control_method_params_schema(request)?;
        let raw = request.params.as_deref().unwrap_or("{}");
        if raw.len() > 4096 {
            return Err(MezError::invalid_args(
                "external helper metadata exceeds limit",
            ));
        }
        let mut params = crate::protocol::strict_json::decode(raw.as_bytes())
            .map_err(|_| MezError::invalid_args("external helper requires unique object fields"))?;
        let session_id = text(&params, "external_session_id", 128)?;
        let origin = connection
            .unix_origin()
            .filter(|origin| origin.uid() == *uid && origin.writer_confirmed())
            .ok_or_else(|| MezError::forbidden("external helper sender unavailable"))?
            .clone();
        let heartbeat = request.method == "agent/external/curated-heartbeat";
        let usage_begin = request.method == "agent/external/curated-usage-begin";
        let usage_end = request.method == "agent/external/curated-usage-end";
        if heartbeat
            && !params
                .get("sequence")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|sequence| sequence > 0 && sequence <= 9_007_199_254_740_991)
        {
            return Err(MezError::invalid_args(
                "curated observer sequence unavailable",
            ));
        }
        let digest = if request.method == "agent/external/helper-observe"
            || heartbeat
            || usage_begin
            || usage_end
        {
            let harness = text(&params, "harness", 64)?;
            crate::integrations::harness_policy::require_active_external_harness(&harness)?;
            let candidates = self
                .control
                .external_agents()
                .enrollments
                .helper_targets
                .candidates(*uid, &harness, &session_id);
            for candidate in candidates {
                self.reconcile_external_agent_registration(candidate);
            }
            let targets = &self.control.external_agents().enrollments.helper_targets;
            if heartbeat || usage_begin || usage_end {
                targets.select_witness(
                    *uid,
                    &harness,
                    &session_id,
                    &text(&params, "observer_witness", 64)?,
                )?
            } else {
                targets.select(*uid, &harness, &session_id)?
            }
        } else {
            super::super::external_agents::credential(&params)?
        };
        self.reconcile_external_agent_registration(digest);
        if (request.method == "agent/external/helper-observe"
            || heartbeat
            || usage_begin
            || usage_end)
            && text(&params, "observer_witness", 64)? != observer_witness(digest)
        {
            return Err(MezError::forbidden(
                "external helper observer instance unavailable",
            ));
        }
        let generation = params
            .get("generation")
            .and_then(serde_json::Value::as_u64)
            .filter(|generation| *generation > 0)
            .ok_or_else(|| MezError::forbidden("external helper generation unavailable"))?;
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .filter(|binding| {
                !binding.retired && binding.uid == *uid && binding.generation == generation
            })
            .ok_or_else(|| MezError::forbidden("external helper handle unavailable"))?;
        let enrollment = binding.enrollment.as_ref().ok_or_else(|| {
            MezError::forbidden("external helper requires an independently enrolled producer")
        })?;
        let registration = binding
            .registration
            .as_ref()
            .filter(|registration| registration.external_session_id == session_id)
            .ok_or_else(|| MezError::forbidden("external helper session unavailable"))?;
        if (heartbeat || usage_end)
            && (binding.harness != "claude"
                || enrollment.curated_observer.is_none()
                || !matches!(enrollment.producer, ProducerEvidence::VerifiedParent(_)))
        {
            return Err(MezError::forbidden(
                "curated observer proof requires its declared parent source",
            ));
        }
        if binding.pi_lifecycle.is_some() {
            return Err(MezError::conflict(
                "external Pi lifecycle owns presentation sequence",
            ));
        }
        let usage_begin = if usage_begin {
            Some(super::helper_usage::HelperUsageBegin::capture(
                self,
                binding,
                text(&params, "request_id", 64)?,
            )?)
        } else {
            None
        };
        let payload = params
            .as_object_mut()
            .ok_or_else(|| MezError::invalid_args("external helper requires an object"))?;
        payload.remove("launch_token");
        payload.remove("harness");
        let usage_end = if usage_end {
            if !enrollment.usage_owner_is_durable {
                return Err(MezError::forbidden("curated request namespace unavailable"));
            }
            Some(self.external_request_work_for_binding(request.id.clone(), binding, &params)?)
        } else {
            None
        };
        let mut work = ExternalEnrollmentWork {
            admission: 0,
            request_id: request.id.clone(),
            origin,
            process: binding.process.clone(),
            pane_id: binding.pane_id.clone(),
            harness: binding.harness.clone(),
            version: binding.version.clone(),
            session_id,
            display_name: registration.display_name.clone(),
            observer_instance: enrollment.instance.clone(),
            predecessor_generation: None,
            deadline: Instant::now() + ADMISSION_BUDGET,
            curated: None,
            helper: Some(HelperPresentation {
                digest,
                generation,
                producer: enrollment.producer.clone(),
                params: SecretString::from(params.to_string()),
                heartbeat,
                usage_begin,
                usage_end: None,
                codex: None,
                codex_order: None,
            }),
            ancestry: Arc::new(OnceLock::new()),
            source_store: None,
            source_owner: Arc::new(OnceLock::new()),
            ancestry_budget: self
                .control
                .external_agents()
                .enrollments
                .ancestry_budget
                .clone(),
            #[cfg(test)]
            worker_gate: None,
            #[cfg(test)]
            settled: None,
        };
        let admissions = &mut self.control.external_agents_mut().enrollments;
        if admissions.pending.len() >= MAX_PENDING {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "external native observation capacity unavailable",
            ));
        }
        work.admission = admissions
            .next
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("external observation generation exhausted"))?;
        admissions.next = work.admission;
        admissions.pending.insert(work.admission);
        #[cfg(test)]
        {
            work.worker_gate = admissions.worker_gate.clone();
            work.settled = admissions.settled.clone();
        }
        if let Some(end) = usage_end {
            match self.reserve_external_usage_work(end) {
                Ok(end) => {
                    if let Some(helper) = &mut work.helper {
                        helper.usage_end = Some(Box::new(end));
                    }
                }
                Err(error) => {
                    self.control
                        .external_agents_mut()
                        .enrollments
                        .pending
                        .remove(&work.admission);
                    return Err(error);
                }
            }
        }
        Ok(work)
    }

    /// Applies one child callback after common reservation/connection/root/time
    /// fencing. The producer handle must still denote the exact current run;
    /// worker success is never a replacement producer or general client grant.
    pub(super) fn commit_external_helper_presentation(
        &mut self,
        work: &ExternalEnrollmentWork,
        helper: &HelperPresentation,
    ) -> Result<String> {
        self.reconcile_external_agent_registration(helper.digest);
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&helper.digest)
            .filter(|binding| {
                !binding.retired
                    && binding.generation == helper.generation
                    && binding.uid == work.origin.uid()
                    && binding.pane_id == work.pane_id
                    && binding.process.same_incarnation(&work.process)
            })
            .ok_or_else(|| MezError::forbidden("external helper run changed"))?;
        if binding.pi_lifecycle.is_some() {
            return Err(MezError::conflict(
                "external Pi lifecycle owns presentation sequence",
            ));
        }
        let producer = binding
            .enrollment
            .as_ref()
            .filter(|enrollment| enrollment.producer.same_owner(&helper.producer))
            .ok_or_else(|| MezError::forbidden("external helper producer changed"))?;
        producer
            .producer
            .reobserve()
            .map_err(|_| MezError::forbidden("external helper producer unavailable"))?;
        let child = work
            .origin
            .reobserve()
            .map_err(|_| MezError::forbidden("external helper unavailable"))?;
        if child.parent_process_id != producer.producer.identity().process_id
            || Instant::now() >= work.deadline
            || !producer.provenance_is_live()
            || !work.ancestry.get().is_some_and(|ancestry| {
                ancestry.is_live()
                    && ancestry
                        .source_matches(producer.producer.uid(), producer.producer.identity())
            })
            || !work.origin.writer_confirmed()
        {
            return Err(MezError::forbidden(
                "external helper relationship or deadline changed",
            ));
        }
        let params: serde_json::Value = serde_json::from_str(helper.params.expose_secret())
            .map_err(|_| MezError::invalid_state("external helper payload unavailable"))?;
        if let Some(event) = &helper.codex {
            if helper
                .codex_order
                .as_ref()
                .is_none_or(|order| !order.is_serving())
            {
                return Err(MezError::conflict(
                    "Codex callback is not current ingress work",
                ));
            }
            return self.commit_external_codex_observation(helper.digest, &work.session_id, event);
        }
        if helper.heartbeat {
            let sequence = params
                .get("sequence")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| MezError::invalid_args("curated observer sequence unavailable"))?;
            let observer = self
                .control
                .external_agents_mut()
                .bindings
                .get_mut(&helper.digest)
                .and_then(|binding| binding.enrollment.as_mut())
                .and_then(|source| source.curated_observer.as_mut())
                .ok_or_else(|| MezError::forbidden("curated observer owner unavailable"))?;
            return observer.observe(sequence);
        }
        if let Some(begin) = &helper.usage_begin {
            let origin = begin.origin(binding)?;
            let witness =
                super::super::external_usage_requests::request_source_witness(&begin.owner);
            self.control
                .external_agents_mut()
                .usage_begins
                .cache_origin(
                    begin.owner.clone(),
                    origin.request_id.clone(),
                    origin.clone(),
                );
            return Ok(serde_json::json!({"protocol":"external-request/1","accepted":true,"request_id":origin.request_id,"admitted_at":origin.admitted_at,"source_witness":witness,"origin":if origin.project.is_some(){"project"}else{"unattributed"},"controls":[]}).to_string());
        }
        if helper.usage_end.is_some() {
            return Ok("{}".into());
        }
        self.update_external_presentation(helper.digest, &params)
    }
}
