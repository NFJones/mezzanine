//! Real ACK jobs on the shared session root, with actual blocking gates/joins.
//! These fixtures exercise lower/store custody, not native application execution;
//! production callers must independently prove canonical input commit first.

use super::*;
use mez_agent::messaging::{Envelope, MessageScope, Recipient, SenderIdentity};
use std::sync::atomic::AtomicUsize;

/// ACK-only physical controls leave original acceptance setup ungated.
pub(in crate::host::async_runtime::mmp_custody) struct Probe {
    /// Announces actual ACK worker entry, independent of observer polling.
    started: tokio::sync::Notify,
    /// Actual blocking-worker Condvar, never held by actor/provider threads.
    gate: (Mutex<bool>, std::sync::Condvar),
    /// Counts physical ACK invocations only, not acceptance/opening.
    attempts: AtomicUsize,
    /// One-shot actual loss after ACK commit before the root receives its fact.
    panic_after_commit: AtomicBool,
    /// Bounded transient failures while obtaining the guarded ACK's next position.
    anchor_failures: AtomicUsize,
    /// One real worker loss during a position-only refresh.
    panic_anchor_refresh: AtomicBool,
}
impl Probe {
    /// Creates a retained physical ACK gate with optional immediate release.
    pub(in crate::host::async_runtime::mmp_custody) fn new(released: bool) -> Arc<Self> {
        Arc::new(Self {
            started: tokio::sync::Notify::new(),
            gate: (Mutex::new(released), std::sync::Condvar::new()),
            attempts: AtomicUsize::new(0),
            panic_after_commit: AtomicBool::new(false),
            anchor_failures: AtomicUsize::new(0),
            panic_anchor_refresh: AtomicBool::new(false),
        })
    }
    /// Loses the next real committed ACK before its owner receives the result.
    pub(in crate::host::async_runtime::mmp_custody) fn lose_next_commit(&self) {
        self.panic_after_commit.store(true, Ordering::SeqCst);
    }
    /// Injects post-commit position-read failures without losing the ACK.
    pub(in crate::host::async_runtime::mmp_custody) fn fail_anchors(&self, count: usize) {
        self.anchor_failures.store(count, Ordering::SeqCst);
    }
    /// Consumes one injected position-read failure on the real worker.
    pub(super) fn take_anchor_failure(&self) -> bool {
        let mut remaining = self.anchor_failures.load(Ordering::SeqCst);
        loop {
            let Some(next) = remaining.checked_sub(1) else {
                return false;
            };
            match self.anchor_failures.compare_exchange_weak(
                remaining,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(actual) => remaining = actual,
            }
        }
    }
    /// Reports actual physical ACK writes, excluding position-only checks.
    pub(in crate::host::async_runtime::mmp_custody) fn physical_attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }
    /// Loses the next actual position-only worker before it returns its input.
    pub(in crate::host::async_runtime::mmp_custody) fn lose_next_anchor_refresh(&self) {
        self.panic_anchor_refresh.store(true, Ordering::SeqCst);
    }
    /// Consumes one injected position-refresh worker loss.
    pub(super) fn take_anchor_refresh_panic(&self) -> bool {
        self.panic_anchor_refresh.swap(false, Ordering::SeqCst)
    }
    /// Blocks only the actual ACK worker, not its abortable observer.
    pub(super) fn before_write(&self) {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        let mut released = self.gate.0.lock().unwrap();
        while !*released {
            released = self.gate.1.wait(released).unwrap();
        }
    }
    /// Loses an actual committed fact before its root callback, with real panic.
    pub(super) fn after_write(&self, outcome: &MessageAcknowledgementWriteOutcome) {
        if matches!(outcome, MessageAcknowledgementWriteOutcome::Committed(_))
            && self.panic_after_commit.swap(false, Ordering::SeqCst)
        {
            panic!("injected actual ACK worker loss after commit");
        }
    }
    /// Releases this generated worker even during assertion-unwind cleanup.
    fn release(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }
}

/// Owns only a generated private destination and its independent ACK gate.
struct Fixture {
    path: PathBuf,
    probe: Arc<Probe>,
}
impl Fixture {
    /// Creates an initially absent private destination with an actual ACK gate.
    fn new(released: bool) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "mez-mmp-root-ack-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            )),
            probe: Probe::new(released),
        }
    }
    /// Registers actual root opening before any accepted source or ACK starts.
    fn root(&self) -> MmpCustodyRoot {
        MmpCustodyRoot::construct(
            Destination {
                root: self.path.clone(),
                owner: "session".into(),
                limits: CustodyStoreLimits::default(),
            },
            Handle::current(),
            None,
            Some(self.probe.clone()),
        )
    }
    /// Reads actual SQLite generation/count rather than monitor status text.
    fn disk(&self) -> (i64, i64) {
        self.connection()
            .query_row(
                "SELECT generation,(SELECT COUNT(*) FROM records) FROM state",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }
    /// Opens only this fixture's real database for fault injection/assertions.
    fn connection(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.path.join("custody.sqlite")).unwrap()
    }
}
impl Drop for Fixture {
    /// Releases the physical gate and removes only this generated private root.
    fn drop(&mut self) {
        self.probe.release();
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Creates a real immutable source for private acceptance and ACK qualification.
fn envelope(sender: &SenderIdentity, target: &SenderIdentity, id: &str) -> Envelope {
    Envelope {
        protocol: "mmp/1",
        id: id.into(),
        message_type: "send".into(),
        time: "original".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "original\0payload".repeat(20_000),
        extension_fields: vec![],
    }
}

/// Persists/adopts a source through the same root, then genuinely offers it. This
/// setup uses no memory-only acceptance or second independent store connection.
async fn prepared(
    root: &MmpCustodyRoot,
) -> (
    MessageService,
    SenderIdentity,
    SenderIdentity,
    MessageAcknowledgementTicket,
) {
    assert_eq!(root.observe().await.unwrap(), CustodyWorkerStatus::Idle);
    let mut service = MessageService::with_limits(16, 4 * 1024 * 1024);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "original"),
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &ticket, None).unwrap();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AwaitingAdoption
    );
    root.adopt(&mut service, None).unwrap();
    service
        .receive_subscribed(&target.agent_id, 20, 100)
        .unwrap();
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, 1, 512 * 1024)
        .unwrap()
        .unwrap();
    (service, sender, target, ticket)
}

/// A historical ACK may reconcile without a write. An unrelated batch after
/// that reconciliation must not qualify as its post-ACK store position.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guarded_duplicate_cannot_rebase_across_later_unrelated_batch() {
    use crate::storage::mmp_custody::CustodyWrite;
    use mez_agent::messaging::MessageAcknowledgementStore;

    let fixture = Fixture::new(true);
    let root = fixture.root();
    let (mut service, _, _, ticket) = prepared(&root).await;
    let write = service.begin_acknowledgement_write(&ticket).unwrap();
    let fence = root.0.ownership.lock().unwrap().as_ref().unwrap().clone();
    let mut state = root.0.state.lock().unwrap();
    let State::Idle(opened) = &mut *state else {
        panic!("root not idle")
    };
    let original = opened.store.acknowledgement_plan().unwrap();
    opened
        .store
        .acknowledgement_port(&original)
        .commit(&write)
        .unwrap();
    let plan = opened.store.acknowledgement_plan().unwrap();
    let suspended = opened.store.acceptance_plan(2).unwrap();
    let settlement = {
        let mut port = opened.store.acknowledgement_port_guarded(&plan, &suspended);
        port.commit(&write).unwrap();
        port.take_guarded_settlement().unwrap()
    };
    let connection = fixture.connection();
    let batch: Vec<u8> = connection
        .query_row("SELECT batch FROM state WHERE singleton=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    connection
        .execute("UPDATE state SET batch=zeroblob(32) WHERE singleton=1", [])
        .unwrap();
    assert!(
        verified_parked_anchor(&fence, &mut opened.store, &suspended, &settlement).is_err(),
        "same-generation batch corruption cannot qualify the captured position"
    );
    connection
        .execute("UPDATE state SET batch=?1 WHERE singleton=1", [&batch])
        .unwrap();
    drop(connection);
    let position = opened.store.acceptance_plan(0).unwrap();
    opened
        .store
        .commit(
            position.generation(),
            &[CustodyWrite {
                key: "unrelated",
                previous: None,
                value: Some(b"later"),
            }],
        )
        .unwrap();
    assert!(
        verified_parked_anchor(&fence, &mut opened.store, &suspended, &settlement).is_err(),
        "unrelated batch cannot advance the parked original's anchor"
    );
}

/// Dropping the final root while a guarded ACK worker is physically blocked
/// must retain the suspended original until that actual worker has terminated.
/// The test-only weak witness tracks ownership rather than a status label.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_root_retains_suspended_original_until_ack_worker_joins() {
    let fixture = Fixture::new(false);
    let root = fixture.root();
    let (mut service, sender, target, ack) = prepared(&root).await;
    let mut foreign = MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
    let foreign_ack = foreign
        .prepare_acknowledgement(&target.agent_id, 1, 512 * 1024)
        .unwrap()
        .unwrap();
    let records = fixture.disk().1 as usize;
    {
        let mut state = root.0.state.lock().unwrap();
        let State::Idle(opened) = &mut *state else {
            panic!("root not idle")
        };
        opened.store.set_test_record_limit(records + 1);
    }
    let second = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "later"),
            MessageScope::Session,
            30,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &second, None).unwrap();
    assert!(matches!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::RetryAt(_)
    ));
    assert!(root.can_yield_acknowledgement().unwrap());
    assert!(
        root.start_acknowledgement(&mut foreign, &foreign_ack)
            .is_err(),
        "a different service cannot take the lane holding the original acceptance"
    );
    foreign.cancel_acknowledgement(&foreign_ack).unwrap();
    root.start_acknowledgement(&mut service, &ack).unwrap();
    fixture.probe.started.notified().await;
    let witness = Arc::new(());
    let weak = Arc::downgrade(&witness);
    {
        let mut state = root.0.state.lock().unwrap();
        let State::Acknowledging { admission, .. } = &mut *state else {
            panic!("ACK not owned")
        };
        admission.parked.as_mut().unwrap().drop_witness = Some(witness);
    }
    drop(root);
    drop(service);
    tokio::task::yield_now().await;
    assert!(
        weak.upgrade().is_some(),
        "suspended original escaped while ACK was running"
    );
    fixture.probe.release();
    tokio::time::timeout(Duration::from_secs(10), async {
        while weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

/// Observer cancellation cannot detach the gated physical ACK or free its charge.
/// Acceptance cannot overtake it; after retirement its exact positive applies
/// without recreating a subscription and remains owned through wrong adopters.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_ack_observer_keeps_shared_lane_until_original_adoption() {
    let fixture = Fixture::new(false);
    let root = fixture.root();
    let (mut service, sender, target, ticket) = prepared(&root).await;
    root.start_acknowledgement(&mut service, &ticket).unwrap();
    fixture.probe.started.notified().await;
    let observer_root = root.clone();
    let observer = tokio::spawn(async move { observer_root.observe().await });
    tokio::task::yield_now().await;
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    assert!(service.cancel_acknowledgement(&ticket).is_err());
    let next = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "cannot-overtake"),
            MessageScope::Session,
            21,
            4096,
            None,
        )
        .unwrap();
    assert!(root.start(&mut service, &next, None).is_err());
    service.cancel_acceptance(&next).unwrap();
    assert_eq!(tokio::spawn(async { 23 }).await.unwrap(), 23);
    service.retire_agent_identity(&target.agent_id);
    assert_eq!(service.settle_delivery_outcomes_at(100).unwrap(), 0);
    root.request_stop();
    fixture.probe.release();
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AcknowledgementAwaitingAdoption
    );
    let mut foreign = MessageService::default();
    assert!(root.adopt_acknowledgement(&mut foreign).is_err());
    assert!(root.adopt(&mut service, None).is_err());
    assert_eq!(
        root.adopt_acknowledgement(&mut service)
            .unwrap()
            .last_sequence,
        1
    );
    assert!(service.subscription(&target.agent_id).is_none());
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
}

/// Stop closes new sends while permitting control settlement for an already
/// accepted/application-committed original. ACK uses the real settled anchor and
/// cannot be confused with another acceptance or canonical native effect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopped_root_allows_existing_accepted_input_ack_drain() {
    let fixture = Fixture::new(true);
    let root = fixture.root();
    let (mut service, sender, target, ticket) = prepared(&root).await;
    root.request_stop();
    let before = fixture.disk();
    root.start_acknowledgement(&mut service, &ticket).unwrap();
    root.observe().await.unwrap();
    assert_eq!(fixture.disk(), (before.0 + 1, before.1 + 1));
    assert_eq!(
        root.adopt_acknowledgement(&mut service)
            .unwrap()
            .last_sequence,
        1
    );
    let next = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "stopped-send"),
            MessageScope::Session,
            30,
            4096,
            None,
        )
        .unwrap();
    assert!(root.start(&mut service, &next, None).is_err());
    service.cancel_acceptance(&next).unwrap();
}

/// Actual postcommit worker panic is observed through its retained handle. The
/// original ACK plan/input is recovered only after that join and monotonic delay,
/// then reconciles without a second journal row, generation or input injection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_lost_ack_worker_reopens_same_original_and_never_recommits() {
    let fixture = Fixture::new(true);
    let root = fixture.root();
    let (mut service, _sender, target, ticket) = prepared(&root).await;
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    root.start_acknowledgement(&mut service, &ticket).unwrap();
    let before = Instant::now();
    let CustodyWorkerStatus::AcknowledgementRetryAt(due) = root.observe().await.unwrap() else {
        panic!("expected actual joined loss")
    };
    assert!(due >= before + Duration::from_secs(1));
    let disk = fixture.disk();
    assert!(
        !root
            .retry_acknowledgement(&mut service, due - Duration::from_nanos(1))
            .unwrap()
    );
    let mut foreign = MessageService::default();
    assert!(root.retry_acknowledgement(&mut foreign, due).is_err());
    root.request_stop();
    assert!(root.retry_acknowledgement(&mut service, due).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AcknowledgementAwaitingAdoption
    );
    assert_eq!(fixture.disk(), disk);
    assert_eq!(
        root.adopt_acknowledgement(&mut service)
            .unwrap()
            .last_sequence,
        1
    );
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        1
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
}

/// A real SQLite refusal preserves Unknown and original nonce/input until retry.
/// Foreign attempts cannot consume it; spacing is1s then2s from actual observed
/// failures. Recovery commits one original ACK after the storage fault is removed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_ack_failure_backs_off_and_foreign_retry_preserves_original_input() {
    let fixture = Fixture::new(true);
    let root = fixture.root();
    let (mut service, _sender, target, ticket) = prepared(&root).await;
    let connection = fixture.connection();
    connection.execute_batch("CREATE TRIGGER fail_ack BEFORE INSERT ON records WHEN NEW.key LIKE 'k1:%' BEGIN SELECT RAISE(ABORT,'fixture ACK failure'); END;").unwrap();
    let disk = fixture.disk();
    root.start_acknowledgement(&mut service, &ticket).unwrap();
    let CustodyWorkerStatus::AcknowledgementRetryAt(first) = root.observe().await.unwrap() else {
        panic!("expected real failure")
    };
    assert_eq!(fixture.disk(), disk);
    assert!(root.failure().unwrap().is_some());
    let original = match &*root.0.state.lock().unwrap() {
        State::Acknowledged { admission, .. } => admission.attempt.clone(),
        _ => panic!("expected joined ACK"),
    };
    let mut foreign = MessageService::default();
    assert!(root.retry_acknowledgement(&mut foreign, first).is_err());
    assert!(service.validate_acknowledgement_attempt(&original).is_ok());
    assert!(root.retry_acknowledgement(&mut service, first).unwrap());
    assert!(service.validate_acknowledgement_attempt(&original).is_err());
    let before = Instant::now();
    let CustodyWorkerStatus::AcknowledgementRetryAt(second) = root.observe().await.unwrap() else {
        panic!("expected second real failure")
    };
    assert!(second >= before + Duration::from_secs(2));
    connection.execute_batch("DROP TRIGGER fail_ack").unwrap();
    assert!(root.retry_acknowledgement(&mut service, second).unwrap());
    root.observe().await.unwrap();
    assert_eq!(fixture.disk(), (disk.0 + 1, disk.1 + 1));
    root.adopt_acknowledgement(&mut service).unwrap();
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        1
    );
}

/// A busy ACK root refuses another prepared original before issuing its lower
/// write. That unrelated never-issued ticket stays cancellable, while no second
/// physical ACK worker or implicit queue has been created behind the root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_shared_root_does_not_issue_another_ack() {
    let fixture = Fixture::new(false);
    let root = fixture.root();
    let (mut service, _sender, _target, ticket) = prepared(&root).await;
    root.start_acknowledgement(&mut service, &ticket).unwrap();
    fixture.probe.started.notified().await;
    let mut other = MessageService::default();
    let sender = other.register_agent(None, None, "sender", vec![]);
    let target = other.register_agent(None, None, "target", vec![]);
    other.subscribe(&target.agent_id).unwrap();
    let sequence = other
        .accept_at_with_scope(
            &sender.agent_id,
            envelope(&sender, &target, "other"),
            MessageScope::Session,
            1,
        )
        .unwrap()
        .sequence;
    other.receive_subscribed(&target.agent_id, 2, 1).unwrap();
    let ticket = other
        .prepare_acknowledgement(&target.agent_id, sequence, 512 * 1024)
        .unwrap()
        .unwrap();
    assert!(root.start_acknowledgement(&mut other, &ticket).is_err());
    other.cancel_acknowledgement(&ticket).unwrap();
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
    fixture.probe.release();
    root.observe().await.unwrap();
    root.adopt_acknowledgement(&mut service).unwrap();
}

/// Acceptance and ACK reuse only the actual off-worker settled position after
/// predecessor adoption. Two sequences can each commit then ACK exactly once,
/// with no stale generation caused by crossing distinct worker owners.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accept_ack_accept_ack_share_one_settled_connection_anchor() {
    let fixture = Fixture::new(true);
    let root = fixture.root();
    let (mut service, sender, target, ticket) = prepared(&root).await;
    root.start_acknowledgement(&mut service, &ticket).unwrap();
    root.observe().await.unwrap();
    root.adopt_acknowledgement(&mut service).unwrap();
    let next = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope(&sender, &target, "second"),
            MessageScope::Session,
            30,
            4096,
            None,
        )
        .unwrap();
    root.start(&mut service, &next, None).unwrap();
    root.observe().await.unwrap();
    assert_eq!(root.adopt(&mut service, None).unwrap().sequence, 2);
    service
        .receive_subscribed(&target.agent_id, 40, 10)
        .unwrap();
    let next = service
        .prepare_acknowledgement(&target.agent_id, 2, 512 * 1024)
        .unwrap()
        .unwrap();
    root.start_acknowledgement(&mut service, &next).unwrap();
    root.observe().await.unwrap();
    assert_eq!(
        root.adopt_acknowledgement(&mut service)
            .unwrap()
            .last_sequence,
        2
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 2);
}

/// The single observer slot remains cancellation-safe for ACK handles too. A
/// queued observer takes over the same actual join after its holder is aborted,
/// rather than losing readiness through another task's overwritten waker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_ack_observer_takes_over_cancelled_actual_join_observer() {
    let fixture = Fixture::new(false);
    let root = fixture.root();
    let (mut service, _sender, _target, ticket) = prepared(&root).await;
    root.start_acknowledgement(&mut service, &ticket).unwrap();
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
    first.abort();
    let _ = first.await;
    fixture.probe.release();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        CustodyWorkerStatus::AcknowledgementAwaitingAdoption
    );
    root.adopt_acknowledgement(&mut service).unwrap();
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 1);
}

/// A same-label replacement after actual worker loss cannot inherit the ACK.
/// Restoring the original allocation later allows a bounded retry to reopen it
/// under the retained plan, instead of retaining a known-foreign connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_ack_plan_rejects_replacement_then_recovers_returned_original_store() {
    let fixture = Fixture::new(true);
    let retained = Fixture::new(true);
    let replaced = Fixture::new(true);
    let root = fixture.root();
    let (mut service, _sender, target, ticket) = prepared(&root).await;
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    root.start_acknowledgement(&mut service, &ticket).unwrap();
    let CustodyWorkerStatus::AcknowledgementRetryAt(due) = root.observe().await.unwrap() else {
        panic!("expected actual failed join")
    };
    let original = fixture.disk();
    std::fs::rename(&fixture.path, &retained.path).unwrap();
    drop(MmpCustodyStore::open(&fixture.path, "session").unwrap());
    assert_eq!(fixture.disk(), (0, 0));
    assert!(root.retry_acknowledgement(&mut service, due).unwrap());
    let CustodyWorkerStatus::AcknowledgementRetryAt(due) = root.observe().await.unwrap() else {
        panic!("expected foreign allocation refusal")
    };
    assert_eq!(fixture.disk(), (0, 0));
    assert_eq!(retained.disk(), original);
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    std::fs::rename(&fixture.path, &replaced.path).unwrap();
    std::fs::rename(&retained.path, &fixture.path).unwrap();
    assert!(root.retry_acknowledgement(&mut service, due).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AcknowledgementAwaitingAdoption
    );
    assert_eq!(fixture.disk(), original);
    assert_eq!(replaced.disk(), (0, 0));
    root.adopt_acknowledgement(&mut service).unwrap();
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        1
    );
    assert_eq!(fixture.probe.attempts.load(Ordering::SeqCst), 3);
}

/// A copied database includes the exact ACK, source bytes and allocation ID,
/// but it does not include the original root's live lock. After a real lost
/// ACK worker result, only restoring the original directory may qualify the
/// retained ACK and advance its original recipient cursor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copied_database_cannot_transfer_original_ack_ownership() {
    let fixture = Fixture::new(true);
    let retained = Fixture::new(true);
    let replaced = Fixture::new(true);
    let root = fixture.root();
    let (mut service, _sender, target, ticket) = prepared(&root).await;
    fixture
        .probe
        .panic_after_commit
        .store(true, Ordering::SeqCst);
    root.start_acknowledgement(&mut service, &ticket).unwrap();
    let CustodyWorkerStatus::AcknowledgementRetryAt(due) = root.observe().await.unwrap() else {
        panic!("expected actual failed join")
    };
    let original = fixture.disk();
    std::fs::rename(&fixture.path, &retained.path).unwrap();
    drop(MmpCustodyStore::open(&fixture.path, "session").unwrap());
    std::fs::copy(
        retained.path.join("custody.sqlite"),
        fixture.path.join("custody.sqlite"),
    )
    .unwrap();
    assert_eq!(fixture.disk(), original);
    assert!(root.retry_acknowledgement(&mut service, due).unwrap());
    let CustodyWorkerStatus::AcknowledgementRetryAt(due) = root.observe().await.unwrap() else {
        panic!("copied database must not transfer original lock ownership")
    };
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    assert!(root.adopt_acknowledgement(&mut service).is_err());
    std::fs::rename(&fixture.path, &replaced.path).unwrap();
    std::fs::rename(&retained.path, &fixture.path).unwrap();
    assert!(root.retry_acknowledgement(&mut service, due).unwrap());
    assert_eq!(
        root.observe().await.unwrap(),
        CustodyWorkerStatus::AcknowledgementAwaitingAdoption
    );
    root.adopt_acknowledgement(&mut service).unwrap();
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        1
    );
    assert_eq!(fixture.disk(), original);
    assert_eq!(replaced.disk(), original);
}

mod actor;
