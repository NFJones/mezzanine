//! CLI storage tests.

use super::*;
use crate::storage::memory::PersistentMemoryStore;

/// Verifies `mez storage export memory` prints the legacy TSV rows for a
/// store whose SQLite conversion already landed.
#[test]
fn storage_export_memory_prints_legacy_tsv_rows() {
    let (env, _home) = test_env("storage-export-memory");
    let mut stderr = Vec::new();
    let mut add_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "memory".to_string(),
            "add".to_string(),
            "m1".to_string(),
            "--scope".to_string(),
            "project:/work/repo".to_string(),
            "--content".to_string(),
            "prefer cargo test".to_string(),
        ],
        env.clone(),
        false,
        &mut add_stdout,
        &mut stderr,
    )
    .unwrap();

    let mut export_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "storage".to_string(),
            "export".to_string(),
            "memory".to_string(),
        ],
        env,
        false,
        &mut export_stdout,
        &mut stderr,
    )
    .unwrap();
    let export = String::from_utf8(export_stdout).unwrap();
    assert!(
        export.contains("prefer cargo test") && export.contains("project:/work/repo"),
        "the export renders the legacy TSV row: {export}"
    );
}

/// Verifies an unsupported store name fails with the supported list instead of
/// printing an empty export.
#[test]
fn storage_export_rejects_unknown_store() {
    let (env, _home) = test_env("storage-export-unknown");
    let mut stderr = Vec::new();
    let mut stdout = Vec::new();
    let error = run_with(
        vec![
            "mez".to_string(),
            "storage".to_string(),
            "export".to_string(),
            "bogus".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();
    assert!(
        error
            .message()
            .contains("supported stores: memory, sessions"),
        "unknown stores report the supported list: {}",
        error.message()
    );
}

/// Verifies exporting a store that does not exist yet reports it and never
/// creates the database, because inspection must not mutate the store.
#[test]
fn storage_export_never_creates_the_store() {
    let (env, _home) = test_env("storage-export-fresh");
    let mut stderr = Vec::new();
    let mut stdout = Vec::new();
    let error = run_with(
        vec![
            "mez".to_string(),
            "storage".to_string(),
            "export".to_string(),
            "memory".to_string(),
        ],
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();
    assert!(
        error.message().contains("no persistent memory store found"),
        "a missing store reports an actionable message: {}",
        error.message()
    );
    let paths = env.config_paths().unwrap();
    assert!(
        PersistentMemoryStore::under_config_root(paths.root())
            .export_tsv_read_only()
            .unwrap()
            .is_none(),
        "the export must leave the store absent"
    );
}

/// Verifies the export command requires a subcommand and says so.
#[test]
fn storage_without_subcommand_reports_usage() {
    let (env, _home) = test_env("storage-export-usage");
    let mut stderr = Vec::new();
    let mut stdout = Vec::new();
    let error = run_with(
        vec!["mez".to_string(), "storage".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();
    assert!(
        error.message().contains("storage requires a subcommand"),
        "the missing subcommand is reported: {}",
        error.message()
    );
}

/// Verifies `mez memory export` is read-only: a fresh environment stays
/// without a store after the export runs.
#[test]
fn memory_export_never_creates_the_store() {
    let (env, _home) = test_env("memory-export-read-only");
    let mut stderr = Vec::new();
    let mut stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "memory".to_string(),
            "export".to_string(),
        ],
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    let paths = env.config_paths().unwrap();
    assert!(
        PersistentMemoryStore::under_config_root(paths.root())
            .export_tsv_read_only()
            .unwrap()
            .is_none(),
        "memory export must leave an absent store absent"
    );
}

/// Verifies every advertised storage exporter is accepted by the command, so
/// an advertised name cannot be missing from the exporter match.
#[test]
fn storage_export_accepts_every_advertised_store() {
    for store in crate::cli::storage::STORAGE_EXPORTERS {
        let (env, _home) = test_env("storage-export-advertised");
        let mut stderr = Vec::new();
        let mut stdout = Vec::new();
        let error = run_with(
            vec![
                "mez".to_string(),
                "storage".to_string(),
                "export".to_string(),
                (*store).to_string(),
            ],
            env,
            false,
            &mut stdout,
            &mut stderr,
        )
        .unwrap_err();
        assert!(
            !error.message().contains("unknown storage store"),
            "advertised store `{store}` must be accepted: {}",
            error.message()
        );
    }
}

/// Verifies `mez storage export leases` and `mez storage export assignments`
/// print the rows of the two reservation stores below the configured root.
#[test]
fn storage_export_reservation_stores_print_rows() {
    let (env, _home) = test_env("storage-export-reservations");
    let paths = env.config_paths().unwrap();
    let leases = crate::storage::lease::RemoteSessionLeaseRepository::new(
        crate::storage::lease::default_remote_session_lease_directory(paths.root()),
    );
    leases
        .reserve_pending(crate::storage::lease::LeaseReservationRequest {
            lease_id: "lease-cli".to_string(),
            session_id: "$cli".to_string(),
            owner_principal_id: "device-cli".to_string(),
            owner_live_session_limit: usize::MAX,
            name: None,
            default_for_owner: false,
            expires_at_unix_seconds: None,
            idempotency_key: "create-cli".to_string(),
            creation_fingerprint: "fingerprint-cli".to_string(),
            now_unix_seconds: 10,
        })
        .unwrap();
    let assignments = crate::storage::local_assignment::LocalSessionAssignmentRepository::new(
        crate::storage::local_assignment::default_local_assignment_directory(paths.root()),
    );
    assignments
        .reserve_pending(
            crate::storage::local_assignment::LocalAssignmentReservationRequest {
                session_id: "$cli".to_string(),
                name: "cli".to_string(),
                default_for_host: false,
                now_unix_seconds: 10,
            },
        )
        .unwrap();

    let mut stderr = Vec::new();
    let mut stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "storage".to_string(),
            "export".to_string(),
            "leases".to_string(),
        ],
        env.clone(),
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    let export = String::from_utf8(stdout).unwrap();
    assert!(
        export.contains("lease-cli\t$cli\t"),
        "the lease export renders its row: {export}"
    );

    let mut stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "storage".to_string(),
            "export".to_string(),
            "assignments".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    let export = String::from_utf8(stdout).unwrap();
    assert!(
        export.contains("$cli\tpending\tfalse\t"),
        "the assignment export renders its row: {export}"
    );
}

/// Verifies `mez storage export history` prints both prompt-history scopes in
/// the legacy TSV shape below the configured root.
#[test]
fn storage_export_history_prints_both_scopes() {
    let (env, _home) = test_env("storage-export-history");
    let paths = env.config_paths().unwrap();
    let store = crate::storage::transcript::AgentTranscriptStore::under_config_root(paths.root());
    assert!(
        store
            .append_prompt_history("conv", "exported prompt")
            .unwrap()
    );
    assert!(store.append_command_prompt_history("help").unwrap());

    let mut stderr = Vec::new();
    let mut stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "storage".to_string(),
            "export".to_string(),
            "history".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    let export = String::from_utf8(stdout).unwrap();
    assert!(export.starts_with("# agent\n"), "{export}");
    assert!(export.contains("\n# command\n"), "{export}");
    assert!(export.contains("exported prompt"), "{export}");
    assert!(export.contains("help"), "{export}");
}

/// Verifies `mez storage export project-trust` prints the trust database rows
/// in the legacy TSV shape below the configured root.
#[test]
fn storage_export_project_trust_prints_legacy_rows() {
    let (env, _home) = test_env("storage-export-trust");
    let paths = env.config_paths().unwrap();
    let path = crate::security::project::default_trust_database_path(paths.root());
    let project = paths.root().join("export-project");
    std::fs::create_dir_all(&project).unwrap();
    crate::security::project::ProjectTrustStore::update_file(&path, |store| {
        store.decide_at(
            project.clone(),
            crate::security::project::TrustDecision::Trusted,
            None,
            100,
        )
    })
    .unwrap();

    let mut stderr = Vec::new();
    let mut stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "storage".to_string(),
            "export".to_string(),
            "project-trust".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    let export = String::from_utf8(stdout).unwrap();
    assert!(
        export.starts_with("# Mezzanine project trust database v1"),
        "{export}"
    );
    assert!(export.contains("export-project"), "{export}");
    assert!(export.contains("trusted"), "{export}");
}

/// Builds one snapshot manifest for the snapshots export fixture.
fn storage_snapshot_manifest(
    id: &str,
    session_id: &str,
    created_at: &str,
) -> crate::storage::snapshot::SnapshotManifest {
    crate::storage::snapshot::SnapshotManifest {
        state: crate::storage::snapshot::SnapshotState {
            id: id.to_string(),
            version: 1,
            session_id: session_id.to_string(),
            name: Some("manual".to_string()),
            created_at: created_at.to_string(),
            kind: crate::storage::snapshot::SnapshotKind::Manual,
            restorable: true,
            window_count: 1,
            pane_count: 1,
            limitations: Vec::new(),
            storage_ref: format!("{id}.payload"),
        },
        contains_terminal_history: false,
        contains_agent_transcripts: false,
        contains_raw_credentials: false,
        active_approvals_restored: false,
        restart_required_panes: Vec::new(),
    }
}

/// Verifies `mez storage export snapshots` prints the latest winners in the
/// retired `latest.index` shape, and never creates the store it inspects.
#[test]
fn storage_export_snapshots_prints_legacy_latest_index_rows() {
    let (env, _home) = test_env("storage-export-snapshots");
    let paths = env.config_paths().unwrap();
    let root = paths.root().join("snapshots");
    let repository = crate::storage::snapshot::SnapshotRepository::new(root.clone());
    repository
        .write(&storage_snapshot_manifest(
            "snap-a",
            "$a",
            "2026-04-30T00:00:00Z",
        ))
        .unwrap();
    repository
        .write(&storage_snapshot_manifest(
            "snap-b",
            "$b",
            "2026-04-30T00:00:01Z",
        ))
        .unwrap();

    let mut stderr = Vec::new();
    let mut stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "storage".to_string(),
            "export".to_string(),
            "snapshots".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    let export = String::from_utf8(stdout).unwrap();
    assert_eq!(
        export, "all\tsnap-b\nsession\t$a\tsnap-a\nsession\t$b\tsnap-b\n",
        "the export keeps the retired latest-index line shape"
    );

    let (absent_env, _absent_home) = test_env("storage-export-snapshots-absent");
    let absent_root = absent_env.config_paths().unwrap().root().join("snapshots");
    let mut absent_stderr = Vec::new();
    let mut absent_stdout = Vec::new();
    let error = run_with(
        vec![
            "mez".to_string(),
            "storage".to_string(),
            "export".to_string(),
            "snapshots".to_string(),
        ],
        absent_env,
        false,
        &mut absent_stdout,
        &mut absent_stderr,
    )
    .unwrap_err();
    assert!(
        error.message().contains("no snapshot index found"),
        "an absent store reports a missing index: {}",
        error.message()
    );
    assert!(
        !absent_root.exists(),
        "the inspection command must not create the snapshot store"
    );
}
