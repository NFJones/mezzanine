//! Current-source selection and final actor fencing for optional lifetime facts.
//!
//! Reuses the existing enrollment admission IDs/incarnation and persistence lane.
//! Scheduling hints never supply native time or authority. Neither cached worker
//! success nor an injected event can publish into another registry/binding.

use super::*;

/// Unserialized original actor custody; no launch credential or mutable selector.
pub(super) struct RenewalActorScope {
    registry: Arc<()>,
    admission: u64,
    digest: [u8; 32],
    generation: u64,
    run: u64,
    epoch: u64,
}

impl RuntimeSessionService {
    /// Nonblocking actor admission following actual connected-observer renewal.
    pub(in crate::runtime::control) fn queue_external_source_renewal(&mut self, digest: [u8; 32]) {
        self.queue_external_source_metadata(digest, false);
    }

    /// Follows actual warm handoff registration, never payload-only admission.
    /// Publication changes public metadata, not native/control/accounting rights.
    pub(in crate::runtime::control::external_enrollment) fn queue_external_source_observer_publication(
        &mut self,
        digest: [u8; 32],
    ) {
        self.queue_external_source_metadata(digest, true);
    }

    /// Common finite lane with distinct lifetime pacing versus explicit actor
    /// handoff. Both share original slot/native proof/physical permit/currentness.
    fn queue_external_source_metadata(&mut self, digest: [u8; 32], observer_change: bool) {
        if self.require_live().is_err() {
            return;
        }
        let Some(slot) = self.current_external_source_renewal_slot(digest) else {
            return;
        };
        if !self
            .persistence
            .cloned_token_usage_store()
            .is_some_and(|store| slot.renewal_store_matches(&store))
        {
            return;
        }
        let Some(binding) = self.control.external_agents().bindings.get(&digest) else {
            return;
        };
        if binding.retired
            || !binding.has_current_telemetry(current_unix_seconds())
            || !self.pane_process_identity_is_current(&binding.pane_id, &binding.process)
        {
            return;
        }
        let Some(enrollment) = &binding.enrollment else {
            return;
        };
        if !enrollment.provenance_is_live()
            || !matches!(enrollment.producer, ProducerEvidence::Socket(_))
        {
            return;
        }
        let Some(origin) = enrollment
            .observers
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .find(|origin| {
                origin.observer_connected() && enrollment.producer.matches_socket(origin)
            })
        else {
            return;
        };
        let Some(ancestry) = enrollment.ancestry.clone() else {
            return;
        };
        let admissions = &self.control.external_agents().enrollments;
        if admissions.pending.len() >= MAX_PENDING {
            return;
        }
        let Some(admission) = admissions.next.checked_add(1) else {
            return;
        };
        let scope = RenewalActorScope {
            registry: admissions.incarnation.clone(),
            admission,
            digest,
            generation: binding.generation,
            run: enrollment.run_generation,
            epoch: enrollment.epoch,
        };
        let observer = if observer_change {
            let Some(previous_scope) = self.current_external_source_observer_scope(digest) else {
                return;
            };
            let previous = previous_scope.current();
            let mut nonce = [0; 32];
            rand::rng().fill_bytes(&mut nonce);
            let Ok(change) = previous.propose_with_history(
                previous.generation(),
                &enrollment.instance,
                nonce,
                &enrollment.instances,
            ) else {
                return;
            };
            let next_scope = super::source_observer::ObserverScope::new(previous);
            Some((
                previous_scope,
                super::source_observer::ObserverMutation {
                    change,
                    scope: next_scope,
                    history_nonce: enrollment.history_nonce,
                },
            ))
        } else {
            None
        };
        let previous_scope = observer.as_ref().map(|(previous, _)| previous.clone());
        let Some(work) =
            slot.prepare_metadata(origin, ancestry, observer.map(|(_, mutation)| mutation))
        else {
            return;
        };
        // We now hold the original physical permit. A predecessor may have
        // committed between our snapshot and this acquisition; never replace
        // its known G2 scope with a pending proposal captured against G1.
        if let Some(previous) = &previous_scope
            && let Some(observer) = &work.0.observer
            && previous.current().as_ref() != observer.scope.current().as_ref()
        {
            return;
        }
        if !observer_change && !slot.admit_renewal_tick() {
            return;
        }
        if work.0.scope.set(scope).is_err() {
            return;
        }
        let admissions = &mut self.control.external_agents_mut().enrollments;
        admissions.next = admission;
        if !admissions.pending.insert(admission) {
            return;
        }
        if let Some(previous) = previous_scope
            && !self.install_external_source_observer_scope(
                digest,
                &previous,
                work.0
                    .observer
                    .as_ref()
                    .map(|observer| observer.scope.clone())
                    .unwrap_or(previous.clone()),
            )
        {
            self.control
                .external_agents_mut()
                .enrollments
                .pending
                .remove(&admission);
            return;
        }
        self.persistence.queue_token_usage(
            crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work },
        );
    }

    /// Only a real observed worker join can settle this exact admission. Stale
    /// registries cannot consume reused IDs; late/duplicate results never clear
    /// another source's health or restore lifecycle/observer/cold authority.
    pub(crate) fn complete_external_source_renewal(
        &mut self,
        work: &ExternalSourceRenewalWork,
    ) -> Option<bool> {
        let scope = work.0.scope.get()?;
        let success = *work.0.completion.get()?;
        let admissions = &mut self.control.external_agents_mut().enrollments;
        if !Arc::ptr_eq(&scope.registry, &admissions.incarnation)
            || !admissions.pending.remove(&scope.admission)
        {
            return None;
        }
        if self.require_live().is_err()
            || Instant::now() >= work.0.deadline
            || !work.0.slot.renewal_is_open()
            || !work.0.origin.observer_connected()
            || !work.0.ancestry.is_live()
        {
            return None;
        }
        let slot = self.current_external_source_renewal_slot(scope.digest)?;
        if !self
            .persistence
            .cloned_token_usage_store()
            .is_some_and(|store| slot.renewal_store_matches(&store))
        {
            return None;
        }
        if !Arc::ptr_eq(&slot, &work.0.slot) {
            return None;
        }
        let binding = self.control.external_agents().bindings.get(&scope.digest)?;
        let enrollment = binding.enrollment.as_ref()?;
        if binding.retired
            || binding.generation != scope.generation
            || enrollment.run_generation != scope.run
            || enrollment.epoch != scope.epoch
            || !binding.has_current_telemetry(current_unix_seconds())
            || !self.pane_process_identity_is_current(&binding.pane_id, &binding.process)
            || !enrollment.provenance_is_live()
            || !enrollment.producer.matches_socket(&work.0.origin)
            || !enrollment
                .observers
                .iter()
                .filter_map(std::sync::Weak::upgrade)
                .any(|origin| Arc::ptr_eq(&origin, &work.0.origin) && origin.observer_connected())
        {
            return None;
        }
        Some(success)
    }
}
