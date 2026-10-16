//! Fixed-allowance convergence through real actor leases and canonical OpenAI
//! Responses HTTP construction. All traffic and synthetic auth are local to
//! this fixture; no user configuration, credentials or live provider is used.

use super::*;
use tokio::io::AsyncWriteExt;

/// An ordinary context rejection must freeze auxiliary work, reject oversized
/// completed output, accept a summary above halved guidance but within ORIGINAL
/// authority, pass full fit and dispatch exactly one ordinary continuation.
/// Every captured canonical OpenAI body omits max_output_tokens, with and
/// without an internal allocation and in streaming and nonstreaming modes.
#[tokio::test(flavor = "current_thread")]
async fn async_openai_compaction_converges_against_original_allowance() {
    for stream in [false, true] {
        for allocation in [false, true] {
            Box::pin(qualify(false, stream, allocation)).await;
        }
    }
}

/// Manual preparation freezes words rather than borrowing provider output
/// tokens. The same correction/wire owner must publish one complete epoch
/// without dispatching an ordinary turn, in both stream/allocation modes.
#[tokio::test(flavor = "current_thread")]
async fn async_openai_manual_compaction_keeps_frozen_word_allowance() {
    for stream in [false, true] {
        for allocation in [false, true] {
            Box::pin(qualify(true, stream, allocation)).await;
        }
    }
}

/// Owns the actor, worker, HTTP endpoint and response gate under one deadline.
/// Publication and archived source are observed independently of worker output.
async fn qualify(manual: bool, streaming: bool, allocation: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let root = std::env::temp_dir().join(format!("mez-openai-convergence-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    let store = AgentTranscriptStore::new(root.join("transcripts"));
    let auth = crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root));
    if streaming {
        // Existing ChatGPT route selects streaming. Synthetic non-expiring
        // credentials are fixture-owned and never leave the loopback endpoint.
        auth.login_openai_provider_credential("default", crate::security::auth::OpenAiProviderCredential {
            api_key: "synthetic-loopback-only".into(),
            refresh_token: None,
            account_id: Some("synthetic-account".into()),
            organization_id: None,
            token_expires_at: None,
        }, &auth.file_credential_store("openai").unwrap()).unwrap();
    }
    let mut service = test_service();
    service.set_agent_transcript_store(store.clone());
    service.set_auth_store(auth);
    let cap = if allocation { "max_output_tokens=12000\n" } else { "" };
    service.replace_config_layers(vec![ConfigLayer {
        name: "canonical-convergence".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: format!("[agents]\ndefault_provider=\"openai\"\ndefault_model_profile=\"default\"\nsession_title_policy=\"objective\"\n[providers.openai]\nkind=\"openai\"\nbase_url=\"http://{address}/v1/responses\"\nmodels=[\"gpt-test\"]\ndefault_model=\"gpt-test\"\n[model_profiles.default]\nprovider=\"openai\"\nmodel=\"gpt-test\"\ncontext_window_tokens=40000\nmax_input_tokens=20000\n{cap}"),
    }]).unwrap();
    let primary = service.attach_primary("primary", true, Size::new(100, 30).unwrap(), 120).unwrap();
    let conversation = service.agent_shell_store_mut().enter_or_resume("%1").unwrap().session_id.clone();
    for sequence in 1..=3 {
        let group = mez_agent::ContextExecutionGroupId::new(format!("canonical-source-{sequence}")).unwrap();
        store.append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: conversation.clone(), sequence, created_at_unix_seconds: sequence,
            role: mez_agent::transcript::TranscriptRole::System, turn_id: format!("old-{sequence}"),
            agent_id: "agent-%1".into(), pane_id: "%1".into(),
            content: mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                mez_agent::ContextSourceKind::TranscriptAssistant, "closed evidence",
                format!("CANONICAL_FROZEN_SOURCE_{sequence} {}", "evidence ".repeat(1000)), group, 1, None,
            ).unwrap().to_transcript_content(),
        }).unwrap();
    }
    let archived = store.inspect(&conversation).unwrap();
    service.agent_shell_store_mut().bind_conversation("%1", &conversation, 3).unwrap();
    service.execute_agent_shell_command(&primary, if manual { "/compact" } else { "continue with frozen evidence" }).unwrap();
    let initial_effect = if manual {
        RuntimeSideEffect::DispatchAgentCompaction {
            pane_id: "%1".into(),
            task_generation: service.pending_agent_compaction_task_generation("%1").unwrap(),
        }
    } else {
        RuntimeSideEffect::DispatchAgentProvider {
            agent_id: AgentId::opaque("agent-%1").unwrap(), turn_id: "turn-1".into(),
        }
    };
    let started = StdArc::new(tokio::sync::Notify::new());
    let release = StdArc::new(tokio::sync::Notify::new());
    let server = async {
        let mut requests = Vec::new();
        if !manual {
            let (mut socket, _) = listener.accept().await.unwrap();
            requests.push(read_body(&mut socket, streaming).await);
            async_provider_concurrency_write_chat_error_response(&mut socket, 400,
                r#"{"error":{"message":"context length exceeded","code":"context_length_exceeded"}}"#).await;
        }
        let (mut socket, _) = listener.accept().await.unwrap();
        let body = read_body(&mut socket, streaming).await;
        assert!(body.to_string().contains("CANONICAL_FROZEN_SOURCE_1"));
        let text = body.to_string();
        let budget: usize = text.split_once("within ").unwrap().1.split_whitespace().next().unwrap().parse().unwrap();
        assert!(budget > 8);
        let invalid = if manual { "INVALID ".repeat(budget + 1) } else { "abcd".repeat(budget + 1) };
        requests.push(body);
        respond(&mut socket, &invalid, streaming).await;
        let corrected = if manual {
            "VALID ".repeat(budget / 2 + 1).trim().to_string()
        } else {
            "abcd".repeat(budget / 2 + 1)
        };
        mez_agent::context_compaction::validate_model_compaction_summary(&corrected, budget, !manual).unwrap();
        let corrected_cost = if manual { mez_agent::model_context_text_word_count(&corrected) }
            else { mez_agent::provider_text_input_token_estimate(&corrected) };
        assert!(corrected_cost > budget / 2);
        let (mut socket, _) = listener.accept().await.unwrap();
        let body = read_body(&mut socket, streaming).await;
        assert!(body.to_string().contains("[private summary correction]"));
        assert!(!body.to_string().contains("INVALID INVALID"));
        requests.push(body);
        started.notify_one();
        release.notified().await;
        respond(&mut socket, &corrected, streaming).await;
        if !manual {
            let (mut socket, _) = listener.accept().await.unwrap();
            let body = read_body(&mut socket, streaming).await;
            assert!(body.to_string().contains(&corrected));
            requests.push(body);
            respond(&mut socket, "ordinary canonical continuation completed", streaming).await;
        }
        (requests, corrected)
    };
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service).build().unwrap();
    let stop = StdArc::new(AtomicBool::new(false));
    let stopped = StdArc::new(tokio::sync::Notify::new());
    let provider = async {
        let report = run_async_agent_provider_service(&handle,
            AsyncAgentProviderServiceConfig::new(1).unwrap().with_idle_interval(Duration::from_millis(5)).unwrap(),
            |_, state| stop.load(Ordering::SeqCst) || matches!(state, RuntimeLifecycleState::Stopping),
        ).await.unwrap();
        stopped.notify_one();
        report
    };
    let client = async {
        handle.queue_runtime_side_effects(vec![initial_effect]).await.unwrap();
        started.notified().await;
        assert!(store.compaction_epoch(&conversation).unwrap().is_none());
        assert!(handle.pending_agent_provider_tasks().await.unwrap().is_empty());
        release.notify_one();
        loop {
            let view = handle.render_client_view(ClientViewRole::Primary, Size::new(100, 30).unwrap(),
                TerminalClientLoopConfig::default()).await.unwrap().unwrap();
            if view.lines.iter().any(|line| line.contains(if manual { "compacted conversation summary" }
                else { "ordinary canonical continuation completed" })) { break; }
            tokio::task::yield_now().await;
        }
        stop.store(true, Ordering::SeqCst);
        stopped.notified().await;
        run_async_persistence_side_effect_service(&handle, AsyncRuntimeSideEffectServiceConfig {
            max_polls: 8, drain_limit: 64, idle_interval: Duration::from_millis(1),
        }, |_, _| false).await.unwrap();
        handle.shutdown().await.unwrap();
    };
    let ((), report, (requests, corrected), exit) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(Box::pin(client), Box::pin(provider), Box::pin(server), actor.run())
    }).await.unwrap();
    assert_eq!(requests.len(), if manual { 2 } else { 4 });
    assert_eq!(report.executions, if manual { 2 } else { 3 });
    assert!(!exit.service.agent_is_compacting("%1"));
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert!(exit.service.pending_agent_compaction_tasks().is_empty());
    let epoch = store.compaction_epoch(&conversation).unwrap().unwrap();
    assert!(epoch.summary.contains(&corrected));
    assert_eq!(&store.inspect(&conversation).unwrap()[..archived.len()], archived.as_slice());
    if !manual {
        assert_eq!(exit.service.agent_turn_ledger().turn("turn-1").unwrap().state, mez_agent::AgentTurnState::Completed);
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// Captures and checks the actual canonical HTTP body, not ModelRequest fields.
async fn read_body(socket: &mut tokio::net::TcpStream, streaming: bool) -> serde_json::Value {
    let request = async_provider_concurrency_read_http_request(socket).await;
    let body: serde_json::Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert!(body.get("max_output_tokens").is_none());
    assert_eq!(body["stream"], streaming);
    body
}

/// Supplies complete framed MAAP through real Responses JSON/SSE parsing.
async fn respond(socket: &mut tokio::net::TcpStream, text: &str, streaming: bool) {
    let batch = serde_json::json!({"rationale":"complete fixture output", "actions":[{
        "type":"say", "status":"final", "content_type":"text/plain; charset=utf-8", "text":text,
    }]}).to_string();
    let response = serde_json::json!({"id":"fixture-response", "model":"gpt-test", "status":"completed",
        "output":[{"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":batch}]}],
        "usage":{"input_tokens":100,"output_tokens":10}});
    let (content_type, body) = if streaming {
        ("text/event-stream", format!("event: response.completed\ndata: {}\n\n", serde_json::json!({"type":"response.completed", "response":response})))
    } else { ("application/json", response.to_string()) };
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    socket.shutdown().await.unwrap();
}
