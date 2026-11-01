//! Real worker/root concurrency tests with private generated SQLite destinations.
//! Gates block actual blocking workers, while observers and unrelated async work
//! remain schedulable. No status label substitutes for the actual joined handle.

use super::*;
use mez_agent::messaging::{
    Envelope, MessageRequestOrigin, MessageScope, Recipient, SenderIdentity,
};
use std::sync::atomic::AtomicUsize;

/// Actual blocking-worker controls, independent of observers and provider slots.
pub(super) struct Probe {
    /// Announces entry into the physical worker before the optional block.
    started: tokio::sync::Notify,
    /// A real Condvar gate used only inside the blocking worker.
    gate: (Mutex<bool>, std::sync::Condvar),
    /// Exact actual physical worker entry count.
    attempts: AtomicUsize,
    /// One-shot loss after real marker completion but before the root gets it.
    panic_after_commit: AtomicBool,
}
impl Probe {
    /// Constructs a retained real worker gate with optional immediate release.
    fn new(released: bool) -> Arc<Self> {
        Arc::new(Self {
            started: tokio::sync::Notify::new(),
            gate: (Mutex::new(released), std::sync::Condvar::new()),
            attempts: AtomicUsize::new(0),
            panic_after_commit: AtomicBool::new(false),
        })
    }
    /// Blocks the physical worker without occupying any async executor thread.
    pub(super) fn before_write(&self) {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        let mut released = self.gate.0.lock().unwrap();
        while !*released {
            released = self.gate.1.wait(released).unwrap();
        }
    }
    /// Injects actual thread panic only after the production port committed.
    pub(super) fn after_write(&self, outcome: &MessageAcceptanceWriteOutcome) {
        if matches!(outcome, MessageAcceptanceWriteOutcome::Committed(_))
            && self.panic_after_commit.swap(false, Ordering::SeqCst)
        {
            panic!("injected loss after actual custody marker commit");
        }
    }
    /// Releases the real original worker, including after an assertion unwind.
    fn release(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }
}

/// Generated destination plus a release guard preventing fixture hang on unwind.
struct Fixture {
    path: PathBuf,
    probe: Arc<Probe>,
}
impl Fixture {
    /// Allocates an initially absent destination and retained physical gate.
    fn new(released: bool) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "mez-mmp-worker-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            )),
            probe: Probe::new(released),
        }
    }
    /// Registers actual root custody before any observer or original write.
    fn root(&self) -> MmpCustodyRoot {
        MmpCustodyRoot::construct(
            Destination {
                root: self.path.clone(),
                owner: "original-session".into(),
                limits: CustodyStoreLimits::default(),
            },
            Handle::current(),
            Some(self.probe.clone()),
        )
    }
    /// Reads actual SQLite row/generation facts, never observer status text.
    fn disk(&self) -> (i64, i64) {
        rusqlite::Connection::open(self.path.join("custody.sqlite"))
            .unwrap()
            .query_row(
                "SELECT generation,(SELECT COUNT(*) FROM records) FROM state",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }
}
impl Drop for Fixture {
    /// Releases only the generated worker gate and removes this private fixture.
    fn drop(&mut self) {
        self.probe.release();
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Creates real registered actors and one trusted original request origin.
fn fixture_service() -> (
    MessageService,
    SenderIdentity,
    SenderIdentity,
    MessageRequestOrigin,
) {
    let mut service = MessageService::with_limits(16, 4 * 1024 * 1024);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    let origin = MessageRequestOrigin {
        agent_id: sender.agent_id.clone(),
        conversation_id: "original-conversation".into(),
        turn_id: "original-turn".into(),
        started_at_unix_seconds: 9,
    };
    (service, sender, target, origin)
}

/// Builds the immutable semantic input, with identities from actual registrations.
fn envelope(sender: &SenderIdentity, target: &SenderIdentity, id: &str) -> Envelope {
    Envelope {
        protocol: "mmp/1",
        id: id.into(),
        message_type: "send".into(),
        time: "original-time".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: Some(1234),
        content_type: "text/plain".into(),
        payload: "original\0payload".repeat(20_000),
        extension_fields: vec![],
    }
}

/// Cancelling a pending observer cannot detach the actual gated blocking worker
/// or fabricate acceptance. A second observer remains able to join the original;
/// Stop closes new admission but preserves exact original publication afterward.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observer_abort_and_stop_keep_actual_worker_owned_until_original_adoption() {
    let fixture = Fixture::new(false);
    let root = fixture.root();
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let (mut service, sender, target, _) = fixture_service();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "one"),
            MessageScope::Session,
            100,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &ticket, None).unwrap();
    fixture.probe.started.notified().await;
    let observer_root = root.clone();
    let observer = tokio::spawn(async move { observer_root.observe().await });
    tokio::task::yield_now().await;
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    root.request_stop();
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
    assert!(service.cancel_acceptance(&ticket).is_err());
    // Genuine unrelated asynchronous work still runs while physical I/O waits.
    assert_eq!(tokio::spawn(async { 17 }).await.unwrap(), 17);
    fixture.probe.release();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    let delivery = root.adopt(&mut service, None).unwrap();
    assert_eq!(delivery.sequence, ticket.sequence());
    assert!(delivery.accepted);
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    assert!(
        !root
            .retry(&mut service, Instant::now() + Duration::from_secs(60))
            .unwrap()
    );
}

/// Busy and stopped root checks happen before lower write issuance. An unrelated
/// request can still cancel its never-issued reservation; no dropped linear input
/// or duplicate physical worker is created by rejected root admission.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_or_stopped_root_does_not_issue_or_drop_another_lower_write() {
    let fixture = Fixture::new(false);
    let root = fixture.root();
    root.observe().await.unwrap();
    let (mut service, sender, target, _) = fixture_service();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "first"),
            MessageScope::Session,
            1,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &ticket, None).unwrap();
    fixture.probe.started.notified().await;
    let (mut other, other_sender, other_target, _) = fixture_service();
    let other_ticket = other
        .prepare_acceptance(
            &other_sender.agent_id,
            envelope(&other_sender, &other_target, "second"),
            MessageScope::Session,
            1,
            4096,
            None,
        )
        .unwrap();
    assert!(root.start(&mut other, &other_ticket, None).is_err());
    assert!(other.cancel_acceptance(&other_ticket).is_ok());
    root.request_stop();
    fixture.probe.release();
    root.observe().await.unwrap();
    root.adopt(&mut service, None).unwrap();
    let other_ticket = other
        .prepare_acceptance(
            &other_sender.agent_id,
            envelope(&other_sender, &other_target, "second"),
            MessageScope::Session,
            2,
            4096,
            None,
        )
        .unwrap();
    assert!(root.start(&mut other, &other_ticket, None).is_err());
    assert!(other.cancel_acceptance(&other_ticket).is_ok());
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
}

/// Positive tracked completion stays retained through wrong-owner or missing-pair
/// adoption attempts. Caller retirement cannot release its unpublished original;
/// correct paired adoption attaches the same receipt to the original turn once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracked_positive_fact_survives_wrong_adopters_and_original_caller_retirement() {
    let fixture = Fixture::new(true);
    let root = fixture.root();
    root.observe().await.unwrap();
    let (mut service, sender, target, origin) = fixture_service();
    let mut requests = MessageRequests::default();
    let ticket = requests
        .prepare_acceptance(
            &mut service,
            origin.clone(),
            envelope(&sender, &target, "tracked"),
            MessageScope::Session,
            100,
        )
        .unwrap();
    let usage = requests.usage();
    root.start(&mut service, &ticket, Some(&requests)).unwrap();
    requests.retire_conversation(&origin.agent_id, &origin.conversation_id);
    root.observe().await.unwrap();
    assert!(root.adopt(&mut service, None).is_err());
    let mut wrong_requests = MessageRequests::default();
    assert!(root.adopt(&mut service, Some(&mut wrong_requests)).is_err());
    let (mut foreign, _, _, _) = fixture_service();
    assert!(root.adopt(&mut foreign, Some(&mut requests)).is_err());
    assert!(service.has_unpublished_acceptance());
    assert_eq!(requests.usage(), usage);
    let delivery = root.adopt(&mut service, Some(&mut requests)).unwrap();
    assert_eq!(delivery.sequence, ticket.sequence());
    let receipt = requests
        .find(&origin.agent_id, &origin.conversation_id, "tracked")
        .unwrap();
    assert_eq!(receipt.origin(), &origin);
    assert_eq!(requests.usage(), (1, usage.1, usage.2));
    assert!(root.adopt(&mut service, Some(&mut requests)).is_err());
}

/// A production store marker commits before an injected actual worker panic.
/// The root observes that real failed join, retains its original plan/escrow,
/// reopens the same allocation after backoff and reconciles without another row
/// or generation. Stop does not convert this already-issued original to unsent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_postcommit_worker_panic_retries_same_plan_without_second_disk_occurrence() {
    let fixture = Fixture::new(true);
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    let root = fixture.root();
    root.observe().await.unwrap();
    let (mut service, sender, target, _) = fixture_service();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "original"),
            MessageScope::Session,
            100,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &ticket, None).unwrap();
    let before_observe = Instant::now();
    let CustodyWorkerStatus::RetryAt(due) = root.observe().await.unwrap() else {
        panic!("expected joined failure")
    };
    assert!(due >= before_observe + Duration::from_secs(1));
    let disk = fixture.disk();
    assert!(disk.0 > 2);
    assert!(
        !root
            .retry(&mut service, due - Duration::from_nanos(1))
            .unwrap()
    );
    root.request_stop();
    assert!(root.retry(&mut service, due).unwrap());
    root.observe().await.unwrap();
    assert_eq!(fixture.disk(), disk);
    assert_eq!(
        root.adopt(&mut service, None).unwrap().sequence,
        ticket.sequence()
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
}

/// Real SQLite marker failures keep the uncertain input and one original plan.
/// Early retries are refused, a foreign lower service cannot consume that input,
/// and consecutive completed failures schedule1s then2s rather than catch-up.
/// Removing the actual storage fault then lets the same original commit once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_storage_failures_back_off_and_foreign_retry_cannot_consume_input() {
    let fixture = Fixture::new(true);
    let root = fixture.root();
    root.observe().await.unwrap();
    let connection = rusqlite::Connection::open(fixture.path.join("custody.sqlite")).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_marker BEFORE INSERT ON records WHEN NEW.key LIKE '%:commit' BEGIN SELECT RAISE(ABORT,'fixture marker failure'); END;").unwrap();
    let (mut service, sender, target, _) = fixture_service();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "original"),
            MessageScope::Session,
            100,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &ticket, None).unwrap();
    let CustodyWorkerStatus::RetryAt(first) = root.observe().await.unwrap() else {
        panic!("expected failure")
    };
    let original_attempt = service.acceptance_write_attempt(&ticket).unwrap();
    let (mut foreign, _, _, _) = fixture_service();
    assert!(root.retry(&mut foreign, first).is_err());
    assert!(
        service
            .validate_acceptance_attempt(&original_attempt)
            .is_ok()
    );
    assert!(root.retry(&mut service, first).unwrap());
    assert!(
        service
            .validate_acceptance_attempt(&original_attempt)
            .is_err()
    );
    let before = Instant::now();
    let CustodyWorkerStatus::RetryAt(second) = root.observe().await.unwrap() else {
        panic!("expected second failure")
    };
    assert!(second >= before + Duration::from_secs(2));
    assert!(
        !root
            .retry(&mut service, second - Duration::from_nanos(1))
            .unwrap()
    );
    connection
        .execute_batch("DROP TRIGGER fail_marker")
        .unwrap();
    let before = fixture.disk();
    assert!(root.retry(&mut service, second).unwrap());
    root.observe().await.unwrap();
    let after = fixture.disk();
    assert_eq!(after.0, before.0 + 1);
    assert_eq!(after.1, before.1 + 1);
    root.adopt(&mut service, None).unwrap();
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 3);
}

/// Settling a first original releases one store for a second occurrence using
/// the actual off-actor-observed generation. Root reuse never starts another
/// message while the first committed positive fact still awaits adoption.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consecutive_originals_advance_store_only_after_exact_actor_adoption() {
    let fixture = Fixture::new(true);
    let root = fixture.root();
    root.observe().await.unwrap();
    let (mut service, sender, target, _) = fixture_service();
    for id in ["first", "second"] {
        let ticket = service
            .prepare_acceptance(
                &sender.agent_id,
                envelope(&sender, &target, id),
                MessageScope::Session,
                100,
                4096,
                None,
            )
            .unwrap();
        root.start(&mut service, &ticket, None).unwrap();
        root.observe().await.unwrap();
        assert!(root.start(&mut service, &ticket, None).is_err());
        assert_eq!(
            root.adopt(&mut service, None).unwrap().sequence,
            ticket.sequence()
        );
    }
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
}

/// Concurrent observers share one cancellation-safe observation slot. Cancelling
/// its current holder wakes the next observer, which joins the same actual
/// worker rather than losing readiness through overwritten task wakers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_observer_takes_over_cancelled_join_observation_without_lost_wake() {
    let fixture = Fixture::new(false);
    let root = fixture.root();
    root.observe().await.unwrap();
    let (mut service, sender, target, _) = fixture_service();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "one"),
            MessageScope::Session,
            100,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &ticket, None).unwrap();
    fixture.probe.started.notified().await;
    let first_root = root.clone();
    let first = tokio::spawn(async move { first_root.observe().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if root.0.observer.try_lock().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let second_root = root.clone();
    let second = tokio::spawn(async move { second_root.observe().await });
    tokio::task::yield_now().await;
    first.abort();
    let _ = first.await;
    fixture.probe.release();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    root.adopt(&mut service, None).unwrap();
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
}

/// Exponential spacing saturates at30s even after arbitrarily many failures.
/// Each root owns one failure counter and schedules relative to observed failure,
/// independent of wall-clock changes and without a catch-up retry budget.
#[test]
fn retry_spacing_has_exact_bounded_exponential_schedule() {
    let seconds: Vec<_> = (1..=8)
        .map(|failures| retry_delay(failures).as_secs())
        .collect();
    assert_eq!(seconds, [1, 2, 4, 8, 16, 30, 30, 30]);
    assert_eq!(retry_delay(u32::MAX), Duration::from_secs(30));
}

/// A tracked ticket cannot be issued through an untracked path or a foreign
/// request owner. These failures happen before lower issuance, leaving the exact
/// original paired reservation cancellable and no physical worker running.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracked_start_requires_original_prepared_request_owner() {
    let fixture = Fixture::new(true);
    let root = fixture.root();
    root.observe().await.unwrap();
    let (mut service, sender, target, origin) = fixture_service();
    let mut requests = MessageRequests::default();
    let ticket = requests
        .prepare_acceptance(
            &mut service,
            origin,
            envelope(&sender, &target, "tracked"),
            MessageScope::Session,
            100,
        )
        .unwrap();
    assert!(root.start(&mut service, &ticket, None).is_err());
    let foreign = MessageRequests::default();
    assert!(root.start(&mut service, &ticket, Some(&foreign)).is_err());
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 0);
    requests
        .cancel_prepared_acceptance(&mut service, &ticket)
        .unwrap();
    assert_eq!(requests.usage(), (0, 0, 0));
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
}

/// A failed actual private-path open is retained as unavailable with diagnostic
/// context and cannot consume a lower write. It is setup failure, not a joined
/// send refusal, and no worker retry may manufacture lower acceptance from it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_private_open_is_unavailable_without_issuing_original() {
    let fixture = Fixture::new(true);
    std::fs::create_dir(&fixture.path).unwrap();
    std::fs::set_permissions(
        &fixture.path,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let root = fixture.root();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::Unavailable
    );
    assert!(root.failure().unwrap().is_some());
    let (mut service, sender, target, _) = fixture_service();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "one"),
            MessageScope::Session,
            1,
            4096,
            None,
        )
        .unwrap();
    assert!(root.start(&mut service, &ticket, None).is_err());
    service.cancel_acceptance(&ticket).unwrap();
    assert!(
        !root
            .retry(&mut service, Instant::now() + Duration::from_secs(60))
            .unwrap()
    );
}

/// A same-label replacement private store after actual postcommit worker loss
/// cannot inherit the original plan. Retry stays uncertain, writes no new row,
/// and preserves the old original's actual bytes at its separately retained path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replacement_store_after_actual_worker_loss_cannot_adopt_original_plan() {
    let fixture = Fixture::new(true);
    let retained = Fixture::new(true);
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    let root = fixture.root();
    root.observe().await.unwrap();
    let (mut service, sender, target, _) = fixture_service();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "original"),
            MessageScope::Session,
            100,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &ticket, None).unwrap();
    let CustodyWorkerStatus::RetryAt(due) = root.observe().await.unwrap() else {
        panic!("expected actual failed join")
    };
    let original = fixture.disk();
    std::fs::rename(&fixture.path, &retained.path).unwrap();
    drop(MmpCustodyStore::open(&fixture.path, "original-session").unwrap());
    assert_eq!(fixture.disk(), (0, 0));
    assert!(root.retry(&mut service, due).unwrap());
    assert!(matches!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RetryAt(_)
    ));
    assert_eq!(fixture.disk(), (0, 0));
    assert_eq!(retained.disk(), original);
    assert!(root.failure().unwrap().is_some());
    assert!(root.adopt(&mut service, None).is_err());
    assert!(service.has_unpublished_acceptance());
}
