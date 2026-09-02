//! Runtime paste buffer, copy, capture, and history command helpers.
//!
//! This module owns live-terminal helpers that coordinate paste-buffer state,
//! copy-mode operations, pane capture, history search/export, and paste byte
//! preparation for the runtime command-support boundary.

use super::{
    CommandInvocation, CopyMode, MezError, PasteBuffer, Result, RuntimeSessionService,
    TerminalScreen, json_escape, runtime_flag_value, runtime_positional_args,
};
use crate::host::terminal::CopySelectionFormat;

/// Runs the runtime capture lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_capture_lines(
    screen: &TerminalScreen,
    invocation: &CommandInvocation,
) -> Vec<String> {
    if invocation.has_flag("-S", "--history") {
        screen.normal_content_lines()
    } else {
        screen.visible_lines()
    }
}

/// Runs the runtime buffer name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_buffer_name(invocation: &CommandInvocation) -> Option<&str> {
    runtime_flag_value(&invocation.args, "-b")
        .or_else(|| runtime_flag_value(&invocation.args, "--buffer"))
        .or_else(|| runtime_positional_args(invocation).first().copied())
}

/// Resolves the buffer name used by copy-mode commands.
///
/// Explicit command arguments take precedence, then the interactive active
/// buffer selection, then the default clipboard buffer.
pub(super) fn runtime_copy_target_buffer_name(
    service: &RuntimeSessionService,
    invocation: &CommandInvocation,
) -> String {
    runtime_buffer_name(invocation)
        .map(ToOwned::to_owned)
        .or_else(|| service.active_paste_buffer().map(ToOwned::to_owned))
        .unwrap_or_else(|| "clipboard".to_string())
}

/// Runs the runtime copy mode command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_copy_mode_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<()> {
    let descriptor = service.active_window_pane_descriptor(invocation.target_arg())?;
    let pane_id = descriptor.pane_id.to_string();
    if invocation
        .args
        .iter()
        .any(|arg| arg == "--cancel" || arg == "-q")
    {
        service.clear_copy_state_for_presented_surface(pane_id.as_str());
        return Ok(());
    }
    if service
        .active_copy_mode_for_presented_surface(pane_id.as_str())
        .is_none()
    {
        let screen = service
            .presented_pane_screen(pane_id.as_str())
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "pane screen not found",
                )
            })?;
        let viewport_rows = service.copy_mode_viewport_rows_for_pane(pane_id.as_str());
        let copy_mode = CopyMode::from_screen(screen, viewport_rows)?;
        service.insert_active_copy_mode_for_presented_surface(pane_id.as_str(), copy_mode);
    }
    let copy_target_buffer = invocation
        .args
        .iter()
        .any(|arg| arg == "--copy")
        .then(|| runtime_copy_target_buffer_name(service, invocation));
    let mut copied = None;
    {
        let copy_mode = service
            .active_copy_mode_for_presented_surface_mut(pane_id.as_str())
            .ok_or_else(|| MezError::invalid_state("copy mode was not retained"))?;
        if invocation
            .args
            .iter()
            .any(|arg| arg == "-u" || arg == "--page-up")
        {
            copy_mode.page_up();
        }
        if invocation.args.iter().any(|arg| arg == "--page-down") {
            copy_mode.page_down();
        }
        if invocation.args.iter().any(|arg| arg == "--top") {
            copy_mode.scroll_to_top();
        }
        if invocation.args.iter().any(|arg| arg == "--bottom") {
            copy_mode.scroll_to_bottom();
        }
        if let Some(name) = copy_target_buffer.as_ref() {
            copied = Some((name.to_string(), copy_mode.copy_selection()?));
        }
    }
    if let Some((name, copied)) = copied {
        service.copy_text_to_buffer_and_host_clipboard(
            name.as_str(),
            copied,
            format!("pane:{pane_id}:copy-mode"),
            false,
        )?;
    }
    Ok(())
}

/// Runs the runtime copy selection command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_copy_selection_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let descriptor = service.active_window_pane_descriptor(invocation.target_arg())?;
    let pane_id = descriptor.pane_id.to_string();
    let buffer_name = runtime_copy_target_buffer_name(service, invocation);
    let Some(copy_mode) = service.active_copy_mode_for_presented_surface(pane_id.as_str()) else {
        return Ok(format!(
            "target={pane_id}:copy=not-copied:reason=copy-mode-inactive"
        ));
    };
    let format = runtime_copy_selection_format(invocation)?;
    let copied = copy_mode.copy_selection_with_format(format)?;
    let bytes = copied.len();
    service.copy_text_to_buffer_and_host_clipboard(
        buffer_name.as_str(),
        copied,
        format!("pane:{pane_id}:copy-mode"),
        false,
    )?;
    if invocation.has_flag("-x", "--exit") {
        service.clear_copy_state_for_presented_surface(pane_id.as_str());
    }
    Ok(format!(
        "target={pane_id}:copy=copied:format={}:buffer={buffer_name}:bytes={bytes}",
        runtime_copy_selection_format_name(format)
    ))
}

/// Parses the explicit representation requested for a copy-mode selection.
fn runtime_copy_selection_format(invocation: &CommandInvocation) -> Result<CopySelectionFormat> {
    match runtime_flag_value(&invocation.args, "--format").unwrap_or("rendered") {
        "rendered" => Ok(CopySelectionFormat::Rendered),
        "source" => Ok(CopySelectionFormat::Source),
        value => Err(MezError::invalid_args(format!(
            "copy-selection format must be rendered or source, got {value}"
        ))),
    }
}

/// Returns the stable command-output name for one selection representation.
fn runtime_copy_selection_format_name(format: CopySelectionFormat) -> &'static str {
    match format {
        CopySelectionFormat::Rendered => "rendered",
        CopySelectionFormat::Source => "source",
    }
}

/// Runs the runtime paste clipboard command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_paste_clipboard_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let descriptor = service.active_window_pane_descriptor(invocation.target_arg())?;
    let primary = service
        .session
        .layout_owner_client_id()
        .cloned()
        .ok_or_else(|| {
            MezError::invalid_state("paste-clipboard requires an attached primary client")
        })?;
    match service.paste_clipboard_or_most_recent_buffer_to_pane(&primary, &descriptor) {
        Ok(true) => Ok(format!(
            "target={}:paste=sent:source=clipboard-or-buffer",
            descriptor.pane_id
        )),
        Ok(false) => Ok(format!(
            "target={}:paste=not-sent:reason=clipboard-and-buffer-empty",
            descriptor.pane_id
        )),
        Err(err) if err.kind() == crate::error::MezErrorKind::NotFound => Ok(format!(
            "target={}:paste=not-sent:reason=pane-process-unavailable",
            descriptor.pane_id
        )),
        Err(err) => Err(err),
    }
}

/// Runs the runtime choose buffer command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_choose_buffer_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    if let Some(buffer_name) = runtime_positional_args(invocation).first() {
        let created = if service.paste_buffers().get(buffer_name).is_none() {
            service.paste_buffers_mut().set_with_origin(
                *buffer_name,
                "",
                Some("runtime:choose-buffer".to_string()),
            )?;
            true
        } else {
            false
        };
        service.set_active_paste_buffer(Some((*buffer_name).to_string()));
        return Ok(format!(
            "buffer={}:selected=true:copy_target=active:paste_source=active:created={} source=runtime",
            buffer_name, created
        ));
    }
    Ok(runtime_choose_buffer_display(
        service.paste_buffers().list(),
        service.active_paste_buffer(),
    ))
}

/// Runs the runtime create buffer command operation for this subsystem.
///
/// The command creates a named internal paste buffer without overwriting an
/// existing buffer unless `--replace` is provided. `--select` makes the buffer
/// active for later copy and paste operations.
pub(super) fn runtime_create_buffer_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let buffer_name = runtime_buffer_name(invocation)
        .ok_or_else(|| MezError::invalid_args("create-buffer requires a buffer name"))?;
    let content = runtime_flag_value(&invocation.args, "--content")
        .or_else(|| runtime_positional_args(invocation).get(1).copied())
        .unwrap_or("");
    let replace = invocation
        .args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-r" | "--replace"));
    let select = invocation.args.iter().any(|arg| arg == "--select");

    let existed = service.paste_buffers().get(buffer_name).is_some();
    let (created, replaced, bytes) = if existed && !replace {
        (
            false,
            false,
            service
                .paste_buffers()
                .get(buffer_name)
                .map(str::len)
                .unwrap_or(0),
        )
    } else {
        let created = if replace {
            service.paste_buffers_mut().set_with_origin(
                buffer_name,
                content,
                Some("runtime:create-buffer".to_string()),
            )?;
            !existed
        } else {
            service.paste_buffers_mut().create_with_origin(
                buffer_name,
                content,
                Some("runtime:create-buffer".to_string()),
            )?
        };
        (created, existed && replace, content.len())
    };

    if select {
        service.set_active_paste_buffer(Some(buffer_name.to_string()));
    }

    Ok(format!(
        "buffer={buffer_name}:created={created}:replaced={replaced}:exists={}:bytes={bytes}:selected={select} source=runtime",
        existed && !created
    ))
}

/// Runs the runtime choose buffer display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_choose_buffer_display(
    buffers: Vec<PasteBuffer>,
    active: Option<&str>,
) -> String {
    if buffers.is_empty() {
        return "buffers=0 chooser=empty source=runtime".to_string();
    }
    let lines = buffers
        .iter()
        .map(|buffer| {
            let origin = buffer.origin.as_deref().unwrap_or("unknown");
            format!(
                "buffer={}:bytes={}:origin={}:preview={}:actions=paste-buffer -b {},delete-buffer {}",
                buffer.name,
                buffer.bytes,
                json_escape(origin),
                json_escape(&buffer.preview),
                buffer.name,
                buffer.name
            )
        })
        .collect::<Vec<_>>();
    format!(
        "buffers={} chooser=select-by-command active={} source=runtime\n{}",
        buffers.len(),
        active.unwrap_or("none"),
        lines.join("\n")
    )
}

/// Runs the runtime paste bytes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_paste_bytes(screen: Option<&TerminalScreen>, content: &str) -> Vec<u8> {
    if screen.is_some_and(TerminalScreen::bracketed_paste_enabled) {
        let mut bytes = Vec::with_capacity(content.len().saturating_add(12));
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(content.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        content.as_bytes().to_vec()
    }
}

/// Runs the runtime list buffers display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_list_buffers_display(buffers: Vec<PasteBuffer>) -> String {
    let mut lines = vec![
        "| buffer | bytes | created at | origin | preview |".to_string(),
        "| --- | ---: | ---: | --- | --- |".to_string(),
    ];
    if buffers.is_empty() {
        lines.push("| — no buffers — | 0 | — | — | — |".to_string());
        return lines.join("\n");
    }
    lines.extend(buffers.iter().map(|buffer| {
        let origin = buffer.origin.as_deref().unwrap_or("unknown");
        format!(
            "| {} | {} | {} | {} | {} |",
            markdown_buffer_table_cell(&buffer.name),
            buffer.bytes,
            buffer.created_at_unix_seconds,
            markdown_buffer_table_cell(origin),
            markdown_buffer_table_cell(&buffer.preview)
        )
    }));
    lines.join("\n")
}

/// Escapes one paste-buffer metadata value for a Markdown table cell.
fn markdown_buffer_table_cell(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies live paste buffers render as an ordered Markdown table with
    /// every available metadata field and escaped table-cell delimiters.
    #[test]
    fn runtime_list_buffers_renders_metadata_table_in_store_order() {
        let body = runtime_list_buffers_display(vec![
            PasteBuffer {
                name: "alpha".to_string(),
                bytes: 5,
                created_at_unix_seconds: 11,
                origin: Some("copy|mode".to_string()),
                preview: "first|line".to_string(),
            },
            PasteBuffer {
                name: "beta".to_string(),
                bytes: 4,
                created_at_unix_seconds: 22,
                origin: None,
                preview: "second".to_string(),
            },
        ]);

        assert_eq!(
            body,
            "| buffer | bytes | created at | origin | preview |\n\
             | --- | ---: | ---: | --- | --- |\n\
             | alpha | 5 | 11 | copy\\|mode | first\\|line |\n\
             | beta | 4 | 22 | unknown | second |"
        );
    }

    /// Verifies an empty live paste-buffer store retains table structure so
    /// the pager renders a clear empty row instead of legacy key-value text.
    #[test]
    fn runtime_list_buffers_renders_empty_table() {
        assert_eq!(
            runtime_list_buffers_display(Vec::new()),
            "| buffer | bytes | created at | origin | preview |\n\
             | --- | ---: | ---: | --- | --- |\n\
             | — no buffers — | 0 | — | — | — |"
        );
    }
}
