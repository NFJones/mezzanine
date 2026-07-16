//! Unit tests for project trust storage and discovery behavior.

use super::{
    PathBuf, ProjectTrustStore, TrustDecision, default_trust_database_path, discover_project_root,
    discover_project_trust_prompt, select_overlay_for_directory, summarize_overlay_capabilities,
};
use std::fs;
use std::path::Path;

/// Returns true when a path or one of its ancestors contains a Git marker.
///
/// # Parameters
/// - `path`: Directory path to inspect.
fn has_git_ancestor(path: &Path) -> bool {
    let mut cursor = path;
    loop {
        if cursor.join(".git").exists() {
            return true;
        }
        let Some(parent) = cursor.parent() else {
            return false;
        };
        if parent == cursor {
            return false;
        }
        cursor = parent;
    }
}

/// Returns a writable temporary base that is not already inside a Git tree.
fn isolated_temp_base() -> PathBuf {
    let mut candidates = vec![
        PathBuf::from("/var/tmp"),
        PathBuf::from("/dev/shm"),
        std::env::temp_dir(),
    ];
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        candidates.insert(0, PathBuf::from(runtime_dir));
    }
    candidates
        .into_iter()
        .find(|path| path.is_dir() && !has_git_ancestor(path))
        .unwrap_or_else(std::env::temp_dir)
}

/// Runs the temp root operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn temp_root(name: &str) -> PathBuf {
    let root = isolated_temp_base().join(format!("mez-project-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

/// Verifies discovers nearest git project root.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn discovers_nearest_git_project_root() {
    let root = temp_root("root");
    fs::create_dir(root.join(".git")).unwrap();
    let nested = root.join("a/b/c");
    fs::create_dir_all(&nested).unwrap();

    assert_eq!(discover_project_root(&nested), root);

    let _ = fs::remove_dir_all(root);
}

/// Verifies no git marker uses current directory as root.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn no_git_marker_uses_current_directory_as_root() {
    let root = temp_root("nogit");
    let nested = root.join("a/b");
    fs::create_dir_all(&nested).unwrap();

    assert_eq!(discover_project_root(&nested), nested);

    let _ = fs::remove_dir_all(root);
}

/// Verifies rejects multiple overlay files in same directory.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_multiple_overlay_files_in_same_directory() {
    let root = temp_root("overlays");
    let files = vec![
        root.join(".mezzanine/config.toml"),
        root.join(".mezzanine/config.json"),
    ];

    let error = select_overlay_for_directory(&files).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Config);

    let _ = fs::remove_dir_all(root);
}

/// Verifies overlay capability summary identifies authority expansion.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn overlay_capability_summary_identifies_authority_expansion() {
    let root = temp_root("overlay-summary");
    let overlay_dir = root.join(".mezzanine");
    fs::create_dir_all(&overlay_dir).unwrap();
    let overlay = overlay_dir.join("config.toml");
    fs::write(
        &overlay,
        "[hooks]\n[mcp_servers.fs]\ncommand = \"mcp-fs\"\n[permissions]\ncommand_rules = []\n",
    )
    .unwrap();

    let capabilities = summarize_overlay_capabilities(&[overlay]).unwrap();

    assert_eq!(
        capabilities,
        vec![
            "command_rules".to_string(),
            "hooks".to_string(),
            "mcp_servers".to_string(),
            "permissions".to_string()
        ]
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies trust prompt lists discovered overlays and blocks pending root.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn trust_prompt_lists_discovered_overlays_and_blocks_pending_root() {
    let root = temp_root("trust-prompt");
    fs::create_dir(root.join(".git")).unwrap();
    let overlay_dir = root.join(".mezzanine");
    fs::create_dir_all(&overlay_dir).unwrap();
    fs::write(
        overlay_dir.join("config.toml"),
        "[hooks]\n[permissions]\ncommand_rules = []\n",
    )
    .unwrap();
    let nested = root.join("src/app");
    fs::create_dir_all(nested.join(".mezzanine")).unwrap();
    fs::write(
        nested.join(".mezzanine/config.json"),
        r#"{"mcp_servers":{"fs":{"command":"mcp-fs"}}}"#,
    )
    .unwrap();
    let store = ProjectTrustStore::default();

    let prompt = discover_project_trust_prompt(&store, &nested)
        .unwrap()
        .unwrap();

    assert_eq!(prompt.project_root, root);
    assert_eq!(prompt.state, TrustDecision::Pending);
    assert!(prompt.blocks_until_primary_decision);
    assert_eq!(prompt.overlay_files.len(), 2);
    assert_eq!(
        prompt.capability_expansion_summary,
        vec![
            "command_rules".to_string(),
            "hooks".to_string(),
            "mcp_servers".to_string(),
            "permissions".to_string(),
        ]
    );

    let _ = fs::remove_dir_all(prompt.project_root);
}

/// Verifies trusted project prompt is informational not blocking.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn trusted_project_prompt_is_informational_not_blocking() {
    let root = temp_root("trust-prompt-trusted");
    fs::create_dir(root.join(".git")).unwrap();
    let overlay_dir = root.join(".mezzanine");
    fs::create_dir_all(&overlay_dir).unwrap();
    fs::write(overlay_dir.join("config.toml"), "[providers]\n").unwrap();
    let mut store = ProjectTrustStore::default();
    store
        .decide_at(
            root.clone(),
            TrustDecision::Trusted,
            Some(root.join(".git")),
            42,
        )
        .unwrap();

    let prompt = discover_project_trust_prompt(&store, &root)
        .unwrap()
        .unwrap();

    assert_eq!(prompt.state, TrustDecision::Trusted);
    assert!(!prompt.blocks_until_primary_decision);
    assert_eq!(
        prompt.capability_expansion_summary,
        vec!["providers".to_string()]
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies project trust is bound to the discovered Git marker.
///
/// A trusted record for the same canonical project root must not automatically
/// trust a different repository identity at that path. Requiring the stored Git
/// marker to match the currently discovered marker makes repository replacement
/// prompt again before project overlays can expand capabilities.
#[test]
fn trusted_project_prompt_reprompts_when_git_marker_changes() {
    let root = temp_root("trust-prompt-git-marker-mismatch");
    fs::create_dir(root.join(".git")).unwrap();
    let overlay_dir = root.join(".mezzanine");
    fs::create_dir_all(&overlay_dir).unwrap();
    fs::write(
        overlay_dir.join("config.toml"),
        "version = 8\n[providers]\n",
    )
    .unwrap();
    let mut store = ProjectTrustStore::default();
    store
        .decide_at(
            root.clone(),
            TrustDecision::Trusted,
            Some(root.join("old-git-marker")),
            42,
        )
        .unwrap();

    let prompt = discover_project_trust_prompt(&store, &root)
        .unwrap()
        .unwrap();

    assert_eq!(prompt.state, TrustDecision::Pending);
    assert!(prompt.blocks_until_primary_decision);

    let _ = fs::remove_dir_all(root);
}

/// Verifies trust store records root decision.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn trust_store_records_root_decision() {
    let root = PathBuf::from("/repo");
    let mut store = ProjectTrustStore::default();

    store
        .decide_at(
            root.clone(),
            TrustDecision::Trusted,
            Some(root.join(".git")),
            42,
        )
        .unwrap();

    assert_eq!(store.get(&root).unwrap().state, TrustDecision::Trusted);
    assert_eq!(store.get(&root).unwrap().trusted_at_unix_seconds, 42);
    assert_eq!(store.get(&root).unwrap().decided_by_client_id, None);
    assert_eq!(
        store.get(&root).unwrap().configuration_schema_version,
        crate::config::CURRENT_CONFIG_SCHEMA_VERSION as u32
    );
}

/// Verifies trust store records deciding client identity.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn trust_store_records_deciding_client_identity() {
    let root = PathBuf::from("/repo");
    let mut store = ProjectTrustStore::default();

    store
        .decide_at_with_client(
            root.clone(),
            TrustDecision::Trusted,
            Some(root.join(".git")),
            42,
            Some("c1".to_string()),
        )
        .unwrap();

    assert_eq!(
        store.get(&root).unwrap().decided_by_client_id.as_deref(),
        Some("c1")
    );
}

/// Verifies trust store round trips private database file.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn trust_store_round_trips_private_database_file() {
    let root = temp_root("trust-db");
    let project = root.join("repo");
    fs::create_dir(&project).unwrap();
    fs::create_dir(project.join(".git")).unwrap();
    let path = default_trust_database_path(&root.join("config"));
    let mut store = ProjectTrustStore::default();
    store
        .decide_at_with_client(
            project.clone(),
            TrustDecision::Trusted,
            Some(project.join(".git")),
            100,
            Some("c1".to_string()),
        )
        .unwrap();

    store.save_to_file(&path).unwrap();
    let loaded = ProjectTrustStore::load_from_file(&path).unwrap();

    let record = loaded.get(&project).unwrap();
    assert_eq!(record.state, TrustDecision::Trusted);
    assert_eq!(record.trusted_at_unix_seconds, 100);
    assert_eq!(record.decided_by_client_id.as_deref(), Some("c1"));
    assert_eq!(
        record.git_marker_path.as_ref().unwrap(),
        &project.join(".git").canonicalize().unwrap()
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies trust database escapes special path characters.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn trust_database_escapes_special_path_characters() {
    let root = temp_root("trust-escape");
    let project = root.join("repo\tname");
    fs::create_dir(&project).unwrap();
    let path = default_trust_database_path(&root.join("config"));
    let mut store = ProjectTrustStore::default();
    store
        .decide_at(project.clone(), TrustDecision::Rejected, None, 101)
        .unwrap();

    store.save_to_file(&path).unwrap();
    let loaded = ProjectTrustStore::load_from_file(&path).unwrap();

    assert_eq!(loaded.get(&project).unwrap().state, TrustDecision::Rejected);

    let _ = fs::remove_dir_all(root);
}
