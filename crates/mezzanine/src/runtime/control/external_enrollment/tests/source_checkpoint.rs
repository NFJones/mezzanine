//! Current public checkpoint export uses actual acceptance/claim caches, not
//! namespace possession or an optional worker's supplied completion status.

use super::*;

/// Ordinary authenticated renewal can return the current known checkpoint
/// without SQLite on actor. A pending new observer cannot export its predecessor.
#[tokio::test(flavor = "current_thread")]
async fn external_source_current_checkpoint_requires_actual_matching_observer_publication() {
    let Some((mut fixture, _store, original, ready)) =
        super::source_activation_fixture::accepted_source().await
    else {
        return;
    };
    let _observer =
        crate::runtime::UnixOriginStream::new(&mut fixture.socket, Some(original.origin.clone()));
    assert_eq!(
        renew(&mut fixture.service, &fixture.connection, &ready)["source_receipt"],
        ready["source_receipt"]
    );
    let mut successor = request();
    let mut params: serde_json::Value =
        serde_json::from_str(successor.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = serde_json::json!("checkpoint-successor");
    params["predecessor_generation"] = ready["generation"].clone();
    successor.params = Some(params.to_string());
    let next =
        super::cold_source_recovery::enroll(&mut fixture.service, &fixture.connection, &successor)
            .await;
    assert_eq!(next["result"]["registered"], true);
    assert!(
        renew(&mut fixture.service, &fixture.connection, &next["result"])
            .get("source_receipt")
            .is_none(),
        "pending successor exported predecessor checkpoint"
    );
    let crate::runtime::RuntimeSideEffect::PersistExternalSourceRenewal { work } = fixture
        .service
        .persistence
        .take_token_usage_effects()
        .pop()
        .unwrap()
    else {
        panic!("missing native observer publication");
    };
    assert!(work.execute_persistence().await);
    assert_eq!(
        fixture.service.complete_external_source_renewal(&work),
        Some(true)
    );
    let checkpoint =
        renew(&mut fixture.service, &fixture.connection, &next["result"])["source_receipt"].clone();
    assert_eq!(checkpoint["protocol"], "external-source-activation/1");
    assert_eq!(
        checkpoint["activation_id"],
        ready["source_receipt"]["activation_id"]
    );
    assert_eq!(checkpoint["observer_generation"], "2");
}

/// Existing Unix-authenticated capability-only RPC; no fixture authority bypass.
fn renew(
    service: &mut RuntimeSessionService,
    connection: &ControlConnectionState,
    ready: &serde_json::Value,
) -> serde_json::Value {
    let request=crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"checkpoint","method":"agent/external/renew","params":{
        "launch_token":ready["launch_token"],"generation":ready["generation"],"external_session_id":"session-a"
    }}).to_string()).unwrap();
    serde_json::from_str(
        &service
            .dispatch_external_agent_request(&request, connection)
            .unwrap(),
    )
    .unwrap()
}
