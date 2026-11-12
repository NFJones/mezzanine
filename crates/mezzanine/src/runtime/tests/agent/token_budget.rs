//! Real runtime preparation -> blocking publication -> actor adoption. No
//! provider HTTP, private configuration, native process or model call is used.

use super::*;
use crate::storage::token_usage::TokenUsageStore;
use crate::storage::token_usage::budget::membership::BudgetMembershipState;

mod auxiliary;
mod checkpoint;
mod reclamation;
mod resolution;
mod transitions;

struct Scene {
    service: RuntimeSessionService,
    store: TokenUsageStore,
    transcripts: AgentTranscriptStore,
    _root: OwnedRoot,
}

struct OwnedRoot(std::path::PathBuf);
impl OwnedRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mez-budget-runtime-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        fs::create_dir(&path).unwrap();
        let root = Self(path);
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }
}
impl std::ops::Deref for OwnedRoot {
    type Target = std::path::PathBuf;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl AsRef<std::path::Path> for OwnedRoot {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for OwnedRoot {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
        assert!(!self.0.exists());
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        // This generated service owns every process it can retire. No caller
        // supplied process identity or another live runtime participates.
        self.service.terminate_all_pane_processes().unwrap();
    }
}

impl Scene {
    fn new() -> Self {
        let root = OwnedRoot::new();
        let mut service = test_runtime_service();
        service.replace_config_layers(vec![ConfigLayer {
            name: "budget-offline".into(), path: None, format: ConfigFormat::Toml,
            scope: ConfigScope::Primary, trusted: true,
            text: "[agents]\nshell_mode = \"pane\"\ndefault_provider = \"openai\"\ndefault_model_profile = \"budget-test\"\n[permissions]\nsandbox = \"policy-only\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-test\"]\n[model_profiles.budget-test]\nprovider = \"openai\"\nmodel = \"gpt-test\"\n".into(),
        }]).unwrap();
        let auth = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(
            &root.join("auth"),
        ));
        let credentials = auth.file_credential_store("openai").unwrap();
        auth.login_openai_api_key("budget-test", "sk-budget-offline-fixture", &credentials)
            .unwrap();
        service.set_auth_store(auth);
        let store = TokenUsageStore::new(root.join("usage.sqlite"));
        store
            .initialize(crate::runtime::current_unix_seconds())
            .unwrap();
        let transcripts = AgentTranscriptStore::new(root.join("transcripts"));
        service.set_token_usage_store(store.clone());
        service.set_agent_transcript_store(transcripts.clone());
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let signature = mez_agent::EnvironmentSignature::new(
            "linux",
            "x86_64",
            None,
            "test-host",
            "test-user",
            None,
            "/bin/sh",
            mez_agent::ShellClassification::PosixSh,
            None,
            Some("/usr/bin:/bin".into()),
            root.to_string_lossy(),
            None,
            false,
            None,
            Vec::new(),
        )
        .unwrap();
        service.set_pane_environment_signature_for_tests("%1", signature);
        mark_test_pane_ready(&mut service, "%1");
        Self {
            service,
            store,
            transcripts,
            _root: root,
        }
    }

    fn start(&mut self) -> AgentTurnRecord {
        self.service.enable_runtime_budget_membership_for_tests();
        let started = self
            .service
            .start_agent_prompt_turn("%1", "qualify budget ownership")
            .unwrap();
        self.service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == started.turn_id)
            .unwrap()
            .clone()
    }
}

/// Production assembly stays inactive while lifecycle wiring is unfinished;
/// attached repositories alone must not alter existing prompt/claim behavior.
#[tokio::test]
async fn runtime_budget_preparation_default_does_not_activate_partial_feature() {
    let mut scene = Scene::new();
    let started = scene
        .service
        .start_agent_prompt_turn("%1", "ordinary disabled assembly")
        .unwrap();
    let work = scene
        .service
        .prepare_agent_provider_work(&started.turn_id)
        .unwrap();
    assert!(work.budget_membership.is_none());
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    let turn = scene
        .service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap();
    assert!(scene.service.runtime_budget_membership_is_ready(turn));
    assert!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .is_none()
    );
}

/// A generic rebind labeled Root and submitted through UserPrompt still cannot
/// create an independent account without constructor birth or explicit ancestry.
#[tokio::test]
async fn runtime_budget_preparation_generic_rebind_cannot_mint_fresh_account() {
    let mut scene = Scene::new();
    scene
        .service
        .agent_shell_store_mut()
        .bind_conversation("%1", "unqualified-existing-root", 0)
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
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .is_none()
    );
}

/// Rebinding the same durable conversation rotates only live custody. An actual
/// claimed dispatch retains its original lock after Stop/rebind, and a new
/// preparation cannot adopt or run until that last original holder retires.
#[tokio::test]
async fn runtime_budget_preparation_rebind_waits_for_original_dispatch_retirement() {
    let mut scene = Scene::new();
    let turn = scene.start();
    let work = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    let original = scene
        .store
        .budget_membership(&turn.conversation_id)
        .unwrap()
        .unwrap()
        .0;
    let dispatch = scene
        .service
        .claim_configured_agent_provider_task(
            &AgentId::opaque(turn.agent_id.clone()).unwrap(),
            &turn.turn_id,
        )
        .unwrap()
        .unwrap();
    assert!(dispatch.budget_binding_lease.is_some());
    let old_native = scene
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .native_message_incarnation
        .clone();
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    scene
        .service
        .agent_shell_store_mut()
        .bind_conversation("%1", &turn.conversation_id, 0)
        .unwrap();
    assert_ne!(
        scene
            .service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .native_message_incarnation,
        old_native
    );
    let replacement = scene.start();
    let work = scene
        .service
        .prepare_agent_provider_work(&replacement.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    assert!(
        scene
            .service
            .apply_agent_provider_preparation(outcome)
            .is_err()
    );
    assert!(
        !scene
            .service
            .runtime_budget_membership_is_ready(&replacement)
    );
    drop(dispatch);
    let work = scene
        .service
        .prepare_agent_provider_work(&replacement.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    assert!(
        scene
            .service
            .runtime_budget_membership_is_ready(&replacement)
    );
    assert_eq!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .unwrap()
            .0,
        original
    );
}

/// A lost initialized store is not recreated during the existing preparation
/// lane, and the affected turn cannot borrow readiness from another account.
#[tokio::test]
async fn runtime_budget_preparation_missing_store_refuses_without_reset() {
    let mut scene = Scene::new();
    let turn = scene.start();
    let absent = scene._root.join("absent.sqlite");
    scene
        .service
        .set_token_usage_store(TokenUsageStore::new(&absent));
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
    assert!(!absent.exists());
}

/// Real configured preparation publishes no account on actor capture; only the
/// supervised worker can publish, and only exact adoption enables the claim.
#[tokio::test]
async fn runtime_budget_preparation_publishes_original_membership_before_claim() {
    let mut scene = Scene::new();
    let turn = scene.start();
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    let work = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    assert!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .is_none()
    );
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    let (member, phase) = scene
        .store
        .budget_membership(&turn.conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(phase, BudgetMembershipState::Active);
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    assert!(scene.service.runtime_budget_membership_is_ready(&turn));
    assert_eq!(
        scene
            .transcripts
            .conversation_budget_membership(&turn.conversation_id)
            .unwrap(),
        Some(member.clone())
    );
    let retry = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(retry).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    assert_eq!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .unwrap()
            .0,
        member
    );
    assert!(
        scene
            .service
            .claim_configured_agent_provider_task(
                &AgentId::opaque(turn.agent_id.clone()).unwrap(),
                &turn.turn_id
            )
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn runtime_budget_preparation_failure_retains_exact_pending_operation_for_recovery() {
    let mut scene = Scene::new();
    let turn = scene.start();
    scene.transcripts.fail_next_metadata_write_after_promotion();
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
    let (original, phase) = scene
        .store
        .budget_membership(&turn.conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(phase, BudgetMembershipState::Pending);
    let work = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    assert!(scene.service.runtime_budget_membership_is_ready(&turn));
    assert_eq!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .unwrap()
            .0,
        original
    );
}

#[tokio::test]
async fn runtime_budget_preparation_late_success_cannot_activate_replacement_conversation() {
    let mut scene = Scene::new();
    let turn = scene.start();
    let work = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    scene
        .service
        .agent_shell_store_mut()
        .bind_conversation("%1", "budget-replacement", 0)
        .unwrap();
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    assert!(
        scene
            .store
            .budget_membership("budget-replacement")
            .unwrap()
            .is_none()
    );
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

/// A completed older preparation cannot authorize a newer request. Failed
/// revalidation must revoke readiness without forgetting original ownership.
#[tokio::test]
async fn runtime_budget_preparation_supersession_and_failure_revoke_readiness() {
    let mut scene = Scene::new();
    let turn = scene.start();
    let older = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    let older = RuntimeSessionService::execute_agent_provider_preparation(older).await;
    let current = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    scene
        .service
        .apply_agent_provider_preparation(older)
        .unwrap();
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    let current = RuntimeSessionService::execute_agent_provider_preparation(current).await;
    scene
        .service
        .apply_agent_provider_preparation(current)
        .unwrap();
    assert!(scene.service.runtime_budget_membership_is_ready(&turn));
    let original = scene
        .store
        .budget_membership(&turn.conversation_id)
        .unwrap()
        .unwrap()
        .0;
    let current = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    // Refuse a missing original publication; never manufacture a replacement
    // account or acknowledge an earlier successful preparation instead.
    fs::remove_file(
        scene
            .transcripts
            .transcript_path(&turn.conversation_id)
            .unwrap()
            .parent()
            .unwrap()
            .join("budget-binding.json"),
    )
    .unwrap();
    let current = RuntimeSessionService::execute_agent_provider_preparation(current).await;
    assert!(
        scene
            .service
            .apply_agent_provider_preparation(current)
            .is_err()
    );
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
}

/// A different, otherwise valid publication racing a captured creation cannot
/// replace its exact original root/operation. Neither source is overwritten.
#[tokio::test]
async fn runtime_budget_preparation_competing_publication_cannot_replace_proposal() {
    let mut scene = Scene::new();
    let turn = scene.start();
    let work = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    let competing = crate::storage::token_usage::budget::membership::BudgetMembership {
        namespace: scene.store.budget_namespace().unwrap(),
        root: mez_agent::token_budget::BudgetRootId::new("competing-root").unwrap(),
        conversation: turn.conversation_id.clone(),
        parent: None,
        operation: "competing-operation".into(),
        started_at: turn.started_at_unix_seconds,
        legacy_coverage_gap: false,
    };
    scene.store.prepare_budget_membership(&competing).unwrap();
    scene
        .transcripts
        .publish_conversation_budget_membership(&competing)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    assert!(
        scene
            .service
            .apply_agent_provider_preparation(outcome)
            .is_err()
    );
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    assert_eq!(
        scene
            .store
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .unwrap(),
        (competing, BudgetMembershipState::Pending)
    );
}

/// Failed A->B revalidation must invalidate an earlier success and an already
/// completed A worker. Restoring A alone never resurrects either receipt.
#[tokio::test]
async fn runtime_budget_preparation_repository_round_trip_requires_new_validation() {
    let mut scene = Scene::new();
    let turn = scene.start();
    let work = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    assert!(scene.service.runtime_budget_membership_is_ready(&turn));
    let replacement = TokenUsageStore::new(scene._root.join("replacement-roundtrip.sqlite"));
    replacement.initialize(1).unwrap();
    scene.service.set_token_usage_store(replacement.clone());
    assert!(
        scene
            .service
            .prepare_agent_provider_work(&turn.turn_id)
            .is_err()
    );
    scene.service.set_token_usage_store(scene.store.clone());
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    let old = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    let old = RuntimeSessionService::execute_agent_provider_preparation(old).await;
    scene.service.set_token_usage_store(replacement);
    assert!(
        scene
            .service
            .prepare_agent_provider_work(&turn.turn_id)
            .is_err()
    );
    scene.service.set_token_usage_store(scene.store.clone());
    scene.service.apply_agent_provider_preparation(old).unwrap();
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    let current = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    let current = RuntimeSessionService::execute_agent_provider_preparation(current).await;
    scene
        .service
        .apply_agent_provider_preparation(current)
        .unwrap();
    assert!(scene.service.runtime_budget_membership_is_ready(&turn));
}

/// Repository replacement cannot borrow an earlier worker's successful
/// publication, even when the live pane and conversation are unchanged.
#[tokio::test]
async fn runtime_budget_preparation_repository_replacement_rejects_old_adoption() {
    let mut scene = Scene::new();
    let turn = scene.start();
    let work = scene
        .service
        .prepare_agent_provider_work(&turn.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    let replacement = TokenUsageStore::new(scene._root.join("replacement.sqlite"));
    replacement
        .initialize(crate::runtime::current_unix_seconds())
        .unwrap();
    scene.service.set_token_usage_store(replacement.clone());
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
    assert!(
        scene
            .service
            .prepare_agent_provider_work(&turn.turn_id)
            .is_err()
    );
    assert!(
        replacement
            .budget_membership(&turn.conversation_id)
            .unwrap()
            .is_none()
    );
}

/// The actual /loop --new command changes context, not accounting ownership.
/// Its fresh ephemeral conversation must publish under the frozen original root.
#[tokio::test]
async fn runtime_budget_preparation_real_loop_new_retains_original_account() {
    let mut scene = Scene::new();
    let primary = scene
        .service
        .attach_primary("primary", true, Size::new(120, 30).unwrap(), 120)
        .unwrap();
    let root = scene.start();
    let work = scene
        .service
        .prepare_agent_provider_work(&root.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    let original = scene
        .store
        .budget_membership(&root.conversation_id)
        .unwrap()
        .unwrap()
        .0;
    scene.service.stop_agent_turn_for_pane("%1").unwrap();
    scene
        .service
        .fill_runtime_budget_captures_for_tests(&root.conversation_id, false);
    scene
        .service
        .execute_agent_shell_command(
            &primary,
            "/loop --new --limit 1 --goal 'qualify ownership' generate one offline result",
        )
        .unwrap();
    let child = scene.service.agent_shell_store().get("%1").unwrap().clone();
    assert!(child.ephemeral);
    assert_ne!(child.session_id, root.conversation_id);
    let task = scene
        .service
        .pending_agent_provider_tasks()
        .into_iter()
        .find(|task| {
            scene
                .service
                .agent_turn_ledger()
                .turn(&task.turn_id)
                .is_some_and(|turn| turn.conversation_id == child.session_id)
        })
        .unwrap();
    let work = scene
        .service
        .prepare_agent_provider_work(&task.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    let (member, phase) = scene
        .store
        .budget_membership(&child.session_id)
        .unwrap()
        .unwrap();
    assert_eq!(phase, BudgetMembershipState::Active);
    assert_eq!(member.root, original.root);
    assert_eq!(
        member.parent.as_deref(),
        Some(original.conversation.as_str())
    );
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

/// Idle persistent descendants retain the creation owner even when their later
/// user-labelled prompt has no parent turn. Publication remains worker-owned.
#[tokio::test]
async fn runtime_budget_preparation_real_persistent_spawn_inherits_before_later_turn() {
    fn spawn(scene: &mut Scene, parent_agent: &str) -> String {
        let parent = scene
            .service
            .agent_shell_store()
            .get(parent_agent.strip_prefix("agent-").unwrap())
            .unwrap()
            .session_id
            .clone();
        let response = scene
            .service
            .spawn_runtime_persistent_subagent_session_owned(
                SubagentSpawnRequest {
                    parent_agent_id: parent_agent.into(),
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
                    task_prompt: String::new(),
                    explicit_user_approval: false,
                    skip_initial_turn: true,
                },
                RuntimeSubagentPlacement::NewPane {
                    direction: SplitDirection::Vertical,
                    select: false,
                },
                &parent,
                "budget inheritance fixture",
            )
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        value["agent"]["id"].as_str().unwrap().to_owned()
    }

    let retained_root;
    let mut scene = Scene::new();
    let _primary = scene
        .service
        .attach_primary("primary", true, Size::new(120, 30).unwrap(), 120)
        .unwrap();
    scene
        .service
        .configure_subagent_policy(8, 8, 8, 8, SubagentWaitPolicy::Detach);
    let root = scene.start();
    let work = scene
        .service
        .prepare_agent_provider_work(&root.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    let root_member = scene
        .store
        .budget_membership(&root.conversation_id)
        .unwrap()
        .unwrap()
        .0;

    let mut parent_agent = "agent-%1".to_owned();
    let mut parent_member = root_member.clone();
    for level in 0..3 {
        let child_agent = spawn(&mut scene, &parent_agent);
        let pane = child_agent.strip_prefix("agent-").unwrap();
        let child_conversation = scene
            .service
            .agent_shell_store()
            .get(pane)
            .unwrap()
            .session_id
            .clone();
        // Capture is actor-only; durable activation still waits for the existing
        // preparation worker, even for an idle persistent child.
        assert!(
            scene
                .store
                .budget_membership(&child_conversation)
                .unwrap()
                .is_none()
        );
        let signature = mez_agent::EnvironmentSignature::new(
            "linux",
            "x86_64",
            None,
            "test-host",
            "test-user",
            None,
            "/bin/sh",
            mez_agent::ShellClassification::PosixSh,
            None,
            Some("/usr/bin:/bin".into()),
            scene._root.to_string_lossy(),
            None,
            false,
            None,
            Vec::new(),
        )
        .unwrap();
        scene
            .service
            .set_pane_environment_signature_for_tests(pane, signature);
        mark_test_pane_ready(&mut scene.service, pane);
        let started = scene
            .service
            .start_agent_prompt_turn(pane, "later persistent work")
            .unwrap();
        let turn = scene
            .service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .clone();
        assert!(turn.parent_turn_id.is_none());
        let work = scene
            .service
            .prepare_agent_provider_work(&turn.turn_id)
            .unwrap();
        if level == 2 {
            let competing = crate::storage::token_usage::budget::membership::BudgetMembership {
                namespace: root_member.namespace.clone(),
                root: root_member.root.clone(),
                conversation: child_conversation.clone(),
                parent: Some(parent_member.conversation.clone()),
                operation: "different-inherited-operation".into(),
                started_at: 1,
                legacy_coverage_gap: parent_member.legacy_coverage_gap,
            };
            scene.store.prepare_budget_membership(&competing).unwrap();
            scene
                .transcripts
                .publish_conversation_budget_membership(&competing)
                .unwrap();
            let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
            assert!(
                scene
                    .service
                    .apply_agent_provider_preparation(outcome)
                    .is_err()
            );
            assert!(!scene.service.runtime_budget_membership_is_ready(&turn));
            assert_eq!(
                scene
                    .store
                    .budget_membership(&child_conversation)
                    .unwrap()
                    .unwrap(),
                (competing, BudgetMembershipState::Pending)
            );
        } else {
            let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
            scene
                .service
                .apply_agent_provider_preparation(outcome)
                .unwrap();
            let (member, phase) = scene
                .store
                .budget_membership(&child_conversation)
                .unwrap()
                .unwrap();
            assert_eq!(phase, BudgetMembershipState::Active);
            assert_eq!(member.root, root_member.root);
            assert_eq!(
                member.parent.as_deref(),
                Some(parent_member.conversation.as_str())
            );
            assert!(scene.service.runtime_budget_membership_is_ready(&turn));
            parent_member = member;
        }
        parent_agent = child_agent;
    }
    // The last successfully published nested child can resume independently
    // after every original native owner retires. The deliberately conflicting
    // third child's pending record is not silently repaired or adopted.
    let store = scene.store.clone();
    let transcripts = scene.transcripts.clone();
    retained_root = std::mem::replace(&mut scene._root, OwnedRoot::new());
    drop(scene);
    let mut resumed = Scene::new();
    resumed.service.set_token_usage_store(store.clone());
    resumed.service.set_agent_transcript_store(transcripts);
    resumed
        .service
        .execute_agent_shell_resume_command(
            "%1",
            &format!("/resume {}", parent_member.conversation),
        )
        .unwrap();
    let reference = resumed
        .service
        .runtime_budget_checkpoint_reference(&parent_member.conversation)
        .unwrap()
        .unwrap();
    assert_eq!(reference.root, root_member.root);
    assert_eq!(reference.operation.as_str(), parent_member.operation);
    assert_eq!(
        store
            .budget_membership(&parent_member.conversation)
            .unwrap()
            .unwrap()
            .0,
        parent_member
    );
    assert!(
        !resumed
            .service
            .subagent_lineage_has_live_parent_authority("agent-%1")
    );
    assert!(resumed.service.persistent_subagent("agent-%1").is_none());
    drop(resumed);
    drop(retained_root);
}
