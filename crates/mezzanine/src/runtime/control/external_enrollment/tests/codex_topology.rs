//! Known shared-server rejection under genuine native pane ancestry and routing.

use super::codex_observation::declaration;
use super::helper_presentation::{child, settle};
use super::*;

/// A normally launched native creator has actual shared-server argv, valid pane
/// ancestry and inherited MEZ routing. Those facts cannot identify a requesting
/// client/session. Real helper observation must refuse before source ownership,
/// registration, request readiness, expense, presence or client-role effects.
/// Arguments are fixture-owned and never included in the public error.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_topology_shared_native_creator_cannot_borrow_launching_pane() {
    let Some(mut fixture) = fixture("codex-shared").await else {
        panic!("Codex topology qualification requires native peer evidence")
    };
    let path = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&path));
    let (_socket, connection) = child(&mut fixture).await;
    let work = fixture
        .service
        .prepare_external_enrollment(&declaration(), &connection)
        .unwrap();
    let reply = settle(&mut fixture, work, &connection).await;
    assert_eq!(
        reply["error"]["message"], "unavailable-codex-native-client-session-association",
        "{reply}"
    );
    let mut direct = request();
    let mut params: serde_json::Value =
        serde_json::from_str(direct.params.as_deref().unwrap()).unwrap();
    params["harness"] = "codex".into();
    direct.params = Some(params.to_string());
    let direct_connection = fixture.connection.clone();
    let work = fixture
        .service
        .prepare_external_enrollment(&direct, &direct_connection)
        .unwrap();
    let direct_reply = settle(&mut fixture, work, &direct_connection).await;
    assert_eq!(
        direct_reply["error"]["message"], "unavailable-codex-native-client-session-association",
        "{direct_reply}"
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
    assert!(fixture.service.external_agent_rows().is_empty());
    assert!(fixture.service.live_pane_harness_status("%1").is_none());
    assert!(!connection.initialized());
    assert!(connection.caller_client_id().is_none());
    // Source ownership itself never ran: no database is created by a refusal.
    assert!(!path.exists());
}
