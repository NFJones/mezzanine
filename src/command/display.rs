//! Command Display implementation.
//!
//! This module owns the command display boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    CommandInvocation, CommandOutcome, ConfigMutationValue, KeyBindings, KeyChord, KeyCode,
    KeyValueLine, LayoutLoadSelector, MezError, Result, Session, baseline_commands,
    explicit_shell_command_flag, flag_value, mcp_server_id, positional_args,
    positional_args_before_double_dash, shell_command_after_double_dash, shell_command_from_words,
};

// Command display helpers and state renderers.

/// Runs the list windows operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn list_windows(session: &Session) -> String {
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

/// Runs the choose window display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn choose_window_display(session: &Session) -> String {
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
                json_escape(&window.name),
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

/// Returns the ordered window groups in the session.
pub(super) fn list_groups(session: &Session) -> String {
    session
        .window_groups()
        .iter()
        .map(|group| {
            format!(
                "{}:{}:{}:active={}:windows={}",
                group.index,
                group.id,
                json_escape(&group.name),
                session
                    .active_group()
                    .is_some_and(|active| active.id == group.id),
                group.window_ids.len()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Returns group chooser rows with concrete `select-group` actions.
pub(super) fn choose_group_display(session: &Session) -> String {
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
                json_escape(&group.name),
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

/// Runs the list panes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn list_panes(session: &Session) -> Result<String> {
    let window = session
        .active_window()
        .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
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

/// Runs the list clients operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn list_clients(session: &Session) -> String {
    session
        .clients()
        .iter()
        .map(|client| {
            let observer = session
                .observers()
                .iter()
                .find(|observer| observer.client_id == client.id);
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

/// Runs the choose client display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn choose_client_display(session: &Session) -> String {
    let clients = session.clients();
    if clients.is_empty() {
        return "clients=0 observers=0 chooser=empty source=session".to_string();
    }
    let lines = clients
        .iter()
        .map(|client| {
            let observer = session
                .observers()
                .iter()
                .find(|observer| observer.client_id == client.id);
            format!(
                "client={}:name={}:role={}:state={}:interactive={}:approval={}:action=detach-client -t {}",
                client.id,
                json_escape(&client.name),
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

/// Runs the client terminal display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn client_terminal_display(
    session: &Session,
    client: &crate::session::Client,
    observer: Option<&crate::session::ObserverRequest>,
) -> String {
    if session
        .primary_client_id()
        .is_some_and(|primary| primary == &client.id)
    {
        return format!(
            "{}x{}:term={}",
            session.authoritative_size.columns,
            session.authoritative_size.rows,
            crate::terminal::DEFAULT_PANE_TERM
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
    if let Some(terminal) = observer.and_then(|observer| observer.descriptor_terminal.as_ref()) {
        return format!(
            "{}x{}:term={}",
            terminal.columns,
            terminal.rows,
            json_escape(&terminal.term)
        );
    }
    "none".to_string()
}

/// Runs the client approval display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn client_approval_display(observer: Option<&crate::session::ObserverRequest>) -> String {
    observer
        .map(|observer| format!("{}:{}", observer.id, observer_state_name(observer.state)))
        .unwrap_or_else(|| "none".to_string())
}

/// Runs the list observers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn list_observers(session: &Session) -> String {
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
                    .map(json_escape)
                    .unwrap_or_else(|| "none".to_string()),
                observer
                    .visible_from_event_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "none".to_string()),
                optional_unix_seconds(observer.visible_from_unix_seconds),
                json_escape(&observer.descriptor_name),
                observer.descriptor_interactive,
                observer_terminal_display(observer),
                observer
                    .reason
                    .as_deref()
                    .map(json_escape)
                    .unwrap_or_else(|| "none".to_string())
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs the choose observer display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn choose_observer_display(session: &Session) -> String {
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

/// Runs the optional unix seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_unix_seconds(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "none".to_string())
}

/// Runs the observer terminal display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn observer_terminal_display(observer: &crate::session::ObserverRequest) -> String {
    observer
        .descriptor_terminal
        .as_ref()
        .map(|terminal| {
            format!(
                "{}x{}:term={}",
                terminal.columns,
                terminal.rows,
                json_escape(&terminal.term)
            )
        })
        .unwrap_or_else(|| "none".to_string())
}

/// Runs the json escape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn json_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Runs the observer actions operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn observer_actions(state: crate::session::ObserverDecisionState) -> &'static str {
    match state {
        crate::session::ObserverDecisionState::Pending => "inspect,approve,reject",
        crate::session::ObserverDecisionState::Approved => "inspect,revoke,detach",
        crate::session::ObserverDecisionState::Rejected => "inspect",
        crate::session::ObserverDecisionState::Revoked => "inspect",
    }
}

/// Runs the observer action commands operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn observer_action_commands(observer: &crate::session::ObserverRequest) -> String {
    match observer.state {
        crate::session::ObserverDecisionState::Pending => format!(
            "approve-observer -t {}|reject-observer -t {}",
            observer.id, observer.id
        ),
        crate::session::ObserverDecisionState::Approved => format!(
            "revoke-observer -t {}|detach-client -t {}",
            observer.client_id, observer.client_id
        ),
        crate::session::ObserverDecisionState::Rejected
        | crate::session::ObserverDecisionState::Revoked => "none".to_string(),
    }
}

/// Runs the list current session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn list_current_session(session: &Session) -> String {
    let attached_clients = session
        .clients()
        .iter()
        .filter(|client| client.state == crate::session::ClientState::Attached)
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

/// Runs the attach session display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn attach_session_display(session: &Session) -> String {
    format!(
        "{}:attach=already-attached:role=primary:state={}",
        session.id,
        session_state_name(session.state)
    )
}

/// Returns the user-facing command guide rendered by the in-pane `help`
/// command.
pub(super) fn command_help_display() -> String {
    command_help_display_with_key_bindings(&list_default_key_bindings())
}

/// Returns the user-facing command guide rendered by the in-pane `help`
/// command with a caller-supplied key binding table.
pub(crate) fn command_help_display_with_key_bindings(key_bindings: &str) -> String {
    let mut rows = terminal_help_command_rows();
    rows.sort_by(|left, right| {
        terminal_command_category(left.0)
            .cmp(terminal_command_category(right.0))
            .then_with(|| left.0.cmp(right.0))
    });
    let mut lines = vec![
        "# Mezzanine command help",
        "",
        "Commands entered through the Mezzanine command prompt run against the active session. Commands that produce output render that output into the active pane.",
        "",
        "| Category | Command | Description |",
        "| --- | --- | --- |",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    let mut current_category = "";
    for (name, description) in rows {
        let category = terminal_command_category(name);
        if category != current_category {
            lines.push(format!("| {} |  |  |", terminal_help_title_case(category)));
            current_category = category;
        }
        lines.push(format!("|  | `{name}` | {description} |"));
    }
    lines.push(String::new());
    lines.push("## Key bindings".to_string());
    lines.push(String::new());
    lines.push("```text".to_string());
    lines.extend(key_bindings.lines().map(str::to_string));
    lines.push("```".to_string());
    lines.join("\n")
}

/// Returns a display heading for one lower-case terminal help category.
fn terminal_help_title_case(category: &str) -> String {
    category
        .split_whitespace()
        .enumerate()
        .map(|(index, word)| {
            if index > 0 {
                return word.to_string();
            }
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Returns terminal command help rows before presentation sorting.
fn terminal_help_command_rows() -> Vec<(&'static str, &'static str)> {
    let mut rows = baseline_commands()
        .iter()
        .map(|command| (command.name, terminal_command_description(command.name)))
        .collect::<Vec<_>>();
    rows.push((
        "list-commands",
        "list every baseline command and support status.",
    ));
    rows
}

/// Returns the help category for one terminal command.
fn terminal_command_category(name: &str) -> &'static str {
    match name {
        "agent-shell" | "auth-status" | "mcp" | "mcp-status" | "refresh-provider-info" => {
            "agent and integrations"
        }
        "bind-key" | "list-keys" | "set-option" | "set-theme" | "show-options" | "source-file"
        | "unbind-key" | "list-themes" => "configuration",
        "capture-pane" | "choose-buffer" | "clear-history" | "copy-mode" | "copy-selection"
        | "create-buffer" | "delete-buffer" | "export-history" | "list-buffers"
        | "paste-buffer" | "paste-clipboard" | "pipe-pane" | "save-buffer" | "search-history" => {
            "copy, buffers, and history"
        }
        "help" | "list-commands" | "mark-pane-ready" | "refresh-client" | "show-messages"
        | "show-metrics" => "diagnostics and help",
        "approve-observer" | "attach-session" | "choose-observer" | "detach-client" | "exit"
        | "kill-session" | "list-clients" | "list-observers" | "list-sessions"
        | "reject-observer" | "rename-session" | "load-layout" | "revoke-observer"
        | "save-layout" => "sessions and clients",
        _ => "windows, groups, and panes",
    }
}

/// Returns the human-readable description for one terminal command.
fn terminal_command_description(name: &str) -> &'static str {
    match name {
        "agent-shell" => "toggle the pane-local agent shell.",
        "approve-observer" => "approve a pending observer.",
        "attach-session" => "attach to an existing session.",
        "auth-status" => "show non-secret auth status.",
        "bind-key" => "add or replace a live key binding.",
        "break-pane" => "move a pane into a new window.",
        "capture-pane" => "capture visible or historical pane text.",
        "choose-buffer" => "pick the active copy/paste buffer interactively.",
        "choose-group" => "pick a window group interactively.",
        "choose-observer" => "select observer actions interactively.",
        "clear-history" => "clear pane history.",
        "copy-mode" => "enter pane-local cursor selection mode.",
        "copy-selection" => "copy the active copy-mode selection to the active buffer.",
        "create-buffer" => "create an empty or seeded paste buffer.",
        "delete-buffer" => "delete a paste buffer.",
        "detach-client" => "detach a client without terminating the session.",
        "display-panes" => "show temporary pane labels for selection.",
        "exit" => "terminate the current session and exit Mezzanine.",
        "export-history" => "export bounded pane history.",
        "help" => "show this guide.",
        "join-pane" => "move a pane into another window or split.",
        "kill-group" => "close a window group and its windows.",
        "kill-pane" => "close a pane, requiring force or approval when needed.",
        "kill-session" => "terminate a session, requiring force or approval when needed.",
        "kill-window" => "close a window, requiring force or approval when needed.",
        "last-group" => "focus the previously active window group.",
        "last-pane" => "focus the previously active pane.",
        "last-window" => "focus the previously active window.",
        "list-buffers" => "show paste buffers.",
        "list-clients" => "show attached clients and pending observers.",
        "list-groups" => "show window group identities, names, and active state.",
        "list-keys" => "show effective key bindings.",
        "list-observers" => "show observer requests and approved observers.",
        "list-panes" => "show pane identities, active state, size, pid, and agent data.",
        "list-sessions" => "show resumable sessions.",
        "list-themes" => "show built-in and configured UI themes.",
        "list-windows" => "show window identities, names, active state, and sizes.",
        "mark-pane-ready" => "temporarily mark a pane as ready after risk acknowledgement.",
        "mcp" => "manage MCP servers, settings, tools, approvals, and retry.",
        "mcp-status" => "show non-secret MCP server auth status.",
        "new-group" => "create a window group with one landing window.",
        "new-window" => "create a window with one pane.",
        "next-group" => "focus the next window group.",
        "next-layout" => "select the next pane layout.",
        "next-pane" => "focus the next pane.",
        "next-window" => "focus the next window.",
        "paste-buffer" => "paste a named or recent paste buffer.",
        "paste-clipboard" => "paste host clipboard text into the active pane.",
        "pipe-pane" => "pipe future pane output to a file or command.",
        "previous-group" => "focus the previous window group.",
        "previous-pane" => "focus the previous pane.",
        "previous-window" => "focus the previous window.",
        "rebalance-window" => "reapply the active window layout.",
        "refresh-client" => "redraw the client.",
        "refresh-provider-info" => "refresh cached provider model and quota information.",
        "reject-observer" => "reject a pending observer.",
        "rename-group" => "rename a window group.",
        "rename-session" => "rename the current or target session.",
        "rename-window" => "rename a window.",
        "resize-pane" => "resize a pane.",
        "load-layout" => "resume a saved session or snapshot.",
        "revoke-observer" => "revoke an approved observer.",
        "rotate-pane" => "rotate panes in the active window.",
        "save-buffer" => "save a paste buffer.",
        "search-history" => "search pane history.",
        "select-group" => "focus a window group.",
        "select-layout" => "select a pane layout.",
        "select-pane" => "focus a pane.",
        "select-window" => "focus a window.",
        "set-option" => "set a live-mutable option.",
        "set-theme" => "switch active UI theme by name.",
        "show-messages" => "show diagnostics, pending approvals, and observer requests.",
        "show-metrics" => "show async runtime counters and histograms.",
        "show-options" => "show effective options.",
        "save-layout" => "create a structured session snapshot.",
        "source-file" => "load a configuration file.",
        "split-window" => "split the active or target pane.",
        "synchronize-panes" => "send primary input to every pane in the active window.",
        "swap-pane" => "exchange two panes.",
        "unbind-key" => "remove a live key binding.",
        "zoom-pane" => "toggle zoom for the active or target pane.",
        _ => "run the terminal command.",
    }
}

/// Runs the list baseline commands operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn list_baseline_commands() -> String {
    baseline_commands()
        .iter()
        .map(|command| format!("{}:status={}", command.name, command.status.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs the list default themes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn list_default_themes() -> String {
    crate::terminal::BUILTIN_UI_THEME_NAMES
        .iter()
        .map(|theme| {
            let preview_fields = crate::terminal::builtin_ui_theme_definition(theme)
                .map(|definition| {
                    let (preview, preview_colors) =
                        crate::terminal::ui_theme_preview_fields(&definition);
                    format!(":preview={preview}:preview_colors={preview_colors}")
                })
                .unwrap_or_default();
            format!(
                "theme={theme}:source=builtin:active={}{}:action=set-theme {theme}",
                *theme == crate::terminal::DEFAULT_UI_THEME_NAME,
                preview_fields,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs the mutated pane command outcome operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mutated_pane_command_outcome(
    invocation: &CommandInvocation,
    shell_command: Option<String>,
    start_directory: Option<String>,
) -> CommandOutcome {
    match shell_command {
        Some(shell_command) => CommandOutcome::MutatedWithPaneCommand {
            command: invocation.name.clone(),
            shell_command,
            start_directory,
        },
        None => CommandOutcome::Mutated {
            command: invocation.name.clone(),
        },
    }
}

/// Runs the new window name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn new_window_name(invocation: &CommandInvocation) -> String {
    flag_value(&invocation.args, "-n")
        .or_else(|| flag_value(&invocation.args, "--name"))
        .map(ToOwned::to_owned)
        .or_else(|| {
            if invocation.args.iter().any(|arg| arg == "--")
                || flag_value(&invocation.args, "--shell-command").is_some()
                || flag_value(&invocation.args, "--command").is_some()
            {
                None
            } else {
                positional_args_before_double_dash(invocation)
                    .first()
                    .map(|value| (*value).to_string())
            }
        })
        .unwrap_or_else(|| "shell".to_string())
}

/// Runs the new window shell command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn new_window_shell_command(invocation: &CommandInvocation) -> Result<Option<String>> {
    if let Some(command) = explicit_shell_command_flag(invocation)? {
        return Ok(Some(command));
    }
    if let Some(command) = shell_command_after_double_dash(invocation)? {
        return Ok(Some(command));
    }
    if flag_value(&invocation.args, "-n")
        .or_else(|| flag_value(&invocation.args, "--name"))
        .is_some()
    {
        return shell_command_from_words(positional_args_before_double_dash(invocation));
    }
    Ok(None)
}

/// Runs the split window shell command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn split_window_shell_command(invocation: &CommandInvocation) -> Result<Option<String>> {
    if let Some(command) = explicit_shell_command_flag(invocation)? {
        return Ok(Some(command));
    }
    if let Some(command) = shell_command_after_double_dash(invocation)? {
        return Ok(Some(command));
    }
    shell_command_from_words(positional_args_before_double_dash(invocation))
}

/// Runs the copy mode display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn copy_mode_display(invocation: &CommandInvocation) -> String {
    let target = invocation.target_arg().unwrap_or("active-pane");
    format!("target={target}:copy_mode=not-entered:reason=live-terminal-state-unavailable")
}

/// Runs the copy selection display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn copy_selection_display(invocation: &CommandInvocation) -> String {
    let target = invocation.target_arg().unwrap_or("active-pane");
    format!("target={target}:copy=not-copied:reason=live-terminal-state-unavailable")
}

/// Runs the paste clipboard display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn paste_clipboard_display(invocation: &CommandInvocation) -> String {
    let target = invocation.target_arg().unwrap_or("active-pane");
    format!(
        "target={target}:paste=not-sent:source=clipboard-or-buffer:reason=live-terminal-state-unavailable"
    )
}

/// Runs the paste buffer display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn paste_buffer_display(invocation: &CommandInvocation) -> String {
    let buffer = flag_value(&invocation.args, "-b")
        .or_else(|| flag_value(&invocation.args, "--buffer"))
        .or_else(|| positional_args(invocation).first().copied())
        .unwrap_or("most-recent");
    format!("buffer={buffer}:paste=not-sent:reason=live-terminal-state-unavailable")
}

/// Runs the create buffer display operation for this subsystem.
///
/// The generic command dispatcher cannot mutate live paste-buffer state, so
/// this fallback reports the missing runtime requirement without pretending a
/// buffer was created.
pub(super) fn create_buffer_display(invocation: &CommandInvocation) -> String {
    let buffer = flag_value(&invocation.args, "-b")
        .or_else(|| flag_value(&invocation.args, "--buffer"))
        .or_else(|| positional_args(invocation).first().copied())
        .unwrap_or("missing");
    format!("buffer={buffer}:created=false:reason=live-paste-buffer-unavailable")
}

/// Runs the list buffers display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn list_buffers_display() -> String {
    KeyValueLine::spaced()
        .push("buffers", 0)
        .push("source", "not-connected")
        .push("status", "empty")
        .finish()
}

/// Runs the choose buffer display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn choose_buffer_display() -> String {
    "buffers=0 chooser=not-entered reason=live-terminal-state-unavailable".to_string()
}

/// Runs the capture pane display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn capture_pane_display(invocation: &CommandInvocation) -> String {
    let target = invocation.target_arg().unwrap_or("active-pane");
    let mode = if invocation.has_flag("-p", "--print") {
        "stdout"
    } else {
        "buffer-or-display"
    };
    format!("target={target}:capture=not-read:output={mode}:reason=live-terminal-state-unavailable")
}

/// Runs the save buffer display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn save_buffer_display(invocation: &CommandInvocation) -> String {
    let buffer = flag_value(&invocation.args, "-b")
        .or_else(|| flag_value(&invocation.args, "--buffer"))
        .or_else(|| positional_args(invocation).first().copied())
        .unwrap_or("most-recent");
    let output = flag_value(&invocation.args, "-o")
        .or_else(|| flag_value(&invocation.args, "--output"))
        .or_else(|| positional_args(invocation).get(1).copied())
        .unwrap_or("stdout");
    format!("buffer={buffer}:save=not-written:output={output}:reason=live-paste-buffer-unavailable")
}

/// Runs the clear history display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn clear_history_display(invocation: &CommandInvocation) -> String {
    let target = invocation.target_arg().unwrap_or("active-pane");
    format!("target={target}:cleared=false:reason=live-terminal-state-unavailable")
}

/// Runs the search history display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn search_history_display(invocation: &CommandInvocation) -> String {
    let target = invocation.target_arg().unwrap_or("active-pane");
    let query = positional_args(invocation).join(" ");
    format!(
        "target={target}:matches=0:query={}:source=not-connected",
        if query.is_empty() {
            "none".to_string()
        } else {
            query
        }
    )
}

/// Runs the export history display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn export_history_display(invocation: &CommandInvocation) -> String {
    let target = invocation.target_arg().unwrap_or("active-pane");
    let output = flag_value(&invocation.args, "-o")
        .or_else(|| flag_value(&invocation.args, "--output"))
        .unwrap_or("stdout");
    format!(
        "target={target}:export=not-written:output={output}:reason=live-terminal-state-unavailable"
    )
}

/// Runs the pipe pane display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pipe_pane_display(invocation: &CommandInvocation) -> String {
    let target = invocation.target_arg().unwrap_or("active-pane");
    let command = positional_args(invocation).join(" ");
    let command = if command.is_empty() {
        "none".to_string()
    } else {
        command
    };
    format!(
        "target={target}:pipe=not-started:command={command}:reason=live-terminal-state-unavailable"
    )
}

/// Returns the optional user-visible snapshot name for a save-layout command.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn save_layout_name(invocation: &CommandInvocation) -> Option<String> {
    flag_value(&invocation.args, "-n")
        .or_else(|| flag_value(&invocation.args, "--name"))
        .or_else(|| positional_args(invocation).first().copied())
        .map(str::to_string)
}

/// Returns the normalized snapshot selector for a load-layout command.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn load_layout_selector(invocation: &CommandInvocation) -> LayoutLoadSelector {
    flag_value(&invocation.args, "--name")
        .or_else(|| flag_value(&invocation.args, "-n"))
        .or_else(|| positional_args(invocation).first().copied())
        .map(|name| LayoutLoadSelector::Name(name.to_string()))
        .unwrap_or(LayoutLoadSelector::Latest)
}

/// Runs the show messages display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn show_messages_display() -> String {
    KeyValueLine::spaced()
        .push("messages", 0)
        .push("source", "in-memory-log")
        .push("status", "empty")
        .finish()
}
/// Runs the show metrics display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn show_metrics_display() -> String {
    KeyValueLine::spaced()
        .push("metrics", "")
        .push("source", "async-runtime")
        .push("status", "unavailable")
        .finish()
        .replace("metrics= ", "metrics ")
}

/// Runs the list default key bindings operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn list_default_key_bindings() -> String {
    let bindings = KeyBindings::default();
    let prefix = key_chord_notation(bindings.escape);
    let mut rows = Vec::new();
    push_optional_key_binding_row(
        &mut rows,
        bindings.split_vertical,
        "default",
        "split-window",
    );
    push_optional_key_binding_row(
        &mut rows,
        bindings.split_horizontal,
        "default",
        "split-window -h",
    );
    push_optional_key_binding_row(&mut rows, bindings.new_window, "default", "new-window");
    push_optional_key_binding_row(&mut rows, bindings.new_group, "default", "new-group");
    push_optional_key_binding_row(&mut rows, bindings.agent_shell, "default", "agent-shell");
    push_optional_key_binding_row(&mut rows, bindings.focus_up, "default", "select-pane -U");
    push_optional_key_binding_row(&mut rows, bindings.focus_down, "default", "select-pane -D");
    push_optional_key_binding_row(&mut rows, bindings.focus_left, "default", "select-pane -L");
    push_optional_key_binding_row(&mut rows, bindings.focus_right, "default", "select-pane -R");
    push_optional_key_binding_row(
        &mut rows,
        bindings.focus_previous_window,
        "default",
        "previous-window",
    );
    push_optional_key_binding_row(
        &mut rows,
        bindings.focus_next_window,
        "default",
        "next-window",
    );
    push_optional_key_binding_row(
        &mut rows,
        bindings.focus_previous_group,
        "default",
        "previous-group",
    );
    push_optional_key_binding_row(
        &mut rows,
        bindings.focus_next_group,
        "default",
        "next-group",
    );

    rows.extend(
        [
            ("C-a", "send-prefix"),
            (":", "command-prompt"),
            ("?", "list-keys"),
            ("d", "detach-client"),
            ("D", "choose-client"),
            ("G", "choose-group"),
            ("c", "new-window"),
            ("C", "new-group"),
            ("a", "agent-shell"),
            (",", "rename-window"),
            ("&", "kill-window --force"),
            ("w", "choose-window"),
            ("(", "previous-group"),
            (")", "next-group"),
            ("n", "next-window"),
            ("p", "previous-window"),
            ("l", "last-window"),
            ("0", "select-window -t 0"),
            ("1", "select-window -t 1"),
            ("2", "select-window -t 2"),
            ("3", "select-window -t 3"),
            ("4", "select-window -t 4"),
            ("5", "select-window -t 5"),
            ("6", "select-window -t 6"),
            ("7", "select-window -t 7"),
            ("8", "select-window -t 8"),
            ("9", "select-window -t 9"),
            ("'", "select-window -t prompt"),
            (".", "move-window -t prompt"),
            ("%", "split-window"),
            ("\"", "split-window -h"),
            ("Up", "select-pane -U"),
            ("Down", "select-pane -D"),
            ("Left", "select-pane -L"),
            ("Right", "select-pane -R"),
            ("o", "select-pane -t next"),
            (";", "last-pane"),
            ("q", "display-panes"),
            ("z", "resize-pane -Z"),
            ("Space", "next-layout"),
            ("x", "kill-pane --force"),
            ("!", "break-pane"),
            ("{", "swap-pane -U"),
            ("}", "swap-pane -D"),
            ("PageUp", "copy-mode -u"),
            ("[", "copy-mode"),
            ("]", "paste-buffer"),
            ("#", "list-buffers"),
            ("=", "choose-buffer"),
            ("-", "delete-buffer"),
            ("O", "choose-observer"),
            ("~", "show-messages"),
        ]
        .into_iter()
        .map(|(key, command)| KeyBindingDisplayRow {
            key: format!("{prefix} {key}"),
            source: "default".to_string(),
            command: command.to_string(),
        }),
    );

    key_binding_rows_display(&rows)
}

/// Carries one rendered key binding row before alignment.
///
/// The type keeps table data structured so both `help` and `list-keys` can
/// present aligned columns without reparsing display strings.
struct KeyBindingDisplayRow {
    /// The display notation for the key chord or chord sequence.
    key: String,
    /// The configuration or generated source for the row.
    source: String,
    /// The command executed by the binding.
    command: String,
}

/// Adds a row when a direct default key binding is enabled.
///
/// # Parameters
/// - `rows`: The table rows being constructed.
/// - `chord`: The optional direct key chord.
/// - `source`: The source label for the binding.
/// - `command`: The command executed by the binding.
fn push_optional_key_binding_row(
    rows: &mut Vec<KeyBindingDisplayRow>,
    chord: Option<KeyChord>,
    source: &str,
    command: &str,
) {
    if let Some(chord) = chord {
        rows.push(KeyBindingDisplayRow {
            key: key_chord_notation(chord),
            source: source.to_string(),
            command: command.to_string(),
        });
    }
}

/// Renders key binding rows with aligned columns.
///
/// # Parameters
/// - `rows`: The key binding rows to display.
fn key_binding_rows_display(rows: &[KeyBindingDisplayRow]) -> String {
    let key_width = rows
        .iter()
        .map(|row| row.key.len())
        .max()
        .unwrap_or("key".len())
        .max("key".len());
    let source_width = rows
        .iter()
        .map(|row| row.source.len())
        .max()
        .unwrap_or("source".len())
        .max("source".len());
    std::iter::once(format!(
        "{:<key_width$}  {:<source_width$}  command",
        "key", "source"
    ))
    .chain(rows.iter().map(|row| {
        format!(
            "{:<key_width$}  {:<source_width$}  {}",
            row.key, row.source, row.command
        )
    }))
    .collect::<Vec<_>>()
    .join("\n")
}

/// Runs the key chord notation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn key_chord_notation(chord: KeyChord) -> String {
    if chord.code == KeyCode::Char('+')
        && chord.modifiers.alt
        && !chord.modifiers.ctrl
        && !chord.modifiers.shift
    {
        return "A-S-=".to_string();
    }
    let mut notation = String::new();
    if chord.modifiers.ctrl {
        notation.push_str("C-");
    }
    if chord.modifiers.alt {
        notation.push_str("A-");
    }
    if chord.modifiers.shift {
        notation.push_str("S-");
    }
    notation.push_str(
        match chord.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(ch) => ch.to_string(),
            KeyCode::Up => "Up".to_string(),
            KeyCode::Down => "Down".to_string(),
            KeyCode::Left => "Left".to_string(),
            KeyCode::Right => "Right".to_string(),
            KeyCode::PageUp => "PageUp".to_string(),
            KeyCode::PageDown => "PageDown".to_string(),
            KeyCode::Home => "Home".to_string(),
            KeyCode::End => "End".to_string(),
        }
        .as_str(),
    );
    notation
}

/// Runs the show default options operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn show_default_options() -> String {
    format!(
        "source=default live_mutation=not-connected\n{}",
        crate::config::DEFAULT_CONFIG_TOML.trim()
    )
}

/// Runs the set option args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn set_option_args(invocation: &CommandInvocation) -> Result<(&str, &str)> {
    let args = positional_args(invocation);
    let path = args
        .first()
        .copied()
        .ok_or_else(|| MezError::invalid_args("set-option requires an option path"))?;
    let value = args
        .get(1)
        .copied()
        .ok_or_else(|| MezError::invalid_args("set-option requires a value"))?;
    Ok((path, value))
}

/// Runs the set theme arg operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn set_theme_arg(invocation: &CommandInvocation) -> Result<&str> {
    let args = positional_args(invocation);
    let theme = args
        .first()
        .copied()
        .ok_or_else(|| MezError::invalid_args("set-theme requires a theme name"))?;
    if args.len() > 1 {
        return Err(MezError::invalid_args(
            "set-theme accepts exactly one theme name",
        ));
    }
    Ok(theme)
}

/// Runs the parse config command value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_config_command_value(value: &str) -> ConfigMutationValue {
    match value {
        "true" => ConfigMutationValue::Boolean(true),
        "false" => ConfigMutationValue::Boolean(false),
        _ => value
            .parse::<i64>()
            .map(ConfigMutationValue::Integer)
            .unwrap_or_else(|_| ConfigMutationValue::String(value.to_string())),
    }
}

/// Runs the bind key args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn bind_key_args(invocation: &CommandInvocation) -> Result<(&str, String)> {
    let key = invocation
        .args
        .first()
        .map(String::as_str)
        .ok_or_else(|| MezError::invalid_args("bind-key requires a key"))?;
    let command_words = invocation
        .args
        .get(1..)
        .ok_or_else(|| MezError::invalid_args("bind-key requires a command"))?;
    if command_words.is_empty() {
        return Err(MezError::invalid_args("bind-key requires a command"));
    }
    let command = command_words.join(" ");
    if command.trim().is_empty() {
        return Err(MezError::invalid_args("bind-key command must not be empty"));
    }
    Ok((key, command))
}

/// Runs the binding config key operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn binding_config_key(notation: &str) -> String {
    let mut key = String::from("key");
    for byte in notation.bytes() {
        key.push('_');
        key.push(hex_digit(byte >> 4));
        key.push(hex_digit(byte & 0x0f));
    }
    key
}

/// Runs the hex digit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0' + nibble),
        10..=15 => char::from(b'a' + (nibble - 10)),
        _ => unreachable!("hex nibble is always less than 16"),
    }
}

/// Runs the auth status display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn auth_status_display() -> String {
    KeyValueLine::spaced()
        .push("authenticated", "unknown")
        .push("provider", "openai")
        .push("profile", "default")
        .push("source", "not-connected")
        .finish()
}

/// Runs the mcp status plan display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_status_plan_display(invocation: &CommandInvocation) -> Result<String> {
    let server_id = mcp_server_id(invocation, "mcp-status requires a server id")?;
    Ok(KeyValueLine::colon_separated()
        .push("server", server_id)
        .push("authenticated", false)
        .push("state", "unknown")
        .push("reason", "auth-store-unavailable")
        .finish())
}

/// Runs the client role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn client_role_name(role: crate::session::ClientRole) -> &'static str {
    match role {
        crate::session::ClientRole::Primary => "primary",
        crate::session::ClientRole::PendingObserver => "pending_observer",
        crate::session::ClientRole::Observer => "observer",
        crate::session::ClientRole::Agent => "agent",
        crate::session::ClientRole::Automation => "automation",
    }
}

/// Runs the client state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn client_state_name(state: crate::session::ClientState) -> &'static str {
    match state {
        crate::session::ClientState::Attached => "attached",
        crate::session::ClientState::Pending => "pending",
        crate::session::ClientState::Detached => "detached",
        crate::session::ClientState::Revoked => "revoked",
        crate::session::ClientState::Failed => "failed",
    }
}

/// Runs the observer state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn observer_state_name(state: crate::session::ObserverDecisionState) -> &'static str {
    match state {
        crate::session::ObserverDecisionState::Pending => "pending",
        crate::session::ObserverDecisionState::Approved => "approved",
        crate::session::ObserverDecisionState::Rejected => "rejected",
        crate::session::ObserverDecisionState::Revoked => "revoked",
    }
}

/// Runs the session state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn session_state_name(state: crate::session::SessionState) -> &'static str {
    match state {
        crate::session::SessionState::Running => "running",
        crate::session::SessionState::Detached => "detached",
        crate::session::SessionState::Empty => "empty",
        crate::session::SessionState::Stopping => "stopping",
        crate::session::SessionState::Failed => "failed",
    }
}
