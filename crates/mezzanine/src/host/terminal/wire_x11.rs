//! Shared X11 route settlement decoding for terminal transport adapters.
//!
//! Preserves established direct-attach capability, version, mode and generation
//! validation. Route proof remains secret-bearing and is never formatted into
//! diagnostics. Callers separately authenticate/correlate the enclosing reply,
//! enforce frame limits and own route cancellation and local credential cleanup.
//! Decoding alone starts no relay, grants no new authority and writes no terminal.

use base64::Engine as _;
use zeroize::Zeroizing;

use crate::error::{MezError, Result};
use crate::runtime::x11::{X11ForwardingMode, X11ForwardingResult};

/// Decodes only explicitly requested route authority. Unrequested capability,
/// missing support, mismatched mode or invalid route evidence rejects before
/// workers start. Enclosing response identity/correlation belongs to the caller.
pub(crate) fn validate_route(
    body: &str,
    requested_mode: Option<X11ForwardingMode>,
) -> Result<Option<X11ForwardingResult>> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("invalid Iroh initialize response"))?;
    let result = value
        .get("result")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("Iroh initialize response omitted result"))?;
    let capable = result
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("features"))
        .and_then(|features| features.get("x11_forwarding"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let negotiated = result.get("x11_forwarding");
    let Some(requested_mode) = requested_mode else {
        if capable || negotiated.is_some_and(|value| !value.is_null()) {
            return Err(MezError::invalid_state(
                "Iroh server returned unrequested X11 forwarding authority",
            ));
        }
        return Ok(None);
    };
    if !capable {
        return Err(MezError::not_implemented(
            "Iroh server does not support requested X11 forwarding",
        ));
    }
    let negotiated = negotiated
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("Iroh X11 negotiation omitted route metadata"))?;
    let version = negotiated
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .filter(|version| *version == crate::runtime::x11::X11_FORWARDING_VERSION)
        .ok_or_else(|| {
            MezError::invalid_state("Iroh X11 negotiation returned an unsupported version")
        })?;
    let mode = match negotiated.get("mode").and_then(serde_json::Value::as_str) {
        Some("untrusted") => X11ForwardingMode::Untrusted,
        Some("trusted") => X11ForwardingMode::Trusted,
        _ => {
            return Err(MezError::invalid_state(
                "Iroh X11 negotiation returned an invalid mode",
            ));
        }
    };
    if mode != requested_mode {
        return Err(MezError::forbidden(
            "Iroh X11 negotiation changed the requested trust mode",
        ));
    }
    let generation = negotiated
        .get("generation")
        .and_then(serde_json::Value::as_u64)
        .filter(|generation| *generation > 0)
        .ok_or_else(|| {
            MezError::invalid_state("Iroh X11 negotiation returned an invalid generation")
        })?;
    let token = negotiated
        .get("route_token_base64")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_state("Iroh X11 negotiation omitted its route token"))?;
    let decoded_token = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(token)
            .map_err(|_| {
                MezError::invalid_state("Iroh X11 negotiation returned an invalid route token")
            })?,
    );
    let token: Zeroizing<[u8; crate::runtime::x11::X11_ROUTE_TOKEN_BYTES]> =
        Zeroizing::new(decoded_token.as_slice().try_into().map_err(|_| {
            MezError::invalid_state("Iroh X11 negotiation returned an invalid route token")
        })?);
    Ok(Some(X11ForwardingResult {
        version,
        mode,
        generation,
        route_token: crate::runtime::x11::X11RouteToken::new(*token),
    }))
}

#[cfg(test)]
mod tests;
