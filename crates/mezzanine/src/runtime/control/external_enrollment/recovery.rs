//! Memory-only same-live source authority outside disposable registrations.
//!
//! SQLite conserves an accounting namespace; it cannot activate a source. This
//! owner retains the original native socket/root ancestry and bounded observer
//! metadata for the SAME control service. Fresh ingress must independently prove
//! its writer, UID, creator incarnation and root. Prepare snapshots a revision;
//! actor commit compares it before rotating credentials and projection ownership.
//! Every ordinary rotation and central retirement updates this owner synchronously.
//! No credential, lifecycle projection, transcript or general capability is stored
//! here or on disk. An accepted transition can replay only through its still-active
//! registration. Losing that registration before receiving its new selector is
//! an explicit availability gap, never permission to look up the newest owner.

use super::*;
use crate::runtime::control::external_agents::{MAX_BINDINGS, Registration};
use crate::storage::token_usage::AccountingOrigin;
use std::collections::BTreeMap;

/// JavaScript producer selectors must round-trip without numeric truncation.
const MAX_SAFE_SELECTOR: u64 = 9_007_199_254_740_991;

/// Non-authoritative current observer receipt, never a bearer capability.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceResume {
    /// Exact lower-case public witness from a previously accepted enrollment.
    observer_witness: String,
    /// Positive, exactly representable generation paired with that witness.
    generation: u64,
}

impl SourceResume {
    /// Decodes only the optional strict two-field receipt selector. Null,
    /// malformed and unknown nested metadata fail before native reservations.
    pub(super) fn from_params(params: &serde_json::Value) -> Result<Option<Self>> {
        let Some(value) = params.get("source_resume") else {
            return Ok(None);
        };
        let selector: Self = serde_json::from_value(value.clone())
            .map_err(|_| MezError::invalid_args("source resume selector unavailable"))?;
        if selector.generation == 0
            || selector.generation > MAX_SAFE_SELECTOR
            || selector.observer_witness.len() != 64
            || !selector
                .observer_witness
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(MezError::invalid_args("source resume selector unavailable"));
        }
        Ok(Some(selector))
    }
}

/// Original service-owned source revision captured before off-actor observation.
#[derive(Debug, Clone)]
pub(super) struct ResumeAdmission {
    /// Opaque server-owned accounting namespace, never selected by the client.
    owner: String,
    /// Exact currentness revision; distinct contenders cannot both publish.
    revision: u64,
}

/// Bounded native authority and inert metadata only, not a shadow registration.
#[derive(Debug, Clone)]
struct BoundSourceAnchor {
    revision: u64,
    digest: [u8; 32],
    pane_id: String,
    process: RuntimePaneProcessIdentity,
    harness: String,
    version: String,
    registration: Registration,
    producer: ProducerEvidence,
    ancestry: Option<Arc<UnixAncestryWitness>>,
    accounting_origin: AccountingOrigin,
    project_scope: Option<mez_agent::messaging::ProjectScopeId>,
    durable: bool,
    request_reports_seen: bool,
    gap: Option<crate::integrations::bootstrap::pi_ipc::UsageGapReason>,
    generation: u64,
    run_generation: u64,
    epoch: u64,
    instance: String,
    predecessor: Option<u64>,
    instances: BTreeSet<String>,
    accepted_resume: Option<SourceResume>,
    expires: u64,
    retired: bool,
}

impl BoundSourceAnchor {
    /// Matches every original native source/routing dimension. This selection
    /// grants no authority; retained and newly captured lifetimes are also fenced.
    fn matches(&self, work: &ExternalEnrollmentWork) -> bool {
        self.pane_id == work.pane_id
            && self.process.same_incarnation(&work.process)
            && self.harness == work.harness
            && self.registration.external_session_id == work.session_id
            && self.producer.matches_socket(&work.origin)
    }

    /// Checks metadata and original native chain without mutating lease/proof.
    fn qualify(&self, work: &ExternalEnrollmentWork) -> Result<()> {
        if !self.matches(work)
            || self.version != work.version
            || self.registration.display_name != work.display_name
            || self.retired
            || self.expires <= current_unix_seconds()
            || !self.producer.is_live()
            || self
                .ancestry
                .as_ref()
                .is_none_or(|ancestry| !ancestry.is_live())
        {
            return Err(MezError::conflict("same-live source anchor unavailable"));
        }
        Ok(())
    }

    /// Recognizes only the exact accepted input while its result is current.
    fn is_replay(&self, work: &ExternalEnrollmentWork) -> bool {
        self.accepted_resume.is_some()
            && self.accepted_resume == work.source_resume
            && self.instance == work.observer_instance
            && self.predecessor == work.predecessor_generation
    }

    /// Requires either the original same-instance predecessor or an exact current
    /// successor predecessor. Retired IDs are never evicted to enable takeover.
    fn validate_transition(&self, work: &ExternalEnrollmentWork) -> Result<()> {
        let selector = work
            .source_resume
            .as_ref()
            .ok_or_else(|| MezError::conflict("explicit source resume required"))?;
        if selector.generation != self.generation
            || selector.observer_witness != observer_witness(self.digest)
        {
            return Err(MezError::conflict("source resume current observer differs"));
        }
        if self.instance == work.observer_instance {
            if self.predecessor != work.predecessor_generation {
                return Err(MezError::conflict(
                    "source resume original predecessor differs",
                ));
            }
        } else {
            if self.instances.contains(&work.observer_instance) {
                return Err(MezError::conflict("external observer instance has retired"));
            }
            if work.predecessor_generation != Some(self.generation) {
                return Err(MezError::conflict("external observer predecessor changed"));
            }
            if self.instances.len() >= MAX_OBSERVER_INSTANCES {
                return Err(MezError::new(
                    crate::error::MezErrorKind::RateLimited,
                    "external observer instance capacity exhausted",
                ));
            }
        }
        Ok(())
    }
}

/// Control-service lifetime owner. Recreating it loses all activation authority;
/// persisted source mappings never reconstruct its rows or native descriptors.
#[derive(Debug)]
pub(in crate::runtime::control) struct LiveSourceAnchors {
    anchors: BTreeMap<String, BoundSourceAnchor>,
    budget: Arc<UnixAncestryBudget>,
    /// Small fixture-only quota exercises fail-closed exhaustion without load.
    #[cfg(test)]
    limit: Option<usize>,
}

impl LiveSourceAnchors {
    /// Keeps existing idle maintenance scheduled while native fences are held.
    pub(in crate::runtime::control) fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }

    /// Shares the existing aggregate descriptor budget, including after registry
    /// reset. Shared Arcs do not duplicate descriptors or create another quota.
    pub(in crate::runtime::control) fn new(budget: Arc<UnixAncestryBudget>) -> Self {
        Self {
            anchors: BTreeMap::new(),
            budget,
            #[cfg(test)]
            limit: None,
        }
    }

    /// Returns the fixed production bound or an explicit test-owned small bound.
    fn limit(&self) -> usize {
        #[cfg(test)]
        if let Some(limit) = self.limit {
            return limit;
        }
        MAX_BINDINGS
    }
}

impl RuntimeSessionService {
    /// Snapshot/session replacement invalidates every retained source, including
    /// one whose disposable registration already disappeared. Keep retired
    /// lifetime fences, but release ancestry and any accepted transition input.
    pub(in crate::runtime::control) fn retire_all_external_source_anchors(&mut self) {
        for anchor in self.control.external_sources_mut().anchors.values_mut() {
            if !anchor.retired {
                anchor.retired = true;
                anchor.revision = anchor.revision.saturating_add(1);
                anchor.ancestry = None;
                anchor.accepted_resume = None;
            }
        }
    }

    /// Sets only a fixture memory quota; no production setting or authority exists.
    #[cfg(test)]
    pub(super) fn external_source_anchor_limit_for_tests(&mut self, limit: usize) {
        self.control.external_sources_mut().limit = Some(limit);
    }
    /// Existing durable mappings never activate a missing service source owner.
    /// Only this exact worker's newly created mapping may accompany ordinary
    /// first allocation. Failed/abandoned earlier admission remains unavailable.
    pub(super) fn validate_external_source_mapping_allocation(
        &self,
        work: &ExternalEnrollmentWork,
    ) -> Result<()> {
        if work.source_owner.get().is_some_and(Option::is_some)
            && work.source_created.get() != Some(&true)
        {
            return Err(MezError::conflict(
                "same-live source activation unavailable",
            ));
        }
        Ok(())
    }

    /// Restores the disposable registry's alias to the original service budget.
    pub(in crate::runtime::control) fn sync_external_ancestry_budget(&mut self) {
        let budget = self.control.external_sources().budget.clone();
        self.control
            .external_agents_mut()
            .enrollments
            .ancestry_budget = budget;
    }

    /// Drops only actually ended native source/root lifetimes. Active or retired
    /// instances with live original witnesses are never evicted to free capacity.
    pub(in crate::runtime::control) fn prune_external_source_anchors(&mut self) {
        let ended = self
            .control
            .external_sources()
            .anchors
            .iter()
            .filter_map(|(owner, anchor)| {
                (!anchor.producer.is_live()
                    || !self.pane_process_identity_is_current(&anchor.pane_id, &anchor.process)
                    || self.pane_process_identity(&anchor.pane_id).is_err())
                .then_some(owner.clone())
            })
            .collect::<Vec<_>>();
        for owner in ended {
            self.control.external_sources_mut().anchors.remove(&owner);
        }
    }

    /// Records successful ordinary publication and diagnostics atomically with
    /// actor mutation. Credentials and presentation are deliberately excluded.
    pub(in crate::runtime::control) fn record_external_source_anchor(&mut self, digest: [u8; 32]) {
        let Some(binding) = self.control.external_agents().bindings.get(&digest) else {
            return;
        };
        let Some(enrollment) = &binding.enrollment else {
            return;
        };
        if !matches!(enrollment.producer, ProducerEvidence::Socket(_)) || binding.retired {
            return;
        }
        let Some(mut registration) = binding.registration.clone() else {
            return;
        };
        registration.presentation = None;
        let owner = binding.accounting_owner.clone();
        let prior = self.control.external_sources().anchors.get(&owner);
        let revision = prior.map_or(1, |prior| {
            if prior.digest == digest && !prior.retired {
                prior.revision
            } else {
                prior.revision.saturating_add(1)
            }
        });
        let mut instances = enrollment.instances.clone();
        if let Some(prior) = prior {
            instances.extend(prior.instances.iter().cloned());
        }
        let anchor = BoundSourceAnchor {
            revision,
            digest,
            pane_id: binding.pane_id.clone(),
            process: binding.process.clone(),
            harness: binding.harness.clone(),
            version: binding.version.clone(),
            registration,
            producer: enrollment.producer.clone(),
            ancestry: enrollment.ancestry.clone(),
            accounting_origin: binding.accounting_origin.clone(),
            project_scope: binding.project_scope.clone(),
            durable: enrollment.usage_owner_is_durable,
            request_reports_seen: enrollment.request_reports_seen,
            gap: binding
                .pi_lifecycle
                .as_ref()
                .and_then(|projection| projection.usage_gap_reason())
                .or(enrollment.resume_usage_gap),
            generation: binding.generation,
            run_generation: enrollment.run_generation,
            epoch: enrollment.epoch,
            instance: enrollment.instance.clone(),
            predecessor: enrollment.predecessor_generation,
            instances,
            accepted_resume: enrollment.accepted_resume.clone(),
            expires: binding.expires,
            retired: false,
        };
        self.control
            .external_sources_mut()
            .anchors
            .insert(owner, anchor);
    }

    /// Synchronizes only bounded metadata; never performs database/process I/O.
    pub(in crate::runtime::control) fn refresh_external_source_diagnostics(&mut self) {
        let digests = self
            .control
            .external_agents()
            .bindings
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for digest in digests {
            self.record_external_source_anchor(digest);
        }
    }

    /// Updates only the immutable source namespace affected by one durable
    /// request completion, not every source on each finalized report.
    pub(in crate::runtime::control) fn refresh_external_source_diagnostics_for_owner(
        &mut self,
        owner: &str,
    ) {
        let digests = self
            .control
            .external_agents()
            .bindings
            .iter()
            .filter(|(_, binding)| binding.accounting_owner == owner)
            .map(|(digest, _)| *digest)
            .collect::<Vec<_>>();
        for digest in digests {
            self.record_external_source_anchor(digest);
        }
    }

    /// Invalidates currentness BEFORE the central retirement drops ancestry or
    /// publishes terminal state. Pending/replay work cannot resurrect that owner.
    pub(in crate::runtime::control) fn retire_external_source_anchor(&mut self, digest: [u8; 32]) {
        for anchor in self.control.external_sources_mut().anchors.values_mut() {
            if anchor.digest == digest && !anchor.retired {
                anchor.retired = true;
                anchor.revision = anchor.revision.saturating_add(1);
                anchor.ancestry = None;
                anchor.accepted_resume = None;
            }
        }
    }

    /// Preserves the original predecessor/newest-instance guards even when an
    /// old caller tries to bypass explicit recovery through initial enrollment.
    pub(super) fn validate_external_source_initial(
        &self,
        work: &ExternalEnrollmentWork,
    ) -> Result<()> {
        if let Some(anchor) = self
            .control
            .external_sources()
            .anchors
            .values()
            .find(|anchor| anchor.matches(work))
        {
            if anchor.retired {
                return Err(MezError::conflict("external native source has retired"));
            }
            if anchor.instance == work.observer_instance && anchor.accepted_resume.is_some() {
                return Err(MezError::conflict("explicit source resume required"));
            }
            if !anchor.retired
                && anchor.instance != work.observer_instance
                && anchor.instances.contains(&work.observer_instance)
            {
                return Err(MezError::conflict("external observer instance has retired"));
            }
            if !anchor.retired
                && !self
                    .control
                    .external_agents()
                    .bindings
                    .contains_key(&anchor.digest)
            {
                return Err(MezError::conflict("explicit source resume required"));
            }
            if anchor.instance != work.observer_instance
                && anchor.instances.len() >= MAX_OBSERVER_INSTANCES
            {
                return Err(MezError::new(
                    crate::error::MezErrorKind::RateLimited,
                    "external observer instance capacity exhausted",
                ));
            }
        } else if self.control.external_sources().anchors.len()
            + self.control.external_agents().enrollments.pending.len()
            > self.control.external_sources().limit()
        {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "same-live source anchor capacity unavailable",
            ));
        }
        Ok(())
    }

    /// Captures only a matched retained source and revision; receipt knowledge
    /// alone cannot select another pane/root/producer or create a source anchor.
    pub(super) fn capture_external_source_resume(
        &self,
        work: &ExternalEnrollmentWork,
    ) -> Result<Option<ResumeAdmission>> {
        if work.curated.is_some() {
            return Ok(None);
        }
        let Some(selector) = &work.source_resume else {
            self.validate_external_source_initial(work)?;
            return Ok(None);
        };
        let (owner, anchor) = self
            .control
            .external_sources()
            .anchors
            .iter()
            .find(|(_, anchor)| {
                (selector.generation == anchor.generation
                    && selector.observer_witness == observer_witness(anchor.digest))
                    || anchor.accepted_resume.as_ref() == Some(selector)
            })
            .ok_or_else(|| MezError::conflict("same-live source anchor unavailable"))?;
        anchor.qualify(work)?;
        if anchor.is_replay(work) {
            if !self
                .control
                .external_agents()
                .bindings
                .contains_key(&anchor.digest)
            {
                return Err(MezError::conflict("source resume retry unavailable"));
            }
        } else {
            anchor.validate_transition(work)?;
        }
        if !anchor.durable || anchor.revision == u64::MAX {
            return Err(MezError::conflict(
                "same-live source continuity unavailable",
            ));
        }
        Ok(Some(ResumeAdmission {
            owner: owner.clone(),
            revision: anchor.revision,
        }))
    }

    /// Revalidates the original source and revision after fresh native observation,
    /// then publishes one new transport/projection owner. Store failure, replaced
    /// source, retired predecessor and distinct stale contenders grant nothing.
    pub(super) fn commit_external_source_resume(
        &mut self,
        work: &ExternalEnrollmentWork,
        connection: &ControlConnectionState,
    ) -> Result<String> {
        let admission = work
            .resume_admission
            .as_ref()
            .ok_or_else(|| MezError::conflict("source resume admission unavailable"))?;
        let anchor = self
            .control
            .external_sources()
            .anchors
            .get(&admission.owner)
            .cloned()
            .ok_or_else(|| MezError::conflict("same-live source anchor unavailable"))?;
        anchor.qualify(work)?;
        anchor.producer.authorize_socket(&work.origin)?;
        if work.source_owner.get().and_then(Option::as_ref) != Some(&admission.owner) {
            return Err(MezError::conflict(
                "same-live source accounting namespace differs",
            ));
        }
        if anchor.is_replay(work) {
            let binding = self
                .control
                .external_agents_mut()
                .bindings
                .get_mut(&anchor.digest)
                .filter(|binding| !binding.retired && binding.generation == anchor.generation)
                .ok_or_else(|| MezError::conflict("source resume retry unavailable"))?;
            let enrollment = binding
                .enrollment
                .as_mut()
                .ok_or_else(|| MezError::conflict("source resume retry unavailable"))?;
            enrollment.authorize(connection)?;
            enrollment.observe_connection(&work.origin);
            let enrollment = binding
                .enrollment
                .as_ref()
                .ok_or_else(|| MezError::conflict("source resume retry unavailable"))?;
            return Ok(enrollment_response(binding, enrollment, &work.session_id));
        }
        if anchor.revision != admission.revision {
            return Err(MezError::conflict("source resume revision changed"));
        }
        anchor.validate_transition(work)?;
        let epoch = anchor
            .epoch
            .checked_add(1)
            .filter(|epoch| *epoch <= MAX_SAFE_SELECTOR)
            .ok_or_else(|| MezError::invalid_state("source resume observer epoch exhausted"))?;
        let generation = self
            .control
            .external_agent_generation()
            .checked_add(1)
            .filter(|generation| *generation <= MAX_SAFE_SELECTOR)
            .ok_or_else(|| MezError::invalid_state("source resume generation exhausted"))?;
        if !self
            .control
            .external_agents()
            .bindings
            .contains_key(&anchor.digest)
            && self.control.external_agents().bindings.len() >= MAX_BINDINGS
        {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "external enrollment registry is full",
            ));
        }
        let mut bytes = [0; 32];
        rand::rng().fill_bytes(&mut bytes);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let digest = Sha256::digest(token.as_bytes()).into();
        let mut instances = anchor.instances.clone();
        instances.insert(work.observer_instance.clone());
        let mut registration = anchor.registration.clone();
        registration.presentation = None;
        let binding = LaunchBinding {
            uid: anchor.producer.uid(),
            pane_id: anchor.pane_id.clone(),
            process: anchor.process.clone(),
            project_scope: anchor.project_scope.clone(),
            generation,
            harness: anchor.harness.clone(),
            version: anchor.version.clone(),
            expires: current_unix_seconds().saturating_add(60),
            registration: Some(registration),
            retired: false,
            accounting_owner: admission.owner.clone(),
            accounting_origin: anchor.accounting_origin.clone(),
            enrollment: Some(EnrollmentBinding {
                producer: anchor.producer.clone(),
                usage_owner_is_durable: true,
                request_reports_seen: anchor.request_reports_seen,
                ancestry: anchor.ancestry.clone(),
                run_generation: anchor.run_generation,
                epoch,
                instance: work.observer_instance.clone(),
                predecessor_generation: work.predecessor_generation,
                accepted_resume: work.source_resume.clone(),
                resume_usage_gap: anchor.gap,
                instances,
                observers: vec![Arc::downgrade(&work.origin)],
                curated_observer: None,
                codex_turns: Default::default(),
                copilot_events: Default::default(),
                cursor_generations: Default::default(),
                token: SecretString::from(token),
            }),
            pi_lifecycle: None,
        };
        if Instant::now() >= work.deadline
            || !work.origin.is_live()
            || !work.origin.writer_confirmed()
            || anchor
                .ancestry
                .as_ref()
                .is_none_or(|ancestry| !ancestry.is_live())
        {
            return Err(MezError::conflict(
                "source resume evidence expired before publication",
            ));
        }
        let response = enrollment_response(
            &binding,
            binding
                .enrollment
                .as_ref()
                .ok_or_else(|| MezError::invalid_state("source resume owner disappeared"))?,
            &work.session_id,
        );
        let (registry, next_generation) = self.control.external_agents_and_generation_mut();
        registry.bindings.remove(&anchor.digest);
        registry.enrollments.helper_targets.remove(
            binding.uid,
            &work.harness,
            &work.session_id,
            anchor.digest,
        );
        registry.enrollments.helper_targets.insert(
            binding.uid,
            &work.harness,
            &work.session_id,
            digest,
        );
        *next_generation = generation;
        registry.bindings.insert(digest, binding);
        self.record_external_source_anchor(digest);
        self.presentation.set_pane_harness_status(
            &anchor.pane_id,
            &format!("external-registration:{}", anchor.generation),
            None,
        );
        Ok(response)
    }
}
