//! Saved-fork storage ownership across the actual actor and command supervisor.
//!
//! Owned synthetic history and managed fixture shells exercise blocking source
//! acquisition, responsive control, actor adoption and durable binding recovery.
//! No provider requests, credentials or historical actions are executed.

use super::super::*;

/// The real supervisor aborts auxiliary services on explicit cancellation,
/// last-primary completion, and primary error. Its report must wait for the
/// separately retained fork owner, not detach a blocking task when aborting the
/// command wrapper. The source lease stays held until supervision has returned.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_saved_fork_supervisor_observes_retirement_on_all_exit_paths() {
    for mode in ["shutdown", "last-primary", "error"] {
        let root =
            std::env::temp_dir().join(format!("mez-fork-supervision-{}", rand::random::<u64>()));
        let store = crate::storage::transcript::AgentTranscriptStore::new(root.clone());
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "supervised-source".into(),
                sequence: 1,
                created_at_unix_seconds: 1,
                role: mez_agent::transcript::TranscriptRole::User,
                turn_id: "owned-turn".into(),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: "owned source".into(),
            })
            .unwrap();
        let mut service = test_service();
        service.set_agent_transcript_store(store.clone());
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
            .bind_conversation("%1", "supervised-source", 1)
            .unwrap();
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        service.set_deferred_agent_command_probe_for_tests(started.clone(), release.clone());
        let (source_lock, _, _) = store
            .lock_presentation_snapshot("supervised-source")
            .unwrap();
        let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
            .build()
            .unwrap();
        let client = async {
            let command_service = crate::host::async_runtime::build_async_agent_command_service(
                "commands",
                handle.clone(),
                AsyncAgentProviderServiceConfig::new(1)
                    .unwrap()
                    .with_idle_interval(Duration::from_millis(5))
                    .unwrap(),
            )
            .unwrap();
            let retirement = command_service.saved_fork_retirement.clone().unwrap();
            let (primary_tx, primary_rx) = tokio::sync::oneshot::channel();
            let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
            let primary_service = AsyncRuntimeService::new("owned-primary", async move {
                primary_rx.await.unwrap();
                if mode == "error" {
                    Err(MezError::invalid_state("owned primary failure"))
                } else {
                    Ok(AsyncRuntimeServiceExit::completed(1))
                }
            });
            let supervisor = tokio::spawn(supervise_async_runtime_services(
                vec![primary_service, command_service],
                async move {
                    let _ = cancel_rx.await;
                },
            ));
            handle
                .apply_attached_terminal_step_plan(
                    primary.clone(),
                    AttachedTerminalClientStepPlan {
                        actions: vec![TerminalClientLoopAction::ForwardToPane(
                            b"/fork supervised-target\r".to_vec(),
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
            tokio::time::timeout(Duration::from_secs(2), started.notified())
                .await
                .unwrap();
            assert_eq!(retirement.retained_workers_for_tests(), 1);
            if mode == "shutdown" {
                cancel_tx.send(()).unwrap();
            } else {
                primary_tx.send(()).unwrap();
            }
            let result = tokio::time::timeout(Duration::from_secs(2), supervisor)
                .await
                .expect("supervisor must cancel held acquisition and observe true retirement")
                .unwrap();
            if mode == "error" {
                assert!(
                    result
                        .unwrap_err()
                        .message()
                        .contains("owned primary failure")
                );
            } else {
                let report = result.unwrap();
                assert_eq!(report.shutdown_requested, mode == "shutdown");
                assert!(
                    report
                        .services
                        .iter()
                        .any(|service| service.name == "commands")
                );
            }
            assert_eq!(retirement.retained_workers_for_tests(), 0);
            assert!(!root.join("supervised-target").exists());
            drop(source_lock);
            assert!(store.saved_session("supervised-target").unwrap().is_none());
            assert_eq!(store.inspect("supervised-source").unwrap().len(), 1);
            handle.shutdown().await.unwrap();
        };
        let ((), mut exit) = tokio::join!(client, actor.run());
        exit.service.terminate_all_pane_processes().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// Stopping the real command service during source-lock acquisition requests
/// fork cancellation and observes its wrapper through actual blocking retirement.
/// The fixture keeps the source locked until the service returns: shutdown must
/// not depend on a successful copy or leave a delayed target writer behind.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_saved_fork_stop_retires_blocked_publication_owner() {
    blocked_saved_fork_retirement(false).await;
}

/// Exercises the real unchanged 120-second command deadline, not a shortened
/// test budget. Its error must settle while source ownership remains unavailable,
/// then service stop must observe the original blocking owner before returning.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_saved_fork_deadline_observes_blocked_owner_retirement() {
    blocked_saved_fork_retirement(true).await;
}

/// Runs either deadline or explicit-stop retirement with an owned source lease.
/// The source remains locked until both supervisor and its fork owner have exited.
async fn blocked_saved_fork_retirement(deadline: bool) {
    let root = std::env::temp_dir().join(format!("mez-fork-stop-{}", rand::random::<u64>()));
    let store = crate::storage::transcript::AgentTranscriptStore::new(root.clone());
    store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "stop-source".into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "fixture-turn".into(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content: "owned source".into(),
        })
        .unwrap();
    store
        .save_compaction_epoch("stop-source", 1, "owned summary")
        .unwrap();
    let mut service = test_service();
    service.set_agent_transcript_store(store.clone());
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
        .bind_conversation("%1", "stop-source", 1)
        .unwrap();
    let started = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    service.set_deferred_agent_command_probe_for_tests(started.clone(), release.clone());
    let (source_lock, _, _) = store.lock_presentation_snapshot("stop-source").unwrap();
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
                move |_, _| worker_stop.load(std::sync::atomic::Ordering::Acquire),
            )
            .await
        });
        handle
            .apply_attached_terminal_step_plan(
                primary.clone(),
                AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(
                        b"/fork stop-target\r".to_vec(),
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
        if deadline {
            tokio::time::timeout(Duration::from_secs(150), async {
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
                    if view.lines.join("\n").contains("exceeded its") {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("unchanged command deadline must settle without releasing source");
        }
        stop.store(true, std::sync::atomic::Ordering::Release);
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .expect("stop must cancel and observe lock acquisition without releasing source")
            .unwrap()
            .unwrap();
        assert!(!root.join("stop-target").exists());
        drop(source_lock);
        let view = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!view.lines.join("\n").contains("forked=true"));
        assert_eq!(store.inspect("stop-source").unwrap().len(), 1);
        assert!(store.saved_session("stop-target").unwrap().is_none());
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

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
        let mut checkpoints = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                for effect in handle.drain_runtime_side_effects(128).await.unwrap() {
                    if let RuntimeSideEffect::PersistAgentSessionMetadata { .. } = &effect {
                        checkpoints.push(effect);
                    }
                }
                if checkpoints.iter().any(|effect| {
                    matches!(effect,
                    RuntimeSideEffect::PersistAgentSessionMetadata { records, .. }
                        if records.iter().any(|record| record.conversation_id == "actor-target"))
                }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual supervisor adoption must settle without target-lock recursion");
        let mut persisted = false;
        for effect in checkpoints {
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
