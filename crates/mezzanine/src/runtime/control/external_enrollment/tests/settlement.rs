//! Positive native qualification diagnostics shared by independent test owners.
//!
//! Genuine off-actor observation and normal actor settlement remain the only
//! authority. Success callers must reject refusal before extracting fields;
//! diagnostics must identify bounded phases without private reply data. Negative
//! fixtures retain their ordinary settlement path and inspect refusals directly.

use super::*;

/// Settles genuine work for a caller requiring one true result field. Refusal
/// reports only immutable phase/category facts and elapsed worker-admission
/// time; no raw reply, error, handle, PID, path or private metadata is printed.
/// These observations confer no authority and add no native probe or retry.
pub(super) async fn settle_success(
    fixture: &mut Fixture,
    work: ExternalEnrollmentWork,
    connection: &ControlConnectionState,
    field: &'static str,
    stage: &'static str,
) -> serde_json::Value {
    let started = Instant::now();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let observation_ms = started.elapsed().as_millis();
    let native_kind = observed.as_ref().err().map(|error| error.kind());
    let ancestry_present = work.ancestry.get().is_some();
    let admission_expired = Instant::now() >= work.deadline;
    let reservation_current = Arc::ptr_eq(
        &work.registry_incarnation,
        &fixture
            .service
            .control
            .external_agents()
            .enrollments
            .incarnation,
    );
    let reply: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, connection),
    )
    .unwrap();
    let rpc_code = reply
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(serde_json::Value::as_i64);
    assert!(
        native_kind.is_none() && reply.get("error").is_none() && reply["result"][field] == true,
        "external success qualification failed: stage={stage} native={native_kind:?} rpc_code={rpc_code:?} observation_ms={observation_ms} admission_expired={admission_expired} ancestry_present={ancestry_present} reservation_current={reservation_current}",
    );
    reply
}

/// An actual instance-local native capture refusal must fail a positive
/// qualification at its original phase, with no private capability/error/reply
/// text. The same diagnostic contract is used by fresh helper observations and
/// root-replacement enrollments. No native predicate or deadline is relaxed.
#[tokio::test(flavor = "current_thread")]
async fn external_success_settlement_preserves_native_refusal_phase() {
    use futures_util::FutureExt;

    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    fixture.service.sync_external_ancestry_budget();
    let budget = fixture
        .service
        .control
        .external_agents()
        .enrollments
        .ancestry_budget
        .clone();
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    budget.force_capture_expiry_at_check_for_tests(1);
    let connection = fixture.connection.clone();
    let outcome = std::panic::AssertUnwindSafe(settle_success(
        &mut fixture,
        work,
        &connection,
        "registered",
        "native-refusal-control",
    ))
    .catch_unwind()
    .await;
    assert_eq!(budget.capture_expiry_observation_for_tests(), (1, 0));
    let Err(failure) = outcome else {
        panic!("failed native qualification reached its success caller");
    };
    let message = failure
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| failure.downcast_ref::<String>().map(String::as_str))
        .unwrap();
    assert!(
        message.contains("stage=native-refusal-control"),
        "{message}"
    );
    assert!(message.contains("native=Some(Forbidden)"), "{message}");
    assert!(message.contains("ancestry_present=false"), "{message}");
    assert!(!message.contains("launch_token"));
    assert!(!message.contains("observer_witness"));
    assert!(!message.contains(fixture.directory.to_str().unwrap()));
    assert_eq!(budget.reserved(), 0);
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
