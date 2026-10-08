//! Intermediate-parent lifetime regressions using genuine ordinary processes.
//!
//! Native chain observations identify only test-owned descendants. Terminating
//! the upper fixture ancestor leaves the producer and its immediate parent alive
//! while the middle process is reparented, distinguishing whole-chain authority
//! from endpoint-only root/origin checks. No payload PID supplies admission proof.

use super::*;
use tokio::io::AsyncWriteExt;

/// Native observation/actor settlement for a test-owned producer, with no
/// fabricated successful evidence and no credentials in assertion messages.
async fn settle(fixture: &mut Fixture, work: ExternalEnrollmentWork) -> serde_json::Value {
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    serde_json::from_str(&fixture.service.complete_external_enrollment(
        work,
        observed,
        &fixture.connection,
    ))
    .unwrap()
}

/// One strict presentation uses only the existing producer handle and session;
/// native connection evidence, not the payload, supplies process authority.
fn presentation(response: &serde_json::Value, method: &str) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"state","method":method,
        "params":{"launch_token":response["result"]["launch_token"],"generation":response["result"]["generation"],
        "external_session_id":"session-a","sequence":1,"state":"running","title":"ancestry fixture"}}).to_string()).unwrap()
}

/// Terminates only the exact upper fixture ancestor from the native chain, then
/// waits for its surviving child to be reparented. Neither pane root nor producer
/// is terminated, and the producer's immediate native record stays unchanged.
async fn orphan_middle(fixture: &Fixture) {
    let origin = fixture.connection.unix_origin().unwrap();
    let middle =
        mez_mux::process::process_parent_identity_for_pid(origin.identity.parent_process_id)
            .unwrap();
    let upper =
        mez_mux::process::process_parent_identity_for_pid(middle.parent_process_id).unwrap();
    let root = fixture.service.pane_process_identity("%1").unwrap();
    assert_ne!(upper.process_id, root.process_id);
    assert_eq!(upper.parent_process_id, root.process_id);
    let pid = libc::pid_t::try_from(upper.process_id).unwrap();
    // SAFETY: native fixture records above identify the deliberately launched
    // test-only upper descendant, not the pane root or an arbitrary caller PID.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let current =
                mez_mux::process::process_parent_identity_for_pid(middle.process_id).unwrap();
            assert_eq!(current.start_token, middle.start_token);
            if current.parent_process_id != upper.process_id {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(origin.reobserve().unwrap(), origin.identity);
    assert!(
        fixture
            .service
            .pane_process_identity_is_current("%1", &root)
    );
}

/// A previously successful ancestry walk must not authorize a registration once
/// an intermediate ancestor exits before actor settlement. The unchanged live
/// producer/immediate-parent/root checks alone would incorrectly accept this
/// orphaned producer; rejection releases admission and allocates no identity.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_intermediate_exit_before_commit_rejects_live_endpoints() {
    let Some(mut fixture) = fixture("tree").await else {
        return;
    };
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    orphan_middle(&fixture).await;
    let result: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert!(
        result.get("error").is_some(),
        "orphaned producer acquired pane authority"
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
    fixture.socket.write_all(&[2]).await.unwrap();
}

/// Once enrolled, ancestor exit must hide title/status/discovery immediately,
/// stop idle renewal and reject credential reuse before the next cleanup timer.
/// Retirement preserves the bounded tombstone but releases ancestry descriptors;
/// producer/root survival cannot keep their severed relationship authoritative.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_intermediate_exit_retires_live_run_and_releases_ancestry() {
    let Some(mut fixture) = fixture("tree").await else {
        return;
    };
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let response = settle(&mut fixture, work).await;
    assert!(response.get("error").is_none());
    let budget = fixture
        .service
        .control
        .external_agents()
        .enrollments
        .ancestry_budget
        .clone();
    assert_eq!(budget.reserved(), 3);
    let update = presentation(&response, "agent/external/presentation");
    fixture
        .service
        .dispatch_external_agent_request(&update, &fixture.connection)
        .unwrap();
    assert!(fixture.service.external_agent_pane_title("%1").is_some());
    assert!(fixture.service.live_pane_harness_status("%1").is_some());
    let expires = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .expires;
    orphan_middle(&fixture).await;
    assert!(fixture.service.external_agent_rows().is_empty());
    assert!(fixture.service.external_agent_pane_title("%1").is_none());
    assert!(fixture.service.live_pane_harness_status("%1").is_none());
    fixture.service.renew_connected_external_observers();
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
    assert!(
        fixture
            .service
            .dispatch_external_agent_request(&update, &fixture.connection)
            .is_err()
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| binding.retired)
    );
    assert_eq!(budget.reserved(), 0);
    fixture.socket.write_all(&[2]).await.unwrap();
}

/// Helper native evidence is also fenced against later intermediate exit. It
/// cannot update an orphaned producer's presentation even though both actual
/// child and direct parent remain alive with unchanged immediate records.
#[tokio::test(flavor = "current_thread")]
async fn external_helper_presentation_intermediate_exit_invalidates_pending_callback() {
    let Some(mut fixture) = fixture("tree").await else {
        return;
    };
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let response = settle(&mut fixture, work).await;
    assert!(response.get("error").is_none());
    let (_socket, connection) = super::helper_presentation::child(&mut fixture).await;
    let request = presentation(&response, "agent/external/helper-presentation");
    let work = fixture
        .service
        .prepare_external_helper_presentation(&request, &connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    orphan_middle(&fixture).await;
    let result: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &connection),
    )
    .unwrap();
    assert!(result.get("error").is_some());
    assert_eq!(fixture.service.reconcile_external_agent_registrations(), 1);
    assert_eq!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .ancestry_budget
            .reserved(),
        0
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
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .all(|binding| binding
                .registration
                .as_ref()
                .unwrap()
                .presentation
                .is_none())
    );
    fixture.socket.write_all(&[2]).await.unwrap();
}

/// Retired producer-only identity proof permits only a receipt that actually
/// retired the session. Nonterminal and reload receipts from a subsequently
/// orphaned producer reject; an exact quit receipt remains safely replayable
/// with no ancestry descriptors, lease, presentation or accounting reactivation.
#[tokio::test(flavor = "current_thread")]
async fn external_pi_ancestry_retirement_replays_only_actual_shutdown_receipt() {
    for (last_event, allowed) in [
        (serde_json::json!({"type":"agent_start"}), false),
        (
            serde_json::json!({"type":"session_shutdown","reason":"reload"}),
            false,
        ),
        (
            serde_json::json!({"type":"session_shutdown","reason":"quit"}),
            true,
        ),
    ] {
        let Some(mut fixture) = fixture("tree").await else {
            return;
        };
        let work = fixture
            .service
            .prepare_external_enrollment(&request(), &fixture.connection)
            .unwrap();
        let response = settle(&mut fixture, work).await;
        assert!(response.get("error").is_none());
        let observation = |sequence, event| {
            crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"pi", "method":"agent/external/pi-observation",
            "params":{"launch_token":response["result"]["launch_token"],"generation":response["result"]["generation"],"external_session_id":"session-a","sequence":sequence,"event":event}}).to_string()).unwrap()
        };
        let start = observation(
            1,
            serde_json::json!({"type":"session_start","reason":"startup"}),
        );
        fixture
            .service
            .dispatch_external_agent_request(&start, &fixture.connection)
            .unwrap();
        let last = observation(2, last_event);
        let receipt = fixture
            .service
            .dispatch_external_agent_request(&last, &fixture.connection)
            .unwrap();
        orphan_middle(&fixture).await;
        fixture.service.reconcile_external_agent_registrations();
        let replay = fixture
            .service
            .dispatch_external_agent_request(&last, &fixture.connection);
        if allowed {
            assert_eq!(replay.unwrap(), receipt);
        } else {
            assert!(
                replay.is_err(),
                "retired Pi accepted a receipt without terminal retirement evidence"
            );
        }
        assert_eq!(
            fixture
                .service
                .control
                .external_agents()
                .enrollments
                .ancestry_budget
                .reserved(),
            0
        );
        assert!(fixture.service.external_agent_rows().is_empty());
        assert!(fixture.service.live_pane_harness_status("%1").is_none());
        fixture.socket.write_all(&[2]).await.unwrap();
    }
}

/// A retained native work clone is a real descriptor owner even after successful
/// settlement and retirement. Budget capacity must not be released early merely
/// because the registration drops its shared witness; the last clone releases it
/// exactly once, without retaining the old run's presentation or control rights.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_ancestry_work_clone_holds_capacity_until_last_drop() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
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
    let retained = work.clone();
    let result = settle(&mut fixture, work).await;
    assert!(result.get("error").is_none());
    assert_eq!(budget.reserved(), 1);
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    fixture.service.retire_external_agent_binding(digest);
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    assert!(fixture.service.external_agent_rows().is_empty());
    assert_eq!(budget.reserved(), 1);
    drop(retained);
    assert_eq!(budget.reserved(), 0);
}

/// Pending workers and live runs share one aggregate descriptor budget. Full
/// capacity rejects retry without evicting the current run; failed work releases
/// admission. Capacity release permits a same-run retry without leaking another
/// retained chain, and retirement returns all original witness reservations.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_ancestry_capacity_is_aggregate_and_retry_preserves_run() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let response = settle(&mut fixture, work).await;
    assert!(response.get("error").is_none());
    let budget = fixture
        .service
        .control
        .external_agents()
        .enrollments
        .ancestry_budget
        .clone();
    let original = budget.reserved();
    assert_eq!(original, 1);
    let full = budget.reserve_for_tests(512 - original).unwrap();
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert_eq!(
        observed.as_ref().unwrap_err().kind(),
        crate::error::MezErrorKind::RateLimited
    );
    fixture
        .service
        .complete_external_enrollment(work, observed, &fixture.connection);
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    assert_eq!(fixture.service.external_agent_rows().len(), 1);
    assert_eq!(budget.reserved(), 512);
    drop(full);
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let retry = settle(&mut fixture, work).await;
    assert!(retry.get("error").is_none());
    assert!(
        response["result"]["launch_token"] == retry["result"]["launch_token"],
        "retry changed private handle"
    );
    assert_eq!(budget.reserved(), original);
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    fixture.service.retire_external_agent_binding(digest);
    assert_eq!(budget.reserved(), 0);
}
