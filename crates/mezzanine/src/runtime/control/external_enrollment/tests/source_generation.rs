//! Service-owned external generations isolate retained presentation sources.
//!
//! Native source and accounting authority remain independently qualified. These
//! tests ensure disposable registry loss cannot reuse presentation-owner keys,
//! revive old status, clear another source, or allocate overlapping generations
//! when multiple sources recover in a different order from their original birth.

use super::*;

/// Bounded inert session metadata and an optional exact public resume selector.
fn metadata(session: &str, instance: &str, resume: Option<&serde_json::Value>) -> JsonRpcRequest {
    let mut request = request();
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["external_session_id"] = session.into();
    params["observer_instance"] = instance.into();
    if let Some(receipt) = resume {
        params["source_resume"] = serde_json::json!({"observer_witness":receipt["result"]["observer_witness"],"generation":receipt["result"]["generation"]});
    }
    request.params = Some(params.to_string());
    request
}

/// Qualifies one actual persistent native socket through the production worker.
async fn enroll(fixture: &mut Fixture, request: &JsonRpcRequest) -> serde_json::Value {
    let work = fixture
        .service
        .prepare_external_enrollment(request, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert_eq!(result["result"]["registered"], true, "{result}");
    result
}

/// Presents one source-bound generic observation, never injecting terminal input.
fn present(fixture: &mut Fixture, receipt: &serde_json::Value, title: &str) {
    let request = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"present","method":"agent/external/presentation","params":{
        "launch_token":receipt["result"]["launch_token"],"generation":receipt["result"]["generation"],
        "external_session_id":receipt["result"]["external_session_id"],"sequence":1,"state":"running","title":title,
    }}).to_string()).unwrap();
    fixture
        .service
        .dispatch_external_agent_request(&request, &fixture.connection)
        .unwrap();
}

/// The service allocator covers initial, primary-issued and recovered sources.
/// Old A status must stay hidden after loss even when B newly enrolls in the same
/// pane. B's presentation survives A recovery, and reverse-order recovery cannot
/// reuse any retained generation or let one owner clear another's status/title.
#[tokio::test(flavor = "current_thread")]
async fn external_source_generations_preserve_presentation_isolation_across_registry_loss() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
            fixture.directory.join("usage.sqlite"),
        ));
    let first = enroll(&mut fixture, &metadata("session-a", "instance-a", None)).await;
    present(&mut fixture, &first, "old A");
    assert!(fixture.service.live_pane_harness_status("%1").is_some());
    *fixture.service.control.external_agents_mut() = Default::default();
    assert!(fixture.service.live_pane_harness_status("%1").is_none());
    let b = enroll(&mut fixture, &metadata("session-b", "instance-b", None)).await;
    assert_ne!(b["result"]["generation"], first["result"]["generation"]);
    assert!(
        fixture.service.live_pane_harness_status("%1").is_none(),
        "new B revived stale A status"
    );
    assert!(fixture.service.external_agent_pane_title("%1").is_none());
    present(&mut fixture, &b, "current B");
    let a = enroll(
        &mut fixture,
        &metadata("session-a", "instance-a", Some(&first)),
    )
    .await;
    assert_ne!(a["result"]["generation"], b["result"]["generation"]);
    assert_eq!(
        fixture
            .service
            .live_pane_harness_status("%1")
            .unwrap()
            .text
            .as_deref(),
        Some("running")
    );
    assert_eq!(
        fixture.service.external_agent_pane_title("%1").as_deref(),
        Some("current B")
    );
    let primary = fixture
        .service
        .attach_primary(
            "generation fixture",
            true,
            mez_mux::layout::Size::new(80, 24).unwrap(),
            120,
        )
        .unwrap();
    let legacy: serde_json::Value = serde_json::from_str(&fixture.service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"launch","method":"agent/external/launch","params":{"pane_id":"%1","harness":"opencode","version":"fixture"}}"#, &primary)).unwrap();
    let generations = [
        first["result"]["generation"].as_u64().unwrap(),
        b["result"]["generation"].as_u64().unwrap(),
        a["result"]["generation"].as_u64().unwrap(),
        legacy["result"]["generation"].as_u64().unwrap(),
    ];
    assert!(generations.windows(2).all(|pair| pair[0] < pair[1]));
    *fixture.service.control.external_agents_mut() = Default::default();
    let recovered_b = enroll(&mut fixture, &metadata("session-b", "instance-b", Some(&b))).await;
    present(&mut fixture, &recovered_b, "recovered B");
    let recovered_a = enroll(&mut fixture, &metadata("session-a", "instance-a", Some(&a))).await;
    assert!(recovered_b["result"]["generation"].as_u64().unwrap() > generations[3]);
    assert!(
        recovered_a["result"]["generation"].as_u64().unwrap()
            > recovered_b["result"]["generation"].as_u64().unwrap()
    );
    assert_eq!(
        fixture.service.external_agent_pane_title("%1").as_deref(),
        Some("recovered B")
    );
    assert!(fixture.service.live_pane_harness_status("%1").is_some());
}
