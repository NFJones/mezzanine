//! Process-free regressions for the two cases' shared final join boundary.
//!
//! Fabricated thread results exercise the real consumer without creating a
//! runtime, thread, fixture, storage, process, signal, or retirement authority.
//! Fresh probes per case check original box and compound payload custody;
//! safe diagnostics are a borrowed projection, not a replacement panic.

use std::sync::atomic::AtomicUsize;

use super::*;
use crate::host::async_runtime::tests::native_fixture::{Failure, Stage};

/// Explicit expected labels catch accidental case aliasing, including replay's
/// original diagnostic spelling. Every ownership test uses fresh data per case.
const CASES: [(FixtureCase, &str); 2] = [
    (FixtureCase::ReplayedForgery, "replayed-forgery"),
    (
        FixtureCase::SpoofedForegroundAuthority,
        "spoofed-foreground-authority",
    ),
];

/// Non-Clone arbitrary panic/value data with observable final ownership.
struct DropProbe {
    drops: Arc<AtomicUsize>,
    private_text: &'static str,
}

impl Drop for DropProbe {
    /// Records only a drop count, never diagnostic text or native activity.
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

/// Supplies the same synthetic sentinel in payload/error/cleanup/path fields.
const PRIVATE_TEXT: &str = "PRIVATE_SENTINEL_screen_log_argv_env_token_panic";

/// Captures exact allocation identity without requiring payload Clone/Debug.
fn identity(payload: &(dyn std::any::Any + Send)) -> *const () {
    payload as *const _ as *const ()
}

#[test]
/// The old unwrap consumer replaces and drops arbitrary panic data. Both cases
/// must unwind with the same allocation, retaining it until the ultimate owner
/// drops it once, and must never reflect its private text.
fn opaque_join_panic_retains_identity_and_single_drop() {
    for (case, label) in CASES {
        let drops = Arc::new(AtomicUsize::new(0));
        let payload: Box<dyn std::any::Any + Send> = Box::new(DropProbe {
            drops: drops.clone(),
            private_text: PRIVATE_TEXT,
        });
        let original = identity(payload.as_ref());
        let mut output = Vec::new();
        let resumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            finish_fixture_thread::<()>(
                Err(payload),
                FixtureCheckpoint::CallerAssertions as usize,
                case,
                &mut output,
            );
        }))
        .unwrap_err();
        assert_eq!(identity(resumed.as_ref()), original);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(
            resumed.downcast_ref::<DropProbe>().unwrap().private_text,
            PRIVATE_TEXT
        );
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("{label} thread-unwind last-entered=caller-assertions opaque-panic\n")
        );
        drop(resumed);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}

#[test]
/// The actual FixtureFailure Any type crosses both boundaries without nesting
/// or reconstruction. Primary/secondary allocations, ordered stages, marked
/// typed errors, cleanup bodies, pending count and root remain unpublished.
fn compound_join_panic_preserves_all_evidence_without_reflection() {
    for (case, label) in CASES {
        let drops = Arc::new(AtomicUsize::new(0));
        let primary: Box<dyn std::any::Any + Send> = Box::new(DropProbe {
            drops: drops.clone(),
            private_text: PRIVATE_TEXT,
        });
        let secondary: Box<dyn std::any::Any + Send> = Box::new(DropProbe {
            drops: drops.clone(),
            private_text: PRIVATE_TEXT,
        });
        let primary_identity = identity(primary.as_ref());
        let secondary_identity = identity(secondary.as_ref());
        let report = Box::new(FixtureFailure {
            failures: vec![
                (Stage::Client, Failure::Panic(primary)),
                (Stage::Supervisor, Failure::Panic(secondary)),
                (
                    Stage::Actor,
                    Failure::Error(
                        MezError::from(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            PRIVATE_TEXT,
                        ))
                        .mark_pane_backend_failure(),
                    ),
                ),
            ],
            cleanup: vec![PRIVATE_TEXT.to_string()],
            pending: 7,
            root: PRIVATE_TEXT.into(),
        });
        let original = identity(report.as_ref());
        let mut output = Vec::new();
        let resumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            finish_fixture_thread::<()>(
                Err(report),
                FixtureCheckpoint::Scenario as usize,
                case,
                &mut output,
            );
        }))
        .unwrap_err();
        assert_eq!(identity(resumed.as_ref()), original);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let report = resumed.downcast_ref::<FixtureFailure>().unwrap();
        assert_eq!(report.failures.len(), 3);
        for (index, expected) in [primary_identity, secondary_identity]
            .into_iter()
            .enumerate()
        {
            let (stage, failure) = &report.failures[index];
            assert_eq!(*stage, [Stage::Client, Stage::Supervisor][index]);
            let Failure::Panic(payload) = failure else {
                panic!("original panic kind replaced");
            };
            assert_eq!(identity(payload.as_ref()), expected);
            assert_eq!(
                payload.downcast_ref::<DropProbe>().unwrap().private_text,
                PRIVATE_TEXT
            );
        }
        let (stage, Failure::Error(error)) = &report.failures[2] else {
            panic!("original secondary error replaced");
        };
        assert_eq!(*stage, Stage::Actor);
        assert_eq!(error.message(), PRIVATE_TEXT);
        assert_eq!(error.io_kind(), Some(std::io::ErrorKind::PermissionDenied));
        assert!(error.is_pane_backend_failure());
        assert_eq!(report.cleanup, [PRIVATE_TEXT]);
        assert_eq!(report.pending, 7);
        assert_eq!(report.root, std::path::PathBuf::from(PRIVATE_TEXT));
        let output = String::from_utf8(output).unwrap();
        assert_eq!(
            output,
            format!(
                "{label} thread-unwind last-entered=scenario fixture-report failure_count=3 cleanup_count=1 pending=7 stages=[Client:panic,Supervisor:panic,Actor:error(kind=Io,io=Some(PermissionDenied),backend=true)] omitted=0\n"
            )
        );
        assert!(!output.contains(PRIVATE_TEXT));
        drop(resumed);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }
}

#[test]
/// Every failure tag and stage is source-owned diagnostic data. For both cases
/// an oversized report proves capped output and omission counts while the
/// underlying report remains intact; case labels are asserted independently.
fn safe_report_distinguishes_kinds_and_bounds_output() {
    for (case, label) in CASES {
        let mut report = FixtureFailure {
            failures: vec![
                (Stage::Client, Failure::UnexpectedCompletion),
                (Stage::Supervisor, Failure::Deadline),
                (Stage::Actor, Failure::Cancelled),
                (
                    Stage::Client,
                    Failure::Error(MezError::invalid_state(PRIVATE_TEXT)),
                ),
            ],
            cleanup: vec![],
            pending: 0,
            root: PRIVATE_TEXT.into(),
        };
        assert_eq!(
            report.safe_summary(),
            "fixture-report failure_count=4 cleanup_count=0 pending=0 stages=[Client:unexpected-completion,Supervisor:deadline,Actor:cancelled,Client:error(kind=InvalidState,io=None,backend=false)] omitted=0"
        );
        report
            .failures
            .extend((0..1000).map(|_| (Stage::Actor, Failure::Cancelled)));
        let summary = fixture_join_diagnostic(&report, FixtureCheckpoint::Scenario as usize, case);
        assert!(summary.starts_with(&format!(
            "{label} thread-unwind last-entered=scenario fixture-report failure_count=1004 "
        )));
        assert!(summary.ends_with("] omitted=996"));
        assert_eq!(summary.matches("Actor:cancelled").count(), 5);
        assert!(summary.len() < 1024);
        assert!(!summary.contains(PRIVATE_TEXT));
        assert_eq!(report.failures.len(), 1004);
    }
}

#[test]
/// Both cases distinguish runtime construction, scenario, caller assertions and
/// runtime destruction; neither string panic content nor invalid checkpoints
/// can become reflected labels. The replay label remains unchanged.
fn opaque_diagnostic_uses_only_static_checkpoints() {
    for (case, case_label) in CASES {
        let payload = PRIVATE_TEXT.to_string();
        for (checkpoint, label) in [
            (
                FixtureCheckpoint::RuntimeConstruction as usize,
                "runtime-construction",
            ),
            (FixtureCheckpoint::Scenario as usize, "scenario"),
            (
                FixtureCheckpoint::CallerAssertions as usize,
                "caller-assertions",
            ),
            (FixtureCheckpoint::RuntimeDrop as usize, "runtime-drop"),
            (usize::MAX, "unknown"),
        ] {
            assert_eq!(
                fixture_join_diagnostic(&payload, checkpoint, case),
                format!("{case_label} thread-unwind last-entered={label} opaque-panic")
            );
        }
    }
}

/// A fallible diagnostic sink cannot replace the original panic ownership.
struct FailedOutput;

impl std::io::Write for FailedOutput {
    /// Simulates stderr write failure without any process or filesystem I/O.
    fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }

    /// There is no buffered data or external output to flush.
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
/// A broken output stream leaves the same original panic box in the ultimate
/// unwind for each case; diagnostic publication cannot replace that failure.
fn failed_diagnostic_write_does_not_replace_original_panic() {
    for (case, _) in CASES {
        let payload: Box<dyn std::any::Any + Send> = Box::new(PRIVATE_TEXT.to_string());
        let original = identity(payload.as_ref());
        let resumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            finish_fixture_thread::<()>(Err(payload), 0, case, &mut FailedOutput);
        }))
        .unwrap_err();
        assert_eq!(identity(resumed.as_ref()), original);
        assert_eq!(resumed.downcast_ref::<String>().unwrap(), PRIVATE_TEXT);
    }
}

#[test]
/// Successful non-Clone outputs transfer once without diagnostics or synthetic
/// failure for either case. Their fresh values remain owned until normal drop.
fn successful_join_preserves_value_without_diagnostic() {
    for (case, _) in CASES {
        let drops = Arc::new(AtomicUsize::new(0));
        let value = Box::new(DropProbe {
            drops: drops.clone(),
            private_text: PRIVATE_TEXT,
        });
        let original = value.as_ref() as *const DropProbe;
        let mut output = Vec::new();
        let returned = finish_fixture_thread(Ok(value), usize::MAX, case, &mut output);
        assert_eq!(returned.as_ref() as *const DropProbe, original);
        assert!(output.is_empty());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(returned);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}
