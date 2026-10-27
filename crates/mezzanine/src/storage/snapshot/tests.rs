//! Unit tests for snapshot manifest, payload, repository, and restore behavior.

use super::{
    SessionSnapshotPayload, SnapshotAgentSession, SnapshotApprovalGrantMetadata,
    SnapshotApprovalRequestMetadata, SnapshotConfigDiagnostic, SnapshotConfigLayerMetadata,
    SnapshotCreationContext, SnapshotFrameSettings, SnapshotFrameState, SnapshotKind,
    SnapshotLayoutNode, SnapshotManifest, SnapshotMcpExternalCapability, SnapshotMcpServerState,
    SnapshotMcpToolEffects, SnapshotMcpToolState, SnapshotPaneCapture, SnapshotPaneGeometry,
    SnapshotRepository, SnapshotSessionState, SnapshotShellMetadata, SnapshotState,
    SnapshotUnsettledPeerPresentation, WindowSnapshotPayload,
};
use crate::host::shell::{ResolvedShell, ShellSource};
use mez_agent::messaging::{Envelope, MessageScope, MessageService, Recipient, SenderIdentity};
use mez_core::ids::{AgentId, PaneId};
use mez_mux::layout::{LayoutNode, LayoutPolicy, PaneGeometry, Size, SplitDirection};
use mez_mux::session::{Session, SessionState};
use mez_terminal::TerminalSavedDecPrivateMode;
use mez_terminal::{
    GraphicRendition, TerminalColor, TerminalCursorState, TerminalModeState, TerminalSavedState,
    TerminalStyleSpan,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::encoding::{PublicationFailurePhase, write_private_new_atomic_failing};

/// Runs the manifest operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn manifest() -> SnapshotManifest {
    SnapshotManifest {
        state: SnapshotState {
            id: "snap-1".to_string(),
            version: 1,
            session_id: "$1".to_string(),
            name: Some("manual".to_string()),
            created_at: "2026-04-30T00:00:00Z".to_string(),
            kind: SnapshotKind::Manual,
            restorable: true,
            window_count: 1,
            pane_count: 1,
            limitations: Vec::new(),
            storage_ref: "snapshots/snap-1".to_string(),
        },
        contains_terminal_history: true,
        contains_agent_transcripts: true,
        contains_raw_credentials: false,
        active_approvals_restored: false,
        restart_required_panes: Vec::new(),
    }
}

/// Verifies valid manifest can be persisted.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn valid_manifest_can_be_persisted() {
    manifest().validate_for_persistence().unwrap();
}

/// Verifies snapshot manifest rejects raw credentials.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_manifest_rejects_raw_credentials() {
    let mut manifest = manifest();
    manifest.contains_raw_credentials = true;

    let error = manifest.validate_for_persistence().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies snapshot manifest rejects restored approval authority.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_manifest_rejects_restored_approval_authority() {
    let mut manifest = manifest();
    manifest.active_approvals_restored = true;

    let error = manifest.validate_for_persistence().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies snapshot manifest round trips to private file.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_manifest_round_trips_to_private_file() {
    let root = std::env::temp_dir().join(format!("mez-snapshot-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let mut manifest = manifest();
    manifest.state.limitations = vec![
        "pane primary processes must be restarted".to_string(),
        "terminal history is not captured \"yet\"".to_string(),
    ];

    let path = manifest.write_to_dir(&root).unwrap();
    let loaded = SnapshotManifest::read_from_file(&path).unwrap();

    assert_eq!(loaded, manifest);

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot manifests persist the SPEC's stable lowercase kind names.
/// This regression scenario protects the serialized manifest contract so new
/// snapshots remain readable by spec-aligned consumers and future migrations.
#[test]
fn snapshot_manifest_persists_spec_kind_names() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-kind-serialize-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);

    let path = manifest().write_to_dir(&root).unwrap();
    let encoded = fs::read_to_string(&path).unwrap();

    assert!(encoded.contains("\nkind=manual\n"), "{encoded}");

    let _ = fs::remove_dir_all(root);
}

/// Verifies version-one manifests written before the `kind` field existed
/// still decode as manual snapshots. This protects snapshot repository and
/// async runtime paths that must continue reading older persisted manifests.
#[test]
fn snapshot_manifest_reads_version_one_manifests_without_kind_as_manual() {
    let root =
        std::env::temp_dir().join(format!("mez-snapshot-kind-legacy-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);

    let manifest = manifest();
    let path = manifest.write_to_dir(&root).unwrap();
    let legacy = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("kind="))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&path, legacy).unwrap();

    let loaded = SnapshotManifest::read_from_file(&path).unwrap();

    assert_eq!(loaded.state.kind, SnapshotKind::Manual);
    assert_eq!(loaded.state.id, manifest.state.id);

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot repository lists and inspects manifests.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_repository_lists_and_inspects_manifests() {
    let root = std::env::temp_dir().join(format!("mez-snapshot-repo-list-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());

    repo.write(&manifest()).unwrap();

    let listed = repo.list().unwrap();
    let inspected = repo.inspect("snap-1").unwrap();

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "snap-1");
    assert_eq!(inspected.state.name.as_deref(), Some("manual"));

    let _ = fs::remove_dir_all(root);
}

/// Verifies that the Tokio snapshot repository path can persist, inspect, list,
/// and delete the same manifest and payload data as the synchronous repository.
/// This gives async side-effect workers a durable API without depending on
/// blocking filesystem calls.
#[tokio::test]
async fn snapshot_repository_async_persists_lists_and_deletes_snapshots() {
    let root = std::env::temp_dir().join(format!("mez-snapshot-repo-async-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let frame_state = SnapshotFrameState::default();

    let state = repo
        .create_from_session_with_context_async(
            "snap-async",
            Some("manual".to_string()),
            &session,
            SnapshotCreationContext::new(&[], &[], &frame_state, &[]),
        )
        .await
        .unwrap();
    let listed = repo.list_async().await.unwrap();
    let manifest = repo.inspect_async("snap-async").await.unwrap();
    let payload = repo.inspect_payload_async("snap-async").await.unwrap();
    let deleted = repo.delete_async("snap-async").await.unwrap();

    assert_eq!(state.id, "snap-async");
    assert_eq!(listed.len(), 1);
    assert_eq!(manifest.state.name.as_deref(), Some("manual"));
    assert_eq!(payload.session_id, session.id.to_string());
    assert!(deleted);
    assert!(repo.list_async().await.unwrap().is_empty());

    let _ = fs::remove_dir_all(root);
}

/// Startup reconciliation removes only repository-owned abandoned publication
/// temporaries before subsequent durable snapshot writes become visible.
#[test]
fn snapshot_repository_reconciles_abandoned_publication_temporaries() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-publication-reconcile-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let abandoned_payload = root.join(".snapshot-publish.abandoned.payload.tmp");
    let abandoned_manifest = root.join(".snapshot-publish.abandoned.manifest.tmp");
    let incomplete_payload = root.join("incomplete.payload");
    let unrelated = root.join(".unrelated.tmp");
    fs::write(&abandoned_payload, b"partial payload\n").unwrap();
    fs::write(&abandoned_manifest, b"partial manifest\n").unwrap();
    fs::write(&incomplete_payload, b"uncommitted payload\n").unwrap();
    fs::write(&unrelated, b"preserve\n").unwrap();
    let repo = SnapshotRepository::new(root.clone());

    assert_eq!(repo.reconcile_publication_temporaries().unwrap(), 3);
    assert!(!abandoned_payload.exists());
    assert!(!abandoned_manifest.exists());
    assert!(!incomplete_payload.exists());
    assert_eq!(fs::read(&unrelated).unwrap(), b"preserve\n");

    let _ = fs::remove_dir_all(root);
}

/// Every injected crash boundary exposes either no destination or the complete
/// newly published bytes, and repository-owned temporary files are recoverable.
#[test]
fn snapshot_publication_is_complete_or_absent_at_every_failure_phase() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-publication-phases-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let bytes = b"complete snapshot artifact\n";
    for (index, phase) in [
        PublicationFailurePhase::AfterWrite,
        PublicationFailurePhase::AfterFileSync,
        PublicationFailurePhase::BeforePublish,
        PublicationFailurePhase::BeforeDirectorySync,
    ]
    .into_iter()
    .enumerate()
    {
        let path = root.join(format!("phase-{index}.payload"));
        write_private_new_atomic_failing(&path, bytes, phase).unwrap_err();
        if phase == PublicationFailurePhase::BeforeDirectorySync {
            assert_eq!(fs::read(&path).unwrap(), bytes);
        } else {
            assert!(!path.exists(), "{phase:?} exposed a partial destination");
        }
    }
    let repository = SnapshotRepository::new(root.clone());
    repository.reconcile_publication_temporaries().unwrap();
    assert!(fs::read_dir(&root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".snapshot-publish.")
    }));

    let _ = fs::remove_dir_all(root);
}

/// A retry after an older manifest-first deletion failure removes a residual
/// directory payload even though the snapshot manifest is already absent.
#[tokio::test]
async fn snapshot_delete_retry_removes_manifestless_directory_payload() {
    let root =
        std::env::temp_dir().join(format!("mez-snapshot-delete-retry-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("orphan.payload/nested")).unwrap();
    fs::write(root.join("orphan.payload/nested/data"), b"orphan\n").unwrap();
    let repo = SnapshotRepository::new(root.clone());

    assert!(repo.delete_async("orphan").await.unwrap());
    assert!(!root.join("orphan.payload").exists());
    assert!(!repo.delete_async("orphan").await.unwrap());

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot repository selects latest snapshot by session.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_repository_selects_latest_snapshot_by_session() {
    let root =
        std::env::temp_dir().join(format!("mez-snapshot-repo-latest-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut old = manifest();
    old.state.id = "snap-old".to_string();
    old.state.session_id = "$target".to_string();
    old.state.created_at = "2026-04-30T00:00:00Z".to_string();
    old.state.storage_ref = "snap-old.payload".to_string();
    let mut new = manifest();
    new.state.id = "snap-new".to_string();
    new.state.session_id = "$target".to_string();
    new.state.created_at = "2026-04-30T00:00:01Z".to_string();
    new.state.storage_ref = "snap-new.payload".to_string();
    let mut other = manifest();
    other.state.id = "snap-other".to_string();
    other.state.session_id = "$other".to_string();
    other.state.created_at = "2026-04-30T00:00:02Z".to_string();
    other.state.storage_ref = "snap-other.payload".to_string();

    repo.write(&old).unwrap();
    repo.write(&new).unwrap();
    repo.write(&other).unwrap();

    let latest_index = super::sqlite::read(&root)
        .unwrap()
        .expect("the write path stores an index");

    assert_eq!(
        repo.latest(Some("$target")).unwrap().unwrap().id,
        "snap-new"
    );
    assert_eq!(repo.latest(None).unwrap().unwrap().id, "snap-other");
    assert!(repo.latest(Some("$missing")).unwrap().is_none());
    assert_eq!(latest_index.latest_all.as_deref(), Some("snap-other"));
    assert_eq!(
        latest_index
            .latest_by_session
            .get("$target")
            .map(String::as_str),
        Some("snap-new")
    );

    repo.delete("snap-other").unwrap();
    assert_eq!(repo.latest(None).unwrap().unwrap().id, "snap-new");

    let _ = fs::remove_dir_all(root);
}

/// Verifies equal creation stamps fall back to the snapshot id so durable
/// selection stays deterministic for snapshots published inside one second.
///
/// The stored winners and a repository rebuilt from manifests alone must agree;
/// otherwise a recovered index could answer with a different snapshot than the
/// manifest scan it replaces.
#[test]
fn snapshot_repository_tie_breaks_equal_creation_stamps_by_id() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-equal-stamps-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut one_pane = manifest();
    one_pane.state.id = "snap-a".to_string();
    one_pane.state.created_at = "2026-04-30T00:00:00Z".to_string();
    one_pane.state.pane_count = 1;
    one_pane.state.storage_ref = "snap-a.payload".to_string();
    let mut two_panes = manifest();
    two_panes.state.id = "snap-z".to_string();
    two_panes.state.created_at = "2026-04-30T00:00:00Z".to_string();
    two_panes.state.pane_count = 2;
    two_panes.state.storage_ref = "snap-z.payload".to_string();

    repo.write(&one_pane).unwrap();
    repo.write(&two_panes).unwrap();

    assert_eq!(repo.latest(None).unwrap().unwrap().id, "snap-z");
    assert_eq!(repo.latest(Some("$1")).unwrap().unwrap().id, "snap-z");
    let winners = super::sqlite::read(&root)
        .unwrap()
        .expect("the write path stores the winners");
    assert_eq!(winners.latest_all.as_deref(), Some("snap-z"));
    assert_eq!(
        winners.latest_by_session.get("$1").map(String::as_str),
        Some("snap-z")
    );

    let rebuilt_root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-equal-stamps-rebuilt-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&rebuilt_root);
    one_pane.write_to_dir(&rebuilt_root).unwrap();
    two_panes.write_to_dir(&rebuilt_root).unwrap();
    let rebuilt = SnapshotRepository::new(rebuilt_root.clone());
    assert_eq!(
        rebuilt.latest(Some("$1")).unwrap().unwrap().id,
        "snap-z",
        "a rebuilt index makes the same tie-break choice as the manifest scan"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(rebuilt_root);
}

/// Verifies a newer creation stamp outranks the snapshot id.
///
/// Ids are only a tie-breaker: ordering by id first would make the selected
/// snapshot depend on its name rather than on when it was created.
#[test]
fn snapshot_repository_prefers_the_newer_creation_stamp_over_id_order() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-stamp-primary-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut older = manifest();
    older.state.id = "snap-z".to_string();
    older.state.created_at = "2026-04-30T00:00:00Z".to_string();
    older.state.pane_count = 1;
    older.state.storage_ref = "snap-z.payload".to_string();
    let mut newer = manifest();
    newer.state.id = "snap-a".to_string();
    newer.state.created_at = "2026-04-30T00:00:01Z".to_string();
    newer.state.pane_count = 2;
    newer.state.window_count = 1;
    newer.state.storage_ref = "snap-a.payload".to_string();

    repo.write(&older).unwrap();
    repo.write(&newer).unwrap();

    assert_eq!(repo.latest(None).unwrap().unwrap().id, "snap-a");
    assert_eq!(repo.latest(Some("$1")).unwrap().unwrap().id, "snap-a");
    let _ = fs::remove_dir_all(root);
}

/// Verifies the exact session filter ignores a newer snapshot from another
/// session while the unfiltered lookup still reports the global winner.
#[test]
fn snapshot_repository_latest_filter_stays_exact_for_unrelated_sessions() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-session-filter-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut matching = manifest();
    matching.state.id = "snap-session".to_string();
    matching.state.created_at = "2026-04-30T00:00:00Z".to_string();
    matching.state.storage_ref = "snap-session.payload".to_string();
    let mut unrelated = manifest();
    unrelated.state.id = "snap-unrelated".to_string();
    unrelated.state.session_id = "$other".to_string();
    unrelated.state.created_at = "2026-04-30T00:00:05Z".to_string();
    unrelated.state.storage_ref = "snap-unrelated.payload".to_string();

    repo.write(&matching).unwrap();
    repo.write(&unrelated).unwrap();

    assert_eq!(
        repo.latest(Some("$1")).unwrap().unwrap().id,
        "snap-session",
        "a newer unrelated session must not win the exact-session lookup"
    );
    assert_eq!(
        repo.latest(Some("$other")).unwrap().unwrap().id,
        "snap-unrelated"
    );
    assert_eq!(repo.latest(None).unwrap().unwrap().id, "snap-unrelated");
    assert!(repo.latest(Some("$missing")).unwrap().is_none());
    let winners = super::sqlite::read(&root)
        .unwrap()
        .expect("the write path stores the winners");
    assert_eq!(winners.latest_all.as_deref(), Some("snap-unrelated"));
    assert_eq!(
        winners.latest_by_session.get("$1").map(String::as_str),
        Some("snap-session")
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies the persisted creation stamp decodes only the shape this module
/// writes, so ordering never guesses from a foreign or truncated value.
#[test]
fn snapshot_creation_stamp_decodes_one_fixed_shape() {
    use super::encoding::{
        MAX_CREATION_SECONDS, unix_seconds_from_rfc3339, unix_seconds_to_rfc3339,
    };

    assert_eq!(unix_seconds_from_rfc3339("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(
        unix_seconds_from_rfc3339("2026-04-30T00:00:00Z"),
        Some(1_777_507_200)
    );
    for seconds in [0, 1, 1_777_507_200, 8_000_000_000] {
        assert_eq!(
            unix_seconds_from_rfc3339(&unix_seconds_to_rfc3339(seconds)),
            Some(seconds),
            "stamp {seconds} must round-trip"
        );
    }
    assert_eq!(
        unix_seconds_from_rfc3339("9999-12-31T23:59:59Z"),
        Some(MAX_CREATION_SECONDS),
        "the written shape ends at a four-digit year"
    );
    for valid in ["2024-02-29T00:00:00Z", "2000-02-29T12:34:56Z"] {
        let seconds = unix_seconds_from_rfc3339(valid).expect("valid calendar date");
        assert_eq!(unix_seconds_to_rfc3339(seconds), valid);
    }
    for invalid in [
        "",
        "2026-04-30T00:00:00",
        "2026-04-30T00:00:00+00:00",
        "2026-04-30 00:00:00Z",
        "2026-13-30T00:00:00Z",
        "2026-04-30T24:00:00Z",
        "2026-02-29T00:00:00Z",
        "2026-02-30T00:00:00Z",
        "2026-04-31T00:00:00Z",
        "1900-02-29T00:00:00Z",
        "1969-12-31T23:59:59Z",
    ] {
        assert_eq!(
            unix_seconds_from_rfc3339(invalid),
            None,
            "{invalid} is not a stamp this store writes"
        );
    }
}

/// Verifies a repeated or regressed wall-clock reading still yields a strictly
/// newer creation stamp, which is what keeps the newest snapshot selected.
#[test]
fn snapshot_creation_stamp_advances_past_repeated_and_regressed_clocks() {
    use super::encoding::{MAX_CREATION_SECONDS, advance_creation_seconds};

    assert_eq!(advance_creation_seconds(10, None), 10);
    assert_eq!(advance_creation_seconds(10, Some(9)), 10);
    assert_eq!(advance_creation_seconds(10, Some(10)), 11);
    assert_eq!(advance_creation_seconds(10, Some(99)), 100);
    assert_eq!(
        advance_creation_seconds(u64::MAX, Some(u64::MAX)),
        MAX_CREATION_SECONDS,
        "an extreme stamp stays inside the written shape instead of wrapping or growing a digit"
    );
    assert_eq!(
        advance_creation_seconds(MAX_CREATION_SECONDS, Some(MAX_CREATION_SECONDS)),
        MAX_CREATION_SECONDS,
        "no newer stamp exists at the end of the representable domain"
    );
}

/// Verifies asynchronous creation repairs a stale winner before choosing its
/// creation stamp.
///
/// An interrupted delete leaves the winners table naming a snapshot whose
/// manifest and row are already gone. If creation trusted that entry it would
/// fall back to the raw clock and, with a regressed reading, publish a snapshot
/// older than the surviving winner. Both creation paths must repair the index
/// first so the new save stays the latest one.
#[tokio::test]
async fn snapshot_repository_async_creation_repairs_a_stale_winner_first() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-async-stale-stamp-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let frame_state = SnapshotFrameState::default();
    let survivor = SnapshotRepository::new(root.clone())
        .with_creation_seconds(1_800_000_100)
        .create_from_session_with_context_async(
            "snap-survivor",
            None,
            &session,
            SnapshotCreationContext::new(&[], &[], &frame_state, &[]),
        )
        .await
        .unwrap();
    let doomed = SnapshotRepository::new(root.clone())
        .with_creation_seconds(1_800_000_050)
        .create_from_session_with_context_async(
            "snap-doomed",
            None,
            &session,
            SnapshotCreationContext::new(&[], &[], &frame_state, &[]),
        )
        .await
        .unwrap();
    assert!(doomed.created_at > survivor.created_at);

    fs::remove_file(root.join("snap-doomed.manifest")).unwrap();
    super::sqlite::remove_metadata_row(&root, "snap-doomed").unwrap();
    assert_eq!(
        super::sqlite::read(&root)
            .unwrap()
            .expect("the winners still name the interrupted deletion")
            .latest_all
            .as_deref(),
        Some("snap-doomed")
    );

    let created = SnapshotRepository::new(root.clone())
        .with_creation_seconds(1_800_000_000)
        .create_from_session_with_context_async(
            "snap-new",
            None,
            &session,
            SnapshotCreationContext::new(&[], &[], &frame_state, &[]),
        )
        .await
        .unwrap();

    assert!(
        created.created_at > survivor.created_at,
        "the new save must outrank the surviving winner the stale index hid"
    );
    let latest = SnapshotRepository::new(root.clone())
        .latest(None)
        .unwrap()
        .expect("the store still holds snapshots");
    assert_eq!(latest.id, "snap-new");
    let _ = fs::remove_dir_all(root);
}

/// Verifies creation fails closed when the store cannot answer its current
/// winner.
///
/// Falling back to the raw wall clock here would publish a snapshot that reports
/// success while sorting behind the winner the broken index hid, so the creation
/// must be refused and leave no manifest, payload, or metadata behind.
#[test]
fn snapshot_repository_creation_fails_when_the_current_winner_is_unreadable() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-unreadable-winner-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    SnapshotRepository::new(root.clone())
        .with_creation_seconds(1_800_000_000)
        .create_from_session("snap-first", None, &session)
        .unwrap();
    fs::write(
        super::sqlite::database_path(&root),
        b"not a snapshot database",
    )
    .unwrap();
    let entries_before = fs::read_dir(&root).unwrap().count();

    let error = SnapshotRepository::new(root.clone())
        .with_creation_seconds(1_800_000_000)
        .create_from_session("snap-second", None, &session)
        .unwrap_err();

    assert!(
        !error.to_string().is_empty(),
        "the refusal carries a diagnostic"
    );
    assert!(!root.join("snap-second.manifest").exists());
    assert!(!root.join("snap-second.payload").exists());
    assert_eq!(
        fs::read_dir(&root).unwrap().count(),
        entries_before,
        "a refused creation leaves the store untouched"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies creation makes no newest-save claim against a winner whose stored
/// creation time is outside the fixed written shape.
///
/// Such a stamp cannot be decoded, so the store keeps its lexical ordering and a
/// new save is stamped from the wall clock. This deliberate legacy exception is
/// documented in the specification; it is not an ordering guarantee.
#[test]
fn snapshot_repository_creation_leaves_a_foreign_winner_stamp_uncompared() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-foreign-winner-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repository = SnapshotRepository::new(root.clone());
    let mut foreign = manifest();
    foreign.state.id = "snap-foreign".to_string();
    foreign.state.created_at = "9998-01-01T00:00:00+00:00".to_string();
    foreign.state.storage_ref = "snap-foreign.payload".to_string();
    repository.write(&foreign).unwrap();
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();

    let created = SnapshotRepository::new(root.clone())
        .with_creation_seconds(1_800_000_000)
        .create_from_session("snap-native", None, &session)
        .unwrap();

    assert_eq!(
        created.created_at,
        super::encoding::unix_seconds_to_rfc3339(1_800_000_000),
        "an uncomparable winner stamp is not advanced past"
    );
    assert_eq!(
        SnapshotRepository::new(root.clone())
            .latest(None)
            .unwrap()
            .unwrap()
            .id,
        "snap-foreign",
        "the store keeps its lexical ordering for an unsupported stamp"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies creation saturates at the last representable stamp, where ordering
/// falls back to the snapshot id.
#[test]
fn snapshot_repository_creation_saturates_at_the_stamp_domain_end() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-stamp-domain-end-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repository = SnapshotRepository::new(root.clone());
    let mut earlier = manifest();
    earlier.state.id = "snap-z".to_string();
    earlier.state.created_at = "9999-12-31T23:59:59Z".to_string();
    earlier.state.storage_ref = "snap-z.payload".to_string();
    repository.write(&earlier).unwrap();
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();

    let created = SnapshotRepository::new(root.clone())
        .with_creation_seconds(1_800_000_000)
        .create_from_session("snap-a", None, &session)
        .unwrap();

    assert_eq!(
        created.created_at, "9999-12-31T23:59:59Z",
        "no newer stamp is representable at the domain end"
    );
    assert_eq!(
        SnapshotRepository::new(root.clone())
            .latest(None)
            .unwrap()
            .unwrap()
            .id,
        "snap-z",
        "the id tie-break decides when the stamp cannot advance"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies asynchronous creation also fails closed when the store cannot answer
/// its current winner.
#[tokio::test]
async fn snapshot_repository_async_creation_fails_when_the_current_winner_is_unreadable() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-async-unreadable-winner-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let frame_state = SnapshotFrameState::default();
    SnapshotRepository::new(root.clone())
        .with_creation_seconds(1_800_000_000)
        .create_from_session_with_context_async(
            "snap-async-first",
            None,
            &session,
            SnapshotCreationContext::new(&[], &[], &frame_state, &[]),
        )
        .await
        .unwrap();
    fs::write(
        super::sqlite::database_path(&root),
        b"not a snapshot database",
    )
    .unwrap();

    let error = SnapshotRepository::new(root.clone())
        .with_creation_seconds(1_800_000_000)
        .create_from_session_with_context_async(
            "snap-async-second",
            None,
            &session,
            SnapshotCreationContext::new(&[], &[], &frame_state, &[]),
        )
        .await
        .unwrap_err();

    assert!(!error.to_string().is_empty());
    assert!(!root.join("snap-async-second.manifest").exists());
    assert!(!root.join("snap-async-second.payload").exists());
    let _ = fs::remove_dir_all(root);
}

/// Verifies an ordinary snapshot write consults only indexed winners instead
/// of parsing unrelated manifests after the latest index has been established.
#[test]
fn snapshot_repository_updates_latest_index_without_scanning_unrelated_manifests() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-incremental-index-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut old = manifest();
    old.state.id = "snap-old".to_string();
    old.state.created_at = "2026-04-30T00:00:00Z".to_string();
    old.state.storage_ref = "snap-old.payload".to_string();
    repo.write(&old).unwrap();
    fs::write(root.join("unrelated.manifest"), "malformed").unwrap();
    let mut new = manifest();
    new.state.id = "snap-new".to_string();
    new.state.created_at = "2026-04-30T00:00:01Z".to_string();
    new.state.storage_ref = "snap-new.payload".to_string();

    repo.write(&new).unwrap();

    let latest_index = super::sqlite::read(&root)
        .unwrap()
        .expect("the write path stores an index");
    assert_eq!(latest_index.latest_all.as_deref(), Some("snap-new"));
    assert_eq!(
        latest_index.latest_by_session.get("$1").map(String::as_str),
        Some("snap-new")
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies a legacy `latest.index` file from a build that predates the SQLite
/// store is ignored rather than parsed.
///
/// The database owns the winners now, so a stale file cannot restore an old
/// winner, fail a write, or leave a temporary replacement behind; leaving the
/// file untouched keeps a rollback to the previous build working.
#[test]
fn snapshot_repository_ignores_the_legacy_latest_index_file() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-index-recovery-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut old = manifest();
    old.state.id = "snap-old".to_string();
    old.state.created_at = "2026-04-30T00:00:00Z".to_string();
    old.state.storage_ref = "snap-old.payload".to_string();
    repo.write(&old).unwrap();
    fs::write(root.join("latest.index"), "malformed\n").unwrap();
    let mut new = manifest();
    new.state.id = "snap-new".to_string();
    new.state.created_at = "2026-04-30T00:00:01Z".to_string();
    new.state.storage_ref = "snap-new.payload".to_string();

    repo.write(&new).unwrap();

    assert_eq!(repo.latest(None).unwrap().unwrap().id, "snap-new");
    assert_eq!(
        fs::read_to_string(root.join("latest.index")).unwrap(),
        "malformed\n",
        "the legacy file is left untouched"
    );
    assert!(fs::read_dir(&root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot resume plans are served from manifest metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_resume_plan_uses_manifest_metadata_without_payload() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-resume-plan-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut manifest = manifest();
    manifest.state.window_count = 2;
    manifest.state.pane_count = 3;
    manifest.state.limitations = vec!["pane primary processes must be restarted".to_string()];
    manifest.state.storage_ref = "missing.payload".to_string();
    manifest.restart_required_panes = vec!["%1".to_string(), "%2".to_string()];
    repo.write(&manifest).unwrap();

    let plan = repo.resume_plan("snap-1").unwrap();

    assert_eq!(plan.session_id, "$1");
    assert_eq!(plan.window_count, 2);
    assert_eq!(plan.pane_count, 3);
    assert_eq!(plan.restart_required_panes, vec!["%1", "%2"]);
    assert_eq!(plan.limitations, manifest.state.limitations);

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot repository deletes manifest and local payload.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_repository_deletes_manifest_and_local_payload() {
    let root =
        std::env::temp_dir().join(format!("mez-snapshot-repo-delete-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut manifest = manifest();
    manifest.state.storage_ref = "snap-1.payload".to_string();
    repo.write(&manifest).unwrap();
    fs::write(root.join("snap-1.payload"), "payload").unwrap();

    assert!(repo.delete("snap-1").unwrap());
    assert!(!root.join("snap-1.manifest").exists());
    assert!(!root.join("snap-1.payload").exists());
    assert!(!repo.delete("snap-1").unwrap());

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot repository rejects path traversal ids.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_repository_rejects_path_traversal_ids() {
    let repo = SnapshotRepository::new(PathBuf::from("/tmp/mezzanine-snapshots"));

    let error = repo.inspect("../secret").unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies session snapshot payload round trips and builds resume plan.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn session_snapshot_payload_round_trips_and_builds_resume_plan() {
    let root = std::env::temp_dir().join(format!("mez-snapshot-payload-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/usr/bin/zsh"), ShellSource::ShellEnv),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    assert_eq!(
        session.cycle_layout(&primary).unwrap(),
        LayoutPolicy::EvenVertical
    );
    let config_layers = vec![SnapshotConfigLayerMetadata {
        id: "primary".to_string(),
        layer_type: "primary".to_string(),
        precedence: 0,
        path: Some("/tmp/mezzanine/config.toml".to_string()),
        trusted: true,
        applied: true,
        schema_version: 1,
        diagnostics: vec![SnapshotConfigDiagnostic {
            path: "history.lines".to_string(),
            message: "example diagnostic".to_string(),
        }],
    }];
    let frame_state = SnapshotFrameState {
        window: SnapshotFrameSettings {
            enabled: true,
            position: "bottom".to_string(),
            style: "inverse".to_string(),
            template: "#{window.id}".to_string(),
            visible_fields: vec!["window.id".to_string(), "window.pane_count".to_string()],
        },
        pane: SnapshotFrameSettings {
            enabled: true,
            position: "top".to_string(),
            style: "bold".to_string(),
            template: "#{pane.id} #{agent.status}".to_string(),
            visible_fields: vec!["pane.id".to_string(), "agent.status".to_string()],
        },
    };
    let agent_sessions = vec![SnapshotAgentSession {
        pane_id: "%1".to_string(),
        conversation_id: "as1".to_string(),
        visibility: "hide-pending-task-completion".to_string(),
        running_turn_id: Some("turn-1".to_string()),
        transcript_entries: 2,
    }];
    let approval_grants = vec![SnapshotApprovalGrantMetadata {
        id: "ap1".to_string(),
        command_prefix: vec!["cargo".to_string(), "test".to_string()],
        scope: "session".to_string(),
        decision: "approve".to_string(),
    }];
    let approval_requests = vec![SnapshotApprovalRequestMetadata {
        id: "ba1".to_string(),
        requesting_agent_id: "agent-1".to_string(),
        pane_id: "%1".to_string(),
        parent_agent_chain: vec!["root-agent".to_string()],
        action_kind: "shell_command".to_string(),
        action_summary: "cargo test".to_string(),
        declared_effects: vec!["read".to_string(), "write".to_string()],
        matched_rules: vec!["safe-cargo-test".to_string()],
        read_scopes: vec!["/tmp/mezzanine".to_string()],
        write_scopes: vec!["/tmp/mezzanine/target".to_string()],
        created_at_unix_seconds: Some(100),
        decided_at_unix_seconds: Some(101),
        decided_by_client_id: Some("primary".to_string()),
        state: "approved".to_string(),
        decision: Some("approve".to_string()),
        redirect_instruction: None,
    }];
    let mut message_service = MessageService::default();
    let sender = message_service.register_agent(None, None, "writer", vec!["code".to_string()]);
    let sender_id = sender.agent_id.clone();
    let target = message_service
        .ensure_agent_identity(
            SenderIdentity {
                agent_id: AgentId::opaque("agent-%1".to_string()).unwrap(),
                project_scope: None,
                pane_id: PaneId::opaque("%1".to_string()),
                window_id: None,
                role: Some("agent".to_string()),
                capabilities: Vec::new(),
                objective: None,
            },
            0,
        )
        .unwrap();
    message_service.subscribe(&target.agent_id).unwrap();
    message_service
        .accept_at_with_scope(
            &sender_id,
            Envelope {
                protocol: "mmp/1",
                id: "snapshot-message".to_string(),
                message_type: "send".to_string(),
                time: "2026-05-01T00:00:00Z".to_string(),
                sender,
                recipient: Recipient::Agent(target.agent_id.clone()),
                correlation_id: Some("turn-1".to_string()),
                ttl_ms: Some(10_000),
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "hello reviewer".to_string(),
                extension_fields: vec![("trace".to_string(), r#"{"span":"one"}"#.to_string())],
            },
            MessageScope::Session,
            10,
        )
        .unwrap();
    message_service
        .advance_subscription(&target.agent_id, 1)
        .unwrap();
    // The actual outer payload codec must retain fresh-only schema4 provenance,
    // not just serde/domain roundtrips. Both sent objectives remain exact while
    // a third unsent current objective must not replace their historical data.
    let raw = message_service
        .register_agent_with_objective(None, None, "raw", Vec::new(), Some("A"))
        .unwrap();
    for (index, objective) in ["A", "B"].into_iter().enumerate() {
        message_service
            .update_agent_objective(&raw.agent_id, Some(objective), 11 + index as u64)
            .unwrap();
        let sender = message_service
            .registered_identity(&raw.agent_id)
            .unwrap()
            .clone();
        message_service
            .accept_at_with_scope(
                &raw.agent_id,
                Envelope {
                    protocol: "mmp/1",
                    id: format!("raw-objective-{index}"),
                    message_type: "send".to_string(),
                    time: "message:test".to_string(),
                    sender,
                    recipient: Recipient::Agent(target.agent_id.clone()),
                    correlation_id: None,
                    ttl_ms: None,
                    content_type: "text/plain; charset=utf-8".to_string(),
                    payload: "fixed historical mail".to_string(),
                    extension_fields: Vec::new(),
                },
                MessageScope::Session,
                11 + index as u64,
            )
            .unwrap();
    }
    message_service
        .update_agent_objective(&raw.agent_id, Some("C unsent"), 13)
        .unwrap();
    message_service
        .retire_fresh_observational_identity(&raw.agent_id)
        .unwrap();
    let message_state = message_service.snapshot_state();
    assert_eq!(message_state.schema_version, 5);
    assert_eq!(
        message_state.retired_delivery_floors[0]
            .fresh_sender_versions
            .as_ref()
            .unwrap()
            .len(),
        2
    );
    let unsettled_peer_presentations = vec![SnapshotUnsettledPeerPresentation {
        occurrence: mez_core::presentation::PresentationOccurrence::new(
            Some(1234),
            mez_core::presentation::PresentationOccurrenceBasis::LocalAcceptance,
        ),
        identity: "peer-message recipient=agent-%1 sequence=1 id=snapshot-message".to_string(),
        recipient_agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        conversation_id: "as1".to_string(),
        turn_id: "turn-1".to_string(),
        sequence: 1,
        peer_label: "writer".to_string(),
        direct_parent: false,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: "hello reviewer".to_string(),
        live_rendered: true,
        presentation_eligible: true,
    }];
    let mcp_servers = vec![SnapshotMcpServerState {
        id: "fs".to_string(),
        name: "filesystem".to_string(),
        kind: "stdio".to_string(),
        enabled: true,
        status: "available".to_string(),
        last_checked_at_unix_seconds: Some(100),
        blacklist_reason: None,
        external_capability: SnapshotMcpExternalCapability {
            mutates_filesystem_outside_shell: false,
            executes_processes_outside_shell: false,
            accesses_credentials_outside_shell: false,
            purpose: String::new(),
            usage_instructions: String::new(),
        },
        tools: vec![SnapshotMcpToolState {
            server_id: "fs".to_string(),
            name: "read_file".to_string(),
            available: true,
            blacklisted: false,
            permission_required: true,
            effects: SnapshotMcpToolEffects {
                reads_filesystem: true,
                mutates_filesystem: false,
                executes_processes: false,
                accesses_credentials: false,
                uses_network: false,
                has_side_effects: false,
            },
            approval: "prompt".to_string(),
            description: "read a file".to_string(),
            input_schema_json: r#"{"type":"object"}"#.to_string(),
        }],
    }];
    let payload = SessionSnapshotPayload::from_session_with_context(
        &session,
        SnapshotCreationContext::new(&[], &config_layers, &frame_state, &agent_sessions)
            .with_approvals(&approval_grants, &approval_requests)
            .with_message_state(&message_state)
            .with_unsettled_peer_presentations(&unsettled_peer_presentations)
            .with_mcp_servers(&mcp_servers),
    );

    repo.write_payload("snap-1", &payload).unwrap();
    let encoded = fs::read_to_string(root.join("snap-1.payload")).unwrap();
    let loaded = repo.inspect_payload("snap-1").unwrap();
    let plan = payload.resume_plan();

    assert!(encoded.starts_with("payload_version\t7\n"));
    assert!(encoded.contains("\nwindow_layout\t@1\t"));
    assert!(encoded.contains("\npane_shell\t%1\t\texited\t\tunknown\n"));
    assert_eq!(loaded, payload);
    assert_eq!(
        loaded.unsettled_peer_presentations[0]
            .occurrence
            .unix_millis,
        Some(1234)
    );
    let mut legacy_receipt = serde_json::to_value(&loaded.unsettled_peer_presentations[0]).unwrap();
    legacy_receipt.as_object_mut().unwrap().remove("occurrence");
    let legacy_receipt: SnapshotUnsettledPeerPresentation =
        serde_json::from_value(legacy_receipt).unwrap();
    assert_eq!(
        legacy_receipt.occurrence,
        mez_core::presentation::PresentationOccurrence::default()
    );
    legacy_receipt.validate().unwrap();
    let occurrence_field = "\"occurrence\":{\"unix_millis\":1234,\"basis\":\"LocalAcceptance\"},";
    assert!(
        encoded.contains(occurrence_field),
        "known stamp must be encoded before constructing the legacy fixture"
    );
    let legacy_payload = encoded
        .replacen("payload_version\t7\n", "payload_version\t6\n", 1)
        .replace(occurrence_field, "");
    fs::write(root.join("legacy-occurrence.payload"), legacy_payload).unwrap();
    let legacy_payload = repo.inspect_payload("legacy-occurrence").unwrap();
    assert_eq!(legacy_payload.payload_version, 6);
    assert_eq!(
        legacy_payload.unsettled_peer_presentations[0].occurrence,
        mez_core::presentation::PresentationOccurrence::default()
    );
    assert_eq!(loaded.shell.path, "/usr/bin/zsh");
    assert_eq!(loaded.shell.source, "shell-env");
    assert!(!loaded.shell.used_fallback);
    assert!(loaded.active_config_layers.is_empty());
    assert_eq!(loaded.frame_state, SnapshotFrameState::default());
    assert_eq!(loaded.agent_sessions, agent_sessions);
    assert!(loaded.approval_grants.is_empty());
    assert!(loaded.approval_requests.is_empty());
    assert_eq!(loaded.message_state, Some(message_state));
    assert_eq!(
        loaded.unsettled_peer_presentations,
        unsettled_peer_presentations
    );
    assert!(loaded.mcp_servers.is_empty());
    assert_eq!(loaded.windows[0].layout_policy, "even-vertical");
    assert_eq!(
        loaded.windows[0].layout_root,
        Some(SnapshotLayoutNode::Split {
            direction: "vertical".to_string(),
            children: vec![
                SnapshotLayoutNode::Pane {
                    pane_id: "%1".to_string(),
                },
                SnapshotLayoutNode::Pane {
                    pane_id: "%2".to_string(),
                },
            ],
            sizes: vec![40, 40],
        })
    );
    assert_eq!(
        loaded.windows[0].panes[0].geometry,
        Some(SnapshotPaneGeometry {
            column: 0,
            row: 0,
            columns: 40,
            rows: 24,
        })
    );
    assert_eq!(
        loaded.windows[0].panes[1].geometry,
        Some(SnapshotPaneGeometry {
            column: 40,
            row: 0,
            columns: 40,
            rows: 24,
        })
    );
    assert_eq!(plan.window_count, 1);
    assert_eq!(plan.pane_count, 2);
    assert!(plan.restart_required_panes.is_empty());
    assert_eq!(
        plan.limitations,
        vec![
            "running agent turns are restored as interrupted and require explicit user confirmation before retrying non-idempotent actions"
                .to_string()
        ]
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies manual capture uses the invoking primary while automatic capture
/// follows the layout owner, without persisting either client identity.
#[test]
fn snapshot_v5_selects_manual_and_automatic_landing_sources() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let owner = session.attach_primary("owner", true).unwrap();
    let owner_window = session.new_window(&owner, "owner", true).unwrap();
    let second = session.attach_primary("second", true).unwrap();
    let second_window = session.new_window(&second, "second", true).unwrap();

    let automatic = SessionSnapshotPayload::from_session(&session);
    let manual = SessionSnapshotPayload::from_session_with_context(
        &session,
        SnapshotCreationContext::new(&[], &[], &SnapshotFrameState::default(), &[])
            .with_navigation_source(&second),
    );

    assert_eq!(
        automatic.landing_navigation.active_window_id.as_deref(),
        Some(owner_window.as_str())
    );
    assert_eq!(
        manual.landing_navigation.active_window_id.as_deref(),
        Some(second_window.as_str())
    );
    let encoded = manual.encode().unwrap();
    assert!(!encoded.contains(owner.as_str()));
    assert!(!encoded.contains(second.as_str()));
}

/// Verifies version-4 payloads without an explicit landing record normalize
/// their singular active group, window, and pane into version-5 landing state.
#[test]
fn snapshot_v4_normalizes_legacy_focus_into_landing_navigation() {
    let root = std::env::temp_dir().join(format!("mez-snapshot-v4-landing-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let repo = SnapshotRepository::new(root.clone());
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let payload = SessionSnapshotPayload::from_session(&session);
    repo.write_payload("legacy", &payload).unwrap();
    let path = root.join("legacy.payload");
    let legacy = fs::read_to_string(&path)
        .unwrap()
        .replace("payload_version\t7\n", "payload_version\t4\n")
        .lines()
        .filter(|line| !line.starts_with("landing_navigation\t"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&path, legacy).unwrap();

    let loaded = repo.inspect_payload("legacy").unwrap();

    assert_eq!(
        loaded.landing_navigation.active_group_id.as_deref(),
        Some("g1")
    );
    assert_eq!(
        loaded.landing_navigation.active_window_id.as_deref(),
        Some("@1")
    );
    assert_eq!(
        loaded.landing_navigation.active_pane_id.as_deref(),
        Some("%1")
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies session snapshot payload preserves terminal and transcript refs.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn session_snapshot_payload_preserves_terminal_and_transcript_refs() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-capture-payload-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let primary = session.attach_primary("primary", true).unwrap();
    let pane_id = session
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let captures = vec![SnapshotPaneCapture {
        pane_id: pane_id.clone(),
        primary_pid: Some(4242),
        process_state: Some("running".to_string()),
        current_working_directory: Some("/workspace/project".to_string()),
        readiness_state: Some("ready".to_string()),
        terminal_history: vec!["history".to_string()],
        terminal_history_line_style_spans: vec![vec![TerminalStyleSpan {
            start: 0,
            length: 7,
            rendition: GraphicRendition {
                bold: false,
                dim: false,
                italic: false,
                strikethrough: false,
                double_underline: false,
                hidden: false,
                underline: true,
                inverse: false,
                foreground: None,
                background: Some(TerminalColor::Indexed(7)),
            },
        }]],
        visible_lines: vec!["visible".to_string()],
        visible_line_style_spans: vec![vec![TerminalStyleSpan {
            start: 0,
            length: 7,
            rendition: GraphicRendition {
                bold: true,
                dim: false,
                italic: false,
                strikethrough: false,
                double_underline: false,
                hidden: false,
                underline: false,
                inverse: false,
                foreground: Some(TerminalColor::Indexed(42)),
                background: None,
            },
        }]],
        terminal_modes: TerminalModeState {
            title: Some("snapshot-title".to_string()),
            bracketed_paste_enabled: true,
            insert_mode_enabled: true,
            ..TerminalModeState::default()
        },
        terminal_saved_state: TerminalSavedState {
            saved_cursor: Some(TerminalCursorState { row: 1, column: 2 }),
            saved_dec_private_modes: vec![TerminalSavedDecPrivateMode {
                mode: 2004,
                enabled: true,
            }],
            g0_dec_special_graphics: false,
            g1_dec_special_graphics: false,
            shift_out: false,
        },
        exit_status: Some(mez_mux::process::PaneExitStatus {
            code: Some(7),
            signal: None,
            success: false,
        }),
        alternate_screen_active: false,
        transcript_refs: vec!["conversation-1".to_string()],
    }];
    session.detach_primary(&primary).unwrap();

    let payload = SessionSnapshotPayload::from_session_with_captures(&session, &captures);
    repo.write_payload("snap-capture", &payload).unwrap();
    let loaded = repo.inspect_payload("snap-capture").unwrap();

    assert_eq!(loaded, payload);
    assert_eq!(loaded.windows[0].panes[0].exit_status, None);
    assert_eq!(loaded.windows[0].panes[0].primary_pid, None);
    assert_eq!(loaded.windows[0].panes[0].process_state, "exited");
    assert_eq!(
        loaded.windows[0].panes[0]
            .current_working_directory
            .as_deref(),
        Some("/workspace/project")
    );
    assert_eq!(loaded.windows[0].panes[0].readiness_state, "unknown");
    assert!(!loaded.contains_terminal_history());
    assert!(!loaded.contains_agent_transcripts());
    assert!(loaded.windows[0].panes[0].terminal_history.is_empty());
    assert!(
        loaded.windows[0].panes[0]
            .terminal_history_line_style_spans
            .is_empty()
    );
    assert!(loaded.windows[0].panes[0].visible_lines.is_empty());
    assert!(
        loaded.windows[0].panes[0]
            .visible_line_style_spans
            .is_empty()
    );
    assert_eq!(
        loaded.windows[0].panes[0].terminal_modes,
        TerminalModeState::default()
    );
    assert_eq!(
        loaded.windows[0].panes[0].terminal_saved_state,
        TerminalSavedState::default()
    );
    assert!(loaded.windows[0].panes[0].transcript_refs.is_empty());

    let state = repo
        .create_from_session_with_captures(
            "snap-captured-session",
            Some("captured".to_string()),
            &session,
            &captures,
        )
        .unwrap();
    let manifest = repo.inspect(&state.id).unwrap();
    assert!(!manifest.contains_terminal_history);
    assert!(!manifest.contains_agent_transcripts);

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot repository restores session shape from payload.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_repository_restores_session_shape_from_payload() {
    let root = std::env::temp_dir().join(format!("mez-snapshot-restore-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    let state = repo
        .create_from_session("snap-restore", Some("manual".to_string()), &session)
        .unwrap();

    let restored = repo
        .restore_session(
            &state.id,
            ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        )
        .unwrap();

    assert_eq!(restored.session.id, session.id);
    assert_eq!(restored.session.windows().len(), 1);
    assert_eq!(restored.session.windows()[0].panes().len(), 2);
    assert!(
        restored.session.windows()[0]
            .panes()
            .iter()
            .all(|pane| !pane.live)
    );
    assert!(restored.resume_plan.restart_required_panes.is_empty());

    let _ = fs::remove_dir_all(root);
}

/// Verifies that snapshot resume restores the explicit split tree rather than
/// rebuilding one possible tree from pane rectangles. A 2x2 grid is ambiguous
/// from geometry alone, so preserving the horizontal root proves the
/// `window_layout` payload record is authoritative.
#[test]
fn snapshot_restore_preserves_ambiguous_layout_ancestry() {
    let root =
        std::env::temp_dir().join(format!("mez-snapshot-layout-tree-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane_select(&primary, SplitDirection::Horizontal, true)
        .unwrap();
    session
        .split_active_pane_select(&primary, SplitDirection::Vertical, true)
        .unwrap();
    session.select_pane(&primary, "%1").unwrap();
    session
        .split_active_pane_select(&primary, SplitDirection::Vertical, true)
        .unwrap();
    let state = repo
        .create_from_session("snap-layout-tree", None, &session)
        .unwrap();

    let restored = repo
        .restore_session(
            &state.id,
            ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        )
        .unwrap();
    let window = restored.session.active_window().unwrap();

    match window.layout_root() {
        LayoutNode::Split {
            direction,
            children,
        } => {
            assert_eq!(*direction, SplitDirection::Horizontal);
            assert_eq!(children.len(), 2);
            assert_eq!(children[0].direction(), Some(SplitDirection::Vertical));
            assert_eq!(children[1].direction(), Some(SplitDirection::Vertical));
        }
        LayoutNode::Pane { .. } => panic!("restored layout root should be a split"),
    }

    let _ = fs::remove_dir_all(root);
}

/// Verifies snapshot payload rejects windows without panes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_payload_rejects_windows_without_panes() {
    let payload = SessionSnapshotPayload {
        payload_version: 6,
        session_id: "$1".to_string(),
        name: "default".to_string(),
        state: SnapshotSessionState::Running,
        authoritative_columns: 80,
        authoritative_rows: 24,
        active_window_id: None,
        landing_navigation: super::SnapshotLandingNavigation::default(),
        shell: SnapshotShellMetadata::default(),
        active_config_layers: Vec::new(),
        frame_state: SnapshotFrameState::default(),
        agent_sessions: Vec::new(),
        approval_grants: Vec::new(),
        approval_requests: Vec::new(),
        message_state: None,
        unsettled_peer_presentations: Vec::new(),
        mcp_servers: Vec::new(),
        window_groups: Vec::new(),
        windows: vec![WindowSnapshotPayload {
            window_id: "@1".to_string(),
            index: 0,
            name: "main".to_string(),
            active: true,
            columns: 80,
            rows: 24,
            layout_policy: "tiled".to_string(),
            layout_root: None,
            panes: Vec::new(),
        }],
    };

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that snapshot payload validation treats the window layout policy as
/// a typed restore field rather than unchecked string metadata. Resume uses
/// this value to restore future layout-cycling behavior, so invalid policy names
/// must be rejected before a session is reconstructed.
#[test]
fn snapshot_payload_rejects_invalid_window_layout_policy() {
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.windows[0].layout_policy = "stacked".to_string();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that pane shell process state is validated as typed snapshot
/// metadata rather than accepted as arbitrary text. Restored sessions use this
/// field to report limitations and state, so invalid values must fail before
/// the payload can be persisted or resumed.
#[test]
fn snapshot_payload_rejects_invalid_pane_process_state() {
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.windows[0].panes[0].process_state = "unknown".to_string();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that pane readiness metadata uses the same vocabulary as runtime
/// pane state. Snapshot payloads are durable protocol artifacts, so accepting a
/// misspelled readiness value would hide schema drift until resume time.
#[test]
fn snapshot_payload_rejects_invalid_pane_readiness_state() {
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.windows[0].panes[0].readiness_state = "full-screen".to_string();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies v5 landing navigation rejects a pane from a different window.
#[test]
fn snapshot_v5_rejects_cross_parent_landing_navigation() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let primary = session.attach_primary("primary", true).unwrap();
    let first_window = session.active_window().unwrap().id.clone();
    let second_window = session.new_window(&primary, "second", true).unwrap();
    let second_pane = session.active_pane_for(&primary).unwrap().id.clone();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.landing_navigation.active_window_id = Some(first_window.to_string());
    payload.landing_navigation.active_pane_id = Some(second_pane.to_string());
    assert_ne!(first_window, second_window);

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(
        error
            .message()
            .contains("landing pane is not in the landing window")
    );
}

/// Verifies snapshot payload rejects unsupported format version.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_payload_rejects_unsupported_format_version() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-payload-version-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let repo = SnapshotRepository::new(root.clone());
    fs::write(
        root.join("snap-version.payload"),
        "payload_version\t7\nsession\t$1\tdefault\trunning\t80\t24\t\n",
    )
    .unwrap();

    let error = repo.inspect_payload("snap-version").unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    let _ = fs::remove_dir_all(root);
}

/// Verifies legacy payload versions decode with an empty receiver presentation
/// outbox, while version 5 rejects the version-6-only outbox record even when
/// that record is empty.
#[test]
fn snapshot_v2_through_v5_keep_peer_presentation_outbox_empty() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-peer-outbox-version-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let repo = SnapshotRepository::new(root.clone());
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let payload = SessionSnapshotPayload::from_session(&session);
    repo.write_payload("legacy", &payload).unwrap();
    let path = root.join("legacy.payload");
    let v6 = fs::read_to_string(&path).unwrap();
    for version in 2..=5 {
        fs::write(
            &path,
            v6.replacen(
                "payload_version\t7\n",
                format!("payload_version\t{version}\n").as_str(),
                1,
            ),
        )
        .unwrap();
        assert!(
            repo.inspect_payload("legacy")
                .unwrap()
                .unsettled_peer_presentations
                .is_empty()
        );
    }
    fs::write(
        &path,
        v6.replacen("payload_version\t7\n", "payload_version\t5\n", 1)
            + "unsettled_peer_presentations\t[]\n",
    )
    .unwrap();
    assert!(repo.inspect_payload("legacy").is_err());
    let _ = fs::remove_dir_all(root);
}

/// Verifies a second outbox record is rejected even when the first record is
/// empty, so duplicate detection does not use vector emptiness as its marker.
#[test]
fn snapshot_v6_rejects_duplicate_empty_peer_presentation_outbox_records() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-peer-outbox-duplicate-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let repo = SnapshotRepository::new(root.clone());
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    repo.write_payload("duplicate", &SessionSnapshotPayload::from_session(&session))
        .unwrap();
    let path = root.join("duplicate.payload");
    let encoded = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        encoded + "unsettled_peer_presentations\t[]\nunsettled_peer_presentations\t[]\n",
    )
    .unwrap();
    assert!(repo.inspect_payload("duplicate").is_err());
    let _ = fs::remove_dir_all(root);
}

/// Verifies the v6 outbox accepts its documented 1,024-record boundary and
/// rejects exactly one additional record without constructing a slow runtime.
#[test]
fn snapshot_v6_peer_presentation_outbox_enforces_boundary_without_loss() {
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    let mut messages = MessageService::default();
    let recipient = messages
        .ensure_agent_identity(
            SenderIdentity {
                agent_id: AgentId::opaque("agent-%1".to_string()).unwrap(),
                project_scope: None,
                pane_id: PaneId::opaque("%1".to_string()),
                window_id: None,
                role: Some("agent".to_string()),
                capabilities: Vec::new(),
                objective: None,
            },
            0,
        )
        .unwrap();
    messages.subscribe(&recipient.agent_id).unwrap();
    let mut message_state = messages.snapshot_state();
    message_state.subscriptions[0].last_sequence = 1_024;
    message_state.next_sequence = 1_025;
    payload.message_state = Some(message_state);
    payload.agent_sessions = vec![SnapshotAgentSession {
        pane_id: "%1".to_string(),
        conversation_id: "conversation".to_string(),
        visibility: "visible".to_string(),
        running_turn_id: None,
        transcript_entries: 0,
    }];
    payload.unsettled_peer_presentations = (1..=1_024)
        .map(|sequence| SnapshotUnsettledPeerPresentation {
            occurrence: Default::default(),
            identity: format!(
                "peer-message recipient=agent-%1 sequence={sequence} id=message-{sequence}"
            ),
            recipient_agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            conversation_id: "conversation".to_string(),
            turn_id: "turn".to_string(),
            sequence,
            peer_label: "sender".to_string(),
            direct_parent: false,
            content_type: "text/plain; charset=utf-8".to_string(),
            payload: "message".to_string(),
            live_rendered: false,
            presentation_eligible: true,
        })
        .collect();

    payload.validate().unwrap();
    let overflow = SnapshotUnsettledPeerPresentation {
        occurrence: Default::default(),
        identity: "peer-message recipient=agent-%1 sequence=1025 id=message-1025".to_string(),
        recipient_agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        conversation_id: "conversation".to_string(),
        turn_id: "turn".to_string(),
        sequence: 1_025,
        peer_label: "sender".to_string(),
        direct_parent: false,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: "message".to_string(),
        live_rendered: false,
        presentation_eligible: true,
    };
    payload.unsettled_peer_presentations.push(overflow);

    assert!(payload.validate().is_err());
}

/// Verifies v6 receipt payloads use the same byte-safe bound during runtime
/// capture and snapshot encode/decode, including a multibyte character split
/// at the raw byte limit.
#[test]
fn snapshot_v6_peer_presentation_payload_round_trips_utf8_at_byte_bound() {
    let source = format!("{}tail", "€".repeat(100_000));
    let suffix = format!(
        "...[mez: peer message payload truncated; original_bytes={}]",
        source.len()
    );
    let mut end = (256 * 1024usize).saturating_sub(suffix.len());
    while !source.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let captured = format!("{}{}", &source[..end], suffix);
    assert!(captured.len() <= 256 * 1024, "{}", captured.len());
    assert!(captured.ends_with(']'));

    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    let mut messages = MessageService::default();
    let recipient = messages
        .ensure_agent_identity(
            SenderIdentity {
                agent_id: AgentId::opaque("agent-%1".to_string()).unwrap(),
                project_scope: None,
                pane_id: PaneId::opaque("%1".to_string()),
                window_id: None,
                role: Some("agent".to_string()),
                capabilities: Vec::new(),
                objective: None,
            },
            0,
        )
        .unwrap();
    messages.subscribe(&recipient.agent_id).unwrap();
    let mut message_state = messages.snapshot_state();
    message_state.subscriptions[0].last_sequence = 1;
    message_state.next_sequence = 2;
    payload.message_state = Some(message_state);
    payload.agent_sessions = vec![SnapshotAgentSession {
        pane_id: "%1".to_string(),
        conversation_id: "conversation".to_string(),
        visibility: "visible".to_string(),
        running_turn_id: None,
        transcript_entries: 0,
    }];
    payload.unsettled_peer_presentations = vec![SnapshotUnsettledPeerPresentation {
        occurrence: Default::default(),
        identity: "peer-message recipient=agent-%1 sequence=1 id=utf8".to_string(),
        recipient_agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        conversation_id: "conversation".to_string(),
        turn_id: "turn".to_string(),
        sequence: 1,
        peer_label: "sender".to_string(),
        direct_parent: false,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: captured.clone(),
        live_rendered: false,
        presentation_eligible: true,
    }];
    let root =
        std::env::temp_dir().join(format!("mez-snapshot-utf8-receipt-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let repo = SnapshotRepository::new(root.clone());
    repo.write_payload("utf8", &payload).unwrap();
    let decoded = repo.inspect_payload("utf8").unwrap();
    assert_eq!(decoded.unsettled_peer_presentations[0].payload, captured);
    let _ = fs::remove_dir_all(root);
}

/// Verifies v6 outbox validation rejects missing message state, an
/// unacknowledged recipient cursor, and a pane/conversation ownership mismatch.
#[test]
fn snapshot_v6_peer_presentation_rejects_missing_cursor_or_owner() {
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.unsettled_peer_presentations = vec![SnapshotUnsettledPeerPresentation {
        occurrence: Default::default(),
        identity: "peer-message recipient=agent-%1 sequence=1 id=malformed".to_string(),
        recipient_agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        conversation_id: "conversation".to_string(),
        turn_id: "turn".to_string(),
        sequence: 1,
        peer_label: "sender".to_string(),
        direct_parent: false,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: "message".to_string(),
        live_rendered: false,
        presentation_eligible: true,
    }];
    assert!(payload.validate().is_err());

    let mut messages = MessageService::default();
    let recipient = messages
        .ensure_agent_identity(
            SenderIdentity {
                agent_id: AgentId::opaque("agent-%1".to_string()).unwrap(),
                project_scope: None,
                pane_id: PaneId::opaque("%1".to_string()),
                window_id: None,
                role: Some("agent".to_string()),
                capabilities: Vec::new(),
                objective: None,
            },
            0,
        )
        .unwrap();
    messages.subscribe(&recipient.agent_id).unwrap();
    let mut message_state = messages.snapshot_state();
    message_state.next_sequence = 2;
    payload.message_state = Some(message_state.clone());
    assert!(payload.validate().is_err());

    message_state.subscriptions[0].last_sequence = 1;
    payload.message_state = Some(message_state);
    assert!(payload.validate().is_err());

    payload.agent_sessions = vec![SnapshotAgentSession {
        pane_id: "%1".to_string(),
        conversation_id: "different-conversation".to_string(),
        visibility: "visible".to_string(),
        running_turn_id: None,
        transcript_entries: 0,
    }];
    assert!(payload.validate().is_err());
}

/// Verifies v6 receipt validation rejects a recipient whose registered identity
/// has no pane, names another pane, or claims a window that does not own the
/// receipt pane. These malformed records must fail before snapshot startup can
/// attach a receiver presentation to an unrelated terminal surface.
#[test]
fn snapshot_v6_peer_presentation_rejects_invalid_recipient_pane_ownership() {
    let session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    let mut messages = MessageService::default();
    let recipient = messages
        .ensure_agent_identity(
            SenderIdentity {
                agent_id: AgentId::opaque("agent-%1".to_string()).unwrap(),
                project_scope: None,
                pane_id: PaneId::opaque("%1".to_string()),
                window_id: None,
                role: Some("agent".to_string()),
                capabilities: Vec::new(),
                objective: None,
            },
            0,
        )
        .unwrap();
    messages.subscribe(&recipient.agent_id).unwrap();
    let mut message_state = messages.snapshot_state();
    message_state.subscriptions[0].last_sequence = 1;
    message_state.next_sequence = 2;
    payload.message_state = Some(message_state);
    payload.agent_sessions = vec![SnapshotAgentSession {
        pane_id: "%1".to_string(),
        conversation_id: "conversation".to_string(),
        visibility: "visible".to_string(),
        running_turn_id: None,
        transcript_entries: 0,
    }];
    payload.unsettled_peer_presentations = vec![SnapshotUnsettledPeerPresentation {
        occurrence: Default::default(),
        identity: "peer-message recipient=agent-%1 sequence=1 id=ownership".to_string(),
        recipient_agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        conversation_id: "conversation".to_string(),
        turn_id: "turn".to_string(),
        sequence: 1,
        peer_label: "sender".to_string(),
        direct_parent: false,
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: "message".to_string(),
        live_rendered: false,
        presentation_eligible: true,
    }];
    payload.validate().unwrap();

    payload.message_state.as_mut().unwrap().registered_agents[0].pane_id = None;
    assert!(payload.validate().is_err());

    payload.message_state.as_mut().unwrap().registered_agents[0].pane_id = Some("%2".to_string());
    assert!(payload.validate().is_err());

    let identity = &mut payload.message_state.as_mut().unwrap().registered_agents[0];
    identity.pane_id = Some("%1".to_string());
    identity.window_id = Some("@other".to_string());
    assert!(payload.validate().is_err());
}

/// Verifies snapshot payload rejects invalid shell metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_payload_rejects_invalid_shell_metadata() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    session.attach_primary("primary", true).unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.shell.path.clear();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.shell.path = "/bin/sh".to_string();
    payload.shell.source = "shell-env".to_string();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies snapshot payload rejects invalid agent session metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_payload_rejects_invalid_agent_session_metadata() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    session.attach_primary("primary", true).unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.agent_sessions.push(SnapshotAgentSession {
        pane_id: "%1".to_string(),
        conversation_id: "as1".to_string(),
        visibility: "unknown".to_string(),
        running_turn_id: None,
        transcript_entries: 0,
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.agent_sessions[0].visibility = "hide-pending-task-completion".to_string();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that persisted terminal rendition metadata cannot contain spans
/// that are empty, default-styled, or outside the pane grid.
#[test]
fn snapshot_payload_rejects_invalid_visible_style_spans() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    session.attach_primary("primary", true).unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    let pane = &mut payload.windows[0].panes[0];
    pane.visible_lines = vec!["styled".to_string()];
    pane.visible_line_style_spans = vec![vec![TerminalStyleSpan {
        start: 0,
        length: 0,
        rendition: GraphicRendition {
            bold: true,
            ..GraphicRendition::default()
        },
    }]];

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.windows[0].panes[0].visible_line_style_spans[0][0].length = 1;
    payload.windows[0].panes[0].visible_line_style_spans[0][0].rendition =
        GraphicRendition::default();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.windows[0].panes[0].visible_line_style_spans[0][0] = TerminalStyleSpan {
        start: usize::from(payload.windows[0].panes[0].columns),
        length: 1,
        rendition: GraphicRendition {
            bold: true,
            ..GraphicRendition::default()
        },
    };

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that saved terminal parser state cannot point outside the pane or
/// include unsupported or duplicate DEC private modes.
#[test]
fn snapshot_payload_rejects_invalid_terminal_saved_state() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    session.attach_primary("primary", true).unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    let pane = &mut payload.windows[0].panes[0];
    pane.terminal_saved_state.saved_cursor = Some(TerminalCursorState {
        row: usize::from(pane.rows),
        column: 0,
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    let pane = &mut payload.windows[0].panes[0];
    pane.terminal_saved_state.saved_cursor = Some(TerminalCursorState { row: 0, column: 0 });
    pane.terminal_saved_state
        .saved_dec_private_modes
        .push(TerminalSavedDecPrivateMode {
            mode: 9999,
            enabled: true,
        });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    let pane = &mut payload.windows[0].panes[0];
    pane.terminal_saved_state.saved_dec_private_modes = vec![
        TerminalSavedDecPrivateMode {
            mode: 2004,
            enabled: true,
        },
        TerminalSavedDecPrivateMode {
            mode: 2004,
            enabled: false,
        },
    ];

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that pane geometry metadata remains tied to the pane dimensions
/// and window bounds stored beside it instead of becoming unchecked secondary
/// layout state. Invalid dimensions, out-of-window rectangles, and overlapping
/// rectangles must fail validation before snapshot inspection or restore can
/// treat the payload as usable.
#[test]
fn snapshot_payload_rejects_invalid_pane_geometry() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    session.attach_primary("primary", true).unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.windows[0].panes[0].geometry = Some(SnapshotPaneGeometry {
        column: 0,
        row: 0,
        columns: 0,
        rows: 24,
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.windows[0].panes[0].geometry = Some(SnapshotPaneGeometry {
        column: 0,
        row: 0,
        columns: 40,
        rows: 24,
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    let mut split_session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let primary = split_session.attach_primary("primary", true).unwrap();
    split_session
        .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&split_session);
    payload.windows[0].panes[1].geometry = Some(SnapshotPaneGeometry {
        column: 20,
        row: 0,
        columns: 40,
        rows: 24,
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.windows[0].panes[1].geometry = Some(SnapshotPaneGeometry {
        column: 60,
        row: 0,
        columns: 40,
        rows: 24,
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies snapshot payload rejects invalid approval metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_payload_rejects_invalid_approval_metadata() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    session.attach_primary("primary", true).unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.approval_grants.push(SnapshotApprovalGrantMetadata {
        id: "ap1".to_string(),
        command_prefix: Vec::new(),
        scope: "session".to_string(),
        decision: "approve".to_string(),
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.approval_grants.clear();
    payload
        .approval_requests
        .push(SnapshotApprovalRequestMetadata {
            id: "ba1".to_string(),
            requesting_agent_id: "agent-1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: Vec::new(),
            action_kind: "shell_command".to_string(),
            action_summary: "cargo test".to_string(),
            declared_effects: Vec::new(),
            matched_rules: Vec::new(),
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            created_at_unix_seconds: Some(100),
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: "pending".to_string(),
            decision: None,
            redirect_instruction: None,
        });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.approval_requests[0].state = "redirected".to_string();
    payload.approval_requests[0].decision = Some("redirect".to_string());

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.approval_requests.clear();
    payload.mcp_servers.push(SnapshotMcpServerState {
        id: "fs".to_string(),
        name: "filesystem".to_string(),
        kind: "stdio".to_string(),
        enabled: true,
        status: "available".to_string(),
        last_checked_at_unix_seconds: None,
        blacklist_reason: None,
        external_capability: SnapshotMcpExternalCapability {
            mutates_filesystem_outside_shell: false,
            executes_processes_outside_shell: false,
            accesses_credentials_outside_shell: false,
            purpose: String::new(),
            usage_instructions: String::new(),
        },
        tools: vec![SnapshotMcpToolState {
            server_id: "fs".to_string(),
            name: "read_file".to_string(),
            available: true,
            blacklisted: false,
            permission_required: true,
            effects: SnapshotMcpToolEffects {
                reads_filesystem: true,
                mutates_filesystem: false,
                executes_processes: false,
                accesses_credentials: false,
                uses_network: false,
                has_side_effects: false,
            },
            approval: "prompt".to_string(),
            description: String::new(),
            input_schema_json: "not-json".to_string(),
        }],
    });

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies snapshot payload rejects invalid frame state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn snapshot_payload_rejects_invalid_frame_state() {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    session.attach_primary("primary", true).unwrap();
    let mut payload = SessionSnapshotPayload::from_session(&session);
    payload.frame_state.window.position = "middle".to_string();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);

    payload.frame_state.window.position = "top".to_string();
    payload.frame_state.pane.template.clear();

    let error = payload.validate().unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that product snapshot decoding rebuilds the saved session topology,
/// seeds future identifiers past restored values, and preserves pane geometry.
#[test]
fn session_restores_layout_from_snapshot_payload_and_seeds_ids() {
    let shell = ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh);
    let payload = SessionSnapshotPayload {
        payload_version: 6,
        session_id: "$4".to_string(),
        name: "restored".to_string(),
        state: SnapshotSessionState::Detached,
        authoritative_columns: 100,
        authoritative_rows: 40,
        active_window_id: Some("@8".to_string()),
        landing_navigation: super::SnapshotLandingNavigation {
            active_group_id: Some("g1".to_string()),
            active_window_id: Some("@8".to_string()),
            active_pane_id: Some("%12".to_string()),
        },
        shell: SnapshotShellMetadata::default(),
        active_config_layers: Vec::new(),
        frame_state: SnapshotFrameState::default(),
        agent_sessions: Vec::new(),
        approval_grants: Vec::new(),
        approval_requests: Vec::new(),
        message_state: None,
        unsettled_peer_presentations: Vec::new(),
        mcp_servers: Vec::new(),
        window_groups: vec![super::WindowGroupSnapshotPayload {
            group_id: "g1".to_string(),
            index: 0,
            name: "0".to_string(),
            window_ids: vec!["@8".to_string()],
            active_window_id: Some("@8".to_string()),
            last_active_window_id: None,
            active: true,
        }],
        windows: vec![WindowSnapshotPayload {
            window_id: "@8".to_string(),
            index: 0,
            name: "work".to_string(),
            active: true,
            columns: 100,
            rows: 40,
            layout_policy: LayoutPolicy::EvenHorizontal.name().to_string(),
            layout_root: None,
            panes: vec![super::PaneSnapshotPayload {
                pane_id: "%12".to_string(),
                index: 0,
                title: "shell".to_string(),
                active: true,
                live_at_snapshot: true,
                columns: 100,
                rows: 40,
                primary_pid: Some(4242),
                process_state: "running".to_string(),
                current_working_directory: Some("/workspace/project".to_string()),
                readiness_state: "ready".to_string(),
                exit_status: None,
                geometry: Some(SnapshotPaneGeometry {
                    column: 0,
                    row: 0,
                    columns: 100,
                    rows: 40,
                }),
                terminal_modes: mez_terminal::TerminalModeState::default(),
                terminal_saved_state: mez_terminal::TerminalSavedState::default(),
                terminal_history: Vec::new(),
                terminal_history_line_style_spans: Vec::new(),
                visible_lines: Vec::new(),
                visible_line_style_spans: Vec::new(),
                alternate_screen_active: false,
                transcript_refs: Vec::new(),
            }],
        }],
    };

    let restore_input = super::session_restore_input(&payload).unwrap();
    let mut session = Session::from_restore_input(shell, restore_input).unwrap();

    assert_eq!(session.id.as_str(), "$4");
    assert_eq!(session.name, "restored");
    assert_eq!(session.state, SessionState::Detached);
    assert!(session.clients().is_empty());
    assert!(session.observer_attachments().is_empty());
    assert_eq!(session.layout_owner_client_id(), None);
    assert_eq!(session.authoritative_size, Size::new(100, 40).unwrap());
    assert_eq!(session.active_window().unwrap().id.as_str(), "@8");
    assert_eq!(
        session.active_window().unwrap().active_pane().id.as_str(),
        "%12"
    );
    assert!(!session.active_window().unwrap().active_pane().live);
    assert_eq!(
        session.active_window().unwrap().pane_geometries(),
        vec![PaneGeometry {
            index: 0,
            column: 0,
            row: 0,
            columns: 100,
            rows: 40,
        }],
    );
    assert_eq!(
        session.active_window().unwrap().layout_policy(),
        LayoutPolicy::EvenHorizontal
    );

    let primary = session.attach_primary("primary", true).unwrap();
    let window_id = session.new_window(&primary, "next", true).unwrap();
    let pane_id = session
        .active_window()
        .and_then(|window| window.panes().first())
        .map(|pane| pane.id.clone())
        .unwrap();
    assert_eq!(window_id.as_str(), "@9");
    assert_eq!(pane_id.as_str(), "%13");
}

/// Verifies a listing is served from the metadata index without opening a
/// manifest, so listing cost no longer grows with manifest size.
#[test]
fn snapshot_repository_lists_from_the_metadata_index_without_reading_manifests() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-list-index-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut first = manifest();
    first.state.storage_ref = "snap-1.payload".to_string();
    let mut second = manifest();
    second.state.id = "snap-2".to_string();
    second.state.session_id = "$2".to_string();
    second.state.created_at = "2026-04-30T00:00:01Z".to_string();
    second.state.storage_ref = "snap-2.payload".to_string();
    repo.write(&first).unwrap();
    repo.write(&second).unwrap();

    for entry in fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("manifest") {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        }
    }

    let listed = repo.list().unwrap();

    assert_eq!(
        listed
            .iter()
            .map(|state| state.id.as_str())
            .collect::<Vec<_>>(),
        vec!["snap-1", "snap-2"],
        "the index answers the listing in manifest order"
    );
    assert_eq!(listed[1].session_id, "$2");
    assert_eq!(listed[1].name.as_deref(), Some("manual"));
    assert!(
        repo.inspect("snap-1").is_err(),
        "the unreadable manifests prove the listing did not parse them"
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies a manifest published without its index row - the state a crashed
/// writer leaves behind - is repaired by the next listing instead of hidden.
#[test]
fn snapshot_repository_rebuilds_the_metadata_index_when_a_row_is_missing() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-index-repair-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let repo = SnapshotRepository::new(root.clone());
    let mut orphan = manifest();
    orphan.state.storage_ref = "snap-1.payload".to_string();

    orphan.write_to_dir(&root).unwrap();

    let listed = repo.list().unwrap();

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "snap-1");
    assert_eq!(
        super::sqlite::read_metadata(&root).unwrap().unwrap().len(),
        1,
        "the repair stores the row it recovered"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies an index row whose manifest has already disappeared is dropped by
/// the next listing, so the derived index cannot serve a snapshot that no
/// longer exists.
#[test]
fn snapshot_repository_drops_an_index_row_whose_manifest_is_gone() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-index-stale-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut saved = manifest();
    saved.state.storage_ref = "snap-1.payload".to_string();
    repo.write(&saved).unwrap();

    fs::remove_file(root.join("snap-1.manifest")).unwrap();

    assert!(repo.list().unwrap().is_empty());
    assert!(
        super::sqlite::read_metadata(&root).unwrap().is_none(),
        "the repair removes the stale row"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies deleting a snapshot removes its metadata row and recomputes the
/// latest winners from the index rather than from a manifest scan.
#[test]
fn snapshot_repository_delete_removes_the_metadata_row() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-delete-index-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut saved = manifest();
    saved.state.storage_ref = "snap-1.payload".to_string();
    repo.write(&saved).unwrap();
    assert!(super::sqlite::read_metadata(&root).unwrap().is_some());

    assert!(repo.delete("snap-1").unwrap());

    assert!(
        super::sqlite::read_metadata(&root).unwrap().is_none(),
        "the deleted snapshot leaves no indexed row"
    );
    assert!(repo.latest(None).unwrap().is_none());
    let _ = fs::remove_dir_all(root);
}

/// Verifies a database written by the previous version of this store is rebuilt
/// from the manifests: the version-1 winners are discarded, the listing is
/// regenerated, and the current schema recomputes the latest winner.
#[test]
fn snapshot_repository_rebuilds_an_older_database_from_manifests() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-schema-upgrade-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let path = super::sqlite::database_path(&root);
    let (mut connection, _) =
        crate::storage::shared_sqlite::open_shared_database(&path, 1).unwrap();
    let transaction = connection.transaction().unwrap();
    transaction
        .execute_batch(
            "CREATE TABLE latest_snapshot (scope TEXT PRIMARY KEY, snapshot_id TEXT NOT NULL);",
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO latest_snapshot (scope, snapshot_id) VALUES ('', 'snap-stale')",
            [],
        )
        .unwrap();
    crate::storage::shared_sqlite::set_schema_version(&transaction, 1).unwrap();
    transaction.commit().unwrap();
    drop(connection);
    let mut saved = manifest();
    saved.state.storage_ref = "snap-1.payload".to_string();
    saved.write_to_dir(&root).unwrap();
    let repo = SnapshotRepository::new(root.clone());

    let listed = repo.list().unwrap();

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "snap-1");
    assert!(
        super::sqlite::read(&root).unwrap().is_none(),
        "the version-1 winner is discarded rather than trusted"
    );
    assert_eq!(
        repo.latest(None).unwrap().unwrap().id,
        "snap-1",
        "the current schema recomputes the winner from the rebuilt index"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies an ordinary write compares against the indexed metadata row instead
/// of re-parsing the previous winner's manifest.
#[test]
fn snapshot_repository_updates_the_latest_index_from_metadata_rows() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-index-rows-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut old = manifest();
    old.state.id = "snap-old".to_string();
    old.state.created_at = "2026-04-30T00:00:00Z".to_string();
    old.state.storage_ref = "snap-old.payload".to_string();
    repo.write(&old).unwrap();
    fs::set_permissions(
        root.join("snap-old.manifest"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    // A malformed extra manifest makes the recovery fallback observable: an
    // implementation that still parsed the previous winner would fail that parse
    // and then fail this rebuild, while the metadata-row comparison succeeds.
    fs::write(root.join("garbage.manifest"), "malformed").unwrap();
    let mut new = manifest();
    new.state.id = "snap-new".to_string();
    new.state.created_at = "2026-04-30T00:00:01Z".to_string();
    new.state.storage_ref = "snap-new.payload".to_string();

    repo.write(&new).unwrap();

    let latest_index = super::sqlite::read(&root)
        .unwrap()
        .expect("the write path stores an index");
    assert_eq!(latest_index.latest_all.as_deref(), Some("snap-new"));
    assert_eq!(
        latest_index.latest_by_session.get("$1").map(String::as_str),
        Some("snap-new")
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies a latest lookup repairs a winners table that lags a crashed delete
/// instead of failing on the deleted snapshot.
///
/// The delete path removes the manifest, the payload, and the metadata row
/// before it rebuilds the winners. A crash between those steps leaves the
/// winners naming a snapshot whose row and manifest are already gone - a state
/// the listing cannot see, because the surviving ids and files agree - so the
/// lookup itself has to rebuild the index and answer with the survivor.
#[test]
fn snapshot_repository_repairs_a_stale_winner_on_latest_lookup() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-stale-winner-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut old = manifest();
    old.state.id = "snap-old".to_string();
    old.state.created_at = "2026-04-30T00:00:00Z".to_string();
    old.state.storage_ref = "snap-old.payload".to_string();
    let mut new = manifest();
    new.state.id = "snap-new".to_string();
    new.state.created_at = "2026-04-30T00:00:01Z".to_string();
    new.state.storage_ref = "snap-new.payload".to_string();
    repo.write(&old).unwrap();
    repo.write(&new).unwrap();

    fs::remove_file(root.join("snap-new.manifest")).unwrap();
    super::sqlite::remove_metadata_row(&root, "snap-new").unwrap();
    let winners = super::sqlite::read(&root)
        .unwrap()
        .expect("the winners table survives the crash window");
    assert_eq!(
        winners.latest_all.as_deref(),
        Some("snap-new"),
        "the fixture leaves the winners naming the deleted snapshot"
    );

    assert_eq!(repo.latest(None).unwrap().unwrap().id, "snap-old");
    assert_eq!(repo.latest(Some("$1")).unwrap().unwrap().id, "snap-old");
    assert_eq!(
        super::sqlite::read(&root)
            .unwrap()
            .unwrap()
            .latest_all
            .as_deref(),
        Some("snap-old"),
        "the repaired winners table no longer names the deleted snapshot"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies one unpublished manifest and one stale row cannot cancel out and
/// leave the listing stale.
#[test]
fn snapshot_repository_rebuilds_when_ids_disagree_even_though_counts_match() {
    let root = std::env::temp_dir().join(format!(
        "mez-snapshot-repo-index-offset-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let repo = SnapshotRepository::new(root.clone());
    let mut first = manifest();
    first.state.storage_ref = "snap-1.payload".to_string();
    let mut second = manifest();
    second.state.id = "snap-2".to_string();
    second.state.created_at = "2026-04-30T00:00:01Z".to_string();
    second.state.storage_ref = "snap-2.payload".to_string();
    repo.write(&first).unwrap();
    repo.write(&second).unwrap();

    // Two manifests and two rows, but neither pair matches.
    fs::remove_file(root.join("snap-2.manifest")).unwrap();
    let mut third = manifest();
    third.state.id = "snap-3".to_string();
    third.state.created_at = "2026-04-30T00:00:02Z".to_string();
    third.state.storage_ref = "snap-3.payload".to_string();
    third.write_to_dir(&root).unwrap();

    let listed = repo.list().unwrap();

    assert_eq!(
        listed
            .iter()
            .map(|state| state.id.as_str())
            .collect::<Vec<_>>(),
        vec!["snap-1", "snap-3"],
        "the repair drops the stale row and indexes the unpublished manifest"
    );
    assert_eq!(
        super::sqlite::read_metadata(&root)
            .unwrap()
            .unwrap()
            .iter()
            .map(|state| state.id.as_str())
            .collect::<Vec<_>>(),
        vec!["snap-1", "snap-3"]
    );
    let _ = fs::remove_dir_all(root);
}
