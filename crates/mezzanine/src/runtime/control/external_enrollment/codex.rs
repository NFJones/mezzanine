//! Bounded first-epoch Codex command-hook observations, not generic control.
//!
//! A unique indexed session is only a candidate. The existing helper worker and
//! actor settlement prove native writer/direct-parent/root/generation before any
//! effect. No supplied credential, generation, title or newest-epoch discovery
//! is admitted. Turn order is arrival-based; known retired turns stay fenced.
//! SessionEnd retires only that exact session. New native facts stamp finite
//! observer proof, not continuous idle health; end never attests creator death.
//! Child facts affect only their separate bounded request scope, never parent
//! presentation, lease, health or retirement.

use super::*;

/// Finite run-local turn history; exhaustion rejects without evicting old fences.
const MAX_TURNS: usize = 4096;

/// One code-owned coarse event and bounded opaque turn selector.
#[derive(Debug, Clone)]
pub(super) struct CodexEvent {
    kind: &'static str,
    /// Explicit child selector; it grants no association before native Start.
    child: Option<String>,
    turn: String,
    /// Normalized model-hook counts only; absent is unknown, never zero.
    counters: Option<crate::storage::token_usage::ExternalCounters>,
}

impl CodexEvent {
    /// Rejects unsupported semantics and content-shaped turn IDs before work.
    fn parse(params: &serde_json::Value) -> Result<Self> {
        let kind = match text(params, "event", 32)?.as_str() {
            "SubagentStart" => "SubagentStart",
            "SubagentStop" => "SubagentStop",
            "UserPromptSubmit" => "UserPromptSubmit",
            "Stop" => "Stop",
            "PermissionRequest" => "PermissionRequest",
            "PreToolUse" => "PreToolUse",
            "PostToolUse" => "PostToolUse",
            "Interrupt" => "Interrupt",
            "SessionEnd" => "SessionEnd",
            "PreModelRequest" => "PreModelRequest",
            "PostModelRequest" => "PostModelRequest",
            _ => return Err(MezError::invalid_args("Codex event unavailable")),
        };
        let child = if params.get("agent_id").is_some() {
            let id = text(params, "agent_id", 128)?;
            if !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
            {
                return Err(MezError::invalid_args("Codex child selector unavailable"));
            }
            Some(id)
        } else {
            None
        };
        if matches!(kind, "SubagentStart" | "SubagentStop") && child.is_none()
            || child.is_some()
                && !matches!(
                    kind,
                    "SubagentStart" | "SubagentStop" | "PreModelRequest" | "PostModelRequest"
                )
        {
            return Err(MezError::invalid_args("Codex child event unavailable"));
        }
        if kind != "PostModelRequest" && params.get("counters").is_some() {
            return Err(MezError::invalid_args(
                "Codex counters require a model post",
            ));
        }
        let counters = if kind == "PostModelRequest" {
            match params.get("counters") {
                Some(serde_json::Value::Null) => None,
                Some(value) => {
                    let counters: crate::storage::token_usage::ExternalCounters =
                        serde_json::from_value(value.clone())
                            .map_err(|_| MezError::invalid_args("Codex counters invalid"))?;
                    counters.validate()?;
                    Some(counters)
                }
                None => {
                    return Err(MezError::invalid_args(
                        "Codex model post counters unavailable",
                    ));
                }
            }
        } else {
            None
        };
        if kind == "SessionEnd" {
            if params.get("turn_id").is_some() {
                return Err(MezError::invalid_args(
                    "Codex session end cannot supply a turn selector",
                ));
            }
            return Ok(Self {
                kind,
                child: None,
                turn: String::new(),
                counters: None,
            });
        }
        let turn = text(params, "turn_id", 128)?;
        if !turn
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        {
            return Err(MezError::invalid_args("Codex turn selector unavailable"));
        }
        Ok(Self {
            kind,
            child,
            turn,
            counters,
        })
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
    /// Checks the original current main turn without selecting a child turn.
    pub(super) fn is_current(&self, turn: &str) -> bool {
        self.current.as_deref() == Some(turn)
    }

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
        let state = match event.kind {
            "Stop" => "ready",
            "PermissionRequest" => "approval-wait",
            "PreToolUse" | "PostToolUse" => "running",
            _ => "interrupted",
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
pub(super) fn original(binding: &LaunchBinding) -> Result<&EnrollmentBinding> {
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
        for target in targets.iter().copied() {
            self.reconcile_external_agent_registration(target);
        }
        // A dormant source and another native creator may share a public
        // session label. Kernel parent PID narrows candidates only; worker
        // reobservation still proves full parent incarnation and pane ancestry.
        let parent = connection
            .unix_origin()
            .ok_or_else(|| MezError::forbidden("Codex callback parent unavailable"))?
            .identity
            .parent_process_id;
        let mut matching = targets.into_iter().filter(|digest| {
            self.control
                .external_agents()
                .bindings
                .get(digest)
                .is_some_and(|binding| {
                    binding.uid == *uid
                        && binding.pane_id == pane
                        && original(binding)
                            .is_ok_and(|source| source.producer.identity().process_id == parent)
                })
        });
        let digest = matching.next().ok_or_else(|| {
            MezError::forbidden("Codex original native creator candidate unavailable")
        })?;
        if matching.next().is_some() {
            return Err(MezError::conflict(
                "Codex native creator candidate ambiguous",
            ));
        }
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .filter(|binding| binding.uid == *uid && binding.pane_id == pane)
            .ok_or_else(|| MezError::forbidden("Codex callback candidate unavailable"))?;
        let source = original(binding)?;
        let order_queue = source.codex_turns.order.clone();
        let plan = if matches!(event.kind, "PreModelRequest" | "PostModelRequest") {
            let writer = connection
                .unix_origin()
                .ok_or_else(|| MezError::forbidden("Codex request writer unavailable"))?
                .identity;
            Some(self.plan_codex_request(
                binding,
                event.kind,
                &event.turn,
                writer,
                event.counters,
                event.child.as_deref(),
            ))
        } else {
            None
        };

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
        // A model plan captures original request custody before reserving its
        // ticket. It cannot wait behind an older callback that may replace or
        // quarantine that captured state before the ledger worker executes.
        // Bounded helper retry may re-plan only after the predecessor settles.
        if plan.is_some() && !order.is_serving() {
            self.control
                .external_agents_mut()
                .enrollments
                .pending
                .remove(&work.admission);
            return Err(MezError::conflict("Codex model predecessor unsettled"));
        }
        // The request id is already encoded JSON, unlike the internal envelope's
        // string construction. Preserve the caller's original wire correlation.
        work.request_id = request.id.clone();
        if let Some(helper) = &mut work.helper {
            helper.codex = Some(event);
            helper.codex_order = Some(order);
        }
        if let Some(plan) = plan
            && let Err(error) = self.attach_codex_request(&mut work, digest, plan)
        {
            self.control
                .external_agents_mut()
                .enrollments
                .pending
                .remove(&work.admission);
            return Err(error);
        }
        Ok(work)
    }

    /// Applies a native-authorized original callback using daemon-owned sequence
    /// and bounded turn fences. Only SessionEnd retires the exact source; changed
    /// native facts stamp finite proof for existing daemon maintenance, never
    /// billing/continuous idle health. A changed native fact can recover an
    /// expired dormant source through its separate visibility owner. Stop is
    /// ready, not success.
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
        original(binding)?;
        if let Some(child) = &event.child {
            let source = self
                .control
                .external_agents_mut()
                .bindings
                .get_mut(&digest)
                .and_then(|binding| binding.enrollment.as_mut())
                .ok_or_else(|| MezError::forbidden("Codex original child owner unavailable"))?;
            let changed = source
                .codex_children
                .observe(event.kind, child, &event.turn)?;
            return Ok(serde_json::json!({"observed":true,"changed":changed}).to_string());
        }
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
        self.recover_codex_after_changed_fact(digest)?;
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
                child: None,
                kind: "UserPromptSubmit",
                turn: format!("turn-{index}"),
                counters: None,
            };
            assert_eq!(owner.state_for(&event).unwrap(), Some("running"));
            owner.commit(&event, "running");
        }
        assert!(
            owner
                .state_for(&CodexEvent {
                    child: None,
                    kind: "UserPromptSubmit",
                    turn: "new".into(),
                    counters: None
                })
                .is_err()
        );
        assert!(
            owner
                .state_for(&CodexEvent {
                    child: None,
                    kind: "Stop",
                    turn: "turn-0".into(),
                    counters: None
                })
                .is_err()
        );
        let stop = CodexEvent {
            child: None,
            kind: "Stop",
            turn: format!("turn-{}", MAX_TURNS - 1),
            counters: None,
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
