//! Declared curated-command creator admission, distinct from generic hook helpers.
//!
//! The Claude curated API contract selects its native direct creator, qualified by
//! the helper's own kernel origin/writer and retained parent/root lifetimes. The
//! profile/session/event labels remain self-reported metadata, not vendor attestation.
//! No payload PID/name/focus/route authorizes a creator. Actor commit allocates only
//! observational registration and retains private credentials service-side; public
//! replies cannot authorize producer sockets. Parent survival is not observer health.

use super::*;

/// Recent original-epoch observer proof, not creator life or caller lease renewal.
/// Equal sequence is an inert retry and cannot refresh a lost observer forever.
#[derive(Debug, Default)]
pub(super) struct CuratedObserver {
    /// Greatest authorized original-epoch proof sequence; replay is time-inert.
    pub(super) sequence: u64,
    /// Local monotonic observation time, never a supplied timestamp or lease.
    pub(super) observed_at: Option<Instant>,
}

impl CuratedObserver {
    /// Records only a new positive sequence under native actor authorization;
    /// stale/invalid sequences reject, and identical replay leaves time unchanged.
    pub(super) fn observe(&mut self, sequence: u64) -> Result<String> {
        if sequence == 0 || sequence > 9_007_199_254_740_991 || sequence < self.sequence {
            return Err(MezError::conflict("curated observer sequence unavailable"));
        }
        let changed = sequence > self.sequence;
        if changed {
            self.sequence = sequence;
            self.observed_at = Some(Instant::now());
        }
        Ok(serde_json::json!({"observed":true,"sequence":sequence,"changed":changed}).to_string())
    }
    /// A finite monotonic proof window, separate from the daemon's 60s lease.
    /// Kernel source/root liveness is checked independently by the binding owner.
    pub(super) fn is_recent(&self) -> bool {
        self.observed_at
            .is_some_and(|observed| observed.elapsed() < Duration::from_secs(30))
    }
}

/// Bounded runtime-only creator/session ownership, independent of capability GC.
/// An entry shares the original source anchor (no duplicate FD/capability) and
/// grants no live registration or observer rights. Exhaustion rejects, never
/// evicts a living namespace merely to permit implicit replacement.
#[derive(Debug, Default)]
pub(in crate::runtime::control) struct CuratedNamespaces {
    entries: Vec<CuratedNamespace>,
}

/// One original native source/root/session and its non-bearer binding selector.
#[derive(Debug)]
struct CuratedNamespace {
    producer: ProducerEvidence,
    pane_id: String,
    process: RuntimePaneProcessIdentity,
    session_id: String,
    digest: [u8; 32],
}

impl CuratedNamespaces {
    /// Releases only entries whose exact kernel creator lifetime has completed.
    /// Expiry, missing helpers, tombstone GC and snapshot binding cleanup alone
    /// cannot make the same living creator/session eligible as a new namespace.
    pub(in crate::runtime::control) fn prune(&mut self) {
        self.entries.retain(|entry| entry.producer.is_live());
    }
    /// Keeps maintenance active until final creator death permits FD release.
    pub(in crate::runtime::control) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// Borrows the unique active namespace selector before rotation publication.
    /// Absence/ambiguity fails before effects; changing this selector grants no
    /// new source authority and does not discard the original lifetime anchor.
    pub(super) fn digest_mut(&mut self, digest: [u8; 32]) -> Result<&mut [u8; 32]> {
        if self
            .entries
            .iter()
            .filter(|entry| entry.digest == digest)
            .count()
            != 1
        {
            return Err(MezError::conflict("curated namespace selector unavailable"));
        }
        self.entries
            .iter_mut()
            .find(|entry| entry.digest == digest)
            .map(|entry| &mut entry.digest)
            .ok_or_else(|| MezError::conflict("curated namespace selector unavailable"))
    }
    /// Selects only the original physically qualified creator/root/session;
    /// candidate metadata equality still requires original binding/live checks.
    fn find(
        &self,
        work: &ExternalEnrollmentWork,
        producer: &ProducerEvidence,
    ) -> Option<([u8; 32], ProducerEvidence)> {
        self.entries
            .iter()
            .find(|entry| {
                entry.pane_id == work.pane_id
                    && entry.process.same_incarnation(&work.process)
                    && entry.session_id == work.session_id
                    && entry
                        .producer
                        .matches_parent(producer.uid(), producer.identity())
            })
            .map(|entry| (entry.digest, entry.producer.clone()))
    }
    /// Complete finite namespace reservation check before any new binding effect.
    fn require_capacity(&self) -> Result<()> {
        if self.entries.len() >= super::super::external_agents::MAX_BINDINGS {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "curated namespace capacity unavailable",
            ));
        }
        Ok(())
    }
    /// Records an accepted original source only; digest is not a bearer token.
    fn remember(
        &mut self,
        work: &ExternalEnrollmentWork,
        producer: &ProducerEvidence,
        digest: [u8; 32],
    ) -> Result<()> {
        self.require_capacity()?;
        self.entries.push(CuratedNamespace {
            producer: producer.clone(),
            pane_id: work.pane_id.clone(),
            process: work.process.clone(),
            session_id: work.session_id.clone(),
            digest,
        });
        Ok(())
    }
}

/// One strict declaration and one-time off-actor verified parent publication.
#[derive(Debug, Clone)]
pub(super) struct CuratedAdmission {
    producer: Arc<OnceLock<ProducerEvidence>>,
}

impl CuratedAdmission {
    /// Revalidates the original retained parent kind and current native helper
    /// against the off-actor captured creator. This grants no socket authority;
    /// the caller already checked exact namespace/root and actor ingress fences.
    pub(super) fn authorize_existing(
        &self,
        work: &ExternalEnrollmentWork,
        source: &EnrollmentBinding,
    ) -> Result<()> {
        let producer = self
            .producer
            .get()
            .ok_or_else(|| MezError::forbidden("curated creator evidence missing"))?;
        producer
            .reobserve()
            .map_err(|_| MezError::forbidden("curated creator unavailable"))?;
        source
            .producer
            .reobserve()
            .map_err(|_| MezError::forbidden("original curated creator unavailable"))?;
        let child = work
            .origin
            .reobserve()
            .map_err(|_| MezError::forbidden("curated helper unavailable"))?;
        if !source
            .producer
            .matches_parent(producer.uid(), producer.identity())
            || work.origin.uid() != producer.uid()
            || child.parent_process_id != producer.identity().process_id
            || !work.origin.writer_confirmed()
            || !source.provenance_is_live()
            || !work.ancestry.get().is_some_and(|ancestry| {
                ancestry.is_live() && ancestry.source_matches(producer.uid(), producer.identity())
            })
        {
            return Err(MezError::forbidden(
                "curated original creator relationship changed",
            ));
        }
        Ok(())
    }
    /// Accepts only the known declared no-shell creator contract and a normalized
    /// initial main-session boundary. Unknown/helper/server/compact declarations
    /// fail before reservation; metadata does not attest executable identity.
    pub(super) fn from_params(params: &serde_json::Value) -> Result<Self> {
        if text(params, "harness", 64)? != "claude"
            || text(params, "observer_kind", 64)? != "curated-command"
            || text(params, "source_contract", 64)? != "claude-curated-command/1"
            || !matches!(
                text(params, "session_boundary", 32)?.as_str(),
                "startup" | "resume" | "clear" | "fork"
            )
        {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotImplemented,
                "curated enrollment requires the declared Claude command creator contract",
            ));
        }
        Ok(Self {
            producer: Arc::new(OnceLock::new()),
        })
    }

    /// Captures the actual same-user creator and its own root chain off actor.
    /// Source-as-root and changed/dead/unsupported/native-capacity evidence reject;
    /// successful Result without this immutable publication cannot grant a run.
    pub(super) fn observe(&self, work: &ExternalEnrollmentWork) -> Result<()> {
        let parent = work
            .origin
            .capture_parent()
            .map_err(|_| MezError::forbidden("curated creator unavailable"))?;
        if parent.uid() != work.origin.uid()
            || parent.identity.process_id == work.process.process_id
        {
            return Err(MezError::forbidden(
                "curated creator must be a distinct same-user pane descendant",
            ));
        }
        let root = mez_mux::process::process_parent_identity_for_pid(work.process.process_id)
            .filter(|root| root.start_token == work.process.start_token)
            .ok_or_else(|| MezError::conflict("curated pane root changed"))?;
        let ancestry = parent
            .capture_ancestry(root, &work.ancestry_budget)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    MezError::new(
                        crate::error::MezErrorKind::RateLimited,
                        "curated ancestry capacity unavailable",
                    )
                } else {
                    MezError::forbidden("curated creator ancestry unavailable")
                }
            })?;
        let producer = ProducerEvidence::VerifiedParent(Arc::new(parent));
        producer
            .reobserve()
            .map_err(|_| MezError::forbidden("curated creator changed"))?;
        let child = work
            .origin
            .reobserve()
            .map_err(|_| MezError::forbidden("curated helper changed"))?;
        if child.parent_process_id != producer.identity().process_id
            || !work.origin.writer_confirmed()
            || !ancestry.is_live()
            || Instant::now() >= work.deadline
        {
            return Err(MezError::forbidden(
                "curated creator relationship or deadline changed",
            ));
        }
        self.producer
            .set(producer)
            .map_err(|_| MezError::conflict("curated creator already observed"))?;
        work.ancestry
            .set(Arc::new(ancestry))
            .map_err(|_| MezError::conflict("curated ancestry already observed"))?;
        Ok(())
    }
}

impl RuntimeSessionService {
    /// Commits only after common reservation/ingress/root fencing and exact native
    /// creator publication. Identical same-source/session/instance retry keeps the
    /// original run; explicit predecessor-fenced handoff shares the rotation
    /// owner without inferring observer health or reviving retired namespaces.
    pub(super) fn commit_curated_enrollment(
        &mut self,
        work: &ExternalEnrollmentWork,
        curated: &CuratedAdmission,
        ancestry: &Arc<UnixAncestryWitness>,
        connection: &ControlConnectionState,
    ) -> Result<String> {
        let producer = curated
            .producer
            .get()
            .ok_or_else(|| MezError::forbidden("curated creator evidence missing"))?;
        let child = work
            .origin
            .reobserve()
            .map_err(|_| MezError::forbidden("curated helper unavailable"))?;
        producer
            .reobserve()
            .map_err(|_| MezError::forbidden("curated creator unavailable"))?;
        if producer.uid() != work.origin.uid()
            || child.parent_process_id != producer.identity().process_id
            || producer.identity().process_id == work.process.process_id
            || !ancestry.is_live()
            || !ancestry.source_matches(producer.uid(), producer.identity())
        {
            return Err(MezError::forbidden("curated creator proof changed"));
        }
        self.reconcile_external_agent_registrations();
        let existing = self
            .control
            .external_agents()
            .enrollments
            .curated_namespaces
            .find(work, producer);
        if let Some((digest, original_source)) = existing {
            let binding = self
                .control
                .external_agents()
                .bindings
                .get(&digest)
                .ok_or_else(|| MezError::conflict("curated creator/session namespace retired"))?;
            let source = binding
                .enrollment
                .as_ref()
                .ok_or_else(|| MezError::invalid_state("curated owner missing"))?;
            if binding.retired
                || !source.producer.same_owner(&original_source)
                || !source.provenance_is_live()
                || binding.version != work.version
                || binding
                    .registration
                    .as_ref()
                    .is_none_or(|registration| registration.display_name != work.display_name)
                || Instant::now() >= work.deadline
                || !work.origin.writer_confirmed()
            {
                return Err(MezError::conflict(
                    "curated observer source or instance changed",
                ));
            }
            return self.replace_or_retry_external_observer(digest, work, connection);
        }
        if work.predecessor_generation.is_some() {
            return Err(MezError::conflict("curated predecessor unavailable"));
        }
        self.control
            .external_agents()
            .enrollments
            .curated_namespaces
            .require_capacity()?;
        self.refresh_project_trust_store_from_disk_if_changed()?;
        let project_scope = self
            .trusted_project_root_for_pane(&work.pane_id)
            .map(mez_agent::messaging::ProjectMembership::from_canonical_root)
            .map(|membership| membership.scope_id());
        let accounting_origin = self.capture_accounting_origin_for_pane(&work.pane_id);
        let registry = self.control.external_agents_mut();
        if registry.bindings.len() >= super::super::external_agents::MAX_BINDINGS {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "curated registry is full",
            ));
        }
        if !producer.is_live()
            || !ancestry.is_live()
            || !work.origin.is_live()
            || !work.origin.writer_confirmed()
            || Instant::now() >= work.deadline
        {
            return Err(MezError::conflict(
                "curated evidence expired before allocation",
            ));
        }
        let generation = registry
            .next_generation
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("curated generation exhausted"))?;
        if generation > 9_007_199_254_740_991 {
            return Err(MezError::invalid_state(
                "curated public observer identity exhausted",
            ));
        }
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let digest = Sha256::digest(token.as_bytes()).into();
        registry.next_generation = generation;
        registry.bindings.insert(
            digest,
            LaunchBinding {
                uid: producer.uid(),
                pane_id: work.pane_id.clone(),
                process: work.process.clone(),
                project_scope,
                generation,
                harness: work.harness.clone(),
                version: work.version.clone(),
                expires: current_unix_seconds().saturating_add(60),
                registration: None,
                retired: false,
                accounting_owner: crate::storage::token_usage::new_token_usage_event_id(),
                accounting_origin,
                enrollment: Some(EnrollmentBinding {
                    producer: producer.clone(),
                    ancestry: Some(ancestry.clone()),
                    run_generation: generation,
                    epoch: 1,
                    instance: work.observer_instance.clone(),
                    predecessor_generation: None,
                    instances: BTreeSet::from([work.observer_instance.clone()]),
                    observers: Vec::new(),
                    curated_observer: Some(CuratedObserver::default()),
                    token: SecretString::from(token),
                }),
                pi_lifecycle: None,
            },
        );
        if let Err(error) = self.register_external_agent(digest, &serde_json::json!({"external_session_id":work.session_id,"display_name":work.display_name})) {
            self.retire_external_agent_binding(digest); return Err(error);
        }
        if let Err(error) = self
            .control
            .external_agents_mut()
            .enrollments
            .curated_namespaces
            .remember(work, producer, digest)
        {
            self.retire_external_agent_binding(digest);
            return Err(error);
        }
        self.control
            .external_agents_mut()
            .enrollments
            .helper_targets
            .insert(producer.uid(), &work.harness, &work.session_id, digest);
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .ok_or_else(|| MezError::invalid_state("curated binding disappeared"))?;
        let source = binding
            .enrollment
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("curated owner disappeared"))?;
        Ok(public_response(binding, source, &work.session_id))
    }
}

/// Public immutable selectors only. Neutral callback success is not this receipt;
/// no bearer credential, socket authority or verified billing identity is exposed.
pub(super) fn public_response(
    binding: &LaunchBinding,
    source: &EnrollmentBinding,
    session: &str,
) -> String {
    serde_json::json!({"protocol":"external-agent/1","registered":true,"controls":[],
        "agent_id":binding.registration.as_ref().map(|registration| registration.agent_id.as_str()),
        "generation":binding.generation,"observer_witness":observer_witness(Sha256::digest(source.token.expose_secret().as_bytes()).into()),
        "run_id":source.run_generation,"observer_epoch":source.epoch,"observer_instance":source.instance,
        "external_session_id":session,"usage":"unavailable-source-continuity","observer_transport":"unavailable-curated-freshness",
        "expires_at_unix_seconds":binding.expires,"lease_seconds":60}).to_string()
}
