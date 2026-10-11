//! First-epoch ordinary Codex turn observations under genuine native provenance.
//! Public turn/pane/session selectors do not authorize effects; original creator
//! and binding generation still fence every off-actor observation and settlement.

use super::curated_admission::curated_request;
use super::helper_presentation::{child, settle};
use super::*;

/// Constructs only the fixed initial Codex creator declaration, not callback
/// capabilities. Actual helper/parent/root evidence remains fixture-independent.
fn declaration() -> JsonRpcRequest {
    let mut request = curated_request();
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["harness"] = "codex".into();
    params["observer_kind"] = "command-hook".into();
    params["source_contract"] = "codex-local-command/1".into();
    params["observer_instance"] = "codex-local-hooks-1".into();
    request.params = Some(params.to_string());
    request
}

/// One bounded metadata-only callback, with no credential/generation or title.
fn callback(event: &str, turn: &str) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"codex-observe","method":"agent/external/codex-observe",
        "params":{"pane_id":"%1","external_session_id":"session-a","event":event,"turn_id":turn}}).to_string()).unwrap()
}

/// Genuine direct helpers update only their original fixed first-epoch source.
/// Daemon sequences conserve identical retries; known old-turn stops cannot
/// overwrite new turns, and a stop never downgrades interruption or claims
/// success. Rotation during native work rejects the old generation, while new
/// callbacks cannot automatically adopt a successor epoch. Reservations clear.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_observation_native_order_and_original_epoch_fences() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Codex observation qualification requires native peer evidence");
    };
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &connection)
        .unwrap();
    let first = settle(&mut fixture, work, &connection).await;
    assert_eq!(first["result"]["registered"], true);
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    let before = (binding.expires, binding.accounting_owner.clone());
    for (event, turn, sequence, changed) in [
        ("UserPromptSubmit", "turn-a", 1, true),
        ("UserPromptSubmit", "turn-a", 1, false),
        ("Stop", "turn-a", 2, true),
        ("Stop", "turn-a", 2, false),
        ("UserPromptSubmit", "turn-b", 3, true),
        ("Interrupt", "turn-b", 4, true),
        ("Stop", "turn-b", 4, false),
    ] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&callback(event, turn), &connection)
            .unwrap();
        let reply = settle(&mut fixture, work, &connection).await;
        assert_eq!(reply["result"]["sequence"], sequence, "{reply}");
        assert_eq!(reply["result"]["changed"], changed, "{reply}");
    }
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("Stop", "turn-a"), &connection)
        .unwrap();
    assert!(
        settle(&mut fixture, work, &connection)
            .await
            .get("error")
            .is_some()
    );
    assert_eq!(
        fixture
            .service
            .live_pane_harness_status("%1")
            .unwrap()
            .text
            .as_deref(),
        Some("interrupted")
    );
    let root_connection = fixture.connection.clone();
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &callback("UserPromptSubmit", "foreign"),
            &root_connection,
        )
        .unwrap();
    assert!(
        settle(&mut fixture, work, &root_connection)
            .await
            .get("error")
            .is_some()
    );
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!((binding.expires, binding.accounting_owner.clone()), before);
    let mut expired = fixture
        .service
        .prepare_external_helper_presentation(&callback("UserPromptSubmit", "expired"), &connection)
        .unwrap();
    expired.deadline = Instant::now();
    assert!(
        settle(&mut fixture, expired, &connection)
            .await
            .get("error")
            .is_some()
    );
    assert_eq!(
        fixture
            .service
            .live_pane_harness_status("%1")
            .unwrap()
            .text
            .as_deref(),
        Some("interrupted")
    );
    let stale = fixture
        .service
        .prepare_external_helper_presentation(&callback("UserPromptSubmit", "late"), &connection)
        .unwrap();
    let mut rotation = declaration();
    let end = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"end","method":"agent/external/codex-observe","params":{"pane_id":"%1","external_session_id":"session-a","event":"SessionEnd"}}).to_string()).unwrap();
    let stale_end = fixture
        .service
        .prepare_external_helper_presentation(&end, &connection)
        .unwrap();
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
    assert!(
        settle(&mut fixture, stale, &connection)
            .await
            .get("error")
            .is_some()
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&callback("Stop", "turn-b"), &connection)
            .is_err()
    );
    assert!(!connection.initialized());
    assert!(
        settle(&mut fixture, stale_end, &connection)
            .await
            .get("error")
            .is_some()
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&end, &connection)
            .is_err()
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| !binding.retired)
    );
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
    assert_eq!(fixture.service.control.external_agents().bindings.len(), 1);
}

/// Two concurrently admitted callbacks must preserve ingress order even when
/// later native work is attempted first. This initially reproduces independent
/// worker completion reversal; the ordered work owner must keep B behind A's
/// actor settlement, not merely behind its successful native observation.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_observation_overlapping_workers_preserve_ingress_order() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["registered"],
        true
    );
    let a = fixture
        .service
        .prepare_external_helper_presentation(&callback("UserPromptSubmit", "a"), &connection)
        .unwrap();
    let b = fixture
        .service
        .prepare_external_helper_presentation(&callback("UserPromptSubmit", "b"), &connection)
        .unwrap();
    let native_a = a.clone();
    let observed_a = tokio::task::spawn_blocking(move || native_a.observe())
        .await
        .unwrap();
    let waiting = b
        .helper
        .as_ref()
        .unwrap()
        .codex_order
        .as_ref()
        .unwrap()
        .waiting_for_tests();
    let native_b = b.clone();
    let pending_b = tokio::task::spawn_blocking(move || native_b.observe());
    tokio::time::timeout(Duration::from_secs(1), waiting.notified())
        .await
        .unwrap();
    assert!(
        !pending_b.is_finished(),
        "later native work overtook actor settlement"
    );
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(a, observed_a, &connection),
    )
    .unwrap();
    assert_eq!(reply["id"], "codex-observe");
    let observed_b = pending_b.await.unwrap();
    assert!(observed_b.is_ok(), "{observed_b:?}");
    fixture
        .service
        .complete_external_enrollment(b, observed_b, &connection);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("Stop", "b"), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        true
    );
}

/// Unknown fields and duplicate metadata reject before native reservation or
/// candidate effects. Credential, supplied sequence and title injection cannot
/// turn this narrow original callback method into general presentation control.
/// An interrupt admitted behind its prompt cannot fail merely because its
/// native worker starts first. A rejected predecessor must also release the
/// ordered worker, preventing callback lifetime/resource stalls after failures.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_observation_prompt_interrupt_and_rejection_release_order() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &connection)
        .unwrap();
    settle(&mut fixture, work, &connection).await;
    for reject in [false, true] {
        let turn = if reject { "rejected" } else { "prompt" };
        let a = fixture
            .service
            .prepare_external_helper_presentation(&callback("UserPromptSubmit", turn), &connection)
            .unwrap();
        let b = fixture
            .service
            .prepare_external_helper_presentation(
                &callback(
                    if reject {
                        "UserPromptSubmit"
                    } else {
                        "Interrupt"
                    },
                    if reject { "after-rejection" } else { turn },
                ),
                &connection,
            )
            .unwrap();
        let waiting = b
            .helper
            .as_ref()
            .unwrap()
            .codex_order
            .as_ref()
            .unwrap()
            .waiting_for_tests();
        let native_b = b.clone();
        let pending = tokio::task::spawn_blocking(move || native_b.observe());
        tokio::time::timeout(Duration::from_secs(1), waiting.notified())
            .await
            .unwrap();
        let observed = if reject {
            Err(MezError::conflict("fixture rejected native work"))
        } else {
            let native_a = a.clone();
            tokio::task::spawn_blocking(move || native_a.observe())
                .await
                .unwrap()
        };
        fixture
            .service
            .complete_external_enrollment(a, observed, &connection);
        let observed = pending.await.unwrap();
        assert!(observed.is_ok(), "{observed:?}");
        let reply: serde_json::Value = serde_json::from_str(
            &fixture
                .service
                .complete_external_enrollment(b, observed, &connection),
        )
        .unwrap();
        assert_eq!(reply["result"]["changed"], true, "{reply}");
        assert_eq!(
            fixture
                .service
                .live_pane_harness_status("%1")
                .unwrap()
                .text
                .as_deref(),
            Some(if reject { "running" } else { "interrupted" })
        );
    }
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

/// Unknown fields and duplicate metadata reject before native reservation or
/// candidate effects. Credential, supplied sequence and title injection cannot
/// turn this narrow original callback method into general presentation control.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_observation_rejects_control_fields_and_unqualified_writer() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    for field in ["launch_token", "generation", "sequence", "title", "harness"] {
        let mut request = callback("UserPromptSubmit", "turn-a");
        let mut params: serde_json::Value =
            serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
        params[field] = "PRIVATE".into();
        request.params = Some(params.to_string());
        assert!(
            fixture
                .service
                .prepare_external_helper_presentation(&request, &fixture.connection)
                .is_err()
        );
    }
    let mut duplicate = callback("Stop", "turn-a");
    duplicate.params = Some(r#"{"pane_id":"%1","external_session_id":"session-a","event":"Stop","turn_id":"a","turn_id":"b"}"#.into());
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&duplicate, &fixture.connection)
            .is_err()
    );
    let connection = ControlConnectionState::new(true, false);
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&callback("Stop", "turn-a"), &connection)
            .is_err()
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
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

/// SessionEnd retires only its native-qualified original first-epoch session,
/// not its live creator or another session. Later queued work and same-source
/// re-enrollment remain fenced without manufacturing completion or billing.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_session_end_retires_only_original_namespace() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let store =
        crate::storage::token_usage::TokenUsageStore::new(fixture.directory.join("usage.sqlite"));
    fixture.service.set_token_usage_store(store.clone());
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &connection)
        .unwrap();
    settle(&mut fixture, work, &connection).await;
    // Existing admitted accounting is seeded through the shared ledger fixture,
    // not through Codex callbacks or provider work; retirement must conserve it.
    let owner = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .accounting_owner
        .clone();
    let marker = "a".repeat(64);
    let now = current_unix_seconds();
    store
        .begin_external_request(&owner, "codex", &marker, None, now)
        .unwrap();
    let report = crate::storage::token_usage::ExternalUsageReport {
        owner,
        project: None,
        harness: "codex".into(),
        epoch: "fixture".into(),
        event_id: "b".repeat(64),
        sequence: 1,
        mode: "observation".into(),
        baseline: false,
        observed_at: now,
        model: mez_agent::ModelTokenUsageKey::new("fixture", "model"),
        counters: crate::storage::token_usage::ExternalCounters {
            input_tokens: 10,
            output_tokens: 3,
            ..Default::default()
        },
    };
    let frozen = store
        .freeze_external_request_report(&report, &marker)
        .unwrap();
    assert!(store.ingest_external_request(&frozen, now).unwrap().applied);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("UserPromptSubmit", "a"), &connection)
        .unwrap();
    settle(&mut fixture, work, &connection).await;
    let end = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"end","method":"agent/external/codex-observe","params":{"pane_id":"%1","external_session_id":"session-a","event":"SessionEnd"}}).to_string()).unwrap();
    let ended = fixture
        .service
        .prepare_external_helper_presentation(&end, &connection)
        .unwrap();
    let late = fixture
        .service
        .prepare_external_helper_presentation(&callback("UserPromptSubmit", "late"), &connection)
        .unwrap();
    let reply = settle(&mut fixture, ended, &connection).await;
    assert_eq!(reply["result"]["retired"], true, "{reply}");
    assert!(
        settle(&mut fixture, late, &connection)
            .await
            .get("error")
            .is_some()
    );
    assert!(fixture.service.external_agent_rows().is_empty());
    assert!(fixture.service.live_pane_harness_status("%1").is_none());
    assert!(fixture.connection.unix_origin().unwrap().is_live());
    let database = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT sum(input_tokens) FROM token_usage_events",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        10
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&end, &connection)
            .is_err()
    );
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &connection)
        .unwrap();
    assert!(
        settle(&mut fixture, work, &connection)
            .await
            .get("error")
            .is_some()
    );
    let mut other = declaration();
    let mut params: serde_json::Value =
        serde_json::from_str(other.params.as_deref().unwrap()).unwrap();
    params["external_session_id"] = "session-b".into();
    other.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_enrollment(&other, &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["registered"],
        true
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&end, &connection)
            .is_err()
    );
    assert_eq!(fixture.service.external_agent_rows().len(), 1);
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
