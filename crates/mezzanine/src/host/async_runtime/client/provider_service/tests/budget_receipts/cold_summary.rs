//! A fresh runtime restores the original stored account before a new prompt
//! can reach failure-summary inference. No interrupted request is resumed.

use super::*;

#[tokio::test]
async fn budget_cold_summary_rechecks_policy_before_summary_and_repair_http() {
    for input in [0, 11, 22, 100] {
        Box::pin(super::dispatch::qualify_cold_summary(input)).await;
    }
}

pub(super) async fn restore(original: Prepared) -> Prepared {
    let Prepared {
        mut service,
        dispatch,
        store,
        member,
        root,
    } = original;
    let layers = service.config_layers().to_vec();
    let auth = service.auth_store().unwrap().clone();
    let transcripts = service.agent_transcript_store().unwrap().clone();
    let primary = service
        .session()
        .clients()
        .iter()
        .find(|client| service.session().is_attached_primary(&client.id))
        .unwrap()
        .id
        .clone();
    let original_native = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .native_message_incarnation
        .snapshot_identity();
    service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    service.checkpoint_agent_session_metadata().unwrap();
    let session = service.session().clone();
    drop(dispatch);
    service.terminate_all_pane_processes().unwrap();
    drop(service);

    let mut service =
        crate::test_support::runtime::RuntimeServiceFixture::new().build_with_session(session);
    service.replace_config_layers(layers).unwrap();
    service.set_auth_store(auth);
    service.set_config_root(root.0.as_ref().unwrap().clone());
    service.set_token_usage_store(store.clone());
    service.set_agent_transcript_store(transcripts);
    service.enable_runtime_budget_membership_for_tests();
    assert_eq!(
        service
            .restore_agent_sessions_from_transcript_store()
            .unwrap(),
        1
    );
    let restored = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(restored.session_id, member.conversation);
    assert_ne!(
        restored.native_message_incarnation.snapshot_identity(),
        original_native
    );
    assert!(
        service.pending_agent_provider_tasks().is_empty(),
        "restore must not replay interrupted inference"
    );
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "Fresh prompt after original owner retirement.")
        .unwrap();
    let task = service
        .pending_agent_provider_tasks()
        .into_iter()
        .next()
        .unwrap();
    let preparation = service.prepare_agent_provider_work(&task.turn_id).unwrap();
    let prepared = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(prepared).unwrap();
    let mut dispatch = service
        .claim_configured_agent_provider_task(
            &AgentId::opaque(task.agent_id).unwrap(),
            &task.turn_id,
        )
        .unwrap()
        .unwrap();
    assert_eq!(dispatch.budget_owner.as_ref().unwrap().member, member);
    service
        .record_claimed_agent_provider_task(&dispatch, 7, 30_000)
        .unwrap();
    dispatch.claim_generation = 7;
    Prepared {
        service,
        dispatch,
        store,
        member,
        root,
    }
}
