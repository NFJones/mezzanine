//! Required durable end publication, separate from logical deregistration.
//!
//! Reuses the enrollment admission pool and original sealed retirement work.
//! Logical closure happens before off-actor I/O and is never undone on failure.
//! Actor completion consumes the exact reservation and current retired binding;
//! neither caller-supplied Ok nor queued work is evidence of a durable tombstone.

use super::*;
use crate::runtime::runtime_json_rpc_error;

/// Finite actor-owned reply custody; no persisted credential or new authority.
#[derive(Clone)]
pub(crate) struct ExternalSourceEndWork {
    admission: u64,
    registry_incarnation: Arc<()>,
    pub(crate) request_id: String,
    digest: [u8; 32],
    generation: u64,
    session_id: String,
    uid: u32,
    deadline: Instant,
    retirement: ExternalSourceRetirementWork,
    outcome: Arc<OnceLock<Result<()>>>,
    /// Retained until all reply/physical worker clones have actually retired.
    _publication_permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    /// Existing fixture-owned enrollment gate; production has no timing hook.
    #[cfg(test)]
    pub(crate) worker_gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    /// Fixture notification after exact actor settlement, not durability proof.
    #[cfg(test)]
    settled: Option<Arc<tokio::sync::Notify>>,
}

impl std::fmt::Debug for ExternalSourceEndWork {
    /// Excludes selectors, credentials, paths, worker errors and reply contents.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalSourceEndWork")
            .finish_non_exhaustive()
    }
}

impl ExternalSourceEndWork {
    /// Runs only on a blocking worker. Clones share the exact proof/outcome;
    /// repeated end requests cannot replenish the source's mutation allowance.
    pub(crate) fn execute(&self) -> Result<()> {
        self.outcome
            .get_or_init(|| self.retirement.execute_for_publication())
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone)
    }
}

impl RuntimeSessionService {
    /// Authenticates through the ordinary end owner, closes logically, then
    /// reserves bounded required publication. An unavailable result may follow
    /// logical closure, but cannot imply durable success or reactivate it.
    pub(crate) fn prepare_external_source_end(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
    ) -> Result<ExternalSourceEndWork> {
        if request.method != "agent/external/source-end"
            || request.id.len() > 256
            || request
                .params
                .as_ref()
                .is_some_and(|params| params.len() > 8192)
        {
            return Err(MezError::invalid_args(
                "external source end requires one bounded control frame",
            ));
        }
        crate::control::validate_control_method_params_schema(request)?;
        let params: serde_json::Value =
            serde_json::from_str(request.params.as_deref().unwrap_or("{}"))
                .map_err(|_| MezError::invalid_args("external source end params unavailable"))?;
        let digest = super::super::external_agents::credential(&params)?;
        let logical = crate::control::parse_json_rpc_request(&format!(
            r#"{{"jsonrpc":"2.0","id":{},"method":"agent/external/deregister","params":{}}}"#,
            request.id, params
        ))?;
        self.dispatch_external_agent_request(&logical, connection)?;
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .ok_or_else(|| MezError::conflict("external source end owner unavailable"))?;
        let generation = binding.generation;
        let uid = binding.uid;
        let session_id = binding
            .registration
            .as_ref()
            .ok_or_else(|| MezError::conflict("external source end registration unavailable"))?
            .external_session_id
            .clone();
        let retirement = self
            .retired_external_source_work(digest)
            .ok_or_else(|| MezError::invalid_state("external source durable end unavailable"))?;
        let admissions = &mut self.control.external_agents_mut().enrollments;
        if admissions.pending.len() >= MAX_PENDING {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "external source end capacity unavailable",
            ));
        }
        let admission = admissions
            .next
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("external enrollment generation exhausted"))?;
        let publication_permit = Arc::new(retirement.reserve_publication()?);
        admissions.next = admission;
        admissions.pending.insert(admission);
        Ok(ExternalSourceEndWork {
            admission,
            registry_incarnation: admissions.incarnation.clone(),
            request_id: request.id.clone(),
            digest,
            generation,
            session_id,
            uid,
            deadline: Instant::now() + ADMISSION_BUDGET,
            retirement,
            outcome: Arc::new(OnceLock::new()),
            _publication_permit: publication_permit,
            #[cfg(test)]
            worker_gate: admissions.worker_gate.clone(),
            #[cfg(test)]
            settled: admissions.settled.clone(),
        })
    }

    /// Consumes an exact actor reservation even after reply loss. No SQLite or
    /// mutex waiting occurs here. Currentness grants only this historical end
    /// acknowledgment, never renewed telemetry/native/general control rights.
    pub(crate) fn complete_external_source_end(
        &mut self,
        work: ExternalSourceEndWork,
        result: Result<()>,
        connection: &ControlConnectionState,
    ) -> String {
        let completed = (|| {
            let admissions = &mut self.control.external_agents_mut().enrollments;
            if !Arc::ptr_eq(&work.registry_incarnation, &admissions.incarnation)
                || !admissions.pending.remove(&work.admission)
            {
                return Err(MezError::conflict(
                    "external source end publication owner changed",
                ));
            }
            self.require_live()?;
            // Storage/open/trigger diagnostics remain in the private immutable
            // worker caches. The external producer gets only a bounded public
            // disposition with the original error kind, never database text.
            result.map_err(|error| {
                MezError::new(error.kind(), "external source durable end unavailable")
            })?;
            if work.outcome.get().is_none_or(|outcome| outcome.is_err())
                || Instant::now() >= work.deadline
                || connection.initialized()
            {
                return Err(MezError::conflict(
                    "external source end publication unavailable",
                ));
            }
            if !matches!(connection.authenticated_peer(), Some(crate::control::AuthenticatedPeer::UnixUser { uid }) if *uid == work.uid)
            {
                return Err(MezError::forbidden(
                    "external source end connection unavailable",
                ));
            }
            let binding = self
                .control
                .external_agents()
                .bindings
                .get(&work.digest)
                .filter(|binding| {
                    binding.retired
                        && binding.uid == work.uid
                        && binding.generation == work.generation
                        && binding.registration.as_ref().is_some_and(|registration| {
                            registration.external_session_id == work.session_id
                        })
                })
                .ok_or_else(|| MezError::conflict("external source end binding changed"))?;
            binding
                .enrollment
                .as_ref()
                .ok_or_else(|| MezError::forbidden("external source end producer unavailable"))?
                .authorize_connection(connection)?;
            if self.retired_external_source_work(work.digest).as_ref() != Some(&work.retirement) {
                return Err(MezError::conflict("external source end source changed"));
            }
            Ok(
                serde_json::json!({"protocol":"external-source-end/1","retired":true,"durable":true,"generation":work.generation}),
            )
        })();
        #[cfg(test)]
        if let Some(settled) = &work.settled {
            settled.notify_one();
        }
        match completed {
            Ok(value) => format!(
                r#"{{"jsonrpc":"2.0","id":{},"result":{value}}}"#,
                work.request_id
            ),
            Err(error) => runtime_json_rpc_error(&work.request_id, error.kind(), error.message()),
        }
    }
}
