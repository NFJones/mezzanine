use std::fs;
use std::os::unix::fs::PermissionsExt;

use super::*;

fn test_root(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "mez-local-assignment-{label}-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let _ = fs::remove_dir_all(&root);
    root
}

#[test]
fn assignment_restart_fences_live_state_and_retains_checkpoint() {
    let root = test_root("restart");
    let repository = LocalSessionAssignmentRepository::new(root.clone());
    let pending = repository
        .reserve_pending(LocalAssignmentReservationRequest {
            session_id: "$1".to_string(),
            name: "one".to_string(),
            default_for_host: true,
            now_unix_seconds: 10,
        })
        .unwrap();
    let active = repository
        .activate(
            &pending.session_id,
            pending.boot_generation,
            pending.assignment_generation,
            11,
        )
        .unwrap();
    let checkpointed = repository
        .update_checkpoint(
            &active.session_id,
            active.boot_generation,
            active.assignment_generation,
            LocalAssignmentCheckpoint {
                snapshot_id: "local-one".to_string(),
                snapshot_version: 1,
                session_id: active.session_id.clone(),
                recorded_at_unix_seconds: 12,
            },
            12,
        )
        .unwrap();

    assert_eq!(repository.advance_boot_generation(20).unwrap(), 1);
    let recovered = repository.get(&checkpointed.session_id).unwrap().unwrap();
    assert_eq!(recovered.state, LocalSessionAssignmentState::Recoverable);
    assert_eq!(recovered.checkpoint, checkpointed.checkpoint);
    assert_eq!(recovered.boot_generation, 1);
    let metadata = fs::metadata(root.join("assignments.json")).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o077, 0);
    let _ = fs::remove_dir_all(root);
}
