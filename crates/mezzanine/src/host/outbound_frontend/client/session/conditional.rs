//! Explicit conditional snapshot exchange against an exact committed base.
//!
//! Full snapshot receipt alone cannot enable reuse. The presenter records the
//! identity/geometry only after complete output and receipt settlement. This API
//! is used by the internal foreground; callers must invalidate its base if
//! their exclusively owned terminal writer is replaced, reset or used elsewhere.
//! Errors consume the connection rather than replaying or returning ambiguous
//! ownership. An unchanged reply supplies no replacement rows or new receipts.

use super::*;

/// Closed unchanged envelope, excluding replacement content and receipt IDs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Unchanged {
    handle: FrontendHandle,
    session: SessionSummary,
    columns: u16,
    rows: u16,
    not_modified: bool,
    view_identity: String,
    event_cutoff: Option<u64>,
    render_rate_limit_fps: Option<u64>,
}

impl OutboundSessionClient {
    /// Invalidates conditional reuse before terminal reset/replacement or any
    /// output outside this owner. This never changes remote state or receipts.
    pub(crate) fn invalidate_committed_view(&mut self) {
        self.committed_view = None;
        self.painted_health = None;
    }

    /// Fetches one view, optionally reusing the exact committed identity/geometry.
    /// Returns true for a full replacement and false for a validated unchanged
    /// base. It does not write output or acknowledge presentation. Callers still
    /// own the physical terminal and must invalidate after external writes.
    pub(crate) async fn conditional_snapshot(
        mut self,
        columns: u16,
        rows: u16,
        budget: Duration,
    ) -> Result<(Self, bool)> {
        validate_budget(columns, rows, budget)?;
        let base = committed_base(&self, columns, rows).map(str::to_string);
        tokio::time::timeout(budget, async move {
            self.client.discovery.validate()?;
            let mut body =
                serde_json::json!({"handle":self.client.handle,"columns":columns,"rows":rows});
            if let Some(identity) = &base {
                body["if_view_identity"] = serde_json::json!(identity);
            }
            self.client
                .stream
                .send(ProtocolFrame::new(CONTENT_TYPE, body.to_string()))
                .await?;
            let frame = self
                .client
                .stream
                .next()
                .await
                .transpose()?
                .ok_or_else(|| {
                    MezError::invalid_state("outbound conditional snapshot unavailable")
                })?;
            if frame.content_type != CONTENT_TYPE {
                return Err(MezError::invalid_state(
                    "outbound conditional snapshot type unsupported",
                ));
            }
            let value: serde_json::Value = serde_json::from_str(&frame.body)
                .map_err(|_| MezError::invalid_state("outbound conditional snapshot invalid"))?;
            let modified = if value.get("not_modified").is_some() {
                let reply: Unchanged = serde_json::from_value(value)
                    .map_err(|_| MezError::invalid_state("outbound unchanged envelope invalid"))?;
                validate_unchanged(&reply, &self, columns, rows, base.as_deref())?;
                self.event_cutoff = reply.event_cutoff;
                self.render_rate_limit_fps = reply.render_rate_limit_fps;
                false
            } else {
                let snapshot: Snapshot = serde_json::from_value(value).map_err(|_| {
                    MezError::invalid_state("outbound conditional full snapshot invalid")
                })?;
                validate_snapshot(&snapshot, &self.client.handle, rows)?;
                if snapshot.session != self.summary {
                    return Err(MezError::conflict("outbound conditional session changed"));
                }
                self.styles = crate::host::terminal::wire_styles::bounded_style_rows(
                    &snapshot.line_style_spans,
                    snapshot.lines.len(),
                    columns,
                )?;
                self.modes = snapshot_modes(&snapshot, columns, rows)?;
                self.iroh_status_slot = snapshot_status_slot(&snapshot, columns, rows)?;
                self.lines = snapshot.lines;
                self.receipts = snapshot.presentation_ids;
                self.view_identity = snapshot.view_identity;
                self.event_cutoff = snapshot.event_cutoff;
                self.render_rate_limit_fps = snapshot.render_rate_limit_fps;
                self.snapshot_size = (columns, rows);
                self.committed_view = None;
                self.painted_health = None;
                true
            };
            self.client.discovery.validate()?;
            Ok((self, modified))
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound conditional snapshot timed out"))?
    }
}

/// Reuse requires the current retained rows, settled receipts and exact geometry
/// to still belong to the separately committed output identity.
fn committed_base(owner: &OutboundSessionClient, columns: u16, rows: u16) -> Option<&str> {
    let base = owner.committed_view.as_ref()?;
    (owner.receipts.is_empty()
        && owner.snapshot_size == (columns, rows)
        && (base.1, base.2) == (columns, rows)
        && owner.view_identity.as_deref() == Some(base.0.as_str()))
    .then_some(base.0.as_str())
}

/// Validates unchanged evidence against both the sent base and retained owner.
fn validate_unchanged(
    reply: &Unchanged,
    owner: &OutboundSessionClient,
    columns: u16,
    rows: u16,
    base: Option<&str>,
) -> Result<()> {
    if !reply.not_modified
        || reply.handle != owner.client.handle
        || reply.session != owner.summary
        || (reply.columns, reply.rows) != (columns, rows)
        || !crate::host::terminal::wire_identity::valid_view_identity(&reply.view_identity)
        || base != Some(reply.view_identity.as_str())
        || committed_base(owner, columns, rows) != base
        || base.is_none()
    {
        return Err(MezError::conflict(
            "outbound unchanged committed base invalid",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
