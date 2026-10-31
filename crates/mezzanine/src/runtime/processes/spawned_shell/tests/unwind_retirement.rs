//! Exact reader joins across a native executor unwind with real child custody.
//!
//! Each fixture owns one typed child, three quiet pipes and staged artifacts.
//! A post-join counter distinguishes observed thread termination from a mere
//! cancellation request, reader notification or detached handle. Assertions run
//! after direct-child reaping and artifact removal. No process-global fault
//! switch, guessed child identity or arbitrary panic text is involved.

use super::*;
use std::sync::atomic::AtomicUsize;

/// Opaque primary failure whose allocation and destruction remain observable
/// without formatting or copying private panic contents during cleanup.
struct PrimaryFailure {
    /// Allocation identity that must survive the unwind unchanged.
    allocation: Box<u8>,
    /// Counts destruction of this single original failure owner.
    dropped: Arc<AtomicUsize>,
}

impl Drop for PrimaryFailure {
    /// Records destruction only after the test has recovered the same payload.
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

/// Exercises real spawn/reader/materialization ownership and returns the count
/// of joined readers after all native resources and the primary payload settle.
fn exercise_reader_retirement(unwind: bool) -> usize {
    let label = if unwind { "unwind" } else { "explicit-join" };
    let (fixture, readiness, descendant_readiness, marker) = reader_failure_fixture(label);
    let mut executor = SpawnedShellExecutor::new(test_context());
    let custody = executor.observe_custody();
    let joined = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let payload = PrimaryFailure {
        allocation: Box::new(17),
        dropped: dropped.clone(),
    };
    let allocation = (&*payload.allocation) as *const u8 as usize;
    let mut observed_pid = None;
    let mut readers_were_live = false;
    let mut settlement = None;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // exec retains the exact direct-child incarnation and all three pipe
        // writers, with no descendant whose custody would need inferring.
        let request = reader_setup_request(
            "printf '%s' \"$$\" > \"$1\"; exec /bin/sleep 30",
            &readiness,
            &descendant_readiness,
            &marker,
        );
        let materialized = executor.materialize_launch(&request.transaction).unwrap();
        let SpawnedChild {
            mut guard,
            status_reader,
        } = executor
            .spawn_child(&request.transaction, &materialized)
            .unwrap();
        let (done, _completion) = mpsc::channel();
        let mut readers = executor
            .start_spawned_child_readers(&mut guard, status_reader, 4096, done, None)
            .unwrap();
        for reader in [&mut readers.stdout, &mut readers.stderr]
            .into_iter()
            .chain(readers.status.as_mut())
        {
            reader.join_witness = Some(joined.clone());
        }
        observed_pid = Some(read_pid_witness(&readiness));
        readers_were_live = [&readers.stdout, &readers.stderr]
            .into_iter()
            .chain(readers.status.as_ref())
            .all(|reader| !reader.handle.as_ref().unwrap().is_finished())
            && readers.status.is_some();
        if unwind {
            std::panic::panic_any(payload);
        }
        let reaped = guard.terminate_and_reap();
        let failures = readers.settle();
        settlement = Some((reaped, failures));
        drop(payload);
    }));
    // Exact child fate and staging cleanup are checked before the regression's
    // join assertion, including on the deliberately failing baseline.
    let witness = custody.lock().unwrap().clone().unwrap();
    let count = joined.load(Ordering::SeqCst);
    let fixture_removed = fs::remove_dir_all(&fixture);
    assert!(witness.reaped, "direct native child was not reaped");
    assert_eq!(observed_pid, Some(witness.pid));
    assert_eq!(witness.process_group, witness.pid);
    assert!(witness.artifact_directory_present_at_spawn);
    assert_eq!(witness.artifact_count, 1);
    assert!(!witness.artifact_directory.exists());
    assert!(readers_were_live, "three live readers were not established");
    fixture_removed.unwrap();
    if unwind {
        let failure = outcome.unwrap_err().downcast::<PrimaryFailure>().unwrap();
        assert_eq!((&*failure.allocation) as *const u8 as usize, allocation);
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        drop(failure);
    } else {
        outcome.unwrap();
        let (reaped, failures) = settlement.unwrap();
        reaped.unwrap();
        assert!(failures.is_empty(), "normal reader settlement failed");
    }
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    count
}

/// Normal explicit cleanup must observe all three real reader joins. This
/// positive control validates the witness independently of unwind fallback.
#[test]
fn native_explicit_cleanup_observes_all_pipe_reader_joins() {
    assert_eq!(exercise_reader_retirement(false), 3);
}

/// An executor unwind must cancel and join stdout, stderr and trusted status
/// readers before releasing their owners, while preserving the primary failure
/// allocation and exact child/artifact custody. Sender disconnection or a done
/// notice is insufficient evidence that a detached thread has been joined.
#[test]
fn native_executor_unwind_observes_all_pipe_reader_joins() {
    assert_eq!(exercise_reader_retirement(true), 3);
}
