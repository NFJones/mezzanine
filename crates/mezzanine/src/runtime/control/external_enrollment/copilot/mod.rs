//! Classic Copilot command-hook observations under retained native creator proof.
//!
//! Session IDs and vendor milliseconds are inert selectors/order fences, never
//! provenance or wall-clock health. A unique original first-epoch binding and
//! the shared helper worker prove writer/direct-parent/root before settlement.
//! Shared ingress tickets serialize native work; no Codex turn semantics apply.
//! Equal replay is time-inert, conflicting ties/stale times fail closed, and only
//! the exact session end retires. Read-only extension attachment returns existing
//! public selectors after the same native proof; it cannot create a boundary,
//! change presentation, refresh health or allocate accounting authority.

use super::*;

/// Concrete native extension freshness, independent of classic presentation.
mod observer;
/// Coverage facts are separate from lease and presentation clocks.
mod usage;
pub(super) use observer::CopilotInstance;

/// One allowlisted coarse fact and its bounded vendor order selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CopilotEvent {
    kind: String,
    timestamp: u64,
}

/// Separate read-only attachment from timestamp-ordered classic lifecycle facts.
/// Only its distinct registered method can request public selector attachment.
#[derive(Debug, Clone)]
pub(super) enum CopilotObservation {
    /// Existing-session public selectors, without freshness or presentation.
    Attach,
    /// Positive freshness only for one original native extension lifetime.
    Pulse {
        /// Inert local instance discriminator, never native source authority.
        observer_id: String,
        /// Original instance counter; identical replay is time-inert.
        sequence: u64,
    },
    /// Accounting work is retained in the common helper reservation.
    Terminal { observer_id: String },
    /// Sticky incomplete coverage, never arbitrary vendor text.
    UsageGap {
        observer_id: String,
        reason: usage::UsageGap,
    },
    /// A strictly validated classic event with a positive vendor order fence.
    Event(CopilotEvent),
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
            "permission_prompt" => "approval-wait",
            "elicitation_dialog" => "input-wait",
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
    /// One bounded actual extension lifetime and its independent proof clock.
    observer: observer::CopilotObserver,
    /// An actual durable terminal receipt, not complete session coverage.
    terminal_reports_seen: bool,
    /// Finite sticky gap bits; a later success cannot hide missing calls.
    usage_gaps: u8,
}

impl CopilotEvents {
    /// Makes source retirement visible to queued native instance snapshots.
    pub(super) fn invalidate_observer(&self) {
        self.observer.invalidate();
    }
    /// Terminal arrival never masquerades as genuine request-bound accounting.
    pub(super) fn usage_coverage(&self) -> Option<&'static str> {
        if self.usage_gaps != 0 {
            Some("partial-terminal-observations-unattributed")
        } else if self.terminal_reports_seen {
            Some("terminal-observation-external-reported-unattributed")
        } else {
            None
        }
    }

    /// Fixture-only proof aging without manufacturing native ownership.
    #[cfg(test)]
    pub(super) fn age_extension_for_test(&mut self) {
        self.observer.age_for_test();
    }

    /// Reads actual pulse settlement for installed/replay assertions.
    #[cfg(test)]
    pub(super) fn extension_snapshot_for_test(&self) -> Option<(u64, Instant)> {
        self.observer.snapshot_for_test()
    }
    /// A real live extension and recent changed counter, not parent survival.
    pub(super) fn observer_recent(&self) -> bool {
        self.observer.is_recent()
    }

    /// Distinguishes an expired concrete proof from never having observed one.
    pub(super) fn observer_started(&self) -> bool {
        self.observer.has_started()
    }
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
    /// No credential or new enrollment is returned. Attachment reads existing
    /// public selectors only after common native worker and actor settlement.
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
        let event = match request.method.as_str() {
            "agent/external/copilot-attach" => CopilotObservation::Attach,
            "agent/external/copilot-pulse" => {
                let observer_id = text(&params, "observer_id", 64)?;
                if observer_id.len() != 64
                    || !observer_id
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                {
                    return Err(MezError::invalid_args(
                        "Copilot observer instance unavailable",
                    ));
                }
                let sequence = params["sequence"]
                    .as_u64()
                    .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
                    .ok_or_else(|| {
                        MezError::invalid_args("Copilot observer counter unavailable")
                    })?;
                CopilotObservation::Pulse {
                    observer_id,
                    sequence,
                }
            }
            "agent/external/copilot-terminal" => CopilotObservation::Terminal {
                observer_id: observer_nonce(&params)?,
            },
            "agent/external/copilot-usage-gap" => CopilotObservation::UsageGap {
                observer_id: observer_nonce(&params)?,
                reason: serde_json::from_value(params["reason"].clone())
                    .map_err(|_| MezError::invalid_args("Copilot usage gap unavailable"))?,
            },
            _ => CopilotObservation::Event(CopilotEvent::parse(&params)?),
        };
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
        if matches!(
            event,
            CopilotObservation::Pulse { .. }
                | CopilotObservation::Terminal { .. }
                | CopilotObservation::UsageGap { .. }
        ) && (params["generation"].as_u64() != Some(binding.generation)
            || text(&params, "observer_witness", 64)? != observer_witness(digest))
        {
            return Err(MezError::forbidden(
                "Copilot original observer selectors changed",
            ));
        }
        let instance = match &event {
            CopilotObservation::Terminal { observer_id }
            | CopilotObservation::UsageGap { observer_id, .. } => {
                Some(source.copilot_events.observer.capture(observer_id)?)
            }
            _ => None,
        };
        let terminal = if matches!(event, CopilotObservation::Terminal { .. }) {
            if !source.usage_owner_is_durable {
                return Err(MezError::forbidden("Copilot durable source unavailable"));
            }
            Some(self.external_copilot_terminal_work(request.id.clone(), binding, &params)?)
        } else {
            None
        };
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
            helper.copilot_instance = instance;
            helper.codex_order = Some(order);
        }
        if let Some(terminal) = terminal {
            match self.reserve_external_usage_work(terminal) {
                Ok(terminal) => {
                    if let Some(helper) = &mut work.helper {
                        helper.usage_end = Some(Box::new(terminal));
                    }
                }
                Err(error) => {
                    self.control
                        .external_agents_mut()
                        .enrollments
                        .pending
                        .remove(&work.admission);
                    return Err(error);
                }
            }
        }
        Ok(work)
    }

    /// Returns existing public selectors or publishes a native-qualified fact.
    /// Attachment and timestamp replay never refresh health; session end is not
    /// process death and cannot establish a replacement source.
    pub(super) fn commit_external_copilot_observation(
        &mut self,
        digest: [u8; 32],
        session: &str,
        observation: &CopilotObservation,
        origin: &Arc<UnixOriginProcess>,
        instance: Option<&CopilotInstance>,
    ) -> Result<String> {
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .ok_or_else(|| MezError::forbidden("Copilot callback owner unavailable"))?;
        let source = original(binding)?;
        let event = match observation {
            CopilotObservation::Attach => {
                return Ok(super::curated::public_response(binding, source, session));
            }
            CopilotObservation::Pulse {
                observer_id,
                sequence,
            } => {
                let source = self
                    .control
                    .external_agents_mut()
                    .bindings
                    .get_mut(&digest)
                    .and_then(|binding| binding.enrollment.as_mut())
                    .ok_or_else(|| MezError::forbidden("Copilot observer source unavailable"))?;
                return source
                    .copilot_events
                    .observer
                    .observe(origin, observer_id, *sequence);
            }
            CopilotObservation::Terminal { .. } | CopilotObservation::UsageGap { .. } => {
                if instance.is_none_or(|instance| !source.copilot_events.observer.matches(instance))
                {
                    return Err(MezError::forbidden("Copilot current observer changed"));
                }
                if let CopilotObservation::UsageGap { reason, .. } = observation {
                    let reason = *reason;
                    let owner = binding.accounting_owner.clone();
                    let source = self
                        .control
                        .external_agents_mut()
                        .bindings
                        .get_mut(&digest)
                        .and_then(|binding| binding.enrollment.as_mut())
                        .ok_or_else(|| MezError::forbidden("Copilot gap source unavailable"))?;
                    source.copilot_events.usage_gaps |= reason.bit();
                    self.refresh_external_source_diagnostics_for_owner(&owner);
                    return Ok(serde_json::json!({"observed":true,"reason":reason}).to_string());
                }
                return Ok("{}".into());
            }
            CopilotObservation::Event(event) => event,
        };
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

/// Only inert fixed-length instance labels can narrow the current native owner.
fn observer_nonce(params: &serde_json::Value) -> Result<String> {
    let nonce = text(params, "observer_id", 64)?;
    if nonce.len() != 64
        || !nonce
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(MezError::invalid_args(
            "Copilot observer instance unavailable",
        ));
    }
    Ok(nonce)
}

impl RuntimeSessionService {
    /// Records only actual durable terminal publication under valid settlement.
    pub(in crate::runtime::control) fn note_copilot_terminal_report(
        &mut self,
        work: &super::super::external_usage::ExternalUsageWork,
    ) {
        for binding in self.control.external_agents_mut().bindings.values_mut() {
            if !binding.retired
                && binding.harness == "copilot"
                && binding.accounting_owner == work.report.owner
                && binding.process.same_incarnation(&work.process)
                && let Some(source) = &mut binding.enrollment
                && source.epoch == 1
                && source.instance == "copilot-local-hooks-1"
            {
                source.copilot_events.terminal_reports_seen = true;
            }
        }
        self.refresh_external_source_diagnostics_for_owner(&work.report.owner);
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
            "approval-wait"
        );
    }
}
