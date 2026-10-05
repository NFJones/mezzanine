//! Closed remote-kill request bounds and settlement facts shared by adapters.
//!
//! These values do not grant destructive authority: the authenticated host owns
//! permissions, target visibility and lease revocation. Only the fixed kill
//! method is permitted, and uncertain mutation delivery must never be replayed.

use super::*;
use mez_core::ids::SessionId;

/// Allowlisted host evidence for one durably revoked lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KillSettlement {
    killed: bool,
    lease_id: String,
    session_id: String,
    state: String,
}

/// Bounds the exact target and logical mutation key without normalizing names.
pub(crate) fn validate_request(target: &str, key: &str) -> Result<()> {
    if target.trim().is_empty()
        || target.len() > 256
        || target.chars().any(char::is_control)
        || key.is_empty()
        || key.len() > 128
        || key.chars().any(char::is_control)
    {
        return Err(MezError::invalid_args(
            "outbound kill target or key invalid",
        ));
    }
    Ok(())
}

/// Projects only producer-defined fields after response correlation elsewhere.
pub(crate) fn project_settlement(
    value: &serde_json::Value,
    target: &str,
) -> Result<KillSettlement> {
    let object = value.as_object().ok_or_else(|| {
        MezError::invalid_state("outbound kill settlement invalid; outcome unknown")
    })?;
    let mut fields = serde_json::Map::new();
    for key in ["killed", "lease_id", "session_id", "state"] {
        fields.insert(
            key.into(),
            object.get(key).cloned().ok_or_else(|| {
                MezError::invalid_state("outbound kill settlement incomplete; outcome unknown")
            })?,
        );
    }
    let settlement = serde_json::from_value(serde_json::Value::Object(fields)).map_err(|_| {
        MezError::invalid_state("outbound kill settlement invalid; outcome unknown")
    })?;
    validate_settlement(&settlement, target)?;
    Ok(settlement)
}

/// Requires well-formed revoked evidence. The public target is untyped and the
/// authenticated host resolves IDs or exact names, including ID-looking names.
/// Correlation and the echoed original target/key are checked by the adapters.
pub(crate) fn validate_settlement(value: &KillSettlement, _target: &str) -> Result<()> {
    if !value.killed
        || value.state != "revoked"
        || SessionId::parse('$', value.session_id.clone()).is_none()
        || !value.lease_id.starts_with("lease-")
        || value.lease_id.len() <= 6
        || value.lease_id.len() > 128
        || value.lease_id.chars().any(char::is_control)
    {
        return Err(MezError::invalid_state(
            "outbound kill evidence changed; outcome unknown",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
