//! Conditional view reuse fenced by the last exact delivered snapshot.
//!
//! The broker checks identity and geometry before forwarding the fixed view
//! request. A correlated unchanged reply cannot contain replacement view bytes
//! or receipts. This is a delivery fence, not proof of terminal commitment; the
//! consumed local client separately owns that evidence. No replay or retargeting.

use super::*;

/// Rejects foreign or malformed bases before the remote view request.
pub(super) fn validate_base(
    request: &ViewRequest,
    delivered: Option<&(String, u16, u16)>,
) -> Result<()> {
    if let Some(identity) = &request.if_view_identity
        && (!crate::host::terminal::wire_identity::valid_view_identity(identity)
            || !delivered.is_some_and(|base| {
                base.0 == *identity && base.1 == request.columns && base.2 == request.rows
            }))
    {
        return Err(MezError::conflict("outbound conditional view base changed"));
    }
    Ok(())
}

/// Projects only exact unchanged metadata; absence selects full-view decoding.
pub(super) fn project_unchanged(
    body: &str,
    request: &ViewRequest,
    session: &serde_json::Value,
) -> Result<Option<String>> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("outbound conditional reply invalid"))?;
    if value["jsonrpc"] != "2.0" || value["id"] != VIEW_REQUEST_ID || value.get("error").is_some() {
        return Err(MezError::invalid_state(
            "outbound conditional reply uncorrelated",
        ));
    }
    let result = value
        .get("result")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("outbound conditional result unavailable"))?;
    if !result.contains_key("not_modified") {
        return Ok(None);
    }
    let (identity, cutoff) = project_revision(&value)?;
    if result.get("not_modified") != Some(&serde_json::Value::Bool(true))
        || result.contains_key("view")
        || result.contains_key("presentation_ids")
        || request.if_view_identity.is_none()
        || identity != request.if_view_identity
    {
        return Err(MezError::conflict("outbound unchanged view base invalid"));
    }
    let rate = project_render_rate(&value)?;
    Ok(Some(
        serde_json::json!({"handle":request.handle,"session":session,
        "columns":request.columns,"rows":request.rows,"not_modified":true,
        "view_identity":identity,"event_cutoff":cutoff,"render_rate_limit_fps":rate})
        .to_string(),
    ))
}

#[cfg(test)]
mod tests;
