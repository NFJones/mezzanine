//! Shared redraw classification for terminal event transport adapters.
//!
//! Preserves the existing attach event interpretation and precedence. Strict
//! event notifications require JSON-RPC 2.0 and matching method/event_type;
//! payload content is neither retained nor returned. This owner does not decode
//! framing/compression, authenticate streams, schedule rendering, or grant input
//! authority. Unknown valid event types retain the existing ordinary-redraw rule.

use crate::error::{MezError, Result};

/// Render action requested by an attached runtime event stream notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttachRenderAction {
    /// No visible attached-terminal redraw is needed.
    None,
    /// Request a fresh view while preserving the diff-render base.
    View,
    /// Refresh immediately without discarding the physical diff base.
    ImmediateView,
    /// Invalidate the diff-render base before requesting a fresh view.
    InvalidateAndView,
    /// The auxiliary event stream disconnected.
    Disconnect,
}

impl AttachRenderAction {
    /// Returns the closed local IPC spelling of a redraw fact, never a method.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::View => "view",
            Self::ImmediateView => "immediate_view",
            Self::InvalidateAndView => "invalidate_and_view",
            Self::Disconnect => "disconnect",
        }
    }

    /// Decodes only the closed redraw vocabulary; unknown values grant no action.
    pub(crate) fn from_str(value: &str) -> Result<Self> {
        match value {
            "none" => Ok(Self::None),
            "view" => Ok(Self::View),
            "immediate_view" => Ok(Self::ImmediateView),
            "invalidate_and_view" => Ok(Self::InvalidateAndView),
            "disconnect" => Ok(Self::Disconnect),
            _ => Err(MezError::invalid_state(
                "outbound redraw action unsupported",
            )),
        }
    }

    /// Combines actions, preserving the strongest requirement for an event burst.
    pub(crate) const fn combine(self, other: Self) -> Self {
        if self.rank() >= other.rank() {
            self
        } else {
            other
        }
    }

    /// Returns established precedence without inspecting payload content.
    const fn rank(self) -> u8 {
        match self {
            Self::None => 0,
            Self::View => 1,
            Self::ImmediateView => 2,
            Self::InvalidateAndView => 3,
            Self::Disconnect => 4,
        }
    }
}

/// Maps an event type onto the existing attached client's redraw requirements.
pub(crate) fn action_for_event_type(event_type: &str) -> AttachRenderAction {
    match event_type {
        "diagnostic" | "snapshot_changed" => AttachRenderAction::None,
        "config_changed" => AttachRenderAction::ImmediateView,
        "client_attached" | "client_detached" | "window_changed" => AttachRenderAction::View,
        "agent_status" | "approval_changed" | "hook_failed" | "mcp_server_changed" | "message"
        | "pane_changed" => AttachRenderAction::View,
        _ => AttachRenderAction::View,
    }
}

/// Validates one event notification and projects only redraw action and optional
/// event identity. Malformed frames reject without exposing their payload.
pub(crate) fn strict_event_action(body: &str) -> Result<(AttachRenderAction, Option<u64>)> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("Iroh event stream contained invalid JSON"))?;
    if value.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return Err(MezError::invalid_state(
            "Iroh event stream notification omitted JSON-RPC 2.0",
        ));
    }
    let method = value
        .get("method")
        .and_then(serde_json::Value::as_str)
        .and_then(|method| method.strip_prefix("event/"))
        .ok_or_else(|| MezError::invalid_state("Iroh event stream contained a non-event frame"))?;
    let params = value
        .get("params")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("Iroh event stream omitted params"))?;
    let event_type = params
        .get("event_type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_state("Iroh event stream omitted event_type"))?;
    if method != event_type {
        return Err(MezError::invalid_state(
            "Iroh event stream method and event_type did not match",
        ));
    }
    Ok((
        action_for_event_type(event_type),
        params.get("event_id").and_then(serde_json::Value::as_u64),
    ))
}

#[cfg(test)]
mod tests;
