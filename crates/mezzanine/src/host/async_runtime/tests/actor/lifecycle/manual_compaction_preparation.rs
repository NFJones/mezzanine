//! Early manual compaction visibility while immutable source preparation waits.
//!
//! Explicit gates hold preparation before durable decoding, not the provider.
//! Actor input, render, duplicate rejection and stop must remain responsive; the
//! existing model compactor may be queued only after exact adoption. No model
//! request or synthetic completion is needed to qualify this ownership phase.

use super::*;

/// Gate the source worker after command admission and require visible compacting
/// without provider dispatch. Another pane still accepts input/render. A second
/// compact cannot replace this owner; Stop clears exactly it, while stale worker
/// completion cannot resurrect cancelled work. The success branch queues the
/// unchanged provider compactor after release, not a fake ordinary user turn.
#[tokio::test(flavor = "current_thread")]
async fn async_manual_compaction_preparation_is_visible_responsive_and_cancellable() {
    for cancel in [false, true] {
        Box::pin(qualify(cancel, false)).await;
    }
}

/// Hold immutable request rendering after source adoption. Current compacting
/// ownership must survive this phase without sending a provider request, and
/// cancellation must reject the later rendered task rather than resurrect work.
#[tokio::test(flavor = "current_thread")]
async fn async_manual_compaction_request_preparation_is_responsive_and_cancellable() {
    for cancel in [false, true] {
        Box::pin(qualify(cancel, true)).await;
    }
}

/// Owns an exact preparation gate and actor under a finite fixture deadline.
async fn qualify(cancel: bool, request_phase: bool) {
    let root = std::env::temp_dir().join(format!("mez-cprepare-{:032x}", rand::random::<u128>()));
    let store = AgentTranscriptStore::new(root.clone());
    for sequence in 1..=3 {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "prepare-compact".into(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: format!("completed source {sequence}"),
            })
            .unwrap();
    }
    let mut service = test_service();
    service.set_agent_transcript_store(store.clone());
    service.replace_config_layers(vec![ConfigLayer {name:"compaction-preparation".into(),path:None,format:ConfigFormat::Toml,scope:ConfigScope::Primary,trusted:true,
        text:"[agents]\ndefault_provider=\"openai\"\ndefault_model_profile=\"default\"\n[providers.openai]\nkind=\"openai\"\nmodels=[\"fixture-model\"]\ndefault_model=\"fixture-model\"\n[model_profiles.default]\nprovider=\"openai\"\nmodel=\"fixture-model\"\ncontext_window_tokens=128000\n".into()}]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 1)
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
        .bind_conversation("%1", "prepare-compact", 3)
        .unwrap();
    let second = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap()
        .pane_id;
    service
        .agent_shell_store_mut()
        .enter_or_resume(second.as_str())
        .unwrap();
    service
        .session_mut_for_tests()
        .select_pane(&primary, "%1")
        .unwrap();
    let started = StdArc::new(tokio::sync::Notify::new());
    let release = StdArc::new(tokio::sync::Notify::new());
    let completed = StdArc::new(tokio::sync::Notify::new());
    if request_phase {
        service.set_manual_compaction_request_probe_for_tests(
            started.clone(),
            release.clone(),
            completed.clone(),
        );
    } else {
        service.set_manual_compaction_preparation_probe_for_tests(
            started.clone(),
            release.clone(),
            completed.clone(),
        );
    }
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        let response = handle
            .execute_agent_shell_command(primary.clone(), "/compact".into())
            .await
            .unwrap();
        assert!(response.contains("state=preparing"));
        started.notified().await;
        let config = handle
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .await
            .unwrap();
        assert_eq!(
            config
                .frame_context
                .panes
                .get("%1")
                .and_then(|pane| pane.agent_status.as_deref()),
            Some("compacting")
        );
        assert!(
            !handle
                .drain_agent_provider_dispatch_side_effects(16)
                .await
                .unwrap()
                .iter()
                .any(|effect| matches!(effect, RuntimeSideEffect::DispatchAgentCompaction { .. }))
        );
        let duplicate = handle
            .execute_agent_shell_command(primary.clone(), "/compact".into())
            .await
            .unwrap();
        assert!(!duplicate.contains("state=preparing"));
        handle
            .execute_terminal_command(primary.clone(), format!("select-pane -t {second}"))
            .await
            .unwrap();
        let input = handle
            .apply_attached_terminal_step_plan(
                primary.clone(),
                AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(b"x".to_vec())],
                    output_lines: vec![],
                    output_line_style_spans: vec![],
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: vec![],
                },
            )
            .await
            .unwrap();
        assert_eq!(input.agent_prompt_inputs_applied, 1);
        assert!(
            handle
                .render_client_view(
                    ClientViewRole::Primary,
                    Size::new(100, 30).unwrap(),
                    TerminalClientLoopConfig::default()
                )
                .await
                .unwrap()
                .is_some()
        );
        handle
            .execute_terminal_command(primary.clone(), "select-pane -t %1".into())
            .await
            .unwrap();
        if cancel {
            let response = handle
                .execute_agent_shell_command(primary.clone(), "/stop".into())
                .await
                .unwrap();
            assert!(response.contains("compaction_cancelled=true"));
        }
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), completed.notified())
            .await
            .unwrap();
        if !cancel {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let effects = handle
                        .drain_agent_provider_dispatch_side_effects(16)
                        .await
                        .unwrap();
                    if effects.iter().any(|effect| {
                        matches!(effect, RuntimeSideEffect::DispatchAgentCompaction { .. })
                    }) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        } else {
            assert!(
                !handle
                    .drain_agent_provider_dispatch_side_effects(16)
                    .await
                    .unwrap()
                    .iter()
                    .any(|effect| matches!(
                        effect,
                        RuntimeSideEffect::DispatchAgentCompaction { .. }
                    ))
            );
            let config = handle
                .terminal_client_loop_config(TerminalClientLoopConfig::default())
                .await
                .unwrap();
            assert_ne!(
                config
                    .frame_context
                    .panes
                    .get("%1")
                    .and_then(|pane| pane.agent_status.as_deref()),
                Some("compacting")
            );
        }
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(Box::pin(client), actor.run())
    })
    .await
    .unwrap();
    if cancel {
        assert!(!exit.service.agent_is_compacting("%1"));
        assert!(exit.service.pending_agent_compaction_task_ids().is_empty());
    }
    assert!(store.compaction_epoch("prepare-compact").unwrap().is_none());
    exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
