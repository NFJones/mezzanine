//! Native-free regressions for the actual original-future coordination owner.
//!
//! These tests create no processes or native signal authority. Channels and
//! retained values stand in for actor/supervisor custody; the production test
//! helper's exact join_originals/capture code is used, not a parallel model.

use std::cell::Cell;
use std::time::Duration;

use super::*;

/// Supplies a finite original budget, with no cleanup extension in any test.
fn deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + Duration::from_secs(1)
}

#[tokio::test(flavor = "current_thread")]
/// An early typed client error must close once WHILE the same actor is polled
/// to answer that request; awaiting close alone would deadlock this regression.
async fn early_error_keeps_original_actor_and_supervisor_polled_until_close() {
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let (request, requested) = tokio::sync::oneshot::channel();
    let (acknowledge, acknowledged) = tokio::sync::oneshot::channel();
    let worker = async move {
        while !worker_stop.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        Ok("same supervisor")
    };
    let actor = async move {
        requested.await.unwrap();
        acknowledge.send(()).unwrap();
        Ok("same actor service")
    };
    let joined = join_originals(
        async { Err::<(), _>(MezError::invalid_state("original client error")) },
        worker,
        actor,
        deadline(),
        stop,
        || async move {
            observed.set(observed.get() + 1);
            request.send(()).unwrap();
            acknowledged.await.unwrap();
            Ok(())
        },
    )
    .await;
    assert_eq!(calls.get(), 1);
    assert!(!joined.timed_out);
    assert!(joined.client.completed && joined.supervisor.completed && joined.actor.completed);
    assert!(
        matches!(joined.client.failure, Some(Failure::Error(ref error)) if error.message() == "original client error")
    );
    assert_eq!(joined.supervisor.value, Some("same supervisor"));
    assert_eq!(joined.actor.value, Some("same actor service"));
}

#[tokio::test(flavor = "current_thread")]
/// Simultaneous typed failures retain both original errors, but cannot create a
/// second failure-triggered shutdown or replace the first recorded stage cause.
async fn simultaneous_stage_errors_request_only_one_shutdown() {
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let (request, requested) = tokio::sync::oneshot::channel();
    let joined = join_originals(
        async { Err::<(), _>(MezError::invalid_state("client")) },
        async { Err::<(), _>(MezError::invalid_state("worker")) },
        async {
            requested.await.unwrap();
            Ok("actor")
        },
        deadline(),
        Arc::new(AtomicBool::new(false)),
        || async move {
            observed.set(observed.get() + 1);
            request.send(()).unwrap();
            Ok(())
        },
    )
    .await;
    assert_eq!(calls.get(), 1);
    assert!(matches!(joined.client.failure, Some(Failure::Error(_))));
    assert!(matches!(joined.supervisor.failure, Some(Failure::Error(_))));
    assert_eq!(joined.order.len(), 2);
    assert_ne!(joined.order[0], joined.order[1]);
}

/// Deliberately non-string payload; no display conversion may consume it.
#[derive(Debug, PartialEq, Eq)]
struct OpaquePanic(u64);

#[tokio::test(flavor = "current_thread")]
/// catch_unwind retains arbitrary payloads together with a secondary close error;
/// no new panic hook or blocking destructor publication is involved.
async fn opaque_panic_and_secondary_close_error_are_both_retained() {
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let actor_stop = stop.clone();
    let joined = join_originals(
        async {
            std::panic::panic_any(OpaquePanic(41));
            #[allow(unreachable_code)]
            Ok::<(), MezError>(())
        },
        async move {
            while !worker_stop.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            Ok(())
        },
        async move {
            while !actor_stop.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            Ok(())
        },
        deadline(),
        stop,
        || async { Err(MezError::invalid_state("secondary close")) },
    )
    .await;
    match joined.client.failure {
        Some(Failure::Panic(payload)) => assert_eq!(
            payload.downcast_ref::<OpaquePanic>(),
            Some(&OpaquePanic(41))
        ),
        _ => panic!("original opaque payload was not retained"),
    }
    assert!(
        joined
            .cleanup
            .iter()
            .any(|error| error.contains("secondary close"))
    );
    assert!(matches!(
        joined.extra.as_slice(),
        [(Stage::Actor, Failure::Error(_))]
    ));
    assert!(joined.actor.completed && joined.supervisor.completed);
}

#[tokio::test(flavor = "current_thread")]
/// Normal original-stage completion conserves all values and must not manufacture
/// a failure close request or replace native proof with future/task completion.
async fn healthy_original_stage_results_are_preserved_without_extra_close() {
    let joined = join_originals(
        async { Ok(1) },
        async { Ok(2) },
        async { Ok(3) },
        deadline(),
        Arc::new(AtomicBool::new(true)),
        || async {
            panic!("unexpected close");
            #[allow(unreachable_code)]
            Ok(())
        },
    )
    .await;
    assert_eq!(
        (
            joined.client.value,
            joined.supervisor.value,
            joined.actor.value
        ),
        (Some(1), Some(2), Some(3))
    );
    assert!(joined.cleanup.is_empty());
    assert!(joined.order.is_empty());
    assert!(!joined.timed_out);
}

#[tokio::test(flavor = "current_thread")]
/// An already expired original budget forbids even the first stage/callback
/// poll, and all three cancellations remain explicitly unjoined/unknown.
async fn expired_budget_does_not_poll_original_stages_or_start_close() {
    let polls = Rc::new(Cell::new(0));
    let counter = polls.clone();
    let joined = join_originals(
        async {
            counter.set(counter.get() + 1);
            Ok(())
        },
        async { Ok(()) },
        async { Ok(()) },
        tokio::time::Instant::now(),
        Arc::new(AtomicBool::new(false)),
        || async {
            panic!("expired close");
            #[allow(unreachable_code)]
            Ok(())
        },
    )
    .await;
    assert_eq!(polls.get(), 0);
    assert!(joined.timed_out);
    assert!(!joined.client.completed && !joined.supervisor.completed && !joined.actor.completed);
    assert!(matches!(joined.client.failure, Some(Failure::Cancelled)));
}

#[tokio::test(flavor = "current_thread")]
/// Actor output is stored before sibling failure/timeout can cancel the join;
/// pending siblings remain cancelled, never promoted to successful completion.
async fn deadline_conserves_actual_completed_actor_output() {
    let joined = join_originals(
        std::future::pending::<Result<(), MezError>>(),
        std::future::pending::<Result<(), MezError>>(),
        async { Ok("returned actor service") },
        tokio::time::Instant::now() + Duration::from_millis(10),
        Arc::new(AtomicBool::new(false)),
        || async { Ok(()) },
    )
    .await;
    assert!(joined.timed_out);
    assert!(joined.actor.completed);
    assert_eq!(joined.actor.value, Some("returned actor service"));
    assert!(matches!(
        joined.actor.failure,
        Some(Failure::UnexpectedCompletion)
    ));
    assert!(!joined.client.completed && !joined.supervisor.completed);
}

#[tokio::test(flavor = "current_thread")]
/// A supervisor panic is contained at the same boundary as a client panic; both
/// the actor and original client still reach observed completion before return.
async fn supervisor_panic_does_not_drop_healthy_original_siblings() {
    let stop = Arc::new(AtomicBool::new(false));
    let client_stop = stop.clone();
    let actor_stop = stop.clone();
    let joined = join_originals(
        async move {
            while !client_stop.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            Ok(())
        },
        async {
            std::panic::panic_any(OpaquePanic(7));
            #[allow(unreachable_code)]
            Ok::<(), MezError>(())
        },
        async move {
            while !actor_stop.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            Ok(())
        },
        deadline(),
        stop,
        || async { Ok(()) },
    )
    .await;
    assert!(joined.client.completed && joined.actor.completed && joined.supervisor.completed);
    assert!(matches!(joined.supervisor.failure, Some(Failure::Panic(_))));
    assert!(!joined.timed_out);
}

#[test]
/// Dropping a preserving custody guard must not invoke a generic late destructor.
/// This checks the actual ManuallyDrop shape without a native process or signal.
fn unresolved_custody_shape_never_invokes_late_destructor() {
    struct DropProbe(Rc<Cell<usize>>);
    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    let drops = Rc::new(Cell::new(0));
    let custody = Some(ManuallyDrop::new(DropProbe(drops.clone())));
    drop(custody);
    assert_eq!(drops.get(), 0);
}

/// Creates an owner/token with no spawned child for native-free retirement tests.
fn reserved_fixture(label: &str) -> NativeFixture {
    let mut fixture = NativeFixture::new(label, Duration::from_millis(20));
    let native = PaneProcessRetirementOwner::default();
    fixture.attempt = Some(
        native
            .capture_cleanup_attempt(fixture.deadline.into_std())
            .unwrap(),
    );
    fixture.native = Some(ManuallyDrop::new(native));
    fixture
}

#[tokio::test(flavor = "current_thread")]
/// A still-reserved obligation is not OS completion. Exercise the actual native
/// owner/drain with only a pre-spawn reservation (no child/process), then prove
/// failed retirement is inert on a second call and preserves generated evidence.
async fn unresolved_reserved_slot_preserves_storage_without_fresh_retirement() {
    let mut fixture = reserved_fixture("reserved-proof");
    let root = fixture.root.clone();
    let native = fixture.owner().clone();
    let reservation = native.reserve().unwrap();
    let original = fixture.deadline;
    let mut joined = Joined {
        client: StageResult {
            value: Some(()),
            failure: None,
            completed: true,
        },
        supervisor: StageResult {
            value: Some(()),
            failure: None,
            completed: true,
        },
        actor: StageResult {
            value: None,
            failure: Some(Failure::Panic(Box::new(OpaquePanic(9)))),
            completed: true,
        },
        order: vec![Stage::Actor],
        cleanup: Vec::new(),
        timed_out: false,
        extra: Vec::new(),
    };
    fixture.retire(&mut joined).await;
    assert!(!fixture.settled);
    assert_eq!(fixture.deadline, original);
    let errors = joined.cleanup.len();
    assert!(errors > 0);
    drop(reservation); // A fake reservation, not a native process or receipt.
    fixture.retire(&mut joined).await;
    assert_eq!(joined.cleanup.len(), errors, "retirement was retried");
    let failure = joined.finish(&fixture).unwrap_err();
    assert!(!failure.cleanup.is_empty());
    assert!(!failure.failures.is_empty());
    assert_eq!(failure.pending, 0); // Zero later does not erase prior failure.
    assert_eq!(failure.root, root);
    drop(fixture);
    assert!(root.is_dir(), "unconfirmed fixture evidence was deleted");
    // Only generated, native-free test storage; no held/native fixture cleanup.
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
/// Private startup files are inert and remain distinct from ambient HOME. Merely
/// preparing a fixture without an admitted child never authorizes its deletion.
fn private_startup_environment_is_generated_and_default_preserved() {
    let fixture = NativeFixture::new("startup-proof", Duration::from_secs(1));
    let root = fixture.root.clone();
    let home = fixture
        .environment
        .iter()
        .find(|(key, _)| key == "HOME")
        .unwrap()
        .1
        .clone();
    let zdotdir = fixture
        .environment
        .iter()
        .find(|(key, _)| key == "ZDOTDIR")
        .unwrap()
        .1
        .clone();
    assert_eq!(home, zdotdir);
    assert!(Path::new(&home).starts_with(&root));
    assert!(
        std::fs::read_to_string(Path::new(&home).join(".zshrc"))
            .unwrap()
            .starts_with('#')
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(Path::new(&home).join(".zshrc"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    drop(fixture);
    assert!(root.is_dir());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
/// Uses the SAME capture/notification path as both real supervisor adapters. A
/// dropped client receiver cannot discard or reconstruct the original I/O/backend
/// error: all typed metadata must survive in external supervisor stage custody.
async fn dropped_completion_receiver_preserves_original_marked_supervisor_error() {
    let (completed, receiver) = tokio::sync::oneshot::channel();
    drop(receiver);
    let marked = MezError::from(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "owned backend failure",
    ))
    .mark_pane_backend_failure();
    let joined = join_originals_notified(
        async { Ok(()) },
        async { Err::<(), _>(marked) },
        async { Ok(()) },
        deadline(),
        Arc::new(AtomicBool::new(true)),
        || async { Ok(()) },
        Some(completed),
    )
    .await;
    match joined.supervisor.failure {
        Some(Failure::Error(error)) => {
            assert_eq!(error.io_kind(), Some(std::io::ErrorKind::PermissionDenied));
            assert!(error.is_pane_backend_failure());
            assert!(error.message().contains("owned backend failure"));
        }
        _ => panic!("actual supervisor error was discarded or normalized"),
    }
}

#[tokio::test(flavor = "current_thread")]
/// A cleanup panic must not replace the original opaque setup panic. The actual
/// setup compound owner retains both non-Clone payloads before final publication.
async fn setup_primary_and_cleanup_panics_remain_separately_owned() {
    let failures = compound_setup_failure(Failure::Panic(Box::new(OpaquePanic(1))), async {
        std::panic::panic_any(OpaquePanic(2));
        #[allow(unreachable_code)]
        Ok(())
    })
    .await;
    assert_eq!(failures.len(), 2);
    for (index, (_, failure)) in failures.into_iter().enumerate() {
        match failure {
            Failure::Panic(payload) => assert_eq!(
                payload.downcast_ref::<OpaquePanic>(),
                Some(&OpaquePanic(index as u64 + 1))
            ),
            _ => panic!("setup panic ownership was replaced"),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
/// Typed setup failure and typed secondary retirement error remain distinct;
/// neither success-shaped completion nor formatted replacement may erase either.
async fn setup_typed_primary_and_secondary_cleanup_errors_remain_owned() {
    let primary = MezError::from(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "setup backend",
    ))
    .mark_pane_backend_failure();
    let failures = compound_setup_failure(Failure::Error(primary), async {
        Err(MezError::invalid_state("cleanup secondary"))
    })
    .await;
    assert!(
        matches!(&failures[0].1, Failure::Error(error) if error.io_kind() == Some(std::io::ErrorKind::BrokenPipe) && error.is_pane_backend_failure())
    );
    assert!(
        matches!(&failures[1].1, Failure::Error(error) if error.message() == "cleanup secondary")
    );
}
