//! Consumed discovery of an exact session's dedicated local X11 publication.
//!
//! The closed reply contains a basename, not route proof or a remote destination.
//! Returning discovery grants no channel authority; consumers still authenticate
//! the local peer and exact handoff. This exchange allocates no occurrence or
//! permit and performs no remote request, terminal output or credential work.
//! Failure consumes control ownership without reconnecting or replaying setup.

use super::*;

/// Closed session-bound publication facts; no absolute path or secret fields.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    handle: FrontendHandle,
    session: SessionSummary,
    version: u8,
    socket_name: Option<String>,
}

impl OutboundSessionClient {
    /// Discovers optional X11 publication on this retained session. A basename
    /// is valid only for primary settlement and the fixed listener grammar.
    /// Absence remains unavailable rather than inferred from transport success.
    #[allow(
        dead_code,
        reason = "ordinary X11 activation follows discovery qualification"
    )]
    pub(crate) async fn discover_x11(mut self, budget: Duration) -> Result<(Self, Option<String>)> {
        validate_budget(1, 1, budget)?;
        tokio::time::timeout(budget, async move {
            self.client.discovery.validate()?;
            self.client
                .stream
                .send(ProtocolFrame::new(
                    CONTENT_TYPE,
                    serde_json::json!({"operation":"x11-discovery","handle":self.client.handle})
                        .to_string(),
                ))
                .await?;
            let frame = self
                .client
                .stream
                .next()
                .await
                .transpose()?
                .ok_or_else(|| {
                    MezError::invalid_state("outbound X11 discovery reply unavailable")
                })?;
            if frame.content_type != CONTENT_TYPE {
                return Err(MezError::invalid_state(
                    "outbound X11 discovery reply type invalid",
                ));
            }
            let reply: Reply = serde_json::from_str(&frame.body)
                .map_err(|_| MezError::invalid_state("outbound X11 discovery reply invalid"))?;
            validate_reply(&reply, &self.client.handle, &self.summary)?;
            self.client.discovery.validate()?;
            Ok((self, reply.socket_name))
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound X11 discovery timed out"))?
    }
}

/// Rejects publication from another session or outside the closed path family.
fn validate_reply(reply: &Reply, handle: &FrontendHandle, summary: &SessionSummary) -> Result<()> {
    if reply.handle != *handle || reply.session != *summary || reply.version != 1 {
        return Err(MezError::conflict(
            "outbound X11 discovery ownership changed",
        ));
    }
    if let Some(name) = &reply.socket_name {
        if summary.granted_role != "primary" {
            return Err(MezError::forbidden(
                "outbound X11 discovery requires primary",
            ));
        }
        crate::host::outbound_frontend::x11_discovery::validate_socket_name(name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
