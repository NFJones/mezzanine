//! Separate local command-hook creator declaration under native admission.
//!
//! Self-reported labels never supply authority. Direct-parent writer/root
//! qualification remains identical, while harness names keep observational
//! namespaces disjoint even when all public session/display selectors collide.

use super::curated_admission::{curated_request, enroll};
use super::helper_presentation::{child, settle};
use super::*;

/// A changed boundary from one living creator/session/instance is not an exact
/// admission retry and cannot certify a same-ID transition. Rejection must leave
/// the original owner, generation, lease and presentation unchanged. A genuinely
/// fresh observer with the exact predecessor may change boundaries; its own
/// repeated declaration is still idempotent and cannot borrow the old boundary.
#[tokio::test(flavor = "current_thread")]
async fn external_curated_changed_boundary_requires_fenced_observer_handoff() {
    for harness in ["claude", "codex"] {
        let Some(mut fixture) = fixture("hold").await else {
            panic!("curated boundary qualification requires native peer evidence");
        };
        let (_socket, connection) = child(&mut fixture).await;
        let mut request = curated_request();
        let mut params: serde_json::Value =
            serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
        if harness == "codex" {
            params["harness"] = "codex".into();
            params["observer_kind"] = "command-hook".into();
            params["source_contract"] = "codex-local-command/1".into();
        }
        request.params = Some(params.to_string());
        let work = fixture
            .service
            .prepare_external_enrollment(&request, &connection)
            .unwrap();
        let original = settle(&mut fixture, work, &connection).await;
        assert_eq!(original["result"]["registered"], true, "{original}");
        let binding = fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap();
        let owner = binding.accounting_owner.clone();
        let generation = binding.generation;
        let expires = binding.expires;
        let rows = fixture.service.external_agent_rows();
        for boundary in ["resume", "clear", "fork"] {
            let mut changed = params.clone();
            changed["session_boundary"] = boundary.into();
            let mut candidate = request.clone();
            candidate.params = Some(changed.to_string());
            let work = fixture
                .service
                .prepare_external_enrollment(&candidate, &connection)
                .unwrap();
            let refused = settle(&mut fixture, work, &connection).await;
            assert!(
                refused.get("error").is_some(),
                "{harness}/{boundary}: {refused}"
            );
            let registry = fixture.service.control.external_agents();
            assert_eq!(registry.bindings.len(), 1);
            let binding = registry.bindings.values().next().unwrap();
            assert_eq!(binding.accounting_owner, owner);
            assert_eq!(binding.generation, generation);
            assert_eq!(binding.expires, expires);
            assert_eq!(binding.enrollment.as_ref().unwrap().epoch, 1);
            assert!(registry.enrollments.pending.is_empty());
            assert_eq!(fixture.service.external_agent_rows(), rows);
        }
        let work = fixture
            .service
            .prepare_external_enrollment(&request, &connection)
            .unwrap();
        assert_eq!(settle(&mut fixture, work, &connection).await, original);
        let mut failed = params.clone();
        failed["observer_instance"] = "failed-module".into();
        failed["session_boundary"] = "fork".into();
        failed["predecessor_generation"] = (generation + 1).into();
        let mut candidate = request.clone();
        candidate.params = Some(failed.to_string());
        let work = fixture
            .service
            .prepare_external_enrollment(&candidate, &connection)
            .unwrap();
        assert!(
            settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_some()
        );
        let work = fixture
            .service
            .prepare_external_enrollment(&request, &connection)
            .unwrap();
        assert_eq!(settle(&mut fixture, work, &connection).await, original);
        let binding = fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap();
        assert_eq!(binding.generation, generation);
        assert_eq!(binding.expires, expires);
        assert_eq!(binding.accounting_owner, owner);
        params["observer_instance"] = "module-b".into();
        params["session_boundary"] = "clear".into();
        params["predecessor_generation"] = generation.into();
        request.params = Some(params.to_string());
        let work = fixture
            .service
            .prepare_external_enrollment(&request, &connection)
            .unwrap();
        let successor = settle(&mut fixture, work, &connection).await;
        assert_eq!(successor["result"]["registered"], true, "{successor}");
        assert_eq!(successor["result"]["observer_epoch"], 2);
        let successor_expiry = fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .expires;
        let work = fixture
            .service
            .prepare_external_enrollment(&request, &connection)
            .unwrap();
        assert_eq!(settle(&mut fixture, work, &connection).await, successor);
        params["session_boundary"] = "startup".into();
        request.params = Some(params.to_string());
        let work = fixture
            .service
            .prepare_external_enrollment(&request, &connection)
            .unwrap();
        assert!(
            settle(&mut fixture, work, &connection)
                .await
                .get("error")
                .is_some()
        );
        let binding = fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap();
        assert_eq!(binding.accounting_owner, owner);
        assert_eq!(
            binding.generation,
            successor["result"]["generation"].as_u64().unwrap()
        );
        assert_eq!(binding.expires, successor_expiry);
        assert_eq!(binding.enrollment.as_ref().unwrap().epoch, 2);
        assert!(!connection.initialized());
        assert!(connection.caller_client_id().is_none());
    }
}

/// Distinct harness declarations sharing one genuine creator, pane root and
/// session must not merge registrations or billing. The Codex declaration models
/// the installed exec-based local command contract, not shared-server attribution
/// or an installed callback. Exact retries expose no capability/client/usage
/// authority and leave the original finite unobserved lease unchanged.
#[tokio::test(flavor = "current_thread")]
async fn external_codex_command_creator_admission_keeps_harness_namespaces_disjoint() {
    let Some(mut fixture) = fixture("hold").await else {
        panic!("local command creator qualification requires native peer evidence");
    };
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
            fixture.directory.join("usage.sqlite"),
        ));
    let (_socket, connection) = child(&mut fixture).await;
    let original = enroll(&mut fixture, &connection).await;
    assert_eq!(original["result"]["registered"], true);
    let mut request = curated_request();
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["harness"] = "codex".into();
    params["observer_kind"] = "command-hook".into();
    params["source_contract"] = "codex-local-command/1".into();
    request.params = Some(params.to_string());
    let work = fixture
        .service
        .prepare_external_enrollment(&request, &connection)
        .unwrap();
    let first = settle(&mut fixture, work, &connection).await;
    assert_eq!(first["result"]["registered"], true, "{first}");
    assert_ne!(first["result"]["agent_id"], original["result"]["agent_id"]);
    assert!(first["result"].get("launch_token").is_none());
    assert_eq!(first["result"]["controls"], serde_json::json!([]));
    assert_eq!(first["result"]["usage"], "unavailable-source-continuity");
    assert_eq!(
        first["result"]["observer_transport"],
        "unavailable-curated-freshness"
    );
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 2);
    assert_eq!(
        registry
            .bindings
            .values()
            .map(|binding| binding.harness.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["claude", "codex"])
    );
    assert_eq!(
        registry
            .bindings
            .values()
            .map(|binding| binding.accounting_owner.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    assert!(
        registry
            .bindings
            .values()
            .all(|binding| { binding.enrollment.as_ref().unwrap().usage_owner_is_durable })
    );
    let database = rusqlite::Connection::open(fixture.directory.join("usage.sqlite")).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT count(DISTINCT owner) FROM external_usage_sources",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2
    );
    let work = fixture
        .service
        .prepare_external_enrollment(&request, &connection)
        .unwrap();
    assert_eq!(settle(&mut fixture, work, &connection).await, first);
    assert_eq!(enroll(&mut fixture, &connection).await, original);
    assert!(!connection.initialized());
    assert!(connection.caller_client_id().is_none());
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );

    for (field, value) in [
        ("harness", "claude"),
        ("observer_kind", "curated-command"),
        ("source_contract", "claude-curated-command/1"),
        ("source_contract", "codex-shared-server/1"),
        ("session_boundary", "compact"),
    ] {
        let mut invalid = params.clone();
        invalid[field] = value.into();
        let mut candidate = request.clone();
        candidate.params = Some(invalid.to_string());
        assert!(
            fixture
                .service
                .prepare_external_enrollment(&candidate, &connection)
                .is_err()
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
    let root_connection = fixture.connection.clone();
    let work = fixture
        .service
        .prepare_external_enrollment(&request, &root_connection)
        .unwrap();
    let rejected = settle(&mut fixture, work, &root_connection).await;
    assert!(
        rejected.get("error").is_some(),
        "pane-root creator must not be admitted"
    );
    assert_eq!(fixture.service.control.external_agents().bindings.len(), 2);
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
