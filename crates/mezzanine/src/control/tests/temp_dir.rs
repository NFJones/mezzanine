//! Temporary filesystem fixture owned by the control protocol tests.
//!
//! Control tests use private roots for snapshots and related persistent state.
//! This guard creates unique directories and removes them when each test ends.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TEST_TEMP_ID: AtomicU64 = AtomicU64::new(1);

/// Owns one automatically cleaned temporary directory.
#[derive(Debug)]
pub(super) struct TestTempDir {
    path: PathBuf,
}

impl TestTempDir {
    /// Creates a unique temporary directory beneath the system temp root.
    pub(super) fn new(label: &str) -> Self {
        let unique = NEXT_TEST_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mez-{label}-{}-{nanos}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap_or_else(|error| {
            panic!(
                "failed to create test temp directory {}: {error}",
                path.display()
            )
        });
        Self { path }
    }

    /// Returns this temporary directory as a path.
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Returns a child path beneath this temporary directory.
    pub(super) fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.path.join(path)
    }
}

impl AsRef<Path> for TestTempDir {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

impl Deref for TestTempDir {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.path()
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
