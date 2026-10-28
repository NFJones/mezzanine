//! Native-qualified Claude coverage diagnostics cannot alter expense or liveness.
//!
//! Real helper/creator ancestry owns every accepted observation. Fixed reasons
//! remain current-epoch metadata; rejected, unpublished or stale work has no
//! presentation, observer-proof, lease, accounting or client-role effects.

use super::curated_admission::enroll;
use super::curated_usage::{begin, end};
use super::helper_presentation::child;
use super::*;

/// Builds only the exact original public selector and one closed diagnostic.
fn gap(first: &serde_json::Value, reason: &str) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(
        &serde_json::json!({"jsonrpc":"2.0","id":"gap",
        "method":"agent/external/curated-usage-gap","params":{
            "harness":"claude","generation":first["result"]["generation"],
            "observer_witness":first["result"]["observer_witness"],
            "external_session_id":"session-a","reason":reason}})
        .to_string(),
    )
    .unwrap()
}

/// Executes actual native worker proof and the common actor settlement owner.
async fn commit(
    fixture: &mut Fixture,
    request: &JsonRpcRequest,
    connection: &ControlConnectionState,
) -> serde_json::Value {
    let work = fixture
        .service
        .prepare_external_helper_presentation(request, connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, connection),
    )
    .unwrap()
}

/// An actual durable request precedes the diagnostic. Repeated fixed gap facts
/// must override reported coverage while retaining exact committed expense and
/// leaving heartbeat proof, lease, presentation, source identity and roles intact.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_usage_gap_is_sticky_without_expense_or_freshness_effects() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let (_socket, connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &connection).await;
    let ready = commit(&mut fixture, &begin(&first), &connection).await;
    let report = end(&first, ready["result"]["admitted_at"].as_u64().unwrap());
    assert_eq!(
        commit(&mut fixture, &report, &connection).await["result"]["durable"],
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
    let before = (
        binding.expires,
        binding.accounting_owner.clone(),
        binding.generation,
    );
    let clients = fixture.service.session().clients().len();
    for _ in 0..2 {
        let reply = commit(
            &mut fixture,
            &gap(&first, "finalized-counters"),
            &connection,
        )
        .await;
        assert_eq!(
            reply["result"],
            serde_json::json!({"observed":true,"reason":"finalized-counters"})
        );
    }
    // Replaying valid expense cannot erase an independently accepted loss fact.
    // End consumption requires revalidating the original begin before replay.
    let replay_ready = commit(&mut fixture, &begin(&first), &connection).await;
    assert_eq!(replay_ready["result"], ready["result"]);
    assert_eq!(
        commit(&mut fixture, &report, &connection).await["result"]["applied"],
        false
    );
    let rows = fixture.service.external_agent_rows();
    assert_eq!(
        rows[0]["usage_coverage"],
        "unavailable-claude-usage-finalized-counters"
    );
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
            binding.expires,
            binding.accounting_owner.clone(),
            binding.generation
        ),
        before
    );
    assert!(
        binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .is_none()
    );
    let proof = binding
        .enrollment
        .as_ref()
        .unwrap()
        .curated_observer
        .as_ref()
        .unwrap();
    assert_eq!(proof.sequence, 0);
    assert!(proof.observed_at.is_none());
    assert_eq!(fixture.service.session().clients().len(), clients);
    assert!(!connection.initialized());
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
}

/// Unknown/content-bearing reasons, another source handle and unobserved/expired
/// native work must not publish a gap. Successful worker proof is still subject
/// to actor deadline checks, so these failures cannot masquerade as custody.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_usage_gap_rejects_invalid_stale_and_unpublished_work() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let (_socket, connection) = child(&mut fixture).await;
    let first = enroll(&mut fixture, &connection).await;
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&gap(&first, "PRIVATE"), &connection)
            .is_err()
    );
    let mut stale = first.clone();
    stale["result"]["generation"] = serde_json::json!(999);
    assert!(
        fixture
            .service
            .prepare_external_helper_presentation(&gap(&stale, "capacity"), &connection)
            .is_err()
    );
    let request = gap(&first, "capacity");
    let work = fixture
        .service
        .prepare_external_helper_presentation(&request, &connection)
        .unwrap();
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, Ok(()), &connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some());
    let mut work = fixture
        .service
        .prepare_external_helper_presentation(&request, &connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    work.deadline = Instant::now();
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &connection),
    )
    .unwrap();
    assert!(reply.get("error").is_some());
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(
        binding
            .enrollment
            .as_ref()
            .unwrap()
            .curated_usage_gap_coverage(),
        None
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
