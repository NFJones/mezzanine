//! Async-runtime tests owned by terminal loop behavior.

use super::super::*;

/// Verifies `/compact` submitted through the pane-local prompt queues async
/// compaction dispatch side effects. This covers the attached-terminal path
/// used by interactive agent mode, where `/compact` must not leave only a
/// pending task and a visible `compacting` status.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_compact_submission_queues_compaction_dispatch() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "async-attached-compact-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "async-attached-compact-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-test"]
default_model = "gpt-compact-test"
[model_profiles.async-attached-compact-test]
provider = "openai"
model = "gpt-compact-test"
context_window_tokens = 128000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-async-attached-compact-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&transcript_root).unwrap();
    let transcript_store = AgentTranscriptStore::new(transcript_root);
    for sequence in 1..=3 {
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "async-attached-compact".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("compact source entry {sequence}"),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "async-attached-compact", 3)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![
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
        ]],
        input_batches: vec![b"/compact\r".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();
        assert_eq!(
            report.actions,
            vec![TerminalClientLoopAction::ForwardToPane(
                b"/compact\r".to_vec()
            )]
        );
        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert!(
            dispatches.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::DispatchAgentCompaction { pane_id } if pane_id == "%1"
            )),
            "{dispatches:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let config = exit
        .service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(
        config
            .frame_context
            .panes
            .get("%1")
            .and_then(|pane| pane.agent_status.as_deref()),
        Some("compacting")
    );
    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that the attached terminal loop can run in deferred pane I/O mode,
/// where forwarded primary input becomes a pane side effect instead of a direct
/// synchronous manager write. This is the mode required once live pane
/// processes are owned by supervised async workers.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_can_defer_pane_input_to_worker() {
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
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: true,
            writable: false,
            hangup: false,
            error: false,
        }]],
        input_batches: vec![b"hello\n".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 1);
        assert_eq!(
            report.actions,
            vec![TerminalClientLoopAction::ForwardToPane(b"hello\n".to_vec())]
        );
        assert_eq!(report.output_frames, 0);
        assert_eq!(io.written_batches.len(), 0);
        assert_eq!(
            handle.drain_pane_io_side_effects("%1", 8).await.unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"hello\n".to_vec(),
            }]
        );
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());

    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies that a stalled attached-terminal readiness await returns a typed
/// loop error instead of monopolizing the foreground client service forever.
/// The outer timeout is intentionally one millisecond longer than the loop
/// step bound: without the production timeout, this regression fails by hitting
/// the outer guard instead of observing the structured operation error.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_loop_times_out_stalled_readiness_poll() {
    struct StalledReadinessIo;

    impl AsyncAttachedTerminalIo for StalledReadinessIo {
        fn poll_readiness<'a>(
            &'a mut self,
        ) -> AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
            Box::pin(std::future::pending())
        }

        fn read_input<'a>(&'a mut self, _max_bytes: usize) -> AsyncTerminalIoFuture<'a, Vec<u8>> {
            Box::pin(async {
                Err(crate::error::MezError::invalid_state(
                    "stalled readiness test should not read input",
                ))
            })
        }

        fn write_styled_output_with_modes<'a>(
            &'a mut self,
            _lines: &'a [String],
            _line_style_spans: &'a [Vec<mez_terminal::TerminalStyleSpan>],
            _modes: AttachedTerminalOutputModes,
        ) -> AsyncTerminalIoFuture<'a, usize> {
            Box::pin(async {
                Err(crate::error::MezError::invalid_state(
                    "stalled readiness test should not write output",
                ))
            })
        }
    }

    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, _actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = StalledReadinessIo;

    let result = tokio::time::timeout(
        Duration::from_millis(251),
        run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        ),
    )
    .await
    .expect("attached-terminal loop should return its own timeout before the test guard");
    let error = result.unwrap_err();

    assert_eq!(
        error.to_string(),
        "InvalidState: async attached terminal readiness poll timed out after 250 ms"
    );
}

/// Verifies large foreground input is drained across bounded client reads.
///
/// Host paste payloads can be larger than one attached-terminal read. The
/// client loop must keep reading subsequent chunks and queue every accepted
/// byte as ordered pane-input side effects instead of treating the first
/// viewport-sized read as the whole paste.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_preserves_large_deferred_paste_across_reads() {
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
    let paste = b"large-paste-".repeat(16);
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![
            vec![AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            }],
            vec![AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            }],
            vec![AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            }],
        ],
        input_batches: vec![paste.clone()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 3,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 3);
        let queued = handle.drain_pane_io_side_effects("%1", 8).await.unwrap();
        let forwarded = queued
            .into_iter()
            .filter_map(|effect| match effect {
                RuntimeSideEffect::WritePaneInput { bytes, .. } => Some(bytes),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(forwarded, paste);
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());

    exit.service.terminate_all_pane_processes().unwrap();
}

/// Verifies async attached terminal loop renders and applies primary actions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_renders_and_applies_primary_actions() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![
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
        ]],
        input_batches: vec![b"\x01\x1b[C".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| {
                Ok(Some(ClientStatusLine {
                    kind: ClientStatusKind::Plain,
                    text: "attached".to_string(),
                }))
            },
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 1);
        assert_eq!(
            report.actions,
            vec![TerminalClientLoopAction::ExecuteMux(MuxAction::FocusPane(
                mez_mux::input::PaneFocusDirection::Right
            ))]
        );
        assert_eq!(report.output_frames, 2);
        assert_eq!(io.written_batches.len(), 2);
        assert_eq!(
            io.written_batches.last().unwrap()[23].trim_end(),
            "attached"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed > 0);
}

/// Verifies that foreground client-step errors are shown through actor-owned
/// overlay state instead of a private prompt-error acknowledgement loop. This
/// keeps the async loop non-blocking even when no acknowledgement input is
/// available in the current batch.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_routes_runtime_errors_to_actor_overlay() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let wrong_primary = ClientId::new('c', 4242);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![
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
        ]],
        input_batches: vec![b"hello\n".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = tokio::time::timeout(
            Duration::from_millis(250),
            run_async_attached_terminal_client_loop(
                &handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: primary.clone(),
                    primary_client_id: Some(wrong_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                |_| Ok(None),
            ),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(report.output_frames, 2);
        assert_eq!(io.written_batches.len(), 2);
        let error_frame = io.written_batches.last().unwrap();
        assert!(
            error_frame
                .iter()
                .any(|line| line.contains("operation requires the primary client")),
            "{:?}",
            error_frame
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    let overlay_view = exit
        .service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        overlay_view
            .lines
            .iter()
            .any(|line| line.contains("operation requires the primary client")),
        "{:?}",
        overlay_view.lines
    );
    assert!(exit.commands_processed >= 4);
}

/// Verifies async attached terminal loop runs actor owned command prompt.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_runs_actor_owned_command_prompt() {
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-async-command-prompt-history-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&transcript_root);
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    let mut service = test_service();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let command_history_path = transcript_store.command_prompt_history_file();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![
            vec![
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
            ],
            vec![
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
            ],
            vec![
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
            ],
        ],
        input_batches: vec![
            b"\x01:".to_vec(),
            b"list-buffers\r".to_vec(),
            b"\x1b".to_vec(),
        ],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 2,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 2);
        assert_eq!(report.output_frames, 4);
        assert_eq!(
            report.actions,
            vec![
                TerminalClientLoopAction::ExecuteMux(MuxAction::EnterCommandPrompt),
                TerminalClientLoopAction::ForwardToPane(b"list-buffers\r".to_vec())
            ]
        );
        assert_eq!(io.written_batches.len(), 4);
        assert_eq!(io.written_batches[1][23].trim_end(), "▐ :");
        assert!(
            io.written_batches[3]
                .iter()
                .any(|line| line.contains("buffers: 0"))
        );
        assert!(
            io.written_batches[3]
                .iter()
                .any(|line| line.contains("source: runtime"))
        );
        assert!(
            io.written_batches[3]
                .iter()
                .any(|line| line.contains("status: empty"))
        );
        assert!(!command_history_path.exists());
        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert_eq!(
            transcript_store.command_prompt_history().unwrap(),
            vec![String::from("list-buffers")]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed > 0);
    let _ = std::fs::remove_dir_all(transcript_root);
}

/// Verifies async attached terminal loop routes agent shell input non modally.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_routes_agent_shell_input_non_modally() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![
            vec![
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
            ],
            vec![
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
            ],
            vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: false,
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
            ],
        ],
        input_batches: vec![b"\x01a".to_vec(), b"/status\r".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 3,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 3);
        assert_eq!(
            report.actions,
            vec![
                TerminalClientLoopAction::ExecuteMux(MuxAction::ToggleAgentShell),
                TerminalClientLoopAction::ForwardToPane(b"/status\r".to_vec()),
            ]
        );
        assert_eq!(report.output_frames, 4);
        assert_eq!(io.written_batches.len(), 4);
        assert!(
            io.written_batches[1]
                .iter()
                .any(|line| line.trim_end() == "▐ mez>")
        );
        let status_output = io.written_batches[2].join("\n");
        assert!(
            status_output.contains("Agent Status")
                && status_output.contains("│ Visibility")
                && !status_output.contains("Quota Usage"),
            "{status_output}"
        );
        assert!(
            !io.written_batches[2]
                .iter()
                .any(|line| line.contains("agent-shell:"))
        );
        assert!(
            !io.written_batches[2]
                .iter()
                .any(|line| line.trim_end() == "▐ agent>"),
            "status display should use the pager overlay instead of pane prompt rows"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 6);
}

/// Verifies that submitting pane-local agent prompt input redraws the client
/// frame in the same attached-terminal loop pass. Without this refresh, the
/// submitted prompt text stayed visible until a later agent state change caused
/// the next render, which made queued follow-up prompts feel blocked.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_clears_agent_prompt_on_submit() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![
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
        ]],
        input_batches: vec![b"list files\r".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 1);
        assert_eq!(
            report.actions,
            vec![TerminalClientLoopAction::ForwardToPane(
                b"list files\r".to_vec()
            )]
        );
        assert_eq!(report.output_frames, 1);
        assert_eq!(io.written_batches.len(), 1);
        let refreshed = io.written_batches.last().unwrap();
        assert!(
            refreshed
                .iter()
                .any(|line| line.trim_end().starts_with("▐ mez> thinking")),
            "{refreshed:?}"
        );
        assert!(
            !refreshed
                .iter()
                .any(|line| line.trim_end() == "▐ mez> list files"),
            "{refreshed:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert_eq!(exit.service.pending_agent_provider_tasks().len(), 1);
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("user> list files"), "{pane_text}");
    assert!(exit.commands_processed >= 4);
}

/// Verifies that leaving pane-local agent mode invalidates the attached
/// terminal's differential frame state before repainting. The agent prompt is a
/// Mezzanine-owned overlay, while the underlying shell prompt is PTY-owned; a
/// full redraw at this boundary keeps cursor placement and stale prompt rows
/// from leaking after the mode switch.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_full_redraws_after_agent_prompt_exit() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeResizingAttachedTerminalLoopIo {
        inner: FakeAttachedTerminalLoopIo {
            readiness_batches: vec![vec![
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
            ]],
            input_batches: vec![b"/exit\r".to_vec()],
            written_batches: Vec::new(),
            write_error_kinds: Vec::new(),
        },
        terminal_size_batches: Vec::new(),
        invalidated_output_frames: 0,
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 1);
        assert_eq!(report.output_frames, 1);
        assert_eq!(io.invalidated_output_frames, 1);
        assert_eq!(io.inner.written_batches.len(), 1);
        assert!(
            !io.inner.written_batches[0]
                .iter()
                .any(|line| line.contains("▐ agent>")),
            "{:?}",
            io.inner.written_batches[0]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 4);
}

/// Verifies async attached terminal loop renders observer without applying input.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_renders_observer_without_applying_input() {
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(test_service())
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![
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
        ]],
        input_batches: vec![b"\x1b=".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Observer,
                client_id: ClientId::new('c', 9001),
                primary_client_id: None,
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| {
                Ok(Some(ClientStatusLine {
                    kind: ClientStatusKind::PendingObserver,
                    text: "observe".to_string(),
                }))
            },
        )
        .await
        .unwrap();

        assert!(report.actions.is_empty());
        assert_eq!(report.output_frames, 1);
        assert_eq!(io.input_batches.len(), 0);
        assert_eq!(io.written_batches[0][23].trim_end(), "observer: observe");
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 4);
}

/// Verifies that direct foreground client-loop rendering schedules the
/// actor-owned cursor and status timers. Foreground attached clients still use a
/// direct render path while the refactor is in progress, and those frames must
/// seed timer-driven invalidations before the blind batch sleep can be removed.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_schedules_render_timers_after_direct_flush() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Output,
            fd: 1,
            interest: TerminalFdInterest::write(),
            readable: false,
            writable: true,
            hangup: false,
            error: false,
        }]],
        input_batches: Vec::new(),
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.output_frames, 1);
        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timers.len(), 1);
        let RuntimeSideEffect::ScheduleTimer { key, .. } = &timers[0] else {
            panic!("expected status refresh timer: {timers:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::StatusRefresh);
        assert_eq!(key.owner_id, primary.to_string());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 5);
}
