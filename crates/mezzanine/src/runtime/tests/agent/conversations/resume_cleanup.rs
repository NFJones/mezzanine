//! Native cleanup ownership across live delegation and saved conversation resume.
//!
//! Fixtures use exclusively created generated stores and runtime-owned PTYs.
//! Assertions exercise actual delegation, durable resume and pane close rather
//! than manufacturing authority by inserting lineage into a map. No provider
//! request is executed and no caller-supplied PID is signaled.

use super::*;

/// Owns one private generated session and its native delegation fixtures.
struct CleanupFixture {
    service: RuntimeSessionService,
    primary: mez_core::ids::ClientId,
    store: AgentTranscriptStore,
    root: PathBuf,
}

impl CleanupFixture {
    /// Creates a new root without deleting or adopting an existing fixture.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mez-r311-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let socket = root.join("c.sock");
        assert!(socket.as_os_str().len() < 108);
        let mut service = RuntimeServiceFixture::new().control_socket(socket).build();
        service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
        let store = AgentTranscriptStore::new(root.join("transcripts"));
        service.set_agent_transcript_store(store.clone());
        let primary = service
            .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
            .unwrap();
        service.start_initial_pane_process(Some("cat")).unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service.configure_subagent_policy(8, 8, 8, 8, SubagentWaitPolicy::Detach);
        Self {
            service,
            primary,
            store,
            root,
        }
    }

    /// Creates an idle reusable child through the real delegation commit path.
    fn spawn(&mut self, parent: &str) -> String {
        let pane = parent.strip_prefix("agent-").unwrap();
        let conversation = self
            .service
            .agent_shell_store()
            .get(pane)
            .unwrap()
            .session_id
            .clone();
        let result = self
            .service
            .spawn_runtime_persistent_subagent_session_owned(
                SubagentSpawnRequest {
                    parent_agent_id: parent.to_string(),
                    requested_role: "worker".to_string(),
                    placement: "new-pane".to_string(),
                    cooperation_mode: CooperationMode::ExploreOnly,
                    cooperation_mode_defaulted: false,
                    read_scopes: Vec::new(),
                    read_scopes_defaulted: false,
                    write_scopes: Vec::new(),
                    write_scopes_defaulted: false,
                    session_mode: mez_agent::SubagentSessionMode::New,
                    initial_model_size: None,
                    initial_reasoning_effort: None,
                    task_prompt: String::new(),
                    explicit_user_approval: false,
                    skip_initial_turn: true,
                },
                RuntimeSubagentPlacement::NewPane {
                    direction: SplitDirection::Vertical,
                    select: false,
                },
                &conversation,
                "generated live cleanup fixture",
            )
            .unwrap();
        let result: serde_json::Value = serde_json::from_str(&result).unwrap();
        result["agent"]["id"].as_str().unwrap().to_string()
    }

    /// Saves only generated conversation material, optionally with historical lineage.
    fn save(&self, conversation: &str, historical_parent: Option<&str>) {
        self.store
            .append(&TranscriptEntry {
                conversation_id: conversation.to_string(),
                sequence: 1,
                created_at_unix_seconds: 1,
                role: TranscriptRole::User,
                turn_id: format!("turn-{conversation}"),
                agent_id: "agent-%99".to_string(),
                pane_id: "%99".to_string(),
                content: "generated resume cleanup fixture".to_string(),
            })
            .unwrap();
        if let Some(parent) = historical_parent {
            self.store
                .save_subagent_conversation_contract(
                    conversation,
                    mez_agent::SubagentSessionLineage {
                        parent_agent_id: parent.to_string(),
                        root_agent_id: "agent-%1".to_string(),
                        depth: 1,
                        display_name: "historical fixture".to_string(),
                        terminal: false,
                    },
                    mez_agent::AllowedActionSet::say_only(),
                )
                .unwrap();
        }
    }

    /// Binds one saved conversation without restarting the original native process.
    fn resume(&mut self, agent: &str, conversation: &str) {
        self.service
            .execute_agent_shell_resume_command(
                agent.strip_prefix("agent-").unwrap(),
                &format!("/resume {conversation}"),
            )
            .unwrap();
    }

    /// Checks actual pane membership, not only a runtime lineage map.
    fn present(&self, agent: &str) -> bool {
        self.service
            .find_pane_descriptor(agent.strip_prefix("agent-").unwrap())
            .is_some()
    }

    /// Executes real pane close only for an exact fixture-owned agent.
    fn close(&mut self, agent: &str) {
        let pane = agent.strip_prefix("agent-").unwrap();
        self.service
            .dispatch_runtime_pane_close(
                &self.primary,
                &format!(r#"{{"pane_id":"{pane}","force":true}}"#),
            )
            .unwrap();
    }
}

impl Drop for CleanupFixture {
    /// Releases only native processes still owned by this generated runtime.
    fn drop(&mut self) {
        self.service.terminate_all_pane_processes().unwrap();
    }
}

/// Verifies genuine direct and transitive delegations still close, even after
/// same-conversation resume. This positive control prevents a historical-edge
/// fix from simply disabling descendant teardown or discarding all fences.
#[test]
fn runtime_cleanup_keeps_same_conversation_live_subtree_obligation() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let grandchild = fixture.spawn(&child);
    let conversation = fixture
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    fixture.service.checkpoint_agent_session_metadata().unwrap();
    fixture.resume("agent-%1", &conversation);
    fixture.close("agent-%1");
    assert!(!fixture.present(&child));
    assert!(!fixture.present(&grandchild));
}

/// Verifies historical-only structure in the middle of an otherwise live
/// delegation chain cuts off ancestor teardown. The independently rebound
/// intermediate still retains cleanup obligations for its own genuine child.
#[test]
fn runtime_cleanup_stops_at_resumed_intermediate_live_edge_boundary() {
    let mut fixture = CleanupFixture::new();
    let intermediate = fixture.spawn("agent-%1");
    let leaf = fixture.spawn(&intermediate);
    fixture.save("historical-intermediate", Some("agent-%1"));
    fixture.resume(&intermediate, "historical-intermediate");
    assert!(
        !fixture
            .service
            .subagent_lineage_has_live_parent_authority(&intermediate)
    );
    fixture.close("agent-%1");
    assert!(fixture.present(&intermediate));
    assert!(
        fixture.present(&leaf),
        "a live leaf must not bridge its restored parent's historical edge"
    );
    fixture.close(&intermediate);
    assert!(
        !fixture.present(&leaf),
        "the original same-owner child obligation must not leak"
    );
}

/// Verifies A's genuine children remain cleanup obligations after A -> B -> C,
/// while an A callback cannot close them after the parent now binds C. Neither
/// parent conversation replacement may overwrite original delegation ownership.
#[test]
fn runtime_cleanup_preserves_first_fenced_owner_and_rejects_stale_callback() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let grandchild = fixture.spawn(&child);
    let original = fixture
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    fixture.save("replacement-b", None);
    fixture.save("replacement-c", None);
    fixture.resume("agent-%1", "replacement-b");
    fixture.resume("agent-%1", "replacement-c");
    assert!(fixture.service.subagent_descendant_is_fenced(&child));
    assert_eq!(
        fixture
            .service
            .close_subagent_descendants_for_parent_conversation(
                "agent-%1",
                &original,
                "stale original conversation callback",
            )
            .unwrap(),
        0
    );
    assert!(fixture.present(&child));
    assert!(fixture.present(&grandchild));
    fixture.close("agent-%1");
    assert!(!fixture.present(&child));
    assert!(!fixture.present(&grandchild));
    assert_eq!(
        fixture
            .service
            .close_subagent_descendants_for_parent_conversation(
                "agent-%1",
                &original,
                "duplicate old callback",
            )
            .unwrap(),
        0
    );
}

/// Verifies a failed resume restores the actual child's cleanup proof as well
/// as its structural state and descendant fences. Teardown must still remove
/// the genuine live subtree after transactional rollback.
#[test]
fn runtime_cleanup_restores_live_obligation_after_failed_resume() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let grandchild = fixture.spawn(&child);
    fixture.save("failed-child-target", Some("agent-%1"));
    fixture
        .service
        .fail_next_agent_resume_after_authority_restore_for_tests();
    assert!(
        fixture
            .service
            .execute_agent_shell_resume_command(
                child.strip_prefix("agent-").unwrap(),
                "/resume failed-child-target",
            )
            .is_err()
    );
    assert!(!fixture.service.subagent_descendant_is_fenced(&grandchild));
    fixture.close("agent-%1");
    assert!(!fixture.present(&child));
    assert!(!fixture.present(&grandchild));
}

/// Verifies the no-layout-owner cleanup branch cannot clear an independently
/// resumed child's shell, MMP registration, or native owner. Duplicate parent
/// callbacks must remain inert instead of treating missing layout as authority
/// to run runtime-state cleanup against a still-present unrelated child.
#[test]
fn runtime_cleanup_without_layout_owner_keeps_independently_resumed_child() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    fixture.save("independent-no-layout", Some("agent-%1"));
    fixture.resume(&child, "independent-no-layout");
    let pane = child.strip_prefix("agent-").unwrap();
    let pid = fixture
        .service
        .primary_pid_for_live_pane_process(pane)
        .unwrap();
    let token = mez_mux::process::process_start_token_for_pid(pid).unwrap();
    let lineage = fixture.service.subagent_lineage(&child).cloned();
    let identity = fixture
        .service
        .ensure_runtime_message_identity(
            &child,
            None,
            "agent",
            &[],
            crate::runtime::current_unix_millis(),
        )
        .unwrap();
    fixture
        .service
        .session
        .detach_client_self(&fixture.primary)
        .unwrap();
    assert!(fixture.service.session().layout_owner_client_id().is_none());
    for _ in 0..2 {
        assert_eq!(
            fixture
                .service
                .close_subagent_descendants_for_parent_agent(
                    "agent-%1",
                    "duplicate parent callback without layout owner",
                )
                .unwrap(),
            0
        );
        assert!(fixture.present(&child));
        assert_eq!(
            fixture
                .service
                .agent_shell_store()
                .get(pane)
                .unwrap()
                .session_id,
            "independent-no-layout"
        );
        assert_eq!(fixture.service.subagent_lineage(&child), lineage.as_ref());
        assert_eq!(
            fixture
                .service
                .message_service()
                .registered_identity(&identity.agent_id),
            Some(&identity)
        );
        assert_eq!(
            fixture.service.primary_pid_for_live_pane_process(pane),
            Some(pid)
        );
        assert_eq!(
            mez_mux::process::process_start_token_for_pid(pid),
            Some(token)
        );
    }
}

/// Verifies ordinary detached task settlement revokes execution authority but
/// does not erase the original cleanup-only obligation. Replacing A with B
/// and then C must still clean the actual task pane when its same owner closes.
#[test]
fn runtime_cleanup_retains_interrupted_task_obligation_without_execution_authority() {
    let mut fixture = CleanupFixture::new();
    let parent = fixture
        .service
        .start_agent_prompt_turn("%1", "delegate generated task")
        .unwrap();
    fixture
        .service
        .remove_pending_agent_provider_task(&parent.turn_id);
    let parent_turn = fixture
        .service
        .agent_turn_ledger()
        .turn(&parent.turn_id)
        .unwrap()
        .clone();
    let spawned = fixture
        .service
        .execute_spawn_action_for_turn(
            &parent_turn,
            &runtime_spawn_agent_action("cleanup-task", "generated detached task"),
        )
        .unwrap();
    let spawned: serde_json::Value =
        serde_json::from_str(spawned.structured_content_json.as_deref().unwrap()).unwrap();
    let child = spawned["spawn"]["agent"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let child_turn = fixture
        .service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.agent_id == child)
        .unwrap()
        .clone();
    fixture
        .service
        .complete_running_agent_turn_and_start_ready(
            &parent_turn,
            AgentTurnState::Completed,
            "generated detached parent completed",
        )
        .unwrap();
    fixture.save("task-parent-b", None);
    fixture.save("task-parent-c", None);
    fixture.resume("agent-%1", "task-parent-b");
    fixture.resume("agent-%1", "task-parent-c");
    assert_eq!(
        fixture
            .service
            .agent_turn_ledger()
            .turn(&child_turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Interrupted
    );
    assert!(
        !fixture
            .service
            .subagent_lineage_has_live_parent_authority(&child)
    );
    assert!(!fixture.service.has_subagent_scope_declaration(&child));
    assert!(fixture.present(&child));
    fixture.close("agent-%1");
    assert!(
        !fixture.present(&child),
        "revoking execution authority must not leak its native task pane"
    );
}

/// Verifies a historical self-parent reference above the closing target cannot
/// veto cleanup of an actually delegated child. Structural ancestry remains
/// cycle-safe for presentation, but it must not be a prerequisite kill grant.
#[test]
fn runtime_cleanup_keeps_live_child_below_self_historical_parent() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    fixture.save("self-historical-parent", Some("agent-%1"));
    fixture.resume("agent-%1", "self-historical-parent");
    assert!(
        !fixture
            .service
            .subagent_lineage_has_live_parent_authority("agent-%1")
    );
    fixture.close("agent-%1");
    assert!(!fixture.present(&child));
}

/// Verifies both terminal macro judge outcomes reject the old macro's child
/// binding before recursive OR direct pane close. The fake provider returns
/// only a generated decision; no provider/network execution is performed.
#[test]
fn runtime_cleanup_macro_decisions_do_not_close_resumed_child_binding() {
    for outcome in ["stop_failure", "finish_success"] {
        let mut fixture = CleanupFixture::new();
        let macro_dir = fixture.root.join("macros/cleanup-fixture");
        fs::create_dir_all(&macro_dir).unwrap();
        fs::write(macro_dir.join("MACRO.md"),
            "---\nname: cleanup-fixture\ndescription: Generated cleanup fixture\n---\n\n# Macro: cleanup-fixture\n\n## Steps\n\n1. Report generated fixture result.\n").unwrap();
        fixture.service.set_config_root(fixture.root.clone());
        let response = fixture
            .service
            .execute_agent_shell_command(&fixture.primary, "#cleanup-fixture")
            .unwrap();
        assert!(response.contains(r#""kind":"turn_started""#), "{response}");
        let parent = fixture
            .service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.agent_id == "agent-%1")
            .unwrap()
            .clone();
        let child = fixture
            .service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.cooperation_mode.as_deref() == Some("macro-step"))
            .unwrap()
            .clone();
        fixture
            .service
            .agent_turn_ledger_mut()
            .finish_turn(&child.turn_id, AgentTurnState::Completed)
            .unwrap();
        fixture
            .service
            .emit_subagent_task_result_for_state(&child, AgentTurnState::Completed)
            .unwrap();
        fixture.save("independent-macro-child", Some("agent-%1"));
        fixture.resume(&child.agent_id, "independent-macro-child");
        let pid = fixture
            .service
            .primary_pid_for_live_pane_process(&child.pane_id)
            .unwrap();
        let token = mez_mux::process::process_start_token_for_pid(pid).unwrap();
        let provider = RuntimeBatchProvider {
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(), model: "test".to_string(),
                raw_text: serde_json::json!({"outcome":outcome,"step_success": outcome == "finish_success",
                    "rationale":"generated terminal decision","adapted_prompt":null,"user_message":null}).to_string(),
                usage: Default::default(), latest_request_usage: None, quota_usage: Default::default(),
                action_batch: None, provider_transcript_events: Vec::new(),
            },
        };
        fixture
            .service
            .execute_agent_turn_with_provider(
                &parent.turn_id,
                &provider,
                runtime_model_profile("runtime-batch", "test"),
            )
            .unwrap();
        assert!(
            fixture.present(&child.agent_id),
            "{outcome} adopted the independently resumed child"
        );
        assert_eq!(
            fixture
                .service
                .agent_shell_store()
                .get(&child.pane_id)
                .unwrap()
                .session_id,
            "independent-macro-child"
        );
        assert_eq!(
            fixture
                .service
                .primary_pid_for_live_pane_process(&child.pane_id),
            Some(pid)
        );
        assert_eq!(
            mez_mux::process::process_start_token_for_pid(pid),
            Some(token)
        );
    }
}

/// A validated live obligation closes through the session owner even when no
/// primary is attached. This must commit real layout/process/state retirement,
/// while the separate historical-child oracle forbids adopting unrelated panes.
#[test]
fn runtime_cleanup_without_layout_owner_preserves_live_obligation() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let pane = child.strip_prefix("agent-").unwrap();
    let pid = fixture
        .service
        .primary_pid_for_live_pane_process(pane)
        .unwrap();
    let token = mez_mux::process::process_start_token_for_pid(pid).unwrap();
    fixture
        .service
        .session
        .detach_client_self(&fixture.primary)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .close_subagent_descendants_for_parent_agent(
                "agent-%1",
                "session-owned descendant close"
            )
            .unwrap(),
        1
    );
    assert!(!fixture.present(&child));
    assert!(fixture.service.agent_shell_store().get(pane).is_none());
    assert!(fixture.service.subagent_scope_declaration(&child).is_none());
    assert_ne!(
        mez_mux::process::process_start_token_for_pid(pid),
        Some(token)
    );
}

/// Verifies a real transcript failure after ordinary task authority retirement
/// cannot drop the fence or original cleanup-only proof. No turn/effect is
/// replayed to recover the obligation, and execution authority stays revoked.
#[test]
fn runtime_cleanup_retains_fenced_task_after_late_transcript_failure() {
    let mut fixture = CleanupFixture::new();
    let parent = fixture
        .service
        .start_agent_prompt_turn("%1", "delegate late-failure fixture")
        .unwrap();
    fixture
        .service
        .remove_pending_agent_provider_task(&parent.turn_id);
    let parent_turn = fixture
        .service
        .agent_turn_ledger()
        .turn(&parent.turn_id)
        .unwrap()
        .clone();
    let spawned = fixture
        .service
        .execute_spawn_action_for_turn(
            &parent_turn,
            &runtime_spawn_agent_action("late-failure-child", "generated late-failure task"),
        )
        .unwrap();
    let spawned: serde_json::Value =
        serde_json::from_str(spawned.structured_content_json.as_deref().unwrap()).unwrap();
    let child_id = spawned["spawn"]["agent"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let child = fixture
        .service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.agent_id == child_id)
        .unwrap()
        .clone();
    fixture
        .service
        .complete_running_agent_turn_and_start_ready(
            &parent_turn,
            AgentTurnState::Completed,
            "generated detached parent completed",
        )
        .unwrap();
    fixture
        .service
        .fence_subagent_descendants_for_parent_conversation(
            "agent-%1",
            &parent_turn.conversation_id,
        );
    fixture
        .service
        .emit_subagent_task_result_for_state(&child, AgentTurnState::Interrupted)
        .unwrap();
    assert!(fixture.service.subagent_descendant_is_fenced(&child_id));
    assert!(
        !fixture
            .service
            .subagent_lineage_has_live_parent_authority(&child_id)
    );
    assert!(!fixture.service.has_subagent_scope_declaration(&child_id));
    assert!(fixture.service.subagent_lineage(&child_id).is_none());
    assert!(
        fixture
            .service
            .active_subagent_write_scopes_for(&child_id)
            .is_empty()
    );
    fixture.store.fail_next_transcript_append();
    let error = fixture
        .service
        .finish_agent_turn_without_shell_session(&child, AgentTurnState::Interrupted)
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("injected transcript append failure before commit"),
        "{error}"
    );
    assert!(fixture.service.subagent_descendant_is_fenced(&child_id));
    assert!(
        !fixture
            .service
            .subagent_lineage_has_live_parent_authority(&child_id)
    );
    assert!(!fixture.service.has_subagent_scope_declaration(&child_id));
    assert!(fixture.service.subagent_lineage(&child_id).is_none());
    assert!(
        fixture
            .service
            .active_subagent_write_scopes_for(&child_id)
            .is_empty()
    );
    assert!(fixture.present(&child_id));
    fixture.close("agent-%1");
    assert!(!fixture.present(&child_id));
}

/// Owns a genuinely handed-off process and reaps it even if an assertion fails.
/// No externally supplied PID is signaled and no fake process ownership is used.
struct CleanupAdapter {
    instance: crate::runtime::PaneProcessInstance,
    process: mez_mux::process::PaneProcess,
}

impl Drop for CleanupAdapter {
    /// Reaps only the actual Child moved from this fixture's manager.
    fn drop(&mut self) {
        let _ = self.process.terminate(Duration::from_millis(10));
    }
}

impl CleanupFixture {
    /// Moves the actual process through the production generation-owning seam.
    fn handoff(&mut self, agent: &str) -> CleanupAdapter {
        let pane = agent.strip_prefix("agent-").unwrap();
        let process = self
            .service
            .take_running_pane_process_for_adapter(pane)
            .unwrap();
        let instance = self
            .service
            .adapter_owned_pane_process_instance(pane)
            .unwrap();
        CleanupAdapter { instance, process }
    }
}

/// Transferring parent and child to async owners must preserve BOTH endpoint
/// proofs. Close must retire the manager-owned grandchild and queue termination
/// only for the exact actual adapter generations, rather than leak the subtree.
#[test]
fn runtime_cleanup_migrates_both_endpoints_across_adapter_handoff() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let grandchild = fixture.spawn(&child);
    let parent_adapter = fixture.handoff("agent-%1");
    let child_adapter = fixture.handoff(&child);
    fixture.close("agent-%1");
    assert!(!fixture.present(&child));
    assert!(!fixture.present(&grandchild));
    let effects = fixture.service.drain_pane_io_transition().side_effects;
    for expected in [&parent_adapter.instance, &child_adapter.instance] {
        assert!(effects.iter().any(|effect| matches!(effect,
            RuntimeSideEffect::PaneProcessIo { instance, effect: crate::runtime::PaneProcessIoEffect::Terminate { .. } }
                if instance == expected)), "missing exact termination: {effects:?}");
    }
}

/// An accepted real adapter process exit must retain just enough original
/// parent proof to close its genuine child even with no attached primary. The
/// independently resumed historical child remains the same actual process.
#[test]
fn runtime_cleanup_adapter_parent_exit_without_layout_closes_only_live_children() {
    let mut fixture = CleanupFixture::new();
    let live = fixture.spawn("agent-%1");
    let independent = fixture.spawn("agent-%1");
    fixture.save("independent-adapter-exit", Some("agent-%1"));
    fixture.resume(&independent, "independent-adapter-exit");
    let pane = independent.strip_prefix("agent-").unwrap();
    let pid = fixture
        .service
        .primary_pid_for_live_pane_process(pane)
        .unwrap();
    let token = mez_mux::process::process_start_token_for_pid(pid).unwrap();
    let mut parent = fixture.handoff("agent-%1");
    fixture
        .service
        .session
        .detach_client_self(&fixture.primary)
        .unwrap();
    let parent_pid = parent.process.primary_pid();
    let status = parent.process.terminate(Duration::from_millis(10)).unwrap();
    fixture
        .service
        .apply_pane_process_exit_event("%1", parent_pid, status)
        .unwrap()
        .unwrap();
    assert!(!fixture.present(&live));
    assert!(fixture.present(&independent));
    assert_eq!(
        fixture.service.primary_pid_for_live_pane_process(pane),
        Some(pid)
    );
    assert_eq!(
        mez_mux::process::process_start_token_for_pid(pid),
        Some(token)
    );
    assert_eq!(
        fixture
            .service
            .agent_shell_store()
            .get(pane)
            .unwrap()
            .session_id,
        "independent-adapter-exit"
    );
    assert_eq!(
        fixture
            .service
            .close_subagent_descendants_for_parent_agent("agent-%1", "duplicate exit")
            .unwrap(),
        0
    );
}

/// Failed synchronous teardown must remove its temporary exit witness. Calling
/// cleanup afterward with no actual parent owner cannot consume the former
/// witness or publish a new delegation from a process that is already gone.
#[test]
fn runtime_cleanup_exit_witness_is_removed_after_error() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    fixture
        .service
        .start_agent_prompt_turn(
            child.strip_prefix("agent-").unwrap(),
            "generated fallible cleanup",
        )
        .unwrap();
    let mut parent = fixture.handoff("agent-%1");
    let pid = parent.process.primary_pid();
    let status = parent.process.terminate(Duration::from_millis(10)).unwrap();
    fixture.store.fail_next_transcript_append();
    let error = fixture
        .service
        .apply_pane_process_exit_event("%1", pid, status)
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("injected transcript append failure"),
        "{error}"
    );
    assert_eq!(
        fixture
            .service
            .close_subagent_descendants_for_parent_agent("agent-%1", "after failed exit")
            .unwrap(),
        0
    );
    assert!(fixture.present(&child));
}

/// Successful binding restoration precedes process startup in real snapshots.
/// Pinning the first real parent, migrating it to an adapter, then installing
/// the child must publish exactly that completed restore's cleanup obligation.
#[test]
fn runtime_cleanup_restore_before_start_survives_first_owner_handoff() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    fixture.service.checkpoint_agent_session_metadata().unwrap();
    fixture.service.terminate_all_pane_processes().unwrap();
    for pane in ["%1", child.strip_prefix("agent-").unwrap()] {
        fixture
            .service
            .session
            .set_pane_live_state(pane, false)
            .unwrap();
    }
    assert_eq!(
        fixture
            .service
            .restore_agent_sessions_from_transcript_store()
            .unwrap(),
        2
    );
    fixture
        .service
        .start_initial_pane_process(Some("cat"))
        .unwrap();
    let _parent = fixture.handoff("agent-%1");
    fixture
        .service
        .session
        .set_pane_live_state("%1", true)
        .unwrap();
    fixture
        .service
        .restart_restored_pane_processes(Some("cat"))
        .unwrap();
    assert_eq!(
        fixture
            .service
            .close_subagent_descendants_for_parent_agent("agent-%1", "restored native owners")
            .unwrap(),
        1
    );
    assert!(!fixture.present(&child));
}

/// Replacing an actual native parent keeps its conversation but revokes the
/// earlier incarnation's edges. PID labels and unchanged conversation IDs must
/// never authorize the replacement to adopt a formerly delegated child.
#[test]
fn runtime_cleanup_native_replacement_does_not_adopt_prior_process_edges() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    fixture
        .service
        .terminate_runtime_pane_process("%1", true)
        .unwrap();
    fixture
        .service
        .start_initial_pane_process(Some("cat"))
        .unwrap();
    fixture.close("agent-%1");
    assert!(fixture.present(&child));
}

/// A pending restore pin cannot adopt a second native installation even when
/// its conversation is unchanged. Only the first startup belonging to the
/// qualified restore may establish the missing endpoint of that operation.
#[test]
fn runtime_cleanup_pending_restore_rejects_replaced_first_owner() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    fixture.service.checkpoint_agent_session_metadata().unwrap();
    fixture.service.terminate_all_pane_processes().unwrap();
    for pane in ["%1", child.strip_prefix("agent-").unwrap()] {
        fixture
            .service
            .session
            .set_pane_live_state(pane, false)
            .unwrap();
    }
    assert_eq!(
        fixture
            .service
            .restore_agent_sessions_from_transcript_store()
            .unwrap(),
        2
    );
    fixture
        .service
        .start_initial_pane_process(Some("cat"))
        .unwrap();
    fixture
        .service
        .terminate_runtime_pane_process("%1", true)
        .unwrap();
    fixture
        .service
        .start_initial_pane_process(Some("cat"))
        .unwrap();
    fixture
        .service
        .session
        .set_pane_live_state("%1", true)
        .unwrap();
    fixture
        .service
        .restart_restored_pane_processes(Some("cat"))
        .unwrap();
    fixture.close("agent-%1");
    assert!(fixture.present(&child));
}
