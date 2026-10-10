//! Exact malformed patch recovery through real local provider and actor workers.
//!
//! The loopback server emits ordinary provider HTTP responses, never fabricated
//! action results or completion events. Gates qualify rejection before effects;
//! production native patch workers own the sole corrected filesystem mutation.
//! Temporary runtime/auth/storage roots isolate the fixture from user settings.

use super::*;

/// A malformed middle patch rejects its whole batch before either valid shell
/// sibling runs. The real provider service sends same-turn correction, then the
/// actor/native worker applies only the corrected patch and finishes normally.
/// The append-only source and action ledger must contain no rejected sibling
/// execution or duplicate successful file mutation.
#[tokio::test(flavor = "current_thread")]
async fn async_provider_exact_patch_repair_rejects_siblings_and_applies_once() {
    Box::pin(qualify(false, false, SummaryReply::Valid)).await;
}

/// Repeated invalid decoded patches, or a malformed outer response followed by
/// invalid patches, spend the same canonical repair capacity. Exactly two
/// corrective exchanges precede one response-only failure summary; the real
/// actor settles failed with no sibling/file effects and conserved mock usage.
#[tokio::test(flavor = "current_thread")]
async fn async_provider_exact_patch_exhaustion_leaves_all_effects_absent() {
    for malformed_outer in [false, true] {
        Box::pin(qualify(true, malformed_outer, SummaryReply::Valid)).await;
    }
}

/// Failure-summary parsing or response-only validation has a separate bounded
/// repair phase. Its billed observations must survive both successful summary
/// repair and failed summary exhaustion, without dispatching hostile actions.
#[tokio::test(flavor = "current_thread")]
async fn async_provider_exact_patch_summary_repair_conserves_all_observed_expense() {
    for reply in [
        SummaryReply::MalformedJson,
        SummaryReply::HostileBatch,
        SummaryReply::ExhaustedBatch,
    ] {
        Box::pin(qualify(true, false, reply)).await;
    }
}

/// Synthetic summary behavior, never a fabricated actor completion.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SummaryReply {
    /// An ordinary valid response-only summary.
    Valid,
    /// Invalid JSON followed by one valid summary correction.
    MalformedJson,
    /// A forbidden action batch followed by one valid summary correction.
    HostileBatch,
    /// Two forbidden action batches exhaust the summary-only repair phase.
    ExhaustedBatch,
}

/// Owns one isolated actor/provider/persistence pipeline under a finite deadline.
/// Auxiliary title generation is disabled so every observed HTTP exchange has
/// the admitted action or correction purpose, not a coincidental title request.
async fn qualify(exhausted: bool, malformed_outer: bool, summary_reply: SummaryReply) {
    let root = std::env::temp_dir().join(format!(
        "mez-real-patch-repair-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let correction_seen = StdArc::new(tokio::sync::Notify::new());
    let correction_release = StdArc::new(tokio::sync::Notify::new());
    let final_seen = StdArc::new(tokio::sync::Notify::new());
    let server = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let first = async_provider_concurrency_read_http_request(&mut stream).await;
        assert!(first.contains("repair the complete patch batch"));
        let first_body: serde_json::Value =
            serde_json::from_str(first.split_once("\r\n\r\n").unwrap().1).unwrap();
        let invalid = serde_json::json!({
            "rationale":"invalid middle patch must reject all actions",
            "actions":[
                {"type":"shell_command","summary":"first invalid sibling","command":"printf first >> forbidden-first"},
                {"type":"apply_patch","patch":"*** Begin Patch\n*** Add File: corrected.txt\n+must not apply\n"},
                {"type":"shell_command","summary":"last invalid sibling","command":"printf last >> forbidden-last"}
            ]
        }).to_string();
        async_provider_concurrency_write_chat_content_response(
            &mut stream,
            "local-chat-model",
            if malformed_outer { "{" } else { &invalid },
        )
        .await;
        drop(stream);
        let (mut stream, _) = listener.accept().await.unwrap();
        let repair = async_provider_concurrency_read_http_request(&mut stream).await;
        let body: serde_json::Value =
            serde_json::from_str(repair.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["model"], first_body["model"]);
        assert_eq!(body["response_format"], first_body["response_format"]);
        if malformed_outer {
            assert!(repair.contains("provider MAAP output is malformed"));
        } else {
            assert!(
                repair.contains("apply_patch Mezzanine patch blocks must end with *** End Patch")
            );
        }
        correction_seen.notify_one();
        correction_release.notified().await;
        if exhausted {
            async_provider_concurrency_write_chat_content_response(
                &mut stream,
                "local-chat-model",
                &invalid,
            )
            .await;
            drop(stream);
            let (mut stream, _) = listener.accept().await.unwrap();
            let second_repair = async_provider_concurrency_read_http_request(&mut stream).await;
            assert!(
                second_repair
                    .contains("apply_patch Mezzanine patch blocks must end with *** End Patch")
            );
            async_provider_concurrency_write_chat_content_response(
                &mut stream,
                "local-chat-model",
                &invalid,
            )
            .await;
            drop(stream);
            let (mut stream, _) = listener.accept().await.unwrap();
            let summary = async_provider_concurrency_read_http_request(&mut stream).await;
            assert!(summary.contains("[failure summary state]"));
            assert!(!root.join("corrected.txt").exists());
            assert!(!root.join("forbidden-first").exists());
            assert!(!root.join("forbidden-last").exists());
            if summary_reply != SummaryReply::Valid {
                let hostile = serde_json::json!({"rationale":"must remain response-only", "actions":[{"type":"shell_command","summary":"must not execute summary","command":"printf forbidden > forbidden-summary"}]}).to_string();
                async_provider_concurrency_write_chat_content_response(
                    &mut stream,
                    "local-chat-model",
                    if summary_reply == SummaryReply::MalformedJson {
                        "{"
                    } else {
                        &hostile
                    },
                )
                .await;
                drop(stream);
                let (mut stream, _) = listener.accept().await.unwrap();
                let summary_repair =
                    async_provider_concurrency_read_http_request(&mut stream).await;
                assert!(summary_repair.contains("[failure summary state]"));
                final_seen.notify_one();
                if summary_reply == SummaryReply::ExhaustedBatch {
                    async_provider_concurrency_write_chat_content_response(
                        &mut stream,
                        "local-chat-model",
                        &hostile,
                    )
                    .await;
                } else {
                    async_provider_concurrency_write_chat_response(
                        &mut stream,
                        "failed bounded patch",
                    )
                    .await;
                }
                return;
            }
            final_seen.notify_one();
            async_provider_concurrency_write_chat_response(&mut stream, "failed bounded patch")
                .await;
            return;
        }
        let corrected = serde_json::json!({
            "rationale":"replace the entire rejected batch",
            "actions":[{"type":"apply_patch","patch":"*** Begin Patch\n*** Add File: corrected.txt\n+corrected once\n*** End Patch"}]
        }).to_string();
        async_provider_concurrency_write_chat_content_response(
            &mut stream,
            "local-chat-model",
            &corrected,
        )
        .await;
        drop(stream);
        let (mut stream, _) = listener.accept().await.unwrap();
        let after_patch = async_provider_concurrency_read_http_request(&mut stream).await;
        assert!(after_patch.contains("apply_patch"));
        assert_eq!(
            std::fs::read(root.join("corrected.txt")).unwrap(),
            b"corrected once\n"
        );
        assert!(!root.join("forbidden-first").exists());
        assert!(!root.join("forbidden-last").exists());
        final_seen.notify_one();
        async_provider_concurrency_write_chat_response(&mut stream, "completed corrected patch")
            .await;
    };
    let mut service = test_service();
    service.replace_config_layers(vec![ConfigLayer {
        name:"real-patch-repair".into(), path:None, format:ConfigFormat::Toml,
        scope:ConfigScope::Primary, trusted:true,
        text:format!("[agents]\nshell_mode=\"native\"\nrouting=false\nsession_title_policy=\"objective\"\ndefault_provider=\"local-chat\"\ndefault_model_profile=\"default\"\n[permissions]\nsandbox=\"policy-only\"\n[providers.local-chat]\nkind=\"openai-compatible\"\nbase_url=\"http://{address}/v1\"\nmodels=[\"local-chat-model\"]\ndefault_model=\"local-chat-model\"\n[providers.local-chat.options]\nmaap_output=\"structured_json\"\nstructured_output=\"json_schema\"\n[model_profiles.default]\nprovider=\"local-chat\"\nmodel=\"local-chat-model\"\ncontext_window_tokens=128000\n"),
    }]).unwrap();
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root.join("auth")),
    ));
    let store = AgentTranscriptStore::new(root.join("transcripts"));
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .start_initial_pane_process_with_launch_context(Some("cat"), Some(&root), None)
        .unwrap();
    service.set_pane_current_working_directory("%1", root.clone());
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let stop = StdArc::new(AtomicBool::new(false));
    let stopped = StdArc::new(tokio::sync::Notify::new());
    let provider = async {
        let report = run_async_agent_provider_service(
            &handle,
            AsyncAgentProviderServiceConfig::new(1)
                .unwrap()
                .with_idle_interval(Duration::from_millis(5))
                .unwrap(),
            |_, state| {
                stop.load(Ordering::SeqCst) || matches!(state, RuntimeLifecycleState::Stopping)
            },
        )
        .await
        .unwrap();
        stopped.notify_one();
        report
    };
    let client = async {
        handle
            .execute_agent_shell_command(primary.clone(), "repair the complete patch batch".into())
            .await
            .unwrap();
        correction_seen.notified().await;
        assert!(!root.join("corrected.txt").exists());
        assert!(!root.join("forbidden-first").exists());
        assert!(!root.join("forbidden-last").exists());
        let config = handle
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .await
            .unwrap();
        assert!(config.frame_context.panes.contains_key("%1"));
        correction_release.notify_one();
        final_seen.notified().await;
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
            if summary_reply == SummaryReply::ExhaustedBatch
                || view.lines.iter().any(|line| {
                    line.contains(if exhausted {
                        "failed bounded patch"
                    } else {
                        "completed corrected patch"
                    })
                })
            {
                run_async_persistence_side_effect_service(
                    &handle,
                    AsyncRuntimeSideEffectServiceConfig {
                        max_polls: 4,
                        drain_limit: 64,
                        idle_interval: Duration::from_millis(1),
                    },
                    |_, _| false,
                )
                .await
                .unwrap();
                if !handle.agent_turn_is_running("turn-1").await.unwrap() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        stop.store(true, Ordering::SeqCst);
        stopped.notified().await;
        handle.shutdown().await.unwrap();
    };
    let ((), report, (), mut exit) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(client, provider, server, actor.run())
    })
    .await
    .unwrap();
    assert_eq!(report.executions, if exhausted { 1 } else { 2 });
    assert_eq!(exit.service.agent_turn_ledger().turns().len(), 1);
    assert_eq!(
        exit.service.agent_turn_ledger().turns()[0].state,
        if exhausted {
            mez_agent::AgentTurnState::Failed
        } else {
            mez_agent::AgentTurnState::Completed
        }
    );
    let entries = store.inspect(&conversation).unwrap();
    let results = entries
        .iter()
        .filter_map(|entry| {
            mez_agent::TranscriptContextEvent::from_transcript_content(&entry.content)
        })
        .filter_map(|event| match event {
            mez_agent::TranscriptContextEvent::ExecutionBlock {
                source: mez_agent::ContextSourceKind::ActionResult,
                content,
                ..
            } => Some(content),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        results
            .iter()
            .filter(|content| content.contains("apply_patch") && content.contains("succeeded"))
            .count(),
        if exhausted { 0 } else { 1 }
    );
    assert!(
        results
            .iter()
            .all(|content| !content.contains("forbidden-first")
                && !content.contains("forbidden-last"))
    );
    let usage = exit
        .service
        .agent_token_usage_for_conversation(&conversation);
    assert_eq!(
        exit.service
            .agent_latest_request_usage(&conversation)
            .map(|sample| sample.usage.input_tokens),
        Some(3)
    );
    assert_eq!(
        exit.service
            .agent_context_usage_snapshot(&conversation)
            .map(|sample| sample.input_tokens),
        Some(3)
    );
    assert_eq!(
        usage.values().map(|usage| usage.input_tokens).sum::<u64>(),
        if summary_reply != SummaryReply::Valid {
            15
        } else if exhausted {
            12
        } else {
            9
        },
        "exhausted={exhausted} malformed_outer={malformed_outer}"
    );
    assert_eq!(
        usage.values().map(|usage| usage.output_tokens).sum::<u64>(),
        if summary_reply != SummaryReply::Valid {
            10
        } else if exhausted {
            8
        } else {
            6
        }
    );
    if exhausted {
        assert!(!root.join("corrected.txt").exists());
    } else {
        assert_eq!(
            std::fs::read(root.join("corrected.txt")).unwrap(),
            b"corrected once\n"
        );
    }
    assert!(!root.join("forbidden-first").exists());
    assert!(!root.join("forbidden-last").exists());
    assert!(!root.join("forbidden-summary").exists());
    exit.service.terminate_all_pane_processes().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
