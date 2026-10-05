//! Shared terminal rendition decoding for product transport adapters.
//!
//! Preserves the existing attach wire interpretation: optional or nonboolean
//! attributes default false, missing/null colors mean default, and explicit
//! palette/RGB values must be bytes. Spans describe terminal cells, not byte
//! offsets. This decoder does not validate viewport geometry, sanitize text,
//! grant presentation authority, or acknowledge receipts; consumers own those
//! boundaries and enforce their enclosing frame/allocation budgets.

use crate::error::{MezError, Result};
use mez_terminal::{GraphicRendition, TerminalColor, TerminalStyleSpan};

/// Independent per-row allocation cap for layered snapshot styles. A full
/// 4096-cell base plus selection overlays may exceed the number of columns.
const MAX_SNAPSHOT_STYLE_SPANS_PER_ROW: usize = 8192;

/// Decodes aligned viewport style rows with finite cell and span budgets.
/// This opt-in check leaves legacy attach decoding unchanged. Positive spans
/// must fit the supplied cell width; overlapping overlays retain source order
/// because later renderer spans take precedence over earlier ones.
pub(crate) fn bounded_style_rows(
    value: &serde_json::Value,
    line_count: usize,
    columns: u16,
) -> Result<Vec<Vec<TerminalStyleSpan>>> {
    let rows = value
        .as_array()
        .filter(|rows| rows.len() == line_count)
        .ok_or_else(|| MezError::invalid_state("snapshot style rows must align with lines"))?;
    let width = usize::from(columns);
    if width == 0 || width > 4096 || line_count > 4096 {
        return Err(MezError::invalid_state(
            "snapshot style geometry unavailable",
        ));
    }
    rows.iter()
        .map(|row| {
            let values = row
                .as_array()
                .filter(|spans| spans.len() <= MAX_SNAPSHOT_STYLE_SPANS_PER_ROW)
                .ok_or_else(|| {
                    MezError::invalid_state("snapshot style row exceeds layer budget")
                })?;
            values
                .iter()
                .map(|value| {
                    let span = parse_terminal_style_span(value)?;
                    span.start
                        .checked_add(span.length)
                        .filter(|next| *next <= width)
                        .ok_or_else(|| {
                            MezError::invalid_state("snapshot style span exceeds geometry")
                        })?;
                    if span.length == 0 {
                        return Err(MezError::invalid_state("snapshot style span is empty"));
                    }
                    Ok(span)
                })
                .collect()
        })
        .collect()
}

/// Projects only decoded rendition fields; unknown peer metadata never crosses
/// transport boundaries. Cell coordinates and all existing flags are retained.
pub(crate) fn style_rows_value(rows: &[Vec<TerminalStyleSpan>]) -> serde_json::Value {
    let color = |color: Option<TerminalColor>| match color {
        None => serde_json::Value::Null,
        Some(TerminalColor::Indexed(index)) => serde_json::json!({"kind":"indexed","index":index}),
        Some(TerminalColor::Rgb(red, green, blue)) => {
            serde_json::json!({"kind":"rgb","red":red,"green":green,"blue":blue})
        }
    };
    serde_json::Value::Array(rows.iter().map(|row| {
        serde_json::Value::Array(row.iter().map(|span| {
            let rendition = span.rendition;
            serde_json::json!({"start":span.start,"length":span.length,"rendition":{
                "bold":rendition.bold,"dim":rendition.dim,"italic":rendition.italic,
                "underline":rendition.underline,"double_underline":rendition.double_underline,
                "strikethrough":rendition.strikethrough,"inverse":rendition.inverse,"hidden":rendition.hidden,
                "foreground":color(rendition.foreground),"background":color(rendition.background)
            }})
        }).collect())
    }).collect())
}

/// Decodes one style row, preserving source order and existing error semantics.
pub(crate) fn parse_terminal_style_span_row(
    value: &serde_json::Value,
) -> Result<Vec<TerminalStyleSpan>> {
    let spans = value
        .as_array()
        .ok_or_else(|| MezError::invalid_state("terminal step style span row is not an array"))?;
    spans.iter().map(parse_terminal_style_span).collect()
}

/// Decodes cell offsets and a rendition without guessing viewport bounds.
fn parse_terminal_style_span(value: &serde_json::Value) -> Result<TerminalStyleSpan> {
    let start = value
        .get("start")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_state("terminal step style span start is missing"))?;
    let length = value
        .get("length")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_state("terminal step style span length is missing"))?;
    let rendition = value
        .get("rendition")
        .ok_or_else(|| MezError::invalid_state("terminal step style span rendition is missing"))
        .and_then(parse_terminal_graphic_rendition)?;
    Ok(TerminalStyleSpan {
        start: usize::try_from(start)
            .map_err(|_| MezError::invalid_state("terminal step style span start is too large"))?,
        length: usize::try_from(length)
            .map_err(|_| MezError::invalid_state("terminal step style span length is too large"))?,
        rendition,
    })
}

/// Decodes the existing optional rendition flags and explicit color variants.
pub(crate) fn parse_terminal_graphic_rendition(
    value: &serde_json::Value,
) -> Result<GraphicRendition> {
    Ok(GraphicRendition {
        bold: bool_field(value, "bold"),
        dim: bool_field(value, "dim"),
        italic: bool_field(value, "italic"),
        underline: bool_field(value, "underline"),
        double_underline: bool_field(value, "double_underline"),
        strikethrough: bool_field(value, "strikethrough"),
        inverse: bool_field(value, "inverse"),
        hidden: bool_field(value, "hidden"),
        foreground: parse_terminal_color_field(value, "foreground")?,
        background: parse_terminal_color_field(value, "background")?,
    })
}

/// Retains the legacy false default for absent or nonboolean attributes.
fn bool_field(value: &serde_json::Value, field: &str) -> bool {
    value
        .get(field)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Treats missing/null colors as default while rejecting invalid explicit values.
fn parse_terminal_color_field(
    value: &serde_json::Value,
    field: &str,
) -> Result<Option<TerminalColor>> {
    let Some(color) = value.get(field) else {
        return Ok(None);
    };
    if color.is_null() {
        return Ok(None);
    }
    parse_terminal_color_value(color).map(Some)
}

/// Restricts palette and RGB components to their existing byte-sized variants.
fn parse_terminal_color_value(color: &serde_json::Value) -> Result<TerminalColor> {
    let kind = color
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_state("terminal step style color kind is missing"))?;
    match kind {
        "indexed" => {
            let index = color
                .get("index")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    MezError::invalid_state("terminal step indexed style color is missing")
                })?;
            Ok(TerminalColor::Indexed(u8::try_from(index).map_err(
                |_| MezError::invalid_state("terminal step indexed style color is out of range"),
            )?))
        }
        "rgb" => Ok(TerminalColor::Rgb(
            parse_u8_color_component(color, "red")?,
            parse_u8_color_component(color, "green")?,
            parse_u8_color_component(color, "blue")?,
        )),
        _ => Err(MezError::invalid_state(
            "terminal step style color kind is invalid",
        )),
    }
}

/// Checks one required RGB byte without truncating or clamping invalid input.
fn parse_u8_color_component(value: &serde_json::Value, field: &str) -> Result<u8> {
    let component = value
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_state("terminal step RGB style color is missing"))?;
    u8::try_from(component)
        .map_err(|_| MezError::invalid_state("terminal step RGB style color is out of range"))
}

#[cfg(test)]
mod tests;
