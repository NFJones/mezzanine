//! Production external accounting ingress and checkpoint projection regressions.
//!
//! The actor authenticates immutable registration provenance, workers commit
//! SQLite outside actor ownership, and replies acknowledge only durable facts.

use super::*;
use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
use crate::runtime::current_unix_seconds;
use crate::storage::token_usage::TokenUsageStore;

mod precommit_retry_controls;
mod view_resets;

/// Invalid combined cache coverage must reject before actor reservations or
/// durable writes, for both delta reports and uncharged cumulative attachment.
/// The same original report identity remains usable after telemetry correction;
/// neither failure nor successful metadata attachment implies provider replay.
#[test]
fn runtime_external_cache_conservation_rejects_before_reservation() {
    for mode in ["delta", "cumulative"] {
        let (mut service, connection, mut params, store) = fixture();
        params["mode"] = mode.into();
        params["baseline"] = (mode == "cumulative").into();
        params["counters"]["cached_input_tokens"] = 6.into();
        params["counters"]["cache_write_input_tokens"] = 5.into();
        let request = |params: &serde_json::Value| {
            crate::control::parse_json_rpc_request(
                &serde_json::json!({
                    "jsonrpc":"2.0","id":"cache","method":"agent/external/usage","params":params
                })
                .to_string(),
            )
            .unwrap()
        };
        assert!(
            service
                .prepare_external_usage(&request(&params), &connection)
                .is_err(),
            "{mode}"
        );
        assert_eq!(
            service.external_usage_reservation_counts_for_tests(),
            (0, 0)
        );
        assert_eq!(external_ledger_input(&store), 0);
        params["counters"]["cache_write_input_tokens"] = 4.into();
        let work = service
            .prepare_external_usage(&request(&params), &connection)
            .unwrap();
        let commit = work.execute().unwrap();
        let reply: serde_json::Value =
            serde_json::from_str(&service.complete_external_usage(work, Ok(commit))).unwrap();
        assert_eq!(reply["result"]["durable"], true, "{reply}");
        assert_eq!(
            service.external_usage_reservation_counts_for_tests(),
            (0, 0)
        );
        assert_eq!(
            external_ledger_input(&store),
            if mode == "delta" { 10 } else { 0 }
        );
        service.terminate_all_pane_processes().unwrap();
    }
}

/// Group admission must not spend a slot per finalized message. Real legacy
/// registration/admission supplies captured scope, while the mapped-source SQL
/// and preexisting ledger expense here are explicitly synthetic fixtures, not
/// ordinary native producer authority. Full-cache migration preserves original
/// reset baselines; reversed snapshots and pending old replies cannot recreate
/// members or revive reset expense, and unrelated groups reject before writes.
#[test]
fn runtime_external_opencode_groups_preserve_reset_and_late_member_capacity() {
    qualify_opencode_group_projection(18, false);
}

/// A canonical-ID checkpoint may commit before migration but arrive at the
/// actor afterward. It must become a distinct member rather than disappear or
/// overwrite the aggregate; later versions, reset and snapshots conserve it.
#[test]
fn runtime_external_opencode_delayed_canonical_id_preserves_expense() {
    qualify_opencode_group_projection(4, true);
}

/// Explicit long-session qualification drives 4,097 distinct adopted messages
/// through the real admitted worker and actor cache, not only normalization.
/// Native ordinary-producer association remains deliberately outside this
/// synthetic mapped legacy fixture; expense conservation and one cache group
/// are checked beyond the old 4,096 projection-entry bound.
#[test]
#[ignore = "explicit bounded 4097-member ledger/actor workload"]
fn runtime_external_opencode_group_projection_exceeds_legacy_stream_cap() {
    qualify_opencode_group_projection(4097, false);
}

/// Shared isolated legacy-scope fixture for short reset/order and long capacity
/// controls. Count varies only synthetic finalized-message metadata, not scope.
fn qualify_opencode_group_projection(count: u64, canonical: bool) {
    let (mut service, mut connection, mut params, store) = fixture();
    service.set_external_binding_harness_for_tests("opencode");
    params["counters"]["cache_write_input_tokens"] = 0.into();
    let seed_request = crate::control::parse_json_rpc_request(
        &serde_json::json!({
            "jsonrpc":"2.0","id":"seed","method":"agent/external/usage","params":params
        })
        .to_string(),
    )
    .unwrap();
    let seed = service
        .prepare_external_usage(&seed_request, &connection)
        .unwrap();
    let owner = seed.report.owner.clone();
    let project = seed.report.project.clone();
    service.complete_external_usage(
        seed,
        Err(crate::error::MezError::conflict("fixture scope lookup")),
    );
    let now = params["observed_at"].as_u64().unwrap();
    let sql = rusqlite::Connection::open(store.path()).unwrap();
    sql.execute(
        "INSERT INTO external_usage_sources(witness,harness,owner) VALUES(?1,'opencode',?2)",
        rusqlite::params!["a".repeat(64), owner],
    )
    .unwrap();
    drop(sql);
    let make = |index: u64, model: &str| {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "role":"assistant","sessionID":"usage-run","id":format!("group-message-{index}"),
            "providerID":"provider","modelID":model,"time":{"completed":now*1000+index%1000},
            "tokens":{"input":7,"output":2,"reasoning":2,"cache":{"read":3,"write":0}}
        }))
        .unwrap();
        let pair = crate::integrations::bootstrap::opencode::completed_observation(
            "fixture",
            "usage-run",
            &owner,
            project.clone(),
            &bytes,
        )
        .unwrap()
        .unwrap();
        let mut body = params.clone();
        body["epoch"] = pair.legacy().epoch.clone().into();
        body["event_id"] = pair.legacy().event_id.clone().into();
        body["model"] = model.into();
        let request = crate::control::parse_json_rpc_request(&serde_json::json!({
            "jsonrpc":"2.0","id":format!("group-{index}"),"method":"agent/external/usage","params":body
        }).to_string()).unwrap();
        (request, pair)
    };
    let (request0, pair0) = make(0, "model");
    let (request1, pair1) = make(1, "model");
    for request in [&request0, &request1] {
        let work = service
            .prepare_external_usage(request, &connection)
            .unwrap();
        let commit = work.execute().unwrap();
        let reply: serde_json::Value =
            serde_json::from_str(&service.complete_external_usage(work, Ok(commit))).unwrap();
        assert_eq!(reply["result"]["durable"], true);
    }
    assert_eq!(service.external_usage_projection_counts_for_tests(), (2, 0));
    let generic_body = if canonical {
        let mut body = params.clone();
        body["epoch"] =
            crate::storage::token_usage::external_observation_epoch(pair0.observation())
                .unwrap()
                .into();
        body["event_id"] = "generic-1".into();
        Some(body)
    } else {
        None
    };
    let generic = generic_body.as_ref().map(|body| {
        let request = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"generic","method":"agent/external/usage","params":body}).to_string()).unwrap();
        let work = service.prepare_external_usage(&request, &connection).unwrap();
        let commit = work.execute().unwrap();
        (work, commit)
    });
    service.set_external_usage_stream_limit_for_tests(2);
    let delayed = service
        .prepare_external_usage(&request0, &connection)
        .unwrap();
    let delayed_commit = delayed.execute().unwrap();
    service.reset_agent_token_usage_for_pane("%1");
    let older = service
        .prepare_external_opencode_handoff(&request0, &connection, pair0.clone())
        .unwrap();
    let newer = service
        .prepare_external_opencode_handoff(&request1, &connection, pair1)
        .unwrap();
    let older_commit = older.execute().unwrap();
    let newer_commit = newer.execute().unwrap();
    for (work, commit) in [
        (newer, newer_commit),
        (older, older_commit),
        (delayed, delayed_commit),
    ] {
        let reply: serde_json::Value =
            serde_json::from_str(&service.complete_external_usage(work, Ok(commit))).unwrap();
        assert_eq!(reply["result"]["durable"], true);
        assert_eq!(service.external_usage_projection_counts_for_tests(), (1, 2));
        assert_eq!(
            service
                .external_token_usage(Some("%1"))
                .values()
                .next()
                .unwrap()
                .input_tokens,
            0
        );
    }
    service.set_external_usage_stream_limit_for_tests(1);
    if let Some((work, commit)) = generic {
        let reply: serde_json::Value =
            serde_json::from_str(&service.complete_external_usage(work, Ok(commit))).unwrap();
        assert_eq!(reply["result"]["durable"], true);
        assert_eq!(
            service
                .external_token_usage(Some("%1"))
                .values()
                .next()
                .unwrap()
                .input_tokens,
            10,
            "delayed canonical checkpoint must be an additional member"
        );
        let mut body = generic_body.unwrap();
        body["sequence"] = 2.into();
        body["event_id"] = "generic-2".into();
        let request = crate::control::parse_json_rpc_request(&serde_json::json!({"jsonrpc":"2.0","id":"generic","method":"agent/external/usage","params":body}).to_string()).unwrap();
        let work = service
            .prepare_external_usage(&request, &connection)
            .unwrap();
        let commit = work.execute().unwrap();
        service.complete_external_usage(work, Ok(commit));
        assert_eq!(
            service
                .external_token_usage(Some("%1"))
                .values()
                .next()
                .unwrap()
                .input_tokens,
            20
        );
        service.reset_agent_token_usage_for_pane("%1");
    }
    for index in 2..count {
        if index % 16 == 2 {
            let renewal =
                serde_json::json!({"jsonrpc":"2.0","id":"renew","method":"agent/external/renew",
                "params":{"launch_token":params["launch_token"],"generation":params["generation"],
                    "external_session_id":params["external_session_id"]}})
                .to_string();
            let reply: serde_json::Value = serde_json::from_str(
                &service.dispatch_runtime_control_body_for_connection(&renewal, &mut connection),
            )
            .unwrap();
            assert!(
                reply.get("error").is_none(),
                "normal renewal must retain the fixture capability"
            );
        }
        let (request, pair) = make(index, "model");
        if index == 2 {
            assert_eq!(
                service
                    .prepare_external_usage(&request, &connection)
                    .unwrap_err()
                    .kind(),
                crate::error::MezErrorKind::RateLimited,
                "legacy per-message admission cannot spend a new slot"
            );
        }
        // Existing incurred expense, never a synthesized original request marker.
        store.ingest_external(pair.legacy(), now).unwrap();
        let work = service
            .prepare_external_opencode_handoff(&request, &connection, pair)
            .unwrap();
        let commit = work.execute().unwrap();
        let reply: serde_json::Value =
            serde_json::from_str(&service.complete_external_usage(work, Ok(commit))).unwrap();
        assert_eq!(reply["result"]["durable"], true);
        assert_eq!(
            service.external_usage_projection_counts_for_tests(),
            (1, index as usize + 1 + usize::from(canonical))
        );
    }
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .next()
            .unwrap()
            .input_tokens,
        (count - 2) * 10
    );
    assert_eq!(
        external_ledger_input(&store),
        (count as i64) * 10 + if canonical { 20 } else { 0 }
    );
    let (other, pair) = make(count, "other-model");
    assert_eq!(
        service
            .prepare_external_opencode_handoff(&other, &connection, pair)
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::RateLimited
    );
    assert_eq!(
        service.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert_eq!(
        external_ledger_input(&store),
        (count as i64) * 10 + if canonical { 20 } else { 0 }
    );
    service.reset_agent_token_usage_for_pane("%1");
    let work = service
        .prepare_external_opencode_handoff(&request0, &connection, pair0)
        .unwrap();
    let commit = work.execute().unwrap();
    service.complete_external_usage(work, Ok(commit));
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .next()
            .unwrap()
            .input_tokens,
        0
    );
    service.terminate_all_pane_processes().unwrap();
}

/// A typed receipt handoff may attach only to exact already-admitted legacy
/// work. Real legacy registration supplies pane/provenance authority; the source
/// mapping below is explicitly synthetic storage setup, not ordinary native
/// enrollment proof. Original reservations and reset baselines survive adoption,
/// unpublished caller commits reject, and actual worker replay never recharges.
#[test]
fn runtime_external_opencode_handoff_requires_exact_published_worker() {
    let (mut service, connection, mut params, store) = fixture();
    service.set_external_binding_harness_for_tests("opencode");
    let now = params["observed_at"].as_u64().unwrap();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "role":"assistant", "sessionID":"usage-run", "id":"message", "providerID":"provider", "modelID":"model",
        "time":{"completed":now*1000}, "tokens":{"input":7,"output":2,"reasoning":2,"cache":{"read":3,"write":0}}
    })).unwrap();
    let template = crate::integrations::bootstrap::opencode::completed_observation(
        "fixture",
        "usage-run",
        "template",
        None,
        &bytes,
    )
    .unwrap()
    .unwrap();
    params["epoch"] = template.legacy().epoch.clone().into();
    params["event_id"] = template.legacy().event_id.clone().into();
    params["counters"]["cache_write_input_tokens"] = 0.into();
    let request = crate::control::parse_json_rpc_request(
        &serde_json::json!({
            "jsonrpc":"2.0","id":"handoff","method":"agent/external/usage","params":params
        })
        .to_string(),
    )
    .unwrap();
    let mut work = service
        .prepare_external_usage(&request, &connection)
        .unwrap();
    let pair = crate::integrations::bootstrap::opencode::completed_observation(
        "fixture",
        "usage-run",
        &work.report.owner,
        work.report.project.clone(),
        &bytes,
    )
    .unwrap()
    .unwrap();
    let sql = rusqlite::Connection::open(store.path()).unwrap();
    sql.execute(
        "INSERT INTO external_usage_sources(witness,harness,owner) VALUES(?1,'opencode',?2)",
        rusqlite::params!["a".repeat(64), work.report.owner],
    )
    .unwrap();
    drop(sql);
    let legacy = work.execute().unwrap();
    let reservations = service.external_usage_reservation_counts_for_tests();
    assert!(
        work.set_opencode_handoff(template).is_err(),
        "wrong owner must retain original work"
    );
    assert_eq!(
        service.external_usage_reservation_counts_for_tests(),
        reservations
    );
    let mut mismatched = work.clone();
    mismatched.report.sequence += 1;
    assert!(mismatched.set_opencode_handoff(pair.clone()).is_err());
    let mut tampered = work.clone();
    tampered.set_opencode_handoff(pair.clone()).unwrap();
    tampered.report.model.model = "changed-after-attachment".into();
    assert!(
        tampered.execute().is_err(),
        "changed work cannot publish handoff evidence"
    );
    assert_eq!(external_ledger_input(&store), 10);
    work.set_opencode_handoff(pair.clone()).unwrap();
    assert!(work.set_opencode_handoff(pair.clone()).is_err());
    let rejected: serde_json::Value =
        serde_json::from_str(&service.complete_external_usage(work.clone(), Ok(legacy))).unwrap();
    assert!(
        rejected.get("error").is_some(),
        "unpublished handoff cannot claim durability"
    );
    assert_eq!(
        service.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    let mut work = service
        .prepare_external_usage(&request, &connection)
        .unwrap();
    work.set_opencode_handoff(pair.clone()).unwrap();
    let commit = work.execute().unwrap();
    assert!(!commit.applied);
    let group = work.published_group_for_tests().unwrap();
    assert_eq!(group.members(), std::slice::from_ref(&commit));
    assert_eq!(group.totals(), commit.totals);
    let accepted: serde_json::Value =
        serde_json::from_str(&service.complete_external_usage(work.clone(), Ok(commit.clone())))
            .unwrap();
    assert_eq!(accepted["result"]["durable"], true);
    assert!(
        serde_json::from_str::<serde_json::Value>(
            &service.complete_external_usage(work, Ok(commit))
        )
        .unwrap()
        .get("error")
        .is_some()
    );
    assert_eq!(external_ledger_input(&store), 10);
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .next()
            .unwrap()
            .input_tokens,
        10
    );
    service.reset_agent_token_usage_for_pane("%1");
    let mut replay = service
        .prepare_external_usage(&request, &connection)
        .unwrap();
    replay.set_opencode_handoff(pair).unwrap();
    let commit = replay.execute().unwrap();
    service.complete_external_usage(replay, Ok(commit));
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .next()
            .unwrap()
            .input_tokens,
        0
    );
    assert_eq!(external_ledger_input(&store), 10);
    assert_eq!(
        service.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    let mut changed = service
        .prepare_external_usage(&request, &connection)
        .unwrap();
    let pair = crate::integrations::bootstrap::opencode::completed_observation(
        "fixture",
        "usage-run",
        &changed.report.owner,
        changed.report.project.clone(),
        &bytes,
    )
    .unwrap()
    .unwrap();
    changed.set_opencode_handoff(pair).unwrap();
    let commit = changed.execute().unwrap();
    changed.report.owner = "changed-after-publication".into();
    changed.report.epoch = "changed-reservation-key".into();
    let rejected: serde_json::Value =
        serde_json::from_str(&service.complete_external_usage(changed, Ok(commit))).unwrap();
    assert!(rejected.get("error").is_some());
    assert_eq!(
        service.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert_eq!(external_ledger_input(&store), 10);
    service.terminate_all_pane_processes().unwrap();
}

/// Valid legacy capabilities retain only published delta/cumulative reporting.
/// Internal observation storage cannot reserve actor work before native request
/// boundary and attachment contracts are enabled, regardless of schema shape.
#[test]
fn runtime_external_usage_rejects_internal_observation_mode_before_reservation() {
    let (mut service, connection, mut params, store) = fixture();
    params["mode"] = serde_json::json!("observation");
    let request = crate::control::parse_json_rpc_request(
        &serde_json::json!({
            "jsonrpc":"2.0","id":"internal-mode","method":"agent/external/usage","params":params,
        })
        .to_string(),
    )
    .unwrap();
    let result = service.prepare_external_usage(&request, &connection);
    service.terminate_all_pane_processes().unwrap();
    let error = result.unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::NotImplemented);
    assert_eq!(
        service.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    assert_eq!(external_ledger_input(&store), 0);
}

/// A retired legacy binding cannot reserve new usage work. Reports admitted
/// before the binding changes still settle their frozen provenance once; the
/// admission guard must not erase incurred expense at completion.
#[test]
fn runtime_external_retired_gemini_rejects_legacy_binding_without_reservation() {
    let (mut service, connection, params, store) = fixture();
    let request = crate::control::parse_json_rpc_request(
        &serde_json::json!({"jsonrpc":"2.0","id":"usage","method":"agent/external/usage","params":params}).to_string(),
    ).unwrap();
    let work = service
        .prepare_external_usage(&request, &connection)
        .unwrap();
    let before = service.external_usage_reservation_counts_for_tests();
    service.set_external_binding_harness_for_tests("gemini");
    let error = service
        .prepare_external_usage(&request, &connection)
        .unwrap_err();
    assert!(error.message().contains("retired"));
    assert_eq!(
        service.external_usage_reservation_counts_for_tests(),
        before
    );
    assert_eq!(external_ledger_input(&store), 0);
    let commit = work.store.ingest_external(&work.report, work.now).unwrap();
    service.complete_external_usage(work, Ok(commit));
    assert_eq!(external_ledger_input(&store), 10);
    assert_eq!(
        service.external_usage_reservation_counts_for_tests(),
        (0, 0)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// External-only status remains observational and must not allocate a native
/// conversation merely to inspect usage or reset its pane-view baseline.
#[test]
fn runtime_harness_status_external_only_does_not_create_native_session() {
    let (mut service, connection, params, _) = fixture();
    let request = crate::control::parse_json_rpc_request(
        &serde_json::json!({
            "jsonrpc":"2.0","id":"usage","method":"agent/external/usage","params":params,
        })
        .to_string(),
    )
    .unwrap();
    let work = service
        .prepare_external_usage(&request, &connection)
        .unwrap();
    let commit = work.store.ingest_external(&work.report, work.now).unwrap();
    service.complete_external_usage(work, Ok(commit));
    let client = service.session().layout_owner_client_id().unwrap().clone();
    let response = service
        .execute_agent_shell_command(&client, "/status")
        .unwrap();
    assert!(response.contains("codex"), "{response}");
    assert!(service.agent_shell_store().get("%1").is_none());
    let terminal = service
        .execute_terminal_command(&client, "agent-status")
        .unwrap();
    let slash: serde_json::Value = serde_json::from_str(&response).unwrap();
    let terminal: serde_json::Value = serde_json::from_str(&terminal).unwrap();
    assert_eq!(terminal["outcomes"][0]["body"], slash["body"]);
    assert!(service.agent_shell_store().get("%1").is_none());
    let report = service
        .prepare_status_report(
            &client,
            "%1",
            mez_agent::slash::StatusOptions {
                scope: mez_agent::slash::StatusScope::Overall,
                extended: true,
            },
        )
        .unwrap();
    assert!(service.status_report_is_current(&report));
    assert!(report.render().unwrap().contains("1-Day Token Usage"));
    service
        .dispatch_deferred_agent_shell_command(&client, "%1", "status", "/status --extended")
        .unwrap();
    let body = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(body.contains("codex"), "{body}");
    assert!(service.agent_shell_store().get("%1").is_none());
    let reset = service
        .execute_agent_shell_command(&client, "/reset-status")
        .unwrap();
    assert!(!reset.contains("pane_token_usage_reset=true"), "{reset}");
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .next()
            .unwrap()
            .input_tokens,
        10
    );
    let reset = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(
        reset.contains("pane_token_usage_reset=true durable=true"),
        "{reset}"
    );
    assert!(service.agent_shell_store().get("%1").is_none());
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .next()
            .unwrap()
            .input_tokens,
        0
    );
    assert_eq!(
        service
            .external_token_usage(None)
            .values()
            .next()
            .unwrap()
            .input_tokens,
        10
    );
    let replay = service
        .prepare_external_usage(&request, &connection)
        .unwrap();
    let commit = replay
        .store
        .ingest_external(&replay.report, replay.now)
        .unwrap();
    service.complete_external_usage(replay, Ok(commit));
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .next()
            .unwrap()
            .input_tokens,
        0
    );
    assert!(service.agent_shell_store().get("%1").is_none());
    service.terminate_all_pane_processes().unwrap();
}

/// Overall status keeps same-model native/external expense separate, retains
/// unknown reasoning coverage, and includes both sources in durable windows.
#[test]
fn runtime_harness_status_separates_same_model_and_unknown_reasoning() {
    let (mut service, connection, mut params, _) = fixture();
    params["counters"]
        .as_object_mut()
        .unwrap()
        .remove("reasoning_tokens");
    let request = crate::control::parse_json_rpc_request(
        &serde_json::json!({
            "jsonrpc":"2.0","id":"usage","method":"agent/external/usage","params":params,
        })
        .to_string(),
    )
    .unwrap();
    let work = service
        .prepare_external_usage(&request, &connection)
        .unwrap();
    let result = work.store.ingest_external(&work.report, work.now).unwrap();
    service.complete_external_usage(work, Ok(result));
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let profile = runtime_model_profile("provider", "model");
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        mez_agent::ModelTokenUsage {
            input_tokens: 20,
            output_tokens: 8,
            reasoning_tokens: 2,
            ..Default::default()
        },
        Default::default(),
        Some(&profile),
    );
    let client = service.session().layout_owner_client_id().unwrap().clone();
    let report = service
        .prepare_status_report(
            &client,
            "%1",
            mez_agent::slash::StatusOptions {
                scope: mez_agent::slash::StatusScope::Overall,
                extended: true,
            },
        )
        .unwrap()
        .render()
        .unwrap();
    assert!(
        report.contains("| Harness | Provider | Model |"),
        "{report}"
    );
    assert!(
        report.contains("| codex | provider | model | 7 | 3 | 4 | unknown |"),
        "{report}"
    );
    assert!(
        report.contains("| mez | provider | model | 20 | unknown | 8 | 2 |"),
        "{report}"
    );
    assert!(
        report
            .split("### 1-Day Token Usage")
            .nth(1)
            .unwrap()
            .contains("| codex |"),
        "{report}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Reads only committed external deltas, independent of the native status reader.
fn external_ledger_input(store: &TokenUsageStore) -> i64 {
    rusqlite::Connection::open(store.path()).unwrap().query_row(
        "SELECT COALESCE(SUM(input_tokens),0) FROM token_usage_events WHERE event_source='external'",
        [], |row| row.get(0),
    ).unwrap()
}

/// Capacity limits new unique streams, not retries or updates of accepted ones.
/// Concurrent reservations for the same stream must share one projection slot.
#[test]
fn runtime_external_usage_capacity_preserves_existing_stream_admission() {
    let (mut service, connection, params, _) = fixture();
    service.set_external_usage_stream_limit_for_tests(1);
    let request = |params: &serde_json::Value| {
        crate::control::parse_json_rpc_request(
        &serde_json::json!({"jsonrpc":"2.0","id":"usage","method":"agent/external/usage","params":params}).to_string(),
    ).unwrap()
    };
    let first = service
        .prepare_external_usage(&request(&params), &connection)
        .unwrap();
    let duplicate = service
        .prepare_external_usage(&request(&params), &connection)
        .expect("same pending stream must not reserve another slot");
    let commit = first
        .store
        .ingest_external(&first.report, first.now)
        .unwrap();
    service.complete_external_usage(first, Ok(commit));
    let replay = duplicate
        .store
        .ingest_external(&duplicate.report, duplicate.now)
        .unwrap();
    service.complete_external_usage(duplicate, Ok(replay));
    let retry = service
        .prepare_external_usage(&request(&params), &connection)
        .expect("existing stream replay must be admitted at capacity");
    let mut next = params.clone();
    next["event_id"] = serde_json::json!("next");
    next["sequence"] = serde_json::json!(2);
    let update = service
        .prepare_external_usage(&request(&next), &connection)
        .expect("existing stream update must be admitted at capacity");
    let mut new_stream = params.clone();
    new_stream["epoch"] = serde_json::json!("new-epoch");
    assert!(
        service
            .prepare_external_usage(&request(&new_stream), &connection)
            .is_err()
    );
    for work in [retry, update] {
        let result = work.store.ingest_external(&work.report, work.now);
        service.complete_external_usage(work, result);
    }
    assert_eq!(
        service
            .external_token_usage(None)
            .values()
            .next()
            .unwrap()
            .input_tokens,
        20
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Creates a registered, capability-only telemetry owner and initialized private
/// usage store, matching host session startup before concurrent ingress begins.
/// Fresh-store and migration contention remain covered by storage-owned tests.
fn fixture() -> (
    RuntimeSessionService,
    ControlConnectionState,
    serde_json::Value,
    TokenUsageStore,
) {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let store = TokenUsageStore::new(temp_root("external-usage-ingress").join("usage.sqlite"));
    store.initialize(current_unix_seconds()).unwrap();
    service.set_token_usage_store(store.clone());
    let launch: serde_json::Value = serde_json::from_str(&service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"launch","method":"agent/external/launch","params":{"pane_id":"%1","harness":"codex","version":"fixture"}}"#, &primary,
    )).unwrap();
    let mut connection = ControlConnectionState::new(true, false);
    connection
        .bind_authenticated_peer(crate::control::AuthenticatedPeer::unix_user(
            crate::runtime::current_effective_uid(),
        ))
        .unwrap();
    let registration = serde_json::json!({"launch_token":launch["result"]["launch_token"],"generation":launch["result"]["generation"],
        "external_session_id":"usage-run","display_name":"Usage Fixture"});
    let response = service.dispatch_runtime_control_body_for_connection(&serde_json::json!({
        "jsonrpc":"2.0","id":"register","method":"agent/external/register","params":registration,
    }).to_string(), &mut connection);
    assert!(
        serde_json::from_str::<serde_json::Value>(&response)
            .unwrap()
            .get("result")
            .is_some()
    );
    let report = serde_json::json!({"launch_token":launch["result"]["launch_token"],"generation":launch["result"]["generation"],
        "external_session_id":"usage-run","epoch":"epoch-one","event_id":"first","sequence":1,"mode":"delta",
        "observed_at":current_unix_seconds(),"provider":"provider","model":"model",
        "counters":{"input_tokens":10,"output_tokens":4,"reasoning_tokens":2,"cached_input_tokens":3}});
    (service, connection, report, store)
}

/// Duplicate RPC replies recover an absolute checkpoint without double charging.
/// Changed counters under the same event ID fail after durable commit, while
/// the hook remains uninitialized and the actor still serves lifecycle requests.
/// Dropping the request waiter while storage is gated must not cancel accepted
/// accounting or hold the actor. Identical retry recovers the committed checkpoint.
#[tokio::test(flavor = "current_thread")]
async fn runtime_external_usage_worker_gate_preserves_actor_and_lost_reply() {
    let (mut service, connection, params, store) = fixture();
    assert_eq!(
        external_ledger_input(&store),
        0,
        "startup must not charge usage"
    );
    let started = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    service.gate_external_usage_worker_for_tests(started.clone(), release.clone());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let actor_task = tokio::spawn(actor.run());
    let input = crate::control::encode_control_body(
        &serde_json::json!({
            "jsonrpc":"2.0","id":"usage","method":"agent/external/usage","params":params,
        })
        .to_string(),
    );
    let first_handle = handle.clone();
    let first_input = input.clone();
    let first_connection = connection.clone();
    let first = tokio::spawn(async move {
        first_handle
            .handle_control_input_for_connection(first_input, 8192, first_connection)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), handle.lifecycle_state())
            .await
            .unwrap()
            .unwrap(),
        RuntimeLifecycleState::Running
    );
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    release.notify_one();
    let retry_handle = handle.clone();
    let retry = tokio::spawn(async move {
        retry_handle
            .handle_control_input_for_connection(input, 8192, connection)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    release.notify_one();
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), retry)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let (body, _) = crate::control::decode_control_frame(&response.output, 8192).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["result"]["durable"],
        true,
        "external usage retry response: {body}"
    );
    handle.shutdown().await.unwrap();
    let mut exit = actor_task.await.unwrap();
    assert_eq!(
        exit.service
            .external_token_usage(None)
            .values()
            .next()
            .unwrap()
            .input_tokens,
        10
    );
    assert_eq!(external_ledger_input(&store), 10);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Duplicate RPC replies recover an absolute checkpoint without double charging.
/// Changed counters under the same event ID fail after durable commit, while
/// the hook remains uninitialized and the actor still serves lifecycle requests.
#[tokio::test(flavor = "current_thread")]
async fn runtime_external_usage_acknowledges_durable_replay_without_double_charge() {
    let (service, connection, params, store) = fixture();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let client =
        async {
            for expected in [true, false] {
                let request = crate::control::encode_control_body(&serde_json::json!({
                "jsonrpc":"2.0","id":"usage","method":"agent/external/usage","params":params,
            }).to_string());
                let response = handle
                    .handle_control_input_for_connection(request, 8192, connection.clone())
                    .await
                    .unwrap();
                let (body, _) =
                    crate::control::decode_control_frame(&response.output, 8192).unwrap();
                let value: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(value["result"]["durable"], true, "{value}");
                assert_eq!(value["result"]["applied"], expected, "{value}");
                assert!(!response.connection.initialized());
            }
            let mut changed = params.clone();
            changed["counters"]["input_tokens"] = serde_json::json!(11);
            let request = crate::control::encode_control_body(
                &serde_json::json!({
                    "jsonrpc":"2.0","id":"changed","method":"agent/external/usage","params":changed,
                })
                .to_string(),
            );
            let response = handle
                .handle_control_input_for_connection(request, 8192, connection.clone())
                .await
                .unwrap();
            let (body, _) = crate::control::decode_control_frame(&response.output, 8192).unwrap();
            assert!(
                serde_json::from_str::<serde_json::Value>(&body)
                    .unwrap()
                    .get("error")
                    .is_some()
            );
            assert_eq!(
                handle.lifecycle_state().await.unwrap(),
                RuntimeLifecycleState::Running
            );
            handle.shutdown().await.unwrap();
        };
    let ((), mut exit) = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        tokio::join!(client, actor.run())
    })
    .await
    .unwrap();
    let totals = exit.service.external_token_usage(None);
    assert_eq!(totals.values().next().unwrap().input_tokens, 10);
    assert_eq!(external_ledger_input(&store), 10);
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Commit-before-projection loss is repaired by identical replay. Pane reset
/// affects only the view baseline, so a later report adds its new increment,
/// and out-of-order completion cannot overwrite a newer checkpoint.
#[test]
fn runtime_external_usage_checkpoint_recovery_and_reset_conserve_totals() {
    let (mut service, connection, params, _) = fixture();
    let request = |params: &serde_json::Value| {
        crate::control::parse_json_rpc_request(
            &serde_json::json!({
                "jsonrpc":"2.0","id":"usage","method":"agent/external/usage","params":params,
            })
            .to_string(),
        )
        .unwrap()
    };
    let work = service
        .prepare_external_usage(&request(&params), &connection)
        .unwrap();
    let first = work.store.ingest_external(&work.report, work.now).unwrap();
    let replay = work.store.ingest_external(&work.report, work.now).unwrap();
    service.complete_external_usage(work.clone(), Ok(replay));
    service.complete_external_usage(work, Ok(first));
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .next()
            .unwrap()
            .input_tokens,
        10
    );
    service.reset_agent_token_usage_for_pane("%1");
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .next()
            .unwrap()
            .input_tokens,
        0
    );
    let mut next = params.clone();
    next["event_id"] = serde_json::json!("next");
    next["sequence"] = serde_json::json!(2);
    let work = service
        .prepare_external_usage(&request(&next), &connection)
        .unwrap();
    let result = work.store.ingest_external(&work.report, work.now).unwrap();
    let mut third = next.clone();
    third["event_id"] = serde_json::json!("third");
    third["sequence"] = serde_json::json!(3);
    let later = service
        .prepare_external_usage(&request(&third), &connection)
        .unwrap();
    let later_result = later
        .store
        .ingest_external(&later.report, later.now)
        .unwrap();
    // Two distinct admissions/revisions complete in reverse order.
    service.complete_external_usage(later, Ok(later_result));
    service.complete_external_usage(work, Ok(result));
    assert_eq!(
        service
            .external_token_usage(None)
            .values()
            .next()
            .unwrap()
            .input_tokens,
        30
    );
    assert_eq!(
        service
            .external_token_usage(Some("%1"))
            .values()
            .next()
            .unwrap()
            .input_tokens,
        20
    );
    service.terminate_all_pane_processes().unwrap();
}
