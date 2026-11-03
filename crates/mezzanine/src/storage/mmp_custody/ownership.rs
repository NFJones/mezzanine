//! Lifetime exclusion for cooperating runtime custody roots.
//!
//! SQLite serializes individual transactions, but an owned custody attempt can
//! span several transactions and continue after its observer disappears. This
//! private lock fences other runtime roots until the actual owner and its
//! workers retire. It does not grant delivery authority, validate recovery, or
//! prevent direct use of the lower-level multi-connection store API. The store
//! directory remains trusted against replacement by the same user.

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

use rustix::fs::{FlockOperation, OFlags, flock};

use super::validate_private_path;
use crate::error::{MezError, Result};

/// A cooperating runtime root's exclusive, kernel-owned lifetime fence.
pub(crate) struct RootOwnership {
    /// Closing the final owner releases the lock, including on process exit.
    _lock: File,
}

impl RootOwnership {
    /// Acquires a nonblocking fence before a runtime root opens its store.
    /// Rejects unsafe lock paths and an already-owned destination.
    pub(crate) fn acquire(root: &Path) -> Result<Self> {
        if !root.exists() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder.create(root)?;
        }
        validate_private_path(root, true)?;
        let path = root.join("owner.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC).bits() as i32)
            .open(&path)?;
        let owned = lock.metadata()?;
        if !owned.is_file()
            || owned.nlink() != 1
            || owned.uid() != rustix::process::geteuid().as_raw()
            || owned.mode() & 0o077 != 0
        {
            return Err(MezError::invalid_state(
                "MMP custody owner lock must be a private owned single-link file",
            ));
        }
        match flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => (),
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => {
                return Err(MezError::conflict("MMP custody root already owned"));
            }
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
        let current = fs::symlink_metadata(&path)?;
        if !current.is_file()
            || current.dev() != owned.dev()
            || current.ino() != owned.ino()
            || current.nlink() != 1
            || current.uid() != owned.uid()
            || current.mode() & 0o077 != 0
        {
            return Err(MezError::conflict("MMP custody owner lock path changed"));
        }
        Ok(Self { _lock: lock })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    const CHILD_ROOT: &str = "MEZ_TEST_MMP_OWNER_CHILD_ROOT";
    const READY: &str = "MMP_OWNER_CHILD_READY";

    /// Runs only when explicitly invoked as a child by the process-exit test.
    /// It holds the actual OS lock until the parent forcibly terminates it.
    #[test]
    fn lock_child() {
        let Some(root) = std::env::var_os(CHILD_ROOT) else {
            return;
        };
        let _owner = RootOwnership::acquire(Path::new(&root)).unwrap();
        println!("{READY}");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }

    /// A generated child and its private destination are always retired even
    /// when an assertion fails, so the test cannot leave a process holding a lock.
    struct ChildFixture {
        child: Child,
        root: PathBuf,
    }

    impl Drop for ChildFixture {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// An independently spawned process owns the same real filesystem lock,
    /// blocks another owner, and releases it when killed without Rust cleanup.
    /// This checks the crash boundary rather than merely dropping a local guard.
    #[test]
    fn process_exit_releases_actual_owner_lock() {
        let root = std::env::temp_dir().join(format!(
            "mez-mmp-owner-process-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "storage::mmp_custody::ownership::tests::lock_child",
                "--nocapture",
            ])
            .env(CHILD_ROOT, &root)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let mut fixture = ChildFixture { child, root };
        let (send, receive) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                match line {
                    Ok(line) if line.contains(READY) => {
                        let _ = send.send(());
                        return;
                    }
                    Ok(_) => (),
                    Err(_) => break,
                }
            }
        });
        assert!(receive.recv_timeout(Duration::from_secs(10)).is_ok());
        assert!(RootOwnership::acquire(&fixture.root).is_err());
        fixture.child.kill().unwrap();
        fixture.child.wait().unwrap();
        reader.join().unwrap();
        let _replacement = RootOwnership::acquire(&fixture.root).unwrap();
    }
}
