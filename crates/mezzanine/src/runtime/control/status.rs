//! Authorized control status snapshots and exact-owned history settlement.
//!
//! The actor captures immutable report inputs and a bounded command lease. The
//! worker only reads captured storage. Completion revalidates client, command,
//! pane and project, caches the reply, and never installs an interactive overlay.

use crate::control::{
    ControlConnectionState, JsonRpcRequest, authorize_control_request,
    validate_control_method_params_schema,
};
use crate::error::{MezError, Result};
use crate::runtime::{RuntimeAgentCommandLifecyclePhase, RuntimeSessionService};

/// One authorized report and its existing pane-command lease.
#[derive(Debug, Clone)]
pub(crate) struct StatusControlWork {
    /// Original RPC identity and replay parameters.
    pub(crate) request: JsonRpcRequest,
    /// Acceptance-owned counters, scope and delivery evidence.
    pub(crate) report: crate::runtime::commands::RuntimeStatusReportWork,
    /// Exact conversation owning the command lease.
    conversation: String,
    /// Exact pane command generation consumed on settlement.
    generation: u64,
    /// Original slash input for transport-compatible response encoding.
    input: String,
    /// Existing client-scoped replay namespace.
    cache_key: String,
    /// Test-only off-actor query barrier.
    #[cfg(test)]
    pub(crate) worker_gate: (
        Option<std::sync::Arc<tokio::sync::Notify>>,
        Option<std::sync::Arc<tokio::sync::Notify>>,
    ),
}

impl RuntimeSessionService {
    /// Captures only extended status; other requests keep ordinary ingress.
    pub(crate) fn prepare_status_control_work(
        &mut self,
        body: &str,
        connection: &ControlConnectionState,
    ) -> Option<std::result::Result<StatusControlWork, String>> {
        let request = crate::control::parse_json_rpc_request(body).ok()?;
        if request.method != "agent/shell/command" || !connection.initialized() {
            return None;
        }
        let input = super::runtime_json_string_field(request.params.as_deref()?, "input")?;
        let slash = mez_agent::slash::parse_slash_command(&input).ok()??;
        if slash.name != "status" {
            return None;
        }
        let options = mez_agent::slash::parse_status_options(&slash.args).ok()?;
        if !options.extended {
            return None;
        }
        let authorized = (|| -> Result<()> {
            self.require_live()?;
            let client = connection.caller_client_id().ok_or_else(|| {
                MezError::forbidden("control connection has no authenticated client")
            })?;
            authorize_control_request(&self.session, client, &request)?;
            validate_control_method_params_schema(&request)?;
            Ok(())
        })();
        if let Err(error) = authorized {
            return Some(Err(crate::runtime::runtime_json_rpc_error(
                &request.id,
                error.kind(),
                error.message(),
            )));
        }
        // Authorization precedes replay, and replay precedes all lease mutation.
        if let Some(client) = connection.caller_client_id()
            && let Some(key) = super::runtime_json_string_field(
                request.params.as_deref().unwrap_or("{}"),
                "idempotency_key",
            )
        {
            match self.control.idempotency_mut().cached_response(
                &format!("{client}:{key}"),
                &request.method,
                &request.params,
            ) {
                Ok(Some(cached)) => return Some(Err(cached)),
                Err(error) => {
                    return Some(Err(crate::runtime::runtime_json_rpc_error(
                        &request.id,
                        error.kind(),
                        error.message(),
                    )));
                }
                Ok(None) => {}
            }
        }
        let prepared = (|| -> Result<StatusControlWork> {
            self.require_live()?;
            let client = connection.caller_client_id().ok_or_else(|| {
                MezError::forbidden("control connection has no authenticated client")
            })?;
            authorize_control_request(&self.session, client, &request)?;
            validate_control_method_params_schema(&request)?;
            let key = super::runtime_json_string_field(
                request.params.as_deref().unwrap_or("{}"),
                "idempotency_key",
            )
            .ok_or_else(|| {
                MezError::invalid_args("mutating control request requires idempotency_key")
            })?;
            let cache_key = format!("{client}:{key}");
            let pane = self.session.active_pane_for(client)?.id.to_string();
            let report = self.prepare_status_report(client, &pane, options)?;
            let conversation = report.command_owner();
            let generation = self.begin_agent_command_claim(&pane, &conversation)?;
            if !self
                .agent
                .claim_agent_command(&pane, &conversation, generation)
            {
                self.agent
                    .cancel_matching_agent_command(&pane, &conversation, generation);
                return Err(MezError::conflict("status command claim unavailable"));
            }
            Ok(StatusControlWork {
                request: request.clone(),
                report,
                conversation,
                generation,
                input,
                cache_key,
                #[cfg(test)]
                worker_gate: self.integration.deferred_agent_command_probe(),
            })
        })();
        Some(prepared.map_err(|error| {
            crate::runtime::runtime_json_rpc_error(&request.id, error.kind(), error.message())
        }))
    }

    /// Settles one report reply without publishing into client presentation state.
    pub(crate) fn complete_status_control_work(
        &mut self,
        work: StatusControlWork,
        result: Result<String>,
    ) -> String {
        let current = self.status_report_is_current(&work.report)
            && authorize_control_request(&self.session, &work.report.client, &work.request).is_ok()
            && self.agent.agent_command_is_claimed(
                &work.report.pane,
                &work.conversation,
                work.generation,
            );
        let result = if current {
            result
        } else {
            Err(MezError::conflict(
                "status report owner or active project changed",
            ))
        };
        let phase = if result.is_ok() {
            RuntimeAgentCommandLifecyclePhase::Completed
        } else {
            RuntimeAgentCommandLifecyclePhase::Failed
        };
        self.agent.settle_agent_command(
            &work.report.pane,
            &work.conversation,
            work.generation,
            phase,
        );
        let response = match result {
            Ok(body) => {
                let outcome =
                    crate::integrations::agent::slash::AgentShellCommandOutcome::Display {
                        command: "status".into(),
                        body,
                    };
                let result = crate::runtime::runtime_agent_shell_command_response_json(
                    &work.report.pane,
                    &work.input,
                    Some(&outcome),
                );
                format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{result}}}",
                    work.request.id
                )
            }
            Err(error) => crate::runtime::runtime_json_rpc_error(
                &work.request.id,
                error.kind(),
                error.message(),
            ),
        };
        self.control.idempotency_mut().remember_response(
            work.cache_key,
            work.request.method,
            work.request.params,
            response.clone(),
        );
        response
    }
}
