//! Bounded first-epoch Codex command-hook observations, not generic control.
//!
//! A unique indexed session is only a candidate. The existing helper worker and
//! actor settlement prove native writer/direct-parent/root/generation before any
//! effect. No supplied credential, generation, title or newest-epoch discovery
//! is admitted. Turn order is arrival-based; known retired turns stay fenced.
//! SessionEnd retires only that exact session. New native facts stamp finite
//! observer proof, not continuous idle health; end never attests creator death.

use super::*;

/// Finite run-local turn history; exhaustion rejects without evicting old fences.
const MAX_TURNS: usize = 4096;

/// One code-owned coarse event and bounded opaque turn selector.
#[derive(Debug, Clone)]
pub(super) struct CodexEvent {
    kind: &'static str,
    turn: String,
}

impl CodexEvent {
    /// Rejects unsupported semantics and content-shaped turn IDs before work.
    fn parse(params: &serde_json::Value) -> Result<Self> {
        let kind = match text(params, "event", 32)?.as_str() {
            "UserPromptSubmit" => "UserPromptSubmit",
            "Stop" => "Stop",
            "Interrupt" => "Interrupt",
            "SessionEnd" => "SessionEnd",
            _ => return Err(MezError::invalid_args("Codex event unavailable")),
        };
        if kind == "SessionEnd" {
            if params.get("turn_id").is_some() {
                return Err(MezError::invalid_args(
                    "Codex session end cannot supply a turn selector",
                ));
            }
            return Ok(Self {
                kind,
                turn: String::new(),
            });
        }
        let turn = text(params, "turn_id", 128)?;
        if !turn
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        {
            return Err(MezError::invalid_args("Codex turn selector unavailable"));
        }
        Ok(Self { kind, turn })
    }
}

/// Per-original-instance replay fence; native life and usage are not derived here.
#[derive(Debug, Default)]
pub(super) struct CodexTurns {
    seen: BTreeSet<String>,
    current: Option<String>,
    state: Option<&'static str>,
    /// Native work order remains held until actor completion or cancellation.
    order: Arc<super::codex_order::CodexOrder>,
}

impl CodexTurns {
    /// Plans a coarse transition without mutation. Duplicate observations are
    /// time/sequence inert, old turns reject, and Interrupt dominates later Stop.
    fn state_for(&self, event: &CodexEvent) -> Result<Option<&'static str>> {
        if event.kind == "UserPromptSubmit" {
            if self.current.as_deref() == Some(&event.turn) {
                return Ok(None);
            }
            if self.seen.contains(&event.turn) {
                return Err(MezError::conflict("Codex turn has retired"));
            }
            if self.seen.len() >= MAX_TURNS {
                return Err(MezError::new(
                    crate::error::MezErrorKind::RateLimited,
                    "Codex turn fence capacity unavailable",
                ));
            }
            return Ok(Some("running"));
        }
        if self.current.as_deref() != Some(&event.turn) {
            return Err(MezError::conflict("Codex current turn unavailable"));
        }
        let state = if event.kind == "Stop" {
            "ready"
        } else {
            "interrupted"
        };
        Ok((self.state != Some(state) && self.state != Some("interrupted")).then_some(state))
    }

    /// Publishes only after successful common presentation settlement, with no
    /// await or independently mutable owner between planning and this commit.
    fn commit(&mut self, event: &CodexEvent, state: &'static str) {
        if event.kind == "UserPromptSubmit" {
            self.seen.insert(event.turn.clone());
            self.current = Some(event.turn.clone());
        }
        self.state = Some(state);
    }
}

/// Selects only the original fixed local-command instance; rotating to another
/// observer permanently excludes stateless callbacks instead of handing off.
fn original(binding: &LaunchBinding) -> Result<&EnrollmentBinding> {
    binding
        .enrollment
        .as_ref()
        .filter(|source| {
            binding.harness == "codex"
                && !binding.retired
                && source.epoch == 1
                && source.instance == "codex-local-hooks-1"
                && source.curated_observer.is_some()
                && matches!(source.producer, ProducerEvidence::VerifiedParent(_))
        })
        .ok_or_else(|| MezError::forbidden("Codex original command instance unavailable"))
}

impl RuntimeSessionService {
    /// Captures one existing first-epoch binding using bounded indexed metadata,
    /// then delegates all native authorization/reservations to the existing helper
    /// pipeline. The private synthesized envelope never leaves daemon memory.
    pub(super) fn prepare_external_codex_observation(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
    ) -> Result<ExternalEnrollmentWork> {
        self.require_live()?;
        if connection.initialized() {
            return Err(MezError::forbidden(
                "Codex callback requires uninitialized Unix ingress",
            ));
        }
        let Some(AuthenticatedPeer::UnixUser { uid }) = connection.authenticated_peer() else {
            return Err(MezError::forbidden(
                "Codex callback requires native Unix ingress",
            ));
        };
        crate::control::validate_control_method_params_schema(request)?;
        if connection
            .unix_origin()
            .is_none_or(|origin| origin.uid() != *uid || !origin.writer_confirmed())
        {
            return Err(MezError::forbidden("Codex callback writer unavailable"));
        }
        let raw = request.params.as_deref().unwrap_or("{}");
        if raw.len() > 4096 {
            return Err(MezError::invalid_args(
                "Codex callback metadata exceeds limit",
            ));
        }
        let params = crate::protocol::strict_json::decode(raw.as_bytes())
            .map_err(|_| MezError::invalid_args("Codex callback requires unique fields"))?;
        let pane = text(&params, "pane_id", 64)?;
        let session = text(&params, "external_session_id", 128)?;
        let event = CodexEvent::parse(&params)?;
        let targets = self
            .control
            .external_agents()
            .enrollments
            .helper_targets
            .candidates(*uid, "codex", &session);
        for target in targets {
            self.reconcile_external_agent_registration(target);
        }
        let digest = self
            .control
            .external_agents()
            .enrollments
            .helper_targets
            .select(*uid, "codex", &session)?;
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .filter(|binding| binding.uid == *uid && binding.pane_id == pane)
            .ok_or_else(|| MezError::forbidden("Codex callback candidate unavailable"))?;
        let source = original(binding)?;
        let order_queue = source.codex_turns.order.clone();
        let callback = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":request.id,"method":"agent/external/helper-presentation","params":{
            "launch_token":source.token.expose_secret(),"generation":binding.generation,"external_session_id":session,"sequence":1,"state":"ready",
        }}).to_string())?;
        let mut work = self.prepare_external_helper_presentation(&callback, connection)?;
        let order = match order_queue.reserve() {
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
        // The request id is already encoded JSON, unlike the internal envelope's
        // string construction. Preserve the caller's original wire correlation.
        work.request_id = request.id.clone();
        if let Some(helper) = &mut work.helper {
            helper.codex = Some(event);
            helper.codex_order = Some(order);
        }
        Ok(work)
    }

    /// Applies a native-authorized original callback using daemon-owned sequence
    /// and bounded turn fences. Only SessionEnd retires the exact source; changed
    /// native facts stamp finite proof for existing daemon maintenance, never
    /// direct renewal/billing/continuous idle health. Stop is ready, not success.
    pub(super) fn commit_external_codex_observation(
        &mut self,
        digest: [u8; 32],
        session: &str,
        event: &CodexEvent,
    ) -> Result<String> {
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .ok_or_else(|| MezError::forbidden("Codex callback owner unavailable"))?;
        let source = original(binding)?;
        let previous = binding
            .registration
            .as_ref()
            .and_then(|registration| registration.presentation.as_ref())
            .map_or(0, |presentation| presentation.sequence);
        if event.kind == "SessionEnd" {
            self.retire_external_agent_binding(digest);
            return Ok(serde_json::json!({"retired":true,"sequence":previous}).to_string());
        }
        let Some(state) = source.codex_turns.state_for(event)? else {
            return Ok(serde_json::json!({"changed":false,"sequence":previous}).to_string());
        };
        let sequence = previous
            .checked_add(1)
            .filter(|n| *n <= 9_007_199_254_740_991)
            .ok_or_else(|| MezError::invalid_state("Codex presentation sequence exhausted"))?;
        let reply = self.update_external_presentation(
            digest,
            &serde_json::json!({"external_session_id":session,"sequence":sequence,"state":state}),
        )?;
        let source = self
            .control
            .external_agents_mut()
            .bindings
            .get_mut(&digest)
            .and_then(|binding| binding.enrollment.as_mut())
            .ok_or_else(|| MezError::invalid_state("Codex callback owner disappeared"))?;
        source.codex_turns.commit(event, state);
        // Only a changed fact reached this point after native/order settlement.
        // Replays and SessionEnd return above without extending observer proof.
        source
            .curated_observer
            .as_mut()
            .ok_or_else(|| MezError::invalid_state("Codex observer proof owner disappeared"))?
            .observe(sequence)?;
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exhaustion preserves every retired-turn fence and accepts only the last
    /// admitted turn's terminal facts. Unknown old stops and malformed selectors
    /// cannot bypass finite storage by evicting prior source state.
    #[test]
    fn external_codex_turn_fence_exhaustion_preserves_terminal_and_old_ids() {
        let mut owner = CodexTurns::default();
        for index in 0..MAX_TURNS {
            let event = CodexEvent {
                kind: "UserPromptSubmit",
                turn: format!("turn-{index}"),
            };
            assert_eq!(owner.state_for(&event).unwrap(), Some("running"));
            owner.commit(&event, "running");
        }
        assert!(
            owner
                .state_for(&CodexEvent {
                    kind: "UserPromptSubmit",
                    turn: "new".into()
                })
                .is_err()
        );
        assert!(
            owner
                .state_for(&CodexEvent {
                    kind: "Stop",
                    turn: "turn-0".into()
                })
                .is_err()
        );
        let stop = CodexEvent {
            kind: "Stop",
            turn: format!("turn-{}", MAX_TURNS - 1),
        };
        assert_eq!(owner.state_for(&stop).unwrap(), Some("ready"));
        owner.commit(&stop, "ready");
        assert_eq!(owner.state_for(&stop).unwrap(), None);
        assert_eq!(owner.seen.len(), MAX_TURNS);
        for (event, turn) in [("SessionEnd", "a"), ("Stop", "../private"), ("Stop", "")] {
            assert!(CodexEvent::parse(&serde_json::json!({"event":event,"turn_id":turn})).is_err());
        }
    }
}
