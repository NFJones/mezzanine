//! Actual private SQLite restart and clock-progress regression controls.

use super::*;

#[derive(Debug)]
struct ManualClock {
    boot: &'static str,
    now: AtomicU64,
}
impl ManualClock {
    fn new(boot: &'static str, now: u64) -> Arc<Self> {
        Arc::new(Self {
            boot,
            now: AtomicU64::new(now),
        })
    }
    fn advance(&self, now: u64) {
        self.now.store(now, Ordering::Release);
    }
}
impl NativeClock for ManualClock {
    fn boot_uuid(&self) -> &str {
        self.boot
    }
    fn millis(&self) -> Result<u64> {
        let now = self.now.load(Ordering::Acquire);
        (now != 0).then_some(now).ok_or_else(changed)
    }
    fn invalidate_for_tests(&self) {
        self.advance(0);
    }
}
const BOOT: &str = "01234567-89ab-cdef-0123-456789abcdef";
const NEXT_BOOT: &str = "11234567-89ab-cdef-0123-456789abcdef";

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "mez-mmp-elapsed-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        )))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Reopen preserves the same actual boot epoch, including time outside the
/// process. A second boot retains committed elapsed progress and credits its
/// own uptime, without resetting the original interval or inventing UTC age.
#[test]
fn custody_clock_same_boot_restart_and_reboot_keep_committed_elapsed() {
    let fixture = Fixture::new();
    let native = ManualClock::new(BOOT, 100);
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let clock = store.qualify_elapsed_clock(native.clone()).unwrap();
    assert_eq!(clock.read().unwrap(), 101);
    native.advance(200);
    store
        .commit(
            0,
            &[CustodyWrite {
                key: "original",
                previous: None,
                value: Some(b"frozen"),
            }],
        )
        .unwrap();
    assert_eq!(store.current_state().unwrap().clock.unwrap().progress, 201);
    native.advance(900);
    assert_eq!(clock.read().unwrap(), 901);
    drop(store);
    drop(clock);
    native.advance(1000);
    let mut store =
        MmpCustodyStore::open_existing(&fixture.0, "session", Default::default()).unwrap();
    let clock = store.qualify_elapsed_clock(native.clone()).unwrap();
    assert_eq!(clock.read().unwrap(), 1001);
    store
        .commit(
            1,
            &[CustodyWrite {
                key: "next",
                previous: None,
                value: Some(b"kept"),
            }],
        )
        .unwrap();
    drop(store);
    drop(clock);
    let mut store =
        MmpCustodyStore::open_existing(&fixture.0, "session", Default::default()).unwrap();
    let clock = store
        .qualify_elapsed_clock(ManualClock::new(NEXT_BOOT, 25))
        .unwrap();
    assert_eq!(clock.read().unwrap(), 1026);
    assert_eq!(store.read("original").unwrap().unwrap(), b"frozen");
    assert_eq!(store.current_state().unwrap().generation, 2);
}

/// Losing a reply cannot advance the committed clock by replaying the same
/// batch. Last-good native observations still let already-issued originals
/// settle after read failure; they cannot be used for a fresh time decision.
#[test]
fn custody_clock_lost_reply_is_exact_and_native_failure_keeps_settlement() {
    let fixture = Fixture::new();
    let native = ManualClock::new(BOOT, 100);
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    let clock = store.qualify_elapsed_clock(native.clone()).unwrap();
    let writes = [CustodyWrite {
        key: "original",
        previous: None,
        value: Some(b"frozen"),
    }];
    let first = store.commit(0, &writes).unwrap();
    native.advance(200);
    assert_eq!(store.commit(0, &writes).unwrap(), first);
    assert_eq!(store.current_state().unwrap().clock.unwrap().progress, 101);
    assert_eq!(clock.read().unwrap(), 201);
    native.advance(0);
    assert!(clock.read().is_err());
    store
        .commit(
            1,
            &[CustodyWrite {
                key: "settled",
                previous: None,
                value: Some(b"original terminal"),
            }],
        )
        .unwrap();
    assert_eq!(store.current_state().unwrap().clock.unwrap().progress, 201);
    assert!(clock.read().is_err());
}

/// New-boot clock installation is a single transaction that cannot alter
/// existing custody or allocator data through a trigger, even with valid SQL.
#[test]
fn custody_clock_qualification_preserves_unrelated_rows_and_state() {
    for trigger in [
        "CREATE TRIGGER damage_clock AFTER UPDATE OF elapsed_clock ON state BEGIN UPDATE records SET value=X'00' WHERE key='original'; END;",
        "CREATE TRIGGER damage_clock AFTER UPDATE OF elapsed_clock ON state BEGIN UPDATE state SET sequence_watermark=X'0100000000000000' WHERE singleton=1; END;",
    ] {
        let fixture = Fixture::new();
        let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
        store
            .qualify_elapsed_clock(ManualClock::new(BOOT, 100))
            .unwrap();
        store
            .commit(
                0,
                &[CustodyWrite {
                    key: "original",
                    previous: None,
                    value: Some(b"frozen"),
                }],
            )
            .unwrap();
        drop(store);
        let mut store =
            MmpCustodyStore::open_existing(&fixture.0, "session", Default::default()).unwrap();
        store.connection.execute_batch(trigger).unwrap();
        assert!(
            store
                .qualify_elapsed_clock(ManualClock::new(NEXT_BOOT, 10))
                .is_err()
        );
        assert_eq!(store.read("original").unwrap().unwrap(), b"frozen");
        let state = store.current_state().unwrap();
        assert_eq!(state.generation, 1);
        assert_eq!(state.watermark, 0);
        assert_eq!(state.clock.unwrap().progress, 101);
    }
}

/// An after-state trigger cannot substitute a clock blob in an otherwise exact
/// custody write. The entire transaction rolls back, keeping its old progress.
#[test]
fn custody_clock_write_detects_tampered_final_progress() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    store
        .qualify_elapsed_clock(ManualClock::new(BOOT, 100))
        .unwrap();
    store.connection.execute_batch("CREATE TRIGGER damage_clock AFTER UPDATE ON state BEGIN UPDATE state SET elapsed_clock=zeroblob(116) WHERE singleton=1; END;").unwrap();
    assert!(
        store
            .commit(
                0,
                &[CustodyWrite {
                    key: "original",
                    previous: None,
                    value: Some(b"frozen")
                }]
            )
            .is_err()
    );
    assert!(store.read("original").unwrap().is_none());
    assert_eq!(store.current_state().unwrap().clock.unwrap().progress, 101);
}

/// Additive v2 migration preserves every retained byte and records unknown
/// legacy timing. A nonempty legacy store cannot silently start a new TTL epoch.
#[test]
fn custody_clock_v2_migration_preserves_legacy_without_inventing_elapsed() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    store
        .commit(
            0,
            &[CustodyWrite {
                key: "legacy",
                previous: None,
                value: Some(b"original"),
            }],
        )
        .unwrap();
    store
        .connection
        .execute_batch("ALTER TABLE state DROP COLUMN elapsed_clock; PRAGMA user_version=2;")
        .unwrap();
    drop(store);
    let mut store =
        MmpCustodyStore::open_existing(&fixture.0, "session", Default::default()).unwrap();
    assert_eq!(store.read("legacy").unwrap().unwrap(), b"original");
    assert!(store.current_state().unwrap().clock.is_none());
    assert!(
        store
            .qualify_elapsed_clock(ManualClock::new(BOOT, 100))
            .is_err()
    );
    assert_eq!(store.current_state().unwrap().generation, 1);
}

/// Clock bytes are bounded and allocation qualified. Malformed checksum/boot,
/// changed allocation or a backward same-boot native sample never yield time.
#[test]
fn custody_clock_corruption_foreign_allocation_and_rollback_refuse() {
    let fixture = Fixture::new();
    let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
    store
        .qualify_elapsed_clock(ManualClock::new(BOOT, 100))
        .unwrap();
    let clock = store.current_state().unwrap().clock.unwrap();
    let bytes = clock.encode(&store.store_id).unwrap();
    assert!(ClockState::decode(&bytes, &[3; 16]).is_err());
    for length in [0, 115, 117, 4096] {
        assert!(ClockState::decode(&vec![0; length], &store.store_id).is_err());
    }
    let mut bad = bytes;
    bad[76] ^= 1;
    assert!(ClockState::decode(&bad, &store.store_id).is_err());
    drop(store);
    let mut store =
        MmpCustodyStore::open_existing(&fixture.0, "session", Default::default()).unwrap();
    assert!(
        store
            .qualify_elapsed_clock(ManualClock::new(BOOT, 99))
            .is_err()
    );
    assert!(store.elapsed_clock.is_none());
    assert_eq!(store.current_state().unwrap().clock.unwrap().progress, 101);
}

/// The real generic transaction is interrupted before or after its actual
/// commit in a separate process. Clock progress and custody must recover as
/// one unit, without relying on destructors or a clean SQLite connection close.
#[test]
fn custody_clock_process_crash_keeps_clock_and_custody_atomic() {
    for mode in ["before", "after"] {
        let fixture = Fixture::new();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "storage::mmp_custody::elapsed_clock::tests::custody_clock_process_child",
                "--ignored",
                "--quiet",
            ])
            .env("MEZ_TEST_CUSTODY_CLOCK_PATH", &fixture.0)
            .env("MEZ_TEST_CUSTODY_CLOCK_CRASH", mode)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let mut store =
            MmpCustodyStore::open_existing(&fixture.0, "session", Default::default()).unwrap();
        let state = store.current_state().unwrap();
        assert_eq!(state.generation, if mode == "before" { 0 } else { 1 });
        assert_eq!(
            state.clock.unwrap().progress,
            if mode == "before" { 101 } else { 201 }
        );
        assert_eq!(store.read("original").unwrap().is_some(), mode == "after");
        let clock = store
            .qualify_elapsed_clock(ManualClock::new(BOOT, 300))
            .unwrap();
        assert_eq!(clock.read().unwrap(), 301);
    }
}

/// Parent-only entry: exit bypasses every destructor at the selected actual
/// transactional boundary. The path and clock are generated test inputs only.
#[test]
#[ignore = "self-executing custody elapsed-clock crash fixture"]
fn custody_clock_process_child() {
    let Some(path) = std::env::var_os("MEZ_TEST_CUSTODY_CLOCK_PATH") else {
        return;
    };
    let mut store = MmpCustodyStore::open(std::path::Path::new(&path), "session").unwrap();
    let native = ManualClock::new(BOOT, 100);
    store.qualify_elapsed_clock(native.clone()).unwrap();
    native.advance(200);
    let writes = [CustodyWrite {
        key: "original",
        previous: None,
        value: Some(b"original"),
    }];
    if std::env::var("MEZ_TEST_CUSTODY_CLOCK_CRASH").unwrap() == "before" {
        let transaction = store.connection.unchecked_transaction().unwrap();
        store
            .commit_in_transaction(&transaction, 0, &writes)
            .unwrap();
        std::process::exit(0);
    }
    store.commit(0, &writes).unwrap();
    std::process::exit(0);
}

/// Actual source4, t1, schema10 capture and original-store cold reconstruction
/// share the same logical clock. Same-boot downtime and a separately qualified
/// reboot retain original TTL/horizon boundaries. Decoded clock domains alone
/// cannot enable timing before the actual root installs its retained reader.
#[test]
fn custody_clock_cold_sources_keep_original_ttl_and_horizon_across_boots() {
    use crate::storage::mmp_custody::ownership::RootOwnership;
    use mez_agent::messaging::{
        Envelope, MESSAGE_TERMINAL_HANDOFF_MS, MessageAcceptanceWriteOutcome, MessageScope,
        MessageService, MessageStorageScope, MessageTerminalWriteOutcome, Recipient,
    };
    for reboot in [false, true] {
        for terminal in [false, true] {
            let fixture = Fixture::new();
            let owner = RootOwnership::acquire(&fixture.0).unwrap();
            let mut store = MmpCustodyStore::open(&fixture.0, "session").unwrap();
            let native = ManualClock::new(BOOT, 100);
            let reader = store.qualify_elapsed_clock(native.clone()).unwrap();
            owner.retain_elapsed_clock_for_tests(reader);
            let mut service = MessageService::default();
            let sender = service.register_agent(None, None, "sender", vec![]);
            let target = service.register_agent(None, None, "target", vec![]);
            let scope = MessageStorageScope::default();
            service
                .enable_storage_custody(&scope, Default::default())
                .unwrap();
            store.install_elapsed_clock(&mut service, &scope).unwrap();
            let ticket = service
                .prepare_acceptance(
                    &sender.agent_id,
                    Envelope {
                        protocol: "mmp/1",
                        id: "clocked".into(),
                        message_type: "send".into(),
                        time: "original".into(),
                        sender: sender.clone(),
                        recipient: Recipient::Agent(target.agent_id.clone()),
                        correlation_id: None,
                        ttl_ms: Some(5),
                        content_type: "text/plain".into(),
                        payload: "original".into(),
                        extension_fields: vec![],
                    },
                    MessageScope::Session,
                    u64::MAX,
                    4096,
                    None,
                )
                .unwrap();
            let plan = store.acceptance_plan(ticket.sequence()).unwrap();
            let write = service
                .begin_storage_acceptance_write(&ticket, &scope)
                .unwrap();
            let MessageAcceptanceWriteOutcome::Committed(commit) =
                write.persist(&mut store.acceptance_port(&plan))
            else {
                panic!("actual source must commit")
            };
            service.publish_acceptance(&commit).unwrap();
            if terminal {
                native.advance(106);
                let ticket = service
                    .prepare_storage_expiry(&scope, 0, 8 * 1024 * 1024)
                    .unwrap()
                    .unwrap();
                let write = service
                    .begin_storage_terminal_write(&ticket, &scope)
                    .unwrap();
                assert_eq!(write.observed_at_ms(), 107);
                let plan = store.acceptance_plan(0).unwrap().for_terminal();
                let mut owned = owner.bind_store(&mut store).unwrap();
                let MessageTerminalWriteOutcome::Committed(commit) =
                    write.persist(&mut owned.terminal_port(&plan, &scope))
                else {
                    panic!("actual t1 must commit")
                };
                service.publish_terminal(&commit).unwrap();
            }
            let request = service.prepare_storage_snapshot(&scope).unwrap();
            let verified = request
                .write()
                .verify(&mut owner.bind_store(&mut store).unwrap())
                .unwrap();
            let checkpoint = Arc::new(request.finish(verified).unwrap().0);
            drop(service);
            drop(store);
            drop(owner);
            let owner = RootOwnership::acquire_existing(&fixture.0).unwrap();
            let mut store =
                MmpCustodyStore::open_existing(&fixture.0, "session", Default::default()).unwrap();
            let native = ManualClock::new(
                if reboot { NEXT_BOOT } else { BOOT },
                if reboot { 25 } else { 120 },
            );
            let reader = store.qualify_elapsed_clock(native.clone()).unwrap();
            owner.retain_elapsed_clock_for_tests(reader);
            let scope = MessageStorageScope::default();
            let (mut service, _lease) = owner
                .bind_store(&mut store)
                .unwrap()
                .reconstruct_cold_service(checkpoint, &scope, Default::default(), 1000, 1024 * 1024)
                .unwrap();
            assert!(
                service.storage_observation_time(&scope, u64::MAX).is_err(),
                "decoded source installed a clock"
            );
            store.install_elapsed_clock(&mut service, &scope).unwrap();
            if terminal {
                assert!(
                    service
                        .prepare_storage_horizon_release(&scope, u64::MAX, 8 * 1024 * 1024)
                        .unwrap()
                        .is_none()
                );
                let native_due = if reboot {
                    MESSAGE_TERMINAL_HANDOFF_MS
                } else {
                    106 + MESSAGE_TERMINAL_HANDOFF_MS
                };
                native.advance(native_due - 1);
                assert!(
                    service
                        .prepare_storage_horizon_release(&scope, u64::MAX, 8 * 1024 * 1024)
                        .unwrap()
                        .is_none()
                );
                native.advance(native_due);
                let ticket = service
                    .prepare_storage_horizon_release(&scope, 0, 8 * 1024 * 1024)
                    .unwrap()
                    .unwrap();
                let write = service
                    .begin_storage_terminal_write(&ticket, &scope)
                    .unwrap();
                assert_eq!(write.observed_at_ms(), 107 + MESSAGE_TERMINAL_HANDOFF_MS);
                let plan = store.acceptance_plan(0).unwrap().for_terminal();
                let mut owned = owner.bind_store(&mut store).unwrap();
                let MessageTerminalWriteOutcome::Committed(commit) =
                    write.persist(&mut owned.terminal_port(&plan, &scope))
                else {
                    panic!("actual horizon must commit")
                };
                service.publish_terminal(&commit).unwrap();
                assert_eq!(
                    service.storage_usage().unwrap().unwrap().pending_messages,
                    0
                );
            } else {
                let now = service.storage_observation_time(&scope, 0).unwrap();
                assert_eq!(now, if reboot { 126 } else { 121 });
                let ticket = service
                    .prepare_storage_expiry(&scope, 0, 8 * 1024 * 1024)
                    .unwrap()
                    .unwrap();
                let write = service
                    .begin_storage_terminal_write(&ticket, &scope)
                    .unwrap();
                assert_eq!(write.observed_at_ms(), now);
            }
        }
    }
}
