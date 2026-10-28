//! Fixture-only executable publication for the three standalone bootstrap tests.
//!
//! The destination writer belongs to a dedicated copier child, never to the
//! multithreaded integration-test process whose concurrent Command spawns might
//! inherit an in-process writer between fork and exec. Successful copier exit
//! is the publication boundary. This removes an ownership hazard, not proof of
//! the historical cause of intermittent ETXTBSY. No production spawn policy,
//! retry, shell serialization or vendor installation is introduced here.
//! Unique owner-only roots live beside the Cargo binary so multi-gigabyte real
//! copies do not consume bounded temporary mounts. RAII also removes partial
//! fixture data when setup or assertions unwind.

use std::path::Path;

/// Copy the real executable into a fresh private absolute fixture pathname.
/// Linux and macOS provide /bin/cp with portable -p metadata preservation;
/// literal absolute argv avoids interpretation and option ambiguity. Any
/// spawn/copy/wait failure fails setup with context.
pub(super) fn copy_fixture_helper(source: &Path, helper: &Path) -> std::io::Result<()> {
    if !source.is_absolute() || !helper.is_absolute() || helper.try_exists()? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "copy requires absolute source and fresh absolute helper: {source:?} -> {helper:?}"
            ),
        ));
    }
    let child = std::process::Command::new("/bin/cp")
        .arg("-p")
        .arg(source)
        .arg(helper)
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("start copier for {helper:?}: {error}"),
            )
        })?;
    finish_fixture_copy(child, helper)
}

/// Reap the sole destination writer before reporting executable readiness.
/// Failure is not readiness and never triggers retries or execution of partial
/// bytes. The controlled writer regression exercises this same wait boundary.
fn finish_fixture_copy(child: std::process::Child, helper: &Path) -> std::io::Result<()> {
    let output = child.wait_with_output().map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("wait for copier of {helper:?}: {error}"),
        )
    })?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "copier failed for {helper:?}: {output:?}"
        )));
    }
    Ok(())
}

/// Own private regression-test paths and clean up even after assertion failures.
pub(super) struct PublicationRoot(std::path::PathBuf);

impl PublicationRoot {
    /// Allocate a unique owner-only root, never a shared mutable helper path.
    pub(super) fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        // Cargo supplies the absolute executable path; its filesystem already
        // supports the large binary and does not depend on TMPDIR capacity.
        let directory = Path::new(env!("CARGO_BIN_EXE_mez")).parent().unwrap();
        let root = directory.join(format!(
            "mez-helper-publication-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        Self(root)
    }

    /// Borrows this root without transferring cleanup ownership to the caller.
    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PublicationRoot {
    /// Best-effort cleanup must not mask the original failure during unwind.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Real-copy roots must stay on the Cargo binary filesystem rather than a
/// capacity-limited TMPDIR. Both normal return and a deliberate setup panic
/// remove nonempty roots, including partial helper data, without hiding the
/// original failure or requiring successful end-of-test manual cleanup.
#[test]
fn copied_helper_roots_use_binary_storage_and_clean_normal_and_panicked_setup() {
    use std::os::unix::fs::MetadataExt;
    for panic_setup in [false, true] {
        let mut created = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let owner = PublicationRoot::new();
            let binary = Path::new(env!("CARGO_BIN_EXE_mez"));
            assert_eq!(owner.path().parent(), binary.parent());
            assert_eq!(
                std::fs::metadata(owner.path()).unwrap().dev(),
                std::fs::metadata(binary).unwrap().dev()
            );
            assert_eq!(
                std::fs::metadata(owner.path()).unwrap().mode() & 0o777,
                0o700
            );
            std::fs::write(owner.path().join("partial helper"), b"partial fixture copy").unwrap();
            created = Some(owner.path().to_path_buf());
            if panic_setup {
                panic!("controlled fixture setup failure");
            }
        }));
        assert_eq!(result.is_err(), panic_setup);
        assert!(!created.unwrap().exists());
    }
}

/// Compare every byte using bounded buffers, without hashing large debug builds.
fn assert_identical_executables(source: &Path, helper: &Path) {
    use std::io::Read;
    let mut original = std::fs::File::open(source).unwrap();
    let mut copied = std::fs::File::open(helper).unwrap();
    let length = original.metadata().unwrap().len();
    assert_eq!(copied.metadata().unwrap().len(), length, "{helper:?}");
    let mut original_buffer = [0; 64 * 1024];
    let mut copied_buffer = [0; 64 * 1024];
    let mut offset = 0;
    while offset < length {
        let count = (length - offset).min(original_buffer.len() as u64) as usize;
        original.read_exact(&mut original_buffer[..count]).unwrap();
        copied.read_exact(&mut copied_buffer[..count]).unwrap();
        assert!(
            original_buffer[..count] == copied_buffer[..count],
            "different copied bytes at offset {offset}: {helper:?}"
        );
        offset += count as u64;
    }
}

/// Execute the literal copied path with cleared routing/config environment.
fn assert_published_helper(helper: &Path) {
    let output = std::process::Command::new(helper)
        .arg("harness-event")
        .env_clear()
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("execute published helper {helper:?}: {error}"));
    assert!(output.status.success(), "{helper:?}: {output:?}");
    assert_eq!(output.stdout, b"{}\n", "{helper:?}");
    assert!(output.stderr.is_empty(), "{helper:?}: {output:?}");
}

/// Overlapping publishers must yield real distinct executable copies, not a
/// symlink/shared path or substituted invocation of the Cargo binary. A bounded
/// pair of workers executes difficult pathnames immediately after publication
/// and checks bytes, inode separation and mode. The initial barrier coordinates
/// launch without serializing other tests or stranding a peer on later failure.
#[test]
fn copied_helpers_publish_independent_identical_executables_in_parallel() {
    use std::os::unix::fs::MetadataExt;
    let source = Path::new(env!("CARGO_BIN_EXE_mez"));
    let source_metadata = std::fs::metadata(source).unwrap();
    let barrier = std::sync::Barrier::new(2);
    let roots = [PublicationRoot::new(), PublicationRoot::new()];
    std::thread::scope(|scope| {
        for root in &roots {
            scope.spawn(|| {
                barrier.wait();
                for round in 0..2 {
                    let helper = root.0.join(format!("owned 'quote' $LITERAL; mez-{round}"));
                    copy_fixture_helper(source, &helper).unwrap();
                    assert_published_helper(&helper);
                    let metadata = std::fs::symlink_metadata(&helper).unwrap();
                    assert!(metadata.is_file());
                    assert_ne!(
                        (metadata.dev(), metadata.ino()),
                        (source_metadata.dev(), source_metadata.ino())
                    );
                    assert_eq!(metadata.mode() & 0o777, source_metadata.mode() & 0o777);
                    assert_identical_executables(source, &helper);
                    std::fs::remove_file(helper).unwrap();
                }
            });
        }
    });
}

/// A child-held destination writer is a deterministic Linux ETXTBSY control.
/// The shell copies the real binary, opens its writer and signals READY before
/// blocking on a release pipe. The fixture publication finisher must not report
/// readiness before child exit. Worker-entry synchronization precedes the timed
/// negative observation. The OS error and post-release real exec establish the
/// lifetime edge, not reproduction of the intermittent historical cause.
/// The release pipe closes on unwind before scope join; no sleeps/retries occur.
#[cfg(target_os = "linux")]
#[test]
fn copied_helper_publication_waits_for_child_writer_exit() {
    use std::io::{Read, Write};
    use std::sync::mpsc;
    use std::time::Duration;
    let root = PublicationRoot::new();
    let source = Path::new(env!("CARGO_BIN_EXE_mez"));
    let helper = root.0.join("owned 'quote' $LITERAL; mez");
    let mut child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "/bin/cp -p \"$1\" \"$2\" || exit; exec 3>>\"$2\"; printf READY; read release",
            "fixture-copier",
        ])
        .arg(source)
        .arg(&helper)
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let release = child.stdin.take().unwrap();
    let mut ready = [0; 5];
    if let Err(error) = child.stdout.as_mut().unwrap().read_exact(&mut ready) {
        drop(release);
        let output = child.wait_with_output().unwrap();
        panic!("writer readiness failed: {error}: {output:?}");
    }
    let (entered_tx, entered_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let helper_ref = &helper;
        scope.spawn(move || {
            entered_tx.send(()).unwrap();
            let result = finish_fixture_copy(child, helper_ref);
            let _ = finished_tx.send(result);
        });
        // Move pipe ownership into this closure: unwind closes it before join.
        let mut release = release;
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(ready, *b"READY");
        let before_exit = finished_rx.recv_timeout(Duration::from_millis(100));
        let blocked_exec = std::process::Command::new(&helper)
            .arg("harness-event")
            .env_clear()
            .stdin(std::process::Stdio::null())
            .output();
        release.write_all(b"release\n").unwrap();
        drop(release);
        assert!(
            matches!(before_exit, Err(mpsc::RecvTimeoutError::Timeout)),
            "{before_exit:?}"
        );
        assert_eq!(
            blocked_exec.unwrap_err().raw_os_error(),
            Some(libc::ETXTBSY)
        );
        finished_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
    });
    assert_published_helper(&helper);
    assert_identical_executables(source, &helper);
}

/// Failed publication must not be reported ready or replayed. Missing source
/// returns copier diagnostics; an existing helper is rejected before spawning,
/// preserving its bytes. These controls cover both child failure and input error.
#[test]
fn copied_helper_publication_rejects_failed_copy_and_existing_destination() {
    let root = PublicationRoot::new();
    let helper = root.0.join("owned 'quote' $LITERAL; mez");
    let error = copy_fixture_helper(&root.0.join("missing"), &helper).unwrap_err();
    assert!(error.to_string().contains("copier failed"), "{error}");
    assert!(!helper.exists());
    std::fs::write(&helper, b"PRESERVE").unwrap();
    let error = copy_fixture_helper(Path::new(env!("CARGO_BIN_EXE_mez")), &helper).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(std::fs::read(&helper).unwrap(), b"PRESERVE");
}
