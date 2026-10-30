//! Partial-startup controls for the same guard used by the real CLI constructors.
//!
//! Boundary faults are local to one fixture and precede child creation. The
//! missing-executable case exercises a real portable-pty spawn error after the
//! reader starts, and the exit case uses a genuine short-lived child. No global
//! resource exhaustion or user processes are required.

use super::*;

/// Reader cloning, writer acquisition and thread construction may each fail.
/// The guarded sequence must return the primary error, affirmatively retire its
/// acquired resources, and never execute the command at a later startup stage.
/// The command's external marker would expose an accidental or replayed spawn.
#[test]
fn foreground_fixture_partial_setup_faults_retire_without_spawning() {
    for (name, fault, expected) in [
        (
            "clone-fault",
            SetupFault::ReaderClone,
            "reader clone failure",
        ),
        (
            "writer-fault",
            SetupFault::Writer,
            "writer acquisition failed",
        ),
        (
            "thread-fault",
            SetupFault::ReaderThread,
            "reader thread creation failure",
        ),
    ] {
        let root = test_root(name);
        let marker = root.with_extension("child-started");
        fs::create_dir(&root).unwrap();
        let owned = storage::OwnedRoot::capture(&root).unwrap();
        let pair = native_pty_system().openpty(PtySize::default()).unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", "printf started > \"$1\"", "fixture"]);
        command.arg(&marker);
        let result = ForegroundProcess::spawn_with_fault(pair, command, owned, name, Some(fault));
        let error = match result {
            Ok(_) => panic!("fault must stop startup"),
            Err(error) => error,
        };
        assert!(error.contains(expected), "{error}");
        assert!(error.contains("fixture cleanup: Retired"), "{error}");
        assert!(
            !marker.exists(),
            "a failed preparation must not launch the child"
        );
        assert!(
            !root.exists(),
            "affirmatively retired partial setup removes its own root"
        );
    }
}

/// A genuine child-spawn failure happens after the reader is live. Cancellation
/// must settle that reader and report both the original spawn error and explicit
/// cleanup outcome, rather than synchronously joining a potentially quiet PTY.
#[test]
fn foreground_fixture_child_spawn_failure_retires_started_reader() {
    let root = test_root("missing-child-executable");
    fs::create_dir(&root).unwrap();
    let owned = storage::OwnedRoot::capture(&root).unwrap();
    let pair = native_pty_system().openpty(PtySize::default()).unwrap();
    let command = CommandBuilder::new(root.join("missing-executable"));
    let started = Instant::now();
    let result = ForegroundProcess::spawn(pair, command, owned, "spawn-failure-reader");
    let error = match result {
        Ok(_) => panic!("missing executable must fail"),
        Err(error) => error,
    };
    assert!(error.contains("child spawn failed"), "{error}");
    assert!(error.contains("fixture cleanup: Retired"), "{error}");
    assert!(started.elapsed() < FIXTURE_RETIREMENT_BOUND + Duration::from_secs(1));
    assert!(!root.exists());
}

/// A child may exit before its caller first interacts with the fixture. The
/// startup guard retains that same child handle, and explicit retirement must
/// observe and reap it without replaying setup or treating Drop as completion.
#[test]
fn foreground_fixture_immediate_child_exit_stays_owned_until_retirement() {
    let root = test_root("immediate-child-exit");
    fs::create_dir(&root).unwrap();
    let owned = storage::OwnedRoot::capture(&root).unwrap();
    let pair = native_pty_system().openpty(PtySize::default()).unwrap();
    let mut command = CommandBuilder::new("/bin/sh");
    command.args(["-c", "exit 0"]);
    let mut fixture =
        ForegroundProcess::spawn(pair, command, owned, "immediate-exit-reader").unwrap();
    fixture.wait_for_exit(Duration::from_secs(2)).unwrap();
    assert!(
        root.exists(),
        "observing exit alone does not retire storage"
    );
    assert_eq!(fixture.retire(), FixtureCleanup::Retired);
    assert!(!root.exists());
}
