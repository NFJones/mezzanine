//! Closed hosted-session summaries shared by owner and local client adapters.
//!
//! Only server-visible lease facts are retained; principal, credentials, failure
//! content and arbitrary metadata are excluded. Frame bytes bound decoding, and
//! a finite count additionally bounds projection. These facts grant no authority.

use super::*;
use mez_core::ids::SessionId;

const MAX_LISTED_SESSIONS: usize = 4096;

/// Allowlisted lease fields, without private owner/proof or checkpoint metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListedSession {
    lease_id: String,
    session_id: String,
    name: Option<String>,
    state: String,
    created_at_unix_seconds: u64,
    expires_at_unix_seconds: Option<u64>,
}

/// Projects bounded remote rows into a closed schema without forwarding unknown
/// fields. Invalid identity, lifecycle, timestamps or duplicate rows reject.
pub(crate) fn project_sessions(value: &serde_json::Value) -> Result<Vec<ListedSession>> {
    let rows = value
        .as_array()
        .filter(|rows| rows.len() <= MAX_LISTED_SESSIONS)
        .ok_or_else(|| MezError::invalid_state("outbound session list exceeds limit"))?;
    let mut output = Vec::with_capacity(rows.len());
    for row in rows {
        let row = row
            .as_object()
            .ok_or_else(|| MezError::invalid_state("outbound session summary invalid"))?;
        let mut fields = serde_json::Map::new();
        for key in [
            "lease_id",
            "session_id",
            "name",
            "state",
            "created_at_unix_seconds",
            "expires_at_unix_seconds",
        ] {
            fields.insert(
                key.into(),
                row.get(key).cloned().ok_or_else(|| {
                    MezError::invalid_state("outbound session summary incomplete")
                })?,
            );
        }
        output.push(
            serde_json::from_value(serde_json::Value::Object(fields))
                .map_err(|_| MezError::invalid_state("outbound session summary invalid"))?,
        );
    }
    validate_sessions(&output)?;
    Ok(output)
}

/// Validates decoded closed rows, retaining order and optional absence/zero.
pub(crate) fn validate_sessions(rows: &[ListedSession]) -> Result<()> {
    if rows.len() > MAX_LISTED_SESSIONS {
        return Err(MezError::invalid_state(
            "outbound session list exceeds limit",
        ));
    }
    let mut leases = std::collections::HashSet::new();
    let mut sessions = std::collections::HashSet::new();
    for row in rows {
        if !row.lease_id.starts_with("lease-")
            || row.lease_id.len() <= 6
            || row.lease_id.len() > 128
            || row.lease_id.chars().any(char::is_control)
            || SessionId::parse('$', row.session_id.clone()).is_none()
            || !matches!(
                row.state.as_str(),
                "pending" | "active" | "failed" | "released" | "revoked"
            )
            || row.name.as_ref().is_some_and(|name| {
                name.trim().is_empty() || name.len() > 256 || name.chars().any(char::is_control)
            })
            || row
                .expires_at_unix_seconds
                .is_some_and(|expiry| expiry <= row.created_at_unix_seconds)
            || !leases.insert(&row.lease_id)
            || !sessions.insert(&row.session_id)
        {
            return Err(MezError::invalid_state(
                "outbound session summary evidence invalid",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
