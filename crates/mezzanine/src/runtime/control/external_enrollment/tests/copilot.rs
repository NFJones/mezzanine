//! Classic command-hook native source and original-session Copilot qualification.
//!
//! Genuine fixture processes provide all native facts; metadata supplies no PID,
//! credential or client role. Installed acceptance invokes production hook bytes.

use super::curated_admission::curated_request;
use super::helper_presentation::{child, settle};
use super::*;

/// Distinct classic declaration; vendor labels are inert native-work selectors.
fn declaration(session: &str) -> JsonRpcRequest {
    let mut request = curated_request();
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["harness"] = "copilot".into();
    params["observer_kind"] = "command-hook".into();
    params["source_contract"] = "copilot-local-command/1".into();
    params["observer_instance"] = "copilot-local-hooks-1".into();
    params["external_session_id"] = session.into();
    request.params = Some(params.to_string());
    request
}

/// Fixed local observation; no payload state/title/credentials can cross ingress.
fn callback(session: &str, event: &str, timestamp: u64) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"copilot-test","method":"agent/external/copilot-observe","params":{"pane_id":"%1","external_session_id":session,"event":event,"timestamp":timestamp}}).to_string()).unwrap()
}

/// Attachment has no event, claimed boundary, generation, credential or PID.
/// The indexed candidate still needs the worker's original parent/root proof.
fn attachment(session: &str) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(
        &serde_json::json!({"jsonrpc":"2.0","id":"copilot-attach-test",
            "method":"agent/external/copilot-attach",
            "params":{"pane_id":"%1","external_session_id":session}})
        .to_string(),
    )
    .unwrap()
}

/// Original public handles and an inert instance/counter, never a supplied PID.
fn pulse(enrolled: &serde_json::Value, nonce: &str, sequence: u64) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"copilot-pulse-test",
        "method":"agent/external/copilot-pulse","params":{"pane_id":"%1","external_session_id":"session-a",
        "generation":enrolled["result"]["generation"],"observer_witness":enrolled["result"]["observer_witness"],
        "observer_id":nonce,"sequence":sequence}}).to_string()).unwrap()
}

/// A real extension child can supply finite proof independently of classic
/// event/presentation counters. Identical/stale pulses cannot refresh its clock,
/// another child or nonce cannot borrow its lifetime, and only daemon maintenance
/// renews the lease. Actual writer death invalidates proof while the CLI survives;
/// a newly qualified child may replace only that dead observer, not the source.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_idle_pulses_have_exact_writer_and_independent_replay() {
    use super::helper_presentation::release;
    let mut fixture = fixture("hold").await.expect("native Copilot fixture");
    let (mut socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration("session-a"), &connection)
        .unwrap();
    let first = settle(&mut fixture, work, &connection).await;
    let a = "a".repeat(64);
    let b = "b".repeat(64);
    for sequence in [0, 2] {
        let request = pulse(&first, &a, sequence);
        match fixture
            .service
            .prepare_external_helper_presentation(&request, &connection)
        {
            Ok(work) => assert!(
                settle(&mut fixture, work, &connection)
                    .await
                    .get("error")
                    .is_some()
            ),
            Err(_) => assert_eq!(sequence, 0),
        }
    }
    let work = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&first, &a, 1), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        true
    );
    for (event, timestamp) in [("userPromptSubmitted", 1), ("agentStop", 2)] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(
                &callback("session-a", event, timestamp),
                &connection,
            )
            .unwrap();
        assert!(
            settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_none()
        );
    }
    let registry = fixture.service.control.external_agents_mut();
    let binding = registry.bindings.values_mut().next().unwrap();
    let original = (
        binding.generation,
        binding.accounting_owner.clone(),
        binding.expires,
    );
    let source = binding.enrollment.as_mut().unwrap();
    source.curated_observer.as_mut().unwrap().observed_at =
        Some(Instant::now() - Duration::from_secs(120));
    source.copilot_events.age_extension_for_test();
    let aged = source.copilot_events.extension_snapshot_for_test();
    assert!(!source.has_live_observer());
    let work = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&first, &a, 1), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        false
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
    assert_eq!(source.copilot_events.extension_snapshot_for_test(), aged);
    assert!(!source.has_live_observer());
    let work = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&first, &a, 2), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        true
    );
    let (mut other_socket, other) = child(&mut fixture).await;
    for (who, nonce, sequence) in [(&other, &a, 3), (&connection, &b, 1), (&connection, &a, 1)] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&pulse(&first, nonce, sequence), who)
            .unwrap();
        assert!(settle(&mut fixture, work, who).await.get("error").is_some());
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
        (
            binding.generation,
            binding.accounting_owner.clone(),
            binding.expires
        ),
        original
    );
    let source = binding.enrollment.as_ref().unwrap();
    assert!(source.has_live_observer());
    assert_eq!(source.curated_observer.as_ref().unwrap().sequence, 2);
    assert_eq!(
        binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .as_ref()
            .unwrap()
            .sequence,
        2
    );
    fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap()
        .expires = current_unix_seconds() - 1;
    fixture.service.renew_connected_external_observers();
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .expires
            > current_unix_seconds()
    );
    let late = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&first, &a, 3), &connection)
        .unwrap();
    release(&mut socket, &connection).await;
    assert!(
        !fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .enrollment
            .as_ref()
            .unwrap()
            .has_live_observer()
    );
    assert!(
        settle(&mut fixture, late, &connection)
            .await
            .get("error")
            .is_some()
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&first, &b, 1), &other)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &other).await["result"]["changed"],
        true
    );
    release(&mut other_socket, &other).await;
    assert!(
        !fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .enrollment
            .as_ref()
            .unwrap()
            .has_live_observer()
    );
    assert!(fixture.connection.unix_origin().unwrap().is_live());
}

/// A real direct child may read the existing original public observer selectors
/// after native verification. Repeating attachment cannot allocate another run,
/// repaint status, refresh freshness, renew a lease or create expense. The pane
/// root itself and a different session cannot borrow that child's relationship.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_attachment_is_native_and_time_inert() {
    let mut fixture = fixture("hold").await.expect("native Copilot fixture");
    let (_socket, connection) = child(&mut fixture).await;
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&attachment("session-a"), &connection,)
            .is_err()
    );
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration("session-a"), &connection)
        .unwrap();
    let enrolled = settle(&mut fixture, work, &connection).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &callback("session-a", "userPromptSubmitted", 1),
            &connection,
        )
        .unwrap();
    settle(&mut fixture, work, &connection).await;
    let before = {
        let binding = fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap();
        let observer = binding
            .enrollment
            .as_ref()
            .unwrap()
            .curated_observer
            .as_ref()
            .unwrap();
        (
            binding.generation,
            binding.accounting_owner.clone(),
            binding.expires,
            observer.sequence,
            observer.observed_at,
        )
    };
    for _ in 0..2 {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&attachment("session-a"), &connection)
            .unwrap();
        let reply = settle(&mut fixture, work, &connection).await;
        assert_eq!(reply["id"], "copilot-attach-test");
        assert_eq!(reply["result"], enrolled["result"]);
        assert!(reply["result"].get("launch_token").is_none());
    }
    let root = fixture.connection.clone();
    let work = fixture
        .service
        .prepare_external_helper_presentation(&attachment("session-a"), &root)
        .unwrap();
    assert!(
        settle(&mut fixture, work, &root)
            .await
            .get("error")
            .is_some()
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&attachment("other-session"), &connection,)
            .is_err()
    );
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 1);
    let binding = registry.bindings.values().next().unwrap();
    let observer = binding
        .enrollment
        .as_ref()
        .unwrap()
        .curated_observer
        .as_ref()
        .unwrap();
    assert_eq!(
        (
            binding.generation,
            binding.accounting_owner.clone(),
            binding.expires,
            observer.sequence,
            observer.observed_at
        ),
        before
    );
    let presentation = binding
        .registration
        .as_ref()
        .unwrap()
        .presentation
        .as_ref()
        .unwrap();
    assert_eq!(presentation.sequence, 1);
    assert_eq!(presentation.state, "running");
    assert!(registry.enrollments.pending.is_empty());
    assert!(!connection.initialized());
    assert!(connection.caller_client_id().is_none());
}

/// Rotation while native lifecycle/end work is pending fences the original
/// generation. Stateless callbacks cannot adopt the successor even with the same
/// creator/session metadata; rejection must not retire the live replacement.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_rotation_rejects_pending_and_new_original_callbacks() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Copilot rotation requires native evidence");
    };
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration("session-a"), &connection)
        .unwrap();
    let first = settle(&mut fixture, work, &connection).await;
    assert_eq!(first["result"]["registered"], true);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&first, &"a".repeat(64), 1), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        true
    );
    let stale = fixture
        .service
        .prepare_external_helper_presentation(
            &callback("session-a", "userPromptSubmitted", 1),
            &connection,
        )
        .unwrap();
    let stale_end = fixture
        .service
        .prepare_external_helper_presentation(&callback("session-a", "sessionEnd", 2), &connection)
        .unwrap();
    let stale_attachment = fixture
        .service
        .prepare_external_helper_presentation(&attachment("session-a"), &connection)
        .unwrap();
    let stale_pulse = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&first, &"a".repeat(64), 2), &connection)
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
    for work in [stale, stale_end, stale_attachment, stale_pulse] {
        assert!(
            settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_some()
        );
    }
    for event in ["agentStop", "sessionEnd"] {
        assert!(
            fixture
                .service
                .prepare_external_helper_presentation(&callback("session-a", event, 3), &connection)
                .is_err()
        );
    }
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&attachment("session-a"), &connection,)
            .is_err()
    );
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 1);
    assert!(registry.bindings.values().all(|b| !b.retired));
    assert!(
        registry
            .bindings
            .values()
            .all(|b| !b.enrollment.as_ref().unwrap().has_live_observer())
    );
    assert!(registry.enrollments.pending.is_empty());
    assert!(!connection.initialized());
    assert!(connection.caller_client_id().is_none());
}

/// Documented classic Copilot exec hooks carry a session identifier without SDK
/// invocation context. A genuine same-user direct creator should enroll under
/// its own contract; a Codex or Claude declaration is not a substitute.
///
/// A successful declaration is only observational registration, not accounting
/// or persistent idle observer health.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_classic_command_creator_is_supported() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Copilot creator requires native peer evidence");
    };
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration("session-a"), &connection)
        .unwrap();
    let reply = settle(&mut fixture, work, &connection).await;
    assert_eq!(reply["result"]["registered"], true, "{reply}");
}

/// Original native callbacks conserve replay sequences, reject root/foreign
/// writers, invalid semantics and stale/tied facts, and never grant a client role
/// or renew leases directly. Ending an old /clear session preserves replacement.
///
/// Observer replacement itself is covered separately to distinguish a
/// different session from a changed epoch of this exact original session.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_native_lifecycle_replay_and_exact_end() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Copilot lifecycle requires native evidence");
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
    for (event, time, state, seq) in [
        ("userPromptSubmitted", 1, "running", 1),
        ("permission_prompt", 2, "approval-wait", 2),
        ("postToolUse", 3, "running", 3),
        ("elicitation_dialog", 4, "input-wait", 4),
        ("preToolUse", 5, "running", 5),
        ("agentStop", 6, "ready", 6),
        ("errorOccurred", 7, "interrupted", 7),
    ] {
        for changed in [true, false] {
            let work = fixture
                .service
                .prepare_external_helper_presentation(
                    &callback("session-a", event, time),
                    &connection,
                )
                .unwrap();
            let reply = settle(&mut fixture, work, &connection).await;
            assert_eq!(reply["id"], "copilot-test");
            assert_eq!(reply["result"]["sequence"], seq, "{reply}");
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
    }
    for time in [6, 7] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(
                &callback("session-a", "agentStop", time),
                &connection,
            )
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
                &callback("session-a", "permissionRequest", 8),
                &connection
            )
            .is_err()
    );
    let root = fixture.connection.clone();
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("session-a", "agentStop", 8), &root)
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
    assert!(!connection.initialized());
    assert!(connection.caller_client_id().is_none());
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
            &callback("session-b", "userPromptSubmitted", 10),
            &connection,
        )
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["changed"],
        true
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("session-a", "sessionEnd", 11), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["retired"],
        true
    );
    let bindings = &fixture.service.control.external_agents().bindings;
    assert!(
        bindings
            .values()
            .find(|b| b.registration.as_ref().unwrap().external_session_id == "session-a")
            .unwrap()
            .retired
    );
    assert!(
        !bindings
            .values()
            .find(|b| b.registration.as_ref().unwrap().external_session_id == "session-b")
            .unwrap()
            .retired
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(
                &callback("session-a", "agentStop", 12),
                &connection
            )
            .is_err()
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

/// Opens an original classic source, then a distinct real native extension
/// child. The shared store is selected only by the fixture owner before native
/// enrollment; no callback supplies a path, PID, role or accounting namespace.
async fn terminal_fixture(
    shared: Option<&std::path::Path>,
) -> (
    Fixture,
    tokio::net::UnixStream,
    ControlConnectionState,
    serde_json::Value,
    String,
    std::path::PathBuf,
) {
    let mut fixture = fixture("hold")
        .await
        .expect("native Copilot terminal fixture");
    let path = shared
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| fixture.directory.join("terminal.sqlite"));
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let (mut hook, hook_connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration("session-a"), &hook_connection)
        .unwrap();
    let enrolled = settle(&mut fixture, work, &hook_connection).await;
    assert!(enrolled.get("error").is_none(), "{enrolled}");
    super::helper_presentation::release(&mut hook, &hook_connection).await;
    let (socket, connection) = child(&mut fixture).await;
    let nonce = "a".repeat(64);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&enrolled, &nonce, 1), &connection)
        .unwrap();
    let reply = settle(&mut fixture, work, &connection).await;
    assert_eq!(reply["result"]["changed"], true);
    (fixture, socket, connection, enrolled, nonce, path)
}

/// This payload already contains a content-redacted opaque identity. Future
/// client tests must independently prove full source-session/sequence/usage-ID
/// hashing; this native fixture never treats a digest as process authority.
fn terminal(enrolled: &serde_json::Value, nonce: &str, call: &str) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(
        &serde_json::json!({
            "jsonrpc":"2.0","id":"copilot-terminal-test","method":"agent/external/copilot-terminal",
            "params":{"pane_id":"%1","external_session_id":"session-a",
                "generation":enrolled["result"]["generation"],
                "observer_witness":enrolled["result"]["observer_witness"],
                "observer_id":nonce,"call_id":call,"model":"fixture-model",
                "counters":{"input_tokens":10,"output_tokens":4,"cached_input_tokens":3}}
        })
        .to_string(),
    )
    .unwrap()
}

/// Reads only fixture expense/alias/request counts, not response envelopes.
fn terminal_counts(path: &std::path::Path) -> (i64, i64, i64) {
    let db = rusqlite::Connection::open(path).unwrap();
    (
        db.query_row("SELECT count(*) FROM token_usage_events", [], |row| {
            row.get(0)
        })
        .unwrap(),
        db.query_row("SELECT count(*) FROM external_copilot_calls", [], |row| {
            row.get(0)
        })
        .unwrap(),
        db.query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
            row.get(0)
        })
        .unwrap(),
    )
}

/// Real native terminal publication/replay updates usage coverage only. It must
/// preserve original lease, pulse/classic clocks and presentation, retain unknown
/// original project and keep known gaps sticky after later successful calls.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_terminal_native_receipts_are_time_inert_and_gaps_sticky() {
    let (mut fixture, _socket, connection, enrolled, nonce, path) = terminal_fixture(None).await;
    let snapshot = |fixture: &Fixture| {
        let binding = fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap();
        let source = binding.enrollment.as_ref().unwrap();
        (
            binding.generation,
            binding.expires,
            source.copilot_events.extension_snapshot_for_test(),
            source.curated_observer.as_ref().unwrap().sequence,
            source.curated_observer.as_ref().unwrap().observed_at,
        )
    };
    let before = snapshot(&fixture);
    for applied in [true, false] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(
                &terminal(&enrolled, &nonce, &"b".repeat(64)),
                &connection,
            )
            .unwrap();
        let reply = settle(&mut fixture, work, &connection).await;
        assert_eq!(reply["result"]["durable"], true, "{reply}");
        assert_eq!(reply["result"]["applied"], applied);
        assert_eq!(reply["result"]["coverage"]["origin"], "unattributed");
        assert_eq!(snapshot(&fixture), before);
    }
    assert_eq!(terminal_counts(&path), (1, 1, 0));
    assert_eq!(
        fixture.service.external_agent_rows()[0]["usage_coverage"],
        "terminal-observation-external-reported-unattributed"
    );
    let mut gap = terminal(&enrolled, &nonce, &"b".repeat(64));
    gap.method = "agent/external/copilot-usage-gap".into();
    let mut params: serde_json::Value =
        serde_json::from_str(gap.params.as_deref().unwrap()).unwrap();
    for field in ["call_id", "model", "counters"] {
        params.as_object_mut().unwrap().remove(field);
    }
    params["reason"] = "queue-pressure".into();
    gap.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_helper_presentation(&gap, &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["observed"],
        true
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &terminal(&enrolled, &nonce, &"c".repeat(64)),
            &connection,
        )
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["applied"],
        true
    );
    assert_eq!(
        fixture.service.external_agent_rows()[0]["usage_coverage"],
        "partial-terminal-observations-unattributed"
    );
    assert_eq!(snapshot(&fixture), before);
    assert_eq!(terminal_counts(&path), (2, 2, 0));
    let db = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM token_usage_events WHERE project_id IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
}

/// A different genuine child has the right CLI parent/root but cannot borrow
/// the concrete extension's nonce. Invalid schema/selectors/counters also refuse
/// before storage, and every failed worker releases its original reservations.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_terminal_rejects_other_writer_and_invalid_metadata() {
    let (mut fixture, _socket, connection, enrolled, nonce, path) = terminal_fixture(None).await;
    let (_other_socket, other) = child(&mut fixture).await;
    let request = terminal(&enrolled, &nonce, &"d".repeat(64));
    let work = fixture
        .service
        .prepare_external_helper_presentation(&request, &other)
        .unwrap();
    assert!(
        settle(&mut fixture, work, &other)
            .await
            .get("error")
            .is_some()
    );
    for field in ["pid", "project", "observed_at", "cost", "prompt"] {
        let mut request = request.clone();
        let mut params: serde_json::Value =
            serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
        params[field] = "PRIVATE".into();
        request.params = Some(params.to_string());
        assert!(
            fixture
                .service
                .prepare_external_helper_presentation(&request, &connection)
                .is_err()
        );
    }
    for (field, value) in [
        ("observer_id", serde_json::json!("e".repeat(64))),
        ("generation", serde_json::json!(0)),
        ("pane_id", serde_json::json!("%999")),
        ("observer_witness", serde_json::json!("f".repeat(64))),
        ("call_id", serde_json::json!("not-a-digest")),
        (
            "counters",
            serde_json::json!({"input_tokens":1,"output_tokens":2,"cached_input_tokens":3}),
        ),
        (
            "counters",
            serde_json::json!({"input_tokens":9007199254740992_u64,"output_tokens":2}),
        ),
    ] {
        let mut request = request.clone();
        let mut params: serde_json::Value =
            serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
        params[field] = value;
        request.params = Some(params.to_string());
        assert!(
            fixture
                .service
                .prepare_external_helper_presentation(&request, &connection)
                .is_err()
        );
    }
    assert_eq!(terminal_counts(&path), (0, 0, 0));
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert!(
        fixture
            .service
            .control
            .external_usage()
            .reservations
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

/// A successful-looking generic worker Result is not a terminal publication.
/// The fixed scope rejects generic execution, consumes forged completion safely,
/// and still permits a later genuine native request to commit the call once.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_terminal_requires_actual_typed_worker_publication() {
    let (mut fixture, _socket, connection, enrolled, nonce, path) = terminal_fixture(None).await;
    let request = terminal(&enrolled, &nonce, &"e".repeat(64));
    let work = fixture
        .service
        .prepare_external_helper_presentation(&request, &connection)
        .unwrap();
    let end = work
        .helper
        .as_ref()
        .unwrap()
        .usage_end
        .as_ref()
        .unwrap()
        .clone();
    assert!(end.execute().is_err());
    let forged = crate::storage::token_usage::ExternalUsageCommit {
        stream_id: crate::storage::token_usage::external_usage_stream_id(
            &end.report.owner,
            &end.report.epoch,
        )
        .unwrap(),
        revision: 1,
        totals: end.report.counters,
        applied: true,
    };
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_usage((*end).clone(), Ok(forged)),
    )
    .unwrap();
    assert!(reply.get("error").is_some());
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, Ok(()), &connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some());
    assert_eq!(terminal_counts(&path), (0, 0, 0));
    let work = fixture
        .service
        .prepare_external_helper_presentation(&request, &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["applied"],
        true
    );
    assert_eq!(terminal_counts(&path), (1, 1, 0));
}

/// Actual storage publication survives retirement between worker and actor
/// settlement. The stale callback gets no ACK or revived registration, while
/// incurred expense and original session totals remain and capacity is released.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_terminal_retirement_preserves_published_expense() {
    let (mut fixture, _socket, connection, enrolled, nonce, path) = terminal_fixture(None).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &terminal(&enrolled, &nonce, &"f".repeat(64)),
            &connection,
        )
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    assert_eq!(terminal_counts(&path), (1, 1, 0));
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    fixture.service.retire_external_agent_binding(digest);
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some());
    assert!(fixture.service.external_agent_rows().is_empty());
    assert_eq!(
        fixture
            .service
            .external_token_usage(None)
            .values()
            .map(|u| u.input_tokens)
            .sum::<u64>(),
        10
    );
    assert_eq!(terminal_counts(&path), (1, 1, 0));
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
}

/// Two native pane creators and concrete children use distinct mapped owners
/// but the same canonical call. The second must ACK existing expense without
/// projecting the first pane's checkpoint or allocating another expense row.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_terminal_cross_owner_ack_has_no_foreign_projection() {
    let (mut a, _a_socket, a_connection, a_enrolled, a_nonce, path) = terminal_fixture(None).await;
    let (mut b, _b_socket, b_connection, b_enrolled, b_nonce, _) =
        terminal_fixture(Some(&path)).await;
    let call = "0".repeat(64);
    let work = a
        .service
        .prepare_external_helper_presentation(
            &terminal(&a_enrolled, &a_nonce, &call),
            &a_connection,
        )
        .unwrap();
    assert_eq!(
        settle(&mut a, work, &a_connection).await["result"]["applied"],
        true
    );
    let work = b
        .service
        .prepare_external_helper_presentation(
            &terminal(&b_enrolled, &b_nonce, &call),
            &b_connection,
        )
        .unwrap();
    let reply = settle(&mut b, work, &b_connection).await;
    assert_eq!(reply["result"]["durable"], true, "{reply}");
    assert_eq!(reply["result"]["applied"], false);
    assert!(b.service.external_token_usage(None).is_empty());
    assert_eq!(
        a.service
            .external_token_usage(None)
            .values()
            .map(|u| u.input_tokens)
            .sum::<u64>(),
        10
    );
    assert_eq!(
        b.service.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert!(b.service.control.external_usage().reservations.is_empty());
    assert_eq!(terminal_counts(&path), (1, 1, 0));
}

/// A queued terminal observation cannot outlive its actual publisher. Proved
/// process death rejects before ledger dispatch even while the original CLI and
/// pane root survive; a fresh qualified child can publish only a new nonce.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_terminal_dead_writer_cannot_dispatch_queued_expense() {
    let (mut fixture, mut socket, connection, enrolled, nonce, path) = terminal_fixture(None).await;
    let request = terminal(&enrolled, &nonce, &"1".repeat(64));
    let work = fixture
        .service
        .prepare_external_helper_presentation(&request, &connection)
        .unwrap();
    super::helper_presentation::release(&mut socket, &connection).await;
    assert!(
        settle(&mut fixture, work, &connection)
            .await
            .get("error")
            .is_some()
    );
    assert_eq!(terminal_counts(&path), (0, 0, 0));
    let (_fresh_socket, fresh) = child(&mut fixture).await;
    let replacement = "2".repeat(64);
    let work = fixture
        .service
        .prepare_external_helper_presentation(&pulse(&enrolled, &replacement, 1), &fresh)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &fresh).await["result"]["changed"],
        true
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&request, &fresh)
            .is_err()
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &terminal(&enrolled, &replacement, &"1".repeat(64)),
            &fresh,
        )
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &fresh).await["result"]["applied"],
        true
    );
    assert_eq!(terminal_counts(&path), (1, 1, 0));
}

/// Retirement or epoch replacement before the queued native worker starts must
/// revoke its concrete instance witness even though CLI, publisher and pane root
/// all remain alive. Native process liveness alone cannot authorize late expense.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_terminal_revoked_source_cannot_dispatch_queued_expense() {
    for rotate in [false, true] {
        let (mut fixture, _socket, connection, enrolled, nonce, path) =
            terminal_fixture(None).await;
        let request = terminal(&enrolled, &nonce, &"3".repeat(64));
        let pending = fixture
            .service
            .prepare_external_helper_presentation(&request, &connection)
            .unwrap();
        if rotate {
            let mut rotation = declaration("session-a");
            let mut params: serde_json::Value =
                serde_json::from_str(rotation.params.as_deref().unwrap()).unwrap();
            params["observer_instance"] = "successor".into();
            params["predecessor_generation"] = enrolled["result"]["generation"].clone();
            rotation.params = Some(params.to_string());
            let work = fixture
                .service
                .prepare_external_enrollment(&rotation, &connection)
                .unwrap();
            assert_eq!(
                settle(&mut fixture, work, &connection).await["result"]["registered"],
                true
            );
        } else {
            let digest = *fixture
                .service
                .control
                .external_agents()
                .bindings
                .keys()
                .next()
                .unwrap();
            fixture.service.retire_external_agent_binding(digest);
        }
        assert!(connection.unix_origin().unwrap().is_live());
        assert!(fixture.connection.unix_origin().unwrap().is_live());
        let reply = settle(&mut fixture, pending, &connection).await;
        assert!(reply.get("error").is_some());
        assert_eq!(
            terminal_counts(&path),
            (0, 0, 0),
            "revoked source dispatched expense; rotate={rotate}"
        );
        assert_eq!(
            fixture
                .service
                .external_usage_reservation_counts_for_tests(),
            (0, 0)
        );
    }
}

/// Replacing all control authority while the original CLI and extension remain
/// alive must revoke queued instance proof, not treat surviving native processes
/// or public selectors as a recovered registration. Reopening the same durable
/// store preserves expense already committed before replacement. A worker that
/// published before replacement retains its expense but receives no stale ACK;
/// a worker dispatched afterward must not write at all. This qualifies restart
/// safety only, without asserting transparent source recovery or new boundaries.
#[tokio::test(flavor = "current_thread")]
async fn external_copilot_control_replacement_revokes_work_and_preserves_expense() {
    for published in [false, true] {
        let (mut fixture, _socket, connection, enrolled, nonce, path) =
            terminal_fixture(None).await;
        let committed = terminal(&enrolled, &nonce, &"4".repeat(64));
        let work = fixture
            .service
            .prepare_external_helper_presentation(&committed, &connection)
            .unwrap();
        assert_eq!(
            settle(&mut fixture, work, &connection).await["result"]["durable"],
            true
        );
        let pending = fixture
            .service
            .prepare_external_helper_presentation(
                &terminal(&enrolled, &nonce, &"5".repeat(64)),
                &connection,
            )
            .unwrap();
        let observed = if published {
            let native = pending.clone();
            let observed = tokio::task::spawn_blocking(move || native.observe())
                .await
                .unwrap();
            assert!(observed.is_ok());
            Some(observed)
        } else {
            None
        };
        fixture.service.retire_all_external_source_anchors();
        fixture.service.control = crate::runtime::control::RuntimeControlComponent::new(
            crate::control::ControlIdempotencyCache::default(),
            mez_agent::messaging::MessageService::default(),
            None,
        );
        fixture
            .service
            .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
        assert!(connection.unix_origin().unwrap().is_live());
        assert!(fixture.connection.unix_origin().unwrap().is_live());
        let reply = if let Some(observed) = observed {
            serde_json::from_str(&fixture.service.complete_external_enrollment(
                pending,
                observed,
                &connection,
            ))
            .unwrap()
        } else {
            settle(&mut fixture, pending, &connection).await
        };
        assert!(reply.get("error").is_some());
        for request in [
            committed,
            attachment("session-a"),
            pulse(&enrolled, &nonce, 2),
            callback("session-a", "agentStop", 100),
        ] {
            assert!(
                fixture
                    .service
                    .prepare_external_helper_presentation(&request, &connection)
                    .is_err(),
                "old selectors revived replaced control authority"
            );
        }
        let expenses = if published { 2 } else { 1 };
        assert_eq!(terminal_counts(&path), (expenses, expenses, 0));
        assert!(fixture.service.external_agent_rows().is_empty());
        assert_eq!(
            fixture
                .service
                .external_usage_reservation_counts_for_tests(),
            (0, 0)
        );
    }
}
