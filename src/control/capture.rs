//! Control Capture implementation.
//!
//! This module owns the control capture boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    CaptureEndpoint, CaptureOrigin, CaptureRange, JsonRpcRequest, MezError, PaneCaptureSource,
    Result, Session, json_escape, pane_state_json, pane_state_json_with_capture,
    pane_target_checked_resolved, target_or_active_pane,
};
use crate::terminal::{GraphicRendition, TerminalColor, TerminalStyleSpan};

// Pane capture parsing and response helpers.

/// Runs the dispatch pane capture request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_pane_capture_request(
    request: &JsonRpcRequest,
    session: &Session,
    captures: &[PaneCaptureSource],
) -> Result<String> {
    let params = request
        .params
        .as_deref()
        .ok_or_else(|| MezError::invalid_args("pane/capture requires a params object"))?;
    let target = pane_target_checked_resolved(session, params)?;
    let (window, pane) = target_or_active_pane(session, target.as_deref())?;
    let source = captures
        .iter()
        .find(|capture| capture.pane_id == pane.id.to_string());
    let params_value = serde_json::from_str::<serde_json::Value>(params)
        .map_err(|_| MezError::invalid_args("pane/capture params must be valid JSON"))?;
    let range = parse_capture_range(
        params_value
            .get("range")
            .ok_or_else(|| MezError::invalid_args("pane/capture requires range"))?,
    )?;
    let mut lines = Vec::new();
    let mut line_style_spans = Vec::new();
    let mut alternate_screen_active = false;
    let mut truncated = false;

    if let Some(source) = source {
        alternate_screen_active = source.alternate_screen_active;
        truncated = source.truncated;
        match range.origin {
            CaptureOrigin::Visible => {
                lines.extend(source.visible_lines.iter().cloned());
                line_style_spans.extend(source.visible_line_style_spans.iter().cloned());
            }
            CaptureOrigin::History => {
                lines.extend(source.history_lines.iter().cloned());
                line_style_spans.extend(source.history_line_style_spans.iter().cloned());
            }
            CaptureOrigin::Combined => {
                lines.extend(source.history_lines.iter().cloned());
                line_style_spans.extend(source.history_line_style_spans.iter().cloned());
                if !source.alternate_screen_active {
                    lines.extend(source.visible_lines.iter().cloned());
                    line_style_spans.extend(source.visible_line_style_spans.iter().cloned());
                }
            }
        }
    }
    line_style_spans.truncate(lines.len());
    while line_style_spans.len() < lines.len() {
        line_style_spans.push(Vec::new());
    }

    let (range_start, range_end) = resolve_capture_range_bounds(&range, lines.len())?;
    let lines = lines
        .get(range_start..range_end)
        .ok_or_else(|| MezError::invalid_state("resolved pane capture range is invalid"))?
        .to_vec();
    let line_style_spans = line_style_spans
        .get(range_start..range_end)
        .unwrap_or_default()
        .to_vec();
    let range_json = capture_range_json(range.origin, range_start, range_end);
    let pane_json = source
        .map(|source| pane_state_json_with_capture(session.id.as_str(), window, pane, source))
        .unwrap_or_else(|| pane_state_json(session, window, pane));
    Ok(format!(
        r#"{{"content":"{}","truncated":{},"range":{},"pane":{},"alternate_screen_active":{},"source_available":{},"line_style_spans":{}}}"#,
        json_escape(&lines.join("\n")),
        truncated,
        range_json,
        pane_json,
        alternate_screen_active,
        source.is_some(),
        terminal_line_style_spans_json(&line_style_spans)
    ))
}

/// Runs the parse capture range operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_capture_range(value: &serde_json::Value) -> Result<CaptureRange> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("pane/capture range must be an object"))?;
    let origin = match object
        .get("origin")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_args("pane/capture range requires origin"))?
    {
        "visible" => CaptureOrigin::Visible,
        "history" => CaptureOrigin::History,
        "combined" => CaptureOrigin::Combined,
        _ => {
            return Err(MezError::invalid_args(
                "pane/capture range origin must be visible, history, or combined",
            ));
        }
    };
    let start = parse_capture_endpoint(
        object
            .get("start")
            .ok_or_else(|| MezError::invalid_args("pane/capture range requires start"))?,
        "start",
    )?;
    let end = parse_capture_endpoint(
        object
            .get("end")
            .ok_or_else(|| MezError::invalid_args("pane/capture range requires end"))?,
        "end",
    )?;
    Ok(CaptureRange { origin, start, end })
}

/// Runs the parse capture endpoint operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_capture_endpoint(
    value: &serde_json::Value,
    name: &str,
) -> Result<CaptureEndpoint> {
    if let Some(value) = value.as_str() {
        return match value {
            "start" => Ok(CaptureEndpoint::Start),
            "end" => Ok(CaptureEndpoint::End),
            _ => Err(MezError::invalid_args(format!(
                "pane/capture range {name} must be an integer, start, or end"
            ))),
        };
    }
    let Some(offset) = value.as_u64() else {
        return Err(MezError::invalid_args(format!(
            "pane/capture range {name} must be an integer, start, or end"
        )));
    };
    let offset = usize::try_from(offset)
        .map_err(|_| MezError::invalid_args("pane/capture range offset is too large"))?;
    Ok(CaptureEndpoint::Offset(offset))
}

/// Runs the resolve capture range bounds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn resolve_capture_range_bounds(
    range: &CaptureRange,
    line_count: usize,
) -> Result<(usize, usize)> {
    let start = resolve_capture_endpoint(range.start, line_count);
    let end = resolve_capture_endpoint(range.end, line_count);
    if start > end {
        return Err(MezError::invalid_args(
            "pane/capture range start must not be greater than end",
        ));
    }
    Ok((start.min(line_count), end.min(line_count)))
}

/// Runs the resolve capture endpoint operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn resolve_capture_endpoint(endpoint: CaptureEndpoint, line_count: usize) -> usize {
    match endpoint {
        CaptureEndpoint::Start => 0,
        CaptureEndpoint::End => line_count,
        CaptureEndpoint::Offset(offset) => offset,
    }
}

/// Runs the capture range json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn capture_range_json(origin: CaptureOrigin, start: usize, end: usize) -> String {
    format!(
        r#"{{"origin":"{}","start":{},"end":{}}}"#,
        capture_origin_name(origin),
        start,
        end
    )
}

/// Runs the capture origin name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn capture_origin_name(origin: CaptureOrigin) -> &'static str {
    match origin {
        CaptureOrigin::Visible => "visible",
        CaptureOrigin::History => "history",
        CaptureOrigin::Combined => "combined",
    }
}

/// Runs the terminal line style spans json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_line_style_spans_json(line_spans: &[Vec<TerminalStyleSpan>]) -> String {
    let lines = line_spans
        .iter()
        .map(|spans| terminal_style_spans_json(spans))
        .collect::<Vec<_>>();
    format!("[{}]", lines.join(","))
}

/// Runs the terminal style spans json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_style_spans_json(spans: &[TerminalStyleSpan]) -> String {
    let spans = spans
        .iter()
        .map(|span| {
            format!(
                r#"{{"start":{},"length":{},"rendition":{}}}"#,
                span.start,
                span.length,
                terminal_rendition_json(span.rendition)
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", spans.join(","))
}

/// Runs the terminal rendition json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_rendition_json(rendition: GraphicRendition) -> String {
    format!(
        r#"{{"bold":{},"underline":{},"inverse":{},"foreground":{},"background":{}}}"#,
        rendition.bold,
        rendition.underline,
        rendition.inverse,
        terminal_color_json(rendition.foreground),
        terminal_color_json(rendition.background)
    )
}

/// Runs the terminal color json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_color_json(color: Option<TerminalColor>) -> String {
    match color {
        Some(TerminalColor::Indexed(index)) => {
            format!(r#"{{"kind":"indexed","index":{index}}}"#)
        }
        Some(TerminalColor::Rgb(red, green, blue)) => {
            format!(r#"{{"kind":"rgb","red":{red},"green":{green},"blue":{blue}}}"#)
        }
        None => "null".to_string(),
    }
}
