//! Host native receipt publication versus resource and callback settlement.
//!
//! Diagnostic publication is deliberately injected at private, per-call readers
//! or a test-only post-native publication point. Native slots, root signaling,
//! wait/reap and strict PID absence remain real production operations. These
//! tests do not claim an OS permission/I/O fault, infer effect from a task join,
//! or treat a cleared entry/counter as physical retirement proof.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

/// Creates a real returned root while its startup callback is deliberately
/// withheld. A duplicated master prevents kernel hangup from supplying proof.
fn returned_starting_owner(
    supervisor: &SessionSupervisor,
    id: &str,
) -> (
    PaneProcessRetirementOwner,
    rustix::process::Pid,
    Option<u64>,
    std::os::fd::OwnedFd,
) {
    let native = PaneProcessRetirementOwner::default();
    let process = mez_mux::process::spawn_retained_argv_pty_process(
        std::path::Path::new("/bin/cat"),
        &[],
        &[],
        Size::new(80, 24).unwrap(),
        None,
        native.reserve().unwrap(),
    )
    .unwrap();
    let raw_pid = process.primary_pid();
    let pid = rustix::process::Pid::from_raw(i32::try_from(raw_pid).unwrap()).unwrap();
    let start = mez_mux::process::process_start_token_for_pid(raw_pid);
    let master = process.duplicate_master_fd().unwrap();
    assert!(
        supervisor
            .inner
            .entries()
            .unwrap()
            .insert(
                id.to_string(),
                SupervisorEntry {
                    generation: 1,
                    native: native.clone(),
                    completion: watch::channel(ExactCompletionState::Pending).0,
                    completion_claimed: false,
                    state: SupervisorEntryState::Starting { cancelled: false },
                }
            )
            .is_none()
    );
    drop(process);
    (native, pid, start, master)
}

/// Supplies only classified diagnostic evidence. The real owner and its count
/// remain untouched; notification proves the initial read preceded publication.
fn owner_receipt_reader(
    published: &AtomicBool,
    mut entered: Option<tokio::sync::oneshot::Sender<()>>,
) -> impl FnMut(&PaneProcessRetirementOwner) -> Option<String> + '_ {
    move |_| {
        if let Some(entered) = entered.take() {
            let _ = entered.send(());
        }
        published
            .load(AtomicOrdering::Acquire)
            .then(|| "late owner-stage receipt".to_string())
    }
}

/// Native completion may precede the host callback. The host must read owner
/// receipts after total zero, even if its aggregate is still empty. Cancellation
/// retries the same owner/deadline; an earlier host cause keeps precedence.
#[tokio::test]
async fn host_zero_native_count_refreshes_owner_receipt_before_callback() {
    for cancel_first in [false, true] {
        for earlier in [false, true] {
            let supervisor = SessionSupervisor::new(0);
            let id = "$owner-before-callback";
            let (native, pid, start, master) = returned_starting_owner(&supervisor, id);
            supervisor.fence_native_admission(false).unwrap();
            if earlier {
                supervisor.retain_native_failure_for_test();
            }
            let authority = native
                .capture_cleanup_attempt(std::time::Instant::now() + Duration::from_secs(1))
                .unwrap();
            let mut claim = native
                .try_begin_drain_for_attempt(&authority)
                .unwrap()
                .unwrap();
            let mut child = claim.take_returned().unwrap();
            let published = AtomicBool::new(false);
            let (entered, observed) = tokio::sync::oneshot::channel();
            let (release, released) = tokio::sync::oneshot::channel();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            let waiting = async {
                if cancel_first {
                    let was_pending = {
                        let first = supervisor.drain_native_with_owner_receipts(
                            deadline,
                            owner_receipt_reader(&published, Some(entered)),
                        );
                        tokio::pin!(first);
                        tokio::time::timeout(Duration::from_millis(30), &mut first)
                            .await
                            .is_err()
                    };
                    assert!(was_pending);
                    release.send(()).unwrap();
                    supervisor
                        .drain_native_with_owner_receipts(
                            deadline,
                            owner_receipt_reader(&published, None),
                        )
                        .await
                } else {
                    release.send(()).unwrap();
                    supervisor
                        .drain_native_with_owner_receipts(
                            deadline,
                            owner_receipt_reader(&published, Some(entered)),
                        )
                        .await
                }
            };
            let completing = async {
                observed.await.unwrap();
                released.await.unwrap();
                published.store(true, AtomicOrdering::Release);
                child.force_retirement().unwrap();
                loop {
                    if child.try_wait().unwrap().is_some() {
                        break;
                    }
                    assert!(tokio::time::Instant::now() < deadline, "{pid:?} {start:?}");
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                drop(child);
                drop(claim);
            };
            let (verdict, ()) = tokio::join!(waiting, completing);
            let error = verdict.unwrap_err();
            assert!(error.message().contains(if earlier {
                "injected native signal failure receipt"
            } else {
                "late owner-stage receipt"
            }));
            assert_eq!(native.outstanding(), 0);
            assert_eq!(
                rustix::process::test_kill_process(pid),
                Err(rustix::io::Errno::SRCH),
                "{start:?}"
            );
            assert!(
                supervisor.contains(id).unwrap(),
                "startup callback was not delivered yet"
            );
            assert_eq!(
                supervisor.native_cleanup_failure().unwrap().is_some(),
                earlier
            );
            assert!(
                !supervisor
                    .inner
                    .retain_native_with_owner_receipt(
                        id,
                        1,
                        "startup callback cause".to_string(),
                        owner_receipt_reader(&published, None)
                    )
                    .unwrap()
            );
            supervisor
                .inner
                .finish_start_failure(id, 1, "startup callback cause".to_string())
                .unwrap();
            assert!(!supervisor.contains(id).unwrap());
            assert!(supervisor.native_cleanup_failure().unwrap().is_some());
            drop(master);
        }
    }
}

/// Callback count and receipt reads use different native-owner locks. Gate a
/// real final wait between them: a prior nonzero snapshot must retain the exact
/// generation for a later receipt refresh, not remove it using a stale None.
#[test]
fn callback_observes_pending_before_receipt_and_retains_racing_generation() {
    let supervisor = SessionSupervisor::new(0);
    let id = "$callback-order";
    let (native, pid, start, master) = returned_starting_owner(&supervisor, id);
    let authority = native
        .capture_cleanup_attempt(std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    let mut claim = native
        .try_begin_drain_for_attempt(&authority)
        .unwrap()
        .unwrap();
    let mut child = claim.take_returned().unwrap();
    let published = Arc::new(AtomicBool::new(false));
    let (release, released) = std::sync::mpsc::channel();
    let (done, completed) = std::sync::mpsc::channel();
    let producer_flag = published.clone();
    let producer = std::thread::spawn(move || {
        released.recv_timeout(Duration::from_secs(2)).unwrap();
        producer_flag.store(true, AtomicOrdering::Release);
        child.force_retirement().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(child);
        drop(claim);
        done.send(()).unwrap();
    });
    let retained = supervisor
        .inner
        .retain_native_with_owner_receipt(id, 1, "initiating callback cause".to_string(), |_| {
            let cached = published
                .load(AtomicOrdering::Acquire)
                .then(|| "racing owner receipt".to_string());
            release.send(()).unwrap();
            completed.recv_timeout(Duration::from_secs(2)).unwrap();
            cached
        })
        .unwrap();
    producer.join().unwrap();
    assert!(
        retained,
        "an earlier pending snapshot must retain the generation"
    );
    assert_eq!(native.outstanding(), 0);
    assert_eq!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH),
        "{start:?}"
    );
    assert!(supervisor.contains(id).unwrap());
    assert!(supervisor.native_cleanup_failure().unwrap().is_none());
    assert!(
        !supervisor
            .inner
            .retain_native_with_owner_receipt(
                id,
                1,
                "initiating callback cause".to_string(),
                |_| {
                    assert!(published.load(AtomicOrdering::Acquire));
                    Some("racing owner receipt".to_string())
                }
            )
            .unwrap()
    );
    supervisor
        .inner
        .finish_start_failure(id, 1, "initiating callback cause".to_string())
        .unwrap();
    assert!(!supervisor.contains(id).unwrap());
    assert_eq!(
        supervisor.native_cleanup_failure().unwrap().as_deref(),
        Some("racing owner receipt")
    );
    drop(master);
}

/// This code-owned probe models delayed callback receipt publication after the
/// native future's result, never a new native error or a fabricated wait result.
fn publish_after_healthy_native(supervisor: &SessionSupervisor, native_ok: bool) {
    assert!(native_ok);
    assert_eq!(supervisor.native_pending_count().unwrap(), 0);
    assert!(supervisor.native_cleanup_failure().unwrap().is_none());
    supervisor.retain_native_failure_for_test();
}

/// The real start/actor/worker path must retire its original child. A labelled
/// receipt published after that native future but before entry settlement still
/// makes shutdown fail; an empty entry map cannot erase the delayed verdict.
#[tokio::test(flavor = "current_thread")]
async fn shutdown_refreshes_callback_receipt_after_native_and_entry_settlement() {
    let supervisor = SessionSupervisor::new(0);
    let request = native_factory_request("shutdown-receipt-order");
    let id = request.session.id.to_string();
    let root = request.config.root.clone();
    let (spawned, witness) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    *supervisor.inner.native_startup_probe.lock().unwrap() = Some((
        id.clone(),
        1,
        super::super::super::NativePaneStartupProbe {
            spawned,
            release: released,
        },
    ));
    let observer = async {
        let witness = witness.await.unwrap();
        release.send(false).unwrap();
        witness
    };
    let (handle, witnesses) = tokio::join!(supervisor.start(request), observer);
    let handle = handle.unwrap();
    assert_eq!(witnesses.len(), 1);
    let result = supervisor
        .shutdown_all_with_receipt_probe(
            true,
            Duration::from_secs(2),
            Some(publish_after_healthy_native),
        )
        .await;
    assert!(
        result
            .unwrap_err()
            .message()
            .contains("injected native signal failure receipt")
    );
    assert!(!supervisor.contains(&id).unwrap());
    assert_eq!(supervisor.native_pending_count().unwrap(), 0);
    for (pid, start) in witnesses {
        let exact = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
        assert_eq!(
            rustix::process::test_kill_process(exact),
            Err(rustix::io::Errno::SRCH),
            "{start:?}"
        );
    }
    drop(handle);
    fs::remove_dir_all(root).unwrap();
}
