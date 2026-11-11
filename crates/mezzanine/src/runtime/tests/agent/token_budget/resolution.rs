//! Explicit legacy choices use real preparation, SQLite and native mirror
//! publication. No supplied lineage or historical aggregate becomes expense.

use super::*;
use crate::runtime::agent::token_budget::RuntimeBudgetLegacyChoice as Choice;

/// A committed existing-account association keeps its immutable operation even
/// if actor checkpointing and then the anchor mirror become unavailable.
#[tokio::test]
async fn runtime_budget_legacy_existing_partial_publication_cannot_reselect() {
    let mut scene = Scene::new();
    let primary = primary(&mut scene);
    let original = scene.start();
    prepare(&mut scene, &original).await.unwrap();
    let anchor = scene
        .store
        .budget_membership(&original.conversation_id)
        .unwrap()
        .unwrap()
        .0;
    let mirror = scene
        ._root
        .join("transcripts")
        .join(&anchor.conversation)
        .join("budget-binding.json");
    let bytes = fs::read(&mirror).unwrap();
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    let turn = unresolved(&mut scene, "legacy-partial-existing").await;
    let confirmation = scene
        .service
        .prepare_runtime_budget_legacy_resolution(
            &primary,
            "%1",
            Choice::Existing {
                root: anchor.root.clone(),
                anchor: anchor.conversation.clone(),
            },
        )
        .unwrap();
    scene
        .service
        .confirm_runtime_budget_legacy_resolution(&primary, confirmation)
        .unwrap();
    scene.transcripts.fail_next_agent_session_metadata_write();
    assert!(prepare(&mut scene, &turn).await.is_err());
    let published = scene
        .store
        .budget_membership(&turn.conversation_id)
        .unwrap()
        .unwrap();
    let account = scene.store.budget_account(&anchor.root).unwrap();
    fs::write(&mirror, b"invalid owned fixture mirror").unwrap();
    assert!(prepare(&mut scene, &turn).await.is_err());
    assert!(
        scene
            .service
            .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent)
            .is_err()
    );
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    assert_eq!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .unwrap(),
        published
    );
    assert_eq!(scene.store.budget_account(&anchor.root).unwrap(), account);
    fs::write(&mirror, bytes).unwrap();
    prepare(&mut scene, &turn).await.unwrap();
    assert!(scene.service.runtime_budget_membership_is_ready(&turn));
    assert_eq!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .unwrap(),
        published
    );
    assert_eq!(scene.store.budget_account(&anchor.root).unwrap(), account);
}

/// An unpublished refusal cannot release another worker's captured proposal.
/// Only a fresh result after that original holder retires can enable correction.
#[tokio::test]
async fn runtime_budget_legacy_correction_waits_for_outstanding_worker() {
    let mut scene = Scene::new();
    let primary = primary(&mut scene);
    let turn = unresolved(&mut scene, "legacy-outstanding-choice").await;
    let confirmation = scene
        .service
        .prepare_runtime_budget_legacy_resolution(
            &primary,
            "%1",
            Choice::Existing {
                root: mez_agent::token_budget::BudgetRootId::new("missing-root").unwrap(),
                anchor: "missing-anchor".into(),
            },
        )
        .unwrap();
    scene
        .service
        .confirm_runtime_budget_legacy_resolution(&primary, confirmation)
        .unwrap();
    let held = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    assert!(prepare(&mut scene, &turn).await.is_err());
    assert!(
        scene
            .service
            .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent)
            .is_err()
    );
    drop(held);
    assert!(prepare(&mut scene, &turn).await.is_err());
    let correction = scene
        .service
        .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent)
        .unwrap();
    scene
        .service
        .confirm_runtime_budget_legacy_resolution(&primary, correction)
        .unwrap();
    prepare(&mut scene, &turn).await.unwrap();
    assert!(scene.service.runtime_budget_membership_is_ready(&turn));
}

/// A repository object compares paths, not database namespaces. Human approval
/// must not authorize creating an account in a replacement ledger at that path.
#[tokio::test]
async fn runtime_budget_legacy_same_path_namespace_change_refuses_publication() {
    let mut scene = Scene::new();
    let primary = primary(&mut scene);
    let turn = unresolved(&mut scene, "legacy-replaced-namespace").await;
    let confirmation = scene
        .service
        .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent)
        .unwrap();
    scene
        .service
        .confirm_runtime_budget_legacy_resolution(&primary, confirmation)
        .unwrap();
    let connection = rusqlite::Connection::open(scene.store.path()).unwrap();
    connection
        .execute(
            "UPDATE budget_namespace SET namespace='replacement-namespace'",
            [],
        )
        .unwrap();
    drop(connection);
    let result = prepare(&mut scene, &turn).await;
    let membership = scene
        .store
        .budget_membership(&turn.conversation_id)
        .unwrap();
    drop(scene);
    assert!(
        result.is_err(),
        "confirmed proposal must retain original namespace"
    );
    assert!(membership.is_none());
}

/// An Active row plus descriptive sidecar is insufficient when the original
/// completed native mirror has disappeared. Association must not repair it.
#[tokio::test]
async fn runtime_budget_legacy_missing_anchor_mirror_refuses_without_registration() {
    let mut scene = Scene::new();
    let primary = primary(&mut scene);
    let original = scene.start();
    prepare(&mut scene, &original).await.unwrap();
    let anchor = scene
        .store
        .budget_membership(&original.conversation_id)
        .unwrap()
        .unwrap()
        .0;
    let before = scene.store.budget_account(&anchor.root).unwrap();
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    let turn = unresolved(&mut scene, "legacy-missing-anchor-mirror").await;
    let confirmation = scene
        .service
        .prepare_runtime_budget_legacy_resolution(
            &primary,
            "%1",
            Choice::Existing {
                root: anchor.root.clone(),
                anchor: anchor.conversation.clone(),
            },
        )
        .unwrap();
    scene
        .service
        .confirm_runtime_budget_legacy_resolution(&primary, confirmation)
        .unwrap();
    fs::remove_file(
        scene
            ._root
            .join("transcripts")
            .join(&anchor.conversation)
            .join("budget-binding.json"),
    )
    .unwrap();
    let result = prepare(&mut scene, &turn).await;
    let registered = scene
        .store
        .budget_membership(&turn.conversation_id)
        .unwrap();
    let account = scene.store.budget_account(&anchor.root).unwrap();
    // All native resources and exact fixture files are retired by Scene even
    // on the desired-behavior RED assertion.
    drop(scene);
    assert!(
        result.is_err(),
        "missing anchor mirror must refuse association"
    );
    assert!(registered.is_none());
    assert_eq!(account, before);
}

/// Losing the actor checkpoint after publication must retain the one original
/// membership/root/operation. Repeating preparation repairs bookkeeping only.
#[tokio::test]
async fn runtime_budget_legacy_checkpoint_failure_retries_original_proposal() {
    let mut scene = Scene::new();
    let primary = primary(&mut scene);
    let turn = unresolved(&mut scene, "legacy-lost-checkpoint").await;
    let confirmation = scene
        .service
        .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent)
        .unwrap();
    let root = confirmation.disclosure().root.clone();
    scene
        .service
        .confirm_runtime_budget_legacy_resolution(&primary, confirmation)
        .unwrap();
    scene.transcripts.fail_next_agent_session_metadata_write();
    assert!(prepare(&mut scene, &turn).await.is_err());
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    let original = scene
        .store
        .budget_membership(&turn.conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(original.0.root, root);
    assert_eq!(original.1, BudgetMembershipState::Active);
    assert!(
        scene
            .service
            .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent,)
            .is_err()
    );
    prepare(&mut scene, &turn).await.unwrap();
    assert!(scene.service.runtime_budget_membership_is_ready(&turn));
    assert_eq!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .unwrap(),
        original
    );
}

/// A delayed membership writer can win after confirmation capture; the worker
/// must reject that conflict, not erase original provenance or recreate totals.
#[tokio::test]
async fn runtime_budget_legacy_concurrent_publication_cannot_reset_known_account() {
    let mut scene = Scene::new();
    let primary = primary(&mut scene);
    let turn = unresolved(&mut scene, "legacy-occupied").await;
    let confirmation = scene
        .service
        .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent)
        .unwrap();
    let original = crate::storage::token_usage::budget::membership::BudgetMembership {
        namespace: scene.store.budget_namespace().unwrap(),
        root: mez_agent::token_budget::BudgetRootId::new("prior-occupied-root").unwrap(),
        conversation: turn.conversation_id.clone(),
        parent: None,
        operation: "prior-occupied-operation".into(),
        started_at: 7,
        legacy_coverage_gap: true,
    };
    scene
        .store
        .publish_budget_membership(&scene.transcripts, &original, "prior-owner")
        .unwrap();
    let account = scene.store.budget_account(&original.root).unwrap();
    scene
        .service
        .confirm_runtime_budget_legacy_resolution(&primary, confirmation)
        .unwrap();
    assert!(prepare(&mut scene, &turn).await.is_err());
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    assert_eq!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .unwrap()
            .0,
        original
    );
    assert_eq!(scene.store.budget_account(&original.root).unwrap(), account);
}

async fn unresolved(scene: &mut Scene, conversation: &str) -> AgentTurnRecord {
    scene
        .service
        .agent_shell_store_mut()
        .bind_conversation("%1", conversation, 0)
        .unwrap();
    let turn = scene.start();
    let work = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    assert!(
        scene
            .service
            .apply_agent_provider_preparation(outcome)
            .is_err()
    );
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    assert!(
        scene
            .store
            .budget_membership(conversation)
            .unwrap()
            .is_none()
    );
    turn
}

async fn prepare(scene: &mut Scene, turn: &AgentTurnRecord) -> Result<()> {
    let work = scene.service.prepare_agent_provider_work(&turn.turn_id)?;
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene.service.apply_agent_provider_preparation(outcome)
}

fn primary(scene: &mut Scene) -> mez_core::ids::ClientId {
    scene
        .service
        .attach_primary("primary", true, Size::new(120, 30).unwrap(), 120)
        .unwrap()
}

#[tokio::test]
async fn runtime_budget_legacy_cancel_then_confirm_discloses_original_boundary() {
    let mut scene = Scene::new();
    let primary = primary(&mut scene);
    let turn = unresolved(&mut scene, "legacy-unknown").await;
    let cancelled = scene
        .service
        .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent)
        .unwrap();
    assert_eq!(cancelled.disclosure().conversation, turn.conversation_id);
    drop(cancelled);
    // Cancellation does not install a proposal, fabricate zero history, or
    // unblock the closest provider readiness gate.
    assert!(prepare(&mut scene, &turn).await.is_err());
    let confirmation = scene
        .service
        .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent)
        .unwrap();
    let disclosure = confirmation.disclosure();
    let root = disclosure.root.clone();
    let boundary = disclosure.membership_started_at;
    assert_eq!(disclosure.choice, &Choice::NewIndependent);
    assert!(disclosure.historical_usage_incomplete);
    assert_eq!(
        disclosure.account_start,
        crate::runtime::agent::token_budget::RuntimeBudgetLegacyAccountStart::NewAt(boundary)
    );
    scene
        .service
        .confirm_runtime_budget_legacy_resolution(&primary, confirmation)
        .unwrap();
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    assert!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .is_none()
    );
    prepare(&mut scene, &turn).await.unwrap();
    let member = scene
        .store
        .budget_membership(&turn.conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(member.1, BudgetMembershipState::Active);
    assert_eq!(member.0.root, root);
    assert_eq!(member.0.started_at, boundary);
    assert!(member.0.legacy_coverage_gap);
    let account = scene.store.budget_account(&root).unwrap();
    assert!(account.legacy_coverage_gap);
    assert_eq!(account.started_at, boundary);
    assert_eq!(account.known.input, 0);
    assert_eq!(account.known.output, 0);
    let reference = scene
        .service
        .runtime_budget_checkpoint_reference(&turn.conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(reference.root, root);
    // Exact preparation replay retains membership, root and operation.
    prepare(&mut scene, &turn).await.unwrap();
    assert_eq!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .unwrap(),
        member
    );
    assert!(
        scene
            .service
            .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent)
            .is_err()
    );
}

#[tokio::test]
async fn runtime_budget_legacy_existing_account_retains_expense_and_marks_gap() {
    use crate::storage::token_usage::budget::receipts::{
        BudgetAttempt, BudgetAttemptPurpose, BudgetReceipt,
    };
    use mez_agent::token_budget::{BudgetAttemptId, BudgetReceiptId, UsageSnapshot};
    let mut scene = Scene::new();
    let primary = primary(&mut scene);
    let original = scene.start();
    prepare(&mut scene, &original).await.unwrap();
    let anchor = scene
        .store
        .budget_membership(&original.conversation_id)
        .unwrap()
        .unwrap()
        .0;
    let attempt = BudgetAttempt {
        namespace: anchor.namespace.clone(),
        id: BudgetAttemptId::new("legacy-original-spend").unwrap(),
        root: anchor.root.clone(),
        conversation: anchor.conversation.clone(),
        model: mez_agent::ModelTokenUsageKey::new("offline", "source"),
        purpose: BudgetAttemptPurpose::Execution,
        reporting_origin: None,
    };
    scene.store.register_budget_attempt(&attempt).unwrap();
    scene
        .store
        .commit_budget_receipt(&BudgetReceipt {
            id: BudgetReceiptId::new("legacy-original-receipt").unwrap(),
            attempt,
            snapshot: UsageSnapshot {
                input: Some(31),
                output: Some(7),
                terminal: true,
            },
            reporting: None,
        })
        .unwrap();
    let before = scene.store.budget_account(&anchor.root).unwrap();
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    let turn = unresolved(&mut scene, "legacy-associated").await;
    let confirmation = scene
        .service
        .prepare_runtime_budget_legacy_resolution(
            &primary,
            "%1",
            Choice::Existing {
                root: anchor.root.clone(),
                anchor: anchor.conversation.clone(),
            },
        )
        .unwrap();
    let disclosure = confirmation.disclosure();
    let boundary = disclosure.membership_started_at;
    assert_eq!(
        disclosure.choice,
        &Choice::Existing {
            root: anchor.root.clone(),
            anchor: anchor.conversation.clone()
        }
    );
    assert!(disclosure.historical_usage_incomplete);
    assert_eq!(
        disclosure.account_start,
        crate::runtime::agent::token_budget::RuntimeBudgetLegacyAccountStart::PreserveExisting
    );
    scene
        .service
        .confirm_runtime_budget_legacy_resolution(&primary, confirmation)
        .unwrap();
    prepare(&mut scene, &turn).await.unwrap();
    let membership = scene
        .store
        .budget_membership(&turn.conversation_id)
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(membership.root, anchor.root);
    assert_eq!(
        membership.parent.as_deref(),
        Some(anchor.conversation.as_str())
    );
    assert!(membership.legacy_coverage_gap);
    assert_eq!(membership.started_at, boundary);
    let after = scene.store.budget_account(&anchor.root).unwrap();
    assert_eq!(after.known, before.known);
    assert_eq!(after.started_at, before.started_at);
    assert_eq!(after.input_override, before.input_override);
    assert_eq!(after.output_override, before.output_override);
    assert!(after.legacy_coverage_gap);
    assert_eq!(after.accounting_revision, before.accounting_revision + 1);
    prepare(&mut scene, &turn).await.unwrap();
    assert_eq!(scene.store.budget_account(&anchor.root).unwrap(), after);
    assert_eq!(
        scene
            .store
            .budget_membership(&anchor.conversation)
            .unwrap()
            .unwrap()
            .0,
        anchor
    );
}

/// Actor I/O-free confirmation cannot manufacture authority from a client ID,
/// survive a rebind, race another preparation, or adopt a replacement store.
#[tokio::test]
async fn runtime_budget_legacy_confirmation_authentication_and_stale_fences() {
    let mut scene = Scene::new();
    let primary = primary(&mut scene);
    let turn = unresolved(&mut scene, "legacy-fences").await;
    let foreign = mez_core::ids::ClientId::opaque("not-attached").unwrap();
    assert!(
        scene
            .service
            .prepare_runtime_budget_legacy_resolution(&foreign, "%1", Choice::NewIndependent)
            .is_err()
    );
    let confirmation = scene
        .service
        .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent)
        .unwrap();
    assert!(
        scene
            .service
            .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent)
            .is_err()
    );
    assert!(
        scene
            .service
            .confirm_runtime_budget_legacy_resolution(&foreign, confirmation)
            .is_err()
    );
    let confirmation = scene
        .service
        .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent)
        .unwrap();
    let held = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    assert!(
        scene
            .service
            .confirm_runtime_budget_legacy_resolution(&primary, confirmation)
            .is_err()
    );
    assert!(
        scene
            .service
            .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent)
            .is_err()
    );
    drop(held);
    let confirmation = scene
        .service
        .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent)
        .unwrap();
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    scene
        .service
        .agent_shell_store_mut()
        .bind_conversation("%1", &turn.conversation_id, 0)
        .unwrap();
    assert!(
        scene
            .service
            .confirm_runtime_budget_legacy_resolution(&primary, confirmation)
            .is_err()
    );
    let turn = scene.start();
    assert!(prepare(&mut scene, &turn).await.is_err());
    let confirmation = scene
        .service
        .prepare_runtime_budget_legacy_resolution(&primary, "%1", Choice::NewIndependent)
        .unwrap();
    scene
        .service
        .set_token_usage_store(TokenUsageStore::new(scene._root.join("missing.sqlite")));
    assert!(
        scene
            .service
            .confirm_runtime_budget_legacy_resolution(&primary, confirmation)
            .is_err()
    );
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    assert!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .is_none()
    );
}

/// A declared existing-root selector remains untrusted until the worker finds
/// its exact active database + sidecar anchor. Refusal never creates an account.
#[tokio::test]
async fn runtime_budget_legacy_unknown_anchor_remains_nonrunnable() {
    let mut scene = Scene::new();
    let primary = primary(&mut scene);
    let turn = unresolved(&mut scene, "legacy-bad-anchor").await;
    let confirmation = scene
        .service
        .prepare_runtime_budget_legacy_resolution(
            &primary,
            "%1",
            Choice::Existing {
                root: mez_agent::token_budget::BudgetRootId::new("missing-root").unwrap(),
                anchor: "missing-anchor".into(),
            },
        )
        .unwrap();
    scene
        .service
        .confirm_runtime_budget_legacy_resolution(&primary, confirmation)
        .unwrap();
    assert!(prepare(&mut scene, &turn).await.is_err());
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    assert!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .is_none()
    );
    assert!(
        scene
            .store
            .budget_account(&mez_agent::token_budget::BudgetRootId::new("missing-root").unwrap())
            .is_err()
    );
    let correction = scene.service.prepare_runtime_budget_legacy_resolution(
        &primary,
        "%1",
        Choice::NewIndependent,
    );
    assert!(
        correction.is_ok(),
        "conclusively unpublished selection must allow correction"
    );
    scene
        .service
        .confirm_runtime_budget_legacy_resolution(&primary, correction.unwrap())
        .unwrap();
    prepare(&mut scene, &turn).await.unwrap();
    assert!(scene.service.runtime_budget_membership_is_ready(&turn));
}
