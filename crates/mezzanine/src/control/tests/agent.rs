//! Control agent tests.

use super::*;
use crate::control::{
    AgentStateProjection, dispatch_control_request_for_client_with_agent_state_and_model_profiles,
};

/// Verifies dispatches agent read and shell visibility methods.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn dispatches_agent_read_and_shell_visibility_methods() {
    let (mut session, primary) = test_session();

    let list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"agent/list","params":{}}"#,
        &mut session,
        &primary,
    );
    assert!(list.contains(r#""agents""#));
    assert!(list.contains(r#""pane_id":"%1""#));

    let targeted_list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":10,"method":"agent/list","params":{"target":{"default":true}}}"#,
        &mut session,
        &primary,
    );
    assert!(targeted_list.contains(r#""agents""#), "{targeted_list}");

    let missing_session_list = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":11,"method":"agent/list","params":{"target":{"session_id":"missing"}}}"#,
        &mut session,
        &primary,
    );
    assert!(
        missing_session_list.contains(r#""mezzanine_code":"not_found""#),
        "{missing_session_list}"
    );

    let tasks = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"agent/task/list","params":{}}"#,
        &mut session,
        &primary,
    );
    assert!(tasks.contains(r#""tasks":[]"#));

    let targeted_tasks = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":12,"method":"agent/task/list","params":{"target":{"default":true}}}"#,
        &mut session,
        &primary,
    );
    assert!(targeted_tasks.contains(r#""tasks":[]"#), "{targeted_tasks}");

    let conflicting_agent_target = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":13,"method":"agent/task/list","params":{"target":{"agent_id":"agent-%1","pane_id":"%1"}}}"#,
        &mut session,
        &primary,
    );
    assert!(
        conflicting_agent_target.contains(r#""mezzanine_code":"invalid_params""#),
        "{conflicting_agent_target}"
    );

    let show = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"agent/shell/show","params":{"target":{"pane_id":"%1"},"idempotency_key":"show-agent"}}"#,
        &mut session,
        &primary,
    );
    assert!(show.contains(r#""visible":true"#));

    let hide = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":4,"method":"agent/shell/hide","params":{"target":{"pane_id":"%1"},"idempotency_key":"hide-agent"}}"#,
        &mut session,
        &primary,
    );
    assert!(hide.contains(r#""visible":false"#));

    let command = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":5,"method":"agent/shell/command","params":{"idempotency_key":"agent-command","input":"summarize"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        command.contains(r#""kind":"requires_runtime""#),
        "{command}"
    );
    assert!(command.contains(r#""command":"prompt""#), "{command}");
    assert!(command.contains(r#""turn":null"#), "{command}");

    let command_alias = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":14,"method":"agent/shell/command","params":{"idempotency_key":"agent-command-alias","command":"summarize"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        command_alias.contains(r#""mezzanine_code":"invalid_params""#),
        "{command_alias}"
    );
    assert!(
        command_alias.contains("agent/shell/command params contains unknown field `command`"),
        "{command_alias}"
    );
}

/// Verifies generic control reports runtime required methods as invalid state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn generic_control_reports_runtime_required_methods_as_invalid_state() {
    let (mut session, primary) = test_session();

    let terminal_view = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":1,"method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#,
        &mut session,
        &primary,
    );
    assert!(
        terminal_view.contains(r#""mezzanine_code":"invalid_state""#),
        "{terminal_view}"
    );
    assert!(
        terminal_view.contains("terminal runtime is not attached"),
        "{terminal_view}"
    );
    assert!(
        !terminal_view.contains(r#""mezzanine_code":"method_not_found""#),
        "{terminal_view}"
    );

    let terminal_step = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":2,"method":"terminal/step","params":{"idempotency_key":"terminal-step","input_bytes":[],"render":false}}"#,
        &mut session,
        &primary,
    );
    assert!(
        terminal_step.contains(r#""mezzanine_code":"invalid_state""#),
        "{terminal_step}"
    );

    let terminal_command = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"terminal/command","params":{"idempotency_key":"terminal-command","input":"list-windows"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        terminal_command.contains(r#""mezzanine_code":"invalid_state""#),
        "{terminal_command}"
    );

    let missing_input = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":4,"method":"terminal/command","params":{"idempotency_key":"terminal-missing"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        missing_input.contains("terminal/command requires input"),
        "{missing_input}"
    );

    let command_alias = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":14,"method":"terminal/command","params":{"idempotency_key":"terminal-command-alias","command":"list-windows"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        command_alias.contains(r#""mezzanine_code":"invalid_params""#),
        "{command_alias}"
    );
    assert!(
        command_alias.contains("terminal/command params contains unknown field `command`"),
        "{command_alias}"
    );

    let spawn = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":5,"method":"agent/spawn","params":{"parent_agent":{"agent_id":"agent-%1"},"placement":{"mode":"new-pane"},"role":"explorer","cooperation_mode":"explore-only","read_scopes":["src"],"write_scopes":[],"prompt":"inspect","idempotency_key":"spawn"}}"#,
        &mut session,
        &primary,
    );
    assert!(
        spawn.contains(r#""mezzanine_code":"invalid_state""#),
        "{spawn}"
    );
    assert!(spawn.contains("agent runtime is not attached"), "{spawn}");

    let invalid_spawn = dispatch_control_request(
        r#"{"jsonrpc":"2.0","id":6,"method":"agent/spawn","params":{"idempotency_key":"spawn-bad","unexpected":true}}"#,
        &mut session,
        &primary,
    );
    assert!(
        invalid_spawn.contains("agent/spawn params contains unknown field"),
        "{invalid_spawn}"
    );
}

/// Runs the primary initialize request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn primary_initialize_request() -> &'static str {
    r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#
}

/// Runs the primary control method fixture request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn primary_control_method_fixture_request(
    method: &str,
    session: &mut Session,
    primary: &ClientId,
) -> String {
    match method {
        "control/initialize" => primary_initialize_request().to_string(),
        "control/cancel" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"control/cancel","params":{"request_id":"missing"}}"#
                .to_string()
        }
        "control/shutdown" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"control/shutdown","params":{}}"#.to_string()
        }
        "session/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"session/list","params":{}}"#.to_string()
        }
        "session/get" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"session/get","params":{}}"#.to_string()
        }
        "session/rename" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"session/rename","params":{"name":"work","idempotency_key":"session-rename"}}"#.to_string()
        }
        "session/kill" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"session/kill","params":{"force":true,"idempotency_key":"session-kill"}}"#.to_string()
        }
        "session/attach" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"session/attach","params":{"role":"observer","client":{"name":"observer","requested_role":"observer","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"idempotency_key":"session-attach"}}"#.to_string()
        }
        "client/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"client/list","params":{}}"#.to_string()
        }
        "client/detach" => format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"client/detach","params":{{"client_id":"{}","idempotency_key":"client-detach"}}}}"#,
            primary
        ),
        "client/select_primary" => format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"client/select_primary","params":{{"client_id":"{}","idempotency_key":"client-select-primary"}}}}"#,
            primary
        ),
        "observer/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"observer/list","params":{}}"#.to_string()
        }
        "observer/inspect" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"observer/inspect","params":{"observer_request_id":"missing-observer"}}"#.to_string()
        }
        "observer/approve" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"observer/approve","params":{"observer_request_id":"missing-observer","idempotency_key":"observer-approve"}}"#.to_string()
        }
        "observer/reject" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"observer/reject","params":{"observer_request_id":"missing-observer","reason":"not now","idempotency_key":"observer-reject"}}"#.to_string()
        }
        "observer/revoke" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"observer/revoke","params":{"client_id":"missing-client","reason":"done","idempotency_key":"observer-revoke"}}"#.to_string()
        }
        "window/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"window/list","params":{}}"#.to_string()
        }
        "window/create" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"window/create","params":{"name":"work","select":true,"idempotency_key":"window-create"}}"#.to_string()
        }
        "window/rename" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"window/rename","params":{"target":{"window_id":"@1"},"name":"renamed","idempotency_key":"window-rename"}}"#.to_string()
        }
        "window/select" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"window/select","params":{"target":{"window_id":"@1"},"idempotency_key":"window-select"}}"#.to_string()
        }
        "window/close" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"window/close","params":{"target":{"window_id":"@1"},"force":true,"idempotency_key":"window-close"}}"#.to_string()
        }
        "pane/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"pane/list","params":{}}"#.to_string()
        }
        "pane/create" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"pane/create","params":{"split":"vertical","select":true,"idempotency_key":"pane-create"}}"#.to_string()
        }
        "pane/select" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"pane/select","params":{"target":{"pane_id":"%1"},"idempotency_key":"pane-select"}}"#.to_string()
        }
        "pane/resize" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"pane/resize","params":{"target":{"pane_id":"%1"},"size":{"mode":"cells","columns":100,"rows":30},"idempotency_key":"pane-resize"}}"#.to_string()
        }
        "pane/move" | "pane/swap" | "pane/join" => {
            session
                .split_active_pane(primary, SplitDirection::Vertical)
                .unwrap();
            match method {
                "pane/move" => {
                    r#"{"jsonrpc":"2.0","id":1,"method":"pane/move","params":{"source":{"pane_id":"%1"},"destination":{"pane_id":"%2"},"position":"horizontal","idempotency_key":"pane-move"}}"#.to_string()
                }
                "pane/swap" => {
                    r#"{"jsonrpc":"2.0","id":1,"method":"pane/swap","params":{"source":{"pane_id":"%1"},"destination":{"pane_id":"%2"},"idempotency_key":"pane-swap"}}"#.to_string()
                }
                _ => {
                    r#"{"jsonrpc":"2.0","id":1,"method":"pane/join","params":{"source":{"pane_id":"%1"},"destination":{"pane_id":"%2"},"position":"vertical","idempotency_key":"pane-join"}}"#.to_string()
                }
            }
        }
        "pane/break" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"pane/break","params":{"target":{"pane_id":"%1"},"name":"broken","idempotency_key":"pane-break"}}"#.to_string()
        }
        "pane/close" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"pane/close","params":{"target":{"pane_id":"%1"},"force":true,"idempotency_key":"pane-close"}}"#.to_string()
        }
        "pane/capture" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"pane/capture","params":{"target":{"pane_id":"%1"},"range":{"origin":"visible","start":"start","end":"end"}}}"#.to_string()
        }
        "terminal/step" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"terminal/step","params":{"input_bytes":[],"render":false,"idempotency_key":"terminal-step"}}"#.to_string()
        }
        "terminal/view" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#.to_string()
        }
        "terminal/command" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"terminal/command","params":{"input":"list-windows","idempotency_key":"terminal-command"}}"#.to_string()
        }
        "frame/read" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"frame/read","params":{}}"#.to_string()
        }
        "agent/shell/show" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"agent/shell/show","params":{"target":{"pane_id":"%1"},"idempotency_key":"agent-show"}}"#.to_string()
        }
        "agent/shell/hide" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"agent/shell/hide","params":{"target":{"pane_id":"%1"},"idempotency_key":"agent-hide"}}"#.to_string()
        }
        "agent/shell/command" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"agent/shell/command","params":{"input":"summarize","idempotency_key":"agent-command"}}"#.to_string()
        }
        "agent/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"agent/list","params":{}}"#.to_string()
        }
        "agent/task/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"agent/task/list","params":{}}"#.to_string()
        }
        "agent/spawn" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"agent/spawn","params":{"parent_agent":{"agent_id":"agent-%1"},"placement":{"mode":"new-pane"},"role":"explorer","cooperation_mode":"explore-only","read_scopes":["src"],"write_scopes":[],"prompt":"inspect","idempotency_key":"agent-spawn"}}"#.to_string()
        }
        "event/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"event/list","params":{}}"#.to_string()
        }
        "config/validate" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"config/validate","params":{}}"#.to_string()
        }
        "config/get" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"config/get","params":{"path":"history.lines","effective":true}}"#.to_string()
        }
        "config/set" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"config/set","params":{"path":"history.lines","value":2048,"idempotency_key":"config-set"}}"#.to_string()
        }
        "config/unset" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"config/unset","params":{"path":"history.persist","idempotency_key":"config-unset"}}"#.to_string()
        }
        "config/reload" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"config/reload","params":{"idempotency_key":"config-reload"}}"#.to_string()
        }
        "project/trust/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"project/trust/list","params":{}}"#.to_string()
        }
        "project/trust/inspect" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"project/trust/inspect","params":{"project_root":"/tmp/missing-project"}}"#.to_string()
        }
        "project/trust/decide" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"project/trust/decide","params":{"project_root":"/tmp/project","decision":"trust","idempotency_key":"project-trust"}}"#.to_string()
        }
        "project/trust/revoke" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"project/trust/revoke","params":{"project_root":"/tmp/project","idempotency_key":"project-revoke"}}"#.to_string()
        }
        "mcp/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"mcp/list","params":{}}"#.to_string()
        }
        "mcp/retry" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"mcp/retry","params":{"server_id":"missing","idempotency_key":"mcp-retry"}}"#.to_string()
        }
        "approval/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"approval/list","params":{}}"#.to_string()
        }
        "approval/decide" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"approval/decide","params":{"approval_id":"missing-approval","decision":"approve","idempotency_key":"approval-decision"}}"#.to_string()
        }
        "snapshot/list" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"snapshot/list","params":{}}"#.to_string()
        }
        "snapshot/create" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"snapshot/create","params":{"target":{"default":true},"name":"manual","idempotency_key":"snapshot-create"}}"#.to_string()
        }
        "snapshot/resume" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"snapshot/resume","params":{"snapshot_id":"missing-snapshot","idempotency_key":"snapshot-resume"}}"#.to_string()
        }
        "snapshot/delete" => {
            r#"{"jsonrpc":"2.0","id":1,"method":"snapshot/delete","params":{"snapshot_id":"missing-snapshot","idempotency_key":"snapshot-delete"}}"#.to_string()
        }
        unknown => panic!("missing primary control fixture for {unknown}"),
    }
}

/// Verifies that the advertised primary control capability set is executable
/// through the initialized connection boundary. The test does not require every
/// method to succeed without its live runtime or repository dependency; it locks
/// in that each baseline method reaches its method-specific implementation and
/// reports a structured result or domain error rather than `method_not_found`.
#[test]
fn advertised_primary_methods_do_not_fall_through_as_unknown_after_initialize() {
    for method in PRIMARY_CONTROL_METHODS {
        let (mut session, primary) = test_session();
        let mut connection = ControlConnectionState::new(true, true);
        let mut cache = ControlIdempotencyCache::default();
        let request = primary_control_method_fixture_request(method, &mut session, &primary);
        let response = if *method == "control/initialize" {
            dispatch_control_request_for_connection(
                &request,
                &mut session,
                &mut connection,
                &mut cache,
            )
        } else {
            let initialize = dispatch_control_request_for_connection(
                primary_initialize_request(),
                &mut session,
                &mut connection,
                &mut cache,
            );
            assert!(
                initialize.contains(r#""granted_role":"primary""#),
                "{method} initialize response: {initialize}"
            );
            dispatch_control_request_for_connection(
                &request,
                &mut session,
                &mut connection,
                &mut cache,
            )
        };

        assert!(
            !response.contains(r#""mezzanine_code":"method_not_found""#),
            "{method} returned method_not_found: {response}"
        );
        assert!(
            !response.contains("is not implemented"),
            "{method} returned implementation placeholder: {response}"
        );
    }
}

/// Verifies agent state dispatch persists visibility and lists turns.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn agent_state_dispatch_persists_visibility_and_lists_turns() {
    let (mut session, primary) = test_session();
    let mut store = AgentShellStore::default();
    let mut ledger = AgentTurnLedger::new(false);

    let show = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":1,"method":"agent/shell/show","params":{"target":{"pane_id":"%1"},"idempotency_key":"show-agent"}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(show.contains(r#""visible":true"#), "{show}");
    let conversation_id = store.get("%1").unwrap().session_id.clone();
    assert!(
        show.contains(&format!(r#""conversation_id":"{conversation_id}""#)),
        "{show}"
    );

    store.start_turn("%1", "turn-1").unwrap();
    ledger
        .start_turn(mez_agent::AgentTurnRecord {
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 42,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: AgentTurnState::Queued,

            initial_capability: None,
        })
        .unwrap();

    let list = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":2,"method":"agent/list","params":{}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(list.contains(r#""visible":true"#), "{list}");
    assert!(list.contains(r#""status":"running""#), "{list}");
    assert!(list.contains(r#""last_turn_id":"turn-1""#), "{list}");

    let targeted_list = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":20,"method":"agent/list","params":{"target":{"default":true}}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(
        targeted_list.contains(r#""visible":true"#),
        "{targeted_list}"
    );

    let missing_session_list = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":21,"method":"agent/list","params":{"target":{"name":"elsewhere"}}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(
        missing_session_list.contains(r#""mezzanine_code":"not_found""#),
        "{missing_session_list}"
    );

    let command = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":30,"method":"agent/shell/command","params":{"idempotency_key":"agent-status","input":"/status"}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(command.contains(r#""kind":"display""#), "{command}");
    assert!(command.contains(r#""command":"status""#), "{command}");
    assert!(command.contains(r#""turn":null"#), "{command}");

    let command_alias = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":33,"method":"agent/shell/command","params":{"idempotency_key":"agent-alias","command":"/status"}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(
        command_alias.contains(r#""mezzanine_code":"invalid_params""#),
        "{command_alias}"
    );
    assert!(
        command_alias.contains("agent/shell/command params contains unknown field `command`"),
        "{command_alias}"
    );

    let tasks = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":3,"method":"agent/task/list","params":{"target":{"agent_id":"agent-%1"}}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(tasks.contains(r#""id":"turn-1""#), "{tasks}");
    assert!(tasks.contains(r#""agent_id":"agent-%1""#), "{tasks}");
    assert!(tasks.contains(r#""state":"running""#), "{tasks}");
    assert!(
        tasks.contains(r#""created_at":"1970-01-01T00:00:42Z""#),
        "{tasks}"
    );
    assert!(
        tasks.contains(r#""started_at":"1970-01-01T00:00:42Z""#),
        "{tasks}"
    );
    assert!(!tasks.contains("unix:"), "{tasks}");
    assert!(
        tasks.contains(r#""prompt_preview":"user prompt""#),
        "{tasks}"
    );

    ledger
        .finish_turn("turn-1", AgentTurnState::Blocked)
        .unwrap();
    let waiting_tasks = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":34,"method":"agent/task/list","params":{"target":{"agent_id":"agent-%1"}}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(
        waiting_tasks.contains(r#""state":"waiting""#),
        "{waiting_tasks}"
    );
    assert!(
        waiting_tasks.contains(r#""approval_ids":[]"#),
        "{waiting_tasks}"
    );

    let approval_ids_by_turn =
        std::collections::BTreeMap::from([("turn-1".to_string(), vec!["approval-1".to_string()])]);
    let approval_tasks = dispatch_control_request_for_client_with_agent_state_and_model_profiles(
        r#"{"jsonrpc":"2.0","id":35,"method":"agent/task/list","params":{"target":{"agent_id":"agent-%1"}}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
        AgentStateProjection::new(None, Some(&approval_ids_by_turn)),
    );
    assert!(
        approval_tasks.contains(r#""state":"waiting_approval""#),
        "{approval_tasks}"
    );
    assert!(
        approval_tasks.contains(r#""approval_ids":["approval-1"]"#),
        "{approval_tasks}"
    );

    let session_tasks = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":31,"method":"agent/task/list","params":{"target":{"default":true}}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(
        session_tasks.contains(r#""id":"turn-1""#),
        "{session_tasks}"
    );

    let missing_pane_tasks = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":32,"method":"agent/task/list","params":{"target":{"pane_id":"missing"}}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(
        missing_pane_tasks.contains(r#""mezzanine_code":"not_found""#),
        "{missing_pane_tasks}"
    );

    let hide = dispatch_control_request_for_client_with_agent_state(
        r#"{"jsonrpc":"2.0","id":4,"method":"agent/shell/hide","params":{"target":{"pane_id":"%1"},"idempotency_key":"hide-agent"}}"#,
        &mut session,
        &primary,
        None,
        &mut store,
        &ledger,
    );
    assert!(hide.contains(r#""visible":false"#), "{hide}");
    assert!(hide.contains(r#""status":"running""#), "{hide}");
}
