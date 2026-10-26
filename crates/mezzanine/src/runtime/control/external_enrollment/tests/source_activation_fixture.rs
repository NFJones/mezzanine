//! Genuine accepted-source fixture shared only by lifetime worker/actor tests.

use super::*;

/// Establishes real native observation, actual registration and its separately
/// persisted acceptance. Only fixture-owned stores/selectors are inspected; the
/// private reply is never formatted as failure diagnostics.
pub(super) async fn accepted_source() -> Option<(
    Fixture,
    crate::storage::token_usage::TokenUsageStore,
    ExternalEnrollmentWork,
    serde_json::Value,
)> {
    let mut fixture = fixture("hold").await?;
    let store = crate::storage::token_usage::TokenUsageStore::new(
        fixture.directory.join("retirement.sqlite"),
    );
    fixture.service.set_token_usage_store(store.clone());
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(observed.is_ok(), "{observed:?}");
    let body =
        fixture
            .service
            .complete_external_enrollment(work.clone(), observed, &fixture.connection);
    let publication = fixture
        .service
        .begin_external_source_publication(work, body)
        .unwrap_or_else(|_| panic!("actual admission did not grant publication"));
    let native = publication.clone();
    let accepted = tokio::task::spawn_blocking(move || native.execute_enrollment())
        .await
        .unwrap();
    assert!(accepted.is_ok(), "{accepted:?}");
    let ready: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_source_publication(
            publication.clone(),
            accepted,
            &fixture.connection,
        ))
        .unwrap();
    assert_eq!(ready["result"]["registered"], true);
    Some((fixture, store, publication, ready["result"].clone()))
}
