//! Durable copies use the actual saved-fork transaction and shared replay
//! worker. Original source comes from real task admission/provider settlement;
//! no checkpoint or fabricated row stands in for a successful publication.

use super::*;
use crate::runtime::control::execute_runtime_agent_prompt_history_work;

fn history(store: &AgentTranscriptStore, conversation: &str) -> mez_agent::ReplayEpochContext {
    history_with_count(store, conversation, 0)
}

/// Raw history has no manifest to discover; resume selects its genuine saved
/// row count. Typed callers deliberately keep the zero-count discovery test.
fn history_with_count(
    store: &AgentTranscriptStore,
    conversation: &str,
    count: u64,
) -> mez_agent::ReplayEpochContext {
    let mut reader = test_runtime_service();
    reader.set_agent_transcript_store(store.clone());
    reader
        .agent_shell_store_mut()
        .enter_or_resume("%2")
        .unwrap();
    reader
        .agent_shell_store_mut()
        .bind_conversation("%2", conversation, count)
        .unwrap();
    execute_runtime_agent_prompt_history_work(
        reader.prepare_runtime_agent_prompt_history_work("%2"),
    )
    .unwrap()
    .replay_owner()
    .unwrap()
    .unwrap()
}

/// Retires actual native/selector owners before propagating a scenario panic.
/// A failing regression must not leave cleanup to final owner destruction.
fn native_scenario(
    service: &mut RuntimeSessionService,
    scenario: impl FnOnce(&mut RuntimeSessionService),
) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scenario(service)));
    let cleanup = crate::runtime::tests::agent::fixture_teardown::settle(service);
    match outcome {
        Ok(()) => cleanup.unwrap(),
        Err(panic) => {
            if let Err(errors) = cleanup {
                eprintln!("owned fork fixture cleanup also failed: {errors:?}");
            }
            std::panic::resume_unwind(panic);
        }
    }
}

/// Drives the real initial publication and authenticated managed-shell startup
/// gates. Captured first-task context exists while queued; provider recovery
/// must not bypass either owner merely because that context can be inspected.
fn admit_and_bootstrap_child(service: &mut RuntimeSessionService, pane: &str, conversation: &str) {
    use crate::runtime::{PersistenceEvent, RuntimeSideEffect};
    let effects = service.persistence.take_transcript_effects();
    let mut initial = None;
    for effect in effects {
        match effect {
            RuntimeSideEffect::PersistInitialTranscript { store, path, batch } => {
                assert!(initial.replace((store, path, batch)).is_none());
            }
            effect => {
                // Metadata effects have no append custody. Keep their original
                // queue order while settling only this child's initial owner.
                assert!(!matches!(
                    effect,
                    RuntimeSideEffect::PersistTranscriptEntries { .. }
                ));
                service.persistence.queue_transcript(effect);
            }
        }
    }
    let (store, path, batch) = initial.expect("original child initial publication");
    assert_eq!(batch.conversation_id(), conversation);
    store.accept_transcript_append_receipt(&batch, 1).unwrap();
    store.settle_transcript_append_receipt(&batch, 1).unwrap();
    assert!(
        service
            .apply_persistence_transition(PersistenceEvent::InitialTranscriptCompleted {
                batch,
                path,
            })
            .unwrap()
            .applied
    );
    let token = service
        .posix_startup_token_for_tests(pane)
        .unwrap()
        .to_owned();
    service
        .observe_managed_shell_protocol_event(
            pane,
            mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
            mez_terminal::ManagedShellAdapter::Posix,
            &token,
            &mez_terminal::ManagedShellProtocolEvent::AdapterAvailable { trigger: None },
        )
        .unwrap();
    let (marker, bootstrap) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.pane_id == pane
                && transaction.kind == crate::runtime::RunningShellTransactionKind::Bootstrap)
                .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .unwrap();
    let agent = format!("agent-{pane}");
    service
        .observe_agent_shell_transaction_start(pane, &marker, &bootstrap, &agent, pane)
        .unwrap();
    let output = format!(
        "env\tos\t{}\nenv\tarch\t{}\nenv\thost\ttest-host\nenv\tuser\ttest-user\nenv\tshell_path\t/bin/sh\nenv\tshell_class\tposix-sh\nenv\tpath\t/usr/bin:/bin\nenv\tcwd\t{}\nenv\tgit_repo\t0\nbootstrap\tcomplete\t1714500000\n",
        if cfg!(target_os = "macos") {
            "Darwin"
        } else {
            "Linux"
        },
        std::env::consts::ARCH,
        std::env::current_dir().unwrap().display(),
    );
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&marker)
        .unwrap();
    transaction.observed_output_bytes = output.len();
    transaction.observed_output_preview = output;
    service
        .observe_agent_shell_transaction_end(pane, &marker, &bootstrap, &agent, pane, 0)
        .unwrap();
    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(pane),
        Some("ready")
    );
}

fn published_source() -> (RuntimeSessionService, AgentTranscriptStore, String, String) {
    let (mut service, store, turn) = provider_limit_replay::recovery_source(
        &format!("COVERED_PARENT_SOURCE {}", "source ".repeat(5_000)),
        "ORIGINAL_PARENT_RAW_TAIL",
        10,
    );
    let conversation = service
        .agent_turn_ledger()
        .turn(&turn)
        .unwrap()
        .conversation_id
        .clone();
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    // The diagnostic unit precedes the retained provider group. Two genuine
    // rewrites keep that group's raw bytes while advancing the canonical epoch.
    for attempt in 1..=2 {
        assert!(
            service
                .recover_agent_provider_context_limit_failure(
                    &AgentId::opaque("agent-%1").unwrap(),
                    &turn,
                    &error,
                    attempt,
                )
                .unwrap()
        );
        let summary = if attempt == 1 {
            "first model-authored fork head"
        } else {
            "original model-authored fork head"
        };
        complete_runtime_test_compaction(&mut service, "%1", summary);
        let completion = replay_publication::execute_pending_publication(&mut service);
        assert!(
            service
                .apply_replay_epoch_completion(completion)
                .unwrap()
                .applied
        );
    }
    (service, store, turn, conversation)
}

/// A saved fork owns its head, raw tail and original post-epoch arrivals. Later
/// parent appends cannot change that selection, and deleting the parent cannot
/// remove required target payloads. Current policy and permissions are not saved.
#[test]
fn saved_replay_fork_preserves_exact_head_tail_and_frozen_arrivals() {
    let (mut service, store, turn, conversation) = published_source();
    let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
    let provider = RuntimeBatchProvider {
        response: runtime_say_response(&turn, "ORIGINAL_POST_EPOCH_ARRIVAL", true),
    };
    assert_eq!(
        service
            .execute_agent_turn_with_provider(&turn, &provider, profile.clone())
            .unwrap()
            .terminal_state,
        AgentTurnState::Completed
    );
    let parent = history(&store, &conversation);
    assert!(parent.context().chronology().iter().any(|event| {
        event
            .block()
            .content
            .contains("ORIGINAL_POST_EPOCH_ARRIVAL")
    }));
    store.fork(&conversation, "saved-replay-copy", 123).unwrap();
    let copied = history(
        &AgentTranscriptStore::new(store.root()),
        "saved-replay-copy",
    );
    assert_eq!(copied.context().chronology(), parent.context().chronology());
    assert_eq!(copied.members(), parent.members());
    assert_eq!(copied.revision(), parent.revision());
    assert_eq!(copied.revision().get(), 2);
    assert_eq!(
        copied.head().unwrap().conversation().as_str(),
        "saved-replay-copy"
    );
    assert!(copied.context().stable_slots().is_empty());
    store
        .fork("saved-replay-copy", "nested-replay-copy", 124)
        .unwrap();
    let nested = history(&store, "nested-replay-copy");
    assert_eq!(nested.context().chronology(), copied.context().chronology());
    assert_eq!(nested.members(), copied.members());
    assert_eq!(nested.revision(), copied.revision());
    assert_eq!(
        nested.head().unwrap().conversation().as_str(),
        "nested-replay-copy"
    );
    let later = service
        .start_agent_prompt_turn("%1", "LATER_PARENT_TASK")
        .unwrap();
    let provider = RuntimeBatchProvider {
        response: runtime_say_response(&later.turn_id, "LATER_PARENT_RESULT", true),
    };
    service
        .execute_agent_turn_with_provider(&later.turn_id, &provider, profile)
        .unwrap();
    assert_eq!(
        history(&store, "saved-replay-copy").context().chronology(),
        copied.context().chronology()
    );
    store.delete(&conversation).unwrap();
    assert_eq!(
        history(&store, "nested-replay-copy").context().chronology(),
        copied.context().chronology()
    );
    assert_eq!(
        history(
            &AgentTranscriptStore::new(store.root()),
            "saved-replay-copy"
        )
        .context()
        .chronology(),
        copied.context().chronology()
    );
}

/// A copied epoch retains its canonical revision but starts an independent
/// storage CAS chain. A fresh runtime must admit a real child task, publish its
/// next head and settle later provider output without changing the parent.
#[test]
fn saved_replay_copy_publishes_independently_from_a_new_runtime() {
    let (parent, store, _parent_turn, conversation) = published_source();
    store.fork(&conversation, "independent-child", 123).unwrap();
    let before_parent = history(&store, &conversation);
    let parent_path = store
        .root()
        .join(&conversation)
        .join("compaction-epoch.json");
    let parent_manifest = fs::read(&parent_path).unwrap();
    let mut child = test_runtime_service();
    child
        .replace_config_layers(parent.integration.config_layers().to_vec())
        .unwrap();
    child.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    child
        .attach_primary("independent-child", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    child.agent_shell_store_mut().enter_or_resume("%1").unwrap();
    child
        .agent_shell_store_mut()
        .bind_conversation("%1", "independent-child", 0)
        .unwrap();
    let turn = child
        .start_agent_prompt_turn("%1", "CHILD_ONLY_TASK")
        .unwrap();
    let before = child.agent_turn_contexts()[&turn.turn_id].clone();
    let owner = mez_agent::ReplayEpochContext::from_owned_context(
        mez_agent::ReplayConversationId::new("independent-child").unwrap(),
        before.clone(),
    )
    .unwrap();
    assert_eq!(owner.revision().get(), 2);
    let selected = match owner
        .prepare_compaction(mez_agent::ReplayTailPolicy::new(40_000, 10).unwrap())
        .unwrap()
    {
        mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
        other => panic!("expected inherited closed child source: {other:?}"),
    };
    let expected_tail = before.chronology()[selected.summary_input().len()..].to_vec();
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    assert!(
        child
            .recover_agent_provider_context_limit_failure(
                &AgentId::opaque("agent-%1").unwrap(),
                &turn.turn_id,
                &error,
                1,
            )
            .unwrap()
    );
    complete_runtime_test_compaction(&mut child, "%1", "independent child head");
    let completion = replay_publication::execute_pending_publication(&mut child);
    assert_eq!(child.agent_turn_contexts()[&turn.turn_id], before);
    assert!(
        child
            .apply_replay_epoch_completion(completion)
            .unwrap()
            .applied
    );
    let after = &child.agent_turn_contexts()[&turn.turn_id];
    assert_eq!(
        after.chronology()[0].block().content,
        "independent child head"
    );
    assert_eq!(&after.chronology()[1..], expected_tail);
    let cold = history(&store, "independent-child");
    assert_eq!(cold.revision().get(), 3);
    assert_eq!(cold.context().chronology(), after.chronology());
    assert!(child.agent_provider_task_is_pending(&turn.turn_id));
    let profile = child
        .agent_turn_model_profile(&turn.turn_id)
        .unwrap()
        .clone();
    let provider = RuntimeBatchProvider {
        response: runtime_say_response(&turn.turn_id, "CHILD_LATER_RESULT", true),
    };
    assert_eq!(
        child
            .execute_agent_turn_with_provider(&turn.turn_id, &provider, profile)
            .unwrap()
            .terminal_state,
        AgentTurnState::Completed
    );
    assert!(
        history(&store, "independent-child")
            .context()
            .chronology()
            .iter()
            .any(|event| { event.block().content.contains("CHILD_LATER_RESULT") })
    );
    assert_eq!(fs::read(parent_path).unwrap(), parent_manifest);
    assert_eq!(
        history(&store, &conversation).context().chronology(),
        before_parent.context().chronology()
    );
}

/// The real command worker and actor adoption must compose a typed copy with
/// fresh current permissions and usage, rather than merely creating files.
#[test]
fn saved_replay_fork_command_adopts_a_typed_child_without_authority_or_usage() {
    let (mut service, store, turn, conversation) = published_source();
    native_scenario(&mut service, |service| {
        let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
        let provider = RuntimeBatchProvider {
            response: runtime_say_response(&turn, "parent settled before saved fork command", true),
        };
        assert_eq!(
            service
                .execute_agent_turn_with_provider(&turn, &provider, profile.clone())
                .unwrap()
                .terminal_state,
            AgentTurnState::Completed,
        );
        service.start_initial_pane_process(Some("cat")).unwrap();
        let primary = service
            .session
            .clients()
            .iter()
            .find(|client| service.session.is_attached_primary(&client.id))
            .unwrap()
            .id
            .clone();
        let expected = history(&store, &conversation);
        let parent_actions = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .allowed_actions
            .clone();
        let usage = mez_agent::ModelTokenUsage {
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: 3,
            cached_input_tokens: Some(10),
            cache_write_input_tokens: None,
        };
        service.record_agent_provider_token_usage_with_profile("%1", usage, usage, Some(&profile));
        let parent_usage = service.agent_token_usage_for_conversation(&conversation);
        assert!(!parent_usage.is_empty());
        service.set_agent_enabled_actions(mez_agent::AllowedActionSet::say_only());
        service
            .execute_agent_shell_command(&primary, "/fork adopted-typed-copy")
            .unwrap();
        let settled = service
            .run_pending_deferred_agent_command_for_tests()
            .unwrap()
            .unwrap();
        assert!(settled.contains("forked=true"), "{settled}");
        let target = service
            .agent_shell_store()
            .sessions()
            .find(|session| session.session_id == "adopted-typed-copy")
            .unwrap()
            .clone();
        assert_ne!(target.pane_id, "%1");
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
            parent_actions
        );
        assert!(
            service
                .agent_token_usage_for_conversation("adopted-typed-copy")
                .is_empty()
        );
        assert_eq!(
            service.agent_token_usage_for_conversation(&conversation),
            parent_usage
        );
        let actual = execute_runtime_agent_prompt_history_work(
            service.prepare_runtime_agent_prompt_history_work(&target.pane_id),
        )
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
        assert_eq!(
            actual.context().chronology(),
            expected.context().chronology()
        );
        assert_eq!(
            actual.head().unwrap().conversation().as_str(),
            "adopted-typed-copy"
        );
        let child = service
            .start_agent_prompt_turn(&target.pane_id, "new child intent")
            .unwrap();
        let context = &service.agent_turn_contexts()[&child.turn_id];
        let owner = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new("adopted-typed-copy").unwrap(),
            context.clone(),
        )
        .unwrap();
        assert_eq!(
            owner.head().unwrap().occurrence_id(),
            expected.head().unwrap().occurrence_id()
        );
        assert!(
            !context
                .blocks()
                .iter()
                .any(|block| block.content.contains("COVERED_PARENT_SOURCE"))
        );
    });
}

/// Explicitly saving a NoStore fork must use its current frozen body after
/// deletion of the older saved ancestor, without saving the parent itself.
#[test]
fn nostore_saved_fork_command_publishes_latest_frozen_source() {
    let (mut service, store, parent_turn, ancestor) = published_source();
    native_scenario(&mut service, |service| {
        let (expected, _) = prepare_nostore_parent(service, &store, &parent_turn, &ancestor);
        store.delete(&ancestor).unwrap();
        service.start_initial_pane_process(Some("cat")).unwrap();
        let primary = service
            .session
            .clients()
            .iter()
            .find(|client| service.session.is_attached_primary(&client.id))
            .unwrap()
            .id
            .clone();
        let parent_actions = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .allowed_actions
            .clone();
        service.set_agent_enabled_actions(mez_agent::AllowedActionSet::say_only());
        service
            .execute_agent_shell_command(&primary, "/fork saved-memory-copy")
            .unwrap();
        let settled = service
            .run_pending_deferred_agent_command_for_tests()
            .unwrap()
            .unwrap();
        assert!(settled.contains("forked=true"), "{settled}");
        let child = service
            .agent_shell_store()
            .sessions()
            .find(|session| session.session_id == "saved-memory-copy")
            .unwrap()
            .clone();
        assert!(!child.ephemeral);
        assert_eq!(
            child.allowed_actions,
            Some(mez_agent::AllowedActionSet::say_only())
        );
        assert_eq!(
            service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .allowed_actions,
            parent_actions
        );
        assert!(
            service
                .agent_token_usage_for_conversation("saved-memory-copy")
                .is_empty()
        );
        let current = execute_runtime_agent_prompt_history_work(
            service.prepare_runtime_agent_prompt_history_work(&child.pane_id),
        )
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
        assert_eq!(
            current.context().chronology(),
            expected.context().chronology()
        );
        assert_eq!(
            history(&store, "saved-memory-copy").context().chronology(),
            expected.context().chronology()
        );
        assert!(
            !store
                .root()
                .join("nostore-parent/compaction-epoch.json")
                .exists()
        );
        assert!(service.agent_shell_store().get("%1").unwrap().ephemeral);
    });
}

/// Uses the real deferred command claim and worker, leaving its original
/// adoption lease alive for the caller's second-round-trip assertions.
fn publish_nostore_saved_command(
    service: &mut RuntimeSessionService,
    primary: &mez_core::ids::ClientId,
) -> (
    crate::runtime::RuntimeAgentCommandAsyncWork,
    std::sync::Arc<crate::runtime::commands::SavedForkRead>,
) {
    service
        .execute_agent_shell_command(primary, "/fork pending-memory-copy")
        .unwrap();
    let dispatch = service
        .take_pending_deferred_agent_commands()
        .pop()
        .unwrap();
    let work = service
        .claim_agent_command_work(
            &dispatch.primary_client_id,
            &dispatch.pane_id,
            &dispatch.command,
            &dispatch.input,
            dispatch.claim_generation,
            &dispatch.conversation_id,
        )
        .unwrap()
        .unwrap();
    let crate::runtime::RuntimeAgentCommandAsyncOutcome::SavedFork(read) =
        RuntimeSessionService::execute_deferred_agent_command(&work)
    else {
        panic!("original saved-fork worker must publish")
    };
    (work, read)
}

/// Equal text and conversation IDs do not make a replacement NoStore binding
/// the original command's source. Proven non-adoption rolls back only its child.
#[test]
fn nostore_saved_fork_stale_binding_compensates_only_its_target() {
    let (mut service, store, parent_turn, ancestor) = published_source();
    native_scenario(&mut service, |service| {
        let (expected, _) = prepare_nostore_parent(service, &store, &parent_turn, &ancestor);
        service.start_initial_pane_process(Some("cat")).unwrap();
        let primary = service
            .session
            .clients()
            .iter()
            .find(|client| service.session.is_attached_primary(&client.id))
            .unwrap()
            .id
            .clone();
        let (work, read) = publish_nostore_saved_command(service, &primary);
        let original = service
            .frozen_replay_binding("%1", "nostore-parent")
            .unwrap()
            .clone();
        service
            .bind_ephemeral_replay_history("%1", "nostore-parent", &original, None)
            .unwrap();
        let reply = service.complete_agent_command_work(
            &work,
            crate::runtime::RuntimeAgentCommandAsyncOutcome::SavedFork(read.clone()),
        );
        let outcome = RuntimeSessionService::finish_saved_fork_worker(&read, reply);
        assert!(matches!(
            outcome,
            crate::runtime::RuntimeAgentCommandAsyncOutcome::Failed { .. }
        ));
        let projected =
            RuntimeSessionService::project_deferred_agent_command_outcome(&work, outcome).unwrap();
        service
            .complete_agent_command_work(&work, projected)
            .unwrap();
        assert!(!store.root().join("pending-memory-copy").exists());
        assert_eq!(
            service
                .frozen_replay_binding("%1", "nostore-parent")
                .unwrap()
                .history()
                .replay_owner()
                .unwrap()
                .unwrap()
                .context()
                .chronology(),
            expected.context().chronology()
        );
        assert!(
            !store
                .root()
                .join("nostore-parent/compaction-epoch.json")
                .exists()
        );
        assert!(store.root().join(ancestor).exists());
    });
}

/// Starting a real new task advances an idle parent's canonical source without
/// replacing its retained frozen binding or performing another compaction.
/// Both copy consumers must fence that transition before actor adoption.
#[test]
fn nostore_saved_fork_parent_admission_invalidates_idle_capture() {
    let (mut service, store, parent_turn, ancestor) = published_source();
    native_scenario(&mut service, |service| {
        prepare_nostore_parent(service, &store, &parent_turn, &ancestor);
        service.start_initial_pane_process(Some("cat")).unwrap();
        let primary = service
            .session
            .clients()
            .iter()
            .find(|client| service.session.is_attached_primary(&client.id))
            .unwrap()
            .id
            .clone();
        assert!(
            service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .running_turn_id
                .is_none()
        );
        let (work, read) = publish_nostore_saved_command(service, &primary);
        let original = service
            .frozen_replay_binding("%1", "nostore-parent")
            .unwrap()
            .clone();
        let epoch = service.agent_compaction_epoch("%1");
        let next = service
            .start_agent_prompt_turn("%1", "NEW_TASK_AFTER_FORK_CAPTURE")
            .unwrap();
        assert!(service.frozen_replay_binding("%1", "nostore-parent") == Some(&original));
        assert_eq!(service.agent_compaction_epoch("%1"), epoch);
        let reply = service.complete_agent_command_work(
            &work,
            crate::runtime::RuntimeAgentCommandAsyncOutcome::SavedFork(read.clone()),
        );
        let outcome = RuntimeSessionService::finish_saved_fork_worker(&read, reply);
        assert!(matches!(
            outcome,
            crate::runtime::RuntimeAgentCommandAsyncOutcome::Failed { .. }
        ));
        let projected =
            RuntimeSessionService::project_deferred_agent_command_outcome(&work, outcome).unwrap();
        service
            .complete_agent_command_work(&work, projected)
            .unwrap();
        assert!(!store.root().join("pending-memory-copy").exists());
        assert!(
            service.agent_turn_contexts()[&next.turn_id]
                .chronology()
                .iter()
                .any(|event| event.block().content == "NEW_TASK_AFTER_FORK_CAPTURE")
        );
        assert!(!store.root().join("nostore-parent").exists());
    });
}

/// A lost actor reply cannot prove non-adoption. The exact saved child remains
/// readable and the duplicate fence refuses a late attempt to allocate a pane.
#[test]
fn nostore_saved_fork_lost_reply_retains_independent_history() {
    let (mut service, store, parent_turn, ancestor) = published_source();
    native_scenario(&mut service, |service| {
        let (expected, _) = prepare_nostore_parent(service, &store, &parent_turn, &ancestor);
        service.start_initial_pane_process(Some("cat")).unwrap();
        let primary = service
            .session
            .clients()
            .iter()
            .find(|client| service.session.is_attached_primary(&client.id))
            .unwrap()
            .id
            .clone();
        let (work, read) = publish_nostore_saved_command(service, &primary);
        let outcome = RuntimeSessionService::finish_saved_fork_worker(
            &read,
            Err(MezError::invalid_state("synthetic lost actor reply")),
        );
        assert!(matches!(
            outcome,
            crate::runtime::RuntimeAgentCommandAsyncOutcome::Failed { .. }
        ));
        assert!(
            service
                .complete_agent_command_work(
                    &work,
                    crate::runtime::RuntimeAgentCommandAsyncOutcome::SavedFork(read)
                )
                .is_err()
        );
        service.agent.cancel_agent_command("%1");
        store.delete(&ancestor).unwrap();
        assert_eq!(
            history(&store, "pending-memory-copy")
                .context()
                .chronology(),
            expected.context().chronology()
        );
        assert!(
            !store
                .root()
                .join("nostore-parent/compaction-epoch.json")
                .exists()
        );
    });
}

/// Before any summary exists, the saved command must persist the actual raw
/// task and original closed execution state, without inventing a summary head.
#[test]
fn nostore_saved_fork_before_first_head_cold_replays_raw_lifecycle() {
    let (mut service, store, turn) =
        provider_limit_replay::recovery_source("old saved task", "old saved final", 10);
    native_scenario(&mut service, |service| {
        let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
        service
            .execute_agent_turn_with_provider(
                &turn,
                &RuntimeBatchProvider {
                    response: runtime_say_response(&turn, "saved ancestor completed", true),
                },
                profile,
            )
            .unwrap();
        service
            .agent_shell_store_mut()
            .bind_ephemeral_conversation_with_lineage("%1", "raw-command-parent", 0, None)
            .unwrap();
        let memory = service
            .start_agent_prompt_turn("%1", "RAW_MEMORY_TASK")
            .unwrap();
        let profile = service
            .agent_turn_model_profile(&memory.turn_id)
            .unwrap()
            .clone();
        service
            .execute_agent_turn_with_provider(
                &memory.turn_id,
                &RuntimeBatchProvider {
                    response: runtime_say_response(&memory.turn_id, "RAW_MEMORY_FINAL", true),
                },
                profile,
            )
            .unwrap();
        let expected = service
            .frozen_replay_binding("%1", "raw-command-parent")
            .unwrap()
            .history()
            .replay_owner()
            .unwrap()
            .unwrap();
        assert!(expected.head().is_none());
        service.start_initial_pane_process(Some("cat")).unwrap();
        let primary = service
            .session
            .clients()
            .iter()
            .find(|client| service.session.is_attached_primary(&client.id))
            .unwrap()
            .id
            .clone();
        service
            .execute_agent_shell_command(&primary, "/fork raw-memory-child")
            .unwrap();
        let outcome = service
            .run_pending_deferred_agent_command_for_tests()
            .unwrap()
            .unwrap();
        assert!(outcome.contains("forked=true"), "{outcome}");
        let count = store
            .summary("raw-memory-child")
            .unwrap()
            .unwrap()
            .entries
            .try_into()
            .unwrap();
        let cold = history_with_count(&store, "raw-memory-child", count);
        assert!(cold.head().is_none());
        assert_eq!(cold.context().chronology(), expected.context().chronology());
        assert!(
            cold.checkpoint()
                .unwrap()
                .with_stable_records(Vec::new())
                .unwrap()
                == expected
                    .checkpoint()
                    .unwrap()
                    .for_conversation_copy(
                        mez_agent::ReplayConversationId::new("raw-memory-child").unwrap()
                    )
                    .unwrap()
                    .with_stable_records(Vec::new())
                    .unwrap()
        );
        assert!(
            !store
                .root()
                .join("raw-memory-child/compaction-epoch.json")
                .exists()
        );
        assert!(!store.root().join("raw-command-parent").exists());
    });
}

/// Two actual arrivals with equal payloads remain distinct untrusted source.
/// Neither raw nor headed NoStore copies may need a nonexistent parent row or
/// recover their source from an older saved ancestor after worker dispatch.
#[test]
fn nostore_saved_fork_preserves_original_peer_occurrences() {
    use mez_agent::messaging::Recipient;
    use mez_agent::transcript::{TranscriptContextEvent, TranscriptContextRecord};

    for headed in [false, true] {
        let (mut service, store, parent_turn, ancestor) = published_source();
        native_scenario(&mut service, |service| {
            if headed {
                prepare_nostore_parent(service, &store, &parent_turn, &ancestor);
            } else {
                let profile = service
                    .agent_turn_model_profile(&parent_turn)
                    .unwrap()
                    .clone();
                service
                    .execute_agent_turn_with_provider(
                        &parent_turn,
                        &RuntimeBatchProvider {
                            response: runtime_say_response(
                                &parent_turn,
                                "saved ancestor completed",
                                true,
                            ),
                        },
                        profile,
                    )
                    .unwrap();
                service
                    .agent_shell_store_mut()
                    .bind_ephemeral_conversation_with_lineage("%1", "nostore-parent", 0, None)
                    .unwrap();
            }
            let turn = service
                .start_agent_prompt_turn("%1", "receive peer inputs before copying")
                .unwrap();
            let now = crate::runtime::current_unix_millis();
            let recipient = service
                .ensure_runtime_message_identity(
                    "agent-%1",
                    mez_core::ids::PaneId::opaque("%1"),
                    "agent",
                    &[],
                    now,
                )
                .unwrap();
            service
                .message_service_mut()
                .subscribe_from_retained_start(&recipient.agent_id)
                .unwrap();
            let sender = service
                .ensure_runtime_message_identity("agent-copy-source", None, "agent", &[], now)
                .unwrap();
            // This isolated fixture exercises actual in-memory receive, not a
            // manufactured native-root receipt or durable transport ACK.
            assert!(!service.native_input_storage_required());
            for ordinal in 1..=2 {
                service
                    .message_service_mut()
                    .accept_at_with_scope(
                        &sender.agent_id,
                        Envelope {
                            protocol: "mmp/1",
                            id: format!("copy-peer-{ordinal}"),
                            message_type: "send".into(),
                            time: format!("runtime:{now}"),
                            sender: sender.clone(),
                            recipient: Recipient::Agent(recipient.agent_id.clone()),
                            correlation_id: None,
                            ttl_ms: None,
                            content_type: "text/plain; charset=utf-8".into(),
                            payload: "identical original peer payload".into(),
                            extension_fields: Vec::new(),
                        },
                        MessageScope::Session,
                        now,
                    )
                    .unwrap();
            }
            assert_eq!(
                service.deliver_pending_runtime_agent_messages(now).unwrap(),
                2
            );
            let profile = service
                .agent_turn_model_profile(&turn.turn_id)
                .unwrap()
                .clone();
            service
                .execute_agent_turn_with_provider(
                    &turn.turn_id,
                    &RuntimeBatchProvider {
                        response: runtime_say_response(&turn.turn_id, "peer source settled", true),
                    },
                    profile,
                )
                .unwrap();
            let expected = service
                .frozen_replay_binding("%1", "nostore-parent")
                .unwrap()
                .history()
                .replay_owner()
                .unwrap()
                .unwrap();
            assert_eq!(expected.head().is_some(), headed);
            let peers = expected
                .context()
                .chronology()
                .iter()
                .filter(|event| event.block().source == ContextSourceKind::PeerMessage)
                .collect::<Vec<_>>();
            assert_eq!(peers.len(), 2);
            assert_ne!(
                peers[0].record().occurrence_id(),
                peers[1].record().occurrence_id()
            );
            store.delete(&ancestor).unwrap();
            service.start_initial_pane_process(Some("cat")).unwrap();
            let primary = service
                .session
                .clients()
                .iter()
                .find(|client| service.session.is_attached_primary(&client.id))
                .unwrap()
                .id
                .clone();
            service
                .execute_agent_shell_command(&primary, "/fork peer-memory-child")
                .unwrap();
            let outcome = service
                .run_pending_deferred_agent_command_for_tests()
                .unwrap()
                .unwrap();
            assert!(
                outcome.contains("forked=true"),
                "headed={headed}: {outcome}"
            );
            let count = store.summary("peer-memory-child").unwrap().unwrap().entries;
            let cold = history_with_count(&store, "peer-memory-child", count.try_into().unwrap());
            assert_eq!(cold.head().is_some(), headed);
            assert_eq!(cold.context().chronology(), expected.context().chronology());
            let mut references = 0;
            for row in store.inspect("peer-memory-child").unwrap() {
                if row.role != mez_agent::TranscriptRole::System {
                    continue;
                }
                let Some(record) =
                    TranscriptContextRecord::from_transcript_content(&row.content).unwrap()
                else {
                    continue;
                };
                assert!(!matches!(
                    record.event(),
                    TranscriptContextEvent::NativePeerMessage { .. }
                ));
                if matches!(
                    record.event(),
                    TranscriptContextEvent::PromptBoundary {
                        source: ContextSourceKind::PeerMessage,
                        ..
                    }
                ) {
                    references += 1;
                    assert!(peers.iter().any(|peer| peer.record() == record.record()));
                    assert_eq!(row.turn_id, turn.turn_id);
                    assert_eq!(row.agent_id, "agent-%1");
                    assert_eq!(row.pane_id, "%1");
                }
            }
            assert_eq!(references, 2);
            assert!(service.agent_shell_store().get("%1").unwrap().ephemeral);
            assert!(
                !store
                    .root()
                    .join("nostore-parent/compaction-epoch.json")
                    .exists()
            );
        });
    }
}

/// A subagent's actual first request must inherit the same typed head and raw
/// records; a zero display count cannot erase its parent's durable authority.
#[test]
fn subagent_replay_fork_inherits_the_typed_parent_head_on_its_first_task() {
    let (mut service, store, _turn, conversation) = published_source();
    native_scenario(&mut service, |service| {
        service.use_transcript_effect_adapter();
        service.start_initial_pane_process(Some("cat")).unwrap();
        let primary = service
            .session
            .clients()
            .iter()
            .find(|client| service.session.is_attached_primary(&client.id))
            .unwrap()
            .id
            .clone();
        let expected = history(&store, &conversation);
        let response = service
            .spawn_runtime_subagent(
                &primary,
                SubagentSpawnRequest {
                    parent_agent_id: "agent-%1".into(),
                    requested_role: "explorer".into(),
                    placement: "new-pane".into(),
                    cooperation_mode: CooperationMode::ExploreOnly,
                    cooperation_mode_defaulted: false,
                    read_scopes: vec![],
                    read_scopes_defaulted: false,
                    write_scopes: vec![],
                    write_scopes_defaulted: false,
                    session_mode: mez_agent::SubagentSessionMode::Fork,
                    initial_model_size: None,
                    initial_reasoning_effort: None,
                    task_prompt: "continue with this frozen context".into(),
                    explicit_user_approval: false,
                    skip_initial_turn: false,
                },
                RuntimeSubagentPlacement::NewPane {
                    direction: SplitDirection::Vertical,
                    select: true,
                },
            )
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let pane = value["pane"]["pane_id"].as_str().unwrap();
        let turn = value["turn"]["id"].as_str().unwrap();
        let target = service.agent_shell_store().get(pane).unwrap();
        let child_conversation = target.session_id.clone();
        let context = &service.agent_turn_contexts()[turn];
        let owner = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new(&target.session_id).unwrap(),
            context.clone(),
        )
        .unwrap();
        assert_eq!(owner.revision(), expected.revision());
        assert_eq!(
            owner.head().unwrap().occurrence_id(),
            expected.head().unwrap().occurrence_id()
        );
        assert!(
            !context
                .blocks()
                .iter()
                .any(|block| block.content.contains("COVERED_PARENT_SOURCE"))
        );
        for original in expected.context().chronology().iter().skip(1) {
            assert!(context.chronology().iter().any(|event| {
                event.record() == original.record() && event.block() == original.block()
            }));
        }
        // The first task carries a planned initial root. A subsequent genuine
        // publication must check that exact durable CAS root, rather than treat
        // the child as an unowned conversation or reuse its parent's authority.
        assert_eq!(
            service.agent_turn_ledger().turn(turn).unwrap().state,
            AgentTurnState::Queued
        );
        admit_and_bootstrap_child(service, pane, &child_conversation);
        let context = &service.agent_turn_contexts()[turn];
        let owner = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new(&child_conversation).unwrap(),
            context.clone(),
        )
        .unwrap();
        assert_eq!(
            service.agent_turn_ledger().turn(turn).unwrap().state,
            AgentTurnState::Running
        );
        assert!(!service.agent_is_compacting(pane));
        assert!(
            matches!(
                owner
                    .prepare_compaction(mez_agent::ReplayTailPolicy::new(40_000, 10).unwrap())
                    .unwrap(),
                mez_agent::ReplayCompactionPreparation::Ready(_)
            ),
            "child source planning: {:?}",
            owner
                .prepare_compaction(mez_agent::ReplayTailPolicy::new(40_000, 10).unwrap())
                .unwrap()
        );
        let before = context.clone();
        let parent_manifest = fs::read(
            store
                .root()
                .join(&conversation)
                .join("compaction-epoch.json"),
        )
        .unwrap();
        let error = MezError::invalid_state("provider context length exceeded")
            .with_provider_failure_json(
                r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
            );
        assert!(
            service
                .recover_agent_provider_context_limit_failure(
                    &AgentId::opaque(format!("agent-{pane}")).unwrap(),
                    turn,
                    &error,
                    1,
                )
                .unwrap()
        );
        complete_runtime_test_compaction(service, pane, "independent subagent head");
        let completion = replay_publication::execute_pending_publication(service);
        assert_eq!(service.agent_turn_contexts()[turn], before);
        assert!(
            service
                .apply_replay_epoch_completion(completion)
                .unwrap()
                .applied
        );
        let after = &service.agent_turn_contexts()[turn];
        assert_eq!(
            after.chronology()[0].block().content,
            "independent subagent head"
        );
        let cold = history(&store, &child_conversation);
        assert_eq!(cold.revision().get(), expected.revision().get() + 1);
        assert_eq!(cold.context().chronology(), after.chronology());
        let bounded = store
            .conversation_replay_snapshot(&child_conversation, &[], |_| {
                Some((
                    crate::storage::transcript::ConversationTranscriptRead::All,
                    true,
                ))
            })
            .unwrap();
        assert_eq!(
            bounded.replay.as_ref().unwrap().covered_archive_frontier(),
            store.inspect(&child_conversation).unwrap().len() as u64
        );
        assert!(
            bounded.view.logical.is_empty(),
            "initial copied rows must not force lifetime decoding"
        );
        assert_eq!(
            fs::read(
                store
                    .root()
                    .join(&conversation)
                    .join("compaction-epoch.json")
            )
            .unwrap(),
            parent_manifest
        );
    });
}

/// Advances actual saved source through a NoStore rewrite and terminal cleanup.
/// Both fork consumers below need the newer body rather than its saved ancestor.
fn prepare_nostore_parent(
    service: &mut RuntimeSessionService,
    store: &AgentTranscriptStore,
    parent_turn: &str,
    conversation: &str,
) -> (mez_agent::ReplayEpochContext, String) {
    let profile = service
        .agent_turn_model_profile(parent_turn)
        .unwrap()
        .clone();
    service
        .execute_agent_turn_with_provider(
            parent_turn,
            &RuntimeBatchProvider {
                response: runtime_say_response(
                    parent_turn,
                    "original durable parent completed",
                    true,
                ),
            },
            profile,
        )
        .unwrap();
    let source = service.freeze_agent_replay_history("%1").unwrap().unwrap();
    let original_manifest = fs::read(
        store
            .root()
            .join(conversation)
            .join("compaction-epoch.json"),
    )
    .unwrap();
    service
        .bind_ephemeral_replay_history("%1", "nostore-parent", &source, None)
        .unwrap();
    let turn = service
        .start_agent_prompt_turn("%1", "MEMORY_PARENT_TASK_ONLY")
        .unwrap();
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    assert!(
        service
            .recover_agent_provider_context_limit_failure(
                &AgentId::opaque("agent-%1").unwrap(),
                &turn.turn_id,
                &error,
                1,
            )
            .unwrap()
    );
    complete_runtime_test_compaction(service, "%1", "latest memory-only parent head");
    let completion = replay_publication::execute_pending_publication(service);
    assert!(
        service
            .apply_replay_epoch_completion(completion)
            .unwrap()
            .applied
    );
    let profile = service
        .agent_turn_model_profile(&turn.turn_id)
        .unwrap()
        .clone();
    service
        .execute_agent_turn_with_provider(
            &turn.turn_id,
            &RuntimeBatchProvider {
                response: runtime_say_response(&turn.turn_id, "MEMORY_ONLY_FINAL", true),
            },
            profile.clone(),
        )
        .unwrap();
    let usage = mez_agent::ModelTokenUsage {
        input_tokens: 100,
        output_tokens: 20,
        reasoning_tokens: 3,
        cached_input_tokens: Some(10),
        cache_write_input_tokens: None,
    };
    service.record_agent_provider_token_usage_with_profile("%1", usage, usage, Some(&profile));
    assert!(
        !service
            .agent_token_usage_for_conversation("nostore-parent")
            .is_empty()
    );
    let expected = service
        .frozen_replay_binding("%1", "nostore-parent")
        .unwrap()
        .history()
        .replay_owner()
        .unwrap()
        .unwrap();
    assert_eq!(expected.revision().get(), 3);
    assert_eq!(
        expected.context().chronology()[0].block().content,
        "latest memory-only parent head"
    );
    assert!(
        expected
            .context()
            .blocks()
            .iter()
            .any(|block| block.content.contains("MEMORY_ONLY_FINAL"))
    );
    assert!(
        !store
            .root()
            .join("nostore-parent")
            .join("compaction-epoch.json")
            .exists()
    );
    assert_eq!(
        fs::read(
            store
                .root()
                .join(conversation)
                .join("compaction-epoch.json")
        )
        .unwrap(),
        original_manifest
    );
    (expected, turn.turn_id)
}

/// A newer NoStore rewrite and its final provider source must survive child
/// capture without saving that parent or resampling its older saved ancestor.
#[test]
fn nostore_subagent_fork_uses_its_latest_frozen_head_without_saving_parent() {
    let (mut service, store, parent_turn, conversation) = published_source();
    native_scenario(&mut service, |service| {
        let (expected, memory_turn) =
            prepare_nostore_parent(service, &store, &parent_turn, &conversation);
        let parent_actions = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .allowed_actions
            .clone();
        let mut profiles = service.integration.subagent_profiles().clone();
        profiles.get_mut("explorer").unwrap().allowed_actions =
            Some(vec![mez_agent::AllowedAction::Say]);
        service.integration.replace_subagent_profiles(profiles);
        service.start_initial_pane_process(Some("cat")).unwrap();
        service.use_transcript_effect_adapter();
        let primary = service
            .session
            .clients()
            .iter()
            .find(|client| service.session.is_attached_primary(&client.id))
            .unwrap()
            .id
            .clone();
        let response = service
            .spawn_runtime_subagent(
                &primary,
                SubagentSpawnRequest {
                    parent_agent_id: "agent-%1".into(),
                    requested_role: "explorer".into(),
                    placement: "new-pane".into(),
                    cooperation_mode: CooperationMode::ExploreOnly,
                    cooperation_mode_defaulted: false,
                    read_scopes: vec![],
                    read_scopes_defaulted: false,
                    write_scopes: vec![],
                    write_scopes_defaulted: false,
                    session_mode: mez_agent::SubagentSessionMode::Fork,
                    initial_model_size: None,
                    initial_reasoning_effort: None,
                    task_prompt: "continue the latest memory-only source".into(),
                    explicit_user_approval: false,
                    skip_initial_turn: false,
                },
                RuntimeSubagentPlacement::NewPane {
                    direction: SplitDirection::Vertical,
                    select: true,
                },
            )
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let pane = value["pane"]["pane_id"].as_str().unwrap();
        let child_turn = value["turn"]["id"].as_str().unwrap();
        let cid = service
            .agent_shell_store()
            .get(pane)
            .unwrap()
            .session_id
            .clone();
        let child = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new(&cid).unwrap(),
            service.agent_turn_contexts()[child_turn].clone(),
        )
        .unwrap();
        assert_eq!(
            service
                .agent_shell_store()
                .get(pane)
                .unwrap()
                .allowed_actions,
            Some(mez_agent::AllowedActionSet::say_only())
        );
        assert_eq!(
            service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .allowed_actions,
            parent_actions
        );
        assert_eq!(child.revision(), expected.revision());
        assert_eq!(
            child.head().unwrap().occurrence_id(),
            expected.head().unwrap().occurrence_id()
        );
        for original in expected.context().chronology().iter().skip(1) {
            assert!(
                child
                    .context()
                    .chronology()
                    .iter()
                    .any(|event| event.record() == original.record()
                        && event.block() == original.block())
            );
        }
        admit_and_bootstrap_child(service, pane, &cid);
        assert_eq!(
            history(&store, &cid).context().chronology(),
            expected.context().chronology()
        );
        assert!(service.agent_token_usage_for_conversation(&cid).is_empty());
        let stored: serde_json::Value = serde_json::from_slice(
            &fs::read(store.root().join(&cid).join("compaction-epoch.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(stored["manifest"]["version"], 9);
        assert!(stored["manifest"]["copy_origin"].is_null());
        assert_eq!(
            stored["manifest"]["memory_copy_origin"]["conversation"],
            "nostore-parent"
        );
        assert!(
            stored["manifest"]["memory_copy_origin"]
                .get("identity")
                .is_none()
        );
        assert!(
            store
                .inspect(&cid)
                .unwrap()
                .iter()
                .any(|row| row.content.contains("MEMORY_ONLY_FINAL") && row.turn_id == memory_turn)
        );
        assert!(
            !store
                .root()
                .join("nostore-parent")
                .join("compaction-epoch.json")
                .exists()
        );
        store.delete(&conversation).unwrap();
        assert_eq!(
            history(&store, &cid).context().chronology(),
            expected.context().chronology()
        );
    });
}

/// A running NoStore task contributes its unsaved instruction. The worker must
/// use that capture after ancestor deletion and refuse a changed live owner.
#[test]
fn nostore_subagent_fork_freezes_active_source_and_rejects_later_parent_progress() {
    let (mut service, store, parent_turn, conversation) = published_source();
    native_scenario(&mut service, |service| {
        let (_expected, memory_turn) =
            prepare_nostore_parent(service, &store, &parent_turn, &conversation);
        let turn = service
            .start_agent_prompt_turn("%1", "UNSAVED_ACTIVE_FORK_TASK")
            .unwrap();
        assert_eq!(turn.state, AgentTurnState::Running);
        let work = service
            .prepare_subagent_fork_read_work("agent-%1")
            .unwrap()
            .unwrap();
        store.delete(&conversation).unwrap();
        let captured = work.execute().unwrap();
        work.check_owner(service).unwrap();
        assert!(
            captured
                .entries
                .iter()
                .any(|row| row.content.contains("UNSAVED_ACTIVE_FORK_TASK")
                    && row.turn_id == turn.turn_id)
        );
        assert!(
            captured
                .entries
                .iter()
                .any(|row| row.content.contains("MEMORY_ONLY_FINAL") && row.turn_id == memory_turn)
        );
        let profile = service
            .agent_turn_model_profile(&turn.turn_id)
            .unwrap()
            .clone();
        service
            .execute_agent_turn_with_provider(
                &turn.turn_id,
                &RuntimeBatchProvider {
                    response: runtime_say_response(
                        &turn.turn_id,
                        "AFTER_FROZEN_WORKER_CAPTURE",
                        false,
                    ),
                },
                profile,
            )
            .unwrap();
        assert!(
            service.agent_turn_contexts()[&turn.turn_id]
                .blocks()
                .iter()
                .any(|block| block.content.contains("AFTER_FROZEN_WORKER_CAPTURE"))
        );
        assert!(work.check_owner(service).is_err());
        assert!(
            !captured
                .entries
                .iter()
                .any(|row| row.content.contains("AFTER_FROZEN_WORKER_CAPTURE"))
        );
        assert!(
            !store
                .root()
                .join("nostore-parent")
                .join("compaction-epoch.json")
                .exists()
        );
    });
}

/// A raw NoStore source has no summary to publish or copy. Its actual task
/// boundaries and original execution lifecycle must survive the child's real
/// initial receipt, startup and cold replay without a fabricated epoch head.
#[test]
fn nostore_subagent_fork_before_first_head_preserves_raw_lifecycle() {
    let (mut service, store, turn_id) = provider_limit_replay::recovery_source(
        "ORIGINAL_RAW_PARENT_TASK",
        "ORIGINAL_RAW_PARENT_FINAL",
        10,
    );
    native_scenario(&mut service, |service| {
        let profile = service.agent_turn_model_profile(&turn_id).unwrap().clone();
        service
            .execute_agent_turn_with_provider(
                &turn_id,
                &RuntimeBatchProvider {
                    response: runtime_say_response(&turn_id, "SECOND_RAW_PARENT_FINAL", true),
                },
                profile,
            )
            .unwrap();
        let ancestor = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone();
        service
            .agent_shell_store_mut()
            .bind_ephemeral_conversation_with_lineage("%1", "raw-nostore-parent", 0, None)
            .unwrap();
        let first = service
            .start_agent_prompt_turn("%1", "ORIGINAL_NOSTORE_RAW_TASK")
            .unwrap();
        let profile = service
            .agent_turn_model_profile(&first.turn_id)
            .unwrap()
            .clone();
        service
            .execute_agent_turn_with_provider(
                &first.turn_id,
                &RuntimeBatchProvider {
                    response: runtime_say_response(
                        &first.turn_id,
                        "ORIGINAL_NOSTORE_RAW_FINAL",
                        true,
                    ),
                },
                profile,
            )
            .unwrap();
        let active = service
            .start_agent_prompt_turn("%1", "UNSAVED_RAW_PARENT_TASK")
            .unwrap();
        let profile = service
            .agent_turn_model_profile(&active.turn_id)
            .unwrap()
            .clone();
        service
            .execute_agent_turn_with_provider(
                &active.turn_id,
                &RuntimeBatchProvider {
                    response: runtime_say_response(
                        &active.turn_id,
                        "UNSAVED_RAW_PARENT_SAY",
                        false,
                    ),
                },
                profile,
            )
            .unwrap();
        let expected = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new("raw-nostore-parent").unwrap(),
            service.agent_turn_contexts()[&active.turn_id].clone(),
        )
        .unwrap();
        assert!(expected.head().is_none());
        assert!(!expected.execution_states().unwrap().is_empty());
        service.start_initial_pane_process(Some("cat")).unwrap();
        service.use_transcript_effect_adapter();
        let primary = service
            .session
            .clients()
            .iter()
            .find(|client| service.session.is_attached_primary(&client.id))
            .unwrap()
            .id
            .clone();
        let response = service
            .spawn_runtime_subagent(
                &primary,
                SubagentSpawnRequest {
                    parent_agent_id: "agent-%1".into(),
                    requested_role: "explorer".into(),
                    placement: "new-pane".into(),
                    cooperation_mode: CooperationMode::ExploreOnly,
                    cooperation_mode_defaulted: false,
                    read_scopes: vec![],
                    read_scopes_defaulted: false,
                    write_scopes: vec![],
                    write_scopes_defaulted: false,
                    session_mode: mez_agent::SubagentSessionMode::Fork,
                    initial_model_size: None,
                    initial_reasoning_effort: None,
                    task_prompt: "continue original raw history".into(),
                    explicit_user_approval: false,
                    skip_initial_turn: false,
                },
                RuntimeSubagentPlacement::NewPane {
                    direction: SplitDirection::Vertical,
                    select: true,
                },
            )
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let pane = value["pane"]["pane_id"].as_str().unwrap();
        let cid = service
            .agent_shell_store()
            .get(pane)
            .unwrap()
            .session_id
            .clone();
        admit_and_bootstrap_child(service, pane, &cid);
        let count = store
            .summary(&cid)
            .unwrap()
            .unwrap()
            .entries
            .try_into()
            .unwrap();
        let cold = history_with_count(&store, &cid, count);
        assert!(cold.head().is_none());
        assert_eq!(cold.revision().get(), 0);
        assert_eq!(cold.context().chronology(), expected.context().chronology());
        assert!(
            cold.checkpoint()
                .unwrap()
                .with_stable_records(Vec::new())
                .unwrap()
                == expected
                    .checkpoint()
                    .unwrap()
                    .for_conversation_copy(mez_agent::ReplayConversationId::new(&cid).unwrap())
                    .unwrap()
                    .with_stable_records(Vec::new())
                    .unwrap()
        );
        assert!(
            store
                .inspect(&cid)
                .unwrap()
                .iter()
                .any(|row| row.content.contains("UNSAVED_RAW_PARENT_SAY")
                    && row.turn_id == active.turn_id)
        );
        assert!(!store.root().join("raw-nostore-parent").exists());
        assert!(
            !store
                .root()
                .join(&cid)
                .join("compaction-epoch.json")
                .exists()
        );
        store.delete(&ancestor).unwrap();
        assert_eq!(
            history_with_count(&store, &cid, count)
                .context()
                .chronology(),
            expected.context().chronology()
        );
        let child_turn = value["turn"]["id"].as_str().unwrap();
        let error = MezError::invalid_state("provider context length exceeded")
            .with_provider_failure_json(
                r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
            );
        assert!(
            service
                .recover_agent_provider_context_limit_failure(
                    &AgentId::opaque(format!("agent-{pane}")).unwrap(),
                    child_turn,
                    &error,
                    1,
                )
                .unwrap()
        );
        complete_runtime_test_compaction(service, pane, "first independent raw-child head");
        let completion = replay_publication::execute_pending_publication(service);
        assert!(
            service
                .apply_replay_epoch_completion(completion)
                .unwrap()
                .applied
        );
        let after = &service.agent_turn_contexts()[child_turn];
        assert_eq!(
            after.chronology()[0].block().content,
            "first independent raw-child head"
        );
        let reopened = history(&store, &cid);
        assert_eq!(reopened.revision().get(), 1);
        assert_eq!(reopened.context().chronology(), after.chronology());
        let bounded = store
            .conversation_replay_snapshot(&cid, &[], |_| {
                Some((
                    crate::storage::transcript::ConversationTranscriptRead::All,
                    true,
                ))
            })
            .unwrap();
        assert_eq!(
            bounded.replay.as_ref().unwrap().covered_archive_frontier(),
            count
        );
        assert!(
            bounded.view.logical.is_empty(),
            "raw initial rows must have original indexed images"
        );
    });
}

/// Failures before and after the copy's manifest swap belong to the original
/// fork generation's compensation. The parent stays authoritative and the same
/// target id can be retried without adopting orphaned files or another epoch.
#[test]
fn saved_replay_fork_faults_compensate_only_the_new_target() {
    use crate::storage::transcript::replay_epoch::Phase;
    let (_service, store, _turn, conversation) = published_source();
    let parent = history(&store, &conversation);
    let path = store
        .root()
        .join(&conversation)
        .join("compaction-epoch.json");
    let original = fs::read(&path).unwrap();
    let archive = store.inspect(&conversation).unwrap();
    for phase in [
        Phase::BeforePayloadSync,
        Phase::BeforeSourceReceiptSync,
        Phase::BeforeManifestSwap,
        Phase::AfterManifestSwap,
        Phase::BeforeRequiredMarker,
        Phase::BeforeAcknowledgment,
    ] {
        store.inject_replay_epoch_fault(phase, 1, false);
        assert!(
            store
                .fork(&conversation, "failed-replay-copy", 123)
                .is_err()
        );
        assert!(!store.root().join("failed-replay-copy").exists());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(store.inspect(&conversation).unwrap(), archive);
        assert_eq!(
            history(&store, &conversation).context().chronology(),
            parent.context().chronology()
        );
        store
            .fork(&conversation, "failed-replay-copy", 123)
            .unwrap();
        assert_eq!(
            history(&store, "failed-replay-copy").context().chronology(),
            parent.context().chronology()
        );
        store.delete("failed-replay-copy").unwrap();
    }
}
