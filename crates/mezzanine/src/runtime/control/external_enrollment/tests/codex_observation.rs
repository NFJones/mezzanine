//! First-epoch ordinary Codex turn observations under genuine native provenance.
//! Public turn/pane/session selectors do not authorize effects; original creator
//! and binding generation still fence every off-actor observation and settlement.

use super::curated_admission::curated_request;
use super::helper_presentation::{child, settle};
use super::*;

/// Constructs only the fixed initial Codex creator declaration, not callback
/// capabilities. Actual helper/parent/root evidence remains fixture-independent.
pub(super) fn declaration() -> JsonRpcRequest {
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
pub(super) fn callback(event: &str, turn: &str) -> JsonRpcRequest {
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
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(
                &callback("UserPromptSubmit", "foreign"),
                &root_connection,
            )
            .is_err()
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

/// Fresh native-authorized callback facts provide only finite observer proof.
/// Daemon maintenance can renew while proof is recent; creator survival or an
/// identical replay cannot revive expired proof or claim continuous idle health.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_callback_freshness_is_finite_and_replay_time_inert() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &connection)
        .unwrap();
    settle(&mut fixture, work, &connection).await;
    assert_eq!(
        fixture.service.external_agent_rows()[0]["telemetry_health"],
        "awaiting-first-proof"
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("UserPromptSubmit", "a"), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        true
    );
    assert_eq!(
        fixture.service.external_agent_rows()[0]["telemetry_health"],
        "enrolled"
    );
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    let short = current_unix_seconds() + 10;
    fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .get_mut(&digest)
        .unwrap()
        .expires = short;
    fixture.service.renew_connected_external_observers();
    let renewed = fixture.service.control.external_agents().bindings[&digest].expires;
    assert!(renewed > short);
    let aged = Instant::now() - Duration::from_secs(31);
    fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .get_mut(&digest)
        .unwrap()
        .enrollment
        .as_mut()
        .unwrap()
        .curated_observer
        .as_mut()
        .unwrap()
        .observed_at = Some(aged);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("UserPromptSubmit", "a"), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        false
    );
    let proof = fixture.service.control.external_agents().bindings[&digest]
        .enrollment
        .as_ref()
        .unwrap()
        .curated_observer
        .as_ref()
        .unwrap();
    assert_eq!(proof.observed_at, Some(aged));
    let mut failed = fixture
        .service
        .prepare_external_helper_presentation(
            &callback("UserPromptSubmit", "failed-proof"),
            &connection,
        )
        .unwrap();
    failed.deadline = Instant::now();
    assert!(
        settle(&mut fixture, failed, &connection)
            .await
            .get("error")
            .is_some()
    );
    assert_eq!(
        fixture.service.control.external_agents().bindings[&digest]
            .enrollment
            .as_ref()
            .unwrap()
            .curated_observer
            .as_ref()
            .unwrap()
            .observed_at,
        Some(aged)
    );
    assert_eq!(
        fixture.service.external_agent_rows()[0]["telemetry_health"],
        "connection-lost"
    );
    assert!(fixture.connection.unix_origin().unwrap().is_live());
    fixture.service.renew_connected_external_observers();
    assert_eq!(
        fixture.service.control.external_agents().bindings[&digest].expires,
        renewed
    );
}

/// Genuine permission/tool callbacks update only their already admitted turn.
/// Replays are sequence-inert; missing prompts, retired turns and interrupted
/// turns cannot be resurrected by activity. All updates cross native parent/root
/// proof and preserve the original accounting owner and finite lease boundary.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_activity_native_wait_resume_and_old_turn_fences() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Codex activity qualification requires native peer evidence");
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
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    let original = (binding.expires, binding.accounting_owner.clone());
    for (event, turn, sequence, changed) in [
        ("UserPromptSubmit", "turn-a", 1, true),
        ("PreToolUse", "turn-a", 1, false),
        ("PermissionRequest", "turn-a", 2, true),
        ("PermissionRequest", "turn-a", 2, false),
        ("PostToolUse", "turn-a", 3, true),
        ("Stop", "turn-a", 4, true),
        ("PreToolUse", "turn-a", 5, true),
        ("Interrupt", "turn-a", 6, true),
        ("PostToolUse", "turn-a", 6, false),
        ("PermissionRequest", "turn-a", 6, false),
        ("UserPromptSubmit", "turn-b", 7, true),
    ] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&callback(event, turn), &connection)
            .unwrap();
        let reply = settle(&mut fixture, work, &connection).await;
        assert_eq!(reply["result"]["sequence"], sequence, "{reply}");
        assert_eq!(reply["result"]["changed"], changed, "{reply}");
        if event == "PermissionRequest" && sequence == 2 {
            assert_eq!(
                fixture
                    .service
                    .live_pane_harness_status("%1")
                    .unwrap()
                    .text
                    .as_deref(),
                Some("approval-wait")
            );
        }
    }
    for turn in ["turn-a", "unknown"] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&callback("PermissionRequest", turn), &connection)
            .unwrap();
        assert!(
            settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_some()
        );
    }
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(
        (binding.expires, binding.accounting_owner.clone()),
        original
    );
    assert!(!binding.enrollment.as_ref().unwrap().request_reports_seen);
}

/// Ordinary model callbacks acquire readiness from their actual native pre
/// writer, then settle immutable inclusive counters through the shared ledger.
/// A repeated post from that same process conserves expense; a later distinct
/// pre/post pair with identical counts remains a distinct incurred request.
/// Missing readiness and overlapping pre writers never borrow a newer origin.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_model_requests_native_pairing_replay_and_overlap() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Codex model qualification requires native peer evidence");
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let (_source_socket, source) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &source)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &source).await["result"]["registered"],
        true
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("UserPromptSubmit", "turn-a"), &source)
        .unwrap();
    assert!(
        settle(&mut fixture, work, &source)
            .await
            .get("error")
            .is_none()
    );
    let post = crate::control::parse_json_rpc_request(
        &serde_json::json!({
            "jsonrpc":"2.0","id":"post","method":"agent/external/codex-observe",
            "params":{"pane_id":"%1","external_session_id":"session-a","event":"PostModelRequest",
                "turn_id":"turn-a","counters":{"input_tokens":10,"output_tokens":3,
                    "cached_input_tokens":2,"reasoning_tokens":1}}
        })
        .to_string(),
    )
    .unwrap();
    let (_orphan_socket, orphan) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&post, &orphan)
        .unwrap();
    let reply = settle(&mut fixture, work, &orphan).await;
    assert_ne!(reply["result"]["durable"], true, "{reply}");
    // Keep native sockets alive until their own observation and settlement end.
    let mut sockets = Vec::new();
    for _ in 0..2 {
        let (pre_socket, pre) = child(&mut fixture).await;
        let work = fixture
            .service
            .prepare_external_helper_presentation(&callback("PreModelRequest", "turn-a"), &pre)
            .unwrap();
        let reply = settle(&mut fixture, work, &pre).await;
        assert_eq!(reply["result"]["observed"], true, "{reply}");
        let (post_socket, connection) = child(&mut fixture).await;
        for replay in [false, true] {
            let work = fixture
                .service
                .prepare_external_helper_presentation(&post, &connection)
                .unwrap();
            let reply = settle(&mut fixture, work, &connection).await;
            assert_eq!(reply["result"]["durable"], true, "{reply}");
            assert_eq!(reply["result"]["applied"], !replay, "{reply}");
        }
        sockets.extend([pre_socket, post_socket]);
    }
    let (missing_pre_socket, missing_pre) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("PreModelRequest", "turn-a"), &missing_pre)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &missing_pre).await["result"]["observed"],
        true
    );
    let mut missing_post = post.clone();
    let mut params: serde_json::Value =
        serde_json::from_str(missing_post.params.as_deref().unwrap()).unwrap();
    params["counters"] = serde_json::Value::Null;
    missing_post.params = Some(params.to_string());
    let (missing_socket, missing) = child(&mut fixture).await;
    for _ in 0..2 {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&missing_post, &missing)
            .unwrap();
        assert_ne!(
            settle(&mut fixture, work, &missing).await["result"]["durable"],
            true
        );
        let source = fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .enrollment
            .as_ref()
            .unwrap();
        assert_eq!(
            source.codex_request_gap_coverage(),
            Some("unavailable-codex-usage-finalized-counters")
        );
    }
    sockets.extend([missing_pre_socket, missing_socket]);
    for _ in 0..2 {
        let (socket, connection) = child(&mut fixture).await;
        let work = fixture
            .service
            .prepare_external_helper_presentation(
                &callback("PreModelRequest", "turn-a"),
                &connection,
            )
            .unwrap();
        // No post may capture old custody while this preceding pre is still
        // uncommitted, even though the common worker can otherwise queue work.
        assert!(
            fixture
                .service
                .prepare_external_helper_presentation(&post, &orphan)
                .is_err()
        );
        assert!(
            settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_none()
        );
        let replay = fixture
            .service
            .prepare_external_helper_presentation(&missing_post, &missing)
            .unwrap();
        assert_ne!(
            settle(&mut fixture, replay, &missing).await["result"]["durable"],
            true
        );
        sockets.push(socket);
    }
    let (_late_socket, late) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&post, &late)
        .unwrap();
    assert_ne!(
        settle(&mut fixture, work, &late).await["result"]["durable"],
        true
    );
    let database = rusqlite::Connection::open(&path).unwrap();
    let totals: (i64, i64, i64) = database
        .query_row(
            "SELECT count(*),sum(input_tokens),sum(output_tokens) FROM token_usage_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(totals, (2, 20, 6));
    assert_eq!(
        fixture
            .service
            .live_pane_harness_status("%1")
            .unwrap()
            .text
            .as_deref(),
        Some("running")
    );
}
