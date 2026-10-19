//! Deterministic lifecycle/stream regressions: no subprocesses or native signals.
//!
//! The fake kernel owns one immutable birth token and keeps a distinct unrelated
//! survivor. Reap attempts and signals are journaled, so moving reap ahead of the
//! final signal decision fails independently of numeric PID reuse or timing.
//! These tests qualify the shared driver, not Linux syscall adapter semantics.

use std::cell::{Cell, RefCell};
use std::os::unix::process::ExitStatusExt;
use std::rc::Rc;

use super::fixture_policy::{FixturePolicy, Settlement};
use super::*;

const BIRTH: u64 = 17;
const UNRELATED: u64 = 23;

/// Semantic events include exact ownership, not a status-reported signal PID.
#[derive(Debug, PartialEq, Eq)]
enum Event {
    Acquired(usize),
    Observe(u64),
    Signal(u64, bool),
    Reap(u64),
    Pump,
    Pause(Duration),
}

/// One owned fixture, its virtual clock, and independently injectable failures.
struct Fake {
    journal: Rc<RefCell<Vec<Event>>>,
    now: Instant,
    exited: bool,
    observe_error: Option<i32>,
    setup_failure: Option<usize>,
    setup_panic: Option<usize>,
    signal_errors: bool,
    reap_pending: bool,
    reap_error: bool,
    io_pending: bool,
    io_error: bool,
    io_panic: bool,
    reaped: bool,
    setup_age: Duration,
    observe_age: Duration,
    pump_age: Duration,
    reap_age: Duration,
}

impl Fake {
    /// Supplies a successful exited root; tests override one failure at a time.
    fn new() -> Self {
        Self {
            journal: Rc::default(),
            now: Instant::now(),
            exited: true,
            observe_error: None,
            setup_failure: None,
            setup_panic: None,
            signal_errors: false,
            reap_pending: false,
            reap_error: false,
            io_pending: false,
            io_error: false,
            io_panic: false,
            reaped: false,
            setup_age: Duration::ZERO,
            observe_age: Duration::ZERO,
            pump_age: Duration::ZERO,
            reap_age: Duration::ZERO,
        }
    }

    /// Returns a driver and shared journal, with a ten-tick original budget.
    fn owner(self) -> (Owner<Self>, Rc<RefCell<Vec<Event>>>) {
        let deadline = self.now + TICK * 10;
        let journal = Rc::clone(&self.journal);
        (Owner::new(self, deadline), journal)
    }

    /// Records one bounded fake operation, never invoking the operating system.
    fn record(&self, event: Event) {
        self.journal.borrow_mut().push(event);
    }
}

impl Workload for Fake {
    fn setup(&mut self) -> io::Result<()> {
        self.now += self.setup_age;
        // Models taking stdout/stderr/stdin and setting each descriptor flag.
        for stage in 0..6 {
            self.record(Event::Acquired(stage));
            assert_ne!(self.setup_panic, Some(stage), "injected setup unwind");
            if self.setup_failure == Some(stage) {
                return Err(io::Error::other(format!("setup stage {stage}")));
            }
        }
        Ok(())
    }

    fn now(&self) -> Instant {
        self.now
    }

    fn pause(&mut self, duration: Duration) {
        self.record(Event::Pause(duration));
        self.now += duration;
    }

    fn exited(&mut self) -> io::Result<bool> {
        self.record(Event::Observe(BIRTH));
        self.now += std::mem::take(&mut self.observe_age);
        match self.observe_error {
            Some(code) => Err(io::Error::from_raw_os_error(code)),
            None => Ok(self.exited),
        }
    }

    fn signal_group(&mut self) -> io::Result<()> {
        assert!(!self.reaped, "group signal after reap");
        self.record(Event::Signal(BIRTH, true));
        if self.signal_errors {
            Err(io::Error::other("group denied"))
        } else {
            Ok(())
        }
    }

    fn signal_root(&mut self) -> io::Result<()> {
        assert!(!self.reaped, "root signal after reap");
        self.record(Event::Signal(BIRTH, false));
        if self.signal_errors {
            Err(io::Error::other("root denied"))
        } else {
            Ok(())
        }
    }

    fn reap(&mut self) -> io::Result<Option<ExitStatus>> {
        self.record(Event::Reap(BIRTH));
        self.now += std::mem::take(&mut self.reap_age);
        if self.reap_error {
            return Err(io::Error::other("reap denied"));
        }
        if self.reap_pending {
            return Ok(None);
        }
        assert!(!self.reaped, "duplicate actual reap");
        self.reaped = true;
        Ok(Some(ExitStatus::from_raw(0)))
    }

    fn pump(&mut self) -> io::Result<bool> {
        self.record(Event::Pump);
        self.now += self.pump_age;
        assert!(!self.io_panic, "injected I/O unwind");
        if self.io_error {
            Err(io::Error::other("capture failed"))
        } else {
            Ok(!self.io_pending)
        }
    }
}

/// Proves all signals precede the first actual wait and only target the owner.
fn assert_order(events: &[Event]) {
    let first_reap = events
        .iter()
        .position(|event| matches!(event, Event::Reap(_)))
        .unwrap();
    let signals: Vec<_> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            if let Event::Signal(owner, group) = event {
                assert_eq!(*owner, BIRTH);
                assert_ne!(*owner, UNRELATED, "unrelated survivor was targeted");
                assert!(index < first_reap, "signal after reap: {events:?}");
                Some(*group)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        signals,
        [true, false],
        "final signal decision must happen once"
    );
}

#[test]
/// Successful root exit must remain unreaped through group cleanup, even after
/// collection, repeated finalization attempts, and Drop. This is the direct
/// regression for the old wait/reap-then-kill ordering, without PID-reuse stress.
fn exited_root_signals_before_reap_and_never_after() {
    let (mut owner, journal) = Fake::new().owner();
    assert!(owner.collect().unwrap().success());
    assert_eq!(owner.authority, Authority::Reaped);
    owner.finalize();
    owner.finalize();
    drop(owner);
    assert_order(&journal.borrow());
}

#[test]
/// A running root uses the same original deadline for the final signal and
/// nonblocking reap; cleanup must not grant another ten-tick timeout.
fn timeout_preserves_original_deadline_and_exact_owner() {
    let mut fake = Fake::new();
    fake.exited = false;
    let (mut owner, journal) = fake.owner();
    let deadline = owner.deadline;
    assert!(
        owner
            .collect()
            .unwrap_err()
            .contains("original deadline exhausted")
    );
    assert_eq!(owner.backend.now, deadline);
    drop(owner);
    assert_order(&journal.borrow());
}

#[test]
/// Every partial pipe acquisition/flag failure is already guarded. It retains
/// the original primary error while making exactly one final signal decision.
fn every_partial_setup_failure_has_cleanup_ownership() {
    for stage in 0..6 {
        let mut fake = Fake::new();
        fake.setup_failure = Some(stage);
        let (mut owner, journal) = fake.owner();
        assert!(
            owner
                .collect()
                .unwrap_err()
                .contains(&format!("setup stage {stage}"))
        );
        drop(owner);
        assert_order(&journal.borrow());
    }
}

#[test]
/// Panics during partial setup or capture use the same Drop path; no reader
/// thread or unguarded Child escapes the unwind, and reaping still comes last.
fn setup_and_io_unwind_preserve_final_signal_order() {
    for stage in 0..7 {
        let mut fake = Fake::new();
        fake.exited = false;
        if stage < 6 {
            fake.setup_panic = Some(stage);
        } else {
            fake.io_panic = true;
        }
        let (mut owner, journal) = fake.owner();
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || owner.collect()));
        assert!(result.is_err());
        assert_order(&journal.borrow());
    }
}

#[test]
/// Primary capture failure and both secondary signal failures remain visible;
/// failed signaling cannot create authority to retry after the actual reap.
fn capture_and_secondary_signal_errors_are_not_hidden() {
    let mut fake = Fake::new();
    fake.exited = false;
    fake.io_error = true;
    fake.signal_errors = true;
    let (mut owner, journal) = fake.owner();
    let error = owner.collect().unwrap_err();
    for expected in ["capture failed", "group denied", "root denied"] {
        assert!(error.contains(expected), "{error}");
    }
    drop(owner);
    assert_order(&journal.borrow());
}

#[test]
/// An unreapable root at deadline is an explicit error, not successful cleanup.
/// Explicit retirement tries one last nonblocking reap; later Drop is inert.
fn pending_reap_is_truthful_and_never_extends_budget() {
    let mut fake = Fake::new();
    fake.reap_pending = true;
    let (mut owner, journal) = fake.owner();
    let deadline = owner.deadline;
    assert!(owner.collect().unwrap_err().contains("root still unreaped"));
    assert_eq!(owner.backend.now, deadline);
    drop(owner);
    assert_order(&journal.borrow());
    let events = journal.borrow();
    assert!(matches!(events.last(), Some(Event::Reap(BIRTH))));
}

#[test]
/// Non-EOF capture remains bounded after successful root reap (for example an
/// escaped pipe holder); no fresh drain budget or post-reap signal is allowed.
fn non_eof_capture_stops_at_original_deadline() {
    let mut fake = Fake::new();
    fake.io_pending = true;
    let (mut owner, journal) = fake.owner();
    let deadline = owner.deadline;
    assert!(
        owner
            .collect()
            .unwrap_err()
            .contains("original deadline exhausted")
    );
    assert_eq!(owner.backend.now, deadline);
    drop(owner);
    assert_order(&journal.borrow());
    assert_eq!(
        journal
            .borrow()
            .iter()
            .filter(|event| matches!(event, Event::Pump))
            .count(),
        10
    );
}

#[test]
/// Loss of the direct-child receipt forbids signaling, including partial setup
/// and Drop. A numeric ID or prior group creation is not replacement authority.
fn echild_revokes_authority_on_observation_and_setup_cleanup() {
    for setup_failure in [None, Some(2)] {
        let mut fake = Fake::new();
        fake.observe_error = Some(libc::ECHILD);
        fake.setup_failure = setup_failure;
        let (mut owner, journal) = fake.owner();
        assert!(owner.collect().unwrap_err().contains("refusing signals"));
        drop(owner);
        assert!(
            !journal
                .borrow()
                .iter()
                .any(|event| matches!(event, Event::Signal(..) | Event::Reap(..)))
        );
    }
}

#[test]
/// Wait errors remain failed/unconfirmed cleanup; they never re-arm signals.
/// Observation failures also preserve their primary error through final reap.
fn wait_failures_do_not_rearm_signal_authority() {
    for reap_error in [false, true] {
        let mut fake = Fake::new();
        fake.observe_error = Some(libc::EIO);
        fake.reap_error = reap_error;
        let (mut owner, journal) = fake.owner();
        let error = owner.collect().unwrap_err();
        assert!(error.contains("nonreaping exit observation"));
        if reap_error {
            assert!(error.contains("reap failed; cleanup unconfirmed"));
        }
        drop(owner);
        assert_order(&journal.borrow());
    }
}

#[test]
/// A continuously readable source gets one quota per call and at most 1 MiB of
/// retained bytes. Overflow fails instead of silently accepting truncation.
fn continuous_capture_is_quota_and_memory_bounded() {
    let mut capture = Capture::default();
    let mut reader = io::repeat(b'x');
    capture.read(&mut reader).unwrap();
    assert_eq!(capture.bytes.len(), IO_QUOTA);
    for _ in 1..CAPTURE_LIMIT / IO_QUOTA {
        capture.read(&mut reader).unwrap();
    }
    assert!(
        capture
            .read(&mut reader)
            .unwrap_err()
            .to_string()
            .contains("capture exceeded")
    );
    assert_eq!(capture.bytes.len(), CAPTURE_LIMIT);
    assert!(!capture.eof);
}

/// Reader that produces exactly one transient error and counts actual reads.
struct InterruptedReader(usize, io::ErrorKind);

impl Read for InterruptedReader {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        self.0 += 1;
        Err(self.1.into())
    }
}

#[test]
/// Interrupted and WouldBlock reads yield immediately so a stream cannot hide
/// the deadline in an internal retry loop or fabricate EOF on a transient.
fn transient_reads_do_not_spin_or_fabricate_eof() {
    for kind in [io::ErrorKind::Interrupted, io::ErrorKind::WouldBlock] {
        let mut reader = InterruptedReader(0, kind);
        let mut capture = Capture::default();
        capture.read(&mut reader).unwrap();
        assert_eq!(reader.0, 1);
        assert!(!capture.eof);
    }
}

/// Quota/short-write fake; optional transient injection must not advance input.
#[derive(Default)]
struct ShortWriter {
    bytes: Vec<u8>,
    error: Option<io::ErrorKind>,
    calls: usize,
}

impl Write for ShortWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        if let Some(error) = self.error.take() {
            return Err(error.into());
        }
        let count = bytes.len().min(2);
        self.bytes.extend_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
/// Staged input preserves short-write offsets and the wrapper/payload gap,
/// without spinning through transient errors or sleeping outside the driver.
fn staged_input_handles_short_writes_transients_and_wrapper_delay() {
    let now = Instant::now();
    let mut input = Input {
        wrapper: b"wrap".to_vec(),
        payload: b"data".to_vec(),
        offset: 0,
        payload_at: None,
    };
    let mut writer = ShortWriter::default();
    for kind in [io::ErrorKind::WouldBlock, io::ErrorKind::Interrupted] {
        writer.error = Some(kind);
        assert!(!input.write(&mut writer, now).unwrap());
        assert_eq!(input.offset, 0);
    }
    assert!(!input.write(&mut writer, now).unwrap());
    assert!(!input.write(&mut writer, now).unwrap());
    assert_eq!(writer.bytes, b"wrap");
    assert_eq!(writer.calls, 4);
    assert!(
        !input
            .write(&mut writer, now + PAYLOAD_DELAY - TICK)
            .unwrap()
    );
    assert_eq!(writer.calls, 4, "premature payload write");
    assert!(!input.write(&mut writer, now + PAYLOAD_DELAY).unwrap());
    assert!(input.write(&mut writer, now + PAYLOAD_DELAY).unwrap());
    assert_eq!(writer.bytes, b"wrapdata");
}

#[test]
/// Setup, observation, and final I/O/reap may exhaust the budget inside one
/// operation. At the exact boundary and beyond, neither acceptance nor a fresh
/// pump/drain budget is allowed, even when status and all bytes are available.
fn operation_aging_never_accepts_at_or_beyond_original_deadline() {
    for age in [TICK * 10, TICK * 11] {
        for operation in 0..4 {
            let mut fake = Fake::new();
            match operation {
                0 => fake.setup_age = age,
                1 => fake.observe_age = age,
                2 => fake.pump_age = age,
                _ => fake.reap_age = age,
            }
            let (mut owner, journal) = fake.owner();
            let deadline = owner.deadline;
            let error = owner.collect().unwrap_err();
            assert!(error.contains("original deadline exhausted"), "{error}");
            assert!(owner.backend.now >= deadline);
            let events = journal.borrow();
            assert!(
                !events.iter().any(|event| matches!(event, Event::Pause(_))),
                "fresh budget: {events:?}"
            );
            if operation < 2 {
                assert!(
                    !events.contains(&Event::Pump),
                    "pump after exhausted setup/observation"
                );
            }
            assert_order(&events);
        }
    }
}

#[test]
/// Explicit retirement precedes result assembly, so even a secondary wait error
/// first discovered during retirement forbids success and remains in the Err.
/// Reports are owned strings only, with no external sink or reporting lock.
fn retirement_errors_are_included_before_result_assembly() {
    let mut fake = Fake::new();
    fake.reap_error = true;
    let (mut owner, journal) = fake.owner();
    owner.finalize();
    let error = owner.finish(Some(ExitStatus::from_raw(0))).unwrap_err();
    assert!(error.contains("reap denied"), "{error}");
    assert!(error.contains("reap unconfirmed"), "{error}");
    let count = journal.borrow().len();
    owner.retire();
    owner.retire();
    drop(owner);
    assert_eq!(journal.borrow().len(), count);
    assert_order(&journal.borrow());
}

#[test]
/// Pending reap and failed waits remain idempotently retired. Drop cannot retry
/// signals/waits, invoke a diagnostic sink, or allocate another cleanup budget.
fn failed_retirement_and_drop_are_inert_after_explicit_retirement() {
    for reap_error in [false, true] {
        let mut fake = Fake::new();
        fake.reap_pending = !reap_error;
        fake.reap_error = reap_error;
        let (mut owner, journal) = fake.owner();
        owner.retire();
        assert!(
            owner
                .errors
                .iter()
                .any(|error| error.contains("reap unconfirmed"))
        );
        let count = journal.borrow().len();
        owner.retire();
        owner.retire();
        drop(owner);
        assert_eq!(journal.borrow().len(), count);
        assert_order(&journal.borrow());
    }
}

#[test]
/// An unwind keeps its original panic while failed cleanup stays in memory only.
/// The fake journal observes bounded signal/reap retirement without a reporting
/// callback, lock, stderr write, or secondary panic replacing the primary panic.
fn unwind_preserves_primary_panic_without_external_diagnostics() {
    let mut fake = Fake::new();
    fake.signal_errors = true;
    fake.reap_pending = true;
    let (owner, journal) = fake.owner();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _owner = owner;
        std::panic::panic_any("primary owner panic");
    }));
    let panic = result.unwrap_err();
    assert_eq!(panic.downcast_ref::<&str>(), Some(&"primary owner panic"));
    assert_order(&journal.borrow());
    assert_eq!(
        journal
            .borrow()
            .iter()
            .filter(|event| matches!(event, Event::Reap(_)))
            .count(),
        1
    );
}

/// Supplies complete synthetic settlement evidence without touching the kernel.
fn complete_settlement() -> Settlement {
    Settlement {
        collection_ok: true,
        root_reaped: true,
        capture_eof: true,
        input_closed: true,
        retired: true,
        handles_closed: true,
    }
}

#[test]
/// Every unknown settlement fact independently preserves fixture evidence. Reap,
/// EOF, input closure, retirement, descriptor closure and error-free collection
/// are not interchangeable, and missing proof cannot silently authorize deletion.
fn fixture_preservation_requires_every_settlement_fact() {
    assert!(!FixturePolicy::default().can_delete());
    for missing in 0..6 {
        let mut policy = FixturePolicy::default();
        let slot = policy.reserve();
        let mut evidence = complete_settlement();
        match missing {
            0 => evidence.collection_ok = false,
            1 => evidence.root_reaped = false,
            2 => evidence.capture_eof = false,
            3 => evidence.input_closed = false,
            4 => evidence.retired = false,
            _ => evidence.handles_closed = false,
        }
        assert!(!policy.settle(slot, evidence));
        assert!(!policy.can_delete());
    }
}

#[test]
/// The primary cannot settle the independent survivor or a failed spawn attempt.
/// All distinct reserved slots must settle; duplicate or invalid slot evidence
/// poisons the deletion decision rather than increasing a success counter.
fn fixture_deletion_requires_all_distinct_attempted_owners() {
    let mut policy = FixturePolicy::default();
    let primary = policy.reserve();
    let survivor = policy.reserve();
    assert!(policy.settle(primary, complete_settlement()));
    assert!(!policy.can_delete());
    assert!(policy.settle(survivor, complete_settlement()));
    assert!(policy.can_delete());
    let failed_spawn = policy.reserve();
    assert!(!policy.can_delete());
    assert!(!policy.settle(failed_spawn, Settlement::default()));
    assert!(!policy.can_delete());

    let mut duplicate = FixturePolicy::default();
    let slot = duplicate.reserve();
    assert!(duplicate.settle(slot, complete_settlement()));
    assert!(!duplicate.settle(slot, complete_settlement()));
    assert!(!duplicate.can_delete());
    let mut invalid = FixturePolicy::default();
    let slot = invalid.reserve();
    assert!(!invalid.settle(slot + 1, complete_settlement()));
    assert!(invalid.settle(slot, complete_settlement()));
    assert!(!invalid.can_delete());
}

/// Pure destruction probe records only a policy decision, never filesystem I/O.
struct FixtureDecisionOnDrop {
    policy: FixturePolicy,
    decision: Rc<Cell<Option<bool>>>,
}

impl Drop for FixtureDecisionOnDrop {
    /// Mirrors the kernel guard's sole deletion predicate without invoking it.
    fn drop(&mut self) {
        self.decision.set(Some(self.policy.can_delete()));
    }
}

#[test]
/// Unwind after reserving a child or after settling only the primary preserves
/// evidence. Dropping an owner, a panic or a timeout cannot substitute for the
/// missing survivor settlement. This test invokes neither processes nor files.
fn fixture_unwind_preserves_unresolved_owner_evidence() {
    for primary_settled in [false, true] {
        let decision = Rc::new(Cell::new(None));
        let observed = Rc::clone(&decision);
        let mut guard = FixtureDecisionOnDrop {
            policy: FixturePolicy::default(),
            decision,
        };
        let primary = guard.policy.reserve();
        if primary_settled {
            assert!(guard.policy.settle(primary, complete_settlement()));
        }
        guard.policy.reserve();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = guard;
            std::panic::panic_any("unresolved fixture owner");
        }));
        assert!(result.is_err());
        assert_eq!(observed.get(), Some(false));
    }
}
