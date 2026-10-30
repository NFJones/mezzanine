//! Accepted log custody across cancellation, correction and joined settlement.
//!
//! Action workers settle independently; presentation must drain accepted source
//! in response order without repeating a worker merely to reconstruct output.

use super::*;
use crate::runtime::CopyMode;
use crate::storage::transcript::activity::{
    ACTIVITY_CONTENT_TYPE, ActivityComponentKind, ActivitySource,
};
use mez_core::presentation::PresentationEntry;
use std::collections::BTreeSet;

/// Production predecessor whose settlement releases a deferred response suffix.
#[derive(Clone, Copy)]
enum DeferredPredecessor<'a> {
    /// A real native command whose outcome arrives after cancellation.
    Native,
    /// Native work with explicit no-store policy for a timer-only actor lane.
    NativeWithoutStore,
    /// Native work followed by an unverified nonstreaming final response.
    NativeWithFinal,
    /// One or more normal task-lifetime joined children.
    Joined(usize),
    /// A real bounded timeout after an independently observable file effect.
    TimedOut(&'a std::path::Path),
}

/// Exact accepted response/action ordinal before any terminal handoff.
type DiscoveryOwner = (
    String,
    String,
    String,
    mez_agent::ContextExecutionGroupId,
    usize,
);
/// Immutable approved header, action and original local acceptance entry.
type DiscoveryHeader = (mez_agent::AgentAction, String, Option<PresentationEntry>);

/// Original accepted source used to check pixels, copy and durable identity.
struct DiscoverySource {
    /// Exact conversation and response binding, including the action ordinal.
    owner: DiscoveryOwner,
    /// Unmodified queued bytes and original observation metadata.
    header: DiscoveryHeader,
}

/// Captures an already approved discovery component without draining it.
fn discovery_source(service: &RuntimeSessionService) -> DiscoverySource {
    let (owner, header) = service
        .presentation
        .agent_queued_provider_headers
        .iter()
        .find(|(_, header)| header.0.id == "accepted-discovery")
        .unwrap();
    DiscoverySource {
        owner: owner.clone(),
        header: header.clone(),
    }
}

/// Checks accepted identity in semantic copy, styled rows and durable activity.
/// A later retry may install the source but cannot resample its original entry,
/// duplicate persistence, substitute another action, or lose its conversation.
fn assert_discovery_source(service: &RuntimeSessionService, accepted: &DiscoverySource) {
    let expected = accepted.header.2.as_ref().unwrap();
    let screen = service.agent_pane_screen("%1").unwrap();
    let copy = CopyMode::from_screen(screen, 24)
        .unwrap()
        .lines()
        .join("\n");
    assert_eq!(copy.matches(&accepted.header.1).count(), 1, "{copy}");
    assert!(screen.normal_styled_content_lines().iter().any(|row| {
        row.presentation
            .as_ref()
            .is_some_and(|entry| entry.id == expected.id && entry.occurrence == expected.occurrence)
    }));
    if let Some(store) = service.agent_transcript_store() {
        let records = store.inspect_presentation(&accepted.owner.2).unwrap();
        let headers = records
            .iter()
            .filter_map(|entry| {
                if entry.source_content_type.as_deref() != Some(ACTIVITY_CONTENT_TYPE) {
                    return None;
                }
                let source = ActivitySource::decode(entry.source_text.as_deref().unwrap()).unwrap();
                (source.kind == ActivityComponentKind::Header
                    && source.action_id.as_deref() == Some("accepted-discovery"))
                .then_some((entry, source))
            })
            .collect::<Vec<_>>();
        assert_eq!(headers.len(), 1, "accepted header must persist once");
        let (entry, source) = &headers[0];
        assert_eq!(entry.presentation.as_ref(), Some(expected));
        assert_eq!(source.source, accepted.header.1);
        assert_eq!(source.conversation_id, accepted.owner.2);
        assert_eq!(source.turn_id, accepted.owner.1);
        assert_eq!(source.response_id, accepted.owner.3.as_str());
        assert_eq!(source.action_ordinal, Some(accepted.owner.4));
    }
}

/// Admits an unstreamed progress barrier followed by an already executed discovery.
/// Native or joined predecessors retain their ordinary runtime owners;
/// accepted discovery publication must survive an intervening terminal handoff.
fn deferred_log_fixture(
    level: AgentLogLevel,
    predecessor: DeferredPredecessor<'_>,
) -> RuntimeSessionService {
    let (joined_children, timeout_marker) = match predecessor {
        DeferredPredecessor::Joined(count) => (count, None),
        DeferredPredecessor::TimedOut(marker) => (0, Some(marker)),
        DeferredPredecessor::Native
        | DeferredPredecessor::NativeWithFinal
        | DeferredPredecessor::NativeWithoutStore => (0, None),
    };
    let hidden_rationale = matches!(level, AgentLogLevel::Normal);
    let mut service = test_runtime_service();
    if timeout_marker.is_none() && !matches!(predecessor, DeferredPredecessor::NativeWithoutStore) {
        service
            .set_agent_transcript_store(AgentTranscriptStore::new(temp_root("deferred-log-owner")));
    }
    let primary = service
        .attach_primary("primary", true, Size::new(120, 32).unwrap(), 200)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", level)
        .unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    service.permission_policy_mut().set_approval_bypass(true);
    if timeout_marker.is_some() {
        service.set_agent_native_shell_timeout_ms(500);
    }
    service
        .execute_agent_shell_command(&primary, "inspect deferred discovery")
        .unwrap();
    service.remove_pending_agent_provider_task("turn-1");
    let mut actions = vec![
        mez_agent::AgentAction {
            id: "pending-native".into(),
            payload: mez_agent::AgentActionPayload::ShellCommand {
                summary: "Native predecessor".into(),
                command: match timeout_marker {
                    Some(marker) => format!(
                        "printf 'once\\n' >> {}; sleep 3",
                        shlex::try_quote(marker.to_str().unwrap()).unwrap()
                    ),
                    None if matches!(predecessor, DeferredPredecessor::Native) => format!(
                        "printf 'completed-native-effect\\n'; while [ ! -e {} ]; do sleep 0.01; done",
                        shlex::try_quote(
                            service
                                .agent_transcript_store()
                                .unwrap()
                                .root()
                                .join("native-release")
                                .to_str()
                                .unwrap()
                        )
                        .unwrap()
                    ),
                    None => "printf 'completed-native-effect\\n'; sleep 0.02".into(),
                },
                interactive: false,
                stateful: false,
                timeout_ms: Some(if timeout_marker.is_some() { 500 } else { 5_000 }),
            },
        },
        mez_agent::AgentAction {
            id: "unverified-progress".into(),
            payload: mez_agent::AgentActionPayload::Say {
                status: mez_agent::SayStatus::Progress,
                text: "UNVERIFIED_DEFERRED_PROGRESS".into(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
            },
        },
        mez_agent::AgentAction {
            id: "accepted-discovery".into(),
            payload: mez_agent::AgentActionPayload::ListAgents {
                agent_type: None,
                scope: None,
            },
        },
    ];
    if joined_children > 0 {
        actions.splice(
            ..1,
            (0..joined_children).map(|index| {
                runtime_spawn_agent_action(
                    &format!("joined-{index}"),
                    "inspect the generated fixture",
                )
            }),
        );
        actions.insert(
            actions.len() - 1,
            mez_agent::AgentAction {
                id: "second-progress".into(),
                payload: mez_agent::AgentActionPayload::Say {
                    status: mez_agent::SayStatus::Progress,
                    text: "SECOND_JOINED_PROGRESS".into(),
                    content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
                },
            },
        );
    }
    if matches!(predecessor, DeferredPredecessor::NativeWithFinal) {
        actions.push(mez_agent::AgentAction {
            id: "unverified-final".into(),
            payload: mez_agent::AgentActionPayload::Say {
                status: mez_agent::SayStatus::Final,
                text: "UNVERIFIED_FINAL_OUTPUT".into(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
            },
        });
    }
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: if hidden_rationale {
                    "UNVERIFIED_DEFERRED_PROGRESS"
                } else {
                    "Keep accepted discovery through cancellation"
                }
                .into(),
                actions,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert!(
        execution
            .action_results
            .iter()
            .any(|result| result.action_id == "accepted-discovery"
                && result.status == ActionStatus::Succeeded)
    );
    assert!(
        service
            .presentation
            .agent_queued_provider_headers
            .keys()
            .any(|key| key.1 == "turn-1"
                && key.4
                    == if joined_children == 0 {
                        2
                    } else {
                        joined_children + 2
                    })
    );
    service
}

/// Exercises both terminal lifecycle owners after actual native execution but
/// before its callback. Cancellation may retire unverified speech, never the
/// already accepted discovery source. Late worker callbacks must remain inert.
fn exercise_deferred_cancellation(without_session: bool, hold_install: bool) {
    for level in [AgentLogLevel::Normal, AgentLogLevel::Verbose] {
        let mut service = deferred_log_fixture(level, DeferredPredecessor::Native);
        let dispatch = service
            .claim_native_shell_action("turn-1", "pending-native")
            .unwrap()
            .unwrap();
        let marker = dispatch.marker.clone();
        let (progress_sender, mut progress_receiver) = tokio::sync::watch::channel(None);
        let release = service
            .agent_transcript_store()
            .unwrap()
            .root()
            .join("native-release");
        let worker = std::thread::spawn(move || {
            crate::runtime::execute_native_shell_dispatch_with_progress(dispatch, progress_sender)
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let (revision, text) = loop {
            if let Some(output) = progress_receiver
                .borrow_and_update()
                .clone()
                .and_then(|progress| progress.output)
            {
                break output;
            }
            if std::time::Instant::now() >= deadline {
                fs::write(&release, "release").unwrap();
                let outcome = worker.join().unwrap();
                service.terminate_all_pane_processes().unwrap();
                panic!("native fixture produced no live progress: {outcome:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        let applied =
            service.apply_native_shell_progress(crate::runtime::RuntimeNativeShellProgress {
                presentation: mez_agent::ActionPresentationProgress::new(
                    "turn-1",
                    "pending-native",
                    mez_agent::ActionPresentationExecutionIdentity::Attempt(marker),
                    revision,
                    mez_agent::ActionPresentationComponentIdentity::ShellOutput,
                    text,
                ),
            });
        let transient = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        // Release and observe the real worker before asserting the UI result, so
        // a fixture assertion cannot abandon its private native process.
        fs::write(&release, "release").unwrap();
        let outcome = worker.join().unwrap();
        assert_eq!(
            applied.unwrap(),
            !level.shows_shell_view(),
            "native preview acceptance follows the selected display policy"
        );
        if !level.shows_shell_view() {
            assert!(
                transient.matches("completed-native-effect").count() >= 2,
                "actual output must appear as a transient tail: {transient}"
            );
        }
        let turn = service.agent_turn_ledger().turn("turn-1").unwrap().clone();
        let accepted = discovery_source(&service);
        service.hold_agent_presentation_installs_for_tests(hold_install);
        if without_session {
            service
                .agent_shell_store_mut()
                .finish_turn("%1", "turn-1")
                .unwrap();
            service
                .finish_agent_turn_without_shell_session(&turn, AgentTurnState::Interrupted)
                .unwrap();
        } else {
            service.stop_agent_turn_for_pane("%1").unwrap();
        }
        let late_applied = service.complete_native_shell_action(outcome).unwrap();
        if hold_install {
            assert!(!service.agent_turn_executions().contains_key("turn-1"));
            for _ in 0..3 {
                service.retry_completion_publications();
                assert_eq!(
                    service
                        .presentation
                        .agent_queued_provider_headers
                        .get(&accepted.owner),
                    Some(&accepted.header)
                );
                assert!(!service.presentation.agent_provider_log_handoffs.is_empty());
                assert!(
                    service
                        .presentation
                        .agent_provider_log_handoffs
                        .values()
                        .all(|owner| owner.failure.is_some())
                );
            }
            service.hold_agent_presentation_installs_for_tests(false);
            for _ in 0..3 {
                service.retry_completion_publications();
            }
            assert!(service.presentation.agent_provider_log_handoffs.is_empty());
        }
        let rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        let pending = service
            .presentation
            .agent_queued_provider_headers
            .keys()
            .any(|key| key.1 == "turn-1");
        assert_discovery_source(&service, &accepted);
        let root = service
            .agent_transcript_store()
            .unwrap()
            .root()
            .to_path_buf();
        service.terminate_all_pane_processes().unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(
            !late_applied,
            "cancelled native delivery cannot regain authority"
        );
        assert!(!rows.contains("UNVERIFIED_DEFERRED_PROGRESS"), "{rows}");
        assert_eq!(
            rows.matches("list agents: type=primary scope=project")
                .count(),
            1,
            "{rows}; pending={pending}; handoffs={:?}",
            service
                .presentation
                .agent_provider_log_handoffs
                .keys()
                .collect::<Vec<_>>()
        );
        assert!(
            !pending,
            "successful cancellation drain must consume accepted source"
        );
    }
}

/// Normal Stop must preserve accepted discovery behind retired unverified speech.
/// This uses production admission, real native execution and the ordinary Stop
/// lifecycle rather than manually asking the ordered presenter to flush.
#[test]
fn runtime_deferred_logs_survive_stop_handoff() {
    exercise_deferred_cancellation(false, false);
}

/// The no-session terminal entry point must obey the same accepted-source rule.
/// Its independent completion/cleanup path must neither drop discovery nor let
/// a late native callback publish unverified progress after interruption.
#[test]
fn runtime_deferred_logs_survive_detached_terminal_handoff() {
    exercise_deferred_cancellation(true, false);
}

/// Stop may finish while display installation is unavailable. Exact accepted
/// header source and entry identity must survive three failed recovery attempts
/// after live execution removal, then install once without regaining authority.
#[test]
fn runtime_deferred_logs_survive_stop_publication_failure() {
    exercise_deferred_cancellation(false, true);
}

/// The detached terminal owner must also transfer failed optional publication.
/// Cleanup cannot delete immutable source merely because the ledger is terminal;
/// healthy bounded recovery must drain it without a worker or provider replay.
#[test]
fn runtime_deferred_logs_survive_detached_publication_failure() {
    exercise_deferred_cancellation(true, true);
}

/// Completes normally admitted child turns without manually touching the parent
/// presenter. Each child result must update the parent's canonical predecessor
/// and immediately release all eligible progress before any later parent event.
fn exercise_deferred_join(children: usize, hold_install: bool) {
    let mut service =
        deferred_log_fixture(AgentLogLevel::Normal, DeferredPredecessor::Joined(children));
    let accepted = discovery_source(&service);
    let child_turns = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .filter(|turn| turn.turn_id != "turn-1")
        .cloned()
        .collect::<Vec<_>>();
    if child_turns.len() != children {
        let execution = service.agent_turn_executions().get("turn-1").cloned();
        service.terminate_all_pane_processes().unwrap();
        panic!(
            "expected {children} admitted children, got {}: {execution:?}",
            child_turns.len()
        );
    }
    assert_eq!(
        service.joined_subagent_dependency_count(),
        children,
        "fixture must use ordinary join-policy children"
    );
    let mut early_progress = false;
    for (index, child) in child_turns.iter().rev().enumerate() {
        if hold_install && index + 1 == children {
            service.hold_agent_presentation_entry_for_tests(
                "%1",
                &accepted.header.2.as_ref().unwrap().id,
                true,
            );
        }
        service.remove_pending_agent_provider_task(&child.turn_id);
        let provider = RuntimeBatchProvider {
            response: runtime_say_response_for_agent(
                &child.turn_id,
                &child.agent_id,
                "Child completed.",
                true,
            ),
        };
        service
            .execute_agent_turn_with_provider(
                &child.turn_id,
                &provider,
                runtime_model_profile("runtime-batch", "test"),
            )
            .unwrap();
        if index + 1 < children {
            early_progress |= service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_content_lines()
                .join("\n")
                .contains("UNVERIFIED_DEFERRED_PROGRESS");
        }
    }
    if hold_install {
        for _ in 0..3 {
            service
                .reconcile_agent_runtime_progress_paths_with_actor_progress(&BTreeSet::new())
                .unwrap();
            assert!(!service.agent_provider_task_is_pending("turn-1"));
            assert!(!service.presentation.agent_provider_log_handoffs.is_empty());
            assert_eq!(
                service
                    .presentation
                    .agent_queued_provider_headers
                    .get(&accepted.owner),
                Some(&accepted.header)
            );
        }
        service.hold_agent_presentation_entry_for_tests(
            "%1",
            &accepted.header.2.as_ref().unwrap().id,
            false,
        );
        for _ in 0..3 {
            service
                .reconcile_agent_runtime_progress_paths_with_actor_progress(&BTreeSet::new())
                .unwrap();
        }
        assert!(service.agent_provider_task_is_pending("turn-1"));
    }
    let immediate = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    for child in &child_turns {
        service
            .emit_subagent_task_result_for_state(child, AgentTurnState::Completed)
            .unwrap();
    }
    assert_discovery_source(&service, &accepted);
    if hold_install {
        // Publication recovery retains an ordinary completion group until a
        // genuine actor lease accepts it. A direct synchronous poll cannot
        // acknowledge that transfer merely by producing another response.
        service = std::thread::Builder::new()
            .name("deferred-join-continuation".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(run_deferred_join_actor(service))
            })
            .unwrap()
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload));
        let rows = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert!(
            rows.find(&accepted.header.1).unwrap() < rows.find("NEW_PARENT_RESPONSE").unwrap(),
            "{rows}"
        );
    } else {
        service.stop_agent_turn_for_pane("%1").unwrap();
    }
    assert_discovery_source(&service, &accepted);
    let root = service
        .agent_transcript_store()
        .unwrap()
        .root()
        .to_path_buf();
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(
        !early_progress,
        "later child cannot release an earlier unfinished join"
    );
    let mut prior = 0;
    for text in [
        "UNVERIFIED_DEFERRED_PROGRESS",
        "SECOND_JOINED_PROGRESS",
        "list agents: type=primary scope=project",
    ] {
        assert_eq!(immediate.matches(text).count(), 1, "{immediate}");
        let position = immediate.find(text).unwrap();
        assert!(position >= prior, "{immediate}");
        prior = position;
    }
}

/// Transfers held joined completion through a real actor claim, then applies
/// one offline lower-runner response through actual generation-qualified ingress.
/// Returns the actor's service after orderly shutdown for the owning log test's
/// original pixel, copy, source-identity and durable-history assertions.
async fn run_deferred_join_actor(mut service: RuntimeSessionService) -> RuntimeSessionService {
    use crate::host::async_runtime::{
        AsyncRuntimeActorConfig, AsyncRuntimeSessionActor, AsyncRuntimeSideEffectServiceConfig,
        run_async_persistence_side_effect_service,
    };
    use crate::integrations::agent::actions::AgentTurnRunner;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let config_root = service
        .agent_transcript_store()
        .unwrap()
        .root()
        .join("actor-config");
    // Configured catalogs are real sandbox read scopes. Materialize this
    // fixture's empty catalogs before provider preparation validates them.
    for catalog in ["macros", "skills"] {
        fs::create_dir_all(config_root.join(catalog)).unwrap();
    }
    service.set_config_root(config_root.clone());
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&config_root),
    ));
    service.replace_config_layers(vec![ConfigLayer {
        name: "deferred-join-actor".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"default\"\n[providers.runtime-batch]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:9/v1\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.default]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=128000\n[permissions]\napproval_policy=\"auto-allow\"\nsandbox=\"policy-only\"\n".into(),
    }]).unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let persistence_stop = stop.clone();
    let persistence_handle = handle.clone();
    let persistence = async move {
        run_async_persistence_side_effect_service(
            &persistence_handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: u64::MAX,
                drain_limit: 32,
                idle_interval: std::time::Duration::from_millis(5),
            },
            move |_, state| {
                persistence_stop.load(Ordering::SeqCst)
                    || matches!(state, crate::runtime::RuntimeLifecycleState::Stopping)
            },
        )
        .await
        .unwrap()
    };
    let client = async {
        let dispatch = handle
            .claim_configured_agent_provider_task(
                AgentId::opaque("agent-%1").unwrap(),
                "turn-1".into(),
            )
            .await
            .unwrap();
        let Some(dispatch) = dispatch else {
            stop.store(true, Ordering::SeqCst);
            return false;
        };
        let permissions = crate::security::permissions::ProductPermissionPlanning::new(
            &dispatch.permission_policy,
            &dispatch.session_approvals,
            dispatch.path_scopes.as_ref(),
        )
        .with_shell_classification(&dispatch.shell_classification)
        .with_sandbox_first_local_prompts(dispatch.sandbox_first_local_prompts)
        .with_macro_bridge_recipients(dispatch.macro_bridge_recipients.clone());
        let provider = RuntimeBatchProvider {
            response: runtime_say_response_for_agent(
                "turn-1",
                "agent-%1",
                "NEW_PARENT_RESPONSE",
                true,
            ),
        };
        let runner = AgentTurnRunner {
            provider: &provider,
            model_profile: dispatch.model_profile.clone(),
            permissions: &permissions,
            subagent_scope: dispatch.subagent_scope.as_ref(),
            subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
            available_mcp_servers: dispatch.available_mcp_servers.clone(),
            available_mcp_tools: &dispatch.available_mcp_tools,
            memory_actions_enabled: dispatch.memory_actions_enabled,
            issue_actions_enabled: dispatch.issue_actions_enabled,
        };
        let execution = runner
            .run_turn_ref_with_allowed_actions(
                &mut mez_agent::AgentTurnLedger::new(false),
                dispatch.turn.clone(),
                dispatch.context.durable(),
                dispatch.allowed_actions.clone(),
                dispatch.interaction_kind,
                dispatch.turn_limits,
            )
            .unwrap();
        assert_eq!(execution.terminal_state, AgentTurnState::Completed);
        let mut batch = crate::runtime::RuntimeEventBatch::new();
        batch.push(crate::runtime::RuntimeEvent::AgentProvider(
            crate::runtime::AgentProviderEvent::Completed {
                agent_id: AgentId::opaque(dispatch.turn.agent_id.clone()).unwrap(),
                turn_id: dispatch.turn.turn_id.clone(),
                claim_generation: dispatch.claim_generation,
                execution: Box::new(execution),
            },
        ));
        assert_eq!(
            handle.submit_runtime_events(batch).await.unwrap().applied,
            1
        );
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while handle.agent_turn_is_running("turn-1").await.unwrap()
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let completed = !handle.agent_turn_is_running("turn-1").await.unwrap();
        stop.store(true, Ordering::SeqCst);
        completed
    };
    // The persistence lane drains its actual typed receipts before shutdown;
    // merely accepting a provider event is not a completed archive handoff.
    let lanes = async {
        let (claimed, report) = tokio::join!(client, persistence);
        handle.shutdown().await.unwrap();
        (claimed, report)
    };
    let ((claimed, report), mut exit) =
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            tokio::join!(lanes, actor.run())
        })
        .await
        .expect("one genuinely admitted offline continuation must settle");
    assert_eq!(report.failed, 0, "{report:?}");
    assert!(
        report.completed > 0,
        "the actual persistence worker must settle: {report:?}"
    );
    if !claimed {
        let detail = format!(
            "turn={:?}; queue={:?}; screen={:?}",
            exit.service.agent_turn_ledger().turn("turn-1"),
            exit.service
                .completion_provider_queue_flags_for_tests("turn-1"),
            exit.service
                .agent_pane_screen("%1")
                .map(|screen| screen.normal_content_lines())
        );
        exit.service.terminate_all_pane_processes().unwrap();
        panic!("released original publication must admit a real actor lease: {detail}");
    }

    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn("turn-1")
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    assert!(!exit.service.agent_provider_task_is_pending("turn-1"));
    exit.service
}

/// A successful joined child must release multiple nonstreaming progress rows
/// and their accepted discovery successor in the child's own settlement event.
#[test]
fn runtime_deferred_logs_release_on_joined_child_settlement() {
    exercise_deferred_join(1, false);
}

/// Reverse child completion must retain the earlier join barrier, then release
/// every eligible parent component once when the final predecessor settles.
#[test]
fn runtime_deferred_logs_release_after_reverse_joined_children() {
    exercise_deferred_join(2, false);
}

/// A failed exact parent entry must retain source and delay provider reacquisition
/// while unrelated child/status publication stays healthy. Three failures then
/// normal coordinator recovery must release a new parent response only after
/// accepted old logs, without replaying child results or persisted components.
#[test]
fn runtime_deferred_logs_join_reacquisition_waits_for_publication() {
    exercise_deferred_join(2, true);
}

/// Real native timeout must publish accepted discovery before a distinct normal
/// correction response, without replaying the command. The actual actor claim
/// and provider worker protect continuation identity; no synchronous test-only
/// continuation bypass or paid provider is used. Debug actor stacks follow the
/// established native-worker fixture convention on both supported platforms.
#[test]
fn runtime_deferred_logs_precede_actual_native_timeout_correction() {
    std::thread::Builder::new()
        .name("deferred-timeout-correction".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            for hold in [false, true] {
                runtime.block_on(run_deferred_timeout_actor(hold));
            }
        })
        .unwrap()
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload));
}

/// Runs a real bounded native failure and one generated local correction model.
/// The effect file, ordered pane rows and request counter independently establish
/// command-once, publication-before-correction and ordinary continuation custody.
async fn run_deferred_timeout_actor(hold_install: bool) {
    use crate::host::async_runtime::{
        AsyncAgentProviderServiceConfig, AsyncRuntimeActorConfig, AsyncRuntimeSessionActor,
        AsyncRuntimeSideEffectServiceConfig, run_async_agent_provider_service,
        run_async_runtime_timer_side_effect_service,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = std::env::temp_dir().join(format!(
        "mez-deferred-timeout-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    fs::create_dir(&root).unwrap();
    let marker = root.join("effects");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut service = deferred_log_fixture(
        AgentLogLevel::Verbose,
        DeferredPredecessor::TimedOut(&marker),
    );
    let native = service
        .claim_native_shell_action("turn-1", "pending-native")
        .unwrap()
        .unwrap();
    assert_eq!(native.request.timeout_ms, Some(500));
    let outcome = crate::runtime::execute_native_shell_dispatch(native);
    assert!(
        outcome.result.as_ref().is_ok_and(|output| output.timed_out),
        "fixture must actually time out: {outcome:?}"
    );
    service.hold_agent_presentation_installs_for_tests(hold_install);
    service
        .complete_native_shell_action(outcome.clone())
        .unwrap();
    if hold_install {
        for _ in 0..3 {
            service.retry_completion_publications();
            assert!(!service.agent_provider_task_is_pending("turn-1"));
        }
        service.hold_agent_presentation_installs_for_tests(false);
    }
    for _ in 0..4 {
        service.retry_completion_publications();
    }
    let before_correction = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(service.agent_provider_task_is_pending("turn-1"));
    assert!(!service.complete_native_shell_action(outcome).unwrap());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "offline-native-continuation".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                r#"[permissions]
sandbox = "policy-only"
[agents]
default_provider = "runtime-batch"
[providers.runtime-batch]
kind = "openai-compatible"
base_url = "{endpoint}"
models = ["test"]
default_model = "test"
[providers.runtime-batch.options]
maap_output = "structured_json"
structured_output = "json_schema"
[model_profiles.default]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
"#
            ),
        }])
        .unwrap();
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root.join("auth")),
    ));
    service.permission_policy_mut().set_approval_bypass(true);
    let first = service
        .pending_agent_provider_tasks()
        .into_iter()
        .next()
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let requests = std::sync::Arc::new(AtomicUsize::new(0));
    let server_stop = stop.clone();
    let request_count = requests.clone();
    let server_marker = marker.clone();
    let server = async move {
        while !server_stop.load(Ordering::SeqCst) {
            let accepted =
                tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept()).await;
            let Ok(Ok((mut socket, _))) = accepted else {
                continue;
            };
            let mut bytes = Vec::new();
            let header_end = loop {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                bytes.push(byte[0]);
                assert!(bytes.len() < 65536);
                if bytes.ends_with(b"\r\n\r\n") {
                    break bytes.len();
                }
            };
            let headers = String::from_utf8_lossy(&bytes);
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse().unwrap())
                })
                .unwrap();
            assert!(length < 1_048_576);
            bytes.resize(header_end + length, 0);
            socket.read_exact(&mut bytes[header_end..]).await.unwrap();
            let request: serde_json::Value = serde_json::from_slice(&bytes[header_end..]).unwrap();
            assert_eq!(
                fs::read_to_string(&server_marker).unwrap(),
                "once\n",
                "provider continued without the timed-out native effect"
            );
            assert_eq!(
                request_count.fetch_add(1, Ordering::SeqCst),
                0,
                "duplicate ordinary continuation"
            );
            let content = serde_json::json!({"rationale":"CORRECTION_RESPONSE","actions":[{"type":"say","status":"final","text":"CORRECTION_COMPLETE","content_type":mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE}]}).to_string();
            mez_agent::parse_maap_action_batch_json(&content).unwrap();
            let (content_type, body) = if request["stream"] == true {
                let chunk = serde_json::json!({"id":"offline-native-pair","model":"test","choices":[{"index":0,"delta":{"role":"assistant","content":content},"finish_reason":null}]}).to_string();
                let end = serde_json::json!({"id":"offline-native-pair","model":"test","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}).to_string();
                (
                    "text/event-stream",
                    format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"),
                )
            } else {
                ("application/json", serde_json::json!({"id":"offline-native-pair","model":"test","choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}]}).to_string())
            };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    };
    let worker_handle = handle.clone();
    let worker_stop = stop.clone();
    let worker = async move {
        run_async_agent_provider_service(
            &worker_handle,
            AsyncAgentProviderServiceConfig::new(1)
                .unwrap()
                .with_idle_interval(std::time::Duration::from_millis(5))
                .unwrap(),
            move |_, state| {
                worker_stop.load(Ordering::SeqCst)
                    || matches!(state, crate::runtime::RuntimeLifecycleState::Stopping)
            },
        )
        .await
        .unwrap()
    };
    let timer_handle = handle.clone();
    let timer_stop = stop.clone();
    let timer = async move {
        run_async_runtime_timer_side_effect_service(
            &timer_handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: u64::MAX,
                drain_limit: 32,
                idle_interval: std::time::Duration::from_millis(5),
            },
            0,
            move |_, state| {
                timer_stop.load(Ordering::SeqCst)
                    || matches!(state, crate::runtime::RuntimeLifecycleState::Stopping)
            },
        )
        .await
        .unwrap()
    };
    let client = async {
        // Native settlement already queued this ordinary correction. Offer only
        // that original provider effect through the production worker lane.
        assert_eq!(
            handle
                .queue_runtime_side_effects(vec![
                    crate::runtime::RuntimeSideEffect::DispatchAgentProvider {
                        agent_id: AgentId::opaque(first.agent_id).unwrap(),
                        turn_id: first.turn_id
                    }
                ])
                .await
                .unwrap(),
            1
        );
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while requests.load(Ordering::SeqCst) == 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        while requests.load(Ordering::SeqCst) > 0
            && handle.agent_turn_is_running("turn-1").await.unwrap()
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        stop.store(true, Ordering::SeqCst);
    };
    // Retire the service lanes before actor shutdown so their final drain
    // requests retain a live reply owner, including on diagnostic failure.
    let lanes = async {
        let ((), report, (), timer_report) = tokio::join!(client, worker, server, timer);
        handle.shutdown().await.unwrap();
        (report, timer_report)
    };
    let ((report, timer_report), mut exit) =
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            tokio::join!(lanes, actor.run())
        })
        .await
        .expect("timed-out native work and one offline correction must settle");
    exit.service.terminate_all_pane_processes().unwrap();
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "pending={:?}; report={report:?}; timer={timer_report:?}; turn={:?}; pane={:?}",
        exit.service.pending_agent_provider_tasks(),
        exit.service.agent_turn_ledger().turn("turn-1"),
        exit.service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
    );
    assert_eq!(report.executions, 1);
    assert_eq!(
        before_correction
            .matches("list agents: type=primary scope=project")
            .count(),
        1,
        "{before_correction}"
    );
    assert!(
        !before_correction.contains("UNVERIFIED_DEFERRED_PROGRESS"),
        "{before_correction}"
    );
    let rows = exit
        .service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(
        rows.matches("list agents: type=primary scope=project")
            .count(),
        1,
        "{rows}"
    );
    assert_eq!(rows.matches("CORRECTION_COMPLETE").count(), 1, "{rows}");
    assert!(
        rows.find("list agents: type=primary scope=project")
            .unwrap()
            < rows.find("CORRECTION_COMPLETE").unwrap(),
        "{rows}"
    );
    assert_eq!(fs::read_to_string(marker).unwrap(), "once\n");
    assert_eq!(
        exit.service
            .agent_turn_ledger()
            .turn("turn-1")
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    assert!(!exit.service.agent_turn_is_running("turn-1"));
    assert!(exit.service.native_shell_progress_turn_ids().is_empty());
    assert!(
        !exit
            .service
            .completion_publication_retry_needed_for_turn("turn-1")
    );
    exit.service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Cancels nonstreaming final-say source through ordinary Stop. Accepted
/// discovery may drain while unverified assistant slots retire; display
/// retirement never establishes the interrupted response's final success.
fn exercise_deferred_unverified_final() {
    let mut service =
        deferred_log_fixture(AgentLogLevel::Verbose, DeferredPredecessor::NativeWithFinal);
    let accepted = discovery_source(&service);
    service.stop_agent_turn_for_pane("%1").unwrap();
    for _ in 0..3 {
        service.retry_completion_publications();
    }
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_discovery_source(&service, &accepted);
    let root = service
        .agent_transcript_store()
        .unwrap()
        .root()
        .to_path_buf();
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(!rows.contains("UNVERIFIED_DEFERRED_PROGRESS"), "{rows}");
    assert!(!rows.contains("UNVERIFIED_FINAL_OUTPUT"), "{rows}");
    assert!(service.pending_native_shell_actions().is_empty());
}

/// Accepted source from a stopped turn can recover while a subsequent real turn
/// awaits approval. Display recovery must preserve that pending decision and
/// never dispatch or perform the unapproved generated-path write.
#[test]
fn runtime_deferred_logs_cancel_pending_approval_without_authorizing_work() {
    let mut service = deferred_log_fixture(AgentLogLevel::Verbose, DeferredPredecessor::Native);
    let accepted = discovery_source(&service);
    let root = service
        .agent_transcript_store()
        .unwrap()
        .root()
        .to_path_buf();
    service.hold_agent_presentation_installs_for_tests(true);
    service.stop_agent_turn_for_pane("%1").unwrap();
    service.hold_agent_presentation_entry_for_tests(
        "%1",
        &accepted.header.2.as_ref().unwrap().id,
        true,
    );
    service.permission_policy_mut().set_approval_bypass(false);
    let started = service
        .start_agent_prompt_turn("%1", "request a generated-path write")
        .unwrap();
    service.remove_pending_agent_provider_task(&started.turn_id);
    let destination = root.join("unapproved-effect");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "Request permission for a generated write".into(),
                actions: vec![mez_agent::AgentAction {
                    id: "approval-command".into(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Request a generated-path write".into(),
                        command: format!(
                            "printf 'requires-approval\\n' > {}",
                            shlex::try_quote(destination.to_str().unwrap()).unwrap()
                        ),
                        interactive: false,
                        stateful: false,
                        timeout_ms: Some(5_000),
                    },
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider(
            &started.turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let pending = service.blocked_approvals().pending().len();
    let unissued = service.pending_native_shell_actions().is_empty();
    service.hold_agent_presentation_entry_for_tests(
        "%1",
        &accepted.header.2.as_ref().unwrap().id,
        false,
    );
    for _ in 0..3 {
        service.retry_completion_publications();
    }
    let pending_after = service.blocked_approvals().pending().len();
    assert_discovery_source(&service, &accepted);
    let effect = destination.exists();
    service.stop_agent_turn_for_pane("%1").unwrap();
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(
        pending > 0,
        "fixture must enter actual approval: {execution:?}"
    );
    assert_eq!(pending_after, pending);
    assert!(unissued, "approval cannot issue native work");
    assert!(
        !effect,
        "unapproved write cannot execute during display recovery"
    );
}

/// A mixed response's unverified final remains unpublished after interruption;
/// preserving accepted discovery is independent of final-say promotion.
#[test]
fn runtime_deferred_logs_cancel_without_promoting_unverified_final() {
    exercise_deferred_unverified_final();
}

/// A held terminal source must retain original activity/copy identity through an
/// intervening row and a real source-backed resize before publication recovery.
#[test]
fn runtime_deferred_logs_survive_intervening_row_and_resize() {
    let mut service = deferred_log_fixture(AgentLogLevel::Verbose, DeferredPredecessor::Native);
    let accepted = discovery_source(&service);
    service.hold_agent_presentation_installs_for_tests(true);
    service.stop_agent_turn_for_pane("%1").unwrap();
    assert!(!service.presentation.agent_provider_log_handoffs.is_empty());
    service.hold_agent_presentation_installs_for_tests(false);
    service
        .append_agent_status_text_to_terminal_buffer("%1", "INTERVENING_ACCEPTED_ROW")
        .unwrap();
    service
        .rebuild_agent_presentation_after_resize("%1", Size::new(61, 20).unwrap())
        .unwrap();
    for _ in 0..3 {
        service.retry_completion_publications();
    }
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_discovery_source(&service, &accepted);
    let root = service
        .agent_transcript_store()
        .unwrap()
        .root()
        .to_path_buf();
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(rows.contains("INTERVENING_ACCEPTED_ROW"), "{rows}");
    assert!(service.presentation.agent_provider_log_handoffs.is_empty());
}

/// Replacement intentionally retires old publication authority. Healthy retry
/// must never append old accepted source into the new conversation or alter its
/// visible/copy rows; explicit retirement must remove the old queue owner.
#[test]
fn runtime_deferred_logs_retire_on_conversation_replacement() {
    let mut service = deferred_log_fixture(AgentLogLevel::Verbose, DeferredPredecessor::Native);
    let accepted = discovery_source(&service);
    service.hold_agent_presentation_installs_for_tests(true);
    service.stop_agent_turn_for_pane("%1").unwrap();
    service
        .agent_shell_store_mut()
        .start_new_conversation("%1")
        .unwrap();
    service.hold_agent_presentation_installs_for_tests(false);
    service
        .append_agent_status_text_to_terminal_buffer("%1", "REPLACEMENT_CONVERSATION")
        .unwrap();
    let before = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    for _ in 0..3 {
        service.retry_completion_publications();
    }
    let after = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    let root = service
        .agent_transcript_store()
        .unwrap()
        .root()
        .to_path_buf();
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
    assert_ne!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        accepted.owner.2
    );
    assert_eq!(before, after);
    assert!(service.presentation.agent_provider_log_handoffs.is_empty());
    assert!(
        !service
            .presentation
            .agent_queued_provider_headers
            .contains_key(&accepted.owner)
    );
}

/// The real idle-cleanup timer lane must drain a terminal response with no live
/// action/execution receipt. Only the initial production timer side effect is
/// offered; no later user event, status query or manual presenter call drives it.
/// The no-store policy isolates timer recovery from persistence-worker coverage.
#[test]
fn runtime_deferred_logs_terminal_handoff_drains_on_idle_timer() {
    std::thread::Builder::new()
        .name("deferred-idle-recovery".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run_deferred_idle_recovery());
        })
        .unwrap()
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload));
}

/// Boots the actual actor/timer lane around an already retained terminal source.
/// Original entry and semantic copy identity must survive its autonomous retry.
async fn run_deferred_idle_recovery() {
    use crate::host::async_runtime::{
        AsyncRuntimeActorConfig, AsyncRuntimeSessionActor, AsyncRuntimeSideEffectServiceConfig,
        run_async_runtime_timer_side_effect_service,
    };
    let mut service = deferred_log_fixture(
        AgentLogLevel::Verbose,
        DeferredPredecessor::NativeWithoutStore,
    );
    let accepted = discovery_source(&service);
    service.hold_agent_presentation_installs_for_tests(true);
    service.stop_agent_turn_for_pane("%1").unwrap();
    assert!(!service.agent_turn_executions().contains_key("turn-1"));
    assert!(service.completion_publication_retry_needed());
    service.hold_agent_presentation_installs_for_tests(false);
    let now = crate::runtime::current_unix_millis();
    let transition = service.idle_cleanup_timer_transition_with_actor_progress(
        &BTreeSet::new(),
        false,
        now,
        1,
        1,
    );
    assert!(!transition.side_effects.is_empty());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let client = async {
        handle
            .queue_runtime_side_effects(transition.side_effects)
            .await
            .unwrap();
        let report = run_async_runtime_timer_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 30,
                drain_limit: 16,
                idle_interval: std::time::Duration::from_millis(2),
            },
            now,
            |polls, _| polls >= 30,
        )
        .await
        .unwrap();
        handle.shutdown().await.unwrap();
        report
    };
    let (report, mut exit) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(client, actor.run())
    })
    .await
    .unwrap();
    exit.service.terminate_all_pane_processes().unwrap();
    assert!(report.fired >= 1, "{report:?}");
    assert!(
        exit.service
            .presentation
            .agent_provider_log_handoffs
            .is_empty()
    );
    assert_discovery_source(&exit.service, &accepted);
}

/// Two native completions arriving in reverse order must release several later
/// progress components exactly once, before terminal cleanup discards ownership.
#[test]
fn runtime_native_settlement_drains_multiple_deferred_logs() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(120, 32).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .execute_agent_shell_command(&primary, "exercise deferred logs")
        .unwrap();
    let shell = |id: &str| mez_agent::AgentAction {
        id: id.to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: format!("run {id}"),
            command: format!(": # {id}"),
            interactive: false,
            stateful: false,
            timeout_ms: Some(5_000),
        },
    };
    let say = |id: &str| mez_agent::AgentAction {
        id: id.to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: id.to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "Verify ordered deferred publication".to_string(),
                actions: vec![
                    shell("FIRST_BARRIER"),
                    say("FIRST_PROGRESS"),
                    say("SECOND_PROGRESS"),
                    shell("SECOND_BARRIER"),
                    say("LAST_PROGRESS"),
                ],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.remove_pending_agent_provider_task("turn-1");
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let first = service
        .claim_native_shell_action("turn-1", "FIRST_BARRIER")
        .unwrap()
        .unwrap_or_else(|| panic!("native dispatch absent: {execution:?}"));
    service
        .dispatch_stored_running_shell_actions("turn-1")
        .unwrap();
    let second = service
        .claim_native_shell_action("turn-1", "SECOND_BARRIER")
        .unwrap()
        .unwrap();
    let before = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!before.contains("FIRST_PROGRESS"));
    let second_outcome = crate::runtime::execute_native_shell_dispatch(second);
    assert!(
        service
            .complete_native_shell_action(second_outcome.clone())
            .unwrap()
    );
    let held = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!held.contains("FIRST_PROGRESS"));
    assert!(!held.contains("LAST_PROGRESS"));
    let first_outcome = crate::runtime::execute_native_shell_dispatch(first);
    assert!(
        service
            .complete_native_shell_action(first_outcome.clone())
            .unwrap()
    );
    assert!(!service.complete_native_shell_action(first_outcome).unwrap());
    assert!(
        !service
            .complete_native_shell_action(second_outcome)
            .unwrap()
    );
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let mut previous = 0;
    for marker in [
        "FIRST_BARRIER",
        "FIRST_PROGRESS",
        "SECOND_PROGRESS",
        "SECOND_BARRIER",
        "LAST_PROGRESS",
    ] {
        assert_eq!(rows.matches(marker).count(), 1, "{rows}");
        let position = rows.find(marker).unwrap();
        assert!(position >= previous, "{rows}");
        previous = position;
    }
    service.terminate_all_pane_processes().unwrap();
}
