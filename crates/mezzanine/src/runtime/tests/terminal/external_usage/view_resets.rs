//! Durable reset command and worker-custody tests through production owners.
//! Legacy capability fixtures exercise accounting/reset semantics only; actual
//! native cold authority remains covered by the qualified-source regressions.

use super::*;

/// Commits one real report and projects its actual durable checkpoint.
fn ingest(
    service: &mut RuntimeSessionService,
    connection: &ControlConnectionState,
    params: &serde_json::Value,
) {
    let request = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"usage","method":"agent/external/usage","params":params}).to_string()).unwrap();
    let work = service
        .prepare_external_usage(&request, connection)
        .unwrap();
    let committed = work.execute().unwrap();
    let reply: serde_json::Value =
        serde_json::from_str(&service.complete_external_usage(work, Ok(committed))).unwrap();
    assert_eq!(reply["result"]["durable"], true);
}

/// Returns only the pane projection, independently of the expense ledger.
fn pane_input(service: &RuntimeSessionService) -> u64 {
    service
        .external_token_usage(Some("%1"))
        .values()
        .map(|usage| usage.input_tokens)
        .sum()
}

/// A real external-only `/reset-status` is deferred with no immediate success
/// or baseline mutation. A late report after capture remains visible; worker
/// and actor settlement commit the captured point without clearing new spend.
#[test]
fn runtime_external_reset_command_commits_captured_point_without_hiding_late_expense() {
    let (mut service, connection, mut params, store) = fixture();
    ingest(&mut service, &connection, &params);
    let primary = service
        .attach_primary("reset client", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let response = service
        .execute_agent_shell_command(&primary, "/reset-status")
        .unwrap();
    assert!(!response.contains("pane_token_usage_reset=true"));
    assert_eq!(pane_input(&service), 10);
    let dispatch = service
        .take_pending_deferred_agent_commands()
        .pop()
        .unwrap();
    let work = service
        .claim_agent_command_work(
            &primary,
            &dispatch.pane_id,
            &dispatch.command,
            &dispatch.input,
            dispatch.claim_generation,
            &dispatch.conversation_id,
        )
        .unwrap()
        .unwrap();
    params["event_id"] = "late".into();
    params["sequence"] = 2.into();
    ingest(&mut service, &connection, &params);
    let outcome = RuntimeSessionService::execute_deferred_agent_command(&work);
    assert_eq!(pane_input(&service), 20);
    let projected =
        RuntimeSessionService::project_deferred_agent_command_outcome(&work, outcome).unwrap();
    assert!(
        service
            .complete_agent_command_work(&work, projected)
            .unwrap()
    );
    assert_eq!(pane_input(&service), 10);
    assert_eq!(external_ledger_input(&store), 20);
    assert!(!service.agent_command_is_active("%1"));
    service.terminate_all_pane_processes().unwrap();
}

/// Supplied success before actual execution cannot seal a reset. A real held
/// SQLite writer fails boundedly without baseline mutation; one NEW command
/// after release commits successfully. No usage/provider operation is replayed.
#[test]
fn runtime_external_reset_requires_actual_write_and_failed_writer_preserves_view() {
    let (mut service, connection, params, store) = fixture();
    ingest(&mut service, &connection, &params);
    let reset = service.prepare_external_usage_reset("%1").unwrap();
    assert!(service.complete_external_usage_reset(&reset).is_err());
    assert_eq!(pane_input(&service), 10);
    let writer = rusqlite::Connection::open(store.path()).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(reset.execute().is_err());
    writer.execute_batch("ROLLBACK").unwrap();
    assert!(reset.execute().is_err(), "failed work silently replayed");
    assert!(service.complete_external_usage_reset(&reset).is_err());
    assert_eq!(pane_input(&service), 10);
    let fresh = service.prepare_external_usage_reset("%1").unwrap();
    fresh.execute().unwrap();
    service.complete_external_usage_reset(&fresh).unwrap();
    assert_eq!(pane_input(&service), 0);
    assert_eq!(external_ledger_input(&store), 10);
    service.terminate_all_pane_processes().unwrap();
}
