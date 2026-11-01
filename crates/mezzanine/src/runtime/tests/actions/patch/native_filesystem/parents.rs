//! Exact directory-purpose admissions through the actual canonical actor owner.
//!
//! These exercise real queued/claimed sources and short actor leases against
//! private descriptor captures. They do not manufacture worker directory results
//! or certify progress/canonical integration, which remains a subsequent phase.

use super::*;
use crate::runtime::processes::native_patch::commit::NativePatchCommitFence;
use crate::runtime::processes::native_patch::lease::NativePatchCommitProposal;
use crate::security::filesystem::capability::parent::{
    MAX_NATIVE_PARENT_DIRECTORIES, NativeParentCapability,
};
use crate::security::filesystem::capability::{NativeFileCapability, NativeFilesystemRoot};
use crate::security::filesystem::native_io::NativeIoControl;
use mez_agent::permissions::PathScopes;
use mez_agent::semantic_patch_planning::{ApplyPatchFileChange, ApplyPatchOriginalState};

/// Actual separately captured parent and exact intended missing file endpoint.
struct Inputs {
    /// Original prepared file allocation, not just a copied target pathname.
    file: NativeFileCapability,
    /// Independently authorized one-entry directory creation capability.
    parent: NativeParentCapability,
    /// Exact change that the authored create operation will eventually request.
    change: ApplyPatchFileChange,
}
impl Inputs {
    /// Uses the claimed source's real root, scope ceiling and original deadline.
    fn new(
        root: &NativeFilesystemRoot,
        scopes: &PathScopes,
        fence: &NativePatchCommitFence,
        path: &std::path::Path,
    ) -> Self {
        let original = fence.clone();
        let control = NativeIoControl::new(move || original.check());
        let file = NativeFileCapability::capture_with_io_control(
            root,
            scopes,
            path.to_str().unwrap(),
            true,
            control.clone(),
        )
        .unwrap();
        let parent = NativeParentCapability::capture(
            root,
            scopes,
            path.parent().unwrap().to_str().unwrap(),
            control,
        )
        .unwrap();
        let change = ApplyPatchFileChange {
            path: path.to_str().unwrap().into(),
            resolved_path: path.to_str().unwrap().into(),
            original: ApplyPatchOriginalState::Missing,
            final_bytes: Some(b"data\n".to_vec()),
        };
        Self {
            file,
            parent,
            change,
        }
    }
    /// Builds an exact parent purpose whose key is separate from its file key.
    fn proposal(&self, operation: usize, ordinal: usize) -> NativePatchCommitProposal {
        NativePatchCommitProposal::capture_parent(
            &self.parent,
            &self.file,
            &self.change,
            operation,
            ordinal,
        )
        .unwrap()
    }
}

/// A genuine actor-issued directory lease can publish one separately scoped
/// directory without pretending a file endpoint completed. Positive mkdir stays
/// retained after lease release; the undispatched file remains absent.
#[test]
fn actor_parent_admission_uses_original_source_and_separate_directory_purpose() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Add File: {}\n+data\n*** End Patch",
            root.join("parent/file").display()
        )
    });
    let attempt = fixture.queue();
    let dispatch = fixture
        .service
        .claim_native_filesystem_action(&attempt)
        .unwrap()
        .unwrap();
    let root = dispatch.work.source.capture_root().unwrap();
    let input = Inputs::new(
        &root,
        &dispatch.scopes,
        &dispatch.fence,
        &fixture.root.join("parent/file"),
    );
    let proposal = input.proposal(0, 0);
    let lease = fixture
        .service
        .authorize_native_filesystem_commit(&attempt, &dispatch.work.identity, &proposal)
        .unwrap();
    let (scopes, guard) = lease.enter(&proposal, &dispatch.fence).unwrap();
    let mut created = input.parent.commit(&root, &scopes).unwrap();
    drop(guard);
    assert_eq!(created.path(), fixture.root.join("parent"));
    created.observe(&root, &scopes).unwrap();
    let ready = input
        .file
        .recapture_with_created_parents(&root, &scopes, &[created])
        .unwrap();
    assert!(ready.missing_parent_paths().unwrap().is_empty());
    assert!(fixture.root.join("parent").is_dir());
    assert!(!fixture.root.join("parent/file").exists());
}

/// Parent admission capacity belongs to the whole original action, including
/// dropped unused leases. Alternating operations must not reset that allowance;
/// exhausted directory capacity also must not consume the distinct file key.
#[test]
fn actor_parent_admission_budget_is_global_and_dropped_leases_remain_spent() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Add File: {}\n+data\n*** Add File: {}\n+data\n*** End Patch",
            root.join("first/file").display(),
            root.join("second/file").display()
        )
    });
    let attempt = fixture.queue();
    let dispatch = fixture
        .service
        .claim_native_filesystem_action(&attempt)
        .unwrap()
        .unwrap();
    let root = dispatch.work.source.capture_root().unwrap();
    let inputs = [
        Inputs::new(
            &root,
            &dispatch.scopes,
            &dispatch.fence,
            &fixture.root.join("first/file"),
        ),
        Inputs::new(
            &root,
            &dispatch.scopes,
            &dispatch.fence,
            &fixture.root.join("second/file"),
        ),
    ];
    for index in 0..MAX_NATIVE_PARENT_DIRECTORIES {
        let operation = index % 2;
        let proposal = inputs[operation].proposal(operation, index / 2);
        let lease = fixture
            .service
            .authorize_native_filesystem_commit(&attempt, &dispatch.work.identity, &proposal)
            .unwrap();
        drop(lease);
    }
    let next = inputs[0].proposal(0, MAX_NATIVE_PARENT_DIRECTORIES / 2);
    let error = fixture
        .service
        .authorize_native_filesystem_commit(&attempt, &dispatch.work.identity, &next)
        .err()
        .unwrap();
    assert!(
        error.message().contains("parent admission count"),
        "{error}"
    );
    let file =
        NativePatchCommitProposal::capture(&inputs[0].file, &inputs[0].change, 0, 0).unwrap();
    let lease = fixture
        .service
        .authorize_native_filesystem_commit(&attempt, &dispatch.work.identity, &file)
        .unwrap();
    drop(lease);
    assert!(!fixture.root.join("first").exists());
    assert!(!fixture.root.join("second").exists());
}

/// Cancelling the actual admitted source must fence a later directory lease,
/// independently of unchanged path labels and broad fixture write scopes.
#[test]
fn actor_parent_admission_rejects_cancelled_original_source() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Add File: {}\n+data\n*** End Patch",
            root.join("parent/file").display()
        )
    });
    let attempt = fixture.queue();
    let dispatch = fixture
        .service
        .claim_native_filesystem_action(&attempt)
        .unwrap()
        .unwrap();
    let root = dispatch.work.source.capture_root().unwrap();
    let input = Inputs::new(
        &root,
        &dispatch.scopes,
        &dispatch.fence,
        &fixture.root.join("parent/file"),
    );
    let proposal = input.proposal(0, 0);
    attempt.cancel();
    assert!(
        fixture
            .service
            .authorize_native_filesystem_commit(&attempt, &dispatch.work.identity, &proposal)
            .is_err()
    );
    assert!(!fixture.root.join("parent").exists());
}
