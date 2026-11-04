//! Typed terminal sequences suspended around actor-owned live layout restore.
//!
//! Trusted custody setup enables one original command owner. A sequence that
//! contains load-layout retains its parsed AST before any command runs; the
//! actor advances it in order, performs repository/status I/O off actor, and
//! uses the existing snapshot control drain before later commands or attached
//! actions. Saved bytes never construct this owner or confer message authority.

use super::*;
use crate::host::terminal::TerminalClientLoopAction;
use crate::runtime::RuntimeTerminalStatusWork;
use crate::storage::snapshot::SnapshotRepository;
use crate::ui::command::LayoutLoadSelector;
use mez_core::ids::ClientId;
use std::{collections::VecDeque, sync::Arc};

/// One private service slot; the actor owns the accepted sequence after capture.
#[derive(Default)]
pub(crate) struct RuntimeTerminalLayoutState {
    enabled: bool,
    owner: Option<Arc<()>>,
    pending: Option<Box<RuntimeTerminalSequence>>,
}
impl std::fmt::Debug for RuntimeTerminalLayoutState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeTerminalLayoutState")
            .field("enabled", &self.enabled)
            .field("owned", &self.owner.is_some())
            .finish_non_exhaustive()
    }
}

/// Original typed commands and outcomes, never an independently replayable plan.
pub(crate) struct RuntimeTerminalSequence {
    pub(crate) client: ClientId,
    identity: Arc<()>,
    commands: VecDeque<CommandInvocation>,
    outcomes: Vec<CommandOutcome>,
    interactive: bool,
    pub(crate) following_actions: Vec<TerminalClientLoopAction>,
    browser: Option<RuntimeAgentBrowserSnapshot>,
    pub(crate) control: Option<RuntimeTerminalControlRequest>,
    pub(crate) provisional_mux_actions: usize,
    diagnostic: Option<(&'static str, String)>,
}

/// Original authenticated RPC idempotency reservation, not a saved capability.
pub(crate) struct RuntimeTerminalControlRequest {
    key: String,
    request: crate::control::JsonRpcRequest,
}
impl RuntimeTerminalControlRequest {
    /// Identifies a terminal-step reply whose view must be rebuilt after restore.
    pub(crate) fn is_step(&self) -> bool {
        self.request.method == "terminal/step"
    }
}

/// The next actual I/O boundary selected at its original sequence position.
#[derive(Clone)]
pub(crate) enum RuntimeTerminalSequenceWork {
    Load {
        snapshots: SnapshotRepository,
        selector: LayoutLoadSelector,
        request_key: String,
    },
    Status(Box<RuntimeTerminalStatusWork>),
}
impl RuntimeTerminalSequenceWork {
    /// Requires the actual framed snapshot success from the actor control route.
    pub(crate) fn validate_load_response(output: &[u8]) -> Result<()> {
        let (body, consumed) = crate::control::decode_control_frame(output, 1024 * 1024)?;
        if consumed != output.len() {
            return Err(MezError::invalid_state(
                "layout restore returned multiple responses",
            ));
        }
        require_snapshot_control_success(&body)?;
        let value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|_| MezError::invalid_state("layout restore response malformed"))?;
        if value
            .pointer("/result/resumed")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            return Err(MezError::invalid_state("layout restore was not applied"));
        }
        Ok(())
    }

    /// Resolves only the captured repository/selector, never live focus or paths.
    pub(crate) fn snapshot_id(&self) -> Result<String> {
        match self {
            Self::Load {
                snapshots,
                selector,
                ..
            } => super::layout::runtime_layout_id_for_selector(snapshots, selector),
            Self::Status(_) => Err(MezError::invalid_state("status is not a layout selector")),
        }
    }
}

/// Validates an actual JSON-RPC snapshot response instead of reporting a failed
/// restore as a successful Noop. Data remains descriptive, never a commit proof.
pub(in crate::runtime) fn require_snapshot_control_success(response: &str) -> Result<()> {
    let value: serde_json::Value = serde_json::from_str(response).map_err(|error| {
        MezError::invalid_state(format!("invalid snapshot control response: {error}"))
    })?;
    if let Some(error) = value.get("error") {
        let kind = match error
            .pointer("/data/mezzanine_code")
            .and_then(serde_json::Value::as_str)
        {
            Some("invalid_params") => crate::error::MezErrorKind::InvalidArgs,
            Some("conflict") => crate::error::MezErrorKind::Conflict,
            Some("not_found") => crate::error::MezErrorKind::NotFound,
            Some("forbidden") => crate::error::MezErrorKind::Forbidden,
            Some("rate_limited") => crate::error::MezErrorKind::RateLimited,
            Some("method_not_found") => crate::error::MezErrorKind::NotImplemented,
            Some("internal_error") => crate::error::MezErrorKind::Io,
            _ => crate::error::MezErrorKind::InvalidState,
        };
        return Err(MezError::new(
            kind,
            error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("snapshot operation failed"),
        ));
    }
    if value.get("result").is_none() {
        return Err(MezError::invalid_state("snapshot control result missing"));
    }
    Ok(())
}

impl RuntimeSessionService {
    /// Enables suspension only for an internally registered actor custody root.
    pub(crate) fn enable_terminal_layout_sequences(&mut self) {
        self.agent.terminal_layout.enabled = true;
    }

    /// Recognizes an asynchronous layout sequence without selecting or executing it.
    pub(in crate::runtime) fn terminal_layout_sequence_requested(&self, input: &str) -> bool {
        self.agent.terminal_layout.enabled
            && parse_command_sequence(input)
                .is_ok_and(|commands| commands.iter().any(|command| command.name == "load-layout"))
    }

    /// Retains the complete accepted AST before any prefix mutation or I/O.
    pub(in crate::runtime) fn defer_terminal_layout_sequence(
        &mut self,
        client: &ClientId,
        input: &str,
    ) -> Result<bool> {
        if !self.agent.terminal_layout.enabled {
            return Ok(false);
        }
        if self.agent.terminal_layout.owner.is_some() {
            return Err(MezError::conflict(
                "terminal layout sequence already pending",
            ));
        }
        let commands = parse_command_sequence(input)?;
        if !commands.iter().any(|command| command.name == "load-layout") {
            return Ok(false);
        }
        self.require_live()?;
        if !self.session.is_attached_primary(client) {
            return Err(MezError::forbidden("layout load requires attached primary"));
        }
        let identity = Arc::new(());
        self.agent.terminal_layout.owner = Some(identity.clone());
        self.agent.terminal_layout.pending = Some(Box::new(RuntimeTerminalSequence {
            client: client.clone(),
            identity,
            commands: commands.into(),
            outcomes: Vec::new(),
            interactive: false,
            following_actions: Vec::new(),
            browser: None,
            control: None,
            provisional_mux_actions: 0,
            diagnostic: None,
        }));
        Ok(true)
    }

    /// Exposes only whether another original terminal sequence owns this slot.
    pub(in crate::runtime) fn terminal_layout_sequence_active(&self) -> bool {
        self.agent.terminal_layout.owner.is_some()
    }

    /// Reserves the actual authenticated RPC key before any deferred AST runs.
    pub(in crate::runtime) fn reserve_terminal_layout_control_request(
        &mut self,
        key: &str,
        request: &crate::control::JsonRpcRequest,
    ) -> Result<bool> {
        if self.agent.terminal_layout.pending.is_none() {
            return Ok(false);
        }
        if let Err(error) =
            self.control
                .idempotency_mut()
                .reserve_request(key, &request.method, &request.params)
        {
            self.agent.terminal_layout.pending = None;
            self.agent.terminal_layout.owner = None;
            return Err(error);
        }
        if let Some(sequence) = self.agent.terminal_layout.pending.as_mut() {
            sequence.control = Some(RuntimeTerminalControlRequest {
                key: key.to_owned(),
                request: request.clone(),
            });
        }
        Ok(true)
    }

    /// Publishes the final original response and releases its pending cache slot.
    pub(crate) fn remember_terminal_layout_control_result(
        &mut self,
        original: RuntimeTerminalControlRequest,
        result: Result<String>,
    ) {
        let response = result.unwrap_or_else(|error| {
            crate::runtime::runtime_json_rpc_error(
                &original.request.id,
                error.kind(),
                error.message(),
            )
        });
        self.control.idempotency_mut().remember_response(
            original.key,
            original.request.method,
            original.request.params,
            response,
        );
    }

    /// Rebuilds a control terminal-step view with empty input, retaining original
    /// consumed-byte and applied-action counts. It never resubmits the command.
    pub(crate) fn refresh_terminal_layout_step_result(
        &mut self,
        client: &ClientId,
        original: &RuntimeTerminalControlRequest,
        prefix: &serde_json::Value,
        tail: Option<&crate::runtime::AttachedClientStepApplication>,
        provisional_mux_actions: usize,
        failed: bool,
    ) -> Result<String> {
        let mut params: serde_json::Value =
            serde_json::from_str(original.request.params.as_deref().unwrap_or("{}"))
                .map_err(|_| MezError::invalid_state("terminal step parameters changed"))?;
        params["input_bytes"] = serde_json::json!([]);
        if let Some(params) = params.as_object_mut() {
            params.remove("client_size");
        }
        let body = self.dispatch_runtime_terminal_step(client, &params.to_string())?;
        let mut refreshed: serde_json::Value = serde_json::from_str(&body)
            .map_err(|_| MezError::invalid_state("terminal step result malformed"))?;
        refreshed["input_bytes"] = prefix["input_bytes"].clone();
        for key in [
            "forwarded_bytes",
            "mux_actions_applied",
            "mouse_actions_reported",
            "agent_prompt_inputs_applied",
        ] {
            let mut count = prefix["application"][key].as_u64().unwrap_or(0);
            if failed && key == "mux_actions_applied" {
                count = count.saturating_sub(provisional_mux_actions as u64);
            }
            let added = tail.map_or(0, |tail| match key {
                "forwarded_bytes" => tail.forwarded_bytes,
                "mux_actions_applied" => tail.mux_actions_applied,
                "mouse_actions_reported" => tail.mouse_actions_reported,
                _ => tail.agent_prompt_inputs_applied,
            });
            refreshed["application"][key] = serde_json::json!(count.saturating_add(added as u64));
        }
        for key in ["view_refresh_required", "full_redraw_required"] {
            refreshed["application"][key] = serde_json::json!(true);
        }
        let mut unsupported = prefix["application"]["unsupported_actions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(tail) = tail {
            unsupported.extend(
                tail.unsupported_actions
                    .iter()
                    .map(|value| serde_json::json!(value)),
            );
        }
        refreshed["application"]["unsupported_actions"] = serde_json::json!(unsupported);
        Ok(refreshed.to_string())
    }

    /// Records an attached action credit that must be removed on deferred failure.
    pub(in crate::runtime) fn mark_terminal_layout_mux_action(&mut self, command: &str) -> bool {
        if let Some(sequence) = self.agent.terminal_layout.pending.as_mut() {
            sequence.provisional_mux_actions = sequence.provisional_mux_actions.saturating_add(1);
            sequence.diagnostic = Some(("key_binding_command", command.to_owned()));
            true
        } else {
            false
        }
    }

    /// Delays an attached command's output diagnostic until actual completion.
    pub(in crate::runtime) fn defer_terminal_layout_display_diagnostic(
        &mut self,
        command: &str,
    ) -> bool {
        if let Some(sequence) = self.agent.terminal_layout.pending.as_mut() {
            sequence.diagnostic = Some(("attached_display_command", command.to_owned()));
            true
        } else {
            false
        }
    }

    /// Marks only the original captured sequence for interactive presentation.
    pub(in crate::runtime) fn mark_terminal_layout_interactive(&mut self) -> bool {
        if let Some(sequence) = self.agent.terminal_layout.pending.as_mut() {
            sequence.interactive = true;
            true
        } else {
            false
        }
    }

    /// Retains unexecuted actions after the command's exact attached-step boundary.
    pub(in crate::runtime) fn defer_terminal_layout_actions(
        &mut self,
        actions: &[TerminalClientLoopAction],
    ) -> bool {
        if let Some(sequence) = self.agent.terminal_layout.pending.as_mut() {
            sequence.following_actions.extend_from_slice(actions);
            true
        } else {
            false
        }
    }

    /// Transfers the single original plan to its actor without ending its lease.
    pub(crate) fn take_terminal_layout_sequence(&mut self) -> Option<Box<RuntimeTerminalSequence>> {
        self.agent.terminal_layout.pending.take()
    }

    /// Executes immediate AST commands until the next genuine I/O boundary.
    pub(crate) fn advance_terminal_layout_sequence(
        &mut self,
        sequence: &mut RuntimeTerminalSequence,
    ) -> Result<Option<RuntimeTerminalSequenceWork>> {
        self.validate_terminal_layout_sequence(sequence)?;
        while let Some(command) = sequence.commands.pop_front() {
            sequence.browser = None;
            if command.name == "load-layout" {
                let Some(snapshots) = self.persistence.cloned_snapshot_repository() else {
                    return Err(MezError::invalid_state(
                        "layout snapshot repository is not configured",
                    ));
                };
                let mut candidate = self.session.clone();
                let CommandOutcome::LayoutLoad { selector, .. } =
                    execute_command(&mut candidate, &sequence.client, &command)?
                else {
                    return Err(MezError::invalid_state(
                        "layout selector command changed kind",
                    ));
                };
                return Ok(Some(RuntimeTerminalSequenceWork::Load {
                    snapshots,
                    selector,
                    request_key: format!("terminal-layout:{:032x}", rand::random::<u128>()),
                }));
            }
            if let Some(status) =
                self.prepare_terminal_status_commands(&sequence.client, vec![command.clone()])
            {
                return status
                    .map(|work| Some(RuntimeTerminalSequenceWork::Status(Box::new(work))));
            }
            sequence
                .outcomes
                .push(execute_runtime_planned_terminal_command_with_browser(
                    self,
                    &mut sequence.client,
                    &command,
                    &mut sequence.browser,
                )?);
        }
        Ok(None)
    }

    /// Applies only the completed selected I/O stage to its original sequence.
    pub(crate) fn complete_terminal_layout_work(
        &mut self,
        sequence: &mut RuntimeTerminalSequence,
        work: &RuntimeTerminalSequenceWork,
        status: Option<Result<Vec<CommandOutcome>>>,
        load: Result<()>,
    ) -> Result<()> {
        match work {
            RuntimeTerminalSequenceWork::Load { .. } => {
                load?;
                sequence.outcomes.push(CommandOutcome::Noop {
                    command: "load-layout".into(),
                });
            }
            RuntimeTerminalSequenceWork::Status(work) => {
                sequence
                    .outcomes
                    .extend(self.complete_terminal_status_outcomes(
                        work,
                        status.ok_or_else(|| MezError::invalid_state("status result missing"))?,
                    )?);
            }
        }
        self.validate_terminal_layout_sequence(sequence)
    }

    /// Ends exactly this original lease before publishing text or attached UI.
    pub(crate) fn finish_terminal_layout_sequence(
        &mut self,
        mut sequence: Box<RuntimeTerminalSequence>,
        result: Result<()>,
    ) -> Result<(String, Vec<TerminalClientLoopAction>)> {
        // Release only the exact private lease even when the live client has
        // detached; a rejected continuation must not pin the global slot.
        if !self
            .agent
            .terminal_layout
            .owner
            .as_ref()
            .is_some_and(|owner| Arc::ptr_eq(owner, &sequence.identity))
        {
            return Err(MezError::conflict("terminal layout sequence owner changed"));
        }
        self.agent.terminal_layout.owner = None;
        result?;
        if !self.session.is_attached_primary(&sequence.client) {
            return Err(MezError::forbidden("terminal layout client detached"));
        }
        let output = if sequence.interactive {
            self.present_terminal_layout_outcomes(
                &sequence.client,
                std::mem::take(&mut sequence.outcomes),
                sequence.browser.take(),
            )?
        } else {
            crate::runtime::runtime_command_outcomes_json(&sequence.outcomes)
        };
        if let Some((kind, command)) = sequence.diagnostic.take() {
            let mut fields = serde_json::Map::new();
            fields.insert(kind.into(), serde_json::json!(command));
            let excerpt = if kind == "attached_display_command" {
                output.chars().take(384).collect::<String>()
            } else {
                output.clone()
            };
            if kind == "attached_display_command" {
                fields.insert(
                    "truncated".into(),
                    serde_json::json!(excerpt.len() < output.len()),
                );
            }
            fields.insert("output".into(), serde_json::json!(excerpt));
            self.append_primary_client_event(
                &sequence.client,
                EventKind::Diagnostic,
                serde_json::Value::Object(fields).to_string(),
            )?;
        }
        Ok((output, std::mem::take(&mut sequence.following_actions)))
    }

    /// Checks private lease and live client identity; public command text is inert.
    fn validate_terminal_layout_sequence(&self, sequence: &RuntimeTerminalSequence) -> Result<()> {
        if !self
            .agent
            .terminal_layout
            .owner
            .as_ref()
            .is_some_and(|owner| Arc::ptr_eq(owner, &sequence.identity))
        {
            return Err(MezError::conflict("terminal layout sequence owner changed"));
        }
        if !self.session.is_attached_primary(&sequence.client) {
            return Err(MezError::forbidden("terminal layout client detached"));
        }
        Ok(())
    }
}
