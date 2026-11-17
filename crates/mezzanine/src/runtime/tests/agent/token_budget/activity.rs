//! Real activated memberships and native reads project one root's inhibition.
//! Physical provider-to-native claim coverage lives in budget_receipts.

use super::*;
use crate::storage::token_usage::budget::operations::BudgetPolicyOperation;
use crate::storage::token_usage::budget::policy_projection::{
    BudgetConfigPolicy, BudgetPolicyPublication,
};
use mez_agent::token_budget::{BudgetLimits, BudgetOverride, BudgetReceiptId, TokenLimit};

async fn prepare(scene: &mut Scene, turn: &AgentTurnRecord) {
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
}

async fn policy(scene: &Scene, limits: BudgetLimits) {
    let store = scene.store.clone();
    tokio::task::spawn_blocking(move || {
        let projection = store.budget_policy_projection().unwrap();
        let op = BudgetPolicyPublication {
            namespace: projection.namespace,
            id: BudgetReceiptId::new(crate::storage::token_usage::new_token_usage_event_id())
                .unwrap(),
            expected_revision: projection.revision,
            previous_digest: projection
                .committed
                .as_ref()
                .map_or_else(|| "a".repeat(64), |p| p.digest.clone()),
            proposed: BudgetConfigPolicy {
                source: "primary".into(),
                digest: format!("{:064x}", projection.revision + 1),
                limits,
            },
            supersedes: None,
        };
        store.begin_budget_policy_publication(&op).unwrap();
        store
            .reconcile_budget_policy_publication(&op, &op.proposed.digest)
            .unwrap();
    })
    .await
    .unwrap();
}

async fn observe(scene: &mut Scene, turn: &AgentTurnRecord) {
    let work = scene
        .service
        .prepare_budget_activity_read(&turn.turn_id)
        .unwrap()
        .unwrap();
    let read = tokio::task::spawn_blocking(move || work.execute())
        .await
        .unwrap();
    assert!(scene.service.apply_budget_activity_read(*read));
}

/// Actual delegated construction, not copied IDs, shares root inhibition.
/// Independent root B keeps its ordinary provider preparation/claim available.
#[tokio::test]
async fn runtime_budget_activity_same_root_descendant_and_independent_claims() {
    let mut scene = Scene::new();
    let primary = scene
        .service
        .attach_primary("primary", true, Size::new(120, 30).unwrap(), 120)
        .unwrap();
    scene
        .service
        .configure_subagent_policy(8, 8, 8, 8, SubagentWaitPolicy::Detach);
    let parent = scene.start();
    prepare(&mut scene, &parent).await;
    policy(&scene, BudgetLimits::default()).await;
    let member = scene
        .store
        .budget_membership(&parent.conversation_id)
        .unwrap()
        .unwrap()
        .0;
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    let response = scene
        .service
        .spawn_runtime_subagent_session_owned(
            SubagentSpawnRequest {
                parent_agent_id: parent.agent_id.clone(),
                requested_role: "worker".into(),
                placement: "new-pane".into(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: mez_agent::SubagentSessionMode::New,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "qualify root activity inhibition".into(),
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
    let child = scene
        .service
        .agent_turn_ledger()
        .turn(response["turn"]["id"].as_str().unwrap())
        .unwrap()
        .clone();
    super::descendants::ready_child(&mut scene, &child.pane_id);
    prepare(&mut scene, &child).await;
    assert_eq!(
        scene
            .store
            .budget_membership(&child.conversation_id)
            .unwrap()
            .unwrap()
            .0
            .root,
        member.root
    );

    let pane = scene
        .service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    scene
        .service
        .agent_shell_store_mut()
        .enter_or_resume(pane.as_str())
        .unwrap();
    scene.service.set_pane_environment_signature_for_tests(
        pane.as_str(),
        test_environment_signature(scene._root.as_ref()),
    );
    mark_test_pane_ready(&mut scene.service, pane.as_str());
    let started = scene
        .service
        .start_agent_prompt_turn(pane.as_str(), "independent root control")
        .unwrap();
    let independent = scene
        .service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    prepare(&mut scene, &independent).await;
    assert_ne!(
        scene
            .store
            .budget_membership(&independent.conversation_id)
            .unwrap()
            .unwrap()
            .0
            .root,
        member.root
    );
    let change = BudgetPolicyOperation {
        id: "activity-zero".into(),
        namespace: member.namespace,
        root: member.root.clone(),
        input: Some(BudgetOverride::Explicit(
            mez_agent::token_budget::TokenLimit::Limited(0),
        )),
        output: Some(BudgetOverride::Explicit(
            mez_agent::token_budget::TokenLimit::Limited(0),
        )),
    };
    let store = scene.store.clone();
    tokio::task::spawn_blocking(move || store.commit_budget_policy(&change))
        .await
        .unwrap()
        .unwrap();
    observe(&mut scene, &parent).await;
    assert!(scene.service.budget_activity_inhibits_turn(&child.turn_id));
    assert!(
        scene
            .service
            .ready_agent_provider_turn(
                &AgentId::opaque(child.agent_id.clone()).unwrap(),
                &child.turn_id
            )
            .unwrap()
            .is_none()
    );
    // Detached projection metadata must not become a lifetime-root cap. The
    // parent/child inhibition remains live while independent B is captured.
    drop(scene.service.fill_budget_activity_metadata_for_tests(false));
    observe(&mut scene, &independent).await;
    assert_eq!(scene.service.budget_activity_metadata_count_for_tests(), 2);
    assert!(scene.service.budget_activity_inhibits_turn(&child.turn_id));
    let independent_dispatch = scene
        .service
        .claim_configured_agent_provider_task(
            &AgentId::opaque(independent.agent_id.clone()).unwrap(),
            &independent.turn_id,
        )
        .unwrap()
        .unwrap();
    assert_ne!(
        independent_dispatch
            .budget_owner
            .as_ref()
            .unwrap()
            .member
            .root,
        member.root
    );
    drop(independent_dispatch);
    scene
        .service
        .stop_agent_turn_for_pane(pane.as_str())
        .unwrap();
    scene
        .service
        .stop_agent_turn_for_pane(&child.pane_id)
        .unwrap();
}

/// A lost observer cannot replace a live read. Missing policy and a real SQL
/// read failure remain operational holds, not an invented successful zero read.
#[tokio::test]
async fn runtime_budget_activity_coalesces_cancelled_reads_and_recovers_original_store() {
    let mut scene = Scene::new();
    let turn = scene.start();
    prepare(&mut scene, &turn).await;
    let work = scene
        .service
        .prepare_budget_activity_read(&turn.turn_id)
        .unwrap()
        .unwrap();
    assert!(
        scene
            .service
            .prepare_budget_activity_read(&turn.turn_id)
            .unwrap()
            .is_none()
    );
    drop(work);
    assert!(scene.service.budget_activity_inhibits_turn(&turn.turn_id));
    observe(&mut scene, &turn).await;
    assert!(
        scene.service.budget_activity_inhibits_turn(&turn.turn_id),
        "uninitialized is not unlimited"
    );
    policy(&scene, BudgetLimits::default()).await;
    observe(&mut scene, &turn).await;
    assert!(!scene.service.budget_activity_inhibits_turn(&turn.turn_id));
    let connection = rusqlite::Connection::open(scene.store.path()).unwrap();
    let original: String = connection
        .query_row(
            "SELECT body FROM budget_policy_projection WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute(
            "UPDATE budget_policy_projection SET body='{}' WHERE singleton=1",
            [],
        )
        .unwrap();
    observe(&mut scene, &turn).await;
    assert!(scene.service.budget_activity_inhibits_turn(&turn.turn_id));
    assert!(
        scene
            .service
            .ready_agent_provider_turn(
                &AgentId::opaque(turn.agent_id.clone()).unwrap(),
                &turn.turn_id
            )
            .unwrap()
            .is_none()
    );
    connection
        .execute(
            "UPDATE budget_policy_projection SET body=?1 WHERE singleton=1",
            [&original],
        )
        .unwrap();
    drop(connection);
    observe(&mut scene, &turn).await;
    assert!(!scene.service.budget_activity_inhibits_turn(&turn.turn_id));
    assert_eq!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .unwrap()
            .1,
        BudgetMembershipState::Active
    );
}

/// A fully read old account is still not authority to project into a new human
/// conversation. The genuine departure invalidates the captured allocation.
#[tokio::test]
async fn runtime_budget_activity_rebind_refuses_original_read_without_new_root_wake() {
    let mut scene = Scene::new();
    let primary = scene
        .service
        .attach_primary("primary", true, Size::new(120, 30).unwrap(), 120)
        .unwrap();
    let turn = scene.start();
    prepare(&mut scene, &turn).await;
    policy(&scene, BudgetLimits::default()).await;
    let work = scene
        .service
        .prepare_budget_activity_read(&turn.turn_id)
        .unwrap()
        .unwrap();
    let read = tokio::task::spawn_blocking(move || work.execute())
        .await
        .unwrap();
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    scene
        .service
        .execute_agent_shell_command(&primary, "/new")
        .unwrap();
    assert_ne!(
        scene
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id,
        turn.conversation_id
    );
    assert!(!scene.service.apply_budget_activity_read(*read));
    let replacement = scene.start();
    assert!(
        !scene
            .service
            .budget_activity_inhibits_turn(&replacement.turn_id)
    );
}

/// Reclamation cannot drop a live original query merely because actual human
/// departure and membership pressure removed its old actor capture. This holds
/// unexecuted Work, not a claim of physical-reader retirement or effect custody.
#[tokio::test]
async fn runtime_budget_activity_pressure_retains_original_query_until_drop() {
    let mut scene = Scene::new();
    let primary = scene
        .service
        .attach_primary("primary", true, Size::new(120, 30).unwrap(), 120)
        .unwrap();
    let original = scene.start();
    prepare(&mut scene, &original).await;
    policy(&scene, BudgetLimits::default()).await;
    let work = scene
        .service
        .prepare_budget_activity_read(&original.turn_id)
        .unwrap()
        .unwrap();
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    scene
        .service
        .fill_runtime_budget_captures_for_tests(&original.conversation_id, false);
    scene
        .service
        .execute_agent_shell_command(&primary, "/new")
        .unwrap();
    let next = scene.start();
    prepare(&mut scene, &next).await;
    assert_eq!(scene.service.runtime_budget_capture_count_for_tests(), 1);
    drop(scene.service.fill_budget_activity_metadata_for_tests(false));
    observe(&mut scene, &next).await;
    assert_eq!(scene.service.budget_activity_metadata_count_for_tests(), 2);
    // The old result cannot adopt through the removed original capture.
    let read = tokio::task::spawn_blocking(move || work.execute())
        .await
        .unwrap();
    assert!(!scene.service.apply_budget_activity_read(*read));
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    scene
        .service
        .execute_agent_shell_command(&primary, "/new")
        .unwrap();
    let third = scene.start();
    prepare(&mut scene, &third).await;
    drop(scene.service.fill_budget_activity_metadata_for_tests(false));
    observe(&mut scene, &third).await;
    // The released original is gone; the two retained membership roots remain.
    assert_eq!(scene.service.budget_activity_metadata_count_for_tests(), 2);
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
}

/// Synthetic live-query tokens exercise the fixed metadata bound, not physical
/// reader retirement. The actual current claim owner must refuse locally, then
/// recover only observation capacity; an exhausted numeric policy still holds.
#[tokio::test]
async fn runtime_budget_activity_pressure_refuses_live_queries_then_recovers() {
    let mut scene = Scene::new();
    let turn = scene.start();
    prepare(&mut scene, &turn).await;
    policy(
        &scene,
        BudgetLimits {
            input: TokenLimit::limited(0).unwrap(),
            output: TokenLimit::Unlimited,
        },
    )
    .await;
    let tokens = scene.service.fill_budget_activity_metadata_for_tests(true);
    assert_eq!(
        scene.service.budget_activity_metadata_count_for_tests(),
        4096
    );
    assert!(matches!(
        scene.service.prepare_budget_activity_claim(&turn.turn_id),
        crate::runtime::RuntimeBudgetActivityPreparation::Refused
    ));
    assert!(scene.service.budget_activity_inhibits_turn(&turn.turn_id));
    assert_eq!(
        scene.service.budget_activity_metadata_count_for_tests(),
        4096
    );
    drop(tokens);
    observe(&mut scene, &turn).await;
    assert_eq!(scene.service.budget_activity_metadata_count_for_tests(), 1);
    assert!(scene.service.budget_activity_inhibits_turn(&turn.turn_id));
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
}
