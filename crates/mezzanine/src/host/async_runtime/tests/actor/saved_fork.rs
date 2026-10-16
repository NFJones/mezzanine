//! Saved-fork storage ownership across the actual actor and command supervisor.
//!
//! Owned synthetic history and managed fixture shells exercise blocking source
//! acquisition, responsive control, actor adoption and durable binding recovery.
//! No provider requests, credentials or historical actions are executed.

use super::super::*;

/// A held source lease blocks the real command worker, not the actor. The actual
/// supervisor must publish/adopt the inherited epoch, settle the projected result
/// and queue binding persistence without recursively locking its target lease.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_saved_fork_store_lock_keeps_control_responsive_and_checkpoints() {
    let root = std::env::temp_dir().join(format!("mez-actor-saved-fork-{}", rand::random::<u64>()));
    let store = crate::storage::transcript::AgentTranscriptStore::new(root.clone());
    for sequence in 1..=100 {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "actor-source".into(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::User,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: format!("row-{sequence}"),
            })
            .unwrap();
    }
    store
        .save_compaction_epoch("actor-source", 80, "actor saved summary")
        .unwrap();
    let mut service = test_service();
    service.set_agent_transcript_store(store.clone());
    let session_id = service.session().id.to_string();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "actor-source", 100)
        .unwrap();
    let started = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    service.set_deferred_agent_command_probe_for_tests(started.clone(), release.clone());
    let (source_lock, _, _) = store.lock_presentation_snapshot("actor-source").unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client = async {
        let worker_handle = handle.clone();
        let worker_stop = stop.clone();
        let worker = tokio::spawn(async move {
            run_async_agent_command_service(
                &worker_handle,
                AsyncAgentProviderServiceConfig::new(1)
                    .unwrap()
                    .with_idle_interval(Duration::from_millis(5))
                    .unwrap(),
                move |_, state| {
                    worker_stop.load(std::sync::atomic::Ordering::Acquire)
                        || matches!(state, RuntimeLifecycleState::Stopping)
                },
            )
            .await
        });
        handle
            .apply_attached_terminal_step_plan(
                primary.clone(),
                AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(
                        b"/fork actor-target\r".to_vec(),
                    )],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        release.notify_one();
        let view = tokio::time::timeout(
            Duration::from_secs(2),
            handle.render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            ),
        )
        .await
        .expect("held store lock must not block actor control")
        .unwrap()
        .unwrap();
        assert!(view.lines.join("\n").contains("command running"));
        assert!(!root.join("actor-target").exists());
        drop(source_lock);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let view = handle
                    .render_client_view(
                        ClientViewRole::Primary,
                        Size::new(80, 24).unwrap(),
                        TerminalClientLoopConfig::default(),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                if view.lines.join("\n").contains("forked=true") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual supervisor adoption must settle without target-lock recursion");
        let mut persisted = false;
        for effect in handle.drain_runtime_side_effects(128).await.unwrap() {
            if let RuntimeSideEffect::PersistAgentSessionMetadata {
                store,
                mezzanine_session_id,
                records,
                ..
            } = effect
            {
                persisted |= records
                    .iter()
                    .any(|record| record.conversation_id == "actor-target");
                tokio::task::spawn_blocking(move || {
                    store.save_agent_session_metadata_checkpoint(&mezzanine_session_id, &records)
                })
                .await
                .unwrap()
                .unwrap();
            }
        }
        assert!(
            persisted,
            "adopted target binding must queue its own checkpoint"
        );
        let reopened = crate::storage::transcript::AgentTranscriptStore::new(root.clone());
        assert!(
            reopened
                .load_agent_session_metadata(&session_id)
                .unwrap()
                .iter()
                .any(|metadata| metadata.conversation_id == "actor-target")
        );
        assert_eq!(
            reopened
                .compaction_epoch("actor-target")
                .unwrap()
                .unwrap()
                .through_sequence,
            80
        );
        assert_eq!(reopened.inspect("actor-target").unwrap().len(), 100);
        stop.store(true, std::sync::atomic::Ordering::Release);
        assert!(
            tokio::time::timeout(Duration::from_secs(2), worker)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                >= 1
        );
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
