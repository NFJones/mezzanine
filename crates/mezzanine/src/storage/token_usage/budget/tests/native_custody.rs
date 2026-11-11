//! Native custody is separate from persisted membership and expense. These
//! private stores exercise the real kernel lock and publication boundaries.

use super::*;
use crate::storage::transcript::AgentTranscriptStore;

/// Missing, future and foreign mirrors and unsafe lock nodes cannot fabricate
/// native retirement. Every refusal preserves both durable owner and expense.
#[test]
fn budget_native_custody_invalid_evidence_refuses_without_mutation() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for case in 0..7 {
        let f = Fixture::new();
        let transcripts = AgentTranscriptStore::new(f.root.join("transcripts"));
        let member = f.member("root", None);
        let lease = f
            .store
            .publish_native_budget_membership(&transcripts, &member, "original", None)
            .unwrap();
        drop(lease);
        let directory = f.root.join("transcripts/root");
        let mirror = directory.join("budget-binding.json");
        let lock = directory.join("budget-binding.lock");
        let before = f.store.budget_account(&member.root).unwrap();
        match case {
            0 => std::fs::remove_file(&mirror).unwrap(),
            1 | 2 => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&mirror).unwrap()).unwrap();
                if case == 1 {
                    value["version"] = 99.into();
                } else {
                    value["membership"]["namespace"] = "foreign".into();
                }
                std::fs::write(&mirror, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            3 => std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap(),
            4 => std::fs::hard_link(&lock, directory.join("another-link")).unwrap(),
            5 => {
                std::fs::rename(&lock, directory.join("pinned-lock")).unwrap();
                symlink("pinned-lock", &lock).unwrap();
            }
            6 => {
                let stage = directory.join(".budget-binding-v2.json.tmp");
                std::fs::write(&stage, b"partial").unwrap();
                std::fs::set_permissions(stage, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            f.store
                .publish_native_budget_membership(&transcripts, &member, "successor", None)
                .is_err(),
            "case {case}"
        );
        assert_eq!(
            f.store.budget_binding_owner(&member).unwrap().as_deref(),
            Some("original")
        );
        assert_eq!(f.store.budget_account(&member.root).unwrap(), before);
    }
}

/// Holding an old descriptor is not authority over a replacement pathname.
/// Refusal must precede the SQLite owner change and preserve the old evidence.
#[test]
fn budget_native_custody_replaced_inode_refuses_database_transfer() {
    let f = Fixture::new();
    let transcripts = AgentTranscriptStore::new(f.root.join("transcripts"));
    let member = f.member("root", None);
    f.store.prepare_budget_membership(&member).unwrap();
    transcripts
        .publish_conversation_budget_membership(&member)
        .unwrap();
    f.store
        .advance_budget_membership(
            &member,
            BudgetMembershipState::Pending,
            BudgetMembershipState::Published,
        )
        .unwrap();
    let lease = transcripts
        .prepare_native_budget_binding(&member, "original", None, false, None)
        .unwrap();
    let directory = f.root.join("transcripts/root");
    std::fs::rename(
        directory.join("budget-binding.lock"),
        directory.join("original-lock"),
    )
    .unwrap();
    use std::os::unix::fs::OpenOptionsExt;
    let replacement = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join("budget-binding.lock"))
        .unwrap();
    assert!(
        f.store
            .transfer_native_budget_binding(&member, "original", None, &lease)
            .is_err()
    );
    assert_eq!(f.store.budget_binding_owner(&member).unwrap(), None);
    assert!(transcripts.complete_native_budget_binding(&lease).is_err());
    drop((lease, replacement));
}

#[test]
fn budget_native_custody_last_holder_controls_transfer_without_reset() {
    let f = Fixture::new();
    let transcripts = AgentTranscriptStore::new(f.root.join("transcripts"));
    let member = f.member("root", None);
    let lease = f
        .store
        .publish_native_budget_membership(&transcripts, &member, "first", None)
        .unwrap();
    let worker = lease.clone();
    let a = attempt(&member, "issued");
    f.store.register_budget_attempt(&a).unwrap();
    f.store
        .commit_budget_receipt(&receipt(&a, "known", Some(19), Some(3), true))
        .unwrap();
    let before = f.store.budget_account(&member.root).unwrap();
    assert!(
        f.store
            .publish_native_budget_membership(&transcripts, &member, "second", None)
            .is_err()
    );
    // Another healthy account can activate while this one has live holders.
    let other = f.member("other", None);
    let unrelated = f
        .store
        .publish_native_budget_membership(&transcripts, &other, "other", None)
        .unwrap();
    drop(lease);
    assert!(
        f.store
            .publish_native_budget_membership(&transcripts, &member, "second", None)
            .is_err()
    );
    // Deleting a DB binding is not native retirement proof.
    assert!(f.store.release_budget_binding("root", "first").unwrap());
    assert!(
        f.store
            .publish_native_budget_membership(&transcripts, &member, "second", None)
            .is_err()
    );
    f.store.claim_budget_binding(&member, "first").unwrap();
    drop(worker);
    let successor = f
        .store
        .publish_native_budget_membership(&transcripts, &member, "second", None)
        .unwrap();
    assert_eq!(
        f.store.budget_binding_owner(&member).unwrap().as_deref(),
        Some("second")
    );
    assert_eq!(f.store.budget_account(&member.root).unwrap(), before);
    assert_eq!(
        f.store.budget_membership("root").unwrap().unwrap().0,
        member
    );
    assert!(f.store.release_budget_binding("root", "first").is_err());
    drop((successor, unrelated));
}

/// A complete post-CAS staging record can survive a failed rename. Repeating
/// the original operation must finish that exact record, not wedge forever or
/// delete arbitrary residue. No inference or effects are part of recovery.
#[test]
fn budget_native_custody_completed_stage_recovers_original_operation() {
    let f = Fixture::new();
    let transcripts = AgentTranscriptStore::new(f.root.join("transcripts"));
    let member = f.member("root", None);
    f.store.prepare_budget_membership(&member).unwrap();
    transcripts
        .publish_conversation_budget_membership(&member)
        .unwrap();
    f.store
        .advance_budget_membership(
            &member,
            BudgetMembershipState::Pending,
            BudgetMembershipState::Published,
        )
        .unwrap();
    let lease = transcripts
        .prepare_native_budget_binding(&member, "original", None, false, None)
        .unwrap();
    f.store
        .transfer_native_budget_binding(&member, "original", None, &lease)
        .unwrap();
    let directory = f.root.join("transcripts/root");
    let mut completed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("budget-binding.json")).unwrap())
            .unwrap();
    completed["bound"] = true.into();
    completed["database_owner"] = "original".into();
    let stage = directory.join(".budget-binding-v2.json.tmp");
    std::fs::write(&stage, serde_json::to_vec(&completed).unwrap()).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o600)).unwrap();
    drop(lease);
    let recovered = f
        .store
        .publish_native_budget_membership(&transcripts, &member, "original", None)
        .unwrap();
    assert!(!stage.exists());
    assert_eq!(
        f.store.budget_membership("root").unwrap().unwrap().1,
        BudgetMembershipState::Active
    );
    assert_eq!(
        f.store.budget_binding_owner(&member).unwrap().as_deref(),
        Some("original")
    );
    drop(recovered);
}

/// Actual process exit releases the original kernel holder. Every crash point
/// retains the same root/membership; recovery never reconstructs provider work.
#[test]
fn budget_native_custody_process_exit_recovers_each_publication_boundary() {
    const ENV: &str = "MEZ_TEST_NATIVE_BUDGET_EXIT";
    const TEST: &str = "storage::token_usage::budget::tests::native_custody::budget_native_custody_process_exit_recovers_each_publication_boundary";
    if let Some(root) = std::env::var_os(ENV) {
        let root = std::path::PathBuf::from(root);
        let store = TokenUsageStore::new(root.join("usage.sqlite"));
        let transcripts = AgentTranscriptStore::new(root.join("transcripts"));
        let member = store.budget_membership("root").unwrap().unwrap().0;
        let phase: u8 = std::env::var("MEZ_TEST_NATIVE_BUDGET_PHASE")
            .unwrap()
            .parse()
            .unwrap();
        let lease = transcripts
            .prepare_native_budget_binding(&member, "original", None, false, None)
            .unwrap();
        if phase >= 1 {
            store
                .transfer_native_budget_binding(&member, "original", None, &lease)
                .unwrap();
        }
        if phase >= 2 {
            transcripts.complete_native_budget_binding(&lease).unwrap();
        }
        if phase >= 3 {
            store
                .advance_budget_membership(
                    &member,
                    BudgetMembershipState::Published,
                    BudgetMembershipState::Active,
                )
                .unwrap();
        }
        std::process::exit(77);
    }
    for phase in 0..4 {
        let f = Fixture::new();
        let transcripts = AgentTranscriptStore::new(f.root.join("transcripts"));
        let member = f.member("root", None);
        f.store.prepare_budget_membership(&member).unwrap();
        transcripts
            .publish_conversation_budget_membership(&member)
            .unwrap();
        f.store
            .advance_budget_membership(
                &member,
                BudgetMembershipState::Pending,
                BudgetMembershipState::Published,
            )
            .unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--quiet"])
            .env(ENV, &f.root)
            .env("MEZ_TEST_NATIVE_BUDGET_PHASE", phase.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(77));
        let lease = f
            .store
            .publish_native_budget_membership(&transcripts, &member, "successor", None)
            .unwrap();
        assert_eq!(
            f.store.budget_membership("root").unwrap().unwrap(),
            (member.clone(), BudgetMembershipState::Active)
        );
        assert_eq!(
            f.store.budget_account(&member.root).unwrap().known,
            UsageTotals::default()
        );
        drop(lease);
    }
}
