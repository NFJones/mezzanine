//! Typed ordinary Pi facts reduced by the existing daemon-owned lifecycle owner.
//!
//! Capability, exact native producer/session and observer-generation checks stay
//! in restricted external ingress. This owner accepts only the existing Pi IPC
//! event enum, enforces contiguous observation identity and exact latest replay,
//! and reuses LifecycleOwner for provisional versus settled outcomes/UI waits.
//! No vendor callback/content, native control, provider work or usage is admitted.
//! Observer rotation resets this reducer together with presentation ownership.

use crate::control::JsonRpcRequest;
use crate::error::{MezError, Result};
use crate::integrations::bootstrap::{
    pi_ipc,
    pi_owner::{LifecycleOwner, Operation},
};
use crate::runtime::RuntimeSessionService;

/// Typed validation includes metadata already authenticated by outer ingress;
/// duplicate/unknown JSON keys cannot be overwritten before event projection.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    /// Private credential spelling is validated outside this owner, never logged.
    #[serde(rename = "launch_token")]
    _token: String,
    /// Exact server-issued observer generation, already authenticated.
    #[serde(rename = "generation")]
    _generation: u64,
    /// Immutable external session selected by authenticated registration.
    external_session_id: String,
    /// Positive, contiguous producer observation sequence within this epoch.
    sequence: u64,
    /// Existing strict allowlisted event enum, not a generic upstream payload.
    event: pi_ipc::Event,
}

/// One latest semantic receipt; older identities are rejected, never reprojected.
#[derive(Debug, Clone)]
struct Receipt {
    sequence: u64,
    event: Vec<u8>,
    response: String,
}

/// Bounded reducer state, independent of process and transport lifetime facts.
#[derive(Debug, Clone)]
pub(super) struct PiLifecycleProjection {
    owner: LifecycleOwner,
    latest: Option<Receipt>,
}

impl RuntimeSessionService {
    /// Settles a typed event only after exact capability/producer authorization.
    /// The reducer clone is committed after source-owned projections succeed;
    /// exact latest reply-loss retry returns its original fixed acknowledgment.
    pub(super) fn apply_external_pi_observation(
        &mut self,
        digest: [u8; 32],
        request: &JsonRpcRequest,
    ) -> Result<String> {
        let raw = request.params.as_deref().unwrap_or("{}");
        if raw.len() > 4096 {
            return Err(MezError::invalid_args(
                "Pi observation metadata exceeds limit",
            ));
        }
        let input: Envelope = serde_json::from_str(raw)
            .map_err(|_| MezError::invalid_args("Pi observation unavailable"))?;
        let event = serde_json::to_vec(&input.event)
            .map_err(|_| MezError::invalid_args("Pi observation unavailable"))?;
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .filter(|binding| binding.harness == "pi" && binding.enrollment.is_some())
            .ok_or_else(|| MezError::forbidden("ordinary Pi enrollment unavailable"))?;
        let registration = binding
            .registration
            .as_ref()
            .filter(|registration| registration.external_session_id == input.external_session_id)
            .ok_or_else(|| MezError::forbidden("Pi observation session differs"))?;
        if let Some(receipt) = binding
            .pi_lifecycle
            .as_ref()
            .and_then(|projection| projection.latest.as_ref())
            && input.sequence == receipt.sequence
            && event == receipt.event
        {
            return Ok(receipt.response.clone());
        }
        if binding.retired {
            return Err(MezError::conflict("Pi observer has retired"));
        }
        if binding.pi_lifecycle.is_none() && registration.presentation.is_some() {
            return Err(MezError::conflict(
                "generic presentation already owns this observer",
            ));
        }
        let mut projection = match &binding.pi_lifecycle {
            Some(projection) => projection.clone(),
            None => PiLifecycleProjection {
                owner: LifecycleOwner::new(&input.external_session_id)?,
                latest: None,
            },
        };
        let expected = projection
            .latest
            .as_ref()
            .map_or(Some(1), |receipt| receipt.sequence.checked_add(1));
        if expected != Some(input.sequence) {
            return Err(MezError::conflict(
                "Pi observation is stale, conflicting or has a gap",
            ));
        }
        let fact = pi_ipc::observation(&input.external_session_id, &event)?;
        projection.owner.observe(
            projection.owner.observer_epoch(),
            &input.external_session_id,
            fact,
        )?;
        let mut retired = false;
        while let Some(delivery) = projection.owner.pending().cloned() {
            match delivery.operation {
                Operation::Present(state) => {
                    self.update_external_presentation(
                        digest,
                        &serde_json::json!({
                            "external_session_id":input.external_session_id,
                            "sequence":delivery.sequence,"state":state,
                        }),
                    )?;
                }
                Operation::Retire => {
                    self.retire_external_agent_binding(digest);
                    retired = true;
                }
            }
            if !projection.owner.acknowledge(&delivery) {
                return Err(MezError::invalid_state(
                    "Pi observation delivery owner changed",
                ));
            }
        }
        let response =
            serde_json::json!({"accepted":true,"sequence":input.sequence,"retired":retired})
                .to_string();
        projection.latest = Some(Receipt {
            sequence: input.sequence,
            event,
            response: response.clone(),
        });
        self.control
            .external_agents_mut()
            .bindings
            .get_mut(&digest)
            .ok_or_else(|| MezError::invalid_state("Pi observation owner disappeared"))?
            .pi_lifecycle = Some(projection);
        Ok(response)
    }
}
