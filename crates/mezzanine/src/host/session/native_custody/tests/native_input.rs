//! Actual process crash after canonical native input and before its exact ACK.
//!
//! The child uses a genuine native user turn and the actual actor/root receive
//! path, then exits without Rust cleanup or a message checkpoint. The parent
//! recovers the original through the production factory and actor. Public IDs
//! and inert epoch bytes in the existing handoff are expectations, not tokens.

use super::*;
use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
use mez_agent::transcript::TranscriptContextEvent;

const NATIVE_CRASH_ROOT: &str = "MEZ_TEST_CANONICAL_NATIVE_CRASH_ROOT";
const NATIVE_PREPARATION_ROOT: &str = "MEZ_TEST_CANONICAL_NATIVE_PREPARATION_ROOT";
/// Child-only control: accept a real finite TTL before preparing canonical input.
pub(super) const EXPIRE_PREPARATION: &str = "MEZ_TEST_CANONICAL_EXPIRE_PREPARATION";
const NATIVE_PARTIAL_STAGE_ROOT: &str = "MEZ_TEST_CANONICAL_NATIVE_PARTIAL_STAGE_ROOT";
const NATIVE_COMPLETE_STAGE_ROOT: &str = "MEZ_TEST_CANONICAL_NATIVE_COMPLETE_STAGE_ROOT";

/// Actual file/journal boundary at which the child exits without destructors.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum NativeCrashPoint {
    Committed,
    Prepared,
    PartialStage,
    CompleteStage,
}

/// Identical local-only native harness configuration in both processes. No
/// provider is dispatched; the loopback endpoint cannot contact a paid service.
pub(super) fn config() -> ConfigLayer {
    ConfigLayer { name: "canonical-native-crash".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"native-crash\"\ndefault_model_profile=\"default\"\nshell_mode=\"native\"\n[providers.native-crash]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:9/v1\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.default]\nprovider=\"native-crash\"\nmodel=\"test\"\n[permissions]\napproval_policy=\"ask\"\nsandbox=\"policy-only\"\n".into() }
}

/// Creates real canonical task context before native source attribution and
/// guarded original registration are captured by the enclosing fixture.
pub(super) fn configure_original(service: &mut RuntimeSessionService) {
    service.replace_config_layers(vec![config()]).unwrap();
    service
        .session_mut_for_tests()
        .set_pane_live_state("%1", false)
        .unwrap();
    let primary = service
        .attach_primary("parent", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "original native task before abrupt process exit")
        .unwrap();
}

/// Drives only real owned root completions, stopping immediately after the
/// actual canonical input commit is adopted and before issuing a native ACK.
pub(super) async fn commit_then_crash(
    service: RuntimeSessionService,
    root: MmpCustodyRoot,
    fixture: Fixture,
    point: NativeCrashPoint,
) -> ! {
    use std::os::unix::fs::OpenOptionsExt;
    let (_, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    actor.register_message_custody(root).unwrap();
    let before_row = point != NativeCrashPoint::Committed;
    match point {
        NativeCrashPoint::Prepared => actor.fail_next_native_canonical_append_for_tests(),
        NativeCrashPoint::PartialStage => actor.fail_native_receipt_publication_for_tests(
            crate::storage::transcript::NativeReceiptPublicationFault::Partial,
        ),
        NativeCrashPoint::CompleteStage => actor.fail_native_receipt_publication_for_tests(
            crate::storage::transcript::NativeReceiptPublicationFault::Complete,
        ),
        NativeCrashPoint::Committed => (),
    }
    let mut canonical_observations = 0;
    let mut payload_observations = 0;
    for _ in 0..16 {
        actor.poll_message_custody_for_tests();
        let status = actor.observe_message_custody_for_tests().await.unwrap();
        if status == CustodyWorkerStatus::PayloadLoadAwaitingAdoption {
            payload_observations += 1;
        }
        if before_row && matches!(status, CustodyWorkerStatus::NativeInputRetryAt(_)) {
            break;
        }
        if status == CustodyWorkerStatus::NativeInputAwaitingAdoption {
            canonical_observations += 1;
            if canonical_observations == 2 {
                break;
            }
        }
    }
    assert_eq!(
        payload_observations, 1,
        "actual original cold source must use the owned reload"
    );
    assert_eq!(
        canonical_observations,
        if before_row { 1 } else { 2 },
        "real absence precedes owned canonical write or joined pre-row failure"
    );
    let database = rusqlite::Connection::open(
        root_path(&fixture.config, &fixture.payload.session_id).join("custody.sqlite"),
    )
    .unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT COUNT(*) FROM records WHERE key LIKE 'c1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        if before_row { 0 } else { 1 }
    );
    assert_eq!(
        database
            .query_row(
                "SELECT COUNT(*) FROM records WHERE key LIKE 'k1:%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1,
        "only earlier seq1 ACK exists before crash"
    );
    if point == NativeCrashPoint::Prepared {
        assert_eq!(
            database
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE key LIKE 'p1:%'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        let store = AgentTranscriptStore::under_config_root(&fixture.config);
        // The root still owns its actual mutation lease in this retry state.
        // Inspect produced bytes forensically; acquiring another lease here
        // would wait for the very child whose process exit we are exercising.
        let paths = fs::read_dir(store.root().join(".append-receipts"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(paths.len(), 1);
        let pending: serde_json::Value =
            serde_json::from_slice(&fs::read(&paths[0]).unwrap()).unwrap();
        let rows = pending["rows"].as_array().unwrap();
        assert!(
            rows.len() > 1,
            "actual preceding chronology belongs to the receipt"
        );
        assert!(
            rows.last()
                .unwrap()
                .as_str()
                .unwrap()
                .contains("native_peer_message")
        );
    }
    if matches!(
        point,
        NativeCrashPoint::PartialStage | NativeCrashPoint::CompleteStage
    ) {
        assert_eq!(
            database
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE key LIKE 'p1:%'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        let store = AgentTranscriptStore::under_config_root(&fixture.config);
        let paths = fs::read_dir(store.root().join(".append-receipts"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(paths.len(), 1);
        assert!(paths[0].to_str().unwrap().ends_with(".json.tmp"));
        let bytes = fs::read(&paths[0]).unwrap();
        let frame = serde_json::from_slice::<serde_json::Value>(&bytes);
        if point == NativeCrashPoint::PartialStage {
            assert!(frame.unwrap_err().is_eof());
        } else {
            assert!(frame.unwrap()["rows"].as_array().unwrap().len() > 1);
        }
    }
    SnapshotRepository::new(fixture.config.clone())
        .write_payload("crash-layout", &fixture.payload)
        .unwrap();
    let mut handoff = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(fixture.config.join("crash-expectations.json"))
        .unwrap();
    serde_json::to_writer(
        &mut handoff,
        &CrashExpectations {
            epoch: fixture.epoch,
            native: fixture.native.to_string(),
            raw: fixture.raw.to_string(),
        },
    )
    .unwrap();
    handoff.sync_all().unwrap();
    fs::File::open(&fixture.config).unwrap().sync_all().unwrap();
    // No actor, source, worker, root, store or Fixture destructor may run here.
    std::process::exit(0);
}

/// Exact-filter child entry is inert during ordinary harness discovery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_native_process_crash_child() {
    let Some(root) = std::env::var_os(NATIVE_CRASH_ROOT) else {
        return;
    };
    let _ = Fixture::build(true, Original::CrashNativeInput(root.into())).await;
    panic!("canonical native child must exit before cleanup");
}

/// Real process loss after canonical receive but before ACK must recover the
/// original epoch/c1 and settle transport once without reinjecting context,
/// creating a new task, resuming a provider or reconstructing a raw bearer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_crash_after_canonical_commit_dedups_before_actor_ack() {
    assert_native_crash_recovery(NativeCrashPoint::Committed, false).await;
}

/// Kernel/process loss after original p1 but before any native row byte must
/// finish the exact retained canonical receipt, without a new receive task or
/// an envelope-cache load. No original actor/source/root destructor runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_recovers_prepared_missing_row_without_reinjection_or_task() {
    assert_native_crash_recovery(NativeCrashPoint::Prepared, false).await;
}

/// The test-only append failure sits at the real write boundary; this child
/// exits with actual joined p1/receipt ownership instead of inventing a status.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_native_preparation_crash_child() {
    let Some(root) = std::env::var_os(NATIVE_PREPARATION_ROOT) else {
        return;
    };
    let _ = Fixture::build(true, Original::CrashNativePreparation(root.into())).await;
    panic!("preparation child must exit before cleanup");
}

/// Executes the actual subprocess/factory/actor pair at either side of the
/// canonical append while retaining the same original scope and absence checks.
async fn assert_native_crash_recovery(point: NativeCrashPoint, expire_preparation: bool) {
    let before_row = point != NativeCrashPoint::Committed;
    let partial = point == NativeCrashPoint::PartialStage;
    let directory = CrashDirectory::new();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    let (entry, variable) = match point {
        NativeCrashPoint::Prepared => (
            "host::session::native_custody::tests::native_input::canonical_native_preparation_crash_child",
            NATIVE_PREPARATION_ROOT,
        ),
        NativeCrashPoint::Committed => (
            "host::session::native_custody::tests::native_input::canonical_native_process_crash_child",
            NATIVE_CRASH_ROOT,
        ),
        NativeCrashPoint::PartialStage => (
            "host::session::native_custody::tests::native_input::canonical_native_partial_stage_crash_child",
            NATIVE_PARTIAL_STAGE_ROOT,
        ),
        NativeCrashPoint::CompleteStage => (
            "host::session::native_custody::tests::native_input::canonical_native_complete_stage_crash_child",
            NATIVE_COMPLETE_STAGE_ROOT,
        ),
    };
    if expire_preparation {
        assert!(point == NativeCrashPoint::Prepared);
        child.env(EXPIRE_PREPARATION, "1");
    }
    child
        .args(["--exact", entry, "--nocapture"])
        .env(variable, &directory.0)
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(45), child.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if expire_preparation {
        // This file schedules the fixture wait only; it supplies no disposition
        // authority. The factory/actor must use the actual accepted TTL/history.
        let deadline: u64 =
            serde_json::from_slice(&fs::read(directory.0.join("expiry-deadline.json")).unwrap())
                .unwrap();
        let wait = deadline
            .saturating_sub(crate::runtime::current_unix_millis())
            .saturating_add(1);
        assert!(wait <= 3001);
        tokio::time::sleep(Duration::from_millis(wait)).await;
        assert!(crate::runtime::current_unix_millis() > deadline);
    }
    let expected: CrashExpectations =
        serde_json::from_slice(&fs::read(directory.0.join("crash-expectations.json")).unwrap())
            .unwrap();
    let fixture = Fixture {
        config: directory.0.clone(),
        payload: SnapshotRepository::new(directory.0.clone())
            .inspect_payload("crash-layout")
            .unwrap(),
        epoch: expected.epoch,
        native: AgentId::opaque(expected.native).unwrap(),
        raw: AgentId::opaque(expected.raw).unwrap(),
    };
    assert!(fixture.payload.message_state.is_none());
    let transcripts = AgentTranscriptStore::under_config_root(&fixture.config);
    let (_, contexts) = transcripts
        .capture_native_session_metadata(&fixture.payload.session_id)
        .unwrap();
    let cid = contexts
        .iter()
        .find(|c| c.pane_id == "%1")
        .unwrap()
        .conversation_id
        .clone();
    let count = |rows: Vec<TranscriptEntry>| {
        rows.into_iter()
            .filter(|r| {
                matches!(
                    TranscriptContextEvent::from_transcript_content(&r.content),
                    Some(TranscriptContextEvent::NativePeerMessage { .. })
                )
            })
            .count()
    };
    assert_eq!(
        count(transcripts.inspect(&cid).unwrap()),
        usize::from(!before_row)
    );
    let mut request = fixture.request();
    request.config.layers = vec![config()];
    let mut runtime = SessionFactory::create_with_native_custody(
        request,
        PaneProcessRetirementOwner::default(),
        None,
    )
    .await
    .unwrap();
    let handle = runtime.handle().actor().clone();
    let restored_turns = runtime.actor.as_ref().unwrap().native_turns_for_tests();
    assert_eq!(restored_turns.len(), 1);
    assert_eq!(
        restored_turns[0].state,
        mez_agent::AgentTurnState::Interrupted
    );
    assert_eq!(
        restored_turns[0].trigger,
        mez_agent::AgentTurnTrigger::ScheduledTask
    );
    let restored_subscription = runtime
        .actor
        .as_ref()
        .unwrap()
        .native_subscription_for_tests(&fixture.native);
    let runner = tokio::spawn(runtime.actor.take().unwrap().run());
    let path = root_path(&fixture.config, &fixture.payload.session_id).join("custody.sqlite");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            handle.lifecycle_state().await.unwrap();
            let db = rusqlite::Connection::open(&path).unwrap();
            if db
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE key LIKE 'k1:%'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                == 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    handle.shutdown().await.unwrap();
    let mut exited = runner.await.unwrap();
    if expire_preparation {
        let db = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM records WHERE key LIKE 't1:%'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0,
            "actual original p1 must finish canonical input and ACK before expiry selection"
        );
    }
    let settled_subscription = exited
        .service
        .message_service()
        .subscription(&fixture.native);
    assert_eq!(
        settled_subscription.is_some(),
        restored_subscription.is_some(),
        "historical lookup must not create a subscription absent at qualified startup"
    );
    if let Some(cursor) = settled_subscription {
        assert_eq!(cursor.last_sequence, 2);
    }
    assert_eq!(
        exited
            .service
            .message_service()
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        0,
        "both original slots settled through real ACK journals"
    );
    assert!(
        exited
            .service
            .message_service()
            .registered_identity(&fixture.raw)
            .is_none()
    );
    assert_eq!(
        exited
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .native_message_incarnation
            .snapshot_identity(),
        fixture.epoch
    );
    if partial {
        let turns = exited.service.agent_turn_ledger().turns();
        for original in &restored_turns {
            assert_eq!(
                turns.iter().find(|t| t.turn_id == original.turn_id),
                Some(original)
            );
        }
        let fresh = turns
            .iter()
            .filter(|t| !restored_turns.iter().any(|old| old.turn_id == t.turn_id))
            .collect::<Vec<_>>();
        assert_eq!(
            fresh.len(),
            1,
            "unpublished partial candidate must permit one genuine fresh receive"
        );
        assert_eq!(fresh[0].trigger, mez_agent::AgentTurnTrigger::LocalMessage);
        assert!(
            exited
                .service
                .pending_agent_provider_tasks()
                .iter()
                .all(|task| !restored_turns.iter().any(|old| old.turn_id == task.turn_id)),
            "no interrupted task/provider restart"
        );
    } else {
        assert_eq!(
            exited.service.agent_turn_ledger().turns(),
            restored_turns,
            "complete canonical recovery cannot create or resume a task"
        );
        assert!(exited.service.pending_agent_provider_tasks().is_empty());
    }
    assert_eq!(
        exited
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_high_water,
        transcripts.inspect(&cid).unwrap().last().unwrap().sequence
    );
    assert_eq!(
        exited
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .transcript_entries,
        transcripts.inspect(&cid).unwrap().len() as u64
    );
    assert!(
        transcripts.pending_append_receipts().unwrap().is_empty(),
        "only actual c1 may retire the canonical row image"
    );
    assert!(
        fs::read_dir(transcripts.root().join(".append-receipts"))
            .unwrap()
            .all(|entry| !entry.unwrap().path().to_string_lossy().ends_with(".tmp")),
        "no unresolved staging remains after actual settlement"
    );
    if !partial {
        assert_eq!(
            exited
                .service
                .message_service()
                .storage_usage()
                .unwrap()
                .unwrap()
                .cache,
            mez_agent::messaging::MessagePayloadUsage::default(),
            "historical c1 settlement must not hydrate payload into the cache"
        );
    }
    assert_eq!(
        count(transcripts.inspect(&cid).unwrap()),
        1,
        "no canonical reinjection across the actual crash"
    );
    exited.service.terminate_all_pane_processes().unwrap();
    drop(runtime);
}

/// Optional discovery labels do not qualify original native continuity. Facts
/// read from the actual guarded original match the private current context even
/// with no capabilities, while external harnesses and changed original epochs
/// remain excluded. These observations alone never install a recovered token.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_private_match_accepts_empty_capabilities_and_rejects_external_or_changed_epoch() {
    let fixture = Fixture::new(true).await;
    let mut runtime = fixture.runtime();
    let context = runtime.prepare_native_custody_context().unwrap();
    let root = root_path(&fixture.config, &fixture.payload.session_id);
    let owner = RootOwnership::acquire_existing(&root).unwrap();
    let mut store = crate::storage::mmp_custody::MmpCustodyStore::open_existing(
        &root,
        &fixture.payload.session_id,
        Default::default(),
    )
    .unwrap();
    let mut bound = owner.bind_store(&mut store).unwrap();
    let mut fact = bound
        .store_mut()
        .read_native_continuity(fixture.native.as_str())
        .unwrap()
        .unwrap()
        .snapshot;
    fact.identity.capabilities.clear();
    assert!(context.matches(&fact));
    fact.identity.capabilities.push("external-harness".into());
    assert!(!context.matches(&fact));
    fact.identity.capabilities.clear();
    fact.native.shell_incarnation[0] ^= 1;
    assert!(!context.matches(&fact));
}

/// Real process loss with a complete synced stage but no final receipt/p1 must
/// finish its original canonical image without task/provider reconstruction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_recovers_complete_unpublished_stage_without_new_task() {
    assert_native_crash_recovery(NativeCrashPoint::CompleteStage, false).await;
}

/// An incomplete unpublished candidate has committed no canonical input. The
/// original root preserves the accepted envelope and permits one fresh receive,
/// while every interrupted original task remains historical and unresumed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_aborts_partial_unpublished_stage_before_fresh_receive() {
    assert_native_crash_recovery(NativeCrashPoint::PartialStage, false).await;
}

/// The exact-filter child exits only after actual partial staged bytes remain
/// with a joined original-root failure, without any p1/c1 or Rust cleanup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_native_partial_stage_crash_child() {
    let Some(root) = std::env::var_os(NATIVE_PARTIAL_STAGE_ROOT) else {
        return;
    };
    let _ = Fixture::build(true, Original::CrashNativeStage(root.into(), true)).await;
    panic!("partial-stage child must exit before cleanup");
}

/// Complete staged row bytes and original physical plan survive child exit;
/// neither the final receipt name nor MMP preparation has yet been published.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_native_complete_stage_crash_child() {
    let Some(root) = std::env::var_os(NATIVE_COMPLETE_STAGE_ROOT) else {
        return;
    };
    let _ = Fixture::build(true, Original::CrashNativeStage(root.into(), false)).await;
    panic!("complete-stage child must exit before cleanup");
}

/// The child exits with a real p1/canonical receipt and a finite accepted TTL.
/// Restart begins only after that original TTL expired. The actual factory and
/// actor must finish canonical recovery/ACK instead of terminalizing the slot,
/// without reloading/reinjecting input or creating task/provider continuation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_recovers_prepared_input_before_expiry_after_process_crash() {
    assert_native_crash_recovery(NativeCrashPoint::Prepared, true).await;
}
