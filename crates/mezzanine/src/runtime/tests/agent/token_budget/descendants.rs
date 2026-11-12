//! Real delegated and routed lifecycle boundaries retain the original expense
//! owner. Synthetic receipts qualify accounting, not physical provider sends.

use super::*;
use crate::storage::token_usage::budget::membership::BudgetMembership;
use crate::storage::token_usage::budget::receipts::BudgetAttemptPurpose;
use mez_agent::token_budget::{UsageSnapshot, UsageTotals};

async fn prepare(scene: &mut Scene, turn: &AgentTurnRecord) -> BudgetMembership {
    assert!(!scene.service.runtime_budget_membership_is_ready(turn));
    assert!(
        scene
            .service
            .claim_configured_agent_provider_task(
                &AgentId::opaque(turn.agent_id.clone()).unwrap(),
                &turn.turn_id,
            )
            .unwrap()
            .is_none(),
        "unpublished membership must not release a provider request",
    );
    assert!(scene.service.agent_provider_task_is_pending(&turn.turn_id));
    let work = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    assert!(scene.service.runtime_budget_membership_is_ready(turn));
    let (member, phase) = scene
        .store
        .budget_membership(&turn.conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(phase, BudgetMembershipState::Active);
    assert!(
        scene
            .service
            .ready_agent_provider_turn(
                &AgentId::opaque(turn.agent_id.clone()).unwrap(),
                &turn.turn_id,
            )
            .unwrap()
            .is_some(),
        "provider admission held: bookkeeping={} pending={} turn={:?}",
        scene
            .service
            .persistence
            .bookkeeping_pending(&turn.conversation_id),
        scene.service.agent_provider_task_is_pending(&turn.turn_id),
        scene.service.agent_turn_ledger().turn(&turn.turn_id),
    );
    member
}

fn ready_child(scene: &mut Scene, pane: &str) {
    // Shell readiness alone does not discharge the native startup owner. Drive
    // the same authenticated adapter/bootstrap events as the managed host;
    // only the returned environment bytes are fixture data.
    let token = scene
        .service
        .posix_startup_token_for_tests(pane)
        .unwrap()
        .to_owned();
    assert_eq!(
        scene
            .service
            .observe_managed_shell_protocol_event(
                pane,
                mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                mez_terminal::ManagedShellAdapter::Posix,
                &token,
                &mez_terminal::ManagedShellProtocolEvent::AdapterAvailable { trigger: None },
            )
            .unwrap(),
        1
    );
    let (marker, turn) = scene
        .service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.pane_id == pane
                && transaction.kind == RunningShellTransactionKind::Bootstrap)
                .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .unwrap();
    let agent = format!("agent-{pane}");
    scene
        .service
        .observe_agent_shell_transaction_start(pane, &marker, &turn, &agent, pane)
        .unwrap();
    let output = format!(
        "env\tos\tLinux\nenv\tarch\tx86_64\nenv\thost\ttest-host\nenv\tuser\ttest-user\nenv\tshell_path\t/bin/sh\nenv\tshell_class\tposix-sh\nenv\tpath\t/usr/bin:/bin\nenv\tcwd\t{}\nenv\tgit_repo\t0\nbootstrap\tcomplete\t1714500000\n",
        scene._root.display()
    );
    let transaction = scene
        .service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&marker)
        .unwrap();
    transaction.observed_output_bytes = output.len();
    transaction.observed_output_preview = output;
    scene
        .service
        .observe_agent_shell_transaction_end(pane, &marker, &turn, &agent, pane, 0)
        .unwrap();
    assert_eq!(
        scene
            .service
            .runtime_agent_surface_startup_phase_for_tests(pane),
        Some("ready")
    );
}

/// Both context modes must inherit before the initial child turn can claim a
/// provider. Actual delegated cleanup may remove the pane, never its incurred
/// expense or an issued request's original lifetime custody.
#[tokio::test]
async fn runtime_budget_descendants_task_new_and_fork_keep_late_closed_child_expense() {
    for mode in [
        mez_agent::SubagentSessionMode::New,
        mez_agent::SubagentSessionMode::Fork,
    ] {
        let mut scene = Scene::new();
        let primary = scene
            .service
            .attach_primary("primary", true, Size::new(120, 30).unwrap(), 120)
            .unwrap();
        scene
            .service
            .configure_subagent_policy(8, 8, 8, 8, SubagentWaitPolicy::Detach);
        let root = scene.start();
        let original = prepare(&mut scene, &root).await;
        scene.service.stop_agent_turn_for_pane("%1").unwrap();
        let response = scene
            .service
            .spawn_runtime_subagent_session_owned(
                SubagentSpawnRequest {
                    parent_agent_id: root.agent_id.clone(),
                    requested_role: "worker".into(),
                    placement: "new-pane".into(),
                    cooperation_mode: CooperationMode::ExploreOnly,
                    cooperation_mode_defaulted: false,
                    read_scopes: Vec::new(),
                    read_scopes_defaulted: false,
                    write_scopes: Vec::new(),
                    write_scopes_defaulted: false,
                    session_mode: mode,
                    initial_model_size: None,
                    initial_reasoning_effort: None,
                    task_prompt: "qualify original child expense".into(),
                    explicit_user_approval: false,
                    skip_initial_turn: false,
                },
                RuntimeSubagentPlacement::NewPane {
                    direction: SplitDirection::Vertical,
                    select: false,
                },
            )
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        let child_id = response["turn"]["id"].as_str().unwrap();
        let child = scene
            .service
            .agent_turn_ledger()
            .turn(child_id)
            .unwrap()
            .clone();
        assert_ne!(child.conversation_id, root.conversation_id);
        assert!(
            scene
                .store
                .budget_membership(&child.conversation_id)
                .unwrap()
                .is_none()
        );
        ready_child(&mut scene, &child.pane_id);
        let member = prepare(&mut scene, &child).await;
        assert_eq!(member.root, original.root);
        assert_eq!(member.namespace, original.namespace);
        assert_eq!(
            member.parent.as_deref(),
            Some(original.conversation.as_str())
        );
        let mut dispatch = scene
            .service
            .claim_configured_agent_provider_task(
                &AgentId::opaque(child.agent_id.clone()).unwrap(),
                &child.turn_id,
            )
            .unwrap()
            .unwrap();
        let owner = dispatch.budget_owner.take().unwrap();
        let attempt = owner
            .prepare_attempt(
                mez_agent::ModelTokenUsageKey::new("offline", "child"),
                BudgetAttemptPurpose::Execution,
            )
            .unwrap();
        let registered = tokio::task::spawn_blocking(move || attempt.execute())
            .await
            .unwrap()
            .unwrap();
        drop((owner, dispatch));
        assert_eq!(
            scene
                .service
                .close_subagent_descendants_for_parent_conversation(
                    &root.agent_id,
                    &root.conversation_id,
                    "budget lifecycle fixture",
                )
                .unwrap(),
            1
        );
        assert!(scene.service.find_pane_descriptor(&child.pane_id).is_none());
        assert!(
            scene
                .service
                .agent_shell_store()
                .get(&child.pane_id)
                .is_none()
        );
        assert_eq!(
            scene
                .store
                .budget_membership(&child.conversation_id)
                .unwrap()
                .unwrap()
                .0,
            member
        );
        // A fresh human conversation cannot absorb the already-issued child's
        // expense. Freeze and commit only after actual child teardown/rebind.
        scene
            .service
            .execute_agent_shell_command(&primary, "/new")
            .unwrap();
        let next = scene.start();
        let independent = prepare(&mut scene, &next).await;
        assert_ne!(independent.root, original.root);
        let receipt = registered
            .freeze_receipt(
                UsageSnapshot {
                    input: Some(11),
                    output: Some(3),
                    terminal: true,
                },
                None,
            )
            .unwrap();
        let retry = receipt.clone();
        let outcome = tokio::task::spawn_blocking(move || receipt.execute())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            tokio::task::spawn_blocking(move || retry.execute())
                .await
                .unwrap()
                .unwrap(),
            outcome
        );
        assert_eq!(
            scene.store.budget_account(&original.root).unwrap().known,
            UsageTotals {
                input: 11,
                output: 3
            }
        );
        assert_eq!(
            scene.store.budget_account(&independent.root).unwrap().known,
            UsageTotals::default()
        );
    }
}

/// Routed depth-zero is a scheduler/delegation label, never permission to mint
/// an independent budget. The real controller's ephemeral CID inherits twice:
/// initial native allocation then rebind, before provider preparation/claim.
#[tokio::test]
async fn runtime_budget_descendants_routed_ephemeral_depth_zero_keeps_root() {
    let mut scene = Scene::new();
    scene
        .service
        .attach_primary("primary", true, Size::new(120, 30).unwrap(), 120)
        .unwrap();
    scene
        .service
        .configure_subagent_policy(8, 8, 8, 8, SubagentWaitPolicy::Detach);
    let root = scene.start();
    let original = prepare(&mut scene, &root).await;
    let profile = scene
        .service
        .agent_turn_model_profile(&root.turn_id)
        .unwrap()
        .clone();
    scene
        .service
        .apply_routing_selected_transition(
            &AgentId::opaque(root.agent_id.clone()).unwrap(),
            &root.turn_id,
            mez_agent::AutoSizingRoutingSelection {
                selected_profile: profile,
                selected_profile_name: "budget-test".into(),
                routing_token_usage_by_model: Default::default(),
                decision_summary: None,
                fallback: None,
            },
        )
        .unwrap();
    let child_id = scene
        .service
        .routed_workflow_for_tests(&root.turn_id)
        .unwrap()
        .child_turn_id
        .clone()
        .unwrap();
    let child = scene
        .service
        .agent_turn_ledger()
        .turn(&child_id)
        .unwrap()
        .clone();
    assert_eq!(
        scene
            .service
            .subagent_lineage(&child.agent_id)
            .unwrap()
            .depth,
        0
    );
    assert!(
        scene
            .service
            .agent_shell_store()
            .get(&child.pane_id)
            .unwrap()
            .ephemeral
    );
    ready_child(&mut scene, &child.pane_id);
    let member = prepare(&mut scene, &child).await;
    assert_eq!(member.root, original.root);
    assert_eq!(member.namespace, original.namespace);
    assert_eq!(
        member.parent.as_deref(),
        Some(original.conversation.as_str())
    );
    let dispatch = scene
        .service
        .claim_configured_agent_provider_task(
            &AgentId::opaque(child.agent_id.clone()).unwrap(),
            &child.turn_id,
        )
        .unwrap()
        .unwrap();
    assert_eq!(dispatch.budget_owner.as_ref().unwrap().member, member);
    assert_eq!(
        scene
            .store
            .budget_membership(&root.conversation_id)
            .unwrap()
            .unwrap()
            .0,
        original
    );
}
