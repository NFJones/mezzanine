//! Isolated native private-state qualification; never uses real HOME/config.

use super::*;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};

/// Reserved lock aliases must be rejected before I/O on every platform, not
/// merely fail to match on case-sensitive Linux. Default macOS filesystems can
/// map these spellings to the live lock, so read/update/delete all refuse them
/// without damaging its inode or allowing a second writer to acquire ownership.
#[test]
fn bootstrap_private_state_rejects_case_insensitive_lock_aliases() {
    let fixture = Fixture::new();
    let mut state = fixture.inspect().unwrap();
    state.acquire().unwrap();
    let lock = state.lock.as_ref().unwrap().metadata().unwrap();
    for alias in ["LOCK", "Lock", "lOcK"] {
        let error = state
            .read(alias, 32)
            .expect_err("reserved lock alias must reject");
        assert!(error.to_string().contains("filename unavailable"));
        for after in [None, Some(b"replacement".as_slice())] {
            let error = state
                .publish(alias, Some(b""), after, 32)
                .expect_err("lock aliases cannot authorize mutation");
            assert!(error.to_string().contains("filename unavailable"));
        }
    }
    let current = std::fs::metadata(state.path.join(LOCK)).unwrap();
    assert_eq!((lock.dev(), lock.ino()), (current.dev(), current.ino()));
    assert_eq!(current.len(), 0);
    assert_eq!(current.nlink(), 1);
    state.acquire().unwrap();
    state
        .publish("journal.json", None, Some(b"still owned"), 32)
        .unwrap();
    let mut contender = fixture.inspect().unwrap();
    assert!(contender.acquire().is_err());
}

/// Replacing a lock pathname while its original holder survives must fence
/// that holder before reusing ownership or publishing. A fresh writer may own
/// the new lock; dropping the old holder cannot release the replacement lock.
/// Admission also rechecks held lock links, privacy, and zero-byte content.
#[test]
fn bootstrap_private_state_rejects_replaced_or_modified_retained_lock() {
    for alteration in ["unlink", "rename", "hardlink", "mode", "content"] {
        let fixture = Fixture::new();
        let mut old = fixture.inspect().unwrap();
        old.acquire().unwrap();
        let lock = old.path.join(LOCK);
        match alteration {
            "unlink" | "rename" => {
                if alteration == "unlink" {
                    std::fs::remove_file(&lock).unwrap();
                } else {
                    std::fs::rename(&lock, old.path.join("old-lock")).unwrap();
                }
                let descriptor = openat(
                    old.directory.file().unwrap(),
                    LOCK,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::RUSR | Mode::WUSR,
                )
                .unwrap();
                drop(descriptor);
            }
            "hardlink" => std::fs::hard_link(&lock, old.path.join("lock-alias")).unwrap(),
            "mode" => {
                std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap()
            }
            "content" => std::fs::write(&lock, b"not a lock").unwrap(),
            _ => unreachable!(),
        }
        let mut current = fixture.inspect().unwrap();
        if matches!(alteration, "unlink" | "rename") {
            current.acquire().unwrap();
        } else {
            assert!(current.acquire().is_err());
        }
        assert!(old.acquire().is_err(), "{alteration}");
        assert!(
            old.publish("journal.json", None, Some(b"stale writer"), 32)
                .is_err(),
            "{alteration}"
        );
        assert!(!old.path.join("journal.json").exists());
        drop(old);
        if matches!(alteration, "unlink" | "rename") {
            let mut contender = fixture.inspect().unwrap();
            assert!(contender.acquire().is_err());
            current
                .publish("journal.json", None, Some(b"current writer"), 32)
                .unwrap();
        }
    }
}

/// An ancestor can be replaced while the original namespace inode is moved
/// intact into the new chain. Keeping only HOME/final witnesses misses this
/// sequence; every captured intermediate incarnation must fence old access.
/// Fresh inspection is allowed, but retains genuine live lock exclusion.
#[test]
fn bootstrap_private_state_rejects_swapped_ancestor_with_same_namespace() {
    for suffix in [
        ".config",
        ".config/mezzanine",
        ".config/mezzanine/bootstrap",
    ] {
        let fixture = Fixture::new();
        let mut old = fixture.inspect().unwrap();
        old.acquire().unwrap();
        old.publish("journal.json", None, Some(b"accepted"), 32)
            .unwrap();
        let original = old.directory.file().unwrap().metadata().unwrap();
        let ancestor = fixture.home.join(suffix);
        let relative = old.path.strip_prefix(&ancestor).unwrap().to_path_buf();
        let moved = fixture.root.join("moved-ancestor");
        std::fs::rename(&ancestor, &moved).unwrap();
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(old.path.parent().unwrap())
            .unwrap();
        std::fs::rename(moved.join(relative), &old.path).unwrap();
        let current = std::fs::metadata(&old.path).unwrap();
        assert_eq!(
            (current.dev(), current.ino()),
            (original.dev(), original.ino())
        );
        assert!(old.read("journal.json", 32).is_err(), "{suffix}");
        assert!(
            old.publish("journal.json", Some(b"accepted"), Some(b"wrong"), 32)
                .is_err(),
            "{suffix}"
        );
        let mut fresh = fixture.inspect().unwrap();
        assert_eq!(
            fresh.read("journal.json", 32).unwrap().as_deref(),
            Some(b"accepted".as_slice())
        );
        assert!(fresh.acquire().is_err());
        drop(old);
        fresh.acquire().unwrap();
        assert_eq!(
            fresh.read("journal.json", 32).unwrap().as_deref(),
            Some(b"accepted".as_slice())
        );
    }
}

/// Owns one unique physical temporary fixture, with separate HOME/vendor trees.
/// Cleanup removes only this test-created workspace, including relocated state.
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    vendor: PathBuf,
}

impl Fixture {
    /// Creates explicit private fixture anchors without ambient HOME mutation.
    fn new() -> Self {
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "mez-private-state-{}",
                crate::storage::token_usage::new_token_usage_event_id()
            ));
        let home = root.join("home");
        let vendor = root.join("vendor");
        for path in [&root, &home, &vendor] {
            std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        Self { root, home, vendor }
    }

    /// Captures production private state using actual held fixture root evidence.
    fn inspect(&self) -> Result<StateDirectory> {
        StateDirectory::inspect(&self.home, &File::open(&self.vendor).unwrap())
    }
}

impl Drop for Fixture {
    /// Best-effort test-only cleanup avoids double panic on assertion failures.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Absent inspection must not create config/state/lock files or grant mutation.
/// Explicit acquisition creates only private managed suffixes; bounded bytes
/// round-trip, exact replacements/deletions work, and staging never remains.
#[test]
fn bootstrap_private_state_inspection_and_publication_are_distinct() {
    let fixture = Fixture::new();
    let mut state = fixture.inspect().unwrap();
    assert_eq!(state.read("journal.json", 64).unwrap(), None);
    assert!(!fixture.home.join(".config").exists());
    assert!(
        state
            .publish("journal.json", None, Some(b"private"), 64)
            .is_err()
    );
    assert!(!fixture.home.join(".config").exists());
    state.acquire().unwrap();
    for path in [
        fixture.home.join(".config"),
        fixture.home.join(".config/mezzanine"),
        fixture.home.join(".config/mezzanine/bootstrap"),
        state.path.clone(),
    ] {
        assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o777, 0o700);
    }
    state
        .publish("journal.json", None, Some(b"private"), 64)
        .unwrap();
    assert_eq!(
        state.read("journal.json", 64).unwrap().as_deref(),
        Some(b"private".as_slice())
    );
    assert_eq!(
        std::fs::metadata(state.path.join("journal.json"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        state
            .publish("journal.json", None, Some(b"foreign overwrite"), 64)
            .is_err()
    );
    assert!(
        state
            .publish("journal.json", Some(b"wrong"), None, 64)
            .is_err()
    );
    state
        .publish("journal.json", Some(b"private"), Some(b"updated"), 64)
        .unwrap();
    state
        .publish("journal.json", Some(b"updated"), Some(b"updated"), 64)
        .unwrap();
    state
        .publish("journal.json", Some(b"updated"), None, 64)
        .unwrap();
    assert_eq!(state.read("journal.json", 64).unwrap(), None);
    assert_eq!(std::fs::read_dir(&state.path).unwrap().count(), 1);
    assert_eq!(std::fs::read_dir(&fixture.vendor).unwrap().count(), 0);
}

/// Existing readable config ancestors remain byte/mode-identical. Inspection
/// and acquisition never chmod them. Shared .config is routing only, while HOME,
/// the owned Mez boundary and private managed leaves remain fail-closed.
#[test]
fn bootstrap_private_state_preserves_config_ancestors_and_refuses_unsafe_modes() {
    let fixture = Fixture::new();
    for suffix in [".config", ".config/mezzanine"] {
        let path = fixture.home.join(suffix);
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&path)
            .unwrap();
        std::fs::write(path.join("authored"), b"unchanged").unwrap();
    }
    let mut state = fixture.inspect().unwrap();
    state.acquire().unwrap();
    for suffix in [".config", ".config/mezzanine"] {
        let path = fixture.home.join(suffix);
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read(path.join("authored")).unwrap(), b"unchanged");
    }
    drop(state);
    for suffix in [
        "",
        ".config",
        ".config/mezzanine",
        ".config/mezzanine/bootstrap",
    ] {
        let path = fixture.home.join(suffix);
        let original = std::fs::metadata(&path).unwrap().permissions();
        let unsafe_mode = if suffix.ends_with("bootstrap") {
            0o755
        } else {
            0o777
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(unsafe_mode)).unwrap();
        if suffix == ".config" {
            let mut shared = fixture
                .inspect()
                .expect("routing ancestor is not private data");
            shared.acquire().unwrap();
        } else {
            assert!(fixture.inspect().is_err(), "{suffix}");
        }
        assert_eq!(
            std::fs::metadata(&path).unwrap().mode() & 0o777,
            unsafe_mode
        );
        std::fs::set_permissions(&path, original).unwrap();
    }
}

/// Physical aliases share a namespace and cooperating lock, independent of
/// root path spelling or harness selection. Other root incarnations get a
/// different namespace. Read-only peers never release the writer's ownership.
#[test]
fn bootstrap_private_state_namespace_and_lock_bind_physical_root() {
    let fixture = Fixture::new();
    let vendor = File::open(&fixture.vendor).unwrap();
    let mut holder = StateDirectory::inspect(&fixture.home, &vendor).unwrap();
    holder.acquire().unwrap();
    let alias = File::open(fixture.vendor.join(".")).unwrap();
    let read_only = StateDirectory::inspect(&fixture.home, &alias).unwrap();
    assert_eq!(holder.path, read_only.path);
    let other_root = fixture.root.join("other-vendor");
    std::fs::create_dir(&other_root).unwrap();
    let other = StateDirectory::inspect(&fixture.home, &File::open(other_root).unwrap()).unwrap();
    assert_ne!(other.path, holder.path);
    drop(read_only);
    let mut contender = fixture.inspect().unwrap();
    assert!(contender.acquire().is_err());
    holder
        .publish("receipt.json", None, Some(b"owned"), 16)
        .unwrap();
    drop(holder);
    contender.acquire().unwrap();
    assert_eq!(
        contender.read("receipt.json", 16).unwrap().as_deref(),
        Some(b"owned".as_slice())
    );
}

/// Two initially absent witnesses cannot silently adopt newly appeared state.
/// A fresh inspection after the first writer finishes can acquire it, but the
/// old absent holder rejects even read/noop mutation without changing bytes.
#[test]
fn bootstrap_private_state_absence_is_fenced_before_creation() {
    let fixture = Fixture::new();
    let mut first = fixture.inspect().unwrap();
    let mut stale = fixture.inspect().unwrap();
    first.acquire().unwrap();
    first
        .publish("journal.json", None, Some(b"accepted"), 16)
        .unwrap();
    drop(first);
    assert!(stale.read("journal.json", 16).is_err());
    assert!(stale.acquire().is_err());
    let mut current = fixture.inspect().unwrap();
    current.acquire().unwrap();
    assert_eq!(
        current.read("journal.json", 16).unwrap().as_deref(),
        Some(b"accepted".as_slice())
    );
}

/// HOME/namespace relocation and symlink replacement cannot retarget held
/// private descriptors. An unchanged original file remains under the relocated
/// tree, while replacement paths stay untouched and stale read/write fail.
#[test]
fn bootstrap_private_state_rejects_replaced_home_and_namespace() {
    for replacement in ["home", "namespace", "symlink"] {
        let fixture = Fixture::new();
        let mut state = fixture.inspect().unwrap();
        state.acquire().unwrap();
        state
            .publish("journal.json", None, Some(b"accepted"), 16)
            .unwrap();
        let (old, moved) = if replacement == "home" {
            (fixture.home.clone(), fixture.root.join("moved-home"))
        } else {
            (state.path.clone(), state.path.with_extension("moved"))
        };
        std::fs::rename(&old, &moved).unwrap();
        if replacement == "symlink" {
            symlink(&moved, &old).unwrap();
        } else {
            std::fs::DirBuilder::new().mode(0o700).create(&old).unwrap();
        }
        assert!(state.read("journal.json", 16).is_err());
        assert!(
            state
                .publish("journal.json", Some(b"accepted"), Some(b"wrong"), 16)
                .is_err()
        );
        let preserved = if replacement == "home" {
            moved
                .join(state.path.strip_prefix(&fixture.home).unwrap())
                .join("journal.json")
        } else {
            moved.join("journal.json")
        };
        assert_eq!(std::fs::read(preserved).unwrap(), b"accepted");
        if replacement != "symlink" {
            assert_eq!(std::fs::read_dir(old).unwrap().count(), 0);
        }
    }
}

/// Every protected namespace ancestor rejects symlink aliases during initial
/// inspection. Unsafe HOME inputs, non-directory vendor evidence and traversal
/// fail before any state files or directories are created.
#[test]
fn bootstrap_private_state_rejects_symlink_ancestors_and_invalid_inputs() {
    for suffix in [
        ".config",
        ".config/mezzanine",
        ".config/mezzanine/bootstrap",
    ] {
        let fixture = Fixture::new();
        let target = fixture.root.join("foreign");
        std::fs::create_dir(&target).unwrap();
        let selected = fixture.home.join(suffix);
        std::fs::create_dir_all(selected.parent().unwrap()).unwrap();
        symlink(&target, selected).unwrap();
        assert!(fixture.inspect().is_err());
        assert_eq!(std::fs::read_dir(target).unwrap().count(), 0);
    }
    let fixture = Fixture::new();
    let vendor = File::open(&fixture.vendor).unwrap();
    for home in [
        PathBuf::from("relative"),
        fixture.home.join("../home"),
        fixture.root.join("absent"),
    ] {
        assert!(StateDirectory::inspect(&home, &vendor).is_err());
    }
    let regular = fixture.root.join("regular");
    std::fs::write(&regular, b"not a directory").unwrap();
    assert!(StateDirectory::inspect(&fixture.home, &File::open(regular).unwrap()).is_err());
    assert!(!fixture.home.join(".config").exists());
}

/// File admission never follows symlinks, blocks on FIFOs, accepts hard links,
/// reads nonprivate files or allocates past caller/global byte limits. Invalid
/// names cannot collide with the lock/staging namespace or escape the directory.
#[test]
fn bootstrap_private_state_rejects_unsafe_files_and_finite_bounds() {
    let fixture = Fixture::new();
    let mut state = fixture.inspect().unwrap();
    state.acquire().unwrap();
    state.publish("owned", None, Some(b"bounded"), 32).unwrap();
    assert!(state.read("owned", 3).is_err());
    assert!(state.read("owned", MAX_BYTES + 1).is_err());
    assert!(state.publish("large", None, Some(b"oversized"), 3).is_err());
    for name in [
        "",
        "../escape",
        "sub/file",
        "/absolute",
        "lock",
        ".stage-test",
        "x\n",
        "unicode-é",
        "a..b",
    ] {
        assert!(state.read(name, 32).is_err(), "{name}");
        assert!(
            state.publish(name, None, Some(b"bad"), 32).is_err(),
            "{name}"
        );
    }
    let outside = fixture.root.join("outside");
    std::fs::write(&outside, b"foreign").unwrap();
    symlink(&outside, state.path.join("symlink")).unwrap();
    assert!(state.read("symlink", 32).is_err());
    let fifo = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(
        state.path.join("pipe").as_os_str(),
    ))
    .unwrap();
    // SAFETY: fifo is a NUL-terminated name under this owned unique fixture.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let started = std::time::Instant::now();
    assert!(state.read("pipe", 32).is_err());
    assert!(state.publish("pipe", None, Some(b"wrong"), 32).is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    std::fs::hard_link(state.path.join("owned"), state.path.join("linked")).unwrap();
    assert!(state.read("owned", 32).is_err());
    assert!(state.read("linked", 32).is_err());
    std::fs::remove_file(state.path.join("linked")).unwrap();
    std::fs::set_permissions(
        state.path.join("owned"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(state.read("owned", 32).is_err());
    assert_eq!(std::fs::read(outside).unwrap(), b"foreign");
    assert_eq!(std::fs::read_dir(&fixture.vendor).unwrap().count(), 0);
}
