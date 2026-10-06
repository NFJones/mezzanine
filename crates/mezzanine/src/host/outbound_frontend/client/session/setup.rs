//! Shared local setup admission before first-use proof or session submission.
//!
//! Exact serialization owns the frame-body budget, including alias escaping and
//! handle overhead. Pre-pair validation uses the largest valid broker handle so
//! a later generation cannot introduce a predictable size rejection. This is
//! local grammar/budget evidence, not remote admission or permission to replay.

use super::*;

impl OutboundFrontendClient {
    /// Validates a future session envelope before consuming an invitation. The
    /// next authenticated handle has a fixed 32-byte hexadecimal owner and at
    /// most a u64 generation; reserve that maximum without borrowing authority
    /// from the current pairing handle. Performs no I/O or state mutation.
    pub(crate) fn validate_session_setup(
        profile: &str,
        initialize: &serde_json::Value,
        columns: u16,
        rows: u16,
        deadline: Duration,
    ) -> Result<()> {
        let handle = FrontendHandle {
            owner: "0".repeat(32),
            generation: u64::MAX,
        };
        encode_setup(&handle, profile, initialize, columns, rows, deadline).map(|_| ())
    }
}

/// Validates and encodes the actual or conservatively reserved setup envelope.
/// Returns parsed parameters for settlement checks, preserving original input.
pub(super) fn encode_setup(
    handle: &FrontendHandle,
    profile: &str,
    initialize: &serde_json::Value,
    columns: u16,
    rows: u16,
    deadline: Duration,
) -> Result<(String, crate::control::InitializeParams)> {
    validate_budget(columns, rows, deadline)?;
    if profile.is_empty() || profile.len() > 128 || profile.chars().any(char::is_control) {
        return Err(MezError::invalid_args("outbound profile alias invalid"));
    }
    if initialize.get("authentication").is_some() {
        return Err(MezError::forbidden(
            "outbound client must not supply credentials",
        ));
    }
    let params = initialize_params_from_json(&initialize.to_string())?;
    if !matches!(
        params.requested_role,
        RequestedRole::Primary | RequestedRole::Observer
    ) {
        return Err(MezError::forbidden("outbound session role unsupported"));
    }
    let body =
        serde_json::json!({"handle":handle,"profile":profile,"initialize":initialize}).to_string();
    if body.len() > HELLO_LIMIT {
        return Err(MezError::invalid_args("outbound setup exceeds limit"));
    }
    Ok((body, params))
}

#[cfg(test)]
mod tests;
