//! Saved-fork continuity at the actual first provider encoding boundary.
//!
//! All history is synthetic. These tests run the real fork command and reopen
//! durable storage without sending provider requests or replaying any actions.

use super::*;

mod fixture_cleanup;

/// Fork-only asynchronous control admission must not defer unrelated inline
/// status reads or bypass the existing visible-shell and attached-primary gates.
/// Rejected requests leave no worker claim, target archive, or source mutation.
#[test]
fn runtime_saved_fork_control_keeps_inline_status_and_admission_guards() {
    for denied in ["hidden", "detached"] {
        let (mut service, primary, store, root) = fork_transaction_fixture("fork-control-guards");
        let status = service.dispatch_runtime_control_body(
            r#"{"jsonrpc":"2.0","id":"inline-status","method":"agent/shell/command","params":{"idempotency_key":"inline-status","input":"/status"}}"#, &primary);
        let status: serde_json::Value = serde_json::from_str(&status).unwrap();
        assert_eq!(status["result"]["kind"], "display");
        assert!(status["result"]["body"].is_string());
        assert!(service.take_pending_deferred_agent_commands().is_empty());
        let before = store.inspect("fork-source").unwrap();
        if denied == "hidden" {
            service.agent_shell_store_mut().request_exit("%1").unwrap();
        } else {
            service.session.detach_primary(&primary).unwrap();
        }
        let response = service.dispatch_runtime_control_body(
            r#"{"jsonrpc":"2.0","id":"denied-fork","method":"agent/shell/command","params":{"idempotency_key":"denied-fork","input":"/fork denied-target"}}"#, &primary);
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(response["error"].is_object(), "{denied}: {response}");
        assert!(service.take_pending_deferred_agent_commands().is_empty());
        assert!(!root.join("denied-target").exists());
        assert_eq!(store.inspect("fork-source").unwrap(), before);
        service.terminate_all_pane_processes().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}

/// Paused source persistence must fail closed, whether the row remains queued,
/// was claimed by a worker, exists only as an accepted receipt, or is known by
/// the actor highwater without a materialized archive row. After the original
/// owner settles, the actual command's first encoding includes the exact suffix
/// once and the inherited summary without changing the source on rejection.
#[test]
fn runtime_saved_fork_waits_for_accepted_source_chronology() {
    for state in ["queued", "claimed", "receipt", "highwater"] {
        let (mut service, primary, store, root) = fork_transaction_fixture("fork-paused-source");
        let row = TranscriptEntry {
            sequence: 2,
            content: "FORK_ROW_ACCEPTED_SOURCE_SUFFIX".into(),
            ..store.inspect("fork-source").unwrap()[0].clone()
        };
        let path = store.transcript_path("fork-source").unwrap();
        let before = fs::read(&path).unwrap();
        if matches!(state, "queued" | "claimed") {
            service
                .persistence
                .queue_transcript(RuntimeSideEffect::PersistTranscriptEntries {
                    path: path.clone(),
                    store: store.clone(),
                    entries: vec![row.clone()],
                });
            if state == "claimed" {
                assert_eq!(service.persistence.take_transcript_effects().len(), 1);
            }
        } else if state == "receipt" {
            store
                .accept_append_receipt(std::slice::from_ref(&row), 1)
                .unwrap();
        }
        service
            .agent_shell_store_mut()
            .bind_conversation("%1", "fork-source", 2)
            .unwrap();
        service
            .execute_agent_shell_command(&primary, "/fork fork-target")
            .unwrap();
        let rejected = service
            .run_pending_deferred_agent_command_for_tests()
            .unwrap();
        if matches!(state, "queued" | "claimed") {
            assert!(rejected.is_none());
        } else {
            assert!(rejected.unwrap().contains("error"), "{state}");
        }
        assert!(!root.join("fork-target").exists(), "{state}");
        assert_eq!(fs::read(&path).unwrap(), before, "{state}");
        assert_eq!(
            service.agent_command_lifecycle_phase_for_tests("%1"),
            Some(crate::runtime::RuntimeAgentCommandLifecyclePhase::Failed)
        );
        if state == "receipt" {
            store.recover_append_receipts().unwrap();
        } else {
            store.append(&row).unwrap();
            if state == "queued" {
                service.persistence.take_transcript_effects();
            }
            if matches!(state, "queued" | "claimed") {
                assert!(service.persistence.settle_transcript_write_at_path(
                    "fork-source",
                    2,
                    &path,
                    std::slice::from_ref(&row)
                ));
            }
        }
        service
            .execute_agent_shell_command(&primary, "/fork fork-target")
            .unwrap();
        let forked = service
            .run_pending_deferred_agent_command_for_tests()
            .unwrap()
            .unwrap();
        assert!(forked.contains("forked=true"), "{state}: {forked}");
        let pane = service
            .agent_shell_store()
            .sessions()
            .find(|session| session.session_id == "fork-target")
            .unwrap()
            .pane_id
            .clone();
        assert_eq!(
            encoded_history(&mut service, &pane),
            vec![
                ("developer".into(), "FORK_SUMMARY".into()),
                ("user".into(), "FORK_ROW_ACCEPTED_SOURCE_SUFFIX".into()),
            ],
            "{state}"
        );
        assert_eq!(store.inspect("fork-source").unwrap().last(), Some(&row));
        fixture_cleanup::settle_and_remove(&mut service, &root, |_| Ok(())).unwrap();
    }
}

/// A reader stopped immediately before taking the target lease must re-read its
/// discovery/history after compensation, not adopt a deleted fork from an earlier
/// snapshot. The pre-lock handshake also fails on the old read-before-lock order.
#[test]
fn runtime_saved_fork_waiting_resume_rechecks_after_compensation() {
    let (mut service, primary, store, root) = fork_transaction_fixture("fork-waiting-resume");
    let work = claim_saved_fork(&mut service, &primary);
    let read = publish_saved_fork(&work);
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    store.gate_next_presentation_lock_for_tests(observed_tx, resume_rx);
    let reader_store = store.clone();
    let reader = std::thread::spawn(move || {
        crate::runtime::commands::read_direct_resume(&reader_store, "fork-target", None)
    });
    observed_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    service.agent.cancel_agent_command("%1");
    let reply = service.complete_agent_command_work(
        &work,
        crate::runtime::RuntimeAgentCommandAsyncOutcome::SavedFork(read.clone()),
    );
    assert!(!reply.as_ref().unwrap());
    RuntimeSessionService::finish_saved_fork_worker(&read, reply);
    resume_tx.send(()).unwrap();
    assert!(
        reader.join().unwrap().is_err(),
        "deleted target must not produce an adoptable resume snapshot"
    );
    assert!(store.saved_session("fork-target").unwrap().is_none());
    assert_eq!(store.inspect("fork-source").unwrap().len(), 1);
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Creates an owned real-shell fixture with a compacted synthetic source; no
/// provider, credential or user conversation participates. Its root is exclusive
/// to this generation, never adopted by deleting an existing PID-named tree.
fn fork_transaction_fixture(
    label: &str,
) -> (
    RuntimeSessionService,
    mez_core::ids::ClientId,
    AgentTranscriptStore,
    std::path::PathBuf,
) {
    let root = fixture_cleanup::exclusive_root(label).unwrap();
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&TranscriptEntry {
            conversation_id: "fork-source".into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "source-turn".into(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content: "FORK_ROW_001".into(),
        })
        .unwrap();
    store
        .save_compaction_epoch("fork-source", 1, "FORK_SUMMARY")
        .unwrap();
    let mut service = test_runtime_service();
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
        .bind_conversation("%1", "fork-source", 1)
        .unwrap();
    service.checkpoint_agent_session_metadata().unwrap();
    (service, primary, store, root)
}

/// Claims the real deferred command but leaves its two-phase worker settlement
/// under test control so rebinds, dropped replies and cleanup faults are causal.
fn claim_saved_fork(
    service: &mut RuntimeSessionService,
    primary: &mez_core::ids::ClientId,
) -> crate::runtime::RuntimeAgentCommandAsyncWork {
    let acknowledgement = service
        .execute_agent_shell_command(primary, "/fork fork-target")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&acknowledgement).unwrap(),
        serde_json::json!({"pane_id":"%1", "input":"/fork fork-target", "kind":"mutated", "command":"fork", "body":null, "turn":null})
    );
    let dispatch = service
        .take_pending_deferred_agent_commands()
        .pop()
        .unwrap();
    service
        .claim_agent_command_work(
            &dispatch.primary_client_id,
            &dispatch.pane_id,
            &dispatch.command,
            &dispatch.input,
            dispatch.claim_generation,
            &dispatch.conversation_id,
        )
        .unwrap()
        .unwrap()
}

/// Executes the actual worker publication and returns its sole adoption receipt.
/// This lease must remain on the worker until actor adoption or compensation.
fn publish_saved_fork(
    work: &crate::runtime::RuntimeAgentCommandAsyncWork,
) -> std::sync::Arc<crate::runtime::commands::SavedForkRead> {
    let crate::runtime::RuntimeAgentCommandAsyncOutcome::SavedFork(read) =
        RuntimeSessionService::execute_deferred_agent_command(work)
    else {
        panic!("saved fork worker must publish a typed receipt")
    };
    read
}

/// Saved MCP retrievals remain inert evidence. Their durable invalidation epoch
/// survives compaction/fork/reopen; it cannot resurrect stale manifests, execute
/// tools or transfer source usage/permissions into the new conversation identity.
#[test]
fn runtime_saved_fork_mcp_invalidation_keeps_usage_and_authority_independent() {
    let (mut service, primary, store, root) = fork_transaction_fixture("fork-mcp-accounting");
    let group = mez_agent::ContextExecutionGroupId::new("saved-mcp-group").unwrap();
    let contents = [
        mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            ContextSourceKind::TranscriptAssistant,
            "assistant",
            "retrieved a historical manifest",
            group.clone(),
            1,
            None,
        )
        .unwrap()
        .to_transcript_content(),
        mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            ContextSourceKind::McpRetrievedManifest,
            "manifest",
            "OBSOLETE_FORK_MCP_MANIFEST",
            group,
            2,
            None,
        )
        .unwrap()
        .to_transcript_content(),
        mez_agent::TranscriptContextEvent::McpCompactionEpoch.to_transcript_content(),
    ];
    for (index, content) in contents.into_iter().enumerate() {
        store
            .append(&TranscriptEntry {
                conversation_id: "fork-source".into(),
                sequence: index as u64 + 2,
                created_at_unix_seconds: 2,
                role: TranscriptRole::System,
                turn_id: "mcp-turn".into(),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content,
            })
            .unwrap();
    }
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "fork-source", 4)
        .unwrap();
    let historical_actions = mez_agent::AllowedActionSet::from_actions([
        mez_agent::AllowedAction::Say,
        mez_agent::AllowedAction::ShellCommand,
    ]);
    service.set_agent_enabled_actions(historical_actions.clone());
    assert_eq!(
        service
            .capture_agent_session_allowed_actions_for_pane("%1")
            .unwrap(),
        historical_actions
    );
    service.set_agent_enabled_actions(mez_agent::AllowedActionSet::say_only());
    let (_, profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    let usage = mez_agent::ModelTokenUsage {
        input_tokens: 100,
        output_tokens: 20,
        reasoning_tokens: 3,
        cached_input_tokens: Some(10),
        cache_write_input_tokens: None,
    };
    service.record_agent_provider_token_usage_with_profile("%1", usage, usage, Some(&profile));
    let source_usage = service.agent_token_usage_for_conversation("fork-source");
    assert!(!source_usage.is_empty());
    let parent_lineage = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .prompt_cache_lineage_id
        .clone();
    let parent_projection = encoded_history(&mut service, "%1");
    service
        .execute_agent_shell_command(&primary, "/fork fork-target")
        .unwrap();
    let settled = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(settled.contains("forked=true"), "{settled}");
    let target = service
        .agent_shell_store()
        .sessions()
        .find(|session| session.session_id == "fork-target")
        .unwrap()
        .clone();
    assert_eq!(
        encoded_history(&mut service, &target.pane_id),
        parent_projection
    );
    let context = service
        .agent_context_for_pane_prompt(&target.pane_id, "continue", 0)
        .unwrap();
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.content.contains("OBSOLETE_FORK_MCP_MANIFEST"))
    );
    assert!(
        service
            .agent_token_usage_for_conversation("fork-target")
            .is_empty()
    );
    assert_eq!(
        service.agent_token_usage_for_conversation("fork-source"),
        source_usage
    );
    assert_ne!(target.prompt_cache_lineage_id, parent_lineage);
    assert_eq!(
        target.allowed_actions,
        Some(mez_agent::AllowedActionSet::say_only())
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .allowed_actions,
        Some(historical_actions)
    );
    assert_eq!(
        target.conversation_kind,
        mez_agent::AgentConversationKind::Root
    );
    assert!(
        service
            .subagent_lineage(&format!("agent-{}", target.pane_id))
            .is_none()
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(store.inspect("fork-target").unwrap().len(), 4);
    fixture_cleanup::settle(&mut service).unwrap();
    let mut reopened = test_runtime_service();
    reopened.set_agent_transcript_store(AgentTranscriptStore::new(root.clone()));
    let primary = reopened
        .attach_primary("reopened", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    reopened
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    reopened
        .execute_agent_shell_command(&primary, "/resume fork-target")
        .unwrap();
    reopened
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    let context = reopened
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.content.contains("OBSOLETE_FORK_MCP_MANIFEST"))
    );
    assert!(
        reopened
            .agent_token_usage_for_conversation("fork-target")
            .is_empty()
    );
    assert!(reopened.pending_agent_provider_tasks().is_empty());
    fixture_cleanup::settle_and_remove(&mut reopened, &root, |_| Ok(())).unwrap();
}

/// A stale source binding, cancelled claim, changed store, deadline cancellation
/// or detached primary can never adopt a worker's published target. The original
/// worker compensates its own generation, leaving source and foreign store intact.
#[test]
fn runtime_saved_fork_stale_adoption_compensates_only_owned_target() {
    for stale in ["rebind", "cancel", "store", "deadline", "detach"] {
        let (mut service, primary, store, root) = fork_transaction_fixture("fork-stale-adoption");
        let work = claim_saved_fork(&mut service, &primary);
        let read = publish_saved_fork(&work);
        match stale {
            "rebind" => {
                service
                    .agent_shell_store_mut()
                    .bind_conversation("%1", "different", 0)
                    .unwrap();
            }
            "cancel" => {
                service.agent.cancel_agent_command("%1");
            }
            "store" => service
                .set_agent_transcript_store(AgentTranscriptStore::new(root.join("foreign-store"))),
            "deadline" => {
                let crate::runtime::RuntimeAgentCommandPrepared::SavedFork(fork) = &work.prepared
                else {
                    unreachable!()
                };
                fork.cancelled
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            _ => {
                service.session.detach_primary(&primary).unwrap();
            }
        }
        let reply = service.complete_agent_command_work(
            &work,
            crate::runtime::RuntimeAgentCommandAsyncOutcome::SavedFork(read.clone()),
        );
        assert!(!reply.as_ref().unwrap(), "{stale}");
        let outcome = RuntimeSessionService::finish_saved_fork_worker(&read, reply);
        assert!(matches!(
            outcome,
            crate::runtime::RuntimeAgentCommandAsyncOutcome::Failed { .. }
        ));
        assert!(
            store.saved_session("fork-target").unwrap().is_none(),
            "{stale}"
        );
        assert_eq!(store.inspect("fork-source").unwrap().len(), 1);
        service.terminate_all_pane_processes().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}

/// A lost reply is not non-adoption proof. Retain complete history and fence any
/// queued late request permanently; duplicate successful adoption is inert even
/// after the worker consumes its result and the source command is cancelled.
#[test]
fn runtime_saved_fork_lost_and_duplicate_adoption_replies_retain_history() {
    for lost in [true, false] {
        let (mut service, primary, store, root) = fork_transaction_fixture("fork-lost-adoption");
        let work = claim_saved_fork(&mut service, &primary);
        let read = publish_saved_fork(&work);
        if lost {
            let outcome = RuntimeSessionService::finish_saved_fork_worker(
                &read,
                Err(crate::error::MezError::invalid_state(
                    "owned fixture dropped actor reply",
                )),
            );
            assert!(matches!(
                outcome,
                crate::runtime::RuntimeAgentCommandAsyncOutcome::Failed { .. }
            ));
        } else {
            let reply = service.complete_agent_command_work(
                &work,
                crate::runtime::RuntimeAgentCommandAsyncOutcome::SavedFork(read.clone()),
            );
            let outcome = RuntimeSessionService::finish_saved_fork_worker(&read, reply);
            let outcome =
                RuntimeSessionService::project_deferred_agent_command_outcome(&work, outcome)
                    .unwrap();
            service.agent.cancel_agent_command("%1");
            assert!(!service.complete_agent_command_work(&work, outcome).unwrap());
            assert!(
                store
                    .load_agent_session_metadata(service.session().id.as_str())
                    .unwrap()
                    .iter()
                    .any(|metadata| metadata.conversation_id == "fork-target")
            );
        }
        assert!(
            service
                .complete_agent_command_work(
                    &work,
                    crate::runtime::RuntimeAgentCommandAsyncOutcome::SavedFork(read)
                )
                .is_err()
        );
        assert_eq!(store.inspect("fork-target").unwrap().len(), 1);
        assert_eq!(
            store
                .compaction_epoch("fork-target")
                .unwrap()
                .unwrap()
                .summary,
            "FORK_SUMMARY"
        );
        fixture_cleanup::settle_and_remove(&mut service, &root, |_| Ok(())).unwrap();
    }
}

/// Failure after binding covers normal rollback, a retained live pane, and a
/// secondary close error after membership removal. Only proven complete teardown
/// allows deletion; primary and secondary errors survive worker settlement.
#[tokio::test(flavor = "current_thread")]
async fn runtime_saved_fork_post_bind_cleanup_retains_uncertain_targets() {
    for fault in [1, 2, 3] {
        let (mut service, primary, store, root) =
            fork_transaction_fixture("fork-post-bind-cleanup");
        let native = service.native_pane_retirement_owner();
        let original_native = native.outstanding();
        let work = claim_saved_fork(&mut service, &primary);
        let read = publish_saved_fork(&work);
        RuntimeSessionService::fail_saved_fork_adoption_for_tests(&read, fault);
        let reply = service.complete_agent_command_work(
            &work,
            crate::runtime::RuntimeAgentCommandAsyncOutcome::SavedFork(read.clone()),
        );
        // Observe BEFORE compensation or independent fixture cleanup. A bound
        // target never crosses an adapter handoff within synchronous adoption.
        // Native slots clear only on actual wait/reap, not map/JoinHandle removal.
        let native_before_compensation = native.outstanding();
        let returned_before_compensation = native.returned_count();
        let target_before_compensation = root.join("fork-target").is_dir();
        let outcome = RuntimeSessionService::finish_saved_fork_worker(&read, reply);
        let native_after_compensation = native.outstanding();
        let target_retained = store.saved_session("fork-target").unwrap().is_some();
        let source_intact = store.inspect("fork-source").unwrap().len() == 1;
        // All phase observations above precede independent fixture teardown.
        // Attempt every exact cleanup before assertions can unwind a live child.
        let manager_cleanup = service.terminate_all_pane_processes();
        service.release_native_panes_for_retirement();
        native.seal();
        let native_cleanup = crate::host::async_runtime::drain_native_pane_retirement(
            &native,
            tokio::time::Instant::now()
                + crate::host::async_runtime::NATIVE_PANE_RETIREMENT_TIMEOUT,
        )
        .await;
        let remaining = native.outstanding();
        if remaining == 0 {
            fs::remove_dir_all(root).unwrap();
        }
        manager_cleanup.unwrap();
        native_cleanup.unwrap();
        let crate::runtime::RuntimeAgentCommandAsyncOutcome::Failed { message, .. } = outcome
        else {
            panic!("setup must fail")
        };
        assert!(original_native > 0, "fixture must own a real source child");
        assert!(
            target_before_compensation,
            "storage survives until worker compensation"
        );
        assert_eq!(
            native_before_compensation,
            original_native + usize::from(fault == 2)
        );
        assert_eq!(
            returned_before_compensation, 0,
            "bound rollback must not certify a returned child by map absence"
        );
        assert_eq!(native_after_compensation, native_before_compensation);
        assert!(message.contains("post-binding setup failure"), "{message}");
        assert_eq!(target_retained, fault != 1, "{message}");
        if fault != 1 {
            assert!(message.contains("close failure"), "{message}");
            assert!(message.contains("retained"), "{message}");
        }
        assert!(source_intact);
        assert_eq!(remaining, 0);
    }
}

/// A pre-bind returned child is an exact native obligation, not an adopted
/// conversation. Fork compensation may remove only its own unadopted target
/// while that child remains reserved; native completion stays false until the
/// actual returned handle is waited/reaped by the existing bounded drain.
#[tokio::test(flavor = "current_thread")]
async fn runtime_saved_fork_pre_bind_return_keeps_exact_native_obligation() {
    let (mut service, primary, store, root) = fork_transaction_fixture("fork-pre-bind-native");
    let native = service.native_pane_retirement_owner();
    let original_native = native.outstanding();
    let work = claim_saved_fork(&mut service, &primary);
    let read = publish_saved_fork(&work);
    RuntimeSessionService::fail_saved_fork_adoption_for_tests(&read, 4);
    let reply = service.complete_agent_command_work(
        &work,
        crate::runtime::RuntimeAgentCommandAsyncOutcome::SavedFork(read.clone()),
    );
    let before = (
        native.outstanding(),
        native.returned_count(),
        root.join("fork-target").is_dir(),
    );
    let unbound = !service
        .agent_shell_store()
        .sessions()
        .any(|session| session.session_id == "fork-target");
    let expected_pid = RuntimeSessionService::saved_fork_returned_native_pid_for_tests(&read);
    // Wait-only claim observes the already-owned exact child, never a numeric
    // reopen or a new native eligibility capture/signaling attempt.
    let returned_pid = native.begin_drain().ok().and_then(|mut drain| {
        let child = drain.take_returned()?;
        let pid = child.process_id();
        drop(child);
        pid
    });
    let outcome = RuntimeSessionService::finish_saved_fork_worker(&read, reply);
    let after = (
        native.outstanding(),
        native.returned_count(),
        root.join("fork-target").exists(),
    );
    let source_intact = store.inspect("fork-source").unwrap().len() == 1;
    // Attempt every exact cleanup before assertions can unwind the fixture.
    let manager_cleanup = service.terminate_all_pane_processes();
    service.release_native_panes_for_retirement();
    native.seal();
    let native_cleanup = crate::host::async_runtime::drain_native_pane_retirement(
        &native,
        tokio::time::Instant::now() + crate::host::async_runtime::NATIVE_PANE_RETIREMENT_TIMEOUT,
    )
    .await;
    let remaining = native.outstanding();
    if remaining == 0 {
        fs::remove_dir_all(root).unwrap();
    }
    manager_cleanup.unwrap();
    native_cleanup.unwrap();
    assert!(original_native > 0);
    assert_eq!(before, (original_native + 1, 1, true));
    assert!(
        unbound,
        "returned startup child must never acquire the target conversation"
    );
    assert_ne!(expected_pid, 0);
    assert_eq!(
        returned_pid,
        Some(expected_pid),
        "retain the exact original native Child"
    );
    assert!(
        matches!(outcome, crate::runtime::RuntimeAgentCommandAsyncOutcome::Failed { ref message, .. }
        if message.contains("pre-binding native guard return"))
    );
    assert_eq!(
        after,
        (original_native + 1, 1, false),
        "storage compensation cannot certify native completion"
    );
    assert!(source_intact);
    assert_eq!(remaining, 0);
}

/// A secondary storage failure after safe post-bind pane teardown retains a
/// durable recovery intent and both diagnostics. Restart removes only the owned
/// provisional target, never the source or a replacement conversation.
#[test]
fn runtime_saved_fork_secondary_cleanup_error_remains_recoverable() {
    let (mut service, primary, store, root) = fork_transaction_fixture("fork-secondary-error");
    let work = claim_saved_fork(&mut service, &primary);
    let read = publish_saved_fork(&work);
    RuntimeSessionService::fail_saved_fork_adoption_for_tests(&read, 1);
    store.fail_next_fork_cleanup();
    let reply = service.complete_agent_command_work(
        &work,
        crate::runtime::RuntimeAgentCommandAsyncOutcome::SavedFork(read.clone()),
    );
    let crate::runtime::RuntimeAgentCommandAsyncOutcome::Failed { message, .. } =
        RuntimeSessionService::finish_saved_fork_worker(&read, reply)
    else {
        panic!("cleanup must fail")
    };
    assert!(message.contains("post-binding setup failure"), "{message}");
    assert!(
        message.contains("injected fork target cleanup failure"),
        "{message}"
    );
    assert!(store.saved_session("fork-target").unwrap().is_none());
    store.initialize(200).unwrap();
    assert!(!root.join("fork-target").exists());
    assert_eq!(store.inspect("fork-source").unwrap().len(), 1);
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Failure after successful epoch publication but before pane setup completes
/// removes every new target artifact, including its epoch and required marker,
/// while keeping the compacted parent's exact durable bytes and live binding.
#[test]
fn runtime_saved_fork_epoch_setup_failure_removes_only_published_target() {
    let root = temp_root("runtime-fork-epoch-setup-fault");
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&TranscriptEntry {
            conversation_id: "fork-source".into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "source-turn".into(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content: "source prompt".into(),
        })
        .unwrap();
    store
        .save_compaction_epoch("fork-source", 1, "source model summary")
        .unwrap();
    let mut service = test_runtime_service();
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
        .bind_conversation("%1", "fork-source", 1)
        .unwrap();
    service.checkpoint_agent_session_metadata().unwrap();
    let before = fs::read_dir(root.join("fork-source"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (path.clone(), fs::read(path).unwrap())
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let panes = service.session().windows()[0].panes().len();
    service.fail_next_agent_fork_after_persistence_for_tests();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"epoch-setup-fault","method":"agent/shell/command","params":{"idempotency_key":"epoch-setup-fault","input":"/fork fork-target"}}"#,
        &primary,
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&response).unwrap()["result"]["body"],
        serde_json::Value::Null
    );
    assert!(service.agent_command_is_active("%1"));
    let response = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    service.terminate_all_pane_processes().unwrap();
    assert!(
        response.contains("injected agent fork failure after persistence"),
        "{response}"
    );
    assert_eq!(service.session().windows()[0].panes().len(), panes);
    assert!(!root.join("fork-target").exists());
    assert!(store.saved_session("fork-target").unwrap().is_none());
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        "fork-source"
    );
    let after = fs::read_dir(root.join("fork-source"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (path.clone(), fs::read(path).unwrap())
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(after, before);
}

/// Selective epochs without reconstructible terminal witnesses must survive a
/// real fork and the first encoded request after reopen. Inclusive ranges keep
/// their original anchors, user barriers and assistant roles remain ordered,
/// and an uncovered provider-owned native item retains its exact wire shape.
/// Later parent work cannot leak in; later target work appears exactly once.
#[test]
fn runtime_saved_fork_selective_epoch_encoded_anchors_and_native_ownership() {
    use crate::storage::transcript::{AgentCompactionEpoch, AgentCompactionRange};
    for prefix in [0, 1] {
        let root = temp_root("runtime-fork-selective-epoch");
        let store = AgentTranscriptStore::new(root.clone());
        let mut service = test_runtime_service();
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
        let (_, profile) = service
            .active_model_profile_for_pane("%1", "agent-%1", None)
            .unwrap();
        let owner = mez_agent::ProviderContinuityOwner::new(
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
            &profile.provider,
        )
        .unwrap();
        let native_item = serde_json::json!({
            "type":"message", "id":"fork-native-msg", "role":"assistant",
            "content":[{"type":"output_text", "text":"FORK_ROW_010"}]
        });
        let native = mez_agent::ProviderTranscriptEvent::validated_openai_response_output(vec![
            native_item.clone(),
        ])
        .unwrap();
        let rows = (1..=11)
            .map(|sequence| {
                let mut row = TranscriptEntry {
                    conversation_id: "fork-source".into(),
                    sequence,
                    created_at_unix_seconds: sequence,
                    role: if matches!(sequence, 5 | 9) {
                        TranscriptRole::Assistant
                    } else {
                        TranscriptRole::User
                    },
                    turn_id: format!("turn-{sequence}"),
                    agent_id: "agent-%1".into(),
                    pane_id: "%1".into(),
                    content: format!("FORK_ROW_{sequence:03}"),
                };
                if matches!(sequence, 2..=3 | 6..=7 | 10..=11) {
                    row.role = TranscriptRole::System;
                    if sequence >= 10 {
                        // Native continuity requires its neutral assistant twin
                        // before provider output in the same complete group.
                        row.turn_id = "native-turn".into();
                    }
                    let (group, ordinal) = match sequence {
                        2..=3 => ("first", sequence - 1),
                        6..=7 => ("second", sequence - 5),
                        _ => ("native", sequence - 9),
                    };
                    row.content = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                        if sequence == 11 {
                            ContextSourceKind::TranscriptTool
                        } else {
                            ContextSourceKind::TranscriptAssistant
                        },
                        "answer",
                        if sequence == 11 {
                            native.to_transcript_content()
                        } else {
                            row.content
                        },
                        mez_agent::ContextExecutionGroupId::new(group).unwrap(),
                        ordinal,
                        (sequence == 11).then(|| owner.clone()),
                    )
                    .unwrap()
                    .to_transcript_content();
                }
                row
            })
            .collect::<Vec<_>>();
        store.append_many(&rows).unwrap();
        let epoch = AgentCompactionEpoch {
            version: 2,
            conversation_id: "fork-source".into(),
            through_sequence: prefix,
            summary: if prefix == 0 {
                String::new()
            } else {
                "FORK_SUMMARY_PREFIX".into()
            },
            ranges: vec![
                AgentCompactionRange {
                    first_sequence: 2,
                    through_sequence: 3,
                    summary: "FORK_SUMMARY_FIRST".into(),
                },
                AgentCompactionRange {
                    first_sequence: 6,
                    through_sequence: 7,
                    summary: "FORK_SUMMARY_SECOND".into(),
                },
            ],
        };
        let selected = rows
            .iter()
            .filter(|row| matches!(row.sequence, 2..=3 | 6..=7))
            .cloned()
            .collect::<Vec<_>>();
        store
            .save_compaction_ranges_with_proof(epoch, &selected)
            .unwrap();
        let source_path = store.transcript_path("fork-source").unwrap();
        let epoch_path = source_path.parent().unwrap().join("compaction-epoch.json");
        let source_bytes = fs::read(&source_path).unwrap();
        let epoch_bytes = fs::read(&epoch_path).unwrap();
        service
            .agent_shell_store_mut()
            .bind_conversation("%1", "fork-source", 11)
            .unwrap();
        let response = service
            .execute_agent_shell_command(&primary, "/fork fork-target")
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response).unwrap()["body"],
            serde_json::Value::Null
        );
        assert!(service.agent_command_is_active("%1"));
        let response = service
            .run_pending_deferred_agent_command_for_tests()
            .unwrap()
            .unwrap();
        assert!(response.contains("forked=true"), "{response}");
        let pane = service
            .agent_shell_store()
            .sessions()
            .find(|session| session.session_id == "fork-target")
            .unwrap()
            .pane_id
            .clone();
        let actual = encoded_history(&mut service, &pane);
        let body = encoded_body(&mut service, &pane);
        let context = service
            .agent_context_for_pane_prompt(&pane, "continue", 0)
            .unwrap();
        service.terminate_all_pane_processes().unwrap();
        let expected = [
            if prefix == 0 {
                ("user", "FORK_ROW_001")
            } else {
                ("developer", "FORK_SUMMARY_PREFIX")
            },
            ("developer", "FORK_SUMMARY_FIRST"),
            ("user", "FORK_ROW_004"),
            ("assistant", "FORK_ROW_005"),
            ("developer", "FORK_SUMMARY_SECOND"),
            ("user", "FORK_ROW_008"),
            ("assistant", "FORK_ROW_009"),
            ("assistant", "FORK_ROW_010"),
        ]
        .into_iter()
        .map(|(role, text)| (role.to_string(), text.to_string()))
        .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        assert!(
            body["input"].as_array().unwrap().contains(&native_item),
            "{body}"
        );
        let switched =
            encoded_body_with_provider(&mut service, &pane, Some("different-native-owner"));
        assert!(
            !switched["input"].as_array().unwrap().contains(&native_item),
            "{switched}"
        );
        assert!(
            switched.to_string().contains("FORK_ROW_010"),
            "neutral assistant twin survives model/provider switch"
        );
        assert!(
            context
                .chronology()
                .iter()
                .any(|event| event.provider_owner() == Some(&owner))
        );
        assert_eq!(fs::read(&source_path).unwrap(), source_bytes);
        assert_eq!(fs::read(&epoch_path).unwrap(), epoch_bytes);
        let mut reopened = test_runtime_service();
        reopened.set_agent_transcript_store(AgentTranscriptStore::new(root));
        let primary = reopened
            .attach_primary("reopened", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        reopened
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        reopened
            .execute_agent_shell_command(&primary, "/resume fork-target")
            .unwrap();
        let settled = reopened
            .run_pending_deferred_agent_command_for_tests()
            .unwrap()
            .unwrap();
        assert!(settled.contains("resumed=true"), "{settled}");
        assert_eq!(encoded_history(&mut reopened, "%1"), expected);
        assert!(
            encoded_body(&mut reopened, "%1")["input"]
                .as_array()
                .unwrap()
                .contains(&native_item)
        );
        assert!(
            reopened
                .agent_context_for_pane_prompt("%1", "continue", 0)
                .unwrap()
                .chronology()
                .iter()
                .any(|event| event.provider_owner() == Some(&owner))
        );
        let mut later = rows[0].clone();
        later.sequence = 12;
        later.content = "FORK_ROW_PARENT_ONLY".into();
        store.append(&later).unwrap();
        later.conversation_id = "fork-target".into();
        later.content = "FORK_ROW_012".into();
        store.append(&later).unwrap();
        let mut expected = expected;
        expected.push(("user".into(), "FORK_ROW_012".into()));
        assert_eq!(encoded_history(&mut reopened, "%1"), expected);
    }
}

/// A rejected existing-target or source-equals-target request owns no durable
/// target. Runtime compensation must close only its newly created pane and
/// preserve both existing conversations byte-for-byte, including epoch markers.
#[test]
fn runtime_saved_fork_rejected_target_preserves_existing_and_source_bytes() {
    for target in ["fork-source", "existing-target"] {
        let root = temp_root("runtime-fork-existing-target");
        let store = AgentTranscriptStore::new(root.clone());
        for conversation in ["fork-source", "existing-target"] {
            store
                .append(&TranscriptEntry {
                    conversation_id: conversation.into(),
                    sequence: 1,
                    created_at_unix_seconds: 1,
                    role: TranscriptRole::User,
                    turn_id: "original-turn".into(),
                    agent_id: "agent-%1".into(),
                    pane_id: "%1".into(),
                    content: format!("original {conversation}"),
                })
                .unwrap();
            store
                .save_compaction_epoch(conversation, 1, "original summary")
                .unwrap();
        }
        let mut service = test_runtime_service();
        service.set_agent_transcript_store(store);
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
            .bind_conversation("%1", "fork-source", 1)
            .unwrap();
        let before = service.session().windows()[0].panes().len();
        let snapshots = ["fork-source", "existing-target"].map(|conversation| {
            fs::read_dir(root.join(conversation))
                .unwrap()
                .map(|entry| {
                    let path = entry.unwrap().path();
                    (path.clone(), fs::read(path).unwrap())
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        });
        let response = service.dispatch_runtime_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"existing-fork","method":"agent/shell/command","params":{{"idempotency_key":"existing-fork","input":"/fork {target}"}}}}"#
        ), &primary);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response).unwrap()["result"]["body"],
            serde_json::Value::Null
        );
        assert!(service.agent_command_is_active("%1"));
        let response = service
            .run_pending_deferred_agent_command_for_tests()
            .unwrap()
            .unwrap();
        service.terminate_all_pane_processes().unwrap();
        assert!(
            response.contains("target conversation already exists"),
            "{response}"
        );
        assert_eq!(service.session().windows()[0].panes().len(), before);
        assert_eq!(
            service.agent_shell_store().get("%1").unwrap().session_id,
            "fork-source"
        );
        for (conversation, expected) in ["fork-source", "existing-target"]
            .into_iter()
            .zip(snapshots)
        {
            let actual = fs::read_dir(root.join(conversation))
                .unwrap()
                .map(|entry| {
                    let path = entry.unwrap().path();
                    (path.clone(), fs::read(path).unwrap())
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(actual, expected, "{conversation}");
        }
    }
}

/// Encodes the first prompt through the real provider adapter without sending
/// it. Native items and message roles remain inspectable in the returned JSON.
fn encoded_body(service: &mut RuntimeSessionService, pane: &str) -> serde_json::Value {
    encoded_body_with_provider(service, pane, None)
}

/// Encodes a synthetic model/provider switch against the same fork context.
/// Foreign native ownership must be omitted while provider-neutral history stays.
fn encoded_body_with_provider(
    service: &mut RuntimeSessionService,
    pane: &str,
    provider: Option<&str>,
) -> serde_json::Value {
    let context = service
        .agent_context_for_pane_prompt(pane, "continue", 0)
        .unwrap();
    let (_, mut profile) = service
        .active_model_profile_for_pane(pane, "agent-%1", None)
        .unwrap();
    if let Some(provider) = provider {
        profile.provider = provider.to_owned();
        profile.model = "switched-model".to_owned();
    }
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "fork-first-request".into(),
        conversation_id: "fork-target".into(),
        agent_id: "agent-%1".into(),
        pane_id: pane.into(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 3,
        deadline_at_unix_millis: 0,
        policy_profile: "runtime".into(),
        model_profile: "test".into(),
        parent_turn_id: None,
        state: AgentTurnState::Running,
        cooperation_mode: None,
        initial_capability: None,
    };
    let request = crate::integrations::agent::context::assemble_model_request(
        &profile,
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        &turn,
        &context,
    )
    .unwrap();
    let bytes = mez_agent::openai_responses_request_body_with_stream(&request, false).unwrap();
    serde_json::from_str(&bytes).unwrap()
}

/// Extracts exact single-line synthetic payloads and roles from encoded provider
/// messages. Only the pane-local replay label is excluded, so reopening in a new
/// pane does not create a false difference in otherwise identical history.
fn encoded_history(service: &mut RuntimeSessionService, pane: &str) -> Vec<(String, String)> {
    let body = encoded_body(service, pane);
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|item| {
            item["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|part| {
                    let text = part["text"].as_str()?;
                    (text.contains("FORK_ROW_") || text.contains("FORK_SUMMARY")).then(|| {
                        (
                            item["role"].as_str().unwrap().to_owned(),
                            text.lines().last().unwrap().to_owned(),
                        )
                    })
                })
        })
        .collect()
}

/// A saved source summarized through row 80 of 100 must fork its active
/// projection, not resurrect 80 covered rows. The real command's first encoded
/// request and a reopened/resumed first request must contain precisely the same
/// summary and ordered suffix while the full archive remains available.
#[test]
fn runtime_saved_fork_epoch_first_encoded_request_survives_reopen() {
    let root = temp_root("runtime-saved-fork-epoch");
    let store = AgentTranscriptStore::new(root.clone());
    for sequence in 1..=100 {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "fork-source".into(),
                sequence,
                created_at_unix_seconds: sequence,
                role: if sequence % 2 == 0 {
                    mez_agent::transcript::TranscriptRole::Assistant
                } else {
                    mez_agent::transcript::TranscriptRole::User
                },
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: format!("FORK_ROW_{sequence:03}"),
            })
            .unwrap();
    }
    store
        .save_compaction_epoch("fork-source", 40, "FORK_SUMMARY_OLD")
        .unwrap();
    store
        .save_compaction_epoch("fork-source", 80, "FORK_SUMMARY")
        .unwrap();
    let source_path = store.transcript_path("fork-source").unwrap();
    let epoch_path = source_path.parent().unwrap().join("compaction-epoch.json");
    let source_bytes = fs::read(&source_path).unwrap();
    let epoch_bytes = fs::read(&epoch_path).unwrap();
    let mut service = test_runtime_service();
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
    let resumed = service
        .execute_agent_shell_command(&primary, "/resume fork-source")
        .unwrap();
    assert!(!resumed.contains("\"error\""), "{resumed}");
    let settled = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(settled.contains("resumed=true"), "{settled}");
    let parent_projection = encoded_history(&mut service, "%1");
    let forked = service
        .execute_agent_shell_command(&primary, "/fork fork-target")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&forked).unwrap()["body"],
        serde_json::Value::Null
    );
    assert!(service.agent_command_is_active("%1"));
    let forked = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(forked.contains("forked=true"), "{forked}");
    let pane = service
        .agent_shell_store()
        .sessions()
        .find(|session| session.session_id == "fork-target")
        .unwrap()
        .pane_id
        .clone();
    let encoded = encoded_history(&mut service, &pane);
    assert_eq!(encoded, parent_projection);
    service.terminate_all_pane_processes().unwrap();
    let expected = std::iter::once(("developer".to_string(), "FORK_SUMMARY".to_string()))
        .chain((81..=100).map(|sequence| {
            (
                if sequence % 2 == 0 {
                    "assistant"
                } else {
                    "user"
                }
                .to_string(),
                format!("FORK_ROW_{sequence:03}"),
            )
        }))
        .collect::<Vec<_>>();
    assert_eq!(encoded, expected);
    assert_eq!(fs::read(&source_path).unwrap(), source_bytes);
    assert_eq!(fs::read(&epoch_path).unwrap(), epoch_bytes);
    assert_eq!(store.inspect("fork-target").unwrap().len(), 100);
    let mut reopened = test_runtime_service();
    reopened.set_agent_transcript_store(AgentTranscriptStore::new(root));
    let primary = reopened
        .attach_primary("reopened", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    reopened
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let resumed = reopened
        .execute_agent_shell_command(&primary, "/resume fork-target")
        .unwrap();
    assert!(!resumed.contains("\"error\""), "{resumed}");
    let settled = reopened
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(settled.contains("resumed=true"), "{settled}");
    assert_eq!(encoded_history(&mut reopened, "%1"), encoded);
}
