//! Ordinary Pi typed ingress regression tests over genuine native fixture origins.

use super::*;

/// Enrolls one real descendant observer, with no synthetic ancestry authority.
async fn enroll(fixture: &mut Fixture) -> serde_json::Value {
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
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

/// Supplies only the normalized known event envelope under a private handle.
fn observation(
    enrollment: &serde_json::Value,
    sequence: u64,
    event: serde_json::Value,
) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"pi",
        "method":"agent/external/pi-observation","params":{
        "launch_token":enrollment["result"]["launch_token"],"generation":enrollment["result"]["generation"],
        "external_session_id":"session-a","sequence":sequence,"event":event}}).to_string()).unwrap()
}

/// Existing LifecycleOwner—not callback timing or agent_end—owns provisional
/// outcome/final settlement, UI restoration and retirement. Latest exact replay
/// is inert; gaps, conflicting observations and generic presentation are rejected.
#[tokio::test(flavor = "current_thread")]
async fn external_pi_observation_reuses_reducer_and_exact_receipts() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let enrollment = enroll(&mut fixture).await;
    for (sequence, event, state) in [
        (
            1,
            serde_json::json!({"type":"session_start","reason":"startup"}),
            "ready",
        ),
        (2, serde_json::json!({"type":"agent_start"}), "running"),
        (
            3,
            serde_json::json!({"type":"ui_prompt_start","reason":"ui_prompt","kind":"select"}),
            "input-wait",
        ),
        (
            4,
            serde_json::json!({"type":"ui_prompt_end","reason":"ui_prompt","kind":"select"}),
            "running",
        ),
        (
            5,
            serde_json::json!({"type":"agent_before_settle","outcome":"completed"}),
            "running",
        ),
        (6, serde_json::json!({"type":"agent_settled"}), "complete"),
    ] {
        let request = observation(&enrollment, sequence, event);
        let response = fixture
            .service
            .dispatch_external_agent_request(&request, &fixture.connection)
            .unwrap();
        let reply: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(reply["accepted"], true);
        assert_eq!(reply["sequence"], sequence);
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
                .registration
                .as_ref()
                .unwrap()
                .presentation
                .as_ref()
                .unwrap()
                .state,
            state
        );
        assert_eq!(
            fixture
                .service
                .dispatch_external_agent_request(&request, &fixture.connection)
                .unwrap(),
            response
        );
    }
    let gap = observation(&enrollment, 8, serde_json::json!({"type":"agent_start"}));
    assert!(
        fixture
            .service
            .dispatch_external_agent_request(&gap, &fixture.connection)
            .is_err()
    );
    let conflict = observation(&enrollment, 6, serde_json::json!({"type":"agent_start"}));
    assert!(
        fixture
            .service
            .dispatch_external_agent_request(&conflict, &fixture.connection)
            .is_err()
    );
    let mut generic = observation(&enrollment, 7, serde_json::json!({"type":"agent_start"}));
    generic.method = "agent/external/presentation".into();
    let mut params: serde_json::Value =
        serde_json::from_str(generic.params.as_deref().unwrap()).unwrap();
    params.as_object_mut().unwrap().remove("event");
    params["state"] = serde_json::json!("running");
    generic.params = Some(params.to_string());
    assert!(
        fixture
            .service
            .dispatch_external_agent_request(&generic, &fixture.connection)
            .is_err()
    );
    let end = observation(
        &enrollment,
        7,
        serde_json::json!({"type":"session_shutdown","reason":"quit"}),
    );
    let response = fixture
        .service
        .dispatch_external_agent_request(&end, &fixture.connection)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .dispatch_external_agent_request(&end, &fixture.connection)
            .unwrap(),
        response
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
}

/// Unknown/duplicate nested keys and vendor content cannot cross typed Pi
/// ingress. Rejection leaves reducer/sequence untouched, so a valid next fact
/// can still be admitted without fabricating skipped chronology.
#[tokio::test(flavor = "current_thread")]
async fn external_pi_observation_rejects_content_aliases_and_mixed_sources() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let enrollment = enroll(&mut fixture).await;
    for event in [
        serde_json::json!({"type":"agent_start","prompt":"private"}),
        serde_json::json!({"type":"agent_end"}),
        serde_json::json!({"type":"session_start","reason":"unknown"}),
    ] {
        let bad = observation(&enrollment, 1, event);
        assert!(
            fixture
                .service
                .dispatch_external_agent_request(&bad, &fixture.connection)
                .is_err()
        );
    }
    let mut duplicate = observation(&enrollment, 1, serde_json::json!({"type":"agent_start"}));
    duplicate.params = Some(format!(
        r#"{{"launch_token":{},"generation":{},"external_session_id":"session-a","sequence":1,"event":{{"type":"agent_start","type":"agent_start"}}}}"#,
        enrollment["result"]["launch_token"], enrollment["result"]["generation"]
    ));
    assert!(
        fixture
            .service
            .dispatch_external_agent_request(&duplicate, &fixture.connection)
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
            .pi_lifecycle
            .is_none()
    );
    let valid = observation(&enrollment, 1, serde_json::json!({"type":"agent_start"}));
    assert!(
        fixture
            .service
            .dispatch_external_agent_request(&valid, &fixture.connection)
            .is_ok()
    );
}
