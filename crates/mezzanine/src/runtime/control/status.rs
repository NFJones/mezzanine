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
    /// Ordered terminal snapshots and immediate outcomes, absent for slash input.
    terminal_work: Option<crate::runtime::commands_support::RuntimeTerminalStatusWork>,
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
        if !matches!(
            request.method.as_str(),
            "agent/shell/command" | "terminal/command"
        ) || !connection.initialized()
        {
            return None;
        }
        let input = super::runtime_json_string_field(request.params.as_deref()?, "input")?;
        let terminal = request.method == "terminal/command";
        let options = if terminal {
            let commands = mez_mux::command::parse_command_sequence(&input).ok()?;
            commands
                .iter()
                .filter(|command| command.name == "agent-status")
                .filter_map(|command| {
                    crate::runtime::commands_support::terminal_status_options(&command.args).ok()
                })
                .find(|options| {
                    options.extended || options.scope != mez_agent::slash::StatusScope::Overall
                })?
        } else {
            let slash = mez_agent::slash::parse_slash_command(&input).ok()??;
            if slash.name != "status" {
                return None;
            }
            mez_agent::slash::parse_status_options(&slash.args).ok()?
        };
        if !options.extended
            && (!terminal || options.scope == mez_agent::slash::StatusScope::Overall)
        {
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
            if terminal {
                self.control.idempotency_mut().reserve_request(
                    &cache_key,
                    &request.method,
                    &request.params,
                )?;
                let prepared = self
                    .prepare_terminal_status_work(client, &input)
                    .unwrap_or_else(|| {
                        Err(MezError::invalid_state(
                            "terminal status preparation unavailable",
                        ))
                    });
                let terminal_work = match prepared {
                    Ok(work) => work,
                    Err(error) => {
                        let response = crate::runtime::runtime_json_rpc_error(
                            &request.id,
                            error.kind(),
                            error.message(),
                        );
                        self.control.idempotency_mut().remember_response(
                            cache_key,
                            request.method.clone(),
                            request.params.clone(),
                            response,
                        );
                        return Err(error);
                    }
                };
                let report = terminal_work
                    .reports
                    .first()
                    .ok_or_else(|| MezError::invalid_state("terminal status snapshot unavailable"))?
                    .1
                    .clone();
                return Ok(StatusControlWork {
                    request: request.clone(),
                    report,
                    terminal_work: Some(terminal_work),
                    conversation: String::new(),
                    generation: 0,
                    input,
                    cache_key,
                    #[cfg(test)]
                    worker_gate: self.integration.deferred_agent_command_probe(),
                });
            }
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
                terminal_work: None,
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
        if let Some(terminal_work) = &work.terminal_work {
            let authorized =
                authorize_control_request(&self.session, &work.report.client, &work.request);
            let result = self.complete_terminal_status_work(terminal_work, authorized.and(result));
            let response = match result {
                Ok(result) => format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{result}}}",
                    work.request.id
                ),
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
            return response;
        }
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

impl StatusControlWork {
    /// Renders owned slash or terminal sequence input only outside actor ownership.
    pub(crate) fn render(&self) -> Result<String> {
        match &self.terminal_work {
            Some(work) => work.render(),
            None => self.report.render(),
        }
    }
}
