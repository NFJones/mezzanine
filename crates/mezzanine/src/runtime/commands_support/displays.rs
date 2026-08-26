//! Runtime pane, window, group, client, and observer display helpers.
//!
//! This module owns live-terminal command display formatting for runtime
//! session topology and observer/client state so the command-support parent can
//! focus on dispatch and mutation orchestration.

use super::{MezError, Result, RuntimeSessionService, json_escape};

/// Runs the runtime list panes display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_list_panes_display(service: &RuntimeSessionService) -> Result<String> {
    let window = service
        .session
        .active_window()
        .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
    let mut lines = vec![
        "| index | pane | title | active | primary pid | size | agent id | live |".to_string(),
        "| --- | --- | --- | --- | --- | --- | --- | --- |".to_string(),
    ];
    lines.extend(window.panes().iter().map(|pane| {
        let primary_pid = service
            .primary_pid_for_live_pane_process(pane.id.as_str())
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "none".to_string());
        let agent_id = service
            .agent_shell_store()
            .get(pane.id.as_str())
            .map(|_| format!("agent-{}", pane.id))
            .unwrap_or_else(|| "none".to_string());
        format!(
            "| {} | {} | {} | {} | {} | {}x{} | {} | {} |",
            pane.index,
            runtime_markdown_table_cell(pane.id.as_str()),
            runtime_markdown_table_cell(&pane.title),
            pane.active,
            primary_pid,
            pane.size.columns,
            pane.size.rows,
            agent_id,
            pane.live
        )
    }));
    Ok(lines.join("\n"))
}

/// Runs the runtime display panes display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_display_panes_display(service: &RuntimeSessionService) -> Result<String> {
    let window = service
        .session
        .active_window()
        .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
    let lines = window
        .panes()
        .iter()
        .map(|pane| {
            format!(
                "pane={}:index={}:label={}:active={}:title={}:size={}x{}:action=select-pane -t {}",
                pane.id,
                pane.index,
                pane.index,
                pane.active,
                json_escape(&pane.title),
                pane.size.columns,
                pane.size.rows,
                pane.id
            )
        })
        .collect::<Vec<_>>();
    Ok(format!(
        "panes={} window={} chooser=select-pane-index source=runtime\n{}",
        lines.len(),
        window.id,
        lines.join("\n")
    ))
}

/// Runs the runtime choose client display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_choose_client_display(service: &RuntimeSessionService) -> String {
    let clients = service.session.clients();
    if clients.is_empty() {
        return "clients=0 observers=0 chooser=empty source=runtime".to_string();
    }
    let observer_count = clients
        .iter()
        .filter(|client| client.role == mez_mux::session::ClientRole::Observer)
        .count();
    let lines = clients
        .iter()
        .map(|client| {
            format!(
                "client={}:name={}:role={}:state={}:interactive={}:action=detach-client -t {}",
                client.id,
                json_escape(&client.name),
                runtime_client_role_name(client.role),
                runtime_client_state_name(client.state),
                client.interactive,
                client.id
            )
        })
        .collect::<Vec<_>>();
    format!(
        "clients={}:observers={}:chooser=detach-client:source=runtime\n{}",
        clients.len(),
        observer_count,
        lines.join("\n")
    )
}

/// Runs the runtime list clients display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_list_clients_display(service: &RuntimeSessionService) -> String {
    let clients = service.session.clients();
    let mut lines = vec![
        "| client | name | role | state | interactive | attached at | last seen at | terminal |"
            .to_string(),
        "| --- | --- | --- | --- | --- | --- | --- | --- |".to_string(),
    ];
    if clients.is_empty() {
        lines.push("| — | no clients | — | — | — | — | — | — |".to_string());
    } else {
        lines.extend(clients.iter().map(|client| {
            format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                runtime_markdown_table_cell(client.id.as_str()),
                runtime_markdown_table_cell(&client.name),
                runtime_markdown_table_cell(runtime_client_role_name(client.role)),
                runtime_markdown_table_cell(runtime_client_state_name(client.state)),
                client.interactive,
                runtime_markdown_table_cell(&runtime_optional_unix_seconds(
                    client_attached_at_for_display(service, client)
                )),
                runtime_markdown_table_cell(&runtime_optional_unix_seconds(
                    client_last_seen_at_for_display(service, client)
                )),
                runtime_markdown_table_cell(&runtime_client_terminal_display(service, client))
            )
        }));
    }
    lines.join("\n")
}

/// Runs the runtime choose window display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_choose_window_display(service: &RuntimeSessionService) -> String {
    let windows = service.session.active_group_windows();
    if windows.is_empty() {
        return "windows=0 chooser=empty source=runtime".to_string();
    }
    let active_id = service
        .session
        .active_window()
        .map(|window| window.id.to_string())
        .unwrap_or_else(|| "none".to_string());
    let lines = windows
        .iter()
        .enumerate()
        .map(|(index, window)| {
            format!(
                "window={}:index={}:name={}:active={}:panes={}:size={}x{}:action=select-window -t {}",
                window.id,
                index,
                json_escape(&window.name),
                window.id.to_string() == active_id,
                window.panes().len(),
                window.size.columns,
                window.size.rows,
                window.id
            )
        })
        .collect::<Vec<_>>();
    format!(
        "windows={}:active={active_id}:chooser=select-window:source=runtime\n{}",
        windows.len(),
        lines.join("\n")
    )
}

/// Returns runtime window group rows for `list-groups`.
pub(super) fn runtime_list_groups_display(service: &RuntimeSessionService) -> String {
    let mut lines = vec![
        "| index | group | name | active | windows |".to_string(),
        "| --- | --- | --- | --- | --- |".to_string(),
    ];
    lines.extend(service.session.window_groups().iter().map(|group| {
        format!(
            "| {} | {} | {} | {} | {} |",
            group.index,
            runtime_markdown_table_cell(group.id.as_str()),
            runtime_markdown_table_cell(&group.name),
            service
                .session
                .active_group()
                .is_some_and(|active| active.id == group.id),
            group.window_ids.len()
        )
    }));
    lines.join("\n")
}

/// Escapes one runtime topology field for a Markdown table cell.
fn runtime_markdown_table_cell(value: &str) -> String {
    value.replace('|', r"\|").replace('\n', "<br>")
}

/// Returns runtime group chooser rows with concrete selection actions.
pub(super) fn runtime_choose_group_display(service: &RuntimeSessionService) -> String {
    let groups = service.session.window_groups();
    if groups.is_empty() {
        return "groups=0:chooser=empty:source=runtime".to_string();
    }
    let lines = groups
        .iter()
        .map(|group| {
            format!(
                "group={}:index={}:name={}:active={}:windows={}:action=select-group -t {}",
                group.id,
                group.index,
                json_escape(&group.name),
                service
                    .session
                    .active_group()
                    .is_some_and(|active| active.id == group.id),
                group.window_ids.len(),
                group.id
            )
        })
        .collect::<Vec<_>>();
    format!(
        "groups={}:chooser=select-group:source=runtime\n{}",
        groups.len(),
        lines.join("\n")
    )
}

/// Runs the client attached at for display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn client_attached_at_for_display(
    service: &RuntimeSessionService,
    client: &mez_mux::session::Client,
) -> Option<u64> {
    if service
        .session
        .layout_owner_client_id()
        .is_some_and(|primary| primary == &client.id)
    {
        service
            .last_attach_at_unix_seconds()
            .or(client.attached_at_unix_seconds)
    } else {
        client.attached_at_unix_seconds
    }
}

/// Runs the client last seen at for display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn client_last_seen_at_for_display(
    service: &RuntimeSessionService,
    client: &mez_mux::session::Client,
) -> Option<u64> {
    if service
        .session
        .layout_owner_client_id()
        .is_some_and(|primary| primary == &client.id)
    {
        service
            .last_attach_at_unix_seconds()
            .or(client.last_seen_at_unix_seconds)
    } else {
        client.last_seen_at_unix_seconds
    }
}

/// Runs the runtime optional unix seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_optional_unix_seconds(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "none".to_string())
}

/// Runs the runtime client terminal display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_client_terminal_display(
    service: &RuntimeSessionService,
    client: &mez_mux::session::Client,
) -> String {
    if service
        .session
        .layout_owner_client_id()
        .is_some_and(|primary| primary == &client.id)
    {
        return format!(
            "{}x{}:term={}",
            service.session.authoritative_size.columns,
            service.session.authoritative_size.rows,
            json_escape(service.terminal_term())
        );
    }
    if let Some(terminal) = client.terminal.as_ref() {
        return format!(
            "{}x{}:term={}",
            terminal.columns,
            terminal.rows,
            json_escape(&terminal.term)
        );
    }
    "none".to_string()
}

/// Runs the runtime client role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_client_role_name(role: mez_mux::session::ClientRole) -> &'static str {
    match role {
        mez_mux::session::ClientRole::Primary => "primary",
        mez_mux::session::ClientRole::Observer => "observer",
        mez_mux::session::ClientRole::Agent => "agent",
        mez_mux::session::ClientRole::Automation => "automation",
    }
}

/// Runs the runtime client state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_client_state_name(state: mez_mux::session::ClientState) -> &'static str {
    match state {
        mez_mux::session::ClientState::Attached => "attached",
        mez_mux::session::ClientState::Pending => "pending",
        mez_mux::session::ClientState::Detached => "detached",
        mez_mux::session::ClientState::Revoked => "revoked",
        mez_mux::session::ClientState::Failed => "failed",
    }
}
