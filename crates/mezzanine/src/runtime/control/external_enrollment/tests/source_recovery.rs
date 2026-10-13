//! Explicit registry-loss recovery under a retained service/native source owner.
//!
//! These tests use genuine fixture Unix writer and ancestry evidence, not client
//! supplied process metadata. They distinguish same-service registry loss from
//! service replacement and fence exact transition replay, retirement and races.
//! Installed Node/Pi qualification remains a separate integration boundary.

use super::*;

/// A durably recovered owner can later retry its CURRENT receipt during a store
/// outage. This creates no new recovery but preserves the original same-handle
/// receipt, including its existing recovery field, and must not rewind an
/// already advanced Pi sequence or alter original accounting identity.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_recovered_owner_current_retry_keeps_receipt_and_sequence() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    let first = enroll(&mut fixture, &request()).await;
    *fixture.service.control.external_agents_mut() = Default::default();
    let recovered = enroll(&mut fixture, &resume(&first, "fixture-instance-a", None)).await;
    assert_eq!(
        recovered["result"]["recovery"],
        serde_json::json!({"kind":"same-live-source","next_sequence":1})
    );
    fixture
        .service
        .dispatch_external_agent_request(
            &pi(
                &recovered,
                1,
                serde_json::json!({"type":"session_start","reason":"startup"}),
            ),
            &fixture.connection,
        )
        .unwrap();
    let running = pi(&recovered, 2, serde_json::json!({"type":"agent_start"}));
    let original = fixture
        .service
        .dispatch_external_agent_request(&running, &fixture.connection)
        .unwrap();
    let blocked = fixture.directory.join("blocked-current-store");
    std::fs::write(&blocked, b"preserve").unwrap();
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
            blocked.join("usage.sqlite"),
        ));
    let current = resume(&recovered, "fixture-instance-a", None);
    assert_eq!(enroll(&mut fixture, &current).await, recovered);
    assert_eq!(
        fixture
            .service
            .dispatch_external_agent_request(&running, &fixture.connection)
            .unwrap(),
        original
    );
    assert_eq!(
        fixture.service.external_agent_rows()[0]["status"],
        "running"
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
            .enrollment
            .as_ref()
            .unwrap()
            .epoch,
        2
    );
}

/// Current lifecycle credentials remain usable when storage fails, whether the
/// run began without durable accounting or mapped successfully before an outage.
/// Rotation/retry preserves the original owner and durability flag rather than
/// upgrading, relabeling or restoring a namespace. Missing-registry admission
/// still requires durable mapping proof and must not fall back to the live mode.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_live_store_failure_preserves_owner_without_recovery() {
    for originally_durable in [false, true] {
        let Some(mut fixture) = fixture("hold").await else {
            return;
        };
        let blocked = fixture.directory.join("blocked-store");
        std::fs::write(&blocked, b"preserve").unwrap();
        let failed =
            crate::storage::token_usage::TokenUsageStore::new(blocked.join("usage.sqlite"));
        if originally_durable {
            storage(&mut fixture);
        } else {
            fixture.service.set_token_usage_store(failed.clone());
        }
        let first = enroll(&mut fixture, &request()).await;
        let owner = fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .accounting_owner
            .clone();
        fixture.service.set_token_usage_store(failed);
        let own = resume(&first, "fixture-instance-a", None);
        assert_eq!(enroll(&mut fixture, &own).await, first);
        let successor = resume(
            &first,
            "fixture-instance-b",
            Some(first["result"]["generation"].as_u64().unwrap()),
        );
        let second = enroll(&mut fixture, &successor).await;
        assert_eq!(second["result"]["registered"], true, "{second}");
        assert!(second["result"].get("recovery").is_none());
        assert_eq!(enroll(&mut fixture, &successor).await, second);
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
            binding.enrollment.as_ref().unwrap().usage_owner_is_durable,
            originally_durable
        );
        assert!(!binding.enrollment.as_ref().unwrap().recovered_projection);
        assert!(fixture.service.external_token_usage(None).is_empty());
        *fixture.service.control.external_agents_mut() = Default::default();
        let missing = resume(
            &second,
            "fixture-instance-b",
            Some(first["result"]["generation"].as_u64().unwrap()),
        );
        assert!(enroll(&mut fixture, &missing).await.get("error").is_some());
        assert!(
            fixture
                .service
                .control
                .external_agents()
                .bindings
                .is_empty()
        );
        assert_eq!(std::fs::read(blocked).unwrap(), b"preserve");
    }
}

/// A captured current-binding mode cannot become namespace recovery if its
/// binding disappears without the corresponding admission owner changing. The
/// selector and native source still exist, but no implicit mode switch may
/// bypass missing-registry proof or publish another private handle.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_captured_live_mode_does_not_fall_back_after_binding_loss() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    let first = enroll(&mut fixture, &request()).await;
    let successor = resume(
        &first,
        "fixture-instance-b",
        Some(first["result"]["generation"].as_u64().unwrap()),
    );
    let work = fixture
        .service
        .prepare_external_enrollment(&successor, &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    fixture
        .service
        .control
        .external_agents_mut()
        .bindings
        .clear();
    let rejected: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert!(
        rejected
            .to_string()
            .contains("source resume current binding unavailable"),
        "{rejected}"
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
}

/// Existing native lifecycle authority does not depend on a token-usage store.
/// Genuine shutdown/reload and a new observer's captured current receipt and
/// exact predecessor use normal rotation, not accounting-source recovery. The
/// actual reload fact initializes the fresh reducer; no fabricated SDK start or
/// restored outcome is needed. Missing-registry recovery still fails without a
/// durable namespace, and malformed/stale receipt metadata cannot be ignored.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_live_lifecycle_reload_does_not_require_usage_store() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    let first = enroll(&mut fixture, &request()).await;
    assert_eq!(first["result"]["registered"], true, "{first}");
    fixture
        .service
        .dispatch_external_agent_request(
            &pi(
                &first,
                1,
                serde_json::json!({"type":"session_start","reason":"startup"}),
            ),
            &fixture.connection,
        )
        .unwrap();
    fixture
        .service
        .dispatch_external_agent_request(
            &pi(
                &first,
                2,
                serde_json::json!({"type":"session_shutdown","reason":"reload"}),
            ),
            &fixture.connection,
        )
        .unwrap();
    let successor = resume(
        &first,
        "fixture-instance-b",
        Some(first["result"]["generation"].as_u64().unwrap()),
    );
    let second = enroll(&mut fixture, &successor).await;
    assert_eq!(second["result"]["registered"], true, "{second}");
    assert!(second["result"].get("recovery").is_none());
    assert_ne!(
        second["result"]["observer_witness"],
        first["result"]["observer_witness"]
    );
    assert_eq!(second["result"]["observer_epoch"], 2);
    assert_eq!(second["result"]["run_id"], first["result"]["run_id"]);
    assert_eq!(enroll(&mut fixture, &successor).await, second);
    fixture
        .service
        .dispatch_external_agent_request(
            &pi(
                &second,
                1,
                serde_json::json!({"type":"session_start","reason":"reload"}),
            ),
            &fixture.connection,
        )
        .unwrap();
    assert_eq!(fixture.service.external_agent_rows()[0]["status"], "ready");
    assert!(fixture.service.external_token_usage(None).is_empty());
    assert!(
        !fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .enrollment
            .as_ref()
            .unwrap()
            .usage_owner_is_durable
    );
    let current = resume(
        &second,
        "fixture-instance-b",
        Some(first["result"]["generation"].as_u64().unwrap()),
    );
    assert_eq!(enroll(&mut fixture, &current).await, second);
    *fixture.service.control.external_agents_mut() = Default::default();
    assert!(enroll(&mut fixture, &current).await.get("error").is_some());
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
}

/// Bulk snapshot retirement must also fence initial worker evidence which has
/// not yet published any source anchor. Its mapping remains inert; it cannot
/// become a post-snapshot registration merely because the native creator survives.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_bulk_retirement_fences_unpublished_initial_worker() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    let work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    fixture.service.retire_unbound_external_message_identities();
    let rejected: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert!(
        rejected
            .to_string()
            .contains("external enrollment reservation owner changed")
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
    assert!(
        enroll(&mut fixture, &request())
            .await
            .to_string()
            .contains("same-live source activation unavailable")
    );
}

/// Once disposable registrations disappear, the retained source is still the
/// exact descriptor owner. Existing maintenance must remain scheduled until
/// genuine creator exit, then release ancestry and stop without new enrollment
/// traffic. No timestamp or arbitrary label substitutes for native source death.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_anchor_alone_keeps_cleanup_until_actual_creator_exit() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    let first = enroll(&mut fixture, &request()).await;
    assert_eq!(first["result"]["registered"], true);
    let budget = fixture
        .service
        .control
        .external_agents()
        .enrollments
        .ancestry_budget
        .clone();
    *fixture.service.control.external_agents_mut() = Default::default();
    assert_eq!(budget.reserved(), 1);
    assert!(fixture.service.external_agent_cleanup_needed());
    super::helper_presentation::release(&mut fixture.socket, &fixture.connection).await;
    fixture.service.reconcile_external_agent_registrations();
    assert_eq!(budget.reserved(), 0);
    assert!(!fixture.service.external_agent_cleanup_needed());
}

/// A different real child creator cannot adopt another source's receipt even
/// though UID, pane hint and declared session are equal. Root-incarnation and
/// mapping mismatches after genuine worker capture also reject settlement with
/// no credential/projection publication. This is not an OS PID-reuse simulation;
/// storage witness tests cover canonical incarnation separation independently.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_foreign_creator_root_and_mapping_cannot_adopt_receipt() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    let first = enroll(&mut fixture, &request()).await;
    let recovery = resume(&first, "fixture-instance-a", None);
    let (mut socket, foreign) = super::helper_presentation::child(&mut fixture).await;
    assert_ne!(
        foreign.unix_origin().unwrap().identity.process_id,
        fixture
            .connection
            .unix_origin()
            .unwrap()
            .identity
            .process_id
    );
    assert!(
        fixture
            .service
            .prepare_external_enrollment(&recovery, &foreign)
            .is_err()
    );
    super::helper_presentation::release(&mut socket, &foreign).await;
    for mismatch in ["root", "mapping"] {
        let mut work = fixture
            .service
            .prepare_external_enrollment(&recovery, &fixture.connection)
            .unwrap();
        let native = work.clone();
        let observed = tokio::task::spawn_blocking(move || native.observe())
            .await
            .unwrap();
        assert!(observed.is_ok());
        match mismatch {
            "root" => work.process.start_token = work.process.start_token.wrapping_add(1),
            _ => work.source_owner = Arc::new(OnceLock::from(Some("foreign-owner".into()))),
        }
        let rejected: serde_json::Value = serde_json::from_str(
            &fixture
                .service
                .complete_external_enrollment(work, observed, &fixture.connection),
        )
        .unwrap();
        assert!(rejected.get("error").is_some(), "{mismatch}: {rejected}");
    }
    assert_eq!(enroll(&mut fixture, &request()).await, first);
    assert_eq!(fixture.service.control.external_agents().bindings.len(), 1);
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

/// Finite retained-source capacity does not evict active source or retired
/// fences. Registry reset retains the original aggregate ancestry quota; explicit
/// current-source recovery remains usable while a genuinely new namespace fails
/// before source allocation or native work. Retirement releases ancestry only.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_anchor_capacity_and_shared_budget_survive_reset() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    fixture.service.external_source_anchor_limit_for_tests(1);
    let first = enroll(&mut fixture, &request()).await;
    let budget = fixture
        .service
        .control
        .external_agents()
        .enrollments
        .ancestry_budget
        .clone();
    assert_eq!(budget.reserved(), 1);
    *fixture.service.control.external_agents_mut() = Default::default();
    assert_eq!(budget.reserved(), 1);
    let mut fresh = request();
    let mut params: serde_json::Value =
        serde_json::from_str(fresh.params.as_deref().unwrap()).unwrap();
    params["external_session_id"] = "new-session-b".into();
    fresh.params = Some(params.to_string());
    assert!(
        enroll(&mut fixture, &fresh)
            .await
            .to_string()
            .contains("same-live source anchor capacity unavailable")
    );
    let recovery = resume(&first, "fixture-instance-a", None);
    let second = enroll(&mut fixture, &recovery).await;
    assert_eq!(second["result"]["registered"], true, "{second}");
    assert!(Arc::ptr_eq(
        &budget,
        &fixture
            .service
            .control
            .external_agents()
            .enrollments
            .ancestry_budget
    ));
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
    assert_eq!(budget.reserved(), 0);
    assert!(enroll(&mut fixture, &fresh).await.get("error").is_some());
}

/// A committed mapping does not mean initial native admission published. Losing
/// that admission before its actor deadline leaves an inert mapping and an
/// explicit availability gap, never an alternate owner or historical recharge.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_abandoned_mapping_cannot_activate_initial_retry() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    let mut work = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = work.clone();
    let observed = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert!(observed.is_ok());
    assert_eq!(work.source_created.get(), Some(&true));
    work.deadline = Instant::now();
    let expired: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(work, observed, &fixture.connection),
    )
    .unwrap();
    assert!(expired.get("error").is_some());
    let retry = enroll(&mut fixture, &request()).await;
    assert!(
        retry
            .to_string()
            .contains("same-live source activation unavailable"),
        "{retry}"
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
    let database = rusqlite::Connection::open(fixture.directory.join("usage.sqlite")).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

/// Concurrent initial workers resolve one New and one Existing mapping. An
/// Existing completion arriving before activation must fail safely; the actual
/// New worker may still publish once and later exact retries reuse that result.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_initial_mapping_order_never_reconstructs_authority() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    let new = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let existing = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    let native = new.clone();
    let new_result = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    let native = existing.clone();
    let existing_result = tokio::task::spawn_blocking(move || native.observe())
        .await
        .unwrap();
    assert_eq!(new.source_created.get(), Some(&true));
    assert_eq!(existing.source_created.get(), Some(&false));
    let rejected: serde_json::Value =
        serde_json::from_str(&fixture.service.complete_external_enrollment(
            existing,
            existing_result,
            &fixture.connection,
        ))
        .unwrap();
    assert!(rejected.get("error").is_some());
    let published: serde_json::Value = serde_json::from_str(
        &fixture
            .service
            .complete_external_enrollment(new, new_result, &fixture.connection),
    )
    .unwrap();
    assert_eq!(published["result"]["registered"], true, "{published}");
    assert_eq!(enroll(&mut fixture, &request()).await, published);
    assert_eq!(fixture.service.control.external_agents().bindings.len(), 1);
}

/// A disposed registry's numeric reservation can coincide with a new one, but
/// its original owner must never consume or publish the new reservation. The
/// retained native source still qualifies the separately prepared fresh work.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_registry_incarnation_fences_pending_work() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    let first = enroll(&mut fixture, &request()).await;
    let old = fixture
        .service
        .prepare_external_enrollment(&request(), &fixture.connection)
        .unwrap();
    *fixture.service.control.external_agents_mut() = Default::default();
    let recovery = resume(&first, "fixture-instance-a", None);
    let current = fixture
        .service
        .prepare_external_enrollment(&recovery, &fixture.connection)
        .unwrap();
    assert!(settle(&mut fixture, old).await.get("error").is_some());
    assert_eq!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .len(),
        1
    );
    let recovered = settle(&mut fixture, current).await;
    assert_eq!(recovered["result"]["registered"], true, "{recovered}");
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

/// Central retirement fences the whole exact source lifetime, not only one
/// instance spelling. A new instance/display/version cannot reactivate it after
/// ephemeral tombstones are discarded; none of these labels proves a new run.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_retired_source_rejects_all_fresh_instance_labels() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    let first = enroll(&mut fixture, &request()).await;
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    fixture.service.retire_external_agent_binding(digest);
    *fixture.service.control.external_agents_mut() = Default::default();
    for instance in ["fixture-instance-a", "new-instance-b"] {
        let mut candidate = request();
        let mut params: serde_json::Value =
            serde_json::from_str(candidate.params.as_deref().unwrap()).unwrap();
        params["observer_instance"] = instance.into();
        params["display_name"] = "changed label".into();
        params["version"] = "changed version".into();
        candidate.params = Some(params.to_string());
        assert!(
            enroll(&mut fixture, &candidate)
                .await
                .get("error")
                .is_some()
        );
    }
    assert!(
        enroll(
            &mut fixture,
            &resume(
                &first,
                "new-instance-c",
                Some(first["result"]["generation"].as_u64().unwrap())
            )
        )
        .await
        .get("error")
        .is_some()
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
}

/// Builds a typed Pi fact under an actually accepted native private handle.
fn pi(receipt: &serde_json::Value, sequence: u64, event: serde_json::Value) -> JsonRpcRequest {
    crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"pi","method":"agent/external/pi-observation","params":{
        "launch_token":receipt["result"]["launch_token"],"generation":receipt["result"]["generation"],
        "external_session_id":"session-a","sequence":sequence,"event":event,
    }}).to_string()).unwrap()
}

/// Recovery is independently qualified; its first actual diagnostic gap does
/// not fabricate a SDK session_start, ready/running presentation, outcome, lease
/// renewal or expense. Exact enrollment replay after sequence advancement must
/// leave the new projection untouched, while normal first-gap rejection remains.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_first_actual_pi_gap_is_diagnostic_and_retry_inert() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    let first = enroll(&mut fixture, &request()).await;
    let gap = serde_json::json!({"type":"usage_gap","reason":"report-clock"});
    assert!(
        fixture
            .service
            .dispatch_external_agent_request(&pi(&first, 1, gap.clone()), &fixture.connection)
            .is_err()
    );
    *fixture.service.control.external_agents_mut() = Default::default();
    let recovery = resume(&first, "fixture-instance-a", None);
    let second = enroll(&mut fixture, &recovery).await;
    assert_eq!(second["result"]["registered"], true, "{second}");
    let request = pi(&second, 1, gap);
    let accepted = fixture
        .service
        .dispatch_external_agent_request(&request, &fixture.connection)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .dispatch_external_agent_request(&request, &fixture.connection)
            .unwrap(),
        accepted
    );
    let binding = fixture
        .service
        .control
        .external_agents()
        .bindings
        .values()
        .next()
        .unwrap();
    assert!(
        binding
            .registration
            .as_ref()
            .unwrap()
            .presentation
            .is_none()
    );
    assert!(!binding.retired);
    assert_eq!(
        binding.pi_lifecycle.as_ref().unwrap().usage_gap_coverage(),
        Some("unavailable-pi-usage-report-clock")
    );
    let running = pi(&second, 2, serde_json::json!({"type":"agent_start"}));
    fixture
        .service
        .dispatch_external_agent_request(&running, &fixture.connection)
        .unwrap();
    assert_eq!(enroll(&mut fixture, &recovery).await, second);
    assert_eq!(
        fixture
            .service
            .dispatch_external_agent_request(&running, &fixture.connection)
            .unwrap(),
        serde_json::json!({"accepted":true,"sequence":2,"retired":false}).to_string()
    );
    *fixture.service.control.external_agents_mut() = Default::default();
    let third = enroll(&mut fixture, &resume(&second, "fixture-instance-a", None)).await;
    fixture
        .service
        .dispatch_external_agent_request(
            &pi(&third, 1, serde_json::json!({"type":"agent_start"})),
            &fixture.connection,
        )
        .unwrap();
    assert_eq!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap()
            .pi_lifecycle
            .as_ref()
            .unwrap()
            .usage_gap_coverage(),
        Some("unavailable-pi-usage-report-clock")
    );
    assert!(fixture.service.external_token_usage(None).is_empty());
}

/// Resume selectors are strict inert receipt metadata, not a generic extension
/// object. Null/extra keys/unsafe numeric rounding/uppercase witnesses and mixed
/// source/session/version/predecessor attempts cannot reserve native admission.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_bad_selectors_and_metadata_reserve_nothing() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    let first = enroll(&mut fixture, &request()).await;
    let valid = resume(&first, "fixture-instance-a", None);
    for selector in [
        serde_json::Value::Null,
        serde_json::json!({"observer_witness":"a".repeat(64),"generation":0}),
        serde_json::json!({"observer_witness":"a".repeat(64),"generation":9_007_199_254_740_992_u64}),
        serde_json::json!({"observer_witness":"A".repeat(64),"generation":1}),
        serde_json::json!({"observer_witness":"a".repeat(64),"generation":1,"owner":"foreign"}),
    ] {
        let mut candidate = valid.clone();
        let mut params: serde_json::Value =
            serde_json::from_str(candidate.params.as_deref().unwrap()).unwrap();
        params["source_resume"] = selector;
        candidate.params = Some(params.to_string());
        assert!(
            fixture
                .service
                .prepare_external_enrollment(&candidate, &fixture.connection)
                .is_err()
        );
    }
    for (key, value) in [
        ("harness", serde_json::json!("claude")),
        ("external_session_id", serde_json::json!("foreign-session")),
        ("display_name", serde_json::json!("foreign-metadata")),
        ("version", serde_json::json!("foreign-version")),
        (
            "predecessor_generation",
            first["result"]["generation"].clone(),
        ),
    ] {
        let mut candidate = valid.clone();
        let mut params: serde_json::Value =
            serde_json::from_str(candidate.params.as_deref().unwrap()).unwrap();
        params[key] = value;
        candidate.params = Some(params.to_string());
        assert!(
            fixture
                .service
                .prepare_external_enrollment(&candidate, &fixture.connection)
                .is_err()
        );
    }
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    assert_eq!(fixture.service.control.external_agents().bindings.len(), 1);
}

/// Installs only a unique fixture SQLite ledger; no user storage is opened.
fn storage(fixture: &mut Fixture) {
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(
            fixture.directory.join("usage.sqlite"),
        ));
}

/// Constructs the explicit published receipt selector while preserving the
/// observer's immutable predecessor field. Selectors alone grant no authority.
fn resume(receipt: &serde_json::Value, instance: &str, predecessor: Option<u64>) -> JsonRpcRequest {
    let mut request = request();
    let mut params: serde_json::Value =
        serde_json::from_str(request.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = instance.into();
    params["source_resume"] = serde_json::json!({
        "observer_witness":receipt["result"]["observer_witness"],
        "generation":receipt["result"]["generation"],
    });
    if let Some(predecessor) = predecessor {
        params["predecessor_generation"] = predecessor.into();
    }
    request.params = Some(params.to_string());
    request
}

/// Completes genuine off-actor observation, including negative admission errors
/// in a uniform test reply. An injected successful result never supplies proof.
async fn enroll(fixture: &mut Fixture, request: &JsonRpcRequest) -> serde_json::Value {
    let work = match fixture
        .service
        .prepare_external_enrollment(request, &fixture.connection)
    {
        Ok(work) => work,
        Err(error) => return serde_json::json!({"error":error.message()}),
    };
    settle(fixture, work).await
}

/// Settles only the captured worker and its original qualified connection.
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

/// Same-instance explicit recovery allocates one fresh projection/credential,
/// not one per retry. Exact lost-response replay preserves its active result;
/// distinct reconnect with the current receipt creates the next owner. A second
/// registry reset does not turn an old accepted input into a newest lookup.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_exact_receipt_and_repeated_registry_loss() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    let first = enroll(&mut fixture, &request()).await;
    assert_eq!(first["result"]["registered"], true);
    *fixture.service.control.external_agents_mut() = Default::default();
    storage(&mut fixture);
    assert!(
        enroll(&mut fixture, &request())
            .await
            .get("error")
            .is_some(),
        "implicit restoration must fail closed"
    );
    let recovery = resume(&first, "fixture-instance-a", None);
    let second = enroll(&mut fixture, &recovery).await;
    assert_eq!(
        second["result"]["recovery"],
        serde_json::json!({"kind":"same-live-source","next_sequence":1}),
        "{second}"
    );
    assert_ne!(
        second["result"]["launch_token"],
        first["result"]["launch_token"]
    );
    assert_ne!(
        second["result"]["observer_witness"],
        first["result"]["observer_witness"]
    );
    assert_ne!(
        second["result"]["generation"],
        first["result"]["generation"]
    );
    assert_eq!(second["result"]["run_id"], first["result"]["run_id"]);
    assert_eq!(second["result"]["observer_epoch"], 2);
    assert!(
        enroll(&mut fixture, &request())
            .await
            .get("error")
            .is_some()
    );
    assert_eq!(enroll(&mut fixture, &recovery).await, second);
    *fixture.service.control.external_agents_mut() = Default::default();
    let unavailable = enroll(&mut fixture, &recovery).await;
    assert!(unavailable.get("error").is_some(), "{unavailable}");
    assert!(
        unavailable
            .to_string()
            .contains("source resume retry unavailable")
    );
    assert!(
        fixture
            .service
            .control
            .external_agents()
            .bindings
            .is_empty()
    );
    let current = resume(&second, "fixture-instance-a", None);
    let third = enroll(&mut fixture, &current).await;
    assert_eq!(third["result"]["observer_epoch"], 3, "{third}");
    assert_ne!(
        third["result"]["observer_witness"],
        second["result"]["observer_witness"]
    );
    assert!(enroll(&mut fixture, &recovery).await.get("error").is_some());
}

/// Native creator survival cannot reactivate a retired run. The same central
/// retirement fence also rejects worker evidence captured before retirement,
/// even when disposable registry state disappears before its settlement.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_retirement_fences_pending_and_reset() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    let first = enroll(&mut fixture, &request()).await;
    let recovery = resume(&first, "fixture-instance-a", None);
    let pending = fixture
        .service
        .prepare_external_enrollment(&recovery, &fixture.connection)
        .unwrap();
    let digest = *fixture
        .service
        .control
        .external_agents()
        .bindings
        .keys()
        .next()
        .unwrap();
    fixture.service.retire_external_agent_binding(digest);
    assert!(settle(&mut fixture, pending).await.get("error").is_some());
    *fixture.service.control.external_agents_mut() = Default::default();
    assert!(enroll(&mut fixture, &recovery).await.get("error").is_some());
    assert!(
        enroll(&mut fixture, &request())
            .await
            .get("error")
            .is_some()
    );
}

/// Two native-qualified successors prepared against one anchor revision cannot
/// both publish. The winning result invalidates the old instance and old receipt
/// across a registry reset; original-instance and predecessor selectors are not
/// a way to discover or borrow the winner's current authority.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_competing_successors_and_old_instance_reject() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    let first = enroll(&mut fixture, &request()).await;
    let generation = first["result"]["generation"].as_u64().unwrap();
    let b = resume(&first, "fixture-instance-b", Some(generation));
    let c = resume(&first, "fixture-instance-c", Some(generation));
    let left = fixture
        .service
        .prepare_external_enrollment(&b, &fixture.connection)
        .unwrap();
    let right = fixture
        .service
        .prepare_external_enrollment(&c, &fixture.connection)
        .unwrap();
    let winner = settle(&mut fixture, left).await;
    assert_eq!(winner["result"]["registered"], true, "{winner}");
    assert!(settle(&mut fixture, right).await.get("error").is_some());
    *fixture.service.control.external_agents_mut() = Default::default();
    assert!(
        enroll(&mut fixture, &resume(&first, "fixture-instance-a", None))
            .await
            .get("error")
            .is_some()
    );
    assert!(enroll(&mut fixture, &c).await.get("error").is_some());
    assert!(
        enroll(
            &mut fixture,
            &resume(
                &winner,
                "fixture-instance-a",
                Some(winner["result"]["generation"].as_u64().unwrap())
            )
        )
        .await
        .get("error")
        .is_some()
    );
    let own = resume(&winner, "fixture-instance-b", Some(generation));
    let recovered = enroll(&mut fixture, &own).await;
    assert_eq!(recovered["result"]["registered"], true, "{recovered}");
    assert_eq!(recovered["result"]["run_id"], first["result"]["run_id"]);
}

/// Private accounting mapping is not persisted activation authority. Removing
/// the retained control owner while keeping the root/socket and SQLite alive
/// models service-owner loss; an explicit old receipt must fail closed.
#[tokio::test(flavor = "current_thread")]
async fn external_source_resume_missing_control_owner_does_not_adopt_mapping() {
    let Some(mut fixture) = fixture("hold").await else {
        return;
    };
    storage(&mut fixture);
    let first = enroll(&mut fixture, &request()).await;
    fixture.service.control = crate::runtime::control::RuntimeControlComponent::new(
        crate::control::ControlIdempotencyCache::default(),
        mez_agent::messaging::MessageService::default(),
        None,
    );
    storage(&mut fixture);
    assert!(
        enroll(&mut fixture, &request())
            .await
            .to_string()
            .contains("same-live source activation unavailable")
    );
    let mut fresh = request();
    let mut params: serde_json::Value =
        serde_json::from_str(fresh.params.as_deref().unwrap()).unwrap();
    params["observer_instance"] = "fresh-instance-after-owner-loss".into();
    fresh.params = Some(params.to_string());
    assert!(
        enroll(&mut fixture, &fresh)
            .await
            .to_string()
            .contains("same-live source activation unavailable")
    );
    assert!(
        enroll(&mut fixture, &resume(&first, "fixture-instance-a", None))
            .await
            .get("error")
            .is_some()
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
    let database = rusqlite::Connection::open(fixture.directory.join("usage.sqlite")).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
