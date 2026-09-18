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
