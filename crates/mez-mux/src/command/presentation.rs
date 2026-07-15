//! Session command presentation.
//!
//! This module renders dependency-neutral session, group, window, pane,
//! client, and observer command output. It does not dispatch commands or read
//! product stores; callers wrap these strings in their own command outcomes.

use crate::session::{
    Client, ClientRole, ClientState, ObserverDecisionState, ObserverRequest, Session, SessionState,
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

/// Renders attached and observer clients as compact state rows.
pub fn list_clients(session: &Session) -> String {
    session
        .clients()
        .iter()
        .map(|client| {
            let observer = observer_for_client(session, client);
            format!(
                "{}:{}:role={}:state={}:interactive={}:attached_at={}:last_seen_at={}:terminal={}:approval={}",
                client.id,
                client.name,
                client_role_name(client.role),
                client_state_name(client.state),
                client.interactive,
                optional_unix_seconds(client.attached_at_unix_seconds),
                optional_unix_seconds(client.last_seen_at_unix_seconds),
                client_terminal_display(session, client, observer),
                client_approval_display(observer)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
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
            let observer = observer_for_client(session, client);
            format!(
                "client={}:name={}:role={}:state={}:interactive={}:approval={}:action=detach-client -t {}",
                client.id,
                escaped(&client.name),
                client_role_name(client.role),
                client_state_name(client.state),
                client.interactive,
                client_approval_display(observer),
                client.id
            )
        })
        .collect::<Vec<_>>();
    format!(
        "clients={}:observers={}:chooser=detach-client:source=session\n{}",
        clients.len(),
        session.observers().len(),
        lines.join("\n")
    )
}

/// Renders observer requests and their decision metadata.
pub fn list_observers(session: &Session) -> String {
    session
        .observers()
        .iter()
        .map(|observer| {
            format!(
                "{}:client={}:state={}:requested_at={}:decided_at={}:decided_by={}:visible_from={}:visible_from_time={}:descriptor={}:interactive={}:terminal={}:reason={}",
                observer.id,
                observer.client_id,
                observer_state_name(observer.state),
                optional_unix_seconds(observer.requested_at_unix_seconds),
                optional_unix_seconds(observer.decided_at_unix_seconds),
                observer
                    .decided_by_client_id
                    .as_deref()
                    .map(escaped)
                    .unwrap_or_else(|| "none".to_string()),
                observer
                    .visible_from_event_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "none".to_string()),
                optional_unix_seconds(observer.visible_from_unix_seconds),
                escaped(&observer.descriptor_name),
                observer.descriptor_interactive,
                observer_terminal_display(observer),
                observer
                    .reason
                    .as_deref()
                    .map(escaped)
                    .unwrap_or_else(|| "none".to_string())
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders observer requests with concrete approval, rejection, and revoke actions.
pub fn choose_observer_display(session: &Session) -> String {
    if session.observers().is_empty() {
        return "observers=0 actions=none".to_string();
    }
    session
        .observers()
        .iter()
        .map(|observer| {
            format!(
                "{}:client={}:state={}:requested_at={}:terminal={}:actions={}:commands={}",
                observer.id,
                observer.client_id,
                observer_state_name(observer.state),
                optional_unix_seconds(observer.requested_at_unix_seconds),
                observer_terminal_display(observer),
                observer_actions(observer.state),
                observer_action_commands(observer)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders the current session as the single local list-sessions row.
pub fn list_current_session(session: &Session) -> String {
    let attached_clients = session
        .clients()
        .iter()
        .filter(|client| client.state == ClientState::Attached)
        .count();
    format!(
        "{}:{}:state={}:created_at={}:last_attached_at={}:windows={}:clients={}:attached_clients={}:primary_available={}",
        session.id,
        session.name,
        session_state_name(session.state),
        session.created_at_unix_seconds,
        optional_unix_seconds(session.last_attached_at_unix_seconds),
        session.windows().len(),
        session.clients().len(),
        attached_clients,
        session.primary_client_id().is_none()
    )
}

/// Renders the local attach-session result for an already attached session.
pub fn attach_session_display(session: &Session) -> String {
    format!(
        "{}:attach=already-attached:role=primary:state={}",
        session.id,
        session_state_name(session.state)
    )
}

fn observer_for_client<'a>(session: &'a Session, client: &Client) -> Option<&'a ObserverRequest> {
    session
        .observers()
        .iter()
        .find(|observer| observer.client_id == client.id)
}

fn client_terminal_display(
    session: &Session,
    client: &Client,
    observer: Option<&ObserverRequest>,
) -> String {
    if session
        .primary_client_id()
        .is_some_and(|primary| primary == &client.id)
    {
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
    if let Some(terminal) = observer.and_then(|observer| observer.descriptor_terminal.as_ref()) {
        return format!(
            "{}x{}:term={}",
            terminal.columns,
            terminal.rows,
            escaped(&terminal.term)
        );
    }
    "none".to_string()
}

fn client_approval_display(observer: Option<&ObserverRequest>) -> String {
    observer
        .map(|observer| format!("{}:{}", observer.id, observer_state_name(observer.state)))
        .unwrap_or_else(|| "none".to_string())
}

fn observer_terminal_display(observer: &ObserverRequest) -> String {
    observer
        .descriptor_terminal
        .as_ref()
        .map(|terminal| {
            format!(
                "{}x{}:term={}",
                terminal.columns,
                terminal.rows,
                escaped(&terminal.term)
            )
        })
        .unwrap_or_else(|| "none".to_string())
}

fn observer_actions(state: ObserverDecisionState) -> &'static str {
    match state {
        ObserverDecisionState::Pending => "inspect,approve,reject",
        ObserverDecisionState::Approved => "inspect,revoke,detach",
        ObserverDecisionState::Rejected | ObserverDecisionState::Revoked => "inspect",
    }
}

fn observer_action_commands(observer: &ObserverRequest) -> String {
    match observer.state {
        ObserverDecisionState::Pending => format!(
            "approve-observer -t {}|reject-observer -t {}",
            observer.id, observer.id
        ),
        ObserverDecisionState::Approved => format!(
            "revoke-observer -t {}|detach-client -t {}",
            observer.client_id, observer.client_id
        ),
        ObserverDecisionState::Rejected | ObserverDecisionState::Revoked => "none".to_string(),
    }
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
        ClientRole::PendingObserver => "pending_observer",
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

fn observer_state_name(state: ObserverDecisionState) -> &'static str {
    match state {
        ObserverDecisionState::Pending => "pending",
        ObserverDecisionState::Approved => "approved",
        ObserverDecisionState::Rejected => "rejected",
        ObserverDecisionState::Revoked => "revoked",
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
    /// metadata, observer actions, and concrete chooser commands together.
    #[test]
    fn renders_session_state_and_chooser_actions() {
        let (mut session, _primary) = test_session();
        let (observer_client, observer_request) = session.request_observer("observer");

        assert!(list_windows(&session).contains("active=true:panes=1:size=80x24"));
        assert!(choose_group_display(&session).contains("action=select-group -t"));
        assert!(
            display_panes(&session)
                .unwrap()
                .contains("action=select-pane -t 0")
        );
        assert!(list_clients(&session).contains("role=primary:state=attached"));
        assert!(
            choose_client_display(&session)
                .contains(&format!("action=detach-client -t {observer_client}"))
        );
        assert!(choose_observer_display(&session).contains(&format!(
            "approve-observer -t {observer_request}|reject-observer -t {observer_request}"
        )));
        assert!(list_current_session(&session).contains("attached_clients=1"));
        assert!(attach_session_display(&session).contains("attach=already-attached"));
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
