//! Lease delivery controls against actual held descriptor capabilities.
//!
//! These isolate deadline, capture and fence identity from actor policy. Actor
//! admission and replay rejection are covered by canonical runtime fixtures.

use super::super::commit::{NativePatchFenceStatus, NativePatchWriteCoordinator};
use super::*;
use crate::runtime::processes::native_cancellation::NativeActionCancellation;
use crate::security::filesystem::{capability::NativeFilesystemRoot, host_resolved_path_scopes};
use sha2::{Digest, Sha256};

/// Actual private file capture and current-process authority without children.
struct Fixture {
    /// Unique owned directory removed only by this fixture.
    directory: std::path::PathBuf,
    /// Captured root with real process lifetime and held cwd witness.
    root: NativeFilesystemRoot,
    /// Explicit resolved grants for only the private fixture directory.
    scopes: PathScopes,
    /// Exact missing entry and proposed final bytes.
    change: ApplyPatchFileChange,
    /// Held original capture whose allocation a valid lease must use.
    capability: NativeFileCapability,
    /// Exact action cancellation and positive in-flight owner.
    fence: NativePatchCommitFence,
}

impl Fixture {
    /// Captures actual descriptor evidence; no mutation happens in this setup.
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("mez-native-lease-{}", rand::random::<u128>()));
        std::fs::create_dir(&directory).unwrap();
        let directory = std::fs::canonicalize(directory).unwrap();
        let root = NativeFilesystemRoot::capture(std::process::id()).unwrap();
        let path = directory.join("file").to_str().unwrap().to_owned();
        let scopes = host_resolved_path_scopes(
            root.cwd(),
            &[directory.to_str().unwrap().to_owned()],
            &[directory.to_str().unwrap().to_owned()],
            &[],
        )
        .unwrap();
        let change = ApplyPatchFileChange {
            path: path.clone(),
            resolved_path: path.clone(),
            original: ApplyPatchOriginalState::Missing,
            final_bytes: Some(b"new\n".to_vec()),
        };
        let capability = NativeFileCapability::capture(&root, &scopes, &path, true).unwrap();
        let fence = NativePatchCommitFence::new(NativeActionCancellation::new(), None).unwrap();
        Self {
            directory,
            root,
            scopes,
            change,
            capability,
            fence,
        }
    }

    /// Issues a test-only admission from the exact captured proposed endpoint.
    fn lease(&self) -> (NativePatchCommitProposal, NativePatchCommitLease) {
        let proposal =
            NativePatchCommitProposal::capture(&self.capability, &self.change, 0, 0).unwrap();
        let lease = NativePatchCommitLease::issue(
            proposal.clone(),
            self.scopes.clone(),
            self.fence
                .try_begin(&NativePatchWriteCoordinator::default())
                .unwrap(),
        )
        .unwrap();
        (proposal, lease)
    }
}

impl Drop for Fixture {
    /// Removes only the fixture's owned subtree, never an external target.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// A lease delayed past its entry deadline cannot become an authorized commit.
/// The deadline is set directly to an elapsed instant, avoiding timing sleeps;
/// dropping rejected admission releases the actual positive in-flight guard.
#[test]
fn expired_delivery_releases_admission_without_entering_commit() {
    let fixture = Fixture::new();
    let (proposal, mut lease) = fixture.lease();
    lease.deadline = Instant::now();
    assert!(lease.enter(&proposal, &fixture.fence).is_err());
    assert_eq!(fixture.fence.cancel(), NativePatchFenceStatus::Fenced);
    assert!(!fixture.directory.join("file").exists());
}

/// Equal spelling and bytes from a fresh capture cannot use an earlier capture's
/// lease. The held capability allocation is mandatory even when both entries
/// are missing, and rejection must release rather than strand the guard.
#[test]
fn equal_path_and_bytes_do_not_substitute_a_different_capture() {
    let fixture = Fixture::new();
    let (_, lease) = fixture.lease();
    let other =
        NativeFileCapability::capture(&fixture.root, &fixture.scopes, &fixture.change.path, true)
            .unwrap();
    let proposal = NativePatchCommitProposal::capture(&other, &fixture.change, 0, 0).unwrap();
    assert!(lease.enter(&proposal, &fixture.fence).is_err());
    assert_eq!(fixture.fence.cancel(), NativePatchFenceStatus::Fenced);
    assert!(!fixture.directory.join("file").exists());
}

/// Changing proposed bytes or supplying another action's fence cannot consume
/// a lease. Both equality gates apply independently of path and scope equality.
#[test]
fn changed_final_digest_and_foreign_fence_cannot_enter_admitted_endpoint() {
    let fixture = Fixture::new();
    let (mut proposal, lease) = fixture.lease();
    proposal.after = Some(Sha256::digest(b"different").into());
    assert!(lease.enter(&proposal, &fixture.fence).is_err());
    let (proposal, lease) = fixture.lease();
    let foreign = NativePatchCommitFence::new(NativeActionCancellation::new(), None).unwrap();
    assert!(lease.enter(&proposal, &foreign).is_err());
    assert_eq!(fixture.fence.cancel(), NativePatchFenceStatus::Fenced);
    assert!(!fixture.directory.join("file").exists());
}

/// Proposal byte proofs must retain the capability's original work control,
/// including revocation during hashing of an actual multi-quantum final image.
/// No proposal or filesystem effect may escape after that original fence fails.
#[test]
fn proposal_hashing_observes_original_capability_work_fence() {
    use crate::security::filesystem::native_io::{NATIVE_IO_QUANTUM, NativeIoControl};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let fixture = Fixture::new();
    let armed = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let (observed_armed, observed_calls) = (armed.clone(), calls.clone());
    let control = NativeIoControl::new(move || {
        if observed_armed.load(Ordering::SeqCst)
            && observed_calls.fetch_add(1, Ordering::SeqCst) >= 4
        {
            Err(MezError::conflict("original proposal byte proof cancelled"))
        } else {
            Ok(())
        }
    });
    let capability = NativeFileCapability::capture_with_io_control(
        &fixture.root,
        &fixture.scopes,
        &fixture.change.path,
        true,
        control,
    )
    .unwrap();
    let mut change = fixture.change.clone();
    change.final_bytes = Some(vec![b'x'; NATIVE_IO_QUANTUM * 4]);
    armed.store(true, Ordering::SeqCst);
    let error = NativePatchCommitProposal::capture(&capability, &change, 0, 0)
        .err()
        .unwrap();
    assert!(
        error
            .message()
            .contains("original proposal byte proof cancelled")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    assert!(!std::path::Path::new(&change.resolved_path).exists());
}
