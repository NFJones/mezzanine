//! Real private SQLite transactions. These tests qualify the repository only,
//! not runtime membership publication or provider admission.

use super::*;
use membership::BudgetMembership;
use mez_agent::token_budget::{BudgetAttemptId, BudgetReceiptId, UsageSnapshot};
use receipts::{BudgetAttempt, BudgetAttemptPurpose, BudgetReceipt};
use rusqlite::params;

struct Fixture {
    store: TokenUsageStore,
    root: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mez-budget-{}",
            super::super::new_token_usage_event_id()
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let store = TokenUsageStore::new(root.join("usage.sqlite"));
        store.initialize(100).unwrap();
        Self { store, root }
    }
    fn member(&self, conversation: &str, parent: Option<&BudgetMembership>) -> BudgetMembership {
        BudgetMembership {
            namespace: self.store.budget_namespace().unwrap(),
            root: parent.map_or_else(
                || BudgetRootId::new(format!("root-{conversation}")).unwrap(),
                |p| p.root.clone(),
            ),
            conversation: conversation.into(),
            parent: parent.map(|p| p.conversation.clone()),
            operation: format!("publish-{conversation}"),
            started_at: 100,
            legacy_coverage_gap: false,
        }
    }
    fn active(&self, member: &BudgetMembership) {
        assert_eq!(
            self.store.prepare_budget_membership(member).unwrap(),
            BudgetMembershipState::Pending
        );
        assert_eq!(
            self.store
                .advance_budget_membership(
                    member,
                    BudgetMembershipState::Pending,
                    BudgetMembershipState::Published
                )
                .unwrap(),
            BudgetMembershipState::Published
        );
        assert_eq!(
            self.store
                .advance_budget_membership(
                    member,
                    BudgetMembershipState::Published,
                    BudgetMembershipState::Active
                )
                .unwrap(),
            BudgetMembershipState::Active
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
        assert!(!self.root.exists());
    }
}
fn attempt(member: &BudgetMembership, id: &str) -> BudgetAttempt {
    BudgetAttempt {
        namespace: member.namespace.clone(),
        id: BudgetAttemptId::new(id).unwrap(),
        root: member.root.clone(),
        conversation: member.conversation.clone(),
        model: mez_agent::ModelTokenUsageKey::new("fake", "offline"),
        purpose: BudgetAttemptPurpose::Execution,
    }
}
fn receipt(
    attempt: &BudgetAttempt,
    id: &str,
    input: Option<u64>,
    output: Option<u64>,
    terminal: bool,
) -> BudgetReceipt {
    BudgetReceipt {
        id: BudgetReceiptId::new(id).unwrap(),
        attempt: attempt.clone(),
        snapshot: UsageSnapshot {
            input,
            output,
            terminal,
        },
    }
}

/// A child cannot run on pending ancestry or change an existing root. Publication
/// recovery returns exact current phase without restarting the operation.
#[test]
fn budget_storage_membership_phases_namespace_and_immutable_ancestry() {
    let f = Fixture::new();
    let root = f.member("root", None);
    let child = f.member("child", Some(&root));
    assert!(f.store.prepare_budget_membership(&child).is_err());
    assert_eq!(
        f.store.prepare_budget_membership(&root).unwrap(),
        BudgetMembershipState::Pending
    );
    assert!(
        f.store
            .register_budget_attempt(&attempt(&root, "too-early"))
            .is_err()
    );
    assert!(
        f.store
            .advance_budget_membership(
                &root,
                BudgetMembershipState::Pending,
                BudgetMembershipState::Active
            )
            .is_err()
    );
    f.store
        .advance_budget_membership(
            &root,
            BudgetMembershipState::Pending,
            BudgetMembershipState::Published,
        )
        .unwrap();
    f.store
        .advance_budget_membership(
            &root,
            BudgetMembershipState::Published,
            BudgetMembershipState::Active,
        )
        .unwrap();
    assert_eq!(
        f.store.prepare_budget_membership(&root).unwrap(),
        BudgetMembershipState::Active
    );
    f.active(&child);
    let nested = f.member("nested", Some(&child));
    f.active(&nested);
    assert_eq!(
        f.store.budget_membership("nested").unwrap().unwrap().0.root,
        root.root
    );
    let mut changed = child.clone();
    changed.root = BudgetRootId::new("foreign").unwrap();
    assert!(f.store.prepare_budget_membership(&changed).is_err());
    let mut changed = root.clone();
    changed.parent = Some(child.conversation.clone());
    assert!(f.store.prepare_budget_membership(&changed).is_err());
    let mut foreign = f.member("foreign", None);
    foreign.namespace = "other-namespace".into();
    assert!(f.store.prepare_budget_membership(&foreign).is_err());
    assert!(f.store.budget_membership("foreign").unwrap().is_none());
    assert_eq!(
        f.store.budget_account(&root.root).unwrap().known,
        UsageTotals::default()
    );
}

/// Lost acknowledgements re-query the same receipt outcome, even after newer
/// receipts. Prefix/final observations and a sibling charge the root only once.
#[test]
fn budget_storage_receipt_replay_reopen_retention_and_partial_final_conserve() {
    let f = Fixture::new();
    let root = f.member("root", None);
    f.active(&root);
    let child = f.member("child", Some(&root));
    f.active(&child);
    let a = attempt(&child, "attempt-a");
    assert!(f.store.register_budget_attempt(&a).unwrap());
    assert!(!f.store.register_budget_attempt(&a).unwrap());
    let prefix = receipt(&a, "prefix", Some(10), Some(2), false);
    let first = f.store.commit_budget_receipt(&prefix).unwrap();
    let final_receipt = receipt(&a, "final", None, Some(5), true);
    let final_outcome = f.store.commit_budget_receipt(&final_receipt).unwrap();
    assert_eq!(
        final_outcome.known,
        UsageTotals {
            input: 10,
            output: 5
        }
    );
    let reopened = TokenUsageStore::new(f.store.path());
    assert_eq!(reopened.commit_budget_receipt(&prefix).unwrap(), first);
    assert_eq!(
        reopened.commit_budget_receipt(&final_receipt).unwrap(),
        final_outcome
    );
    let mut changed = prefix.clone();
    changed.snapshot.input = Some(11);
    assert!(reopened.commit_budget_receipt(&changed).is_err());
    let duplicate_final = receipt(&a, "new-receipt-same-final", None, Some(5), true);
    assert_eq!(
        reopened
            .commit_budget_receipt(&duplicate_final)
            .unwrap()
            .known,
        final_outcome.known
    );
    let connection = reopened.open_budget().unwrap();
    let stored: String = connection
        .query_row(
            "SELECT snapshot FROM budget_attempts WHERE id=?1",
            [a.id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored["prefix"]["input"], 10);
    assert!(stored["terminal"]["input"].is_null());
    drop(connection);
    reopened.initialize(1000 * 86_400).unwrap();
    assert_eq!(reopened.commit_budget_receipt(&prefix).unwrap(), first);
    assert_eq!(
        reopened.budget_account(&root.root).unwrap().known,
        final_outcome.known
    );
    let zero = attempt(&root, "zero");
    reopened.register_budget_attempt(&zero).unwrap();
    let z = reopened
        .commit_budget_receipt(&receipt(&zero, "zero-receipt", Some(0), Some(0), true))
        .unwrap();
    assert_eq!(z.known, final_outcome.known);
}

/// SQLite refuses the last INSERT after both UPDATEs: transaction rollback must
/// leave root and prefix untouched so retry adopts only original bookkeeping.
#[test]
fn budget_storage_actual_sql_refusal_and_overflow_are_atomic() {
    let f = Fixture::new();
    let root = f.member("root", None);
    f.active(&root);
    let a = attempt(&root, "attempt");
    f.store.register_budget_attempt(&a).unwrap();
    let r = receipt(&a, "receipt", Some(10), Some(3), true);
    let connection = f.store.open_budget().unwrap();
    connection.execute_batch("CREATE TRIGGER refuse_budget BEFORE INSERT ON budget_receipts BEGIN SELECT RAISE(ABORT,'fixture refusal'); END;").unwrap();
    assert!(f.store.commit_budget_receipt(&r).is_err());
    assert_eq!(
        f.store.budget_account(&root.root).unwrap().known,
        UsageTotals::default()
    );
    connection
        .execute_batch("DROP TRIGGER refuse_budget;")
        .unwrap();
    assert_eq!(
        f.store.commit_budget_receipt(&r).unwrap().known,
        UsageTotals {
            input: 10,
            output: 3
        }
    );
    let b = attempt(&root, "overflow");
    f.store.register_budget_attempt(&b).unwrap();
    let mut before = f.store.budget_account(&root.root).unwrap();
    before.known.output = u64::MAX;
    connection
        .execute(
            "UPDATE budget_roots SET body=?1 WHERE id=?2",
            params![encode(&before).unwrap(), root.root.as_str()],
        )
        .unwrap();
    assert!(
        f.store
            .commit_budget_receipt(&receipt(&b, "overflow-receipt", Some(1), Some(1), true))
            .is_err()
    );
    assert_eq!(f.store.budget_account(&root.root).unwrap(), before);
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM budget_receipts WHERE id='overflow-receipt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn budget_storage_regression_provenance_and_missing_store_refuse_without_reset() {
    let f = Fixture::new();
    let root = f.member("root", None);
    f.active(&root);
    let a = attempt(&root, "attempt");
    f.store.register_budget_attempt(&a).unwrap();
    f.store
        .commit_budget_receipt(&receipt(&a, "prefix", Some(5), Some(3), false))
        .unwrap();
    let before = f.store.budget_account(&root.root).unwrap();
    assert!(
        f.store
            .commit_budget_receipt(&receipt(&a, "regression", Some(6), Some(2), true))
            .is_err()
    );
    let mut foreign = receipt(&a, "foreign", Some(6), Some(4), true);
    foreign.attempt.conversation = "other".into();
    assert!(f.store.commit_budget_receipt(&foreign).is_err());
    assert_eq!(f.store.budget_account(&root.root).unwrap(), before);
    let missing_path = f.root.join("missing.sqlite");
    assert!(
        TokenUsageStore::new(&missing_path)
            .budget_namespace()
            .is_err()
    );
    assert!(!missing_path.exists());
    let connection = f.store.open_budget().unwrap();
    connection
        .pragma_update(
            None,
            "user_version",
            super::super::store::SCHEMA_VERSION + 1,
        )
        .unwrap();
    assert!(f.store.budget_account(&root.root).is_err());
}

/// Independent off-actor workers serialize their root updates through SQLite,
/// not a process-local counter that would lose one participant's expense.
#[test]
fn budget_storage_concurrent_original_receipts_sum_once() {
    let f = Fixture::new();
    let root = f.member("root", None);
    f.active(&root);
    let a = attempt(&root, "a");
    let b = attempt(&root, "b");
    f.store.register_budget_attempt(&a).unwrap();
    f.store.register_budget_attempt(&b).unwrap();
    let r1 = receipt(&a, "one", Some(7), Some(2), true);
    let r2 = receipt(&b, "two", Some(9), Some(3), true);
    let s1 = f.store.clone();
    let s2 = f.store.clone();
    let one = std::thread::spawn(move || s1.commit_budget_receipt(&r1));
    let two = std::thread::spawn(move || s2.commit_budget_receipt(&r2));
    one.join().unwrap().unwrap();
    two.join().unwrap().unwrap();
    assert_eq!(
        f.store.budget_account(&root.root).unwrap().known,
        UsageTotals {
            input: 16,
            output: 5
        }
    );
}

/// Policy edits are absolute, independently authored and exactly replayable;
/// invalid second values cannot partly install a first value or clear expense.
#[test]
fn budget_storage_policy_reopen_replay_and_invalid_second_field_are_atomic() {
    use mez_agent::token_budget::TokenLimit;
    use operations::BudgetPolicyOperation;
    let f = Fixture::new();
    let root = f.member("root", None);
    f.active(&root);
    let a = attempt(&root, "a");
    f.store.register_budget_attempt(&a).unwrap();
    f.store
        .commit_budget_receipt(&receipt(&a, "expense", Some(10), Some(4), true))
        .unwrap();
    let first = BudgetPolicyOperation {
        id: "policy-one".into(),
        namespace: root.namespace.clone(),
        root: root.root.clone(),
        input: Some(BudgetOverride::Explicit(TokenLimit::Limited(10))),
        output: Some(BudgetOverride::Explicit(TokenLimit::Unlimited)),
    };
    let original = f.store.commit_budget_policy(&first).unwrap();
    let second = BudgetPolicyOperation {
        id: "policy-two".into(),
        input: Some(BudgetOverride::Inherit),
        output: None,
        ..first.clone()
    };
    let latest = f.store.commit_budget_policy(&second).unwrap();
    assert_eq!(latest.known, original.known);
    assert_eq!(latest.input_override, BudgetOverride::Inherit);
    assert_eq!(
        latest.output_override,
        BudgetOverride::Explicit(TokenLimit::Unlimited)
    );
    let reopened = TokenUsageStore::new(f.store.path());
    assert_eq!(reopened.commit_budget_policy(&first).unwrap(), original);
    assert_eq!(reopened.budget_account(&root.root).unwrap(), latest);
    let mut invalid = second.clone();
    invalid.id = "invalid".into();
    invalid.input = Some(BudgetOverride::Explicit(TokenLimit::Limited(20)));
    invalid.output = Some(BudgetOverride::Explicit(TokenLimit::Limited(u64::MAX)));
    assert!(reopened.commit_budget_policy(&invalid).is_err());
    assert_eq!(reopened.budget_account(&root.root).unwrap(), latest);
    let mut changed = first.clone();
    changed.input = Some(BudgetOverride::Inherit);
    assert!(reopened.commit_budget_policy(&changed).is_err());
}

#[test]
fn budget_storage_binding_is_exact_exclusive_and_has_no_age_reclamation() {
    let f = Fixture::new();
    let root = f.member("root", None);
    f.store.prepare_budget_membership(&root).unwrap();
    assert!(f.store.claim_budget_binding(&root, "owner-a").is_err());
    f.store
        .advance_budget_membership(
            &root,
            BudgetMembershipState::Pending,
            BudgetMembershipState::Published,
        )
        .unwrap();
    assert!(f.store.claim_budget_binding(&root, "owner-a").unwrap());
    assert!(!f.store.claim_budget_binding(&root, "owner-a").unwrap());
    assert!(f.store.claim_budget_binding(&root, "owner-b").is_err());
    assert!(f.store.release_budget_binding("root", "owner-b").is_err());
    assert!(f.store.release_budget_binding("root", "owner-a").unwrap());
    assert!(!f.store.release_budget_binding("root", "owner-a").unwrap());
    assert!(f.store.claim_budget_binding(&root, "owner-b").unwrap());
    assert!(f.store.release_budget_binding("root", "owner-a").is_err());
}

/// Migration rolls back a colliding object rather than adopting an arbitrary
/// similarly named table, and preserves unrelated legacy expense on success.
#[test]
fn budget_storage_schema14_upgrade_and_collision_preserve_original_data() {
    let f = Fixture::new();
    let connection = f.store.open().unwrap();
    super::super::remove_empty_budget_schema_for_legacy_fixture(&connection);
    connection.execute_batch("PRAGMA user_version=14; CREATE TABLE budget_roots(collision TEXT); INSERT INTO budget_roots VALUES('sentinel');").unwrap();
    assert!(f.store.initialize(100).is_err());
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 14);
    let sentinel: String = connection
        .query_row("SELECT collision FROM budget_roots", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sentinel, "sentinel");
    assert!(
        connection
            .prepare("SELECT * FROM budget_namespace")
            .is_err()
    );
    connection
        .execute_batch("DROP TABLE budget_roots;")
        .unwrap();
    f.store.initialize(100).unwrap();
    let namespace = f.store.budget_namespace().unwrap();
    f.store.initialize(100).unwrap();
    assert_eq!(f.store.budget_namespace().unwrap(), namespace);
}

/// Actual metadata publication refusal retains the original pending DB record;
/// retry completes the same operation without minting another account.
#[test]
fn budget_publication_metadata_failure_recovers_original_operation() {
    let f = Fixture::new();
    let transcripts =
        crate::storage::transcript::AgentTranscriptStore::new(f.root.join("transcripts"));
    let member = f.member("root", None);
    transcripts.fail_next_metadata_write_after_promotion();
    assert!(
        f.store
            .publish_budget_membership(&transcripts, &member, "owner")
            .is_err()
    );
    assert_eq!(
        f.store.budget_membership("root").unwrap().unwrap().1,
        BudgetMembershipState::Pending
    );
    assert!(
        f.store
            .register_budget_attempt(&attempt(&member, "early"))
            .is_err()
    );
    f.store
        .publish_budget_membership(&transcripts, &member, "owner")
        .unwrap();
    assert_eq!(
        f.store.budget_membership("root").unwrap().unwrap().1,
        BudgetMembershipState::Active
    );
    assert_eq!(
        transcripts.conversation_budget_membership("root").unwrap(),
        Some(member.clone())
    );
    assert!(
        transcripts
            .has_budget_binding_mirror(&member, "owner")
            .unwrap()
    );
    let reopened = TokenUsageStore::new(f.store.path());
    reopened
        .publish_budget_membership(&transcripts, &member, "owner")
        .unwrap();
    assert!(
        reopened
            .publish_budget_membership(&transcripts, &member, "foreign")
            .is_err()
    );
    assert!(
        reopened
            .register_budget_attempt(&attempt(&member, "allowed"))
            .unwrap()
    );
}

/// A real wrong-kind mirror node refuses after metadata publication and binding
/// reservation. Repair resumes publication, not model work or an alternate root.
#[test]
fn budget_publication_mirror_failure_retains_published_phase() {
    let f = Fixture::new();
    let transcripts =
        crate::storage::transcript::AgentTranscriptStore::new(f.root.join("transcripts"));
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
    let mirror = f.root.join("transcripts/root/budget-binding.json");
    std::fs::create_dir(&mirror).unwrap();
    assert!(
        f.store
            .publish_budget_membership(&transcripts, &member, "owner")
            .is_err()
    );
    assert_eq!(
        f.store.budget_membership("root").unwrap().unwrap().1,
        BudgetMembershipState::Published
    );
    assert!(f.store.claim_budget_binding(&member, "foreign").is_err());
    std::fs::remove_dir(&mirror).unwrap();
    f.store
        .publish_budget_membership(&transcripts, &member, "owner")
        .unwrap();
    assert!(
        transcripts
            .has_budget_binding_mirror(&member, "owner")
            .unwrap()
    );
}

/// Already-active metadata/mirror loss is not a fresh publication. Future or
/// foreign subjects refuse without changing the original lifetime account.
#[test]
fn budget_publication_active_missing_future_and_foreign_evidence_refuse() {
    let f = Fixture::new();
    let transcripts =
        crate::storage::transcript::AgentTranscriptStore::new(f.root.join("transcripts"));
    let member = f.member("root", None);
    f.store
        .publish_budget_membership(&transcripts, &member, "owner")
        .unwrap();
    let metadata = f.root.join("transcripts/root/metadata.json");
    let mirror = f.root.join("transcripts/root/budget-binding.json");
    let original_metadata = std::fs::read(&metadata).unwrap();
    let original_mirror = std::fs::read(&mirror).unwrap();
    std::fs::remove_file(&mirror).unwrap();
    assert!(
        f.store
            .publish_budget_membership(&transcripts, &member, "owner")
            .is_err()
    );
    assert!(!mirror.exists());
    let mut future: serde_json::Value = serde_json::from_slice(&original_mirror).unwrap();
    future["version"] = serde_json::json!(2);
    std::fs::write(&mirror, serde_json::to_vec(&future).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&mirror, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert!(
        f.store
            .publish_budget_membership(&transcripts, &member, "owner")
            .is_err()
    );
    std::fs::write(&mirror, &original_mirror).unwrap();
    let mut foreign: serde_json::Value = serde_json::from_slice(&original_metadata).unwrap();
    foreign["budget_membership"]["conversation"] = serde_json::json!("other");
    std::fs::write(&metadata, serde_json::to_vec(&foreign).unwrap()).unwrap();
    assert!(transcripts.conversation_budget_membership("root").is_err());
    std::fs::write(&metadata, &original_metadata).unwrap();
    std::fs::remove_file(&metadata).unwrap();
    assert!(
        f.store
            .publish_budget_membership(&transcripts, &member, "owner")
            .is_err()
    );
    assert!(!metadata.exists());
    assert_eq!(
        f.store.budget_account(&member.root).unwrap().known,
        UsageTotals::default()
    );
}

/// The fixed temporary slot supports exact complete crash residue, but never
/// overwrites partial/foreign content or follows links/blocks on a FIFO.
#[test]
fn budget_publication_temporary_recovery_and_special_nodes_are_bounded() {
    let f = Fixture::new();
    let transcripts =
        crate::storage::transcript::AgentTranscriptStore::new(f.root.join("transcripts"));
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
    f.store.claim_budget_binding(&member, "owner").unwrap();
    transcripts
        .publish_budget_binding_mirror(&member, "owner")
        .unwrap();
    let mirror = f.root.join("transcripts/root/budget-binding.json");
    let temporary = f.root.join("transcripts/root/.budget-binding.json.tmp");
    std::fs::rename(&mirror, &temporary).unwrap();
    f.store
        .publish_budget_membership(&transcripts, &member, "owner")
        .unwrap();
    assert!(mirror.exists());
    assert!(!temporary.exists());
    #[cfg(unix)]
    {
        let saved = f.root.join("saved-mirror");
        std::fs::rename(&mirror, &saved).unwrap();
        std::os::unix::fs::symlink(&saved, &mirror).unwrap();
        assert!(
            transcripts
                .has_budget_binding_mirror(&member, "owner")
                .is_err()
        );
        std::fs::remove_file(&mirror).unwrap();
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            &mirror,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        assert!(
            transcripts
                .has_budget_binding_mirror(&member, "owner")
                .is_err()
        );
        std::fs::remove_file(&mirror).unwrap();
        std::fs::rename(&saved, &mirror).unwrap();
    }
}
