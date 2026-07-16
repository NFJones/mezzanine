//! Runtime paste buffer, copy, capture, and history command helpers.
//!
//! This module owns live-terminal helpers that coordinate paste-buffer state,
//! copy-mode operations, pane capture, history search/export, and paste byte
//! preparation for the runtime command-support boundary.

use super::{
    CommandInvocation, CopyMode, MezError, PasteBuffer, Result, RuntimeSessionService,
    TerminalScreen, json_escape, runtime_flag_value, runtime_positional_args,
};

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
        service.active_copy_modes_mut().remove(pane_id.as_str());
        return Ok(());
    }
    if !service.active_copy_modes().contains_key(pane_id.as_str()) {
        let screen = service.pane_screen(pane_id.as_str()).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane screen not found",
            )
        })?;
        let viewport_rows = service.copy_mode_viewport_rows_for_pane(pane_id.as_str());
        let copy_mode = CopyMode::from_screen(screen, viewport_rows)?;
        service
            .active_copy_modes_mut()
            .insert(pane_id.clone(), copy_mode);
    }
    let copy_target_buffer = invocation
        .args
        .iter()
        .any(|arg| arg == "--copy")
        .then(|| runtime_copy_target_buffer_name(service, invocation));
    let mut copied = None;
    {
        let copy_mode = service
            .active_copy_modes_mut()
            .get_mut(pane_id.as_str())
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
    let Some(copy_mode) = service.active_copy_modes().get(pane_id.as_str()) else {
        return Ok(format!(
            "target={pane_id}:copy=not-copied:reason=copy-mode-inactive"
        ));
    };
    let copied = copy_mode.copy_selection()?;
    let bytes = copied.len();
    service.copy_text_to_buffer_and_host_clipboard(
        buffer_name.as_str(),
        copied,
        format!("pane:{pane_id}:copy-mode"),
    )?;
    if invocation.has_flag("-x", "--exit") {
        service.active_copy_modes_mut().remove(pane_id.as_str());
    }
    Ok(format!(
        "target={pane_id}:copy=copied:buffer={buffer_name}:bytes={bytes}"
    ))
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
        .primary_client_id()
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
    if buffers.is_empty() {
        return "buffers=0 source=runtime status=empty".to_string();
    }
    let lines = buffers
        .iter()
        .map(|buffer| {
            let origin = buffer.origin.as_deref().unwrap_or("unknown");
            format!(
                "buffer={}:bytes={}:created_at={}:origin={}:preview={}",
                buffer.name,
                buffer.bytes,
                buffer.created_at_unix_seconds,
                json_escape(origin),
                json_escape(&buffer.preview)
            )
        })
        .collect::<Vec<_>>();
    format!(
        "buffers={} source=runtime\n{}",
        buffers.len(),
        lines.join("\n")
    )
}
