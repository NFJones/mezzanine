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
) -> ! {
    use std::os::unix::fs::OpenOptionsExt;
    let (_, mut actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    actor.register_message_custody(root).unwrap();
    let mut canonical_observations = 0;
    let mut payload_observations = 0;
    for _ in 0..16 {
        actor.poll_message_custody_for_tests();
        let status = actor.observe_message_custody_for_tests().await.unwrap();
        if status == CustodyWorkerStatus::PayloadLoadAwaitingAdoption {
            payload_observations += 1;
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
        canonical_observations, 2,
        "real absence then real canonical write must finish"
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
        1
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
    let directory = CrashDirectory::new();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child.args(["--exact", "host::session::native_custody::tests::native_input::canonical_native_process_crash_child", "--nocapture"])
        .env(NATIVE_CRASH_ROOT, &directory.0).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(45), child.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
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
    assert_eq!(count(transcripts.inspect(&cid).unwrap()), 1);
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
    assert_eq!(
        exited.service.agent_turn_ledger().turns(),
        restored_turns,
        "canonical recovery must preserve interrupted history without creating or resuming a task"
    );
    assert!(exited.service.pending_agent_provider_tasks().is_empty());
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
