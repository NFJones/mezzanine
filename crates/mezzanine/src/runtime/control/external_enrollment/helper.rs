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
    pub(super) producer: Arc<UnixOriginProcess>,
    params: SecretString,
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
        self.require_live()?;
        if !matches!(
            request.method.as_str(),
            "agent/external/helper-presentation" | "agent/external/helper-observe"
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
        let digest = if request.method == "agent/external/helper-observe" {
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
            self.control
                .external_agents()
                .enrollments
                .helper_targets
                .select(*uid, &harness, &session_id)?
        } else {
            super::super::external_agents::credential(&params)?
        };
        self.reconcile_external_agent_registration(digest);
        if request.method == "agent/external/helper-observe"
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
        if binding.pi_lifecycle.is_some() {
            return Err(MezError::conflict(
                "external Pi lifecycle owns presentation sequence",
            ));
        }
        let payload = params
            .as_object_mut()
            .ok_or_else(|| MezError::invalid_args("external helper requires an object"))?;
        payload.remove("launch_token");
        payload.remove("harness");
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
            helper: Some(HelperPresentation {
                digest,
                generation,
                producer: enrollment.origin.clone(),
                params: SecretString::from(params.to_string()),
            }),
            ancestry: Arc::new(OnceLock::new()),
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
            .filter(|enrollment| Arc::ptr_eq(&enrollment.origin, &helper.producer))
            .ok_or_else(|| MezError::forbidden("external helper producer changed"))?;
        producer
            .origin
            .reobserve()
            .map_err(|_| MezError::forbidden("external helper producer unavailable"))?;
        let child = work
            .origin
            .reobserve()
            .map_err(|_| MezError::forbidden("external helper unavailable"))?;
        if child.parent_process_id != producer.origin.identity.process_id
            || Instant::now() >= work.deadline
            || !producer.provenance_is_live()
            || !work
                .ancestry
                .get()
                .is_some_and(|ancestry| ancestry.is_live())
            || !work.origin.writer_confirmed()
        {
            return Err(MezError::forbidden(
                "external helper relationship or deadline changed",
            ));
        }
        let params = serde_json::from_str(helper.params.expose_secret())
            .map_err(|_| MezError::invalid_state("external helper payload unavailable"))?;
        self.update_external_presentation(helper.digest, &params)
    }
}
