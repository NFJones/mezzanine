//! Obsolete provider intents cannot consume an outstanding native execution.

use super::*;

/// Completion can win the race against retirement of an old provider offer.
/// Its checked queue transition must adopt the qualified continuation rather
/// than leave an unqualified pending bit suppressing the genuine handoff.
#[test]
fn runtime_native_provider_completion_supersedes_obsolete_unclaimed_intent() {
    let (mut service, _) = admit_siblings(Discovery::Absent, AgentLogLevel::Verbose, None);
    let agent = mez_core::ids::AgentId::opaque(
        service
            .agent_turn_ledger()
            .turn("turn-1")
            .unwrap()
            .agent_id
            .clone(),
    )
    .unwrap();
    for action in ["shell-a", "shell-b"] {
        let dispatch = service
            .claim_native_shell_action("turn-1", action)
            .unwrap()
            .unwrap();
        if action == "shell-b" {
            assert!(
                service
                    .queue_agent_provider_retry_task("turn-1", 1, Some("obsolete_offer_fixture"))
                    .unwrap()
            );
        }
        assert!(
            service
                .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(
                    dispatch
                ))
                .unwrap()
        );
    }
    let ready = service.ready_agent_provider_turn(&agent, "turn-1");
    let original = service.agent_turn_executions().get("turn-1").cloned();
    service.terminate_all_pane_processes().unwrap();
    assert!(
        ready.is_ok(),
        "qualified completion was suppressed by stale intent: {ready:?}"
    );
    assert!(ready.unwrap().is_some());
    assert_eq!(original.unwrap().response.raw_text, "native pair");
}

/// A mandatory completion audit refusal must still inhibit new dispatch, but
/// cannot turn an exact successful transcript ACK into a persistence failure.
/// The real writer and actor remain available for independent durable work.
#[tokio::test(flavor = "current_thread")]
async fn runtime_native_provider_admission_fence_keeps_persistence_ack_service_live() {
    use crate::host::async_runtime::{
        AsyncRuntimeActorConfig, AsyncRuntimeSessionActor, AsyncRuntimeSideEffectServiceConfig,
        run_async_persistence_side_effect_service,
    };
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};
    let root = std::env::temp_dir().join(format!(
        "mez-native-ack-fence-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    fs::create_dir(&root).unwrap();
    let (mut service, _) = admit_siblings(Discovery::Absent, AgentLogLevel::Verbose, None);
    let dispatch = service
        .claim_native_shell_action("turn-1", "shell-a")
        .unwrap()
        .unwrap();
    let outcome = crate::runtime::execute_native_shell_dispatch(dispatch);
    service.replace_config_layers(vec![ConfigLayer {
        name: "required-post-audit".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[hooks.post]\nevent=\"post_shell_command\"\nprogram=\"/missing/native-post-hook\"\n".into(),
    }]).unwrap();
    service.set_audit_log(crate::security::audit::AuditLog::new(
        crate::security::audit::AuditConfig {
            enabled: false,
            required: true,
            hash_chain: false,
            path: root.join("audit.jsonl"),
        },
    ));
    assert!(service.complete_native_shell_action(outcome).is_err());
    assert!(service.ensure_completion_admission_open().is_err());
    // Only the fixture's sink is made usable for teardown. The retained gate
    // must remain failed; this is not permission to rerun the entered hook.
    service.set_audit_log(crate::security::audit::AuditLog::new(
        crate::security::audit::AuditConfig {
            enabled: false,
            required: false,
            hash_chain: false,
            path: root.join("audit.jsonl"),
        },
    ));
    let store = crate::storage::transcript::AgentTranscriptStore::new(root.join("transcripts"));
    let rows = vec![TranscriptEntry {
        conversation_id: "independent-ack".into(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "independent-turn".into(),
        agent_id: "agent-independent".into(),
        pane_id: "%independent".into(),
        content: "retained once".into(),
    }];
    let batch =
        crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows.clone()).unwrap();
    let path = store.transcript_path("independent-ack").unwrap();
    service.queue_transcript_for_tests(
        crate::runtime::RuntimeSideEffect::PersistTranscriptEntries {
            store: store.clone(),
            path,
            batch,
        },
    );
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let client = async {
        let mut watcher = handle.side_effect_delivery_watcher();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while store.pending_append_receipts().unwrap().is_empty() {
                watcher.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 100,
                drain_limit: 8,
                idle_interval: std::time::Duration::from_millis(1),
            },
            |_, _| false,
        )
        .await;
        let live = handle.agent_turn_is_running("turn-1").await;
        let remaining = handle.drain_persistence_claims(8).await;
        let shutdown = handle.shutdown().await;
        (report, live, remaining, shutdown)
    };
    let ((report, live, remaining, shutdown), mut exit) = tokio::join!(client, actor.run());
    let still_fenced = exit.service.ensure_completion_admission_open().is_err();
    exit.service.terminate_all_pane_processes().unwrap();
    let actual = store.inspect("independent-ack").unwrap();
    drop((exit, handle, store));
    fs::remove_dir_all(&root).unwrap();
    assert!(!root.exists());
    assert!(
        report.is_ok(),
        "successful ACK killed persistence: {report:?}"
    );
    let report = report.unwrap();
    assert_eq!(report.completed, 1);
    assert_eq!(report.failed, 0);
    assert!(
        remaining.unwrap().is_empty(),
        "exact transcript claim must retire"
    );
    assert!(
        live.is_ok() && shutdown.is_ok(),
        "control must remain available"
    );
    assert!(still_fenced, "ACK cannot clear a required failure");
    assert_eq!(actual, rows);
}

/// Genuine failure ingress cannot synthesize a replacement batch over issued
/// native effects. Refusal conserves the original execution and context, with
/// no global admission poisoning or repeated reporting on duplicate delivery.
#[test]
fn runtime_native_provider_failure_preserves_issued_original_source() {
    assert_native_failure_source(false);
    assert_native_failure_source(true);
}

fn assert_native_failure_source(completion_application: bool) {
    let (mut service, _) = admit_siblings(Discovery::Absent, AgentLogLevel::Verbose, None);
    let original = service.agent_turn_executions()["turn-1"].clone();
    let context = service.agent_turn_contexts()["turn-1"].clone();
    let dispatch = service
        .claim_native_shell_action("turn-1", "shell-a")
        .unwrap()
        .unwrap();
    let cause =
        crate::error::MezError::invalid_state("offline provider failure with native custody");
    let result = if completion_application {
        let turn = service.agent_turn_ledger().turn("turn-1").unwrap().clone();
        let profile = runtime_model_profile("runtime-batch", "test");
        service.fail_agent_turn_after_provider_completion_application_error(
            &turn,
            "runtime-batch",
            Some(&profile),
            &cause,
        );
        Ok(())
    } else {
        service.fail_configured_agent_provider_task("turn-1", &cause)
    };
    let retained = service
        .provider_failure_source_for_tests("turn-1")
        .map(|(context, execution)| (context.clone(), execution.cloned()));
    let live = service.agent_turn_executions().get("turn-1").cloned();
    let global = service.ensure_completion_admission_open();
    let duplicate = service.fail_configured_agent_provider_task("turn-1", &cause);
    let outcome = crate::runtime::execute_native_shell_dispatch(dispatch);
    let key = (
        outcome.turn_id.clone(),
        outcome.action_id.clone(),
        outcome.marker.clone(),
    );
    let late = service.complete_native_shell_action(outcome.clone());
    let repeated = service.complete_native_shell_action(outcome);
    let raw_retained = service.retained_native_outcome_for_tests(&key).is_some();
    let next = service.claim_native_shell_action("turn-1", "shell-b");
    service.terminate_all_pane_processes().unwrap();
    assert!(result.is_ok(), "{result:?}");
    assert!(duplicate.is_ok(), "{duplicate:?}");
    assert!(
        late.unwrap(),
        "late actual outcome must enter original custody"
    );
    assert!(
        repeated.is_ok() && raw_retained,
        "duplicate cannot erase raw evidence"
    );
    assert!(
        !matches!(next, Ok(Some(_))),
        "refused owner cannot dispatch its sibling"
    );
    assert!(
        global.is_ok(),
        "a local provider refusal poisoned shared admission: {global:?}"
    );
    assert_eq!(live.unwrap().action_results, original.action_results);
    let (retained_context, retained_execution) = retained.expect("original refusal custody");
    assert_eq!(retained_context, context);
    assert_eq!(
        retained_execution.unwrap().action_results,
        original.action_results
    );
}

/// Exercise the actual actor Prepare/Claim entrypoint, not merely the completion
/// predicate. An obsolete unclaimed intent can exist independently of the
/// initiating bridge race; none of these phases grants provider authority.
#[tokio::test(flavor = "current_thread")]
async fn runtime_native_provider_obsolete_offer_is_held_by_exact_shell_owner() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};

    for phase in ["queued", "claimed", "publication-held"] {
        let (mut service, _) = admit_siblings(Discovery::Absent, AgentLogLevel::Verbose, None);
        let first = service
            .claim_native_shell_action("turn-1", "shell-a")
            .unwrap()
            .unwrap();
        assert!(
            service
                .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(first))
                .unwrap()
        );
        // The final sibling has no undispatched successor to accidentally
        // trigger the existing pending-dispatch guard in place of its owner.
        let dispatch = if phase != "queued" {
            Some(
                service
                    .claim_native_shell_action("turn-1", "shell-b")
                    .unwrap()
                    .unwrap(),
            )
        } else {
            None
        };
        if phase == "publication-held" {
            service.hold_agent_presentation_installs_for_tests(true);
            assert!(
                service
                    .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(
                        dispatch.unwrap()
                    ))
                    .unwrap()
            );
        }
        assert!(
            service
                .queue_agent_provider_retry_task("turn-1", 1, Some("obsolete_offer_fixture"))
                .unwrap()
        );
        let agent = mez_core::ids::AgentId::opaque(
            service
                .agent_turn_ledger()
                .turn("turn-1")
                .unwrap()
                .agent_id
                .clone(),
        )
        .unwrap();
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let client = async {
            let claim = handle
                .claim_configured_agent_provider_task(agent, "turn-1".into())
                .await;
            let shutdown = handle.shutdown().await;
            (claim, shutdown)
        };
        let ((claim, shutdown), mut exit) = tokio::join!(client, actor.run());
        // Clean up actual owned processes before any desired-behavior assertion,
        // including on the pre-fix failure path. No provider or command retries.
        let refused = exit.service.agent_provider_failure_is_refused("turn-1");
        let execution = exit.service.agent_turn_executions().get("turn-1").cloned();
        let pending = exit.service.agent_provider_task_is_pending("turn-1");
        exit.service.terminate_all_pane_processes().unwrap();
        assert!(
            claim.unwrap().is_none(),
            "{phase}: obsolete provider claim issued"
        );
        assert!(shutdown.is_ok(), "{phase}: {shutdown:?}");
        assert!(
            !refused,
            "{phase}: temporary native ownership became permanent failure"
        );
        let execution = execution.expect("original native execution must remain owned");
        assert_eq!(
            execution.response.raw_text, "native pair",
            "{phase}: original execution replaced"
        );
        assert_eq!(execution.terminal_state, AgentTurnState::Running, "{phase}");
        assert!(
            !pending,
            "{phase}: stale intent must not suppress the later legitimate continuation"
        );
    }
}
