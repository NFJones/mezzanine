//! Actual descriptor I/O under deliberately revoked explicit work controls.
//!
//! These cases observe real cursor/length/content changes, not just callback
//! counts. Files are private fixture data; no permission or commit is inferred
//! from a successful checkpoint.

use super::*;
use std::io::Seek;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Owns one private ordinary file for read/write/verification controls.
struct Fixture {
    /// Unique subtree removed after every descriptor use finishes.
    root: PathBuf,
    /// Exact fixture file whose cursor and bytes are observable.
    path: PathBuf,
}
impl Fixture {
    /// Creates an actual regular file with the supplied initial bytes.
    fn new(bytes: &[u8]) -> Self {
        let root = std::env::temp_dir().join(format!("mez-native-io-{}", rand::random::<u128>()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("file");
        std::fs::write(&path, bytes).unwrap();
        Self { root, path }
    }
    /// Opens both directions without replacing or truncating the fixture.
    fn open(&self) -> File {
        File::options()
            .read(true)
            .write(true)
            .open(&self.path)
            .unwrap()
    }
}
impl Drop for Fixture {
    /// Removes only the unique fixture subtree after normal or failed checks.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Revokes the actual operation on the requested checkpoint, without globals.
fn cancel_on(checkpoint: usize) -> NativeIoControl {
    let count = AtomicUsize::new(0);
    NativeIoControl::new(move || {
        if count.fetch_add(1, Ordering::Relaxed) + 1 >= checkpoint {
            Err(MezError::conflict("original native I/O work cancelled"))
        } else {
            Ok(())
        }
    })
}

/// Revocation after the first read must stop before a second quantum, even
/// though the original regular file remains readable and below its byte limit.
#[test]
fn read_stops_between_real_descriptor_quanta() {
    let fixture = Fixture::new(&vec![b'x'; NATIVE_IO_QUANTUM * 4]);
    let mut file = fixture.open();
    let error = cancel_on(2)
        .read_bounded(&mut file, (NATIVE_IO_QUANTUM * 4) as u64)
        .unwrap_err();
    assert!(error.message().contains("work cancelled"));
    assert_eq!(file.stream_position().unwrap(), NATIVE_IO_QUANTUM as u64);
}

/// Partial staging writes remain bounded by cooperative checks; the primitive
/// cannot silently finish the full payload after its original budget is revoked.
#[test]
fn write_stops_between_real_descriptor_quanta() {
    let fixture = Fixture::new(&[]);
    let mut file = fixture.open();
    let error = cancel_on(2)
        .write_all(&mut file, &vec![b'x'; NATIVE_IO_QUANTUM * 4])
        .unwrap_err();
    assert!(error.message().contains("work cancelled"));
    assert_eq!(file.metadata().unwrap().len(), NATIVE_IO_QUANTUM as u64);
}

/// Both hashing and positional verification must observe revocation during a
/// multi-quantum byte sequence. Ordinary verification also rejects modified data
/// and leaves the caller's file cursor untouched.
#[test]
fn digest_and_verification_observe_original_control() {
    let bytes = vec![b'x'; NATIVE_IO_QUANTUM * 4];
    let fixture = Fixture::new(&bytes);
    let mut file = fixture.open();
    let control = NativeIoControl::bounded_only();
    let digest = control.digest(&bytes).unwrap();
    control
        .verify_digest(&file, bytes.len() as u64, &digest)
        .unwrap();
    assert!(cancel_on(3).digest(&bytes).is_err());
    assert!(
        cancel_on(3)
            .verify_digest(&file, bytes.len() as u64, &digest)
            .is_err()
    );
    assert_eq!(file.stream_position().unwrap(), 0);
    file.write_at(b"changed", NATIVE_IO_QUANTUM as u64).unwrap();
    assert!(
        control
            .verify_digest(&file, bytes.len() as u64, &digest)
            .is_err()
    );
}

/// A bounded read returns exact raw bytes but rejects growth beyond its limit
/// and arithmetic overflow instead of weakening the limit or treating it as EOF.
#[test]
fn bounded_read_preserves_bytes_and_rejects_oversize() {
    let bytes = vec![b'x'; NATIVE_IO_QUANTUM + 1];
    let fixture = Fixture::new(&bytes);
    let control = NativeIoControl::bounded_only();
    assert_eq!(
        control
            .read_bounded(&mut fixture.open(), bytes.len() as u64)
            .unwrap(),
        bytes
    );
    assert!(
        control
            .read_bounded(&mut fixture.open(), NATIVE_IO_QUANTUM as u64)
            .is_err()
    );
    assert!(control.read_bounded(&mut fixture.open(), u64::MAX).is_err());
}
