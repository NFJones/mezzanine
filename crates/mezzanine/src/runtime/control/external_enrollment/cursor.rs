//! Interactive Cursor native command callbacks with original-generation fences.
//!
//! CLI conversation/generation labels select an existing original first-epoch
//! source, never authority. Shared helper work independently qualifies writer,
//! retained direct creator/root and settlement deadline. No timestamp, usage,
//! transcript or IDE-only field is guessed. Native ingress order is held through
//! actor commit. Turn history is bounded, replays are time-inert, and old session
//! ends cannot adopt a rotated observer or affect another replacement session.

use super::*;

/// Finite generation history preserves old fences rather than evicting them.
const MAX_GENERATIONS: usize = 4096;

/// Separate Cursor semantics: generation and optional Stop status, not counters.
#[derive(Debug, Clone)]
pub(super) struct CursorEvent {
    kind: String,
    generation: String,
    state: &'static str,
}

impl CursorEvent {
    /// Parses exact code-owned event grammar without a caller-selected state.
    fn parse(params: &serde_json::Value) -> Result<Self> {
        let kind = text(params, "event", 32)?;
        if !matches!(
            kind.as_str(),
            "beforeSubmitPrompt"
                | "postToolUse"
                | "preCompact"
                | "afterAgentResponse"
                | "stop"
                | "sessionEnd"
        ) {
            return Err(MezError::invalid_args("Cursor event unavailable"));
        }
        if kind == "sessionEnd" {
            if params.get("generation_id").is_some() || params.get("status").is_some() {
                return Err(MezError::invalid_args(
                    "Cursor end cannot select a generation",
                ));
            }
            return Ok(Self {
                kind,
                generation: String::new(),
                state: "ready",
            });
        }
        let generation = text(params, "generation_id", 128)?;
        if !generation
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
            || params["external_session_id"].as_str() == Some(&generation)
        {
            return Err(MezError::invalid_args("Cursor generation unavailable"));
        }
        let state = if kind == "stop" {
            match text(params, "status", 32)?.as_str() {
                "completed" => "ready",
                "aborted" => "interrupted",
                "error" => "failed",
                _ => return Err(MezError::invalid_args("Cursor Stop status unavailable")),
            }
        } else {
            if params.get("status").is_some() {
                return Err(MezError::invalid_args(
                    "Cursor activity cannot select status",
                ));
            }
            "running"
        };
        Ok(Self {
            kind,
            generation,
            state,
        })
    }
}

/// Run-local generation fence and shared native callback order, not idle health.
#[derive(Debug, Default)]
pub(super) struct CursorGenerations {
    seen: BTreeSet<String>,
    current: Option<String>,
    state: Option<&'static str>,
    order: Arc<super::codex_order::CodexOrder>,
}

impl CursorGenerations {
    /// Only a prompt starts a distinct generation. Missing starts are observable
    /// loss, not implicit enrollment or a newest-generation fallback. Once a
    /// terminal boundary arrives, delayed activity cannot repaint it as running.
    fn changed(&self, event: &CursorEvent) -> Result<bool> {
        if event.kind == "beforeSubmitPrompt" {
            if self.current.as_deref() == Some(&event.generation) {
                return Ok(false);
            }
            if self.seen.contains(&event.generation) {
                return Err(MezError::conflict("Cursor generation retired"));
            }
            if self.seen.len() >= MAX_GENERATIONS {
                return Err(MezError::new(
                    crate::error::MezErrorKind::RateLimited,
                    "Cursor generation fence capacity unavailable",
                ));
            }
            return Ok(true);
        }
        if self.current.as_deref() != Some(&event.generation) {
            return Err(MezError::conflict("Cursor current generation unavailable"));
        }
        if matches!(self.state, Some("ready" | "interrupted" | "failed")) {
            return Ok(false);
        }
        Ok(self.state != Some(event.state))
    }
    /// Commits only after shared presentation succeeds with no intervening await.
    fn commit(&mut self, event: &CursorEvent) {
        if event.kind == "beforeSubmitPrompt" {
            self.seen.insert(event.generation.clone());
            self.current = Some(event.generation.clone());
        }
        self.state = Some(event.state);
    }
}

/// Fixed original instance only; stateless hooks never discover a successor.
fn original(binding: &LaunchBinding) -> Result<&EnrollmentBinding> {
    binding
        .enrollment
        .as_ref()
        .filter(|source| {
            binding.harness == "cursor"
                && !binding.retired
                && source.epoch == 1
                && source.instance == "cursor-local-hooks-1"
                && source.curated_observer.is_some()
                && matches!(source.producer, ProducerEvidence::VerifiedParent(_))
        })
        .ok_or_else(|| MezError::forbidden("Cursor original command instance unavailable"))
}

impl RuntimeSessionService {
    /// Selects one indexed source then delegates all native proof to common work.
    /// Synthesized private credentials never leave daemon memory or grant roles.
    pub(super) fn prepare_external_cursor_observation(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
    ) -> Result<ExternalEnrollmentWork> {
        self.require_live()?;
        if connection.initialized() {
            return Err(MezError::forbidden(
                "Cursor callback requires uninitialized ingress",
            ));
        }
        let Some(AuthenticatedPeer::UnixUser { uid }) = connection.authenticated_peer() else {
            return Err(MezError::forbidden(
                "Cursor callback requires native Unix ingress",
            ));
        };
        crate::control::validate_control_method_params_schema(request)?;
        if connection
            .unix_origin()
            .is_none_or(|origin| origin.uid() != *uid || !origin.writer_confirmed())
        {
            return Err(MezError::forbidden("Cursor callback writer unavailable"));
        }
        let raw = request.params.as_deref().unwrap_or("{}");
        if raw.len() > 4096 {
            return Err(MezError::invalid_args(
                "Cursor callback metadata exceeds limit",
            ));
        }
        let params = crate::protocol::strict_json::decode(raw.as_bytes())
            .map_err(|_| MezError::invalid_args("Cursor callback requires unique fields"))?;
        let pane = text(&params, "pane_id", 64)?;
        let session = text(&params, "external_session_id", 128)?;
        let event = CursorEvent::parse(&params)?;
        let targets = self
            .control
            .external_agents()
            .enrollments
            .helper_targets
            .candidates(*uid, "cursor", &session);
        for target in targets {
            self.reconcile_external_agent_registration(target);
        }
        let digest = self
            .control
            .external_agents()
            .enrollments
            .helper_targets
            .select(*uid, "cursor", &session)?;
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .filter(|binding| binding.uid == *uid && binding.pane_id == pane)
            .ok_or_else(|| MezError::forbidden("Cursor callback candidate unavailable"))?;
        let source = original(binding)?;
        let queue = source.cursor_generations.order.clone();
        let callback = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"cursor-internal","method":"agent/external/helper-presentation","params":{"launch_token":source.token.expose_secret(),"generation":binding.generation,"external_session_id":session,"sequence":1,"state":"ready"}}).to_string())?;
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
            helper.cursor = Some(event);
            helper.codex_order = Some(order);
        }
        Ok(work)
    }

    /// Applies a new generation fact or exact end after common native settlement.
    /// Stop is ready/interrupted/failed, not success/billing/creator death. Replays
    /// and stable activity do not extend proof; silence cannot auto-renew leases.
    pub(super) fn commit_external_cursor_observation(
        &mut self,
        digest: [u8; 32],
        session: &str,
        event: &CursorEvent,
    ) -> Result<String> {
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .ok_or_else(|| MezError::forbidden("Cursor callback owner unavailable"))?;
        let source = original(binding)?;
        let previous = binding
            .registration
            .as_ref()
            .and_then(|registration| registration.presentation.as_ref())
            .map_or(0, |presentation| presentation.sequence);
        if event.kind == "sessionEnd" {
            self.retire_external_agent_binding(digest);
            return Ok(serde_json::json!({"retired":true,"sequence":previous}).to_string());
        }
        if !source.cursor_generations.changed(event)? {
            return Ok(serde_json::json!({"changed":false,"sequence":previous}).to_string());
        }
        let sequence = previous
            .checked_add(1)
            .filter(|n| *n <= 9_007_199_254_740_991)
            .ok_or_else(|| MezError::invalid_state("Cursor sequence exhausted"))?;
        let reply = self.update_external_presentation(digest,&serde_json::json!({"external_session_id":session,"sequence":sequence,"state":event.state}))?;
        let source = self
            .control
            .external_agents_mut()
            .bindings
            .get_mut(&digest)
            .and_then(|binding| binding.enrollment.as_mut())
            .ok_or_else(|| MezError::invalid_state("Cursor source disappeared"))?;
        source.cursor_generations.commit(event);
        source
            .curated_observer
            .as_mut()
            .ok_or_else(|| MezError::invalid_state("Cursor observer unavailable"))?
            .observe(sequence)?;
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Storage exhaustion never evicts old generation fences, and terminal stops
    /// remain deliverable for the last admitted generation. Malformed/fallback
    /// IDs and caller-selected end statuses cannot manufacture a transition.
    #[test]
    fn cursor_generation_history_is_bounded_without_old_id_eviction() {
        let mut owner = CursorGenerations::default();
        for index in 0..MAX_GENERATIONS {
            let event = CursorEvent::parse(&serde_json::json!({"external_session_id":"session","event":"beforeSubmitPrompt","generation_id":format!("turn-{index}")})).unwrap();
            assert!(owner.changed(&event).unwrap());
            owner.commit(&event);
        }
        let prompt = CursorEvent::parse(&serde_json::json!({"external_session_id":"session","event":"beforeSubmitPrompt","generation_id":"new"})).unwrap();
        assert!(owner.changed(&prompt).is_err());
        let stop = CursorEvent::parse(&serde_json::json!({"external_session_id":"session","event":"stop","generation_id":format!("turn-{}",MAX_GENERATIONS-1),"status":"completed"})).unwrap();
        assert!(owner.changed(&stop).unwrap());
        owner.commit(&stop);
        assert!(!owner.changed(&stop).unwrap());
        for params in [
            serde_json::json!({"event":"stop","generation_id":"../private","status":"error"}),
            serde_json::json!({"event":"beforeSubmitPrompt","generation_id":"s","external_session_id":"s"}),
            serde_json::json!({"event":"sessionEnd","status":"completed"}),
        ] {
            assert!(CursorEvent::parse(&params).is_err());
        }
    }
}
