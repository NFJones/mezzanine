//! Closed local X11 publication spelling, not route or connection authority.
//!
//! Dedicated sockets use one fixed basename grammar under the retained private
//! configuration root. No absolute path, traversal, credential or remote target
//! can be supplied by discovery. Consumers must still authenticate the socket
//! peer and exact session handshake and enforce lifetime/capacity independently.

use crate::error::{MezError, Result};

/// Accepts only the dedicated listener's fixed randomized basename format.
/// Validation is pure and deliberately excludes all directory components.
pub(super) fn validate_socket_name(name: &str) -> Result<()> {
    let valid = name.len() == 22
        && name.starts_with('x')
        && name.ends_with(".sock")
        && name.as_bytes()[1..17]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte));
    if !valid {
        return Err(MezError::invalid_state(
            "outbound X11 discovery name invalid",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
