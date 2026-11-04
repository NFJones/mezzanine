//! Actual process exit after t1/d1/r1 commit, then original factory recovery.
//!
//! Historical clocks enter through the real lower/root fixture boundary. Every
//! source, probe and journal is genuine; no snapshot terminal bits or decoded
//! frame supplies positive authority. No task/provider or raw bearer resumes.

use super::*;
const TERMINAL_CRASH_ROOT: &str = "MEZ_TEST_TERMINAL_CUSTODY_CRASH_ROOT";
const TERMINAL_CRASH_RETIRED: &str = "MEZ_TEST_TERMINAL_CUSTODY_CRASH_RETIRED";
const TERMINAL_CRASH_RELEASE: &str = "MEZ_TEST_TERMINAL_CUSTODY_CRASH_RELEASE";

/// First qualifies actual absence under the original transcript/store owner,
/// then commits expiry or horizon release through the real owned worker. Exit
/// occurs while the actual positive still awaits original lower adoption.
pub(super) async fn commit_then_crash(
    mut service: RuntimeSessionService,
    root: MmpCustodyRoot,
    fixture: Fixture,
    first: u64,
    release: bool,
    retired: bool,
) -> ! {
    if retired {
        assert!(
            service
                .message_service_mut()
                .retire_agent_identity(&fixture.native)
        );
        assert!(
            root.start_registration_end(service.message_service())
                .unwrap()
        );
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::RegistrationEndAwaitingAdoption
        );
        assert!(
            root.adopt_registration_end(service.message_service_mut())
                .unwrap()
        );
    }
    let source = service
        .message_service()
        .capture_native_input_lookup(2, 0)
        .unwrap();
    root.start_native_input(
        service.message_service(),
        source.clone(),
        service.agent_transcript_store().unwrap().clone(),
        None,
    )
    .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::NativeInputAwaitingAdoption
    );
    let observation = root.adopt_native_input(service.message_service()).unwrap();
    drop(source);
    let ticket = if retired {
        let mez_agent::messaging::MessageNativeInputObservation::RecipientEnded(witness) =
            observation
        else {
            panic!("actual ended original source required");
        };
        service
            .message_service_mut()
            .prepare_storage_native_retirement(root.storage_scope(), &witness, first, usize::MAX)
            .unwrap()
            .unwrap()
    } else {
        assert!(observation.into_commit().is_none());
        service
            .message_service_mut()
            .prepare_storage_expiry(root.storage_scope(), first, usize::MAX)
            .unwrap()
            .unwrap()
    };
    root.start_terminal(service.message_service_mut(), &ticket)
        .unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::TerminalAwaitingAdoption
    );
    if release {
        assert_eq!(
            root.adopt_terminal(service.message_service_mut()).unwrap(),
            1
        );
        drop(ticket);
        let ticket = service
            .message_service_mut()
            .prepare_storage_horizon_release(
                root.storage_scope(),
                crate::runtime::current_unix_millis(),
                usize::MAX,
            )
            .unwrap()
            .unwrap();
        root.start_terminal(service.message_service_mut(), &ticket)
            .unwrap();
        assert_eq!(
            root.observe().await.unwrap(),
            CustodyWorkerStatus::TerminalAwaitingAdoption
        );
    }
    assert!(service.message_service().has_unpublished_terminal());
    let db = rusqlite::Connection::open(
        root_path(&fixture.config, &fixture.payload.session_id).join("custody.sqlite"),
    )
    .unwrap();
    assert_eq!(count(&db, "t1"), i64::from(!retired));
    assert_eq!(count(&db, "d1"), i64::from(retired));
    assert_eq!(count(&db, "o1"), i64::from(retired));
    assert_eq!(count(&db, "r1"), i64::from(release));
    assert_eq!(count(&db, "k1"), 1);
    finish_crash(&fixture);
}

/// Exact child entry remains inert during ordinary test discovery. In its
/// private subprocess scope it exits inside actual original positive custody.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_custody_process_crash_child() {
    let Some(config) = std::env::var_os(TERMINAL_CRASH_ROOT) else {
        return;
    };
    let release = std::env::var_os(TERMINAL_CRASH_RELEASE).is_some();
    let original = if std::env::var_os(TERMINAL_CRASH_RETIRED).is_some() {
        Original::CrashRetirement(config.into(), release)
    } else {
        Original::CrashTerminal(config.into(), release)
    };
    let _ = Fixture::build(true, original).await;
    panic!("terminal child must exit before Rust cleanup");
}

/// t1 itself must survive abrupt process exit before adoption, independent of
/// manual message checkpoints. Actual factory startup preserves its outcome
/// and refuses native reinjection or raw owner/task/provider resurrection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_recovers_expiry_commit_after_process_crash_before_adoption() {
    recover(false, false).await;
}

/// r1 commit must survive the same abrupt exit without requiring live release
/// adoption or a fresh checkpoint. Original pending custody is settled without
/// a second native ACK or changing the old first terminal observation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_recovers_horizon_commit_after_process_crash_before_adoption() {
    recover(true, false).await;
}

/// Uses only the generated private row store's bounded internal namespaces.
fn count(db: &rusqlite::Connection, prefix: &str) -> i64 {
    db.query_row(
        "SELECT COUNT(*) FROM records WHERE key LIKE ?1",
        [format!("{prefix}:%")],
        |row| row.get(0),
    )
    .unwrap()
}
async fn recover(release: bool, retired: bool) {
    let directory = CrashDirectory::new();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "host::session::native_custody::tests::terminal::terminal_custody_process_crash_child",
            "--nocapture",
        ])
        .env(TERMINAL_CRASH_ROOT, &directory.0)
        .kill_on_drop(true);
    if release {
        child.env(TERMINAL_CRASH_RELEASE, "1");
    }
    if retired {
        child.env(TERMINAL_CRASH_RETIRED, "1");
    }
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
    let mut request = fixture.request();
    request.config.layers = vec![super::native_input::config()];
    let mut runtime = SessionFactory::create_with_native_custody(
        request,
        PaneProcessRetirementOwner::default(),
        None,
    )
    .await
    .unwrap();
    let turns = runtime.actor.as_ref().unwrap().native_turns_for_tests();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].state, mez_agent::AgentTurnState::Interrupted);
    let original_cursor = runtime
        .actor
        .as_ref()
        .unwrap()
        .native_subscription_for_tests(&fixture.native);
    let handle = runtime.handle().actor().clone();
    let runner = tokio::spawn(runtime.actor.take().unwrap().run());
    handle.lifecycle_state().await.unwrap();
    handle.shutdown().await.unwrap();
    let mut exited = runner.await.unwrap();
    let messages = exited.service.message_service();
    assert_eq!(
        messages.storage_usage().unwrap().unwrap().pending_messages,
        usize::from(!release)
    );
    assert!(
        messages.capture_native_input_lookup(2, 0).is_err(),
        "actual terminal slot cannot become new input"
    );
    assert!(messages.registered_identity(&fixture.raw).is_none());
    assert_eq!(
        messages.subscription(&fixture.native),
        original_cursor.as_ref(),
        "terminal release cannot advance a cumulative cursor from historical ACK bits"
    );
    assert_eq!(
        exited.service.agent_turn_ledger().turns(),
        turns.as_slice(),
        "terminal history cannot replace, resume or recreate the interrupted task"
    );
    assert!(exited.service.pending_agent_provider_tasks().is_empty());
    let recovered_epoch = exited
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .native_message_incarnation
        .snapshot_identity();
    if retired {
        assert_ne!(
            recovered_epoch, fixture.epoch,
            "ended namespace must not regain its old private native incarnation"
        );
    } else {
        assert_eq!(recovered_epoch, fixture.epoch);
    }
    assert!(!exited.service.has_pending_native_input());
    let db = rusqlite::Connection::open(
        root_path(&fixture.config, &fixture.payload.session_id).join("custody.sqlite"),
    )
    .unwrap();
    assert_eq!(count(&db, "t1"), i64::from(!retired));
    assert_eq!(count(&db, "d1"), i64::from(retired));
    assert_eq!(count(&db, "o1"), i64::from(retired));
    assert_eq!(count(&db, "r1"), i64::from(release));
    assert_eq!(count(&db, "k1"), 1);
    assert_eq!(count(&db, "p1"), 0);
    assert_eq!(count(&db, "c1"), 0);
    let store = crate::storage::mmp_custody::MmpCustodyStore::open_existing(
        &root_path(&fixture.config, &fixture.payload.session_id),
        &fixture.payload.session_id,
        Default::default(),
    )
    .unwrap();
    let history = store
        .fold_custody_history(mez_agent::messaging::MessageAcceptanceDecodeLimits {
            payload_bytes: 1024 * 1024,
            auxiliary_bytes: 512 * 1024,
            recipient_count: 4096,
            recipient_bytes: 1024 * 1024,
        })
        .unwrap();
    assert_eq!(history.terminal_observations.len(), 1);
    assert_eq!(
        history.terminal_observations[0].reason,
        if retired {
            mez_agent::messaging::DeliveryTerminalReason::RecipientRetired
        } else {
            mez_agent::messaging::DeliveryTerminalReason::Expired
        }
    );
    assert_eq!(history.terminal_observations[0].released(), release);
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM records WHERE key LIKE 'a1:%:commit'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    exited.service.terminate_all_pane_processes().unwrap();
    drop(runtime);
}

/// Actual process exit loses the live d1 positive before adoption. Registered
/// original-root factory recovery must retain its first reason/time, refuse the
/// ended native interval and preserve source custody without reinjection or ACK.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_recovers_retirement_commit_after_process_crash_before_adoption() {
    recover(false, true).await;
}

/// Abrupt exit after r1 loses its lower release adoption, yet factory recovery
/// must settle that exact retired original without reviving its native owner,
/// fabricating an ACK or changing its reason to expiry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_factory_recovers_retirement_horizon_after_process_crash_before_adoption() {
    recover(true, true).await;
}
