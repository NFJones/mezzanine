//! Protected invitation decoding shared by CLI and outbound endpoint owners.
//!
//! Reads one bounded, owner-protected file using the existing filesystem checks.
//! Parsing preserves the established envelope, alias, pinned endpoint and legacy
//! scope semantics. It does not redeem tokens, acquire endpoint identities, save
//! profiles or establish application authority. Token-bearing results deliberately
//! have no Debug/serialization implementation; callers separately enforce expiry,
//! profile conflicts, transport policy and response settlement.

use std::path::Path;

use iroh::EndpointAddr;
use secrecy::SecretString;

use super::{RemoteClientProfileScope, RemoteRoleCeiling, read_remote_invitation_file};
use crate::error::{MezError, Result};

const MAX_INVITATION_BYTES: u64 = 64 * 1024;

/// Parsed invitation evidence, not independently granted remote authority.
pub(crate) struct ParsedIrohInvitation {
    /// Client-local alias; never part of authentication.
    pub(crate) profile_name: String,
    /// Pinned server identity and authored route hints.
    pub(crate) server_addr: EndpointAddr,
    /// First-use proof, kept outside ordinary diagnostics and serialization.
    pub(crate) token: SecretString,
    /// Authored maximum requested role, still subject to server validation.
    pub(crate) role: RemoteRoleCeiling,
    /// Omission remains conservative legacy-session scope.
    pub(crate) scope: RemoteClientProfileScope,
    /// Authored expiry, checked by the operation owner before dialing.
    pub(crate) expires_at_unix_seconds: u64,
}

/// Reads and validates one protected invitation, preserving established errors.
/// An alias override changes only local naming; it does not bypass the required
/// authored profile-name field or change endpoint, role, token or scope.
pub(crate) fn read_iroh_invitation(
    path: &Path,
    save_as: Option<&str>,
) -> Result<ParsedIrohInvitation> {
    let bytes = read_remote_invitation_file(path, MAX_INVITATION_BYTES)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| MezError::invalid_args("invalid Iroh invitation JSON"))?;
    let invitation = value.get("result").unwrap_or(&value);
    let object = invitation
        .as_object()
        .ok_or_else(|| MezError::invalid_args("Iroh invitation must be a JSON object"))?;
    if object
        .get("format_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(MezError::invalid_args(
            "Iroh invitation format_version must be 1",
        ));
    }
    let server_addr: EndpointAddr = serde_json::from_value(
        object
            .get("server_addr")
            .cloned()
            .ok_or_else(|| MezError::invalid_args("Iroh invitation omitted server_addr"))?,
    )
    .map_err(|_| MezError::invalid_args("Iroh invitation contains an invalid server_addr"))?;
    if let Some(server_endpoint_id) = object
        .get("server_endpoint_id")
        .and_then(serde_json::Value::as_str)
        && server_addr.id.to_string() != server_endpoint_id
    {
        return Err(MezError::forbidden(
            "Iroh invitation server identity does not match its address",
        ));
    }
    let profile_name = save_as
        .map(str::to_string)
        .unwrap_or(invitation_string(object, "profile_name")?);
    let token = invitation_string(object, "token")?;
    let role = match invitation_string(object, "role")?.as_str() {
        "observer" => RemoteRoleCeiling::Observer,
        "primary" => RemoteRoleCeiling::Primary,
        _ => {
            return Err(MezError::invalid_args(
                "Iroh invitation role is unsupported",
            ));
        }
    };
    let expires_at_unix_seconds = object
        .get("expires_at_unix_seconds")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_args("Iroh invitation omitted expiration"))?;
    let scope = match object
        .get("profile_scope")
        .and_then(serde_json::Value::as_str)
    {
        Some("host") => RemoteClientProfileScope::Host,
        None | Some("legacy_session") => RemoteClientProfileScope::LegacySession,
        Some(_) => {
            return Err(MezError::invalid_args(
                "Iroh invitation profile_scope must be host or legacy_session",
            ));
        }
    };
    Ok(ParsedIrohInvitation {
        profile_name,
        server_addr,
        token: SecretString::from(token),
        role,
        scope,
        expires_at_unix_seconds,
    })
}

/// Returns one required nonempty string without retaining errors containing it.
fn invitation_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| MezError::invalid_args(format!("Iroh invitation omitted {field}")))
}

#[cfg(test)]
mod tests;
