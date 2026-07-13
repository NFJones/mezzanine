//! Unit tests for session registry persistence and JSON output.

use super::{
    PathBuf, RegistrySessionState, SessionRecord, SessionRegistry, records_to_json,
    resolve_session_record_target, session_record_index_aliases,
};
use crate::runtime::{effective_uid_for_tests, ensure_private_socket_directory};
use crate::shell::{ResolvedShell, ShellSource};
use mez_mux::layout::Size;
use mez_mux::session::Session;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc::{self, TryRecvError};
use std::time::{Duration, Instant};

/// Runs the test root operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("mez-registry-test-{name}-{}", std::process::id()))
}

/// Runs the wait for registry writer operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn wait_for_registry_writer(rx: &mpsc::Receiver<()>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match rx.try_recv() {
            Ok(()) => return true,
            Err(TryRecvError::Disconnected) => return false,
            Err(TryRecvError::Empty) if Instant::now() >= deadline => return false,
            Err(TryRecvError::Empty) => std::thread::yield_now(),
        }
    }
}

/// Runs the record operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn record(id: &str) -> SessionRecord {
    SessionRecord {
        session_id: id.to_string(),
        name: "default".to_string(),
        state: RegistrySessionState::Running,
        socket_path: PathBuf::from(format!("/tmp/mez-1000/{id}.sock")),
        created_at_unix_seconds: 10,
        last_attach_at_unix_seconds: Some(20),
        window_count: 1,
        client_count: 1,
        primary_available: false,
        authoritative_columns: 80,
        authoritative_rows: 24,
    }
}

/// Verifies missing registry lists empty.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn missing_registry_lists_empty() {
    let root = test_root("missing");
    let _ = fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), effective_uid_for_tests());

    assert!(registry.list().unwrap().is_empty());

    let _ = fs::remove_dir_all(root);
}

/// Verifies upsert round trips registry records.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn upsert_round_trips_registry_records() {
    let root = test_root("roundtrip");
    let _ = fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), effective_uid_for_tests());

    registry.upsert(record("$1")).unwrap();

    let records = registry.list().unwrap();
    assert_eq!(records, vec![record("$1")]);
    assert_eq!(
        fs::metadata(registry.registry_file())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies that the async registry mutation path preserves the same record
/// ordering, replacement, removal, and private-file permissions as the
/// synchronous registry API used by compatibility callers.
#[tokio::test]
async fn async_upsert_and_remove_round_trip_registry_records() {
    let root = test_root("async-roundtrip");
    let _ = fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), effective_uid_for_tests());

    registry.upsert_async(record("$1")).await.unwrap();
    let mut updated = record("$1");
    updated.state = RegistrySessionState::Detached;
    updated.primary_available = true;
    registry.upsert_async(updated.clone()).await.unwrap();
    registry.upsert_async(record("$2")).await.unwrap();

    let records = registry.list_async().await.unwrap();
    assert_eq!(records, vec![updated, record("$2")]);
    assert_eq!(
        fs::metadata(registry.registry_file())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(registry.remove_async("$1").await.unwrap());
    assert_eq!(registry.list_async().await.unwrap(), vec![record("$2")]);

    let _ = fs::remove_dir_all(root);
}

/// Verifies that registry read-modify-write updates hold an interprocess file
/// lock for the full mutation. Multiple detached daemons tick and update the
/// same registry directory; without this lock, concurrent upserts can overwrite
/// one another and make `mez list` show only the last writer.
#[test]
fn upsert_waits_for_exclusive_registry_lock() {
    let root = test_root("locked-upsert");
    let _ = fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), effective_uid_for_tests());
    let lock = registry.acquire_exclusive_lock().unwrap();
    let writer = SessionRegistry::new(root.clone(), effective_uid_for_tests());
    let (done, written) = mpsc::channel();

    let thread = std::thread::spawn(move || {
        writer.upsert(record("$blocked")).unwrap();
        done.send(()).unwrap();
    });

    assert!(
        !wait_for_registry_writer(&written, Duration::from_millis(50)),
        "registry upsert completed while the registry lock was still held"
    );
    drop(lock);
    assert!(
        wait_for_registry_writer(&written, Duration::from_secs(2)),
        "registry upsert did not continue after the lock was released"
    );
    thread.join().unwrap();
    assert_eq!(registry.list().unwrap(), vec![record("$blocked")]);

    let _ = fs::remove_dir_all(root);
}

/// Verifies that async registry writes wait for the registry file lock without
/// blocking a current-thread runtime. This protects the daemon from a
/// self-deadlock where one task holds the session-registry flock and another
/// task blocks the only async reactor thread while trying to reacquire it.
#[test]
fn async_upsert_waits_for_registry_lock_without_blocking_current_thread_runtime() {
    let root = test_root("async-locked-upsert");
    let _ = fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), effective_uid_for_tests());
    let lock = registry.acquire_exclusive_lock().unwrap();
    let writer = SessionRegistry::new(root.clone(), effective_uid_for_tests());
    let (tick_tx, tick_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();

    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let update = tokio::spawn(async move {
                writer.upsert_async(record("$blocked")).await.unwrap();
                done_tx.send(()).unwrap();
            });
            tokio::time::sleep(Duration::from_millis(50)).await;
            tick_tx.send(()).unwrap();
            update.await.unwrap();
        });
    });

    let reactor_progressed = wait_for_registry_writer(&tick_rx, Duration::from_secs(1));
    assert!(
        !wait_for_registry_writer(&done_rx, Duration::from_millis(50)),
        "registry upsert completed while the registry lock was still held"
    );
    drop(lock);
    assert!(
        wait_for_registry_writer(&done_rx, Duration::from_secs(2)),
        "registry upsert did not continue after the lock was released"
    );
    thread.join().unwrap();
    assert!(
        reactor_progressed,
        "async registry upsert blocked the current-thread runtime while waiting for the registry lock"
    );
    assert_eq!(registry.list().unwrap(), vec![record("$blocked")]);

    let _ = fs::remove_dir_all(root);
}

/// Verifies upsert replaces existing record.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn upsert_replaces_existing_record() {
    let root = test_root("replace");
    let _ = fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), effective_uid_for_tests());
    registry.upsert(record("$1")).unwrap();

    let mut updated = record("$1");
    updated.state = RegistrySessionState::Detached;
    updated.primary_available = true;
    registry.upsert(updated.clone()).unwrap();

    assert_eq!(registry.list().unwrap(), vec![updated]);

    let _ = fs::remove_dir_all(root);
}

/// Verifies remove deletes record by session id.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn remove_deletes_record_by_session_id() {
    let root = test_root("remove");
    let _ = fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), effective_uid_for_tests());
    registry.upsert(record("$1")).unwrap();
    registry.upsert(record("$2")).unwrap();

    assert!(registry.remove("$1").unwrap());
    assert_eq!(registry.list().unwrap(), vec![record("$2")]);
    assert!(!registry.remove("$missing").unwrap());

    let _ = fs::remove_dir_all(root);
}

/// Verifies prune stale removes records without live socket paths.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn prune_stale_removes_records_without_live_socket_paths() {
    let root = test_root("prune");
    let _ = fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), effective_uid_for_tests());
    let live_socket = root.join("live.sock");
    ensure_private_socket_directory(&root, effective_uid_for_tests()).unwrap();
    fs::write(&live_socket, "").unwrap();
    let mut live = record("$1");
    live.socket_path = live_socket;
    registry.upsert(live.clone()).unwrap();
    registry.upsert(record("$2")).unwrap();

    let pruned = registry.prune_stale().unwrap();

    assert_eq!(pruned, 1);
    assert_eq!(registry.list().unwrap(), vec![live]);

    let _ = fs::remove_dir_all(root);
}

/// Verifies rejects relative socket paths.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_relative_socket_paths() {
    let mut record = record("$1");
    record.socket_path = PathBuf::from("relative.sock");

    let error = record.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies builds record from session state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn builds_record_from_session_state() {
    let shell = ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh);
    let mut session = Session::new_default(shell, Size::new(80, 24).unwrap());
    let primary = session.attach_primary("primary", true).unwrap();
    session.detach_primary(&primary).unwrap();

    let record = SessionRecord::from_session(
        &session,
        PathBuf::from("/tmp/mez-1000/default.sock"),
        100,
        Some(110),
    );

    assert_eq!(record.state, RegistrySessionState::Detached);
    assert!(record.primary_available);
    assert_eq!(record.client_count, 1);
}

/// Verifies records render as json array.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn records_render_as_json_array() {
    let json = records_to_json(&[record("$1")]);

    assert!(json.starts_with("[{"));
    assert!(json.contains("\"session_id\":\"$1\""));
    assert!(json.contains("\"index_alias\":\"$1\""));
    assert!(json.contains("\"primary_available\":false"));
}

/// Verifies creation-order index aliases are derived from registry records
/// without being persisted into the registry data model.
#[test]
fn index_aliases_follow_session_creation_order() {
    let mut newest = record("$newest");
    newest.created_at_unix_seconds = 30;
    let mut oldest = record("$oldest");
    oldest.created_at_unix_seconds = 10;
    let mut middle = record("$middle");
    middle.created_at_unix_seconds = 20;

    let records = vec![newest.clone(), oldest.clone(), middle.clone()];
    let aliases = session_record_index_aliases(&records);

    assert_eq!(aliases[0].index_alias, "$1");
    assert_eq!(aliases[0].record.session_id, "$oldest");
    assert_eq!(aliases[1].index_alias, "$2");
    assert_eq!(aliases[1].record.session_id, "$middle");
    assert_eq!(aliases[2].index_alias, "$3");
    assert_eq!(aliases[2].record.session_id, "$newest");
}

/// Verifies registry target resolution accepts full session ids plus displayed
/// and bare creation-order aliases while keeping exact ids authoritative.
#[test]
fn resolves_session_records_by_exact_id_and_index_alias() {
    let mut first = record("$canonical");
    first.created_at_unix_seconds = 10;
    let mut second = record("$1");
    second.created_at_unix_seconds = 20;
    let records = vec![second.clone(), first.clone()];

    assert_eq!(
        resolve_session_record_target(&records, "$canonical")
            .map(|record| record.session_id.as_str()),
        Some("$canonical")
    );
    assert_eq!(
        resolve_session_record_target(&records, "1").map(|record| record.session_id.as_str()),
        Some("$canonical")
    );
    assert_eq!(
        resolve_session_record_target(&records, "$1").map(|record| record.session_id.as_str()),
        Some("$1")
    );
    assert!(resolve_session_record_target(&records, "$99").is_none());
}
