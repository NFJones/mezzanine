//! Narrow Linux kernel-adapter qualification with freshly owned synthetic roots.
//!
//! These fixtures do not run Bubblewrap, capability probes, providers, or ambient
//! shell configuration. Every absolute executable receives a cleared environment
//! and generated HOME/cwd; fixture storage is declared before and outlives all
//! owners. The same guarded spawn, birth group, WNOWAIT observation, immutable
//! deadline and nonblocking collector used by real workloads own every child.
//! No numeric PID from output/status, process scan, detached reader or separate
//! cleanup budget is used. Ordinary assertions follow explicit cleanup.
//!
//! The retained-pipe case proves prompt capture closure, not grandchild reaping;
//! its survivor is one independently owned control, not a PID-reuse stress test
//! or universal isolation proof. These tests cannot qualify namespaces, real
//! Bubblewrap, escaped descendants, macOS, stalled syscalls, or guaranteed reap
//! after budget exhaustion. On exceptional unwind, reviewed Drop cleanup remains
//! best effort with no guaranteed secondary diagnostic reporting.

use std::os::unix::process::ExitStatusExt;

use super::super::{REAL_BUBBLEWRAP_EXECUTION_TIMEOUT, RealBubblewrapFixture};
use super::*;

/// Uses only generated fixture paths and explicit executables, never user rc or
/// ambient HOME/PATH/TMPDIR/configuration. No caller can supply an unowned PID.
fn command(fixture: &RealBubblewrapFixture, executable: &str, args: &[&str]) -> Command {
    let mut command = Command::new(executable);
    command
        .args(args)
        .env_clear()
        .env("HOME", &fixture.host_home)
        .env("TMPDIR", &fixture.target)
        .env("LC_ALL", "C")
        .current_dir(&fixture.source);
    command
}

/// Gets two positive WNOWAIT receipts from the same owned unreaped root. Errors
/// and timeout are returned for interpretation only after explicit collection.
fn observe_exit_twice(owner: &mut Owner<LiveWorkload>) -> Result<(), String> {
    let mut positives = 0;
    loop {
        if owner.backend.now() >= owner.deadline {
            return Err("original deadline expired while observing owned root".to_owned());
        }
        match owner.backend.exited() {
            Ok(true) => {
                positives += 1;
                if positives == 2 {
                    return if owner.backend.now() < owner.deadline {
                        Ok(())
                    } else {
                        Err("original deadline expired at second exit observation".to_owned())
                    };
                }
            }
            Ok(false) if positives == 0 => {
                let remaining = owner
                    .deadline
                    .saturating_duration_since(owner.backend.now());
                if !remaining.is_zero() {
                    owner.backend.pause(TICK.min(remaining));
                }
            }
            Ok(false) => return Err("owned exit observation unexpectedly disappeared".to_owned()),
            Err(error) => return Err(format!("owned nonreaping exit observation: {error}")),
        }
    }
}

#[test]
/// Exercises real waitid(WNOWAIT) twice on an exited child before any actual
/// reap, then verifies final group/root cleanup preserves its exit 7 receipt and
/// complete stdout/stderr. The shared journal separately proves signal ordering.
fn kernel_nonreaping_observation_preserves_exact_root_status_and_capture() {
    let fixture = RealBubblewrapFixture::new("kernel-wnowait");
    let mut owner = spawn_owner(
        command(
            &fixture,
            "/bin/sh",
            &["-c", "printf owned-out; printf owned-err >&2; exit 7"],
        ),
        None,
        REAL_BUBBLEWRAP_EXECUTION_TIMEOUT,
    )
    .unwrap();
    let observed = observe_exit_twice(&mut owner);
    let collected = owner.collect();
    let authority = owner.authority;
    let stdout = std::mem::take(&mut owner.backend.out.bytes);
    let stderr = std::mem::take(&mut owner.backend.err.bytes);
    drop(owner);

    observed.unwrap();
    let status = collected.unwrap();
    assert_eq!(authority, Authority::Reaped);
    assert_eq!(status.code(), Some(7));
    assert_eq!(stdout, b"owned-out");
    assert_eq!(stderr, b"owned-err");
}

#[test]
/// Exercises real nonblocking input/output pressure without a writer thread.
/// Staged bytes exceed individual I/O quotas but stay below the capture cap;
/// exact equality forbids truncation, duplication, or dropped short-write bytes.
/// The 50ms staging separation itself remains a virtual-clock journal assertion.
fn kernel_staged_input_and_capture_preserve_all_bounded_bytes() {
    let fixture = RealBubblewrapFixture::new("kernel-staged-io");
    let wrapper = vec![b'w'; 128 * 1024];
    let payload = vec![b'p'; 128 * 1024];
    let mut expected = wrapper.clone();
    expected.extend_from_slice(&payload);
    let collected = output(
        command(&fixture, "/bin/cat", &[]),
        Some((wrapper, payload)),
        REAL_BUBBLEWRAP_EXECUTION_TIMEOUT,
    );

    let output = collected.unwrap();
    assert!(output.status.success(), "{:?}", output.status);
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    assert_eq!(output.stdout, expected);
}

#[test]
/// Preserves the original retained-capture-pipe regression name while adding one
/// separately owned survivor. The shell exits normally with a sleeping descendant
/// holding both pipes; group cleanup must close capture promptly without touching
/// the independent root. This does not claim the harness reaped the grandchild.
/// Survivor finalization/collection occurs before interpreting either observation
/// or primary output, with its original deadline unchanged while primary runs.
fn bounded_child_output_reaps_descendants_that_retain_capture_pipes() {
    let fixture = RealBubblewrapFixture::new("kernel-pipe-owner");
    let mut survivor = spawn_owner(
        command(&fixture, "/bin/sleep", &["120"]),
        None,
        REAL_BUBBLEWRAP_EXECUTION_TIMEOUT,
    )
    .unwrap();
    let before = survivor.backend.exited();
    let started = Instant::now();
    let primary = output(
        command(
            &fixture,
            "/bin/sh",
            &[
                "-c",
                "printf owned-out; printf owned-err >&2; /bin/sleep 120 &",
            ],
        ),
        None,
        REAL_BUBBLEWRAP_EXECUTION_TIMEOUT,
    );
    let elapsed = started.elapsed();
    let after = survivor.backend.exited();
    survivor.finalize();
    let cleanup = survivor.collect();
    let survivor_authority = survivor.authority;
    drop(survivor);

    // Interpret nothing until both owners have been explicitly retired.
    assert!(matches!(before, Ok(false)), "before={before:?}");
    assert!(matches!(after, Ok(false)), "after={after:?}");
    let survivor_status = cleanup.unwrap();
    assert_eq!(survivor_authority, Authority::Reaped);
    assert_eq!(survivor_status.signal(), Some(libc::SIGKILL));
    let output = primary.unwrap();
    assert!(output.status.success(), "status={:?}", output.status);
    assert_eq!(output.stdout, b"owned-out");
    assert_eq!(output.stderr, b"owned-err");
    assert!(
        elapsed < Duration::from_secs(5),
        "capture cleanup took {elapsed:?}"
    );
}
