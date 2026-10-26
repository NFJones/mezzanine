//! Actor-qualified initial/cold acceptance using the existing enrollment lane.
//!
//! Native observation can prepare but never accept a receipt. Only successful
//! actor registration seals the exact binding digest/generation below. The same
//! finite admission pool then retains the private reply through off-actor clock
//! and SQLite work, followed by a fresh actor/currentness check. Reply loss does
//! not replay admission or persistence; original write outcomes remain cached.
//! Explicit cold recovery requires fresh original-kind native evidence and its
//! own committed exact observer claim BEFORE exposing a private reply. Initial
//! acceptance metadata remains optional. Native lifetime renewal and
//! same-live observer-fence updates use the bounded metadata lane. Source-lifetime
//! retirement already closes acceptance and queues an optional exact tombstone.
//! Optional metadata failure never invents durable success or changes
//! independently qualified lifecycle rights.

use super::*;

/// Closed public historical source selector. This is deliberately distinct from
/// private observer handles and same-live recovery. Decimal generation is text
/// so SQLite's full checked range cannot be truncated by a JavaScript number.
/// No native identity, ancestry, path, namespace, time or credential is exposed.
#[derive(serde::Serialize)]
struct AcceptedSourceReceipt {
    protocol: &'static str,
    activation_id: String,
    observer_generation: String,
}

impl AcceptedSourceReceipt {
    /// Builds only from an actually successful sealed acceptance cache, never
    /// from a caller-provided Ok, current namespace or guessed generation.
    fn from_preparation(preparation: &source::NativeSourcePreparation) -> Option<Self> {
        let receipt = preparation.accepted_receipt()?;
        Some(Self {
            protocol: "external-source-activation/1",
            activation_id: receipt.activation.public_id(),
            observer_generation: receipt.observer.as_ref()?.generation().to_string(),
        })
    }
}

/// Unserialized evidence that the original actor really registered this source.
/// Construction is local to the successful registration handoff, not the worker.
#[derive(Debug)]
pub(super) struct SourceActorAdmission {
    /// Exact digest of the admitted handle; never its private credential bytes.
    digest: [u8; 32],
    /// Original binding generation, not a caller-selected publication revision.
    generation: u64,
    /// One actor handoff only, even after the original publication settles.
    scheduled: OnceLock<()>,
}

impl SourceActorAdmission {
    /// Pure exact grant selectors for unexposed cold-publication cleanup only.
    pub(super) fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
}

impl ExternalEnrollmentWork {
    /// Seals only successfully prepared, original first-admission work. Calling
    /// this after register_external_agent succeeds separates actual acceptance
    /// from abandoned preparation and failed actor qualification.
    pub(super) fn record_source_actor_admission(&self, digest: [u8; 32], generation: u64) {
        if let Some(preparation) = self.source_preparation.get() {
            let _ = preparation.admission.set(SourceActorAdmission {
                digest,
                generation,
                scheduled: OnceLock::new(),
            });
        }
    }

    /// True only for the actor-created second phase, never for caller metadata.
    pub(crate) fn is_source_publication(&self) -> bool {
        self.source_publication.is_some()
    }

    /// Fences original native callbacks after the same slot transfers phases.
    /// This one-shot marker outlives publication so late work cannot re-admit.
    pub(super) fn source_publication_was_scheduled(&self) -> bool {
        self.source_preparation
            .get()
            .and_then(|preparation| preparation.admission.get())
            .is_some_and(|admission| admission.scheduled.get().is_some())
    }

    /// Executes one native observation or sealed post-admission store phase.
    /// Always run off actor. A caller-provided Ok result cannot create a grant.
    pub(crate) fn execute_enrollment(&self) -> Result<()> {
        if !self.is_source_publication() {
            return self.observe();
        }
        if Instant::now() >= self.deadline
            || !self.origin.is_live()
            || !self.origin.writer_confirmed()
            || self
                .ancestry
                .get()
                .is_none_or(|ancestry| !ancestry.is_live())
        {
            return Err(MezError::conflict(
                "external source publication evidence expired",
            ));
        }
        self.source_preparation.accept_for_work(
            self.source_store
                .as_ref()
                .ok_or_else(|| MezError::invalid_state("external source store disappeared"))?,
            &self.origin,
            self.ancestry
                .get()
                .ok_or_else(|| MezError::conflict("external source ancestry unavailable"))?,
            self.deadline,
        )
    }
}

impl RuntimeSessionService {
    /// Moves the already-consumed native reservation into bounded publication
    /// custody. No database, ancestry walk or mutex wait occurs on the actor.
    /// Missing first-allocation metadata preserves lifecycle-only replies; a
    /// required unexposed cold claim instead fails and discards memory authority.
    pub(crate) fn begin_external_source_publication(
        &mut self,
        mut work: ExternalEnrollmentWork,
        body: String,
    ) -> std::result::Result<ExternalEnrollmentWork, String> {
        let grant = work
            .source_preparation
            .get()
            .filter(|preparation| preparation.outcome_is_prepared())
            .and_then(|preparation| preparation.admission.get());
        let Some(grant) = grant else {
            return Err(self.reject_unpublished_cold(&work, body));
        };
        let admissions = &mut self.control.external_agents_mut().enrollments;
        if work.is_source_publication()
            || !Arc::ptr_eq(&work.registry_incarnation, &admissions.incarnation)
            || admissions.pending.len() >= MAX_PENDING
            || grant.scheduled.set(()).is_err()
            || !admissions.pending.insert(work.admission)
        {
            return Err(self.reject_unpublished_cold(&work, body));
        }
        work.source_publication = Some(SecretString::from(body));
        #[cfg(test)]
        {
            work.worker_gate = None;
        }
        Ok(work)
    }

    /// Consumes only this publication slot and rechecks current ownership before
    /// exposing the retained reply. Store failure remains optional/unavailable;
    /// neither an Ok worker result nor a stored Accepted bit supplies authority.
    pub(crate) fn complete_external_source_publication(
        &mut self,
        work: ExternalEnrollmentWork,
        _persisted: Result<()>,
        connection: &ControlConnectionState,
    ) -> String {
        let result = (|| {
            let admissions = &mut self.control.external_agents_mut().enrollments;
            if !work.is_source_publication()
                || !Arc::ptr_eq(&work.registry_incarnation, &admissions.incarnation)
                || !admissions.pending.remove(&work.admission)
            {
                return Err(MezError::conflict(
                    "external source publication owner changed",
                ));
            }
            self.require_live()?;
            let admission = work
                .source_preparation
                .get()
                .and_then(|preparation| preparation.admission.get())
                .ok_or_else(|| {
                    MezError::invalid_state("external source actor admission disappeared")
                })?;
            let binding = self
                .control
                .external_agents()
                .bindings
                .get(&admission.digest)
                .filter(|binding| {
                    !binding.retired
                        && binding.generation == admission.generation
                        && binding.pane_id == work.pane_id
                        && binding.harness == work.harness
                        && binding.registration.as_ref().is_some_and(|registration| {
                            registration.external_session_id == work.session_id
                        })
                        && binding.enrollment.as_ref().is_some_and(|enrollment| {
                            enrollment.instance == work.observer_instance
                                && enrollment.producer.matches_socket(&work.origin)
                        })
                })
                .ok_or_else(|| MezError::conflict("external source publication binding changed"))?;
            if Instant::now() >= work.deadline
                || !self.pane_process_identity_is_current(&work.pane_id, &work.process)
                || !binding.process.same_incarnation(&work.process)
                || !work.origin.is_live()
                || !work.origin.writer_confirmed()
                || work
                    .ancestry
                    .get()
                    .is_none_or(|ancestry| !ancestry.is_live())
                || connection.initialized()
                || !matches!(connection.authenticated_peer(), Some(AuthenticatedPeer::UnixUser { uid }) if *uid == work.origin.uid())
                || connection
                    .unix_origin()
                    .is_none_or(|origin| !Arc::ptr_eq(origin, &work.origin))
            {
                return Err(MezError::conflict(
                    "external source publication evidence changed",
                ));
            }
            let body = work
                .source_publication
                .as_ref()
                .map(|body| body.expose_secret().to_string())
                .ok_or_else(|| {
                    MezError::invalid_state("external source publication reply disappeared")
                })?;
            let receipt = work
                .source_preparation
                .get()
                .and_then(AcceptedSourceReceipt::from_preparation);
            let Some(receipt) = receipt else {
                if work.cold_resume.is_some() {
                    return Err(MezError::conflict("durable source claim was not accepted"));
                }
                return Ok(body);
            };
            let unavailable =
                || MezError::invalid_state("external source publication reply invalid");
            let mut reply: serde_json::Value =
                serde_json::from_str(&body).map_err(|_| unavailable())?;
            let result = reply
                .get_mut("result")
                .and_then(serde_json::Value::as_object_mut)
                .ok_or_else(unavailable)?;
            result.insert(
                "source_receipt".into(),
                serde_json::to_value(receipt).map_err(|_| unavailable())?,
            );
            let reply = serde_json::to_string(&reply).map_err(|_| unavailable())?;
            if work.cold_resume.is_some() {
                self.publish_cold_claim(&work)?;
            }
            Ok(reply)
        })();
        if result.is_err() {
            self.discard_unpublished_cold(&work);
        }
        result.unwrap_or_else(|error| {
            super::super::runtime_json_rpc_error(&work.request_id, error.kind(), error.message())
        })
    }
}
