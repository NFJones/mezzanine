//! Classic Copilot command-hook observations under retained native creator proof.
//!
//! Session IDs and vendor milliseconds are inert selectors/order fences, never
//! provenance or wall-clock health. A unique original first-epoch binding and
//! the shared helper worker prove writer/direct-parent/root before settlement.
//! Shared ingress tickets serialize native work; no Codex turn semantics apply.
//! Equal replay is time-inert, conflicting ties/stale times fail closed, and only
//! the exact session end retires. Silence does not attest idle observer health.

use super::*;

/// One allowlisted coarse fact and its bounded vendor order selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CopilotEvent {
    kind: String,
    timestamp: u64,
}

impl CopilotEvent {
    /// Validates fixed facts without accepting a caller-selected state or title.
    fn parse(params: &serde_json::Value) -> Result<Self> {
        let kind = text(params, "event", 32)?;
        if !matches!(
            kind.as_str(),
            "userPromptSubmitted"
                | "preToolUse"
                | "postToolUse"
                | "permission_prompt"
                | "elicitation_dialog"
                | "agentStop"
                | "errorOccurred"
                | "sessionEnd"
        ) {
            return Err(MezError::invalid_args("Copilot event unavailable"));
        }
        let timestamp = params["timestamp"]
            .as_u64()
            .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
            .ok_or_else(|| MezError::invalid_args("Copilot event order unavailable"))?;
        Ok(Self { kind, timestamp })
    }
    /// Stop means ready, not success; permission notifications alone mean wait.
    fn state(&self) -> &'static str {
        match self.kind.as_str() {
            "permission_prompt" | "elicitation_dialog" => "waiting",
            "agentStop" => "ready",
            "errorOccurred" => "interrupted",
            _ => "running",
        }
    }
}

/// Constant-space replay fence and shared native-work queue per original source.
#[derive(Debug, Default)]
pub(super) struct CopilotEvents {
    last: Option<CopilotEvent>,
    order: Arc<super::codex_order::CodexOrder>,
}

impl CopilotEvents {
    /// Rejects ambiguous millisecond ties rather than inventing a turn identity.
    fn changed(&self, event: &CopilotEvent) -> Result<bool> {
        if let Some(last) = &self.last {
            if last == event {
                return Ok(false);
            }
            if event.timestamp <= last.timestamp {
                return Err(MezError::conflict("Copilot event order unavailable"));
            }
        }
        Ok(true)
    }
}

/// Selects only the fixed original command instance, never a newest successor.
fn original(binding: &LaunchBinding) -> Result<&EnrollmentBinding> {
    binding
        .enrollment
        .as_ref()
        .filter(|source| {
            binding.harness == "copilot"
                && !binding.retired
                && source.epoch == 1
                && source.instance == "copilot-local-hooks-1"
                && source.curated_observer.is_some()
                && matches!(source.producer, ProducerEvidence::VerifiedParent(_))
        })
        .ok_or_else(|| MezError::forbidden("Copilot original command instance unavailable"))
}

impl RuntimeSessionService {
    /// Narrows an existing indexed session then uses the common native pipeline.
    /// No public credential/generation or new enrollment is returned to a helper.
    pub(super) fn prepare_external_copilot_observation(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
    ) -> Result<ExternalEnrollmentWork> {
        self.require_live()?;
        if connection.initialized() {
            return Err(MezError::forbidden(
                "Copilot callback requires uninitialized Unix ingress",
            ));
        }
        let Some(AuthenticatedPeer::UnixUser { uid }) = connection.authenticated_peer() else {
            return Err(MezError::forbidden(
                "Copilot callback requires native Unix ingress",
            ));
        };
        crate::control::validate_control_method_params_schema(request)?;
        if connection
            .unix_origin()
            .is_none_or(|origin| origin.uid() != *uid || !origin.writer_confirmed())
        {
            return Err(MezError::forbidden("Copilot callback writer unavailable"));
        }
        let raw = request.params.as_deref().unwrap_or("{}");
        if raw.len() > 4096 {
            return Err(MezError::invalid_args(
                "Copilot callback metadata exceeds limit",
            ));
        }
        let params = crate::protocol::strict_json::decode(raw.as_bytes())
            .map_err(|_| MezError::invalid_args("Copilot callback requires unique fields"))?;
        let pane = text(&params, "pane_id", 64)?;
        let session = text(&params, "external_session_id", 128)?;
        let event = CopilotEvent::parse(&params)?;
        let targets = self
            .control
            .external_agents()
            .enrollments
            .helper_targets
            .candidates(*uid, "copilot", &session);
        for target in targets {
            self.reconcile_external_agent_registration(target);
        }
        let digest = self
            .control
            .external_agents()
            .enrollments
            .helper_targets
            .select(*uid, "copilot", &session)?;
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .filter(|binding| binding.uid == *uid && binding.pane_id == pane)
            .ok_or_else(|| MezError::forbidden("Copilot callback candidate unavailable"))?;
        let source = original(binding)?;
        let queue = source.copilot_events.order.clone();
        let callback = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"copilot-internal","method":"agent/external/helper-presentation","params":{
            "launch_token":source.token.expose_secret(),"generation":binding.generation,"external_session_id":session,"sequence":1,"state":"ready",
        }}).to_string())?;
        let mut work = self.prepare_external_helper_presentation(&callback, connection)?;
        let order = match queue.reserve() {
            Ok(order) => order,
            Err(error) => {
                self.control
                    .external_agents_mut()
                    .enrollments
                    .pending
                    .remove(&work.admission);
                return Err(error);
            }
        };
        work.request_id = request.id.clone();
        if let Some(helper) = &mut work.helper {
            helper.copilot = Some(event);
            helper.codex_order = Some(order);
        }
        Ok(work)
    }

    /// Publishes a native-qualified new fact and finite observer proof atomically.
    /// Timestamp replay never refreshes health; session end is not process death.
    pub(super) fn commit_external_copilot_observation(
        &mut self,
        digest: [u8; 32],
        session: &str,
        event: &CopilotEvent,
    ) -> Result<String> {
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .ok_or_else(|| MezError::forbidden("Copilot callback owner unavailable"))?;
        let source = original(binding)?;
        let previous = binding
            .registration
            .as_ref()
            .and_then(|registration| registration.presentation.as_ref())
            .map_or(0, |presentation| presentation.sequence);
        if !source.copilot_events.changed(event)? {
            return Ok(serde_json::json!({"changed":false,"sequence":previous}).to_string());
        }
        if event.kind == "sessionEnd" {
            self.retire_external_agent_binding(digest);
            return Ok(serde_json::json!({"retired":true,"sequence":previous}).to_string());
        }
        let sequence = previous
            .checked_add(1)
            .filter(|n| *n <= 9_007_199_254_740_991)
            .ok_or_else(|| MezError::invalid_state("Copilot sequence exhausted"))?;
        let reply = self.update_external_presentation(digest, &serde_json::json!({"external_session_id":session,"sequence":sequence,"state":event.state()}))?;
        let source = self
            .control
            .external_agents_mut()
            .bindings
            .get_mut(&digest)
            .and_then(|binding| binding.enrollment.as_mut())
            .ok_or_else(|| MezError::invalid_state("Copilot callback owner disappeared"))?;
        source.copilot_events.last = Some(event.clone());
        source
            .curated_observer
            .as_mut()
            .ok_or_else(|| MezError::invalid_state("Copilot observer unavailable"))?
            .observe(sequence)?;
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Replay is time-inert and ambiguous/stale timestamps cannot repaint a run.
    #[test]
    fn copilot_order_is_bounded_and_rejects_conflicting_ties() {
        let first =
            CopilotEvent::parse(&serde_json::json!({"event":"userPromptSubmitted","timestamp":10}))
                .unwrap();
        let owner = CopilotEvents {
            last: Some(first.clone()),
            ..Default::default()
        };
        assert!(!owner.changed(&first).unwrap());
        for (event, timestamp) in [("agentStop", 10), ("agentStop", 9)] {
            assert!(
                owner
                    .changed(
                        &CopilotEvent::parse(
                            &serde_json::json!({"event":event,"timestamp":timestamp})
                        )
                        .unwrap()
                    )
                    .is_err()
            );
        }
        for event in ["permissionRequest", "unknown"] {
            assert!(
                CopilotEvent::parse(&serde_json::json!({"event":event,"timestamp":11})).is_err()
            );
        }
        assert_eq!(
            CopilotEvent::parse(&serde_json::json!({"event":"permission_prompt","timestamp":11}))
                .unwrap()
                .state(),
            "waiting"
        );
    }
}
