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
//! best effort with no guaranteed secondary diagnostic reporting. Generated
//! fixture storage defaults to preservation before any spawn attempt and is
//! deleted only after explicit settlement of EVERY reserved owner. Unknown reap,
//! capture/input, setup/spawn failure and unwind preserve the root/socket instead
//! of deleting possible cleanup evidence. This is kernel-fixture-only policy.

use std::mem::ManuallyDrop;
use std::os::unix::process::ExitStatusExt;

use super::super::{REAL_BUBBLEWRAP_EXECUTION_TIMEOUT, RealBubblewrapFixture};
use super::fixture_policy::{FixturePolicy, Settlement};
use super::*;

/// Defaults to preserving generated paths without changing the ordinary real
/// sandbox fixture destructor. No owner lifetime can authorize deletion by Drop.
struct KernelFixture {
    paths: ManuallyDrop<RealBubblewrapFixture>,
    policy: FixturePolicy,
}

impl KernelFixture {
    /// Creates generated storage before any associated child can be spawned.
    fn new(label: &str) -> Self {
        Self {
            paths: ManuallyDrop::new(RealBubblewrapFixture::new(label)),
            policy: FixturePolicy::default(),
        }
    }

    /// Reserves preservation responsibility before even constructing the command;
    /// failed spawn attempts deliberately remain unsettled as a conservative rule.
    fn spawn(
        &mut self,
        executable: &str,
        args: &[&str],
        input: Option<(Vec<u8>, Vec<u8>)>,
    ) -> Result<(usize, Owner<LiveWorkload>), String> {
        let slot = self.policy.reserve();
        spawn_owner(
            command(&self.paths, executable, args),
            input,
            REAL_BUBBLEWRAP_EXECUTION_TIMEOUT,
        )
        .map(|owner| (slot, owner))
        .map_err(|error| {
            format!(
                "{error}; fixture preserved at {}",
                self.paths.root.display()
            )
        })
    }

    /// Collects, verifies actual root/I/O retirement, and closes every descriptor
    /// before settling this exact slot. Any failure preserves fixture evidence.
    fn collect(&mut self, slot: usize, mut owner: Owner<LiveWorkload>) -> Result<Output, String> {
        let collected = owner.collect();
        let mut proof = Settlement {
            collection_ok: collected.is_ok(),
            root_reaped: owner.authority == Authority::Reaped,
            capture_eof: owner.backend.out.eof && owner.backend.err.eof,
            input_closed: owner.backend.input.is_none() && owner.backend.stdin.is_none(),
            retired: owner.retired,
            handles_closed: false,
        };
        let stdout = std::mem::take(&mut owner.backend.out.bytes);
        let stderr = std::mem::take(&mut owner.backend.err.bytes);
        drop(owner);
        proof.handles_closed = true;
        let settled = self.policy.settle(slot, proof);
        match collected {
            Ok(status) if settled => Ok(Output {
                status,
                stdout,
                stderr,
            }),
            result => Err(format!(
                "fixture settlement unconfirmed: {result:?}; fixture preserved at {}; stdout={:?}; stderr={:?}",
                self.paths.root.display(),
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            )),
        }
    }
}

impl Drop for KernelFixture {
    /// Runs the original fixture destructor only with all-owner settlement proof.
    /// Preservation deliberately retains the generated paths and their small
    /// PathBuf allocations until the test process exits; no diagnostic I/O runs.
    fn drop(&mut self) {
        if self.policy.can_delete() {
            // SAFETY: paths is initialized once, never moved/dropped elsewhere,
            // and ManuallyDrop suppresses the field's automatic destructor.
            unsafe { ManuallyDrop::drop(&mut self.paths) };
        }
    }
}

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
    let mut fixture = KernelFixture::new("kernel-wnowait");
    let (slot, mut owner) = fixture
        .spawn(
            "/bin/sh",
            &["-c", "printf owned-out; printf owned-err >&2; exit 7"],
            None,
        )
        .unwrap();
    let observed = observe_exit_twice(&mut owner);
    let collected = fixture.collect(slot, owner);

    observed.unwrap();
    let output = collected.unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"owned-out");
    assert_eq!(output.stderr, b"owned-err");
}

#[test]
/// Exercises real nonblocking input/output pressure without a writer thread.
/// Staged bytes exceed individual I/O quotas but stay below the capture cap;
/// exact equality forbids truncation, duplication, or dropped short-write bytes.
/// The 50ms staging separation itself remains a virtual-clock journal assertion.
fn kernel_staged_input_and_capture_preserve_all_bounded_bytes() {
    let mut fixture = KernelFixture::new("kernel-staged-io");
    let wrapper = vec![b'w'; 128 * 1024];
    let payload = vec![b'p'; 128 * 1024];
    let mut expected = wrapper.clone();
    expected.extend_from_slice(&payload);
    let (slot, owner) = fixture
        .spawn("/bin/cat", &[], Some((wrapper, payload)))
        .unwrap();
    let collected = fixture.collect(slot, owner);

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
    let mut fixture = KernelFixture::new("kernel-pipe-owner");
    let (survivor_slot, mut survivor) = fixture.spawn("/bin/sleep", &["120"], None).unwrap();
    let before = survivor.backend.exited();
    let started = Instant::now();
    let primary = fixture
        .spawn(
            "/bin/sh",
            &[
                "-c",
                "printf owned-out; printf owned-err >&2; /bin/sleep 120 &",
            ],
            None,
        )
        .and_then(|(slot, owner)| fixture.collect(slot, owner));
    let elapsed = started.elapsed();
    let after = survivor.backend.exited();
    survivor.finalize();
    let cleanup = fixture.collect(survivor_slot, survivor);

    // Interpret nothing until both owners have been explicitly retired.
    assert!(matches!(before, Ok(false)), "before={before:?}");
    assert!(matches!(after, Ok(false)), "after={after:?}");
    let survivor_output = cleanup.unwrap();
    assert_eq!(survivor_output.status.signal(), Some(libc::SIGKILL));
    let output = primary.unwrap();
    assert!(output.status.success(), "status={:?}", output.status);
    assert_eq!(output.stdout, b"owned-out");
    assert_eq!(output.stderr, b"owned-err");
    assert!(
        elapsed < Duration::from_secs(5),
        "capture cleanup took {elapsed:?}"
    );
}
