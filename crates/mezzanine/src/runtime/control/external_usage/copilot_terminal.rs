//! Sealed terminal results beside the shared accounting reservation owner.
//!
//! Native helpers must qualify the original concrete writer before execution.
//! Terminal arrival never creates a request-start marker or project attribution.
//! Actual storage publication can settle after retirement; only an unchanged
//! native callback gets an ACK. A cross-owner canonical receipt has no pane
//! checkpoint, so it consumes capacity without entering generic projection.

use super::*;
use crate::storage::token_usage::{CopilotTerminalCommit, copilot_terminal_call_id};

/// Private immutable terminal scope; no RPC accepts a UID or worker outcome.
#[derive(Debug, Clone)]
pub(super) struct TerminalScope {
    /// Authenticated binding UID; never selected by a wire parameter.
    uid: u32,
    /// Content-redacted canonical call selector, not reporting authority.
    call: String,
    /// Only the storage invocation can publish its immutable actual outcome.
    published: Arc<OnceLock<Result<CopilotTerminalCommit>>>,
}

impl ExternalUsageWork {
    /// Selects only the code-owned worker route, never a payload mode switch.
    pub(in crate::runtime::control) fn is_copilot_terminal(&self) -> bool {
        self.copilot_terminal.is_some()
    }

    /// Returns only the actual storage worker's first immutable publication.
    pub(in crate::runtime::control) fn terminal_result(&self) -> Result<CopilotTerminalCommit> {
        if self.report != self.admitted_report {
            return Err(MezError::conflict(
                "Copilot admitted terminal report changed",
            ));
        }
        self.copilot_terminal
            .as_ref()
            .and_then(|scope| scope.published.get())
            .ok_or_else(|| MezError::conflict("Copilot terminal result unpublished"))?
            .clone()
    }

    /// Executes only after the native helper's ancestry AND concrete-instance
    /// proof. Generic execution cannot substitute another expense path.
    pub(in crate::runtime::control) fn execute_copilot_terminal(&self) -> Result<()> {
        let scope = self
            .copilot_terminal
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("Copilot terminal scope unavailable"))?;
        if self.report != self.admitted_report
            || self.admission == 0
            || self.request_marker.is_some()
            || self.request_origin.is_some()
            || self.opencode_handoff.is_some()
            || self.socket_precommit_retry
        {
            return Err(MezError::conflict("Copilot terminal work changed"));
        }
        if scope.published.get().is_some() {
            return self.terminal_result().map(|_| ());
        }
        let result =
            self.store
                .ingest_copilot_terminal(&self.report, scope.uid, &scope.call, self.now);
        scope
            .published
            .set(result)
            .map_err(|_| MezError::conflict("Copilot terminal result already published"))?;
        self.terminal_result().map(|_| ())
    }
}

impl RuntimeSessionService {
    /// Freezes only native binding metadata and allowlisted terminal counters.
    /// Caller owns native helper admission and reservation; no I/O occurs here.
    pub(in crate::runtime::control) fn external_copilot_terminal_work(
        &self,
        request_id: String,
        binding: &super::super::external_agents::LaunchBinding,
        params: &serde_json::Value,
    ) -> Result<ExternalUsageWork> {
        let text = super::super::external_agents::text;
        let call = text(params, "call_id", 64)?;
        let event_id = copilot_terminal_call_id(binding.uid, &call)?;
        let counters: ExternalCounters = serde_json::from_value(
            params
                .get("counters")
                .cloned()
                .ok_or_else(|| MezError::invalid_args("Copilot counters unavailable"))?,
        )
        .map_err(|_| MezError::invalid_args("Copilot counters invalid"))?;
        counters.validate()?;
        if [
            Some(counters.input_tokens),
            Some(counters.output_tokens),
            counters.reasoning_tokens,
            counters.cached_input_tokens,
            counters.cache_write_input_tokens,
        ]
        .into_iter()
        .flatten()
        .any(|value| value > 9_007_199_254_740_991)
        {
            return Err(MezError::invalid_args(
                "Copilot counters exceed safe integers",
            ));
        }
        let now = current_unix_seconds();
        let mut report = ExternalUsageReport {
            owner: binding.accounting_owner.clone(),
            project: None,
            harness: "copilot".into(),
            epoch: "pending-terminal".into(),
            event_id,
            sequence: 1,
            mode: "observation".into(),
            baseline: false,
            observed_at: now,
            model: ModelTokenUsageKey::new("copilot", text(params, "model", 128)?),
            counters,
        };
        report.epoch = crate::storage::token_usage::external_observation_epoch(&report)?;
        Ok(ExternalUsageWork {
            admission: 0,
            accounting_incarnation: self.control.external_usage().incarnation.clone(),
            request_id,
            store: self
                .persistence
                .cloned_token_usage_store()
                .ok_or_else(|| MezError::invalid_state("Copilot durable storage unavailable"))?,
            admitted_report: report.clone(),
            report,
            pane_id: binding.pane_id.clone(),
            process: binding.process.clone(),
            projection_id: None,
            now,
            request_marker: None,
            request_origin: None,
            socket_precommit_retry: false,
            opencode_handoff: None,
            copilot_terminal: Some(TerminalScope {
                uid: binding.uid,
                call,
                published: Arc::new(OnceLock::new()),
            }),
            published: Arc::new(OnceLock::new()),
            #[cfg(test)]
            worker_gate: self.control.external_usage().worker_gate.clone(),
            #[cfg(test)]
            worker_probe: self.control.external_usage().worker_probe.clone(),
        })
    }

    /// Settles only actual terminal publication. Native validity controls
    /// diagnostics/ACK eligibility, never whether real expense may be erased.
    pub(in crate::runtime::control) fn complete_copilot_terminal_usage(
        &mut self,
        work: ExternalUsageWork,
        native_valid: bool,
    ) -> String {
        let outcome = (|| {
            if work.report != work.admitted_report {
                return Err(MezError::conflict("Copilot terminal report changed"));
            }
            let projection = self.control.external_usage();
            if !Arc::ptr_eq(&projection.incarnation, &work.accounting_incarnation)
                || !projection.pending.contains(&work.admission)
            {
                return Err(MezError::conflict("Copilot accounting reservation changed"));
            }
            work.terminal_result()
        })();
        let stale_reply = crate::runtime::runtime_json_rpc_error(
            &work.request_id,
            crate::error::MezErrorKind::Conflict,
            "Copilot native settlement unavailable",
        );
        let reply = match outcome {
            Ok(terminal) => {
                if native_valid {
                    self.note_copilot_terminal_report(&work);
                }
                if let Some(commit) = terminal.projection() {
                    self.complete_external_usage(work, Ok(commit.clone()))
                } else {
                    if let Err(error) = self.take_external_usage_reservation(&work) {
                        return crate::runtime::runtime_json_rpc_error(
                            &work.request_id,
                            error.kind(),
                            error.message(),
                        );
                    }
                    serde_json::json!({
                    "jsonrpc":"2.0",
                    "id":serde_json::from_str::<serde_json::Value>(&work.request_id).unwrap_or_default(),
                    "result":{"accepted":true,"durable":true,"applied":terminal.applied(),
                        "revision":terminal.revision(),
                        "coverage":{"source":"terminal-observation-external-reported",
                            "origin":"unattributed",
                            "reasoning_known":work.report.counters.reasoning_tokens.is_some()}}
                }).to_string()
                }
            }
            Err(error) => {
                self.release_unexecuted_external_usage(&work);
                crate::runtime::runtime_json_rpc_error(
                    &work.request_id,
                    error.kind(),
                    error.message(),
                )
            }
        };
        if native_valid { reply } else { stale_reply }
    }
}
