//! Client state serialization.

use super::approvals::optional_rfc3339_timestamp_json;
use super::snapshots::{client_role_name, client_state_name};
use super::{
    ClientRole, ClientState, ClientTerminalDescriptor, DEFAULT_PANE_TERM, Session, json_escape,
    string_array_json,
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
    let navigation_revision = client
        .navigation
        .as_ref()
        .map(|navigation| navigation.revision.to_string())
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#"{{"id":"{}","version":2,"client_id":"{}","name":"{}","role":"{}","requested_role":"{}","state":"{}","attached_at":{},"last_seen_at":{},"descriptor":{{"name":"{}","interactive":{},"terminal":{}}},"terminal_size":{},"interactive":{},"navigation_revision":{}}}"#,
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
        client.interactive,
        navigation_revision
    )
}

/// Runs the client requested role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn client_requested_role_name(role: ClientRole) -> &'static str {
    client_role_name(role)
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
    let is_primary = session.is_attached_primary(&client.id);
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
