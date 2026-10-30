//! Ordinary interactive Cursor source admission using genuine native fixtures.
//!
//! Public session/generation selectors never replace native helper writer,
//! retained direct creator or exact pane-root qualification.

use super::curated_admission::curated_request;
use super::helper_presentation::{child, settle};
use super::*;

/// Exact native Cursor session-start contract; no guessed vendor source field.
fn declaration(session: &str) -> JsonRpcRequest {
    let mut request = curated_request();
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["harness"] = "cursor".into();
    params["observer_kind"] = "command-hook".into();
    params["source_contract"] = "cursor-local-command/1".into();
    params["observer_instance"] = "cursor-local-hooks-1".into();
    params["session_boundary"] = "session-start".into();
    params["external_session_id"] = session.into();
    request.params = Some(params.to_string());
    request
}

/// Vendor-confirmed interactive native session hooks should enroll their genuine
/// local creator without a launcher, fd3, inherited process claim or relabel to
/// another vendor. This initially reproduces missing distinct Cursor admission.
#[tokio::test(flavor = "current_thread")]
async fn external_cursor_native_session_creator_is_supported() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Cursor creator requires native evidence");
    };
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration("session-a"), &connection)
        .unwrap();
    let reply = settle(&mut fixture, work, &connection).await;
    assert_eq!(reply["result"]["registered"], true, "{reply}");
}

/// Exact native event grammar with no private credentials, counters or title.
fn callback(
    session: &str,
    event: &str,
    generation: Option<&str>,
    status: Option<&str>,
) -> JsonRpcRequest {
    let mut params =
        serde_json::json!({"pane_id":"%1","external_session_id":session,"event":event});
    if let Some(generation) = generation {
        params["generation_id"] = generation.into();
    }
    if let Some(status) = status {
        params["status"] = status.into();
    }
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"cursor-test","method":"agent/external/cursor-observe","params":params}).to_string()).unwrap()
}

/// Native interactive prompt generations fence stale Stops and terminal replay.
/// Completed means ready, aborted/error preserve distinct states, compact/tool
/// context never charges usage, and ending an old session preserves replacement.
#[tokio::test(flavor = "current_thread")]
async fn external_cursor_native_generations_stops_and_exact_end() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Cursor lifecycle requires native evidence");
    };
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration("session-a"), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["registered"],
        true
    );
    let expires = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .expires;
    for (event, generation, status, state, sequence, changed) in [
        ("beforeSubmitPrompt", "a", None, "running", 1, true),
        ("beforeSubmitPrompt", "a", None, "running", 1, false),
        ("preCompact", "a", None, "running", 1, false),
        ("postToolUse", "a", None, "running", 1, false),
        ("stop", "a", Some("completed"), "ready", 2, true),
        ("stop", "a", Some("completed"), "ready", 2, false),
        ("afterAgentResponse", "a", None, "ready", 2, false),
        ("beforeSubmitPrompt", "b", None, "running", 3, true),
        ("stop", "b", Some("aborted"), "interrupted", 4, true),
        ("beforeSubmitPrompt", "c", None, "running", 5, true),
        ("stop", "c", Some("error"), "failed", 6, true),
    ] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(
                &callback("session-a", event, Some(generation), status),
                &connection,
            )
            .unwrap();
        let reply = settle(&mut fixture, work, &connection).await;
        assert_eq!(reply["id"], "cursor-test");
        assert_eq!(reply["result"]["sequence"], sequence, "{reply}");
        assert_eq!(reply["result"]["changed"], changed, "{reply}");
        assert_eq!(
            fixture
                .service
                .live_pane_harness_status("%1")
                .unwrap()
                .text
                .as_deref(),
            Some(state)
        );
    }
    for request in [
        callback("session-a", "stop", Some("a"), Some("completed")),
        callback("session-a", "beforeSubmitPrompt", Some("a"), None),
    ] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&request, &connection)
            .unwrap();
        assert!(
            settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_some()
        );
    }
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(
                &callback("session-a", "beforeSubmitPrompt", Some("session-a"), None),
                &connection
            )
            .is_err()
    );
    let root = fixture.connection.clone();
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &callback("session-a", "beforeSubmitPrompt", Some("foreign"), None),
            &root,
        )
        .unwrap();
    assert!(
        settle(&mut fixture, work, &root)
            .await
            .get("error")
            .is_some()
    );
    assert_eq!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .expires,
        expires
    );
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration("session-b"), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["registered"],
        true
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &callback("session-a", "sessionEnd", None, None),
            &connection,
        )
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["retired"],
        true
    );
    assert!(
        !fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .find(|b| b.registration.as_ref().unwrap().external_session_id == "session-b")
            .unwrap()
            .retired
    );
    assert!(!connection.initialized());
    assert!(connection.caller_client_id().is_none());
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
}

/// Queued original activity/end work cannot adopt a rotated observer even when
/// source/session labels still match. The live successor survives both queued
/// work rejection and subsequent stateless old callbacks, with no client grant.
#[tokio::test(flavor = "current_thread")]
async fn external_cursor_observer_rotation_fences_original_callbacks() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Cursor rotation requires native evidence");
    };
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration("session-a"), &connection)
        .unwrap();
    let first = settle(&mut fixture, work, &connection).await;
    let stale = fixture
        .service
        .prepare_external_helper_presentation(
            &callback("session-a", "beforeSubmitPrompt", Some("a"), None),
            &connection,
        )
        .unwrap();
    let end = fixture
        .service
        .prepare_external_helper_presentation(
            &callback("session-a", "sessionEnd", None, None),
            &connection,
        )
        .unwrap();
    let mut rotation = declaration("session-a");
    let mut params: serde_json::Value =
        serde_json::from_str(rotation.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = "successor".into();
    params["predecessor_generation"] = first["result"]["generation"].clone();
    rotation.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_enrollment(&rotation, &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["registered"],
        true
    );
    for work in [stale, end] {
        assert!(
            settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_some()
        );
    }
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(
                &callback("session-a", "sessionEnd", None, None),
                &connection
            )
            .is_err()
    );
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 1);
    assert!(registry.bindings.values().all(|b| !b.retired));
    assert!(registry.enrollments.pending.is_empty());
}

/// A real pane-root replacement must not let already observed Cursor work adopt
/// a newly created native producer, even when the vendor reuses its session ID.
/// Both roots and creators are generated through production process ownership.
/// Old callbacks keep their original native proof, while the fresh source owns
/// a distinct durable namespace and can still paint its own prompt generation.
/// Unsupported usage stays absent throughout; no historical expense is replayed.
#[tokio::test(flavor = "current_thread")]
async fn external_cursor_actual_root_replacement_fences_queued_callbacks() {
    for kind in ["beforeSubmitPrompt", "sessionEnd"] {
        let Some(mut fixture) = fixture("hold").await else {
            panic!("Cursor root replacement requires native evidence");
        };
        let usage = fixture.directory.join("cursor-root-usage.sqlite");
        fixture
            .service
            .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&usage));
        let original_root = fixture.service.pane_process_identity("%1").unwrap();
        let (_old_socket, old_connection) = child(&mut fixture).await;
        let work = fixture
            .service
            .prepare_external_enrollment(&declaration("session-a"), &old_connection)
            .unwrap();
        assert_eq!(
            settle(&mut fixture, work, &old_connection).await["result"]["registered"],
            true
        );
        let old_owner = fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .find(|binding| !binding.retired)
            .unwrap()
            .accounting_owner
            .clone();
        let queued_request = callback(
            "session-a",
            kind,
            (kind == "beforeSubmitPrompt").then_some("old-turn"),
            None,
        );
        let queued = fixture
            .service
            .prepare_external_helper_presentation(&queued_request, &old_connection)
            .unwrap();
        let native = queued.clone();
        let observed = tokio::task::spawn_blocking(move || native.observe())
            .await
            .unwrap();
        assert!(
            observed.is_ok(),
            "queued {kind} native proof failed: {:?}",
            observed.as_ref().err()
        );
        fixture.service.terminate_all_pane_processes().unwrap();
        fixture.service.start_initial_pane_process(None).unwrap();
        let fresh_root = fixture.service.pane_process_identity("%1").unwrap();
        assert!(!fresh_root.same_incarnation(&original_root));
        fixture.service.reconcile_external_agent_registrations();
        let (socket, connection) =
            launch_ordinary_fixture(&mut fixture.service, &fixture.listener, "hold")
                .await
                .unwrap();
        fixture.socket = socket;
        fixture.connection = connection;
        let (_new_socket, new_connection) = child(&mut fixture).await;
        let work = fixture
            .service
            .prepare_external_enrollment(&declaration("session-a"), &new_connection)
            .unwrap();
        assert_eq!(
            settle(&mut fixture, work, &new_connection).await["result"]["registered"],
            true
        );
        let fresh_owner = fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .find(|binding| !binding.retired)
            .unwrap()
            .accounting_owner
            .clone();
        assert_ne!(fresh_owner, old_owner);
        let refused: serde_json::Value = serde_json::from_str(
            &fixture
                .service
                .complete_external_enrollment(queued, observed, &old_connection),
        )
        .unwrap();
        assert!(
            refused.get("error").is_some(),
            "old {kind} work adopted fresh root: {refused}"
        );
        let old_end = callback("session-a", "sessionEnd", None, None);
        if let Ok(work) = fixture
            .service
            .prepare_external_helper_presentation(&old_end, &old_connection)
        {
            assert!(
                settle(&mut fixture, work, &old_connection)
                    .await
                    .get("error")
                    .is_some()
            );
        }
        let prompt = callback("session-a", "beforeSubmitPrompt", Some("fresh-turn"), None);
        let work = fixture
            .service
            .prepare_external_helper_presentation(&prompt, &new_connection)
            .unwrap();
        let accepted = settle(&mut fixture, work, &new_connection).await;
        assert_eq!(accepted["result"]["changed"], true, "{accepted}");
        let rows = fixture.service.external_agent_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["harness"], "cursor");
        assert_eq!(rows[0]["usage_coverage"], "unavailable-source-continuity");
        assert_eq!(
            fixture
                .service
                .live_pane_harness_status("%1")
                .unwrap()
                .text
                .as_deref(),
            Some("running")
        );
        let fresh_end = callback("session-a", "sessionEnd", None, None);
        let work = fixture
            .service
            .prepare_external_helper_presentation(&fresh_end, &new_connection)
            .unwrap();
        assert_eq!(
            settle(&mut fixture, work, &new_connection).await["result"]["retired"],
            true
        );
        assert!(fixture.service.external_agent_rows().is_empty());
        assert!(
            fixture
                .service
                .control
                .external_agents()
                .enrollments
                .pending
                .is_empty()
        );
        let database = rusqlite::Connection::open(usage).unwrap();
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
}
