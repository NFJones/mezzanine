//! Process-free controls for the Fish-owned outer-result return observation.
//!
//! Fabricated results exercise the same publication and final join helpers.
//! No watcher, actor, runtime, thread, filesystem, process, or retirement is
//! created. The only sampled fact is the existing fixture-stop atomic; unknown
//! lifecycle and absent evidence cannot become shutdown or causal authority.

use super::*;
use crate::host::async_runtime::tests::native_fixture::{Failure, Stage};
use std::sync::atomic::AtomicUsize;

/// Owned non-Clone evidence with a single observable final drop.
struct Probe(Arc<AtomicUsize>);

impl Drop for Probe {
    /// Counts final release without native activity or diagnostic text.
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Captures allocation identity without requiring Clone or Debug.
fn identity(payload: &(dyn std::any::Any + Send)) -> *const () {
    payload as *const _ as *const ()
}

#[test]
/// A fresh slot records either bool only on Err. A second Err cannot overwrite
/// the first sample, so later stop changes cannot rewrite return provenance.
fn return_observation_publishes_once_for_each_stop_value() {
    for stopped in [false, true] {
        let slot = SupervisorReturnObservation::default();
        let stop = AtomicBool::new(stopped);
        assert_eq!(slot.fixture_stop_at_return(), None);
        let error = MezError::from(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "PRIVATE_original_error",
        ))
        .mark_pane_backend_failure();
        let original_message = error.message().as_ptr();
        let returned = slot.observe_result::<(), _>(Err(error), &stop).unwrap_err();
        assert_eq!(returned.message().as_ptr(), original_message);
        assert_eq!(returned.message(), "PRIVATE_original_error");
        assert_eq!(
            returned.io_kind(),
            Some(std::io::ErrorKind::PermissionDenied)
        );
        assert!(returned.is_pane_backend_failure());
        assert_eq!(slot.fixture_stop_at_return(), Some(stopped));
        stop.store(!stopped, Ordering::SeqCst);
        assert!(slot.observe_result::<(), ()>(Err(()), &stop).is_err());
        assert_eq!(slot.fixture_stop_at_return(), Some(stopped));
    }
}

#[test]
/// Success transfers the original non-Clone value without publishing a sample
/// or emitting a diagnostic, even through the optional observed join consumer.
fn successful_result_and_join_leave_observation_absent() {
    let slot = SupervisorReturnObservation::default();
    let drops = Arc::new(AtomicUsize::new(0));
    let value = Box::new(Probe(drops.clone()));
    let original = identity(value.as_ref());
    let result = slot.observe_result::<_, ()>(Ok(value), &AtomicBool::new(true));
    let mut output = Vec::new();
    let returned = finish_fixture_thread_observed(
        Ok(result.unwrap()),
        0,
        FixtureCase::SpoofedForegroundFishAuthority,
        Some(&slot),
        &mut output,
    );
    assert_eq!(identity(returned.as_ref()), original);
    assert_eq!(slot.fixture_stop_at_return(), None);
    assert!(output.is_empty());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(returned);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
/// Panic before an outer result is available cannot fabricate a return sample.
/// The unpublished slot stays unknown through the final original-box unwind.
fn pre_return_unwind_keeps_unknown_observation_and_original_payload() {
    let slot = SupervisorReturnObservation::default();
    let drops = Arc::new(AtomicUsize::new(0));
    let payload: Box<dyn std::any::Any + Send> = Box::new(Probe(drops.clone()));
    let original = identity(payload.as_ref());
    let prior = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        std::panic::resume_unwind(payload)
    }))
    .unwrap_err();
    let mut output = Vec::new();
    let resumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        finish_fixture_thread_observed::<()>(
            Err(prior),
            FixtureCheckpoint::Scenario as usize,
            FixtureCase::SpoofedForegroundFishAuthority,
            Some(&slot),
            &mut output,
        );
    }))
    .unwrap_err();
    assert_eq!(slot.fixture_stop_at_return(), None);
    assert_eq!(identity(resumed.as_ref()), original);
    assert!(
        String::from_utf8(output)
            .unwrap()
            .ends_with(" supervisor-return-observed lifecycle=unknown fixture-stop=unknown\n")
    );
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(resumed);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
/// Compound primary/secondary evidence retains outer and nested allocations
/// through observation and final unwind. A preoccupied invalid slot fails closed
/// without replacing the original result or dropping either owned payload.
fn compound_result_survives_observation_and_failed_publication() {
    for encoding in [0, usize::MAX] {
        let slot = SupervisorReturnObservation(AtomicUsize::new(encoding));
        let drops = Arc::new(AtomicUsize::new(0));
        let primary: Box<dyn std::any::Any + Send> = Box::new(Probe(drops.clone()));
        let secondary: Box<dyn std::any::Any + Send> = Box::new(Probe(drops.clone()));
        let nested = [identity(primary.as_ref()), identity(secondary.as_ref())];
        let report = Box::new(FixtureFailure {
            failures: vec![
                (Stage::Client, Failure::Panic(primary)),
                (Stage::Supervisor, Failure::Panic(secondary)),
            ],
            cleanup: vec!["PRIVATE_cleanup".into()],
            pending: 7,
            root: "PRIVATE_root".into(),
        });
        let original = identity(report.as_ref());
        let report = slot
            .observe_result::<(), _>(Err(report), &AtomicBool::new(true))
            .err()
            .unwrap();
        let mut output = Vec::new();
        let resumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            finish_fixture_thread_observed::<()>(
                Err(report),
                FixtureCheckpoint::Scenario as usize,
                FixtureCase::SpoofedForegroundFishAuthority,
                Some(&slot),
                &mut output,
            );
        }))
        .unwrap_err();
        assert_eq!(identity(resumed.as_ref()), original);
        let report = resumed.downcast_ref::<FixtureFailure>().unwrap();
        for ((_, failure), expected) in report.failures.iter().zip(nested) {
            let Failure::Panic(payload) = failure else {
                panic!("original compound evidence was replaced");
            };
            assert_eq!(identity(payload.as_ref()), expected);
        }
        assert_eq!(report.cleanup, ["PRIVATE_cleanup"]);
        assert_eq!(report.pending, 7);
        assert_eq!(report.root, std::path::PathBuf::from("PRIVATE_root"));
        assert!(!String::from_utf8(output).unwrap().contains("PRIVATE"));
        assert_eq!(
            slot.fixture_stop_at_return(),
            (encoding == 0).then_some(true)
        );
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(resumed);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }
}

#[test]
/// Every possible projected label is static, absent/invalid samples remain
/// unknown, and None instrumentation preserves the existing diagnostic exactly.
fn return_projection_is_bounded_private_and_explicitly_unknown() {
    let private = "PRIVATE_screen_env_argv_error_panic".to_string();
    for (encoding, label) in [
        (0, "unknown"),
        (1, "false"),
        (2, "true"),
        (usize::MAX, "unknown"),
    ] {
        let slot = SupervisorReturnObservation(AtomicUsize::new(encoding));
        let diagnostic = fixture_join_diagnostic_observed(
            &private,
            FixtureCheckpoint::Scenario as usize,
            FixtureCase::SpoofedForegroundFishAuthority,
            Some(&slot),
        );
        assert_eq!(
            diagnostic,
            format!(
                "spoofed-foreground-fish-authority thread-unwind last-entered=scenario opaque-panic supervisor-return-observed lifecycle=unknown fixture-stop={label}"
            )
        );
        assert!(diagnostic.len() < 256);
        assert!(!diagnostic.contains(&private));
    }
    for case in [
        FixtureCase::ReplayedForgery,
        FixtureCase::SpoofedForegroundAuthority,
    ] {
        assert_eq!(
            fixture_join_diagnostic_observed(&private, 0, case, None),
            fixture_join_diagnostic(&private, 0, case)
        );
    }
}

/// A synthetic sink that refuses diagnostics without performing external I/O.
struct FailedOutput;

impl std::io::Write for FailedOutput {
    /// Returns an ordinary write error, never a new panic or process action.
    fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }

    /// This sink owns no buffered output.
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
/// A failed diagnostic write after observation cannot replace or release the
/// primary original panic. Publication remains independent of output success.
fn observed_write_failure_preserves_original_panic_ownership() {
    let slot = SupervisorReturnObservation::default();
    let drops = Arc::new(AtomicUsize::new(0));
    let payload: Box<dyn std::any::Any + Send> = Box::new(Probe(drops.clone()));
    let original = identity(payload.as_ref());
    let payload = slot
        .observe_result::<(), _>(Err(payload), &AtomicBool::new(false))
        .err()
        .unwrap();
    let resumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        finish_fixture_thread_observed::<()>(
            Err(payload),
            0,
            FixtureCase::SpoofedForegroundFishAuthority,
            Some(&slot),
            &mut FailedOutput,
        );
    }))
    .unwrap_err();
    assert_eq!(identity(resumed.as_ref()), original);
    assert_eq!(slot.fixture_stop_at_return(), Some(false));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(resumed);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
