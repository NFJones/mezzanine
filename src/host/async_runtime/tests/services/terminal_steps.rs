//! Async-runtime tests owned by terminal steps behavior.

use super::super::*;

/// Verifies async attached terminal step uses runtime rendered view.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_step_uses_runtime_rendered_view() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();

    let client = async {
        let readiness = vec![
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            },
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Output,
                fd: 1,
                interest: TerminalFdInterest::write(),
                readable: false,
                writable: true,
                hangup: false,
                error: false,
            },
        ];
        let status = ClientStatusLine {
            kind: ClientStatusKind::Plain,
            text: "attached".to_string(),
        };
        let plan = plan_async_attached_terminal_client_step(
            &handle,
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            TerminalClientLoopConfig::default(),
            &readiness,
            Some(b"\x01\""),
            Some(&status),
        )
        .await
        .unwrap();

        assert_eq!(
            plan.actions,
            vec![crate::host::terminal::TerminalClientLoopAction::ExecuteMux(
                MuxAction::SplitPaneHorizontal
            )]
        );
        assert_eq!(plan.output_lines.len(), 24);
        assert_eq!(plan.output_lines[23].trim_end(), "attached");
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert_eq!(exit.commands_processed, 2);
}

/// Verifies async attached terminal step can be applied through actor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_step_can_be_applied_through_actor() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();

    let client = async {
        let readiness = vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: true,
            writable: false,
            hangup: false,
            error: false,
        }];
        let (_plan, application) = plan_and_apply_async_attached_terminal_client_step(
            &handle,
            AsyncAttachedTerminalStepRequest {
                primary_client_id: primary.clone(),
                role: ClientViewRole::Primary,
                client_size: Size::new(80, 24).unwrap(),
                config: TerminalClientLoopConfig::default(),
                readiness: &readiness,
                input: Some(b"hello\n"),
                status: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(application.forwarded_bytes, 6);
        assert_eq!(
            handle.drain_pane_io_side_effects("%1", 8).await.unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"hello\n".to_vec(),
            }]
        );
        let large_input = vec![b'x'; 468_586];
        let (_plan, application) = plan_and_apply_async_attached_terminal_client_step(
            &handle,
            AsyncAttachedTerminalStepRequest {
                primary_client_id: primary.clone(),
                role: ClientViewRole::Primary,
                client_size: Size::new(80, 24).unwrap(),
                config: TerminalClientLoopConfig::default(),
                readiness: &readiness,
                input: Some(&large_input),
                status: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(application.forwarded_bytes, large_input.len());
        assert_eq!(
            handle
                .drain_pane_io_side_effects("%1", usize::MAX)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: large_input,
            }]
        );
        let split = AttachedTerminalClientStepPlan {
            actions: vec![TerminalClientLoopAction::ExecuteMux(
                MuxAction::SplitPaneVertical,
            )],
            output_lines: Vec::new(),
            output_line_style_spans: Vec::new(),
            input_hangup: false,
            output_hangup: false,
            error_roles: Vec::new(),
        };
        let split_application = handle
            .apply_attached_terminal_step_plan(primary, split)
            .await
            .unwrap();
        assert_eq!(split_application.mux_actions_applied, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());

    assert!(
        exit.commands_processed >= 5,
        "actor should process client-step, drain, split, and shutdown requests"
    );
    exit.service.terminate_all_pane_processes().unwrap();
}
