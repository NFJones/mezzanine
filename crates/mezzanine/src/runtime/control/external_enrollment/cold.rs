//! Explicit same-native-source recovery selectors, never persisted credentials.
//!
//! A selector narrows freshly observed kernel/boot/ancestry evidence and one
//! original accepted activation. Namespace existence alone still cannot admit.
//! Actual registration precedes the required exact observer claim; the private
//! reply stays withheld until that claim and final actor checks succeed.

use super::*;

/// Public historical receipt copied without private observer credentials.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    protocol: String,
    activation_id: String,
    observer_generation: String,
}

/// One immutable cold intent. Reconnect repeats this EXACT public operation,
/// not the failed private handle or a newly generated claim/namespace.
#[derive(Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ColdSourceResume {
    receipt: Receipt,
    operation_id: String,
}

impl std::fmt::Debug for ColdSourceResume {
    /// Public opaque selectors are still not useful diagnostic payloads.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColdSourceResume").finish_non_exhaustive()
    }
}

impl ColdSourceResume {
    /// Strict optional closed grammar before reserving native work. Cold and
    /// same-live/curated/predecessor modes cannot silently fall back or mix.
    pub(super) fn from_params(params: &serde_json::Value) -> Result<Option<Self>> {
        let Some(value) = params.get("durable_source_resume") else {
            return Ok(None);
        };
        let unavailable = || MezError::invalid_args("durable source resume selector unavailable");
        if params.get("source_resume").is_some() || params.get("predecessor_generation").is_some() {
            return Err(unavailable());
        }
        let selector: Self = serde_json::from_value(value.clone()).map_err(|_| unavailable())?;
        if selector.receipt.protocol != "external-source-activation/1"
            || decode_id(&selector.receipt.activation_id).is_none()
            || decode_id(&selector.operation_id).is_none()
            || selector.generation().is_none()
        {
            return Err(unavailable());
        }
        Ok(Some(selector))
    }

    /// Exact canonical decimal text within the store's positive checked range.
    fn generation(&self) -> Option<u64> {
        let text = &self.receipt.observer_generation;
        if text.is_empty()
            || text.len() > 19
            || text.starts_with('0')
            || !text.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        text.parse::<u64>()
            .ok()
            .filter(|value| *value > 0 && *value < i64::MAX as u64)
    }

    /// Selects only the original immutable public attempt, not its native proof.
    pub(super) fn matches_activation(
        &self,
        activation: &crate::storage::token_usage::ExternalSourceActivation,
    ) -> bool {
        activation.public_id() == self.receipt.activation_id
    }

    /// Distinct claims require the exact predecessor. Exact committed intent
    /// replay may observe ONLY predecessor+1 with this same instance/operation;
    /// initial no-op, old generation/instance and arbitrary latest all refuse.
    pub(super) fn mutation(
        &self,
        previous: &crate::storage::token_usage::ExternalSourceObserverFence,
        instance: &str,
    ) -> Result<source_observer::ObserverMutation> {
        let unavailable = || MezError::conflict("durable source observer claim unavailable");
        let generation = self.generation().ok_or_else(unavailable)?;
        let change = previous.propose(
            generation,
            instance,
            decode_id(&self.operation_id).ok_or_else(unavailable)?,
        )?;
        if change.next_fence().generation() != generation.checked_add(1).ok_or_else(unavailable)? {
            return Err(unavailable());
        }
        Ok(source_observer::ObserverMutation {
            change,
            scope: source_observer::ObserverScope::new(Arc::new(previous.clone())),
            history_nonce: None,
        })
    }

    /// Published replay formatting only, never evidence that publication ran.
    pub(super) fn successor_receipt(&self) -> serde_json::Value {
        serde_json::json!({"protocol":"external-source-activation/1","activation_id":self.receipt.activation_id,
            "observer_generation":self.generation().and_then(|generation|generation.checked_add(1)).map(|generation|generation.to_string())})
    }
}

/// Fixed-size canonical public IDs, nonzero and never interpreted as tokens.
fn decode_id(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut bytes = [0; 32];
    for (index, pair) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let nibble = |byte: u8| {
            if byte.is_ascii_digit() {
                byte - b'0'
            } else {
                byte - b'a' + 10
            }
        };
        bytes[index] = (nibble(pair[0]) << 4) | nibble(pair[1]);
    }
    (bytes != [0; 32]).then_some(bytes)
}

impl RuntimeSessionService {
    /// Reuses an already published SAME cold intent only under this current
    /// exact native binding. It does not attach to a pending/unexposed grant.
    pub(super) fn cold_reply_is_published(&self, work: &ExternalEnrollmentWork) -> bool {
        if !Arc::ptr_eq(
            &work.registry_incarnation,
            &self.control.external_agents().enrollments.incarnation,
        ) || self.persistence.cloned_token_usage_store() != work.source_store
        {
            return false;
        }
        let Some(now) = source_clock::native_boot_millis() else {
            return false;
        };
        if work
            .source_preparation
            .get()
            .is_none_or(|preparation| !preparation.cold_observation_is_accepted_at(now))
        {
            return false;
        }
        let Some(selector) = work.cold_resume.as_ref() else {
            return false;
        };
        self.control
            .external_agents()
            .bindings
            .values()
            .any(|binding| {
                !binding.retired
                    && binding.pane_id == work.pane_id
                    && binding.harness == work.harness
                    && work.source_owner.get().and_then(Option::as_ref)
                        == Some(&binding.accounting_owner)
                    && binding.process.same_incarnation(&work.process)
                    && binding.registration.as_ref().is_some_and(|registration| {
                        registration.external_session_id == work.session_id
                    })
                    && binding.enrollment.as_ref().is_some_and(|enrollment| {
                        enrollment.cold_published
                            && enrollment.accepted_cold_resume.as_ref() == Some(selector)
                            && enrollment.instance == work.observer_instance
                            && enrollment.producer.matches_socket(&work.origin)
                    })
            })
    }

    /// A failed phase transfer must never expose a tentative cold private reply.
    /// Published exact retries may return their existing handle without redoing
    /// a claim; all unexposed failures retire memory only, not another DB owner.
    pub(super) fn reject_unpublished_cold(
        &mut self,
        work: &ExternalEnrollmentWork,
        body: String,
    ) -> String {
        if work.cold_resume.is_none() || self.cold_reply_is_published(work) {
            return body;
        }
        self.discard_unpublished_cold(work);
        super::super::runtime_json_rpc_error(
            &work.request_id,
            crate::error::MezErrorKind::Conflict,
            "durable source publication unavailable",
        )
    }

    /// Final actor step uses only a constant-time native boot clock syscall,
    /// cached exact committed evidence and current source custody. No database,
    /// ancestry traversal or mutex wait runs on actor. Suspend can expire a boot
    /// window without advancing Instant, so that clock MUST be checked here.
    pub(super) fn publish_cold_claim(&mut self, work: &ExternalEnrollmentWork) -> Result<()> {
        let unavailable = || MezError::conflict("durable source final publication unavailable");
        let preparation = work.source_preparation.get().ok_or_else(unavailable)?;
        let receipt = preparation.accepted_receipt().ok_or_else(unavailable)?;
        let now = source_clock::native_boot_millis().ok_or_else(unavailable)?;
        if !receipt.is_accepted_at(now)
            || !work.source_preparation.renewal_is_open()
            || self.persistence.cloned_token_usage_store().as_ref()
                != Some(preparation.original_store())
        {
            return Err(unavailable());
        }
        let grant = preparation.admission.get().ok_or_else(unavailable)?;
        self.control
            .external_agents()
            .bindings
            .get(&grant.digest())
            .filter(|binding| binding.generation == grant.generation() && !binding.retired)
            .and_then(|binding| binding.enrollment.as_ref())
            .filter(|enrollment| enrollment.accepted_cold_resume == work.cold_resume)
            .ok_or_else(unavailable)?;
        // Projection preflight/adoption precedes exposing the private binding.
        // Its snapshot is sealed by the actual required native worker only.
        self.restore_external_source_usage(
            grant.digest(),
            grant.generation(),
            preparation.usage_snapshot().ok_or_else(unavailable)?,
        )?;
        // Snapshot adoption is bounded but not constant-time. Its historical
        // expense is independent of registration; it cannot extend eligibility
        // or let an expired private grant escape after that projection work.
        let now = source_clock::native_boot_millis().ok_or_else(unavailable)?;
        if !receipt.is_accepted_at(now) || !work.source_preparation.renewal_is_open() {
            return Err(unavailable());
        }
        let enrollment = self
            .control
            .external_agents_mut()
            .bindings
            .get_mut(&grant.digest())
            .and_then(|binding| binding.enrollment.as_mut())
            .ok_or_else(unavailable)?;
        enrollment.cold_published = true;
        self.record_external_source_anchor(grant.digest());
        Ok(())
    }

    /// Failure discards ONLY this unexposed grant's memory authority. Detach its
    /// anchor BEFORE central retirement so no failed claim tombstones somebody
    /// else's winning durable fence. Actual committed facts remain retryable.
    pub(crate) fn discard_unpublished_cold(&mut self, work: &ExternalEnrollmentWork) {
        if work.cold_resume.is_none()
            || !Arc::ptr_eq(
                &work.registry_incarnation,
                &self.control.external_agents().enrollments.incarnation,
            )
        {
            return;
        }
        let Some(grant) = work
            .source_preparation
            .get()
            .and_then(|preparation| preparation.admission.get())
        else {
            return;
        };
        let digest = grant.digest();
        if self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .is_some_and(|binding| {
                binding.generation == grant.generation()
                    && binding.enrollment.as_ref().is_some_and(|enrollment| {
                        !enrollment.cold_published
                            && enrollment.accepted_cold_resume == work.cold_resume
                    })
            })
        {
            work.source_preparation.invalidate_acceptance();
            self.discard_unpublished_source_anchor(digest);
            self.retire_external_agent_binding(digest);
        }
    }
}
