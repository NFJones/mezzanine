//! Original Codex source dormancy and genuinely fresh native callback recovery.
//!
//! Expiring a fixture clock does not supply new authority. Every successful
//! recovery or incurred outcome still runs the real helper/creator/root worker
//! and actor fences. Silence and replay remain invisible and time-inert.

use super::codex_observation::{callback, declaration};
use super::helper_presentation::{child, settle};
use super::*;
use mez_agent::messaging::AgentPresenceStatus;

/// Ages only the existing observer/lease clocks, preserving native lifetimes,
/// original identity and request custody. No production clock override exists.
fn age(fixture: &mut Fixture) -> Option<Instant> {
    let binding = fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap();
    binding.expires = 0;
    let proof = binding
        .enrollment
        .as_mut()
        .unwrap()
        .curated_observer
        .as_mut()
        .unwrap();
    proof.observed_at = Instant::now().checked_sub(Duration::from_secs(31));
    proof.observed_at
}

/// Reads one actual registered observational presence rather than fabricating
/// status from a callback payload or assuming that hidden rows imply retirement.
fn presence(fixture: &Fixture) -> AgentPresenceStatus {
    let agent = &fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .registration
        .as_ref()
        .unwrap()
        .agent_id;
    fixture
        .service
        .control
        .message_service()
        .presence()
        .into_iter()
        .find(|row| row.identity.agent_id == *agent)
        .unwrap()
        .status
}

/// Idle maintenance hides stale metadata and sets observational presence offline
/// without renewing clocks or abandoning the original namespace. Source-start
/// retry, duplicate Stop, a wrong native writer and an unpublished worker cannot
/// wake it. A changed genuine prompt restores the same identity/run/owner and a
/// finite lease; explicit SessionEnd remains absorbing despite a live creator.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_idle_fresh_fact_recovers_without_silence_or_replay_health() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Codex idle qualification requires native peer evidence")
    };
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &connection)
        .unwrap();
    let first = settle(&mut fixture, work, &connection).await;
    assert_eq!(first["result"]["registered"], true);
    for event in ["UserPromptSubmit", "Stop"] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&callback(event, "turn-a"), &connection)
            .unwrap();
        assert_eq!(
            settle(&mut fixture, work, &connection).await["result"]["changed"],
            true
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
    let identity = (
        binding.registration.as_ref().unwrap().agent_id.clone(),
        binding.generation,
        binding.accounting_owner.clone(),
        binding.enrollment.as_ref().unwrap().run_generation,
    );
    let clients = fixture.service.session().clients().len();
    let observed = age(&mut fixture);
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    fixture
        .service
        .reconcile_external_agent_registration(digest);
    assert!(fixture.service.external_agent_rows().is_empty());
    for attempt in 0..2 {
        fixture.service.renew_connected_external_observers();
        assert_eq!(
            fixture.service.reconcile_external_agent_registrations(),
            usize::from(attempt == 0)
        );
        let binding = fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap();
        assert!(!binding.retired);
        assert_eq!(binding.expires, 0);
        let source = binding.enrollment.as_ref().unwrap();
        assert!(source.codex_telemetry_dormant());
        assert_eq!(
            source.curated_observer.as_ref().unwrap().observed_at,
            observed
        );
        assert!(fixture.service.external_agent_rows().is_empty());
        assert!(fixture.service.live_pane_harness_status("%1").is_none());
        assert_eq!(presence(&fixture), AgentPresenceStatus::Offline);
    }
    // Repeating the original declaration does not make it a heartbeat.
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["registered"],
        true
    );
    let duplicate = fixture
        .service
        .prepare_external_helper_presentation(&callback("Stop", "turn-a"), &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, duplicate, &connection).await["result"]["changed"],
        false
    );
    let wrong = fixture.connection.clone();
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(
                &callback("UserPromptSubmit", "wrong-parent"),
                &wrong,
            )
            .is_err()
    );
    let unpublished = fixture
        .service
        .prepare_external_helper_presentation(
            &callback("UserPromptSubmit", "unpublished"),
            &connection,
        )
        .unwrap();
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(unpublished, Ok(()), &connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some());
    assert!(fixture.service.external_agent_rows().is_empty());
    assert_eq!(presence(&fixture), AgentPresenceStatus::Offline);
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(binding.expires, 0);
    assert_eq!(
        binding
            .enrollment
            .as_ref()
            .unwrap()
            .curated_observer
            .as_ref()
            .unwrap()
            .observed_at,
        observed
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("UserPromptSubmit", "turn-b"), &connection)
        .unwrap();
    let reply = settle(&mut fixture, work, &connection).await;
    assert_eq!(reply["result"]["changed"], true, "{reply}");
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
            binding.registration.as_ref().unwrap().agent_id.clone(),
            binding.generation,
            binding.accounting_owner.clone(),
            binding.enrollment.as_ref().unwrap().run_generation
        ),
        identity
    );
    assert!(
        !binding
            .enrollment
            .as_ref()
            .unwrap()
            .codex_telemetry_dormant()
    );
    assert!(binding.expires > current_unix_seconds());
    assert_eq!(binding.enrollment.as_ref().unwrap().epoch, 1);
    assert_eq!(fixture.service.external_agent_rows().len(), 1);
    assert_eq!(
        fixture
            .service
            .live_pane_harness_status("%1")
            .unwrap()
            .text
            .as_deref(),
        Some("running")
    );
    assert_eq!(presence(&fixture), AgentPresenceStatus::Available);
    assert_eq!(fixture.service.session().clients().len(), clients);
    assert!(!connection.initialized());
    let end = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"end","method":"agent/external/codex-observe","params":{"pane_id":"%1","external_session_id":"session-a","event":"SessionEnd"}}).to_string()).unwrap();
    let work = fixture
        .service
        .prepare_external_helper_presentation(&end, &connection)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &connection).await["result"]["retired"],
        true
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(
                &callback("UserPromptSubmit", "turn-c"),
                &connection
            )
            .is_err()
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .retired
    );
}

/// An original pre can outlive its telemetry lease while a real upstream model
/// request runs. A later genuine post records its original incurred expense once
/// without displaying old status, renewing a lease or claiming observer health.
/// A changed Stop supplies fresh lifecycle evidence, while later pane-root loss
/// still permanently retires the original dormant source and pending callbacks.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_idle_model_outcome_conserves_custody_and_root_loss_retires() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Codex idle accounting requires native peer evidence")
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
    assert_eq!(
        settle(&mut fixture, work, &source).await["result"]["changed"],
        true
    );
    let (_pre_socket, pre) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("PreModelRequest", "turn-a"), &pre)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &pre).await["result"]["observed"],
        true
    );
    let observed = age(&mut fixture);
    assert_eq!(fixture.service.reconcile_external_agent_registrations(), 1);
    let post = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"post","method":"agent/external/codex-observe",
        "params":{"pane_id":"%1","external_session_id":"session-a","event":"PostModelRequest","turn_id":"turn-a","counters":{"input_tokens":10,"output_tokens":3}}}).to_string()).unwrap();
    let (_post_socket, connection) = child(&mut fixture).await;
    for replay in [false, true] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&post, &connection)
            .unwrap();
        let reply = settle(&mut fixture, work, &connection).await;
        assert_eq!(reply["result"]["durable"], true, "{reply}");
        assert_eq!(reply["result"]["applied"], !replay, "{reply}");
        let binding = fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap();
        assert_eq!(binding.expires, 0);
        assert_eq!(
            binding
                .enrollment
                .as_ref()
                .unwrap()
                .curated_observer
                .as_ref()
                .unwrap()
                .observed_at,
            observed
        );
        assert!(fixture.service.external_agent_rows().is_empty());
        assert!(fixture.service.live_pane_harness_status("%1").is_none());
        assert_eq!(presence(&fixture), AgentPresenceStatus::Offline);
    }
    let work = fixture
        .service
        .prepare_external_helper_presentation(&callback("Stop", "turn-a"), &source)
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &source).await["result"]["changed"],
        true
    );
    assert_eq!(fixture.service.external_agent_rows().len(), 1);
    assert_eq!(presence(&fixture), AgentPresenceStatus::Available);
    let database = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT count(*),sum(input_tokens) FROM token_usage_events",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        (1, 10)
    );
    age(&mut fixture);
    assert_eq!(fixture.service.reconcile_external_agent_registrations(), 1);
    let pending = fixture
        .service
        .prepare_external_helper_presentation(
            &callback("UserPromptSubmit", "after-root-loss"),
            &source,
        )
        .unwrap();
    fixture.service.terminate_all_pane_processes().unwrap();
    fixture.service.start_initial_pane_process(None).unwrap();
    assert_eq!(fixture.service.reconcile_external_agent_registrations(), 1);
    assert!(
        settle(&mut fixture, pending, &source)
            .await
            .get("error")
            .is_some()
    );
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&post, &connection)
            .is_err()
    );
    assert!(fixture.service.external_agent_rows().is_empty());
    assert_eq!(
        fixture
            .service
            .external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
}

/// Two genuine native creators can reuse one public conversation label. Keeping
/// the first dormant must neither block the second nor make either callback
/// borrow the other's generation. Kernel-parent candidate narrowing precedes
/// full worker proof; recovery changes only its original source and identity.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_idle_same_session_creators_keep_native_candidate_scope() {
    use super::helper_presentation::child_of;
    let Some(mut fixture) = fixture("hold").await else {
        panic!("Codex creator qualification requires native peer evidence")
    };
    let (_first_socket, first_connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &first_connection)
        .unwrap();
    let first = settle(&mut fixture, work, &first_connection).await;
    let first_generation = first["result"]["generation"].as_u64().unwrap();
    for event in ["UserPromptSubmit", "Stop"] {
        let work = fixture
            .service
            .prepare_external_helper_presentation(&callback(event, "turn-a"), &first_connection)
            .unwrap();
        assert_eq!(
            settle(&mut fixture, work, &first_connection).await["result"]["changed"],
            true
        );
    }
    age(&mut fixture);
    assert_eq!(fixture.service.reconcile_external_agent_registrations(), 1);
    let (mut second_creator, _creator_connection) = child(&mut fixture).await;
    let (_second_socket, second_connection) =
        child_of(&mut second_creator, &fixture.listener).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &second_connection)
        .unwrap();
    let second = settle(&mut fixture, work, &second_connection).await;
    assert_eq!(second["result"]["registered"], true);
    let second_generation = second["result"]["generation"].as_u64().unwrap();
    assert_ne!(first_generation, second_generation);
    assert_ne!(first["result"]["agent_id"], second["result"]["agent_id"]);
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &callback("UserPromptSubmit", "turn-b"),
            &first_connection,
        )
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &first_connection).await["result"]["sequence"],
        3
    );
    let second_binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .find(|binding| binding.generation == second_generation)
        .unwrap();
    assert!(
        second_binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .is_none()
    );
    let work = fixture
        .service
        .prepare_external_helper_presentation(
            &callback("UserPromptSubmit", "other-creator-turn"),
            &second_connection,
        )
        .unwrap();
    assert_eq!(
        settle(&mut fixture, work, &second_connection).await["result"]["sequence"],
        1
    );
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 2);
    let first_binding = registry
        .bindings
        .values()
        .find(|binding| binding.generation == first_generation)
        .unwrap();
    let second_binding = registry
        .bindings
        .values()
        .find(|binding| binding.generation == second_generation)
        .unwrap();
    assert_eq!(
        first_binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .as_ref()
            .unwrap()
            .sequence,
        3
    );
    assert_eq!(
        second_binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .as_ref()
            .unwrap()
            .sequence,
        1
    );
    assert_ne!(
        first_binding.accounting_owner,
        second_binding.accounting_owner
    );
    assert!(registry.enrollments.pending.is_empty());
}
