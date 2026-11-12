//! Real auxiliary queue -> membership preparation -> claim boundaries. No
//! fabricated ordinary turn or provider request is needed to allocate custody.

use super::*;
use crate::runtime::RuntimeBudgetAuxiliarySource;

#[tokio::test]
async fn runtime_budget_auxiliary_replaced_repository_cannot_borrow_original_capture() {
    let (mut scene, primary, conversation) = setup();
    scene
        .service
        .execute_agent_shell_command(&primary, "/remember Keep original stores.")
        .unwrap();
    let observation = scene
        .service
        .pending_agent_remember_observation_id("%1")
        .unwrap()
        .to_owned();
    let source = RuntimeBudgetAuxiliarySource::Remember {
        pane: "%1".into(),
        observation: observation.clone(),
    };
    prepare(&mut scene, &source).await.unwrap();
    let replacement = TokenUsageStore::new(scene._root.join("replacement.sqlite"));
    replacement.initialize(1).unwrap();
    scene.service.set_token_usage_store(replacement.clone());
    assert!(
        scene
            .service
            .prepare_runtime_budget_auxiliary(&source)
            .is_err()
    );
    assert!(
        scene
            .service
            .claim_agent_remember_task("%1", &observation)
            .is_err()
    );
    assert!(
        replacement
            .budget_membership(&conversation)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        scene.service.pending_agent_remember_observation_id("%1"),
        Some(observation.as_str())
    );
    scene.service.set_token_usage_store(scene.store.clone());
    prepare(&mut scene, &source).await.unwrap();
    let dispatch = scene
        .service
        .claim_agent_remember_task("%1", &observation)
        .unwrap()
        .unwrap();
    assert_eq!(
        dispatch.budget_owner.as_ref().unwrap().member.conversation,
        conversation
    );
    drop(dispatch);
}

/// The public host handle must perform off-actor membership publication before
/// its actual claim. A real SQL refusal returns locally, leaving the actor and
/// same original pending observation usable for one later qualified claim.
#[tokio::test]
async fn runtime_budget_auxiliary_actor_handle_recovers_original_memory_after_storage_refusal() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let (mut scene, primary, conversation) = setup();
    scene
        .service
        .execute_agent_shell_command(&primary, "/remember Qualify actor membership admission.")
        .unwrap();
    let observation = scene
        .service
        .pending_agent_remember_observation_id("%1")
        .unwrap()
        .to_owned();
    let connection = rusqlite::Connection::open(scene.store.path()).unwrap();
    connection.execute_batch("CREATE TRIGGER refuse_actor_aux BEFORE INSERT ON budget_memberships BEGIN SELECT RAISE(ABORT,'fixture refusal'); END;").unwrap();
    let service = std::mem::replace(&mut scene.service, test_runtime_service());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let client = async {
        let refused = handle
            .claim_agent_remember_task("%1".into(), observation.clone())
            .await;
        let responsive = handle.metrics().await;
        connection
            .execute_batch("DROP TRIGGER refuse_actor_aux;")
            .unwrap();
        let recovered = handle
            .claim_agent_remember_task("%1".into(), observation.clone())
            .await;
        let duplicate = handle
            .claim_agent_remember_task("%1".into(), observation)
            .await;
        let shutdown = handle.shutdown().await;
        (refused, responsive, recovered, duplicate, shutdown)
    };
    let ((refused, responsive, recovered, duplicate, shutdown), exit) =
        tokio::join!(client, actor.run());
    scene.service = exit.service;
    assert!(refused.is_err());
    responsive.unwrap();
    shutdown.unwrap();
    let recovered = recovered.unwrap().unwrap();
    assert_eq!(
        recovered.budget_owner.as_ref().unwrap().member.conversation,
        conversation
    );
    assert!(duplicate.unwrap().is_none());
    assert!(scene.service.agent_turn_ledger().turns().is_empty());
    assert_eq!(
        scene
            .store
            .budget_membership(&conversation)
            .unwrap()
            .unwrap()
            .1,
        BudgetMembershipState::Active
    );
    drop(recovered);
    drop(connection);
}

/// A cold generated title uses the same production handle without allocating
/// an ordinary turn, and the issued dispatch owns the durable original root.
#[tokio::test]
async fn runtime_budget_auxiliary_actor_handle_cold_title_publishes_before_claim() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    let (mut scene, _primary, conversation) = setup();
    scene
        .service
        .schedule_runtime_agent_session_title(&conversation, Some("Qualify actual title admission"))
        .unwrap();
    let dispatch = scene
        .service
        .pending_agent_session_title_dispatch_id(&conversation)
        .unwrap()
        .to_owned();
    let service = std::mem::replace(&mut scene.service, test_runtime_service());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let client = async {
        let claim = handle
            .claim_agent_session_title_task(conversation.clone(), dispatch)
            .await;
        let shutdown = handle.shutdown().await;
        (claim, shutdown)
    };
    let ((claim, shutdown), exit) = tokio::join!(client, actor.run());
    scene.service = exit.service;
    shutdown.unwrap();
    let claim = claim.unwrap().unwrap();
    let member = &claim.budget_owner.as_ref().unwrap().member;
    assert_eq!(member.conversation, conversation);
    assert_eq!(
        scene
            .store
            .budget_membership(&conversation)
            .unwrap()
            .unwrap(),
        (member.clone(), BudgetMembershipState::Active)
    );
    assert!(scene.service.agent_turn_ledger().turns().is_empty());
    drop(claim);
}

#[tokio::test]
async fn runtime_budget_auxiliary_manual_compaction_uses_original_cold_conversation() {
    qualify_manual_compaction(false).await;
}

#[tokio::test]
async fn runtime_budget_auxiliary_raw_tail_needs_no_membership_or_inference() {
    qualify_manual_compaction(true).await;
}

async fn qualify_manual_compaction(raw_tail: bool) {
    use mez_agent::transcript::{TranscriptEntry, TranscriptRole};
    let (mut scene, primary, conversation) = setup();
    if raw_tail {
        scene
            .service
            .execute_agent_shell_command(&primary, "/compaction-mode raw_tail")
            .unwrap();
    }
    for sequence in 1..=3 {
        scene
            .transcripts
            .append(&TranscriptEntry {
                conversation_id: conversation.clone(),
                sequence,
                created_at_unix_seconds: sequence,
                role: TranscriptRole::User,
                turn_id: format!("historical-{sequence}"),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: format!(
                    "historical public reference {sequence} {}",
                    "source ".repeat(5_000)
                ),
            })
            .unwrap();
    }
    scene
        .service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 3)
        .unwrap();
    let response = scene
        .service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    let generation = scene
        .service
        .pending_agent_compaction_task_generation("%1")
        .unwrap_or_else(|| panic!("compaction absent: {response}"));
    if raw_tail {
        assert!(
            scene
                .service
                .prepare_runtime_budget_auxiliary(&RuntimeBudgetAuxiliarySource::Compaction {
                    pane: "%1".into(),
                    generation,
                })
                .unwrap()
                .is_none()
        );
        assert!(
            scene
                .service
                .claim_agent_compaction_task("%1", generation)
                .unwrap()
                .is_none()
        );
        assert!(
            scene
                .store
                .budget_membership(&conversation)
                .unwrap()
                .is_none()
        );
        assert!(scene.service.agent_turn_ledger().turns().is_empty());
        return;
    }
    assert!(
        scene
            .service
            .claim_agent_compaction_task("%1", generation)
            .is_err()
    );
    prepare(
        &mut scene,
        &RuntimeBudgetAuxiliarySource::Compaction {
            pane: "%1".into(),
            generation,
        },
    )
    .await
    .unwrap();
    let dispatch = scene
        .service
        .claim_agent_compaction_task("%1", generation)
        .unwrap()
        .unwrap();
    assert_eq!(
        dispatch.budget_owner.as_ref().unwrap().member.conversation,
        conversation
    );
    assert!(scene.service.agent_turn_ledger().turns().is_empty());
    drop(dispatch);
}

fn setup() -> (Scene, mez_core::ids::ClientId, String) {
    // Every test uses a real original constructor birth and private stores.
    let mut scene = Scene::new();
    scene.service.set_config_root(scene._root.to_path_buf());
    scene.service.replace_config_layers(vec![ConfigLayer {
        name: "budget-auxiliary".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\nshell_mode=\"pane\"\ndefault_provider=\"openai\"\ndefault_model_profile=\"budget-test\"\nsession_title_policy=\"generated\"\n[permissions]\nsandbox=\"policy-only\"\n[providers.openai]\nkind=\"openai\"\nmodels=[\"gpt-test\"]\n[model_profiles.budget-test]\nprovider=\"openai\"\nmodel=\"gpt-test\"\ncontext_window_tokens=128000\n".into(),
    }]).unwrap();
    scene.service.enable_runtime_budget_membership_for_tests();
    let primary = scene
        .service
        .attach_primary("primary", true, Size::new(120, 30).unwrap(), 120)
        .unwrap();
    let conversation = scene
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    (scene, primary, conversation)
}

async fn prepare(scene: &mut Scene, source: &RuntimeBudgetAuxiliarySource) -> Result<()> {
    let work = scene
        .service
        .prepare_runtime_budget_auxiliary(source)?
        .unwrap();
    assert!(work.mcp_plans.is_empty());
    assert!(!work.refresh_provider_credential);
    assert!(!work.allow_stdio);
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    scene.service.apply_agent_provider_preparation(outcome)
}

#[tokio::test]
async fn runtime_budget_auxiliary_cold_title_and_remember_share_one_account_without_turn() {
    let (mut scene, primary, conversation) = setup();
    scene
        .service
        .schedule_runtime_agent_session_title(
            &conversation,
            Some("Qualify cold auxiliary ownership"),
        )
        .unwrap();
    let dispatch = scene
        .service
        .pending_agent_session_title_dispatch_id(&conversation)
        .unwrap()
        .to_owned();
    assert!(
        scene
            .service
            .claim_agent_session_title_dispatch(&conversation, &dispatch)
            .is_err()
    );
    assert!(
        scene
            .store
            .budget_membership(&conversation)
            .unwrap()
            .is_none()
    );
    assert!(scene.service.agent_turn_ledger().turns().is_empty());
    prepare(
        &mut scene,
        &RuntimeBudgetAuxiliarySource::Title {
            conversation: conversation.clone(),
            dispatch: dispatch.clone(),
        },
    )
    .await
    .unwrap();
    let title = scene
        .service
        .claim_agent_session_title_dispatch(&conversation, &dispatch)
        .unwrap()
        .unwrap();
    let original = title.budget_owner.as_ref().unwrap().member.clone();
    assert_eq!(original.conversation, conversation);
    assert_eq!(
        scene
            .store
            .budget_membership(&conversation)
            .unwrap()
            .unwrap()
            .1,
        BudgetMembershipState::Active
    );
    scene
        .service
        .execute_agent_shell_command(&primary, "/remember Retain original accounting ownership.")
        .unwrap();
    let observation = scene
        .service
        .pending_agent_remember_observation_id("%1")
        .unwrap()
        .to_owned();
    prepare(
        &mut scene,
        &RuntimeBudgetAuxiliarySource::Remember {
            pane: "%1".into(),
            observation: observation.clone(),
        },
    )
    .await
    .unwrap();
    let remember = scene
        .service
        .claim_agent_remember_task("%1", &observation)
        .unwrap()
        .unwrap();
    assert_eq!(remember.budget_owner.as_ref().unwrap().member, original);
    assert!(scene.service.agent_turn_ledger().turns().is_empty());
    {
        use crate::storage::token_usage::budget::receipts::BudgetAttemptPurpose;
        let work = remember
            .budget_owner
            .as_ref()
            .unwrap()
            .prepare_attempt(
                mez_agent::ModelTokenUsageKey::new(
                    &remember.task.model_profile.provider,
                    &remember.task.model_profile.model,
                ),
                BudgetAttemptPurpose::Memory,
            )
            .unwrap();
        let registered = tokio::task::spawn_blocking(move || work.execute())
            .await
            .unwrap()
            .unwrap();
        let receipt = registered
            .freeze_receipt(
                mez_agent::token_budget::UsageSnapshot {
                    input: Some(5),
                    output: Some(2),
                    terminal: true,
                },
                None,
            )
            .unwrap();
        let outcome = tokio::task::spawn_blocking(move || receipt.execute())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome.root, original.root);
        assert_eq!(outcome.known.input, 5);
        assert_eq!(outcome.known.output, 2);
    }
    // Both genuine dispatches hold the original inode independently of actor
    // lifetime. No parent/child cleanup or execution authority is implied.
    let title_owner = title.budget_owner.unwrap();
    let memory_owner = remember.budget_owner.unwrap();
    title_owner.validate_request(&conversation).unwrap();
    assert!(title_owner.validate_request("replacement").is_err());
    let reference = scene
        .service
        .runtime_budget_checkpoint_reference(&conversation)
        .unwrap()
        .unwrap();
    let native = scene
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .native_message_incarnation
        .clone();
    scene
        .service
        .agent_shell_store_mut()
        .bind_conversation("%1", &conversation, 0)
        .unwrap();
    scene
        .service
        .retire_runtime_budget_departure("%1", &conversation, &native);
    assert!(
        scene
            .store
            .restore_native_budget_membership(&scene.transcripts, &reference)
            .is_err()
    );
    drop(title_owner);
    assert!(
        scene
            .store
            .restore_native_budget_membership(&scene.transcripts, &reference)
            .is_err()
    );
    drop(memory_owner);
    let restored = scene
        .store
        .restore_native_budget_membership(&scene.transcripts, &reference)
        .unwrap();
    drop(restored);
}

#[tokio::test]
async fn runtime_budget_auxiliary_storage_refusal_retains_original_memory_capture() {
    let (mut scene, primary, conversation) = setup();
    scene
        .service
        .execute_agent_shell_command(
            &primary,
            "/remember Qualify membership publication failure.",
        )
        .unwrap();
    let observation = scene
        .service
        .pending_agent_remember_observation_id("%1")
        .unwrap()
        .to_owned();
    let source = RuntimeBudgetAuxiliarySource::Remember {
        pane: "%1".into(),
        observation: observation.clone(),
    };
    let connection = rusqlite::Connection::open(scene.store.path()).unwrap();
    connection.execute_batch("CREATE TRIGGER refuse_aux BEFORE INSERT ON budget_memberships BEGIN SELECT RAISE(ABORT,'fixture refusal'); END;").unwrap();
    assert!(prepare(&mut scene, &source).await.is_err());
    assert!(
        scene
            .service
            .claim_agent_remember_task("%1", &observation)
            .is_err()
    );
    assert_eq!(
        scene.service.pending_agent_remember_observation_id("%1"),
        Some(observation.as_str())
    );
    assert!(
        scene
            .store
            .budget_membership(&conversation)
            .unwrap()
            .is_none()
    );
    connection
        .execute_batch("DROP TRIGGER refuse_aux;")
        .unwrap();
    drop(connection);
    prepare(&mut scene, &source).await.unwrap();
    let dispatch = scene
        .service
        .claim_agent_remember_task("%1", &observation)
        .unwrap()
        .unwrap();
    assert_eq!(
        dispatch.budget_owner.as_ref().unwrap().member.conversation,
        conversation
    );
    assert!(
        scene
            .service
            .claim_agent_remember_task("%1", &observation)
            .unwrap()
            .is_none()
    );
    assert!(scene.service.agent_turn_ledger().turns().is_empty());
}

#[tokio::test]
async fn runtime_budget_auxiliary_queued_same_conversation_rebind_cannot_borrow_new_native() {
    let (mut scene, primary, conversation) = setup();
    scene
        .service
        .execute_agent_shell_command(&primary, "/remember Keep original native custody.")
        .unwrap();
    let observation = scene
        .service
        .pending_agent_remember_observation_id("%1")
        .unwrap()
        .to_owned();
    scene
        .service
        .agent_shell_store_mut()
        .bind_conversation("%1", &conversation, 0)
        .unwrap();
    let source = RuntimeBudgetAuxiliarySource::Remember {
        pane: "%1".into(),
        observation: observation.clone(),
    };
    assert!(
        scene
            .service
            .prepare_runtime_budget_auxiliary(&source)
            .is_err()
    );
    assert!(
        scene
            .service
            .claim_agent_remember_task("%1", &observation)
            .is_err()
    );
    assert!(
        scene
            .store
            .budget_membership(&conversation)
            .unwrap()
            .is_none()
    );
    assert!(scene.service.agent_turn_ledger().turns().is_empty());
}
