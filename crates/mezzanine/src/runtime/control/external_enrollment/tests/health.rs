//! Live discovery health uses native producer and concrete observer lifetimes,
//! independently of callback state and delayed daemon lease maintenance.

use super::*;

/// Settles real native admission for these lifecycle tests without replacing
/// kernel origin, sender or ancestry evidence with injected authority.
async fn enroll_health_fixture(fixture: &mut Fixture) -> serde_json::Value {
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert!(response.get("error").is_none());
    response
}

/// Discovery must retain the same overdue connected run as reconciliation,
/// without advancing its deadline during reads. Losing the observer retains
/// only the remaining lease, reports connection loss separately from the last
/// vendor state, and never advertises unavailable usage as fabricated zero.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_discovery_tracks_observer_health_without_renewal() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let origin = fixture.connection.unix_origin().unwrap().clone();
    let response = enroll_health_fixture(&mut fixture).await;
    let qualified =
        crate::runtime::UnixOriginStream::new(&mut fixture.socket, Some(origin.clone()));
    let observation = crate::control::parse_json_rpc_request(
        &serde_json::json!({"jsonrpc":"2.0","id":"state","method":"agent/external/presentation",
            "params":{"launch_token":response["result"]["launch_token"],"generation":response["result"]["generation"],
                "external_session_id":"session-a","sequence":1,"state":"running"}}).to_string(),
    ).unwrap();
    fixture
        .service
        .dispatch_external_agent_request(&observation, &fixture.connection)
        .unwrap();
    let overdue = current_unix_seconds() - 1;
    fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap()
        .expires = overdue;

    let rows = fixture.service.external_agent_rows();
    assert_eq!(
        rows.len(),
        1,
        "discovery must not hide a qualified idle observer before maintenance"
    );
    assert_eq!(rows[0]["telemetry_health"], "enrolled");
    assert_eq!(rows[0]["usage_coverage"], "unavailable-source-continuity");
    assert_eq!(rows[0]["agent_id"], response["result"]["agent_id"]);
    let status = fixture
        .service
        .runtime_agent_status_display_with_options("%1", false)
        .unwrap();
    assert!(status.contains("### External Telemetry"), "{status}");
    assert!(status.contains("enrolled"), "{status}");
    assert!(status.contains("unavailable-source-continuity"), "{status}");
    assert!(fixture.service.agent_shell_store().get("%1").is_none());
    let primary = fixture
        .service
        .attach_primary(
            "health-status",
            true,
            mez_mux::layout::Size::new(100, 40).unwrap(),
            120,
        )
        .unwrap();
    fixture
        .service
        .split_pane_with_process(
            &primary,
            mez_mux::layout::SplitDirection::Vertical,
            Some("cat >/dev/null"),
        )
        .unwrap();
    let other = fixture
        .service
        .runtime_agent_status_display_with_options("%2", false)
        .unwrap();
    assert!(
        other.contains("No current external telemetry is observed"),
        "{other}"
    );
    assert!(
        !other.contains("| enrolled |"),
        "another pane's live observer must not leak"
    );
    // A retained native conversation and external observer share diagnostics,
    // not execution authority or accounting; formatting must remain read-only.
    fixture
        .service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let native = fixture
        .service
        .runtime_agent_status_display_with_options("%1", false)
        .unwrap();
    assert!(native.contains("## Agent Status"), "{native}");
    assert!(native.contains("### External Telemetry"), "{native}");
    assert!(native.contains("enrolled"), "{native}");
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
        overdue
    );
    assert!(
        !serde_json::to_string(&rows)
            .unwrap()
            .contains(response["result"]["launch_token"].as_str().unwrap())
    );

    drop(qualified);
    assert!(
        origin.is_live(),
        "observer loss must not claim producer death"
    );
    let remaining = current_unix_seconds() + 60;
    fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap()
        .expires = remaining;
    let agent_id = response["result"]["agent_id"].as_str().unwrap();
    let row = fixture.service.external_agent_metadata(agent_id).unwrap();
    assert_eq!(row["telemetry_health"], "connection-lost");
    let status = fixture
        .service
        .runtime_agent_status_display_with_options("%1", false)
        .unwrap();
    assert!(status.contains("connection-lost"), "{status}");
    assert!(status.contains("running"), "{status}");
    assert!(
        status.contains("not installation, completion or invoice proof"),
        "{status}"
    );
    assert!(!status.contains(response["result"]["launch_token"].as_str().unwrap()));
    assert_eq!(
        row["status"], "running",
        "observer health must not overwrite the last vendor-reported state"
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
        remaining
    );

    fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .values_mut()
        .next()
        .unwrap()
        .expires = overdue;
    assert!(fixture.service.external_agent_rows().is_empty());
    let status = fixture
        .service
        .runtime_agent_status_display_with_options("%1", false)
        .unwrap();
    assert!(
        status.contains("No current external telemetry is observed"),
        "{status}"
    );
    assert!(
        !status.contains("| session-a |"),
        "expired registration must not remain a live row"
    );
    assert!(
        !fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .retired
    );
    assert_eq!(fixture.service.reconcile_external_agent_registrations(), 1);
}

/// A dead ordinary producer must disappear immediately from read-only
/// discovery even before the maintenance owner retires its identity. A live
/// shell and a retained observer adapter cannot revive the exact native origin,
/// and discovery itself must not mutate leases or retirement state.
#[tokio::test(flavor = "current_thread")]
async fn external_enrollment_discovery_suppresses_dead_producer_before_maintenance() {
    use tokio::io::AsyncWriteExt;
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let origin = fixture.connection.unix_origin().unwrap().clone();
    enroll_health_fixture(&mut fixture).await;
    let mut qualified =
        crate::runtime::UnixOriginStream::new(&mut fixture.socket, Some(origin.clone()));
    let expires = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap()
        .expires;
    qualified.write_all(&[2]).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while origin.is_live() {
        assert!(Instant::now() < deadline, "ordinary producer did not exit");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        fixture.service.pane_process_identity("%1").is_ok(),
        "pane shell must still be alive"
    );

    assert!(
        fixture.service.external_agent_rows().is_empty(),
        "a dead producer cannot remain discoverable under a live shell lease"
    );
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(binding.expires, expires);
    assert!(
        !binding.retired,
        "discovery is read-only; maintenance owns retirement"
    );
    assert_eq!(fixture.service.reconcile_external_agent_registrations(), 1);
    drop(qualified);
}
