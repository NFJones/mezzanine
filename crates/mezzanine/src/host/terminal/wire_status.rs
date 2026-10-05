//! Shared server-owned Iroh status-slot decoding for terminal adapters.
//!
//! Preserves established attach interpretation, checked coordinate conversions
//! and rendition defaults. A decoded slot is presentation metadata, not network
//! health evidence or authority. Consumers own geometry, frame-size, rendering
//! and output-commit checks; decoding does not compose or acknowledge a frame.

use super::TerminalIrohStatusSlot;
use super::wire_styles::parse_terminal_graphic_rendition;
use crate::error::{MezError, Result};

/// Decodes one server-owned client-space Iroh status slot. Required numeric
/// coordinates must fit usize and all four rendition fields must be present.
/// Present rendition values retain the shared decoder's permissive defaults.
pub(crate) fn parse_terminal_iroh_status_slot(
    value: &serde_json::Value,
) -> Result<TerminalIrohStatusSlot> {
    let number = |field: &str| {
        value
            .get(field)
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| MezError::invalid_state("terminal Iroh status slot is incomplete"))
            .and_then(|value| {
                usize::try_from(value)
                    .map_err(|_| MezError::invalid_state("terminal Iroh status slot is too large"))
            })
    };
    let rendition = |field: &str| {
        value
            .get(field)
            .ok_or_else(|| MezError::invalid_state("terminal Iroh status rendition is missing"))
            .and_then(parse_terminal_graphic_rendition)
    };
    Ok(TerminalIrohStatusSlot {
        row: number("row")?,
        column: number("column")?,
        width: number("width")?,
        good: rendition("good")?,
        degraded: rendition("degraded")?,
        poor: rendition("poor")?,
        unknown: rendition("unknown")?,
    })
}

#[cfg(test)]
mod tests;
