//! Agent context and diagnostic export helpers for runtime commands.
//!
//! This child module owns the shared export destinations for agent context
//! dumps, trace logs, copied say output, and retained patch payloads. Live
//! command dispatch remains in the parent commands-support module while these
//! diagnostic formatting and destination rules stay isolated here.

use super::super::service_state::RuntimeAgentPatchRecord;
use super::super::{
    AgentTurnRecord, AgentTurnState, AgentTurnTrigger, MezError, Result, RuntimeSessionService,
    current_unix_seconds, json_escape,
};
use crate::agent::assemble_model_request_with_retained_tail_percent;
use mez_agent::{ContextSourceKind, ModelMessageRole, ModelRequest, append_mcp_context};

/// Captures one assembled model request dump before it is written to a target.
struct RuntimeAgentContextDump {
    /// Exact provider-facing request context dump.
    dump: String,
    /// Turn identifier used to assemble the context dump.
    turn_id: String,
    /// Number of model messages in the assembled request.
    message_count: usize,
    /// Optional source marker for synthetic context previews.
    source: Option<&'static str>,
}

/// Result of attempting to assemble a model request context dump.
enum RuntimeAgentContextDumpResult {
    /// A dump was assembled and is ready to write.
    Written(RuntimeAgentContextDump),
    /// A dump could not be assembled for the live runtime state.
    NotWritten {
        /// Running turn, when one existed.
        turn_id: Option<String>,
        /// Stable reason fragment for status reporting.
        reason: &'static str,
        /// Human-readable pane message.
        message: String,
    },
}

/// Writes the assembled model request for the current agent turn to a target.
///
/// # Parameters
/// - `service`: The runtime service that owns pane-local agent state.
/// - `pane_id`: The pane whose current or idle model context should be copied.
/// - `args`: Shared copy-target arguments: `pane`, `buffer [name]`, or
///   `clipboard`.
pub(in crate::runtime) fn runtime_write_agent_context_for_pane(
    service: &mut RuntimeSessionService,
    pane_id: &str,
    args: &str,
) -> Result<(String, bool)> {
    let target = runtime_agent_export_target(args, "copy-context", "agent-context")?;
    match runtime_agent_context_dump_for_pane(service, pane_id)? {
        RuntimeAgentContextDumpResult::Written(context_dump) => {
            let lines = context_dump.dump.lines().count();
            let bytes = context_dump.dump.len();
            let source = context_dump
                .source
                .map(|source| format!(":source={source}"))
                .unwrap_or_default();
            match target {
                RuntimeAgentExportTarget::Pane => {
                    service
                        .append_agent_status_text_to_terminal_buffer(pane_id, &context_dump.dump)?;
                    Ok((
                        format!(
                            "target={pane_id}:context_dump=written:destination=pane:turn={}:messages={}:lines={lines}:bytes={bytes}:format=model-request-json{source}",
                            context_dump.turn_id, context_dump.message_count
                        ),
                        false,
                    ))
                }
                RuntimeAgentExportTarget::Buffer(name) => {
                    service.paste_buffers.set_with_origin(
                        &name,
                        &context_dump.dump,
                        Some(format!("pane:{pane_id}:agent-context")),
                    )?;
                    Ok((
                        format!(
                            "target={pane_id}:context_dump=written:destination=buffer:name=\"{}\":turn={}:messages={}:lines={lines}:bytes={bytes}:format=model-request-json{source}",
                            json_escape(&name),
                            context_dump.turn_id,
                            context_dump.message_count
                        ),
                        true,
                    ))
                }
                RuntimeAgentExportTarget::Clipboard => {
                    service.copy_text_to_buffer_and_host_clipboard(
                        "clipboard",
                        context_dump.dump.clone(),
                        format!("pane:{pane_id}:agent-context"),
                    )?;
                    Ok((
                        format!(
                            "target={pane_id}:context_dump=written:destination=clipboard:turn={}:messages={}:lines={lines}:bytes={bytes}:format=model-request-json{source}",
                            context_dump.turn_id, context_dump.message_count
                        ),
                        true,
                    ))
                }
            }
        }
        RuntimeAgentContextDumpResult::NotWritten {
            turn_id,
            reason,
            message,
        } => {
            if matches!(target, RuntimeAgentExportTarget::Pane) {
                service.append_agent_error_text_to_terminal_buffer(pane_id, &message)?;
            }
            let turn = turn_id
                .map(|turn_id| format!(":turn={turn_id}"))
                .unwrap_or_default();
            Ok((
                format!("target={pane_id}:context_dump=not-written{turn}:reason={reason}"),
                false,
            ))
        }
    }
}

/// Assembles the model request context for the current or idle pane state.
fn runtime_agent_context_dump_for_pane(
    service: &mut RuntimeSessionService,
    pane_id: &str,
) -> Result<RuntimeAgentContextDumpResult> {
    let pane_id = pane_id.to_string();
    let running_turn_id = service
        .agent_shell_store
        .get(&pane_id)
        .and_then(|session| session.running_turn_id.as_deref())
        .map(ToOwned::to_owned);
    let Some(turn_id) = running_turn_id else {
        return runtime_idle_agent_context_dump_for_pane(service, &pane_id)
            .map(RuntimeAgentContextDumpResult::Written);
    };
    if !service.agent_turn_contexts.contains_key(&turn_id) {
        let message = format!("agent context dump: running turn {turn_id} has no stored context");
        return Ok(RuntimeAgentContextDumpResult::NotWritten {
            turn_id: Some(turn_id),
            reason: "context-not-found",
            message,
        });
    }
    let Some(turn) = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
    else {
        let message = format!("agent context dump: running turn {turn_id} has no ledger record");
        return Ok(RuntimeAgentContextDumpResult::NotWritten {
            turn_id: Some(turn_id),
            reason: "turn-not-found",
            message,
        });
    };
    let Some(model_profile) = service.agent_turn_model_profiles.get(&turn_id).cloned() else {
        let message = format!("agent context dump: running turn {turn_id} has no model profile");
        return Ok(RuntimeAgentContextDumpResult::NotWritten {
            turn_id: Some(turn_id),
            reason: "model-profile-not-found",
            message,
        });
    };
    service.refresh_agent_turn_project_guidance_context(&turn)?;
    let context = service
        .agent_turn_contexts
        .get(&turn_id)
        .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
    let mcp_summary = service.mcp_registry.prompt_summary();
    let context = append_mcp_context(context.clone(), &mcp_summary)?;
    let mut request = assemble_model_request_with_retained_tail_percent(
        &model_profile,
        &turn,
        &context,
        service.agent_compaction_raw_retention_percent,
    )?;
    request.available_mcp_tools = mcp_summary.available_tools.clone();
    let dump = runtime_model_request_context_dump(&pane_id, &turn_id, &request)?;
    let message_count = request.messages.len();
    Ok(RuntimeAgentContextDumpResult::Written(
        RuntimeAgentContextDump {
            dump,
            turn_id,
            message_count,
            source: None,
        },
    ))
}

/// Assembles the model request context that would be used for the next idle prompt.
///
/// # Parameters
/// - `service`: The live runtime service that owns pane context assembly.
/// - `pane_id`: The pane whose idle model context should be previewed.
fn runtime_idle_agent_context_dump_for_pane(
    service: &mut RuntimeSessionService,
    pane_id: &str,
) -> Result<RuntimeAgentContextDump> {
    let agent_id = format!("agent-{pane_id}");
    let (model_profile_name, model_profile) =
        service.active_model_profile_for_pane(pane_id, &agent_id, None)?;
    let context = service.agent_context_for_pane_prompt(
        pane_id,
        "[idle dump placeholder: next user prompt will be inserted here]",
        100,
    )?;
    let context = service.apply_agent_shell_preference_context(pane_id, context)?;
    let mcp_summary = service.mcp_registry.prompt_summary();
    let context = append_mcp_context(context, &mcp_summary)?;
    let turn_id = format!("idle-context-preview-{pane_id}");
    let turn = AgentTurnRecord {
        turn_id: turn_id.clone(),
        agent_id,
        pane_id: pane_id.to_string(),
        trigger: AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: current_unix_seconds().max(1),
        policy_profile: "runtime".to_string(),
        model_profile: model_profile_name,
        parent_turn_id: None,
        cooperation_mode: None,
        state: AgentTurnState::Queued,
        initial_capability: None,
    };
    let mut request = assemble_model_request_with_retained_tail_percent(
        &model_profile,
        &turn,
        &context,
        service.agent_compaction_raw_retention_percent,
    )?;
    request.available_mcp_tools = mcp_summary.available_tools.clone();
    let dump = runtime_model_request_context_dump(pane_id, &turn_id, &request)?;
    let message_count = request.messages.len();
    Ok(RuntimeAgentContextDump {
        dump,
        turn_id,
        message_count,
        source: Some("idle-preview"),
    })
}

/// Writes the retained bounded pane trace log to the requested destination.
pub(in crate::runtime) fn runtime_write_agent_trace_log_for_pane(
    service: &mut RuntimeSessionService,
    pane_id: &str,
    args: &str,
) -> Result<(String, bool)> {
    let target = runtime_agent_export_target(args, "copy-trace-log", "agent-trace")?;
    let Some(trace_log) = service.agent_pane_trace_log_text(pane_id) else {
        if matches!(target, RuntimeAgentExportTarget::Pane) {
            service.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!("agent trace log: no retained trace entries for pane {pane_id}"),
            )?;
        }
        return Ok((
            format!("target={pane_id}:trace_log=not-written:reason=empty"),
            false,
        ));
    };
    let dump = format!("agent trace log for pane {pane_id}\n{trace_log}");
    let lines = dump.lines().count();
    let bytes = dump.len();
    match target {
        RuntimeAgentExportTarget::Pane => {
            service.append_agent_status_text_to_terminal_buffer(pane_id, &dump)?;
            Ok((
                format!(
                    "target={pane_id}:trace_log=written:destination=pane:lines={lines}:bytes={bytes}"
                ),
                false,
            ))
        }
        RuntimeAgentExportTarget::Buffer(name) => {
            service.paste_buffers.set_with_origin(
                name.as_str(),
                dump,
                Some(format!("pane:{pane_id}:agent-trace-log")),
            )?;
            Ok((
                format!(
                    "target={pane_id}:trace_log=written:destination=buffer:buffer={}:lines={lines}:bytes={bytes}",
                    json_escape(&name)
                ),
                true,
            ))
        }
        RuntimeAgentExportTarget::Clipboard => {
            service.copy_text_to_buffer_and_host_clipboard(
                "clipboard",
                dump,
                format!("pane:{pane_id}:agent-trace-log"),
            )?;
            Ok((
                format!(
                    "target={pane_id}:trace_log=written:destination=clipboard:buffer=clipboard:lines={lines}:bytes={bytes}"
                ),
                true,
            ))
        }
    }
}

/// Writes the latest model-authored `say` text to the requested destination.
pub(in crate::runtime) fn runtime_write_agent_copy_output_for_pane(
    service: &mut RuntimeSessionService,
    pane_id: &str,
    args: &str,
) -> Result<(String, bool)> {
    let target = runtime_agent_export_target(args, "copy", "agent-output")?;
    let Some((turn_id, output, content_type)) = service.latest_agent_copy_output_for_pane(pane_id)
    else {
        if matches!(target, RuntimeAgentExportTarget::Pane) {
            service.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!("agent copy: no retained say action text for pane {pane_id}"),
            )?;
        }
        return Ok((
            format!(
                "target={pane_id}:say=not-written:reason=no-say-action:source=runtime-agent-say"
            ),
            false,
        ));
    };
    let lines = output.lines().count();
    let bytes = output.len();
    let turn = json_escape(&turn_id);
    match target {
        RuntimeAgentExportTarget::Pane => {
            service.append_agent_assistant_content_to_terminal_buffer(
                pane_id,
                &output,
                &content_type,
            )?;
            Ok((
                format!(
                    "target={pane_id}:say=written:destination=pane:turn={turn}:lines={lines}:bytes={bytes}:source=runtime-agent-say"
                ),
                false,
            ))
        }
        RuntimeAgentExportTarget::Buffer(name) => {
            service.paste_buffers.set_with_origin(
                name.as_str(),
                output,
                Some(format!("agent:{turn_id}:say")),
            )?;
            Ok((
                format!(
                    "target={pane_id}:say=written:destination=buffer:buffer={}:turn={turn}:lines={lines}:bytes={bytes}:source=runtime-agent-say",
                    json_escape(&name)
                ),
                true,
            ))
        }
        RuntimeAgentExportTarget::Clipboard => {
            service.copy_text_to_buffer_and_host_clipboard(
                "clipboard",
                output,
                format!("agent:{turn_id}:say"),
            )?;
            Ok((
                format!(
                    "target={pane_id}:say=written:destination=clipboard:buffer=clipboard:turn={turn}:lines={lines}:bytes={bytes}:source=runtime-agent-say"
                ),
                true,
            ))
        }
    }
}

/// Writes retained `apply_patch` payloads and statuses to the target destination.
pub(in crate::runtime) fn runtime_write_agent_patches_for_pane(
    service: &mut RuntimeSessionService,
    pane_id: &str,
    args: &str,
) -> Result<(String, bool)> {
    let target = runtime_agent_export_target(args, "copy-patches", "agent-patches")?;
    let session_id = service
        .agent_shell_store
        .get(pane_id)
        .map(|session| session.session_id.clone())
        .ok_or_else(|| MezError::invalid_state("agent shell session missing for copy-patches"))?;
    let Some(records) = service
        .agent_session_patch_records
        .get(&session_id)
        .filter(|records| !records.is_empty())
    else {
        if matches!(target, RuntimeAgentExportTarget::Pane) {
            service.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!("agent patches: no retained apply_patch actions for pane {pane_id}"),
            )?;
        }
        return Ok((
            format!("target={pane_id}:patches=not-written:reason=empty"),
            false,
        ));
    };
    let dump = runtime_agent_patch_dump(pane_id, &session_id, records);
    let lines = dump.lines().count();
    let bytes = dump.len();
    let patches = records.len();
    match target {
        RuntimeAgentExportTarget::Pane => {
            service.append_agent_status_text_to_terminal_buffer(pane_id, &dump)?;
            Ok((
                format!(
                    "target={pane_id}:patches=written:destination=pane:patches={patches}:lines={lines}:bytes={bytes}"
                ),
                false,
            ))
        }
        RuntimeAgentExportTarget::Buffer(name) => {
            service.paste_buffers.set_with_origin(
                name.as_str(),
                dump,
                Some(format!("pane:{pane_id}:agent-patches")),
            )?;
            Ok((
                format!(
                    "target={pane_id}:patches=written:destination=buffer:buffer={}:patches={patches}:lines={lines}:bytes={bytes}",
                    json_escape(&name)
                ),
                true,
            ))
        }
        RuntimeAgentExportTarget::Clipboard => {
            service.copy_text_to_buffer_and_host_clipboard(
                "clipboard",
                dump,
                format!("pane:{pane_id}:agent-patches"),
            )?;
            Ok((
                format!(
                    "target={pane_id}:patches=written:destination=clipboard:buffer=clipboard:patches={patches}:lines={lines}:bytes={bytes}"
                ),
                true,
            ))
        }
    }
}

/// Formats retained patch records as a plain text export.
fn runtime_agent_patch_dump(
    pane_id: &str,
    session_id: &str,
    records: &[RuntimeAgentPatchRecord],
) -> String {
    let mut lines = vec![format!(
        "agent patches for pane {pane_id} session {session_id}"
    )];
    for (index, record) in records.iter().enumerate() {
        let strip = record
            .strip
            .map(|strip| format!(" strip={strip}"))
            .unwrap_or_default();
        let error = match (&record.error_code, &record.error_message) {
            (Some(code), Some(message)) => format!(
                r#" error_code={} error_message="{}""#,
                json_escape(code),
                json_escape(&message.replace('\n', "\\n"))
            ),
            (Some(code), None) => format!(" error_code={}", json_escape(code)),
            (None, Some(message)) => format!(
                r#" error_message="{}""#,
                json_escape(&message.replace('\n', "\\n"))
            ),
            (None, None) => String::new(),
        };
        lines.push(format!(
            "patch {}: turn={} action={} status={} bytes={}{}{}",
            index.saturating_add(1),
            record.turn_id,
            record.action_id,
            record.status,
            record.patch.len(),
            strip,
            error
        ));
        lines.push(record.patch.clone());
    }
    lines.join("\n")
}

/// Destination for an agent diagnostic export.
enum RuntimeAgentExportTarget {
    /// Write the export into the pane buffer.
    Pane,
    /// Write the export into one named internal paste buffer.
    Buffer(String),
    /// Write the export into the clipboard paste buffer and host clipboard.
    Clipboard,
}

/// Parses shared agent export target arguments.
fn runtime_agent_export_target(
    args: &str,
    command: &str,
    default_buffer_name: &str,
) -> Result<RuntimeAgentExportTarget> {
    let mut parts = args.split_whitespace();
    let target = parts.next().unwrap_or("pane");
    match target {
        "pane" => {
            if parts.next().is_some() {
                return Err(MezError::invalid_args(format!(
                    "{command} pane does not accept additional arguments"
                )));
            }
            Ok(RuntimeAgentExportTarget::Pane)
        }
        "buffer" => {
            let name = parts.next().unwrap_or(default_buffer_name).to_string();
            if parts.next().is_some() {
                return Err(MezError::invalid_args(format!(
                    "{command} buffer accepts at most one buffer name"
                )));
            }
            Ok(RuntimeAgentExportTarget::Buffer(name))
        }
        "clipboard" => {
            if parts.next().is_some() {
                return Err(MezError::invalid_args(format!(
                    "{command} clipboard does not accept additional arguments"
                )));
            }
            Ok(RuntimeAgentExportTarget::Clipboard)
        }
        _ => Err(MezError::invalid_args(format!(
            "{command} expects one of: pane, buffer [name], clipboard"
        ))),
    }
}

/// Formats the exact provider-facing model request context as JSON.
fn runtime_model_request_context_dump(
    pane_id: &str,
    turn_id: &str,
    request: &ModelRequest,
) -> Result<String> {
    let payload = serde_json::json!({
        "kind": "model_request_context_dump",
        "pane_id": pane_id,
        "turn_id": turn_id,
        "provider": &request.provider,
        "model": &request.model,
        "agent_id": &request.agent_id,
        "interaction_kind": request.interaction_kind.as_str(),
        "allowed_actions": request.allowed_actions.action_type_names(),
        "available_mcp_tools": request
            .available_mcp_tools
            .iter()
            .map(|tool| serde_json::json!({
                "server_id": &tool.server_id,
                "tool_name": &tool.tool_name,
                "description": &tool.description,
                "approval_required": tool.approval_required,
                "input_schema": serde_json::from_str::<serde_json::Value>(&tool.input_schema_json)
                    .unwrap_or_else(|_| serde_json::json!(&tool.input_schema_json))
            }))
            .collect::<Vec<_>>(),
        "messages": request
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| serde_json::json!({
                "index": index,
                "role": runtime_model_message_role_name_for_dump(message.role),
                "source": runtime_context_source_name(message.source),
                "content_bytes": message.content.len(),
                "content": &message.content
            }))
            .collect::<Vec<_>>()
    });
    serde_json::to_string_pretty(&payload).map_err(|error| {
        MezError::invalid_state(format!("model request context dump JSON failed: {error}"))
    })
}

/// Returns the stable display label for a context block source.
fn runtime_context_source_name(source: ContextSourceKind) -> &'static str {
    match source {
        ContextSourceKind::System => "system",
        ContextSourceKind::UserInstruction => "user-instruction",
        ContextSourceKind::SkillInstruction => "skill-instruction",
        ContextSourceKind::DeveloperInstruction => "developer-instruction",
        ContextSourceKind::Policy => "policy",
        ContextSourceKind::Configuration => "configuration",
        ContextSourceKind::RuntimeHint => "runtime-hint",
        ContextSourceKind::LocalMessage => "local-message",
        ContextSourceKind::ProjectGuidance => "project-guidance",
        ContextSourceKind::Memory => "memory",
        ContextSourceKind::Transcript => "transcript",
        ContextSourceKind::TranscriptUser => "transcript-user",
        ContextSourceKind::TranscriptAssistant => "transcript-assistant",
        ContextSourceKind::TranscriptTool => "transcript-tool",
        ContextSourceKind::EvidenceLedger => "evidence-ledger",
        ContextSourceKind::CommittedEvidence => "committed-evidence",
        ContextSourceKind::ActionResult => "action-result",
    }
}

/// Returns the stable display label for a model request message role.
fn runtime_model_message_role_name_for_dump(role: ModelMessageRole) -> &'static str {
    match role {
        ModelMessageRole::System => "system",
        ModelMessageRole::Developer => "developer",
        ModelMessageRole::User => "user",
        ModelMessageRole::Assistant => "assistant",
        ModelMessageRole::Tool => "tool",
    }
}
