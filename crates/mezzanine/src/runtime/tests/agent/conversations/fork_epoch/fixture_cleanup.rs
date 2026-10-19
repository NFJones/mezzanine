//! Saved-fork fixture generation ownership and selector-writer retirement.
//!
//! The real fork path can schedule a selector snapshot with a cloned transcript
//! store. Catalog enumeration can create a lock file, so terminating panes alone
//! does not authorize deleting storage. Use the existing selector shutdown/join
//! barrier independently of native cleanup, then delete only this exclusively
//! created fixture generation. No retry, delay, ignored ENOTEMPTY, global temp
//! helper change, production API or renewed cleanup deadline is introduced.
//!
//! Selector joins prove thread completion, not successful optional computation;
//! the existing synchronous barrier is bounded only by its store-walk contracts,
//! not interruptible against arbitrary filesystem/kernel stalls. The gated
//! regression proves a reachable concurrent creator, not the historical failed
//! run's writer identity or the cause of its ENOTEMPTY receipt.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::RuntimeSessionService;

static NEXT_FORK_ROOT: AtomicU64 = AtomicU64::new(1);

/// Allocates a private directory from at most sixteen owned candidate names.
/// Existing nodes are never removed, modified or adopted as this generation.
fn allocate_root(candidates: impl IntoIterator<Item = PathBuf>) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    for path in candidates.into_iter().take(16) {
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return path.canonicalize(),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "exclusive fork fixture candidates exhausted",
    ))
}

/// Creates only a fresh generation; no PID/counter alone confers root ownership.
pub(super) fn exclusive_root(label: &str) -> std::io::Result<PathBuf> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    allocate_root((0..16).map(|_| {
        let generation = NEXT_FORK_ROOT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "mez-fork-{label}-{}-{nonce}-{generation}",
            std::process::id()
        ))
    }))
}

/// Cleanup failures retain actual typed native/I/O errors and panic ownership.
pub(super) enum CleanupFailure {
    Native(crate::MezError),
    Panic(&'static str, Box<dyn Any + Send>),
    Boundary(&'static str),
    Filesystem(std::io::Error),
}

impl std::fmt::Debug for CleanupFailure {
    /// Formats bounded fixture diagnostics without interpreting opaque payloads.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native(error) => formatter.debug_tuple("Native").field(error).finish(),
            Self::Panic(stage, payload) => formatter
                .debug_tuple("Panic")
                .field(stage)
                .field(&payload.as_ref().type_id())
                .finish(),
            Self::Boundary(reason) => formatter.debug_tuple("Boundary").field(reason).finish(),
            Self::Filesystem(error) => formatter.debug_tuple("Filesystem").field(error).finish(),
        }
    }
}

/// Attempts independent existing native and selector teardown before deletion.
/// Any failure preserves storage. The boundary callback is a test observation
/// seam after joins, not permission to repair/retry a failed deletion.
pub(super) fn settle_and_remove(
    service: &mut RuntimeSessionService,
    root: &Path,
    before_remove: impl FnOnce(&Path) -> Result<(), &'static str>,
) -> Result<(), Vec<CleanupFailure>> {
    let mut failures = Vec::new();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        service.terminate_all_pane_processes()
    })) {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => failures.push(CleanupFailure::Native(error)),
        Err(payload) => failures.push(CleanupFailure::Panic("native fixture cleanup", payload)),
    }
    // A failed native stage must not skip the independent filesystem writer.
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        service.clear_agent_prompt_inputs()
    })) {
        failures.push(CleanupFailure::Panic("selector fixture cleanup", payload));
    }
    if !failures.is_empty() {
        return Err(failures);
    }
    if let Err(reason) = before_remove(root) {
        return Err(vec![CleanupFailure::Boundary(reason)]);
    }
    std::fs::remove_dir_all(root).map_err(|error| vec![CleanupFailure::Filesystem(error)])
}

#[test]
/// A pre-existing generation and its sentinel must remain byte-identical when
/// exclusive allocation collides; only a newly created candidate is returned.
fn fork_fixture_allocation_never_adopts_or_removes_existing_generation() {
    let parent = exclusive_root("allocation-control").unwrap();
    let occupied = parent.join("occupied");
    let fresh = parent.join("fresh");
    std::fs::create_dir(&occupied).unwrap();
    std::fs::write(occupied.join("sentinel"), b"old-generation").unwrap();
    let allocated = allocate_root([occupied.clone(), fresh.clone()]).unwrap();
    assert_eq!(allocated, fresh.canonicalize().unwrap());
    assert_eq!(
        std::fs::read(occupied.join("sentinel")).unwrap(),
        b"old-generation"
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&allocated).unwrap().permissions().mode() & 0o777,
        0o700
    );
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
/// Holds one ACTUALLY CLAIMED selector request before its real private-store
/// enumeration. Cleanup starts only after claim; shutdown then releases that
/// exact gate, joins the writer, and observes its catalog lock/completion before
/// deletion. Removing the clear/join call makes the boundary fail deterministically.
/// Independent gate/channel/pool/helper settlement always precedes assertions, so
/// that causal RED does not leak a blocked worker or erase an unknown generation.
fn fork_fixture_cleanup_joins_claimed_catalog_writer_before_removing_root() {
    use crate::runtime::render::{
        RuntimeAgentSelectorRefreshOutcome, RuntimeAgentSelectorRefreshSnapshot,
    };
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(10);
    let root = exclusive_root("catalog-writer-control").unwrap();
    let store = crate::storage::transcript::AgentTranscriptStore::new(root.clone());
    let mut service = super::test_runtime_service(); // No native pane is started.
    let (release, gate) = mpsc::channel();
    let (claimed, claim) = mpsc::sync_channel(1);
    let (shutdown, shutdown_started) = mpsc::sync_channel(1);
    let (result, completion) = mpsc::sync_channel(1); // Drainable even during join.
    let pool = service.agent_prompt_selector_refresh_pool();
    pool.observe_shutdown_for_tests(shutdown);
    let queued = pool.submit(
        (mez_core::ids::ClientId::new('c', 1), "%owned-fork".into()),
        1,
        RuntimeAgentSelectorRefreshSnapshot {
            candidates: Vec::new(),
            user_config_root: None,
            project_root: None,
            issue_database_path: None,
            transcript_store: Some(store),
            session_title_policy: crate::session_title::SessionTitlePolicy::default(),
            test_gate: Some(gate),
            test_claimed: Some(claimed),
        },
        result,
    );
    // No assertion can unwind an already admitted/gated request.
    let claim_receipt = claim.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    let writer_pending_before_cleanup = claim_receipt.is_ok()
        && !root.join(".catalog-migration.lock").exists()
        && completion.try_recv().is_err();
    let helper_release = release.clone();
    let releaser = std::thread::Builder::new()
        .name("owned-fork-selector-release".into())
        .spawn(move || {
            let observed = shutdown_started
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_ok();
            let _ = helper_release.send(());
            observed
        });
    if releaser.is_err() {
        let _ = release.send(());
    }
    let mut catalog_touch_before_delete = false;
    let mut completion_before_delete = false;
    let cleanup = settle_and_remove(&mut service, &root, |path| {
        catalog_touch_before_delete = path.join(".catalog-migration.lock").is_file();
        completion_before_delete = completion.try_recv().is_ok();
        if catalog_touch_before_delete && completion_before_delete {
            Ok(())
        } else {
            Err("claimed selector writer was not completed before deletion")
        }
    });
    // Always unblock/drain/join before interpreting any failed claim/cleanup.
    let _ = release.send(());
    let fallback = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        service.clear_agent_prompt_inputs()
    }));
    let helper = releaser.map(|handle| handle.join());
    let _ = completion.try_recv();
    let removed_at_boundary = !root.exists();
    let independent_storage_cleanup = if fallback.is_ok() && root.exists() {
        std::fs::remove_dir_all(&root)
    } else {
        Ok(())
    };

    assert_eq!(queued, RuntimeAgentSelectorRefreshOutcome::Queued);
    assert!(claim_receipt.is_ok(), "exact request was never claimed");
    assert!(
        writer_pending_before_cleanup,
        "control must exercise a claimed, unfinished creator"
    );
    assert!(
        matches!(helper, Ok(Ok(true))),
        "shutdown/releaser did not settle"
    );
    assert!(fallback.is_ok(), "independent selector settlement panicked");
    independent_storage_cleanup.unwrap();
    cleanup.unwrap();
    assert!(catalog_touch_before_delete && completion_before_delete);
    assert!(
        removed_at_boundary,
        "cleanup did not remove only after writer joins"
    );
}
