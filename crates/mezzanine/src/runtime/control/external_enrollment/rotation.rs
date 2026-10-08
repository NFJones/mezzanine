//! Serialized observer-instance replacement within one qualified native run.
//!
//! Instance and predecessor values are selectors, never process authority. The
//! parent admission already fenced native origin/ancestry/root, and this owner
//! rechecks producer liveness, writer evidence and deadline before rotation.
//! Replacement keeps immutable agent/accounting provenance but changes private
//! credentials, generation and presentation sequence owner. Retired instance IDs
//! are retained with a finite fail-closed bound; no old instance is evicted to
//! permit stale takeover. No provider or expense work is replayed.
//! Socket sources retain socket-native authorization and private replies; curated
//! sources revalidate their original declared native creator, retain its namespace
//! lifetime anchor, return only public selectors and reset observer freshness.

use super::*;

impl RuntimeSessionService {
    /// Retries the same accepted instance or replaces it only against the exact
    /// current predecessor generation. Concurrent/out-of-order replacements from
    /// the same predecessor cannot both commit; stale private handles disappear.
    pub(super) fn replace_or_retry_external_observer(
        &mut self,
        digest: [u8; 32],
        work: &ExternalEnrollmentWork,
        connection: &ControlConnectionState,
    ) -> Result<String> {
        let registry = self.control.external_agents_mut();
        let binding = registry
            .bindings
            .get_mut(&digest)
            .ok_or_else(|| MezError::conflict("external observer run unavailable"))?;
        let registration = binding.registration.as_ref().ok_or_else(|| {
            MezError::invalid_state("external enrollment registration disappeared")
        })?;
        if binding.version != work.version || registration.display_name != work.display_name {
            return Err(MezError::conflict(
                "external enrollment metadata differs from existing run",
            ));
        }
        let enrollment = binding
            .enrollment
            .as_mut()
            .ok_or_else(|| MezError::invalid_state("external enrollment owner disappeared"))?;
        let curated = work.curated.as_ref();
        if let Some(curated) = curated {
            curated.authorize_existing(work, enrollment)?;
        } else {
            enrollment.authorize(connection)?;
        }
        if Instant::now() >= work.deadline {
            return Err(MezError::conflict("external observer evidence expired"));
        }
        if enrollment.instance == work.observer_instance {
            if enrollment.predecessor_generation != work.predecessor_generation {
                return Err(MezError::conflict(
                    "external observer retry predecessor differs",
                ));
            }
            enrollment.observe_connection(&work.origin);
            let enrollment = binding
                .enrollment
                .as_ref()
                .ok_or_else(|| MezError::invalid_state("external enrollment owner disappeared"))?;
            return Ok(if curated.is_some() {
                curated::public_response(binding, enrollment, &work.session_id)
            } else {
                enrollment_response(binding, enrollment, &work.session_id)
            });
        }
        if enrollment.instances.contains(&work.observer_instance) {
            return Err(MezError::conflict("external observer instance has retired"));
        }
        if work.predecessor_generation != Some(binding.generation) {
            return Err(MezError::conflict("external observer predecessor changed"));
        }
        if enrollment.instances.len() >= MAX_OBSERVER_INSTANCES {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "external observer instance capacity exhausted",
            ));
        }
        let epoch = enrollment
            .epoch
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("external observer epoch exhausted"))?;
        let generation = registry
            .next_generation
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("external observer generation exhausted"))?;
        if curated.is_some()
            && (generation > 9_007_199_254_740_991 || epoch > 9_007_199_254_740_991)
        {
            return Err(MezError::invalid_state(
                "curated public observer identity exhausted",
            ));
        }
        let namespace_digest = if curated.is_some() {
            Some(registry.enrollments.curated_namespaces.digest_mut(digest)?)
        } else {
            None
        };
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let replacement_digest = Sha256::digest(token.as_bytes()).into();
        // No native/store work after this last fence and before publication.
        // Missing invariant state below restores the original binding unchanged.
        if Instant::now() >= work.deadline
            || !work.origin.is_live()
            || !work.origin.writer_confirmed()
            || !enrollment.provenance_is_live()
        {
            return Err(MezError::conflict(
                "external observer evidence expired before replacement",
            ));
        }
        let mut binding = registry
            .bindings
            .remove(&digest)
            .ok_or_else(|| MezError::invalid_state("external observer run disappeared"))?;
        let old_generation = binding.generation;
        let Some(mut enrollment) = binding.enrollment.take() else {
            registry.bindings.insert(digest, binding);
            return Err(MezError::invalid_state(
                "external observer owner disappeared",
            ));
        };
        enrollment.epoch = epoch;
        enrollment.instance = work.observer_instance.clone();
        enrollment.predecessor_generation = Some(old_generation);
        enrollment.instances.insert(work.observer_instance.clone());
        enrollment.token = SecretString::from(token);
        enrollment.observers.clear();
        if curated.is_some() {
            enrollment.curated_observer = Some(curated::CuratedObserver::default());
        }
        enrollment.observe_connection(&work.origin);
        if let Some(registration) = &mut binding.registration {
            registration.presentation = None;
        }
        binding.pi_lifecycle = None;
        binding.generation = generation;
        binding.expires = current_unix_seconds().saturating_add(60);
        let response = if curated.is_some() {
            curated::public_response(&binding, &enrollment, &work.session_id)
        } else {
            enrollment_response(&binding, &enrollment, &work.session_id)
        };
        binding.enrollment = Some(enrollment);
        let pane = binding.pane_id.clone();
        registry.next_generation = generation;
        if let Some(namespace_digest) = namespace_digest {
            *namespace_digest = replacement_digest;
        }
        registry.enrollments.helper_targets.remove(
            binding.uid,
            &work.harness,
            &work.session_id,
            digest,
        );
        registry.enrollments.helper_targets.insert(
            binding.uid,
            &work.harness,
            &work.session_id,
            replacement_digest,
        );
        registry.bindings.insert(replacement_digest, binding);
        self.presentation.set_pane_harness_status(
            &pane,
            &format!("external-registration:{old_generation}"),
            None,
        );
        Ok(response)
    }
}
