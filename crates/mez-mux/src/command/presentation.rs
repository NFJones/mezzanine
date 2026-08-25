//! Session command presentation.
//!
//! This module renders dependency-neutral session, group, window, pane,
//! client, and observer command output. It does not dispatch commands or read
//! product stores; callers wrap these strings in their own command outcomes.

use crate::session::{
    ClientRole, ClientState, MAX_ATTACHED_PRIMARY_CLIENTS, Session, SessionState,
};
use crate::{MuxError, Result};

/// Renders windows in the active group as compact state rows.
pub fn list_windows(session: &Session) -> String {
    session
        .active_group_windows()
        .iter()
        .enumerate()
        .map(|(index, window)| {
            format!(
                "{}:{}:{}:active={}:panes={}:size={}x{}",
                index,
                window.id,
                window.name,
                session
                    .active_window()
                    .is_some_and(|active| active.id == window.id),
                window.panes().len(),
                window.size.columns,
                window.size.rows
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders windows with concrete select-window actions.
pub fn choose_window_display(session: &Session) -> String {
    let windows = session.active_group_windows();
    if windows.is_empty() {
        return "windows=0 chooser=empty source=session".to_string();
    }
    let lines = windows
        .iter()
        .enumerate()
        .map(|(index, window)| {
            format!(
                "window={}:index={}:name={}:active={}:panes={}:size={}x{}:action=select-window -t {}",
                window.id,
                index,
                escaped(&window.name),
                session
                    .active_window()
                    .is_some_and(|active| active.id == window.id),
                window.panes().len(),
                window.size.columns,
                window.size.rows,
                window.id
            )
        })
        .collect::<Vec<_>>();
    format!(
        "windows={}:chooser=select-window:source=session\n{}",
        windows.len(),
        lines.join("\n")
    )
}

/// Renders ordered window groups as compact state rows.
pub fn list_groups(session: &Session) -> String {
    session
        .window_groups()
        .iter()
        .map(|group| {
            format!(
                "{}:{}:{}:active={}:windows={}",
                group.index,
                group.id,
                escaped(&group.name),
                session
                    .active_group()
                    .is_some_and(|active| active.id == group.id),
                group.window_ids.len()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders groups with concrete select-group actions.
pub fn choose_group_display(session: &Session) -> String {
    let groups = session.window_groups();
    if groups.is_empty() {
        return "groups=0 chooser=empty source=session".to_string();
    }
    let lines = groups
        .iter()
        .map(|group| {
            format!(
                "group={}:index={}:name={}:active={}:windows={}:action=select-group -t {}",
                group.id,
                group.index,
                escaped(&group.name),
                session
                    .active_group()
                    .is_some_and(|active| active.id == group.id),
                group.window_ids.len(),
                group.id
            )
        })
        .collect::<Vec<_>>();
    format!(
        "groups={}:chooser=select-group:source=session\n{}",
        groups.len(),
        lines.join("\n")
    )
}

/// Renders panes in the active window as compact state rows.
pub fn list_panes(session: &Session) -> Result<String> {
    let window = session
        .active_window()
        .ok_or_else(|| MuxError::invalid_state("session has no active window"))?;
    Ok(window
        .panes()
        .iter()
        .map(|pane| {
            format!(
                "{}:{}:{}:active={}:primary_pid=none:size={}x{}:agent_id=none:live={}",
                pane.index,
                pane.id,
                pane.title,
                pane.active,
                pane.size.columns,
                pane.size.rows,
                pane.live
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Renders panes in the active window with concrete select-pane actions.
pub fn display_panes(session: &Session) -> Result<String> {
    let window = session
        .active_window()
        .ok_or_else(|| MuxError::invalid_state("session has no active window"))?;
    let mut body = String::new();
    for pane in window.panes() {
        body.push_str(&format!(
            "{}:{}:action=select-pane -t {}\n",
            pane.index, pane.id, pane.index
        ));
    }
    Ok(body)
}

/// Renders attached and observer clients as a pager-friendly Markdown table.
pub fn list_clients(session: &Session) -> String {
    let mut lines = vec![
        "| client | name | role | state | interactive | attached at | last seen at | terminal |"
            .to_string(),
        "| --- | --- | --- | --- | --- | --- | --- | --- |".to_string(),
    ];
    if session.clients().is_empty() {
        lines.push("| — | no clients | — | — | — | — | — | — |".to_string());
    } else {
        lines.extend(session.clients().iter().map(|client| {
            format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                markdown_table_cell(&client.id.to_string()),
                markdown_table_cell(&client.name),
                markdown_table_cell(client_role_name(client.role)),
                markdown_table_cell(client_state_name(client.state)),
                client.interactive,
                markdown_table_cell(&optional_unix_seconds(client.attached_at_unix_seconds)),
                markdown_table_cell(&optional_unix_seconds(client.last_seen_at_unix_seconds)),
                markdown_table_cell(&client_terminal_display(session, client))
            )
        }));
    }
    lines.join("\n")
}

/// Renders clients with concrete detach-client actions.
pub fn choose_client_display(session: &Session) -> String {
    let clients = session.clients();
    if clients.is_empty() {
        return "clients=0 observers=0 chooser=empty source=session".to_string();
    }
    let lines = clients
        .iter()
        .map(|client| {
            format!(
                "client={}:name={}:role={}:state={}:interactive={}:action=detach-client -t {}",
                client.id,
                escaped(&client.name),
                client_role_name(client.role),
                client_state_name(client.state),
                client.interactive,
                client.id
            )
        })
        .collect::<Vec<_>>();
    let observer_count = clients
        .iter()
        .filter(|client| client.role == ClientRole::Observer)
        .count();
    format!(
        "clients={}:observers={}:chooser=detach-client:source=session\n{}",
        clients.len(),
        observer_count,
        lines.join("\n")
    )
}

/// Renders the current session as a single-row pager-friendly Markdown table.
pub fn list_current_session(session: &Session) -> String {
    let attached_clients = session
        .clients()
        .iter()
        .filter(|client| client.state == ClientState::Attached)
        .count();
    let attached_primaries = session.attached_primaries().count();
    [
        "| session | name | state | created at | last attached at | windows | clients | attached clients | attached primaries | max attached primaries | accepts primary | layout owner |",
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |",
        &format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            markdown_table_cell(&session.id.to_string()),
            markdown_table_cell(&session.name),
            markdown_table_cell(session_state_name(session.state)),
            session.created_at_unix_seconds,
            markdown_table_cell(&optional_unix_seconds(session.last_attached_at_unix_seconds)),
            session.windows().len(),
            session.clients().len(),
            attached_clients,
            attached_primaries,
            MAX_ATTACHED_PRIMARY_CLIENTS,
            attached_primaries < MAX_ATTACHED_PRIMARY_CLIENTS,
            markdown_table_cell(
                &session
                    .layout_owner_client_id()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "none".to_string()),
            )
        ),
    ]
    .join("\n")
}

/// Escapes a value for a Markdown table cell without changing its meaning.
fn markdown_table_cell(value: &str) -> String {
    value.replace('|', r"\|").replace('\n', "<br>")
}

/// Renders the local attach-session result for an already attached session.
pub fn attach_session_display(session: &Session) -> String {
    format!(
        "{}:attach=already-attached:role=primary:state={}",
        session.id,
        session_state_name(session.state)
    )
}

fn client_terminal_display(session: &Session, client: &crate::session::Client) -> String {
    if session.is_attached_primary(&client.id) && client.terminal.is_none() {
        return format!(
            "{}x{}:term={}",
            session.authoritative_size.columns,
            session.authoritative_size.rows,
            mez_terminal::DEFAULT_PANE_TERM
        );
    }
    if let Some(terminal) = client.terminal.as_ref() {
        return format!(
            "{}x{}:term={}",
            terminal.columns,
            terminal.rows,
            escaped(&terminal.term)
        );
    }
    "none".to_string()
}

fn optional_unix_seconds(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "none".to_string())
}

fn escaped(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn client_role_name(role: ClientRole) -> &'static str {
    match role {
        ClientRole::Primary => "primary",
        ClientRole::Observer => "observer",
        ClientRole::Agent => "agent",
        ClientRole::Automation => "automation",
    }
}

fn client_state_name(state: ClientState) -> &'static str {
    match state {
        ClientState::Attached => "attached",
        ClientState::Pending => "pending",
        ClientState::Detached => "detached",
        ClientState::Revoked => "revoked",
        ClientState::Failed => "failed",
    }
}

fn session_state_name(state: SessionState) -> &'static str {
    match state {
        SessionState::Running => "running",
        SessionState::Detached => "detached",
        SessionState::Empty => "empty",
        SessionState::Stopping => "stopping",
        SessionState::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Size;
    use crate::session::SessionShell;
    use std::path::PathBuf;

    fn test_session() -> (Session, mez_core::ids::ClientId) {
        let mut session = Session::new_default(
            SessionShell::new(PathBuf::from("/bin/sh"), "fallback-bin-sh", true),
            Size::new(80, 24).unwrap(),
        );
        let primary = session.attach_primary("primary", true).unwrap();
        (session, primary)
    }

    /// Verifies lower command presentation renders session topology, client
    /// metadata, and concrete generic client actions together.
    #[test]
    fn renders_session_state_and_chooser_actions() {
        let (mut session, _primary) = test_session();
        let observer_client = session
            .attach_observer_with_terminal("observer", None, 1)
            .unwrap();

        assert!(list_windows(&session).contains("active=true:panes=1:size=80x24"));
        assert!(choose_group_display(&session).contains("action=select-group -t"));
        assert!(
            display_panes(&session)
                .unwrap()
                .contains("action=select-pane -t 0")
        );
        let clients = list_clients(&session);
        assert!(
            clients.starts_with("| client | name | role | state |"),
            "{clients}"
        );
        assert!(
            clients.contains("| c1 | primary | primary | attached |"),
            "{clients}"
        );
        assert!(
            choose_client_display(&session)
                .contains(&format!("action=detach-client -t {observer_client}"))
        );
        let session_row = list_current_session(&session);
        assert!(
            session_row.starts_with("| session | name | state |"),
            "{session_row}"
        );
        assert!(
            session_row.contains("| 1 | 2 | 2 | 1 | 16 | true | c1 |"),
            "{session_row}"
        );
        assert!(attach_session_display(&session).contains("attach=already-attached"));
    }

    /// Verifies an empty client list retains table structure and a readable
    /// empty row so pager rendering does not collapse into unstructured text.
    #[test]
    fn list_clients_renders_an_empty_table_row() {
        let session = Session::new_default(
            SessionShell::new(PathBuf::from("/bin/sh"), "fallback-bin-sh", true),
            Size::new(80, 24).unwrap(),
        );

        assert_eq!(
            list_clients(&session),
            "| client | name | role | state | interactive | attached at | last seen at | terminal |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n| — | no clients | — | — | — | — | — | — |"
        );
    }

    /// Verifies pane renderers report a mux invalid-state error after session
    /// shutdown removes every window instead of producing misleading output.
    #[test]
    fn pane_presentations_require_an_active_window() {
        let (mut session, primary) = test_session();
        session.kill_session(&primary, true).unwrap();

        let list_error = list_panes(&session).unwrap_err();
        let display_error = display_panes(&session).unwrap_err();
        assert_eq!(list_error.kind(), crate::MuxErrorKind::InvalidState);
        assert_eq!(display_error.kind(), crate::MuxErrorKind::InvalidState);
    }
}
