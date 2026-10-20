//! Initial Bash qualification observations and synchronous failure custody.
//!
//! This one-consumer fixture helper preserves the nominal wait allowance and
//! authority predicate. State transitions are bounded and payload-free; elapsed time is
//! measured, never inferred from activity-wait attempts. The original service
//! stays outside the caught scenario and attempts manager-owned native cleanup
//! and independent selector shutdown exactly once before propagating failure.
//! No directory is deleted, native effect retried, or PID adopted from a report.

use super::*;
use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

/// Closed protocol states and presence/count facts only; never environment,
/// transaction markers, event bodies, shell screens or free-form diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BootstrapState {
    phase: Option<&'static str>,
    readiness: PaneReadinessState,
    pending: bool,
    active: bool,
    certified: bool,
    signature_present: bool,
    bounded_owner: bool,
    transaction_count: usize,
}

impl BootstrapState {
    /// Reads existing in-memory projections without additional native probes.
    fn capture(service: &RuntimeSessionService) -> Self {
        Self {
            phase: service.foreign_shell_bootstrap_phase_for_tests("%1"),
            readiness: service.pane_readiness_state("%1"),
            pending: service.pane_bootstrap_is_pending_for_tests("%1"),
            active: service.agent_subshell_is_active("%1"),
            certified: service.pane_environment_authority("%1")
                == RuntimePaneEnvironmentAuthority::Certified,
            signature_present: service.pane_environment_signature("%1").is_some(),
            bounded_owner: service.pane_bootstrap_has_bounded_progress_owner("%1"),
            transaction_count: service.running_shell_transactions_for_tests().len(),
        }
    }
}

/// Timing is deliberately separate from state equality: a repeated state is
/// not a transition merely because another poll or microsecond has elapsed.
#[derive(Debug)]
struct BootstrapSample {
    poll: usize,
    elapsed: Duration,
    state: BootstrapState,
}

/// Retains at most sixteen transitions plus an unconditional final sample.
/// The final sample remains truthful after transition capacity is exhausted.
#[derive(Debug, Default)]
pub(super) struct BootstrapTrace {
    transitions: Vec<BootstrapSample>,
    final_sample: Option<BootstrapSample>,
    omitted_transitions: usize,
}

impl BootstrapTrace {
    /// Records one projection without changing the scenario's driver behavior.
    pub(super) fn observe(
        &mut self,
        service: &RuntimeSessionService,
        poll: usize,
        elapsed: Duration,
    ) {
        self.record(BootstrapState::capture(service), poll, elapsed);
    }

    /// Updates the final timing even if no new state transition was observed.
    pub(super) fn finish(&mut self, service: &RuntimeSessionService, elapsed: Duration) {
        let poll = self.final_sample.as_ref().map_or(0, |sample| sample.poll);
        self.observe(service, poll, elapsed);
    }

    /// Bounded insertion seam used by deterministic process-free controls.
    fn record(&mut self, state: BootstrapState, poll: usize, elapsed: Duration) {
        if self
            .final_sample
            .as_ref()
            .is_none_or(|last| last.state != state)
        {
            if self.transitions.len() < 16 {
                self.transitions.push(BootstrapSample {
                    poll,
                    elapsed,
                    state: state.clone(),
                });
            } else {
                self.omitted_transitions += 1;
            }
        }
        self.final_sample = Some(BootstrapSample {
            poll,
            elapsed,
            state,
        });
    }
}

/// Preserves the live initial-entry authority predicate at the driver boundary.
/// A signature's separate successful-publication assertion remains in the test.
pub(super) fn initial_bootstrap_is_complete(pending: bool, certified: bool) -> bool {
    !pending && certified
}

/// One-consumer driver shared by live qualification and process-free controls.
/// The pump owns poll then reconciliation then authority observation; the wait
/// is activity-driven, not a guaranteed ten-millisecond delay. Preserve the old
/// nominal 400*10ms allowance as one explicit monotonic elapsed budget instead
/// of spending an attempt on each immediate wake. No transition renews it.
/// A pump may itself overrun (it is synchronous); such a late observation must
/// not qualify success, and no further wait or pump starts after expiry.
pub(super) fn drive_initial_bootstrap<S>(
    state: &mut S,
    elapsed: impl Fn(&S) -> Duration,
    mut pump: impl FnMut(&mut S) -> crate::Result<bool>,
    mut wait: impl FnMut(&mut S, Duration),
) -> crate::Result<bool> {
    const BUDGET: Duration = Duration::from_millis(400 * 10);
    while elapsed(state) < BUDGET {
        let complete = pump(state)?;
        let remaining = BUDGET.saturating_sub(elapsed(state));
        if remaining.is_zero() {
            return Ok(false);
        }
        if complete {
            return Ok(true);
        }
        wait(state, remaining.min(Duration::from_millis(10)));
    }
    Ok(false)
}

/// Actual failures remain owned, but Debug output must never reflect arbitrary
/// panic payloads or error messages (which may carry private source or paths).
enum CleanupFailure {
    Native(crate::MezError),
    Panic(&'static str, Box<dyn Any + Send>),
    UnprovenNative { outstanding: usize, failed: bool },
}

impl std::fmt::Debug for CleanupFailure {
    /// Exposes only the closed stage/disposition, not retained payload content.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native(_) => formatter.write_str("NativeError"),
            Self::Panic(stage, _) => formatter.debug_tuple("Panic").field(stage).finish(),
            Self::UnprovenNative {
                outstanding,
                failed,
            } => formatter
                .debug_struct("UnprovenNative")
                .field("outstanding", outstanding)
                .field("failed", failed)
                .finish(),
        }
    }
}

/// Owns primary and secondary evidence across the final synchronous boundary.
struct FixtureFailure {
    primary: Option<Box<dyn Any + Send>>,
    cleanup: Vec<CleanupFailure>,
}

/// Attempts both independent existing owners even if either unwinds. Keeping
/// state explicit lets process-free tests verify the same sequencing algorithm.
fn settle<S>(
    state: &mut S,
    native: impl FnOnce(&mut S) -> crate::Result<()>,
    selectors: impl FnOnce(&mut S),
) -> Vec<CleanupFailure> {
    let mut failures = Vec::new();
    match catch_unwind(AssertUnwindSafe(|| native(state))) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => failures.push(CleanupFailure::Native(error)),
        Err(payload) => failures.push(CleanupFailure::Panic("native", payload)),
    }
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| selectors(state))) {
        failures.push(CleanupFailure::Panic("selectors", payload));
    }
    failures
}

/// Resumes the identical primary payload on successful cleanup; compound
/// failure retains every payload without interpreting or dropping the primary.
fn finish<T>(primary: std::thread::Result<T>, cleanup: Vec<CleanupFailure>) -> T {
    if cleanup.is_empty() {
        return primary.unwrap_or_else(|payload| resume_unwind(payload));
    }
    let primary = primary.err();
    eprintln!(
        "Bash reentry fixture cleanup: primary_failed={} secondary={cleanup:?}",
        primary.is_some()
    );
    resume_unwind(Box::new(FixtureFailure { primary, cleanup }));
}

/// Covers startup itself and all later assertions with original-service cleanup.
/// This fixture never hands its Child to an async adapter; outstanding slots or
/// a sticky native failure therefore remain unproven, not healthy completion.
pub(super) fn with_cleanup(
    service: &mut RuntimeSessionService,
    body: impl FnOnce(&mut RuntimeSessionService),
) {
    let primary = catch_unwind(AssertUnwindSafe(|| body(service)));
    let mut failures = settle(
        service,
        |service| service.terminate_all_pane_processes().map(|_| ()),
        |service| service.clear_agent_prompt_inputs(),
    );
    let owner = service.pane_processes().retirement_owner();
    let outstanding = owner.outstanding();
    let failed = owner.has_cleanup_failure();
    if outstanding != 0 || failed {
        failures.push(CleanupFailure::UnprovenNative {
            outstanding,
            failed,
        });
    }
    finish(primary, failures);
}

/// Creates only closed synthetic facts, without native process/environment I/O.
fn state(pending: bool) -> BootstrapState {
    BootstrapState {
        phase: Some("identity-probing"),
        readiness: PaneReadinessState::Probing,
        pending,
        active: false,
        certified: false,
        signature_present: false,
        bounded_owner: true,
        transaction_count: 1,
    }
}

/// Repeated immediate activity wakes must retain their measured elapsed time,
/// not manufacture a four-second duration or fill the state-transition cap.
#[test]
fn reentry_qualification_repeated_state_keeps_actual_elapsed() {
    let mut trace = BootstrapTrace::default();
    for poll in 0..=400 {
        trace.record(state(true), poll, Duration::from_micros(poll as u64));
    }
    assert_eq!(trace.transitions.len(), 1);
    assert_eq!(trace.omitted_transitions, 0);
    let final_sample = trace.final_sample.unwrap();
    assert_eq!(final_sample.poll, 400);
    assert_eq!(final_sample.elapsed, Duration::from_micros(400));
}

/// Transition overflow cannot erase the final state/timing, including the
/// independent signature-present fact while Pending still takes precedence.
#[test]
fn reentry_qualification_trace_cap_preserves_final_facts() {
    let mut trace = BootstrapTrace::default();
    for poll in 0..40 {
        let mut sample = state(poll % 2 == 1);
        sample.signature_present = poll == 39;
        trace.record(sample, poll, Duration::from_millis(poll as u64));
    }
    assert_eq!(trace.transitions.len(), 16);
    assert_eq!(trace.omitted_transitions, 24);
    let final_sample = trace.final_sample.unwrap();
    assert_eq!(final_sample.poll, 39);
    assert!(final_sample.state.pending);
    assert!(final_sample.state.signature_present);
    assert_eq!(final_sample.elapsed, Duration::from_millis(39));
}

/// Opaque non-string panic identity and destruction remain owned through both
/// successful cleanup and compound secondary failure, never stringified.
#[test]
fn reentry_qualification_preserves_primary_and_secondary_payloads() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Payload(Arc<AtomicUsize>);
    impl Drop for Payload {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    for cleanup_fails in [false, true] {
        let drops = Arc::new(AtomicUsize::new(0));
        let payload = Box::new(Payload(drops.clone()));
        let address = &*payload as *const Payload;
        let cleanup = if cleanup_fails {
            vec![CleanupFailure::Native(crate::MezError::invalid_state(
                "synthetic-private-sentinel",
            ))]
        } else {
            Vec::new()
        };
        let result =
            catch_unwind(AssertUnwindSafe(|| finish::<()>(Err(payload), cleanup))).unwrap_err();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let retained = if cleanup_fails {
            let compound = result
                .downcast::<FixtureFailure>()
                .unwrap_or_else(|_| panic!("compound payload missing"));
            assert!(matches!(compound.cleanup[0], CleanupFailure::Native(_)));
            assert!(!format!("{:?}", compound.cleanup).contains("synthetic-private-sentinel"));
            compound.primary.unwrap()
        } else {
            result
        };
        let original = retained
            .downcast::<Payload>()
            .unwrap_or_else(|_| panic!("original payload missing"));
        assert_eq!(&*original as *const Payload, address);
        drop(original);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}

/// Native errors or either cleanup panic cannot skip selector shutdown; each
/// original closure is attempted once and every secondary payload is retained.
#[test]
fn reentry_qualification_cleanup_attempts_independent_owners_once() {
    for mode in 0..5 {
        let mut calls = Vec::new();
        let failures = settle(
            &mut calls,
            |calls| {
                calls.push("native");
                if mode == 1 {
                    return Err(crate::MezError::invalid_state("synthetic"));
                }
                if mode == 2 || mode == 4 {
                    resume_unwind(Box::new(17usize));
                }
                Ok(())
            },
            |calls| {
                calls.push("selectors");
                if mode >= 3 {
                    resume_unwind(Box::new(23usize));
                }
            },
        );
        assert_eq!(calls, ["native", "selectors"]);
        assert_eq!(failures.len(), [0, 1, 1, 1, 2][mode]);
        if mode == 2 || mode == 4 {
            assert!(
                matches!(&failures[0], CleanupFailure::Panic("native", value) if value.downcast_ref::<usize>() == Some(&17))
            );
        }
        if mode >= 3 {
            assert!(
                matches!(failures.last(), Some(CleanupFailure::Panic("selectors", value)) if value.downcast_ref::<usize>() == Some(&23))
            );
        }
        if let Some(CleanupFailure::Native(error)) = failures.first() {
            assert_eq!(error.message(), "synthetic");
        }
    }
}

/// A successful scenario cannot turn an unresolved native obligation into
/// success merely because the native cleanup closure returned without panic.
#[test]
fn reentry_qualification_unknown_native_disposition_remains_failure() {
    for (outstanding, failed) in [(1, false), (0, true)] {
        let failure = catch_unwind(|| {
            finish(
                Ok(()),
                vec![CleanupFailure::UnprovenNative {
                    outstanding,
                    failed,
                }],
            );
        })
        .unwrap_err()
        .downcast::<FixtureFailure>()
        .unwrap_or_else(|_| panic!("compound payload missing"));
        assert!(failure.primary.is_none());
        assert!(
            matches!(failure.cleanup[0], CleanupFailure::UnprovenNative { outstanding: count, failed: sticky } if count == outstanding && sticky == failed)
        );
    }
}

/// Immediate activity wakes can consume 400 attempts in microseconds. The same
/// live driver must permit a later certified result inside the original nominal
/// 400*10ms allowance, without changing either pump order or authority predicate.
/// This is a scheduling counterexample, not attribution of the saved failure.
#[test]
fn reentry_qualification_initial_driver_uses_nominal_elapsed_budget() {
    #[derive(Default)]
    struct Driver {
        elapsed: Duration,
        polls: usize,
        reconciliations: usize,
        waits: usize,
    }
    let mut state = Driver::default();
    let completed = drive_initial_bootstrap(
        &mut state,
        |state| state.elapsed,
        |state| {
            state.polls += 1;
            state.reconciliations += 1;
            state.elapsed += Duration::from_micros(1);
            Ok(initial_bootstrap_is_complete(
                state.polls <= 400,
                state.polls > 400,
            ))
        },
        |state, timeout| {
            assert!(timeout <= Duration::from_millis(10));
            state.waits += 1; // Ready fd: no minimum elapsed delay.
        },
    )
    .unwrap();
    assert!(
        completed,
        "initial driver exhausted attempts inside nominal elapsed allowance"
    );
    assert_eq!(state.polls, 401);
    assert_eq!(state.reconciliations, 401);
    assert_eq!(state.waits, 400);
    assert!(state.elapsed < Duration::from_secs(4));
}

/// Ready-first admission does not add a wait, and Certified cannot replace
/// the independent requirement that bootstrap Pending was actually cleared.
#[test]
fn reentry_qualification_initial_driver_preserves_authority_and_early_stop() {
    assert!(!initial_bootstrap_is_complete(true, true));
    assert!(!initial_bootstrap_is_complete(false, false));
    let mut pumps = 0;
    assert!(
        drive_initial_bootstrap(
            &mut pumps,
            |_| Duration::ZERO,
            |pumps| {
                *pumps += 1;
                Ok(initial_bootstrap_is_complete(false, true))
            },
            |_, _| panic!("ready initial bootstrap must not wait")
        )
        .unwrap()
    );
    assert_eq!(pumps, 1);
}

/// Permanently Pending remains a failure under one fixed allowance even as
/// useful pumps consume time. Waits cannot push the driver beyond its budget or
/// restart it; no synthetic success or retries are used to make this terminate.
#[test]
fn reentry_qualification_initial_driver_expires_pending_at_nominal_budget() {
    let mut clock = Duration::ZERO;
    let mut pumps = 0;
    let completed = drive_initial_bootstrap(
        &mut clock,
        |clock| *clock,
        |clock| {
            pumps += 1;
            *clock += Duration::from_millis(1);
            Ok(initial_bootstrap_is_complete(true, true))
        },
        |clock, timeout| {
            assert!(timeout <= Duration::from_millis(10));
            assert!(*clock + timeout <= Duration::from_secs(4));
            *clock += timeout;
        },
    )
    .unwrap();
    assert!(!completed);
    assert_eq!(clock, Duration::from_secs(4));
    assert_eq!(pumps, 364);
}

/// A final activity wait gets only the five milliseconds still available;
/// readiness must not extend or reset the original allowance after that wake.
#[test]
fn reentry_qualification_initial_driver_clips_last_activity_wait() {
    let mut clock = Duration::from_millis(3995);
    let mut pumps = 0;
    let mut waits = Vec::new();
    let completed = drive_initial_bootstrap(
        &mut clock,
        |clock| *clock,
        |_| {
            pumps += 1;
            Ok(false)
        },
        |clock, timeout| {
            waits.push(timeout);
            *clock += timeout;
        },
    )
    .unwrap();
    assert!(!completed);
    assert_eq!(pumps, 1);
    assert_eq!(waits, [Duration::from_millis(5)]);
    assert_eq!(clock, Duration::from_secs(4));
}

/// Synchronous pumps cannot be preempted here. Ready observed at or after the
/// bound is explicitly rejected without any extra wait, while just-in-budget
/// ready is accepted. The deadline is not an execution-cancellation claim.
#[test]
fn reentry_qualification_initial_driver_rejects_late_pump_success() {
    for (elapsed_ms, ready) in [(3999, true), (4000, true), (4001, true), (4001, false)] {
        let mut clock = Duration::ZERO;
        let mut pumps = 0;
        let completed = drive_initial_bootstrap(
            &mut clock,
            |clock| *clock,
            |clock| {
                pumps += 1;
                *clock = Duration::from_millis(elapsed_ms);
                Ok(ready)
            },
            |_, _| panic!("completed or expired pump must not wait"),
        )
        .unwrap();
        assert_eq!(completed, elapsed_ms < 4000 && ready);
        assert_eq!(pumps, 1);
    }
}

/// A pump error remains its original error and does not spend an activity wait,
/// qualify success, or replay the poll/reconciliation step to manufacture state.
#[test]
fn reentry_qualification_initial_driver_preserves_pump_error() {
    let mut pumps = 0;
    let error = drive_initial_bootstrap(
        &mut pumps,
        |_| Duration::ZERO,
        |pumps| {
            *pumps += 1;
            Err(crate::MezError::invalid_state("synthetic-pump-error"))
        },
        |_, _| panic!("failed pump must not wait"),
    )
    .unwrap_err();
    assert_eq!(error.message(), "synthetic-pump-error");
    assert_eq!(pumps, 1);
}
