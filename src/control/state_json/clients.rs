//! Client and observer state serialization.

use super::approvals::optional_rfc3339_timestamp_json;
use super::mcp::observer_json_by_ref;
use super::snapshots::{client_role_name, client_state_name};
use super::{
    ClientRole, ClientState, ClientTerminalDescriptor, DEFAULT_PANE_TERM, ObserverDecisionState,
    Session, json_escape, string_array_json,
};
/// Runs the clients json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn clients_json(session: &Session) -> String {
    let clients = session
        .clients()
        .iter()
        .map(|client| client_json(session, client))
        .collect::<Vec<_>>();
    format!("[{}]", clients.join(","))
}

/// Runs the client json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::control) fn client_json(
    session: &Session,
    client: &mez_mux::session::Client,
) -> String {
    let terminal_descriptor = generic_client_terminal_descriptor(session, client);
    let terminal_size = terminal_descriptor
        .as_ref()
        .map(|terminal| mez_mux::layout::Size {
            columns: terminal.columns,
            rows: terminal.rows,
        });
    format!(
        r#"{{"id":"{}","version":1,"client_id":"{}","name":"{}","role":"{}","requested_role":"{}","state":"{}","attached_at":{},"last_seen_at":{},"descriptor":{{"name":"{}","interactive":{},"terminal":{}}},"terminal_size":{},"interactive":{}}}"#,
        json_escape(&client.id.to_string()),
        json_escape(&client.id.to_string()),
        json_escape(&client.name),
        client_role_name(client.role),
        client_requested_role_name(client.role),
        client_state_name(client.state),
        optional_rfc3339_timestamp_json(client.attached_at_unix_seconds),
        optional_rfc3339_timestamp_json(client.last_seen_at_unix_seconds),
        json_escape(&client.name),
        client.interactive,
        generic_client_terminal_descriptor_json(terminal_descriptor.as_ref()),
        generic_size_object_json(terminal_size),
        client.interactive
    )
}

/// Runs the client requested role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn client_requested_role_name(role: ClientRole) -> &'static str {
    match role {
        ClientRole::PendingObserver => "observer",
        _ => client_role_name(role),
    }
}

/// Runs the generic client terminal descriptor operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn generic_client_terminal_descriptor(
    session: &Session,
    client: &mez_mux::session::Client,
) -> Option<ClientTerminalDescriptor> {
    if let Some(terminal) = client.terminal.as_ref() {
        return Some(terminal.clone());
    }
    let is_primary = session
        .primary_client_id()
        .is_some_and(|primary| primary == &client.id);
    (is_primary && client.interactive && client.state == ClientState::Attached).then(|| {
        ClientTerminalDescriptor {
            columns: session.authoritative_size.columns,
            rows: session.authoritative_size.rows,
            term: DEFAULT_PANE_TERM.to_string(),
            features: Vec::new(),
        }
    })
}

/// Runs the generic size object json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn generic_size_object_json(size: Option<mez_mux::layout::Size>) -> String {
    size.map(|size| format!(r#"{{"columns":{},"rows":{}}}"#, size.columns, size.rows))
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the generic client terminal descriptor json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn generic_client_terminal_descriptor_json(
    terminal: Option<&ClientTerminalDescriptor>,
) -> String {
    terminal
        .map(generic_client_terminal_descriptor_object_json)
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the generic client terminal descriptor object json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn generic_client_terminal_descriptor_object_json(
    terminal: &ClientTerminalDescriptor,
) -> String {
    if terminal.features.is_empty() {
        format!(
            r#"{{"columns":{},"rows":{},"term":"{}"}}"#,
            terminal.columns,
            terminal.rows,
            json_escape(&terminal.term)
        )
    } else {
        format!(
            r#"{{"columns":{},"rows":{},"term":"{}","features":{}}}"#,
            terminal.columns,
            terminal.rows,
            json_escape(&terminal.term),
            string_array_json(&terminal.features)
        )
    }
}

/// Runs the observers json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn observers_json(session: &Session) -> String {
    observers_json_for_state(session, None)
}

/// Runs the observers json for state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn observers_json_for_state(
    session: &Session,
    state: Option<ObserverDecisionState>,
) -> String {
    let observers = session
        .observers()
        .iter()
        .filter(|observer| state.is_none_or(|state| observer.state == state))
        .map(observer_json_by_ref)
        .collect::<Vec<_>>();
    format!("[{}]", observers.join(","))
}
