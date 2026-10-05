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

/// Decodes an optional snapshot slot while bounding its cell range and row to
/// the delivered lines. Missing/null slots remain absent (for example in zen).
/// This opt-in boundary leaves legacy attach interpretation unchanged.
pub(crate) fn bounded_status_slot(
    value: Option<&serde_json::Value>,
    line_count: usize,
    columns: u16,
    rows: u16,
) -> Result<Option<TerminalIrohStatusSlot>> {
    if !(1..=4096).contains(&columns)
        || !(1..=4096).contains(&rows)
        || line_count > usize::from(rows)
    {
        return Err(MezError::invalid_state(
            "snapshot status geometry unavailable",
        ));
    }
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let slot = parse_terminal_iroh_status_slot(value)?;
    if slot.row >= line_count
        || slot.width == 0
        || slot
            .column
            .checked_add(slot.width)
            .is_none_or(|end| end > usize::from(columns))
    {
        return Err(MezError::invalid_state(
            "snapshot status slot exceeds geometry",
        ));
    }
    Ok(Some(slot))
}

/// Serializes only decoded slot fields and shared canonical rendition values.
/// Unknown remote metadata is excluded; no health state is inferred or rendered.
pub(crate) fn status_slot_value(slot: Option<TerminalIrohStatusSlot>) -> serde_json::Value {
    let Some(slot) = slot else {
        return serde_json::Value::Null;
    };
    let renditions = super::wire_styles::style_rows_value(&[vec![
        mez_terminal::TerminalStyleSpan {
            start: 0,
            length: 1,
            rendition: slot.good,
        },
        mez_terminal::TerminalStyleSpan {
            start: 0,
            length: 1,
            rendition: slot.degraded,
        },
        mez_terminal::TerminalStyleSpan {
            start: 0,
            length: 1,
            rendition: slot.poor,
        },
        mez_terminal::TerminalStyleSpan {
            start: 0,
            length: 1,
            rendition: slot.unknown,
        },
    ]]);
    serde_json::json!({"row":slot.row,"column":slot.column,"width":slot.width,
        "good":renditions[0][0]["rendition"],"degraded":renditions[0][1]["rendition"],
        "poor":renditions[0][2]["rendition"],"unknown":renditions[0][3]["rendition"]})
}

#[cfg(test)]
mod tests;
