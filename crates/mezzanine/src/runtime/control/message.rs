//! Runtime control message-protocol ingress helpers.
//!
//! This module owns MMP frame dispatch through the runtime control adapter and
//! the associated local protocol bridge audit records. Keeping these routines
//! out of the main control dispatcher separates message transport handling from
//! control method routing and mutation orchestration.

use super::super::{
    AgentId, AuditActor, AuditRecord, MessageConnection, Result, RuntimeSessionService,
    decode_mmp_frame, handle_mmp_frame, runtime_json_string_field,
};
use super::protocol::{runtime_mmp_message_type, runtime_mmp_response_succeeded};

impl RuntimeSessionService {
    /// Handles one message-protocol frame for a runtime-owned MMP connection.
    pub fn handle_message_input(
        &mut self,
        input: &[u8],
        max_content_length: usize,
        connection: &mut MessageConnection,
        now_ms: u64,
    ) -> Result<(Vec<u8>, usize)> {
        self.require_live()?;
        let decoded_body = decode_mmp_frame(input, max_content_length)
            .ok()
            .map(|(body, _)| body);
        let previous_agent_id = connection.agent_id.clone();
        let (output, consumed) = handle_mmp_frame(
            input,
            max_content_length,
            self.control.message_service_mut(),
            connection,
            now_ms,
        )?;
        if runtime_mmp_response_succeeded(&output, max_content_length)
            && let Some(body) = decoded_body.as_deref()
        {
            self.append_runtime_message_protocol_audit(
                body,
                previous_agent_id.as_ref(),
                connection.agent_id.as_ref(),
            )?;
            self.deliver_pending_runtime_agent_messages(now_ms)?;
        }
        Ok((output, consumed))
    }

    /// Appends an audit record for message-protocol bridge identity changes.
    fn append_runtime_message_protocol_audit(
        &mut self,
        body: &str,
        previous_agent_id: Option<&AgentId>,
        current_agent_id: Option<&AgentId>,
    ) -> Result<()> {
        let Some(message_type) = runtime_mmp_message_type(body) else {
            return Ok(());
        };
        let (change, bridge_id) = match message_type.as_str() {
            "hello" => ("register", current_agent_id),
            "presence" => ("presence", previous_agent_id.or(current_agent_id)),
            _ => return Ok(()),
        };
        let Some(bridge_id) = bridge_id else {
            return Ok(());
        };
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let mut record = AuditRecord::local_protocol_bridge_change(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: bridge_id.to_string(),
            },
            "mmp/1",
            bridge_id.to_string(),
            change,
            "applied",
        );
        if let Some(role) = runtime_json_string_field(body, "role") {
            record = record.with_metadata("role", role);
        }
        if let Some(status) = runtime_json_string_field(body, "status") {
            record = record.with_metadata("status", status);
        }
        let _ = audit_log.append(record.sanitized())?;
        Ok(())
    }
}
