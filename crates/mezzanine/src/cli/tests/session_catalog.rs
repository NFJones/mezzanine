//! Saved-session catalog CLI tests.

use super::*;

/// Verifies catalog status is bounded and rebuild reconstructs retained session metadata.
#[test]
fn session_catalog_cli_reports_status_and_rebuilds_retained_sessions() {
    let (env, _home) = test_env("session-catalog-cli");
    let paths = env.config_paths().unwrap();
    let store = crate::storage::transcript::AgentTranscriptStore::under_config_root(paths.root());
    store.initialize(100).unwrap();
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "catalog-cli-session".to_string(),
            sequence: 1,
            created_at_unix_seconds: 100,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "catalog-cli-turn".to_string(),
            agent_id: "catalog-cli-agent".to_string(),
            pane_id: "%1".to_string(),
            content: "catalog cli prompt".to_string(),
        })
        .unwrap();

    let mut stderr = Vec::new();
    let mut status_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "session-catalog".to_string(),
            "status".to_string(),
        ],
        env.clone(),
        false,
        &mut status_stdout,
        &mut stderr,
    )
    .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&status_stdout).unwrap();
    assert_eq!(status["integrity_ok"], true);
    assert_eq!(status["indexed_conversations"], 1);
    let scans_before_rebuild = status["full_scans"].as_u64().unwrap();
    assert!(scans_before_rebuild >= 1);

    let connection = rusqlite::Connection::open(store.catalog_path()).unwrap();
    connection
        .execute("DELETE FROM saved_conversations", [])
        .unwrap();
    drop(connection);

    let mut rebuild_stdout = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "session-catalog".to_string(),
            "rebuild".to_string(),
        ],
        env,
        false,
        &mut rebuild_stdout,
        &mut stderr,
    )
    .unwrap();
    let rebuilt: serde_json::Value = serde_json::from_slice(&rebuild_stdout).unwrap();
    assert_eq!(rebuilt["integrity_ok"], true);
    assert_eq!(rebuilt["indexed_conversations"], 1);
    assert!(rebuilt["rebuilds"].as_u64().unwrap() >= 1);
    assert!(rebuilt["full_scans"].as_u64().unwrap() > scans_before_rebuild);
    assert!(store.catalog_path().exists());
}

/// Verifies status reports a missing catalog without creating catalog state.
#[test]
fn session_catalog_cli_status_is_read_only_when_catalog_is_missing() {
    let (env, home) = test_env("session-catalog-missing");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "session-catalog".to_string(),
            "status".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let status: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(status["database_exists"], false);
    assert!(status["diagnostic"].as_str().unwrap().contains("rebuild"));
    assert!(!home.join(".config/mez/agent-sessions").exists());
}
