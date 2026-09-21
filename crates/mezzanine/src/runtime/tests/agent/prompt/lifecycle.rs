//! Runtime tests for agent prompt lifecycle behavior.

use super::*;
use crate::runtime::current_unix_seconds;
use mez_agent::SubagentSessionMode;
use mez_agent::messaging::{Envelope, Recipient};

/// Spawns one native child and returns its canonical id with the advertised display name.
///
/// The display-name contract is emitted in the spawn response after pane creation.
/// Keeping this setup in one helper lets mode-specific tests assert that each configured
/// policy is resolved against the same canonical child identity and lifecycle path.
fn spawn_subagent_for_display_name_test(service: &mut RuntimeSessionService) -> (String, String) {
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: SubagentSessionMode::New,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "inspect display-name allocation".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    (
        spawned["agent"]["id"].as_str().unwrap().to_string(),
        spawned["agent"]["display_name"]
            .as_str()
            .unwrap()
            .to_string(),
    )
}

/// Verifies every configured naming policy is applied after a child receives its
/// canonical runtime identity, preserving literal ids and selecting only product corpora.
///
/// This exercises the real pane spawn path rather than a parser-only configuration test,
/// ensuring display names exposed through the response and stored lifecycle state honor the
/// prospective runtime setting for nonhuman, human, and literal child allocations.
#[test]
fn runtime_subagent_spawn_applies_configured_display_name_mode() {
    let mut nonhuman_service = test_runtime_service();
    let (_child_id, nonhuman_name) = spawn_subagent_for_display_name_test(&mut nonhuman_service);
    assert!(
        crate::integrations::agent::subagent::SUBAGENT_NONHUMAN_NAMES
            .contains(&nonhuman_name.as_str()),
        "default mode must use the nonhuman corpus: {nonhuman_name}"
    );
    nonhuman_service.terminate_all_pane_processes().unwrap();

    let mut human_service = test_runtime_service();
    human_service.set_subagent_name_mode(crate::runtime::config::SubagentNameMode::Human);
    let (_child_id, human_name) = spawn_subagent_for_display_name_test(&mut human_service);
    assert!(
        crate::integrations::agent::subagent::SUBAGENT_HUMAN_NAMES.contains(&human_name.as_str()),
        "human mode must use the human corpus: {human_name}"
    );
    human_service.terminate_all_pane_processes().unwrap();

    let mut literal_service = test_runtime_service();
    literal_service.set_subagent_name_mode(crate::runtime::config::SubagentNameMode::Literal);
    let (child_id, literal_name) = spawn_subagent_for_display_name_test(&mut literal_service);
    assert_eq!(literal_name, child_id);
    literal_service.terminate_all_pane_processes().unwrap();
}

/// Verifies shorthand prompt words still resolve to the read-only subagent mode.
///
/// Provider prompts describe cooperation mode as a safety/scope concept, and
/// some models may echo those words back even when the compact spawn schema
/// omits the explicit field. Accepting these shorthands keeps runtime subagent
/// spawns compatible with that model behavior instead of failing validation.
#[test]
fn runtime_cooperation_mode_accepts_prompt_shorthand_scope_words() {
    assert_eq!(
        runtime_cooperation_mode("safety").unwrap(),
        CooperationMode::ExploreOnly
    );
    assert_eq!(
        runtime_cooperation_mode("scope").unwrap(),
        CooperationMode::ExploreOnly
    );
    assert_eq!(
        runtime_cooperation_mode("scoped").unwrap(),
        CooperationMode::ExploreOnly
    );
}

/// Verifies native runtime-owned subagent startup validates only root-process
/// context and permits the child turn to start without pane bootstrap state.
///
/// This protects native mode from accidentally inheriting the foreign-shell
/// prompt gate introduced for pane-mode compatibility startup.
#[test]
fn runtime_native_subagent_startup_bypasses_pane_bootstrap() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
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
                task_prompt: "inspect native startup".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    let pane_id = spawned["pane"]["pane_id"].as_str().unwrap();
    let turn_id = spawned["turn"]["id"].as_str().unwrap();

    assert_eq!(
        spawned["allowed_actions"],
        serde_json::json!([
            "say",
            "shell_command",
            "web_search",
            "fetch_url",
            "send_message",
            "spawn_agent",
            "close_agent",
            "mcp_server_search",
            "mcp_server_get",
            "mcp_call",
            "memory_search",
            "list_agents",
            "issue_query",
        ])
    );
    assert!(spawned["action_schema_digest"].as_str().is_some());

    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(pane_id),
        Some("ready")
    );
    assert!(!service.pane_bootstrap_is_pending_for_tests(pane_id));
    assert!(!service.pane_has_uncertified_foreign_shell_boundary(pane_id));
    assert!(service.agent_provider_task_is_pending(turn_id));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Running)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a durable child retains its captured action catalog after its pane
/// is removed, including direct UUID resume in a fresh runtime instance.
///
/// Root-only checkpoints intentionally omit child pane bindings. The child
/// catalog must therefore be saved by durable conversation identity at spawn
/// time, before pane cleanup can discard the in-memory session that captured
/// it. Otherwise direct child recovery after a configuration change would
/// silently recapture the new live action surface.
#[test]
fn runtime_durable_child_catalog_survives_pane_cleanup_direct_resume_and_restart() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-child-catalog-resume"));
    let catalog_a = mez_agent::AllowedActionSet::from_actions([
        mez_agent::AllowedAction::Say,
        mez_agent::AllowedAction::ShellCommand,
    ]);
    let catalog_b = mez_agent::AllowedActionSet::say_only();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service.set_agent_enabled_actions(catalog_a.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "default".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: SubagentSessionMode::New,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "preserve this child catalog".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let child_pane_id =
        serde_json::from_str::<serde_json::Value>(&spawned).unwrap()["pane"]["pane_id"]
            .as_str()
            .unwrap()
            .to_string();
    let child_conversation_id = service
        .agent_shell_store()
        .get(&child_pane_id)
        .unwrap()
        .session_id
        .clone();
    assert_eq!(
        transcript_store
            .conversation_allowed_actions(&child_conversation_id)
            .unwrap(),
        Some(catalog_a.clone())
    );
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: child_conversation_id.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "child-catalog-prompt".to_string(),
            agent_id: format!("agent-{child_pane_id}"),
            pane_id: child_pane_id.clone(),
            content: "preserve this child catalog".to_string(),
        })
        .unwrap();

    service
        .dispatch_runtime_pane_close(
            &primary,
            &format!(r#"{{"pane_id":"{child_pane_id}","force":true}}"#),
        )
        .unwrap();
    service.set_agent_enabled_actions(catalog_b.clone());
    let resumed = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"resume-child","method":"agent/shell/command","params":{{"idempotency_key":"resume-child","input":"/resume {child_conversation_id}"}}}}"#
        ),
        &primary,
    );
    assert!(resumed.contains(&child_conversation_id), "{resumed}");
    let resumed_child = service
        .agent_shell_store()
        .sessions()
        .find(|session| session.session_id == child_conversation_id);
    assert!(
        resumed_child.is_some(),
        "{resumed}; sessions={:?}",
        service.agent_shell_store().sessions().collect::<Vec<_>>()
    );
    assert_eq!(
        resumed_child.and_then(|session| session.allowed_actions.as_ref()),
        Some(&catalog_a)
    );
    service.terminate_all_pane_processes().unwrap();

    let mut restarted = test_runtime_service();
    restarted.set_agent_transcript_store(transcript_store.clone());
    restarted.set_agent_enabled_actions(catalog_b);
    let restarted_primary = restarted
        .attach_primary("restarted", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    restarted.start_initial_pane_process(Some("cat")).unwrap();
    restarted
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let resumed_after_restart = restarted.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"resume-child-restart","method":"agent/shell/command","params":{{"idempotency_key":"resume-child-restart","input":"/resume {child_conversation_id}"}}}}"#
        ),
        &restarted_primary,
    );
    assert!(
        resumed_after_restart.contains(&child_conversation_id),
        "{resumed_after_restart}"
    );
    assert_eq!(
        restarted
            .agent_shell_store()
            .sessions()
            .find(|session| session.session_id == child_conversation_id)
            .and_then(|session| session.allowed_actions.as_ref()),
        Some(&catalog_a)
    );
    restarted.terminate_all_pane_processes().unwrap();
    let _ = std::fs::remove_dir_all(transcript_store.root());
}

/// Verifies a real subagent spawn leaves the parent pane with only its
/// structural `subagent ...` lines and no bridge echo row.
///
/// Every runtime-owned bridge notification already has one dedicated line in the
/// parent pane, so the normal peer-message log mode must not add a second
/// `{child-agent-id}> ` row for the spawn notice, the running update, or the
/// terminal result, whose JSON payload does carry an `output` field.
#[test]
fn runtime_subagent_spawn_bridge_notifications_log_no_parent_pane_echo() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        mez_terminal::TerminalScreen::new(Size::new(100, 30).unwrap(), 120).unwrap(),
    );
    // The parent keeps an active turn, so each bridge notification commits at
    // arrival and reaches the pane echo path instead of waiting for the cursor.
    service
        .start_agent_prompt_turn("%1", "supervise the spawned child")
        .unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
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
                task_prompt: "inspect the parent pane echo".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    let turn_id = spawned["turn"]["id"].as_str().unwrap().to_string();
    let parent_pane_text = |service: &crate::runtime::RuntimeSessionService| {
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
    };
    let spawn_text = parent_pane_text(&service);
    assert!(
        spawn_text.contains("subagent ") && spawn_text.contains("started in pane"),
        "the spawn notice keeps its structural `subagent ...` line: {spawn_text}"
    );

    let child_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
        .expect("spawned child turn");
    service
        .emit_subagent_task_status(
            &child_turn,
            mez_agent::messaging::TaskState::Running,
            Some(50),
            "subagent task running",
        )
        .unwrap();
    let running_text = parent_pane_text(&service);
    assert!(
        running_text.contains("subagent task running"),
        "the running update keeps its structural `subagent ...` line: {running_text}"
    );
    assert_eq!(
        running_text.matches("subagent task running").count(),
        1,
        "normal mode keeps only the structural status line for JSON bridge traffic: {running_text}"
    );

    service
        .emit_subagent_task_result_for_state(&child_turn, AgentTurnState::Completed)
        .unwrap();
    let result_text = parent_pane_text(&service);
    assert!(
        result_text.contains("subagent "),
        "the terminal result keeps its structural `subagent ...` line: {result_text}"
    );
    assert!(
        !result_text.contains("completed without provider output"),
        "normal mode suppresses the JSON bridge result payload: {result_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a retained pre-restart lifecycle id cannot poison a new spawn whose
/// compact turn id is reused after historical turn-ledger state was discarded.
///
/// The MMP sequence survives snapshot restore, so runtime-authored lifecycle
/// ids must include that occurrence identity rather than relying only on the
/// restart-local `turn-N` allocator and task state.
#[test]
fn runtime_subagent_spawn_avoids_restored_legacy_lifecycle_message_id() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .start_agent_prompt_turn("%1", "retain turn-1 so the child receives turn-2")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let parent = service
        .ensure_runtime_message_identity("agent-%1", None, "agent", &[], now_ms)
        .unwrap();
    service
        .control
        .message_service_mut()
        .subscribe(&parent.agent_id)
        .unwrap();
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &parent.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "turn-2:task_status:started".to_string(),
                message_type: "task_status".to_string(),
                time: "runtime:1".to_string(),
                sender: parent.clone(),
                recipient: Recipient::Agent(parent.agent_id.clone()),
                correlation_id: Some("turn-2".to_string()),
                ttl_ms: None,
                content_type: "application/json".to_string(),
                payload: mez_agent::messaging::TaskStatusPayload {
                    task_id: "turn-2".to_string(),
                    state: mez_agent::messaging::TaskState::Running,
                    progress_percent: Some(0),
                    summary: "retained pre-restart status".to_string(),
                }
                .to_json(),
                extension_fields: Vec::new(),
            },
            mez_agent::messaging::MessageScope::Session,
            now_ms,
        )
        .unwrap();
    let restored_message_state = service.message_service().snapshot_state();
    *service.control.message_service_mut() =
        mez_agent::messaging::MessageService::from_snapshot_state(&restored_message_state).unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
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
                task_prompt: "verify restart-safe lifecycle identity".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    assert_eq!(spawned["turn"]["id"], "turn-2");
    let accepted = service.message_service().snapshot_state().accepted_messages;
    assert!(
        accepted
            .iter()
            .any(|message| message.envelope.id == "turn-2:task_status:started")
    );
    assert!(accepted.iter().any(|message| {
        message
            .envelope
            .id
            .starts_with("turn-2:task_status:started:")
    }));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies repeated transitions to the same task state produce distinct MMP
/// occurrences even when their payload summaries differ.
///
/// A state name is descriptive data, not an idempotency key; approval and wait
/// lifecycles may legitimately enter `blocked` more than once in one turn.
#[test]
fn runtime_subagent_repeated_task_state_updates_use_distinct_message_ids() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
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
                task_prompt: "exercise repeated blocked states".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    let turn_id = spawned["turn"]["id"].as_str().unwrap();
    let child_turn = service.agent_turn_ledger().turn(turn_id).cloned().unwrap();

    service
        .emit_subagent_task_status(
            &child_turn,
            mez_agent::messaging::TaskState::Blocked,
            None,
            "first blocked occurrence",
        )
        .unwrap();
    service
        .emit_subagent_task_status(
            &child_turn,
            mez_agent::messaging::TaskState::Blocked,
            None,
            "second blocked occurrence",
        )
        .unwrap();

    let blocked_ids = service
        .message_service()
        .snapshot_state()
        .accepted_messages
        .into_iter()
        .filter(|message| {
            message
                .envelope
                .id
                .starts_with(&format!("{turn_id}:task_status:blocked:"))
        })
        .map(|message| message.envelope.id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(blocked_ids.len(), 2, "{blocked_ids:#?}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a subagent spawn publishes its own bounded objective derived from
/// the spawn task prompt through the same identity registry discovery reads.
#[test]
fn runtime_subagent_spawn_publishes_bounded_task_objective() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
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
                task_prompt: "  inspect   the subagent objective  ".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    let pane_id = spawned["pane"]["pane_id"].as_str().unwrap();
    let child_agent_id = AgentId::opaque(format!("agent-{pane_id}")).unwrap();
    assert_eq!(
        service
            .message_service()
            .registered_identity(&child_agent_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("inspect the subagent objective")
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies an idle persistent spawn becomes a reusable MMP actor rather than
/// a joined one-shot child, and later messages reuse its durable conversation.
#[test]
fn runtime_persistent_subagent_reuses_identity_and_conversation_across_mmp_turns() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root(
        "runtime-persistent-subagent-reuse",
    )));
    service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let parent = service
        .start_agent_prompt_turn("%1", "provision a reusable MMP worker")
        .unwrap();
    service.remove_pending_agent_provider_task(&parent.turn_id);
    let parent_turn = service
        .agent_turn_ledger()
        .turn(&parent.turn_id)
        .cloned()
        .unwrap();
    let mut action = runtime_spawn_agent_action("persistent-spawn", "");
    let mez_agent::AgentActionPayload::SpawnAgent {
        lifetime,
        objective,
        ..
    } = &mut action.payload
    else {
        unreachable!("spawn fixture must contain spawn_agent");
    };
    *lifetime = mez_agent::SubagentLifetime::Persistent;
    *objective = Some("Triage incoming MMP defects".to_string());

    let result = service
        .execute_spawn_action_for_turn(&parent_turn, &action)
        .unwrap();
    assert_eq!(result.status, ActionStatus::Succeeded);
    assert_eq!(service.joined_subagent_dependency_count(), 0);
    let structured: serde_json::Value = serde_json::from_str(
        result
            .structured_content_json
            .as_deref()
            .expect("persistent spawn structured content"),
    )
    .unwrap();
    assert_eq!(structured["lifetime"], "persistent");
    assert_eq!(structured["spawn"]["turn"], serde_json::Value::Null);
    let child_agent_id = structured["spawn"]["agent"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let child_pane_id = structured["spawn"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let child_conversation_id = service
        .agent_shell_store()
        .get(&child_pane_id)
        .unwrap()
        .session_id
        .clone();
    let child_id = AgentId::opaque(child_agent_id.clone()).unwrap();
    assert_eq!(
        service
            .persistent_subagent(&child_agent_id)
            .map(|record| record.conversation_id.as_str()),
        Some(child_conversation_id.as_str())
    );
    assert_eq!(
        service
            .message_service()
            .registered_identity(&child_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Triage incoming MMP defects")
    );
    assert!(service.message_service().subscription(&child_id).is_some());
    service
        .complete_running_agent_turn_and_start_ready(
            &parent_turn,
            AgentTurnState::Completed,
            "persistent child provisioning settled",
        )
        .unwrap();

    let now_ms = crate::runtime::current_unix_millis();
    let sender = service
        .ensure_runtime_message_identity("agent-%1", None, "agent", &[], now_ms)
        .unwrap();
    for (index, payload) in ["first defect", "second defect"].into_iter().enumerate() {
        service
            .control
            .message_service_mut()
            .accept_at(
                &sender.agent_id,
                mez_agent::messaging::Envelope {
                    protocol: "mmp/1",
                    id: format!("persistent-message-{index}"),
                    message_type: "send".to_string(),
                    time: format!("runtime:{now_ms}"),
                    sender: sender.clone(),
                    recipient: mez_agent::messaging::Recipient::Agent(child_id.clone()),
                    correlation_id: Some(parent_turn.turn_id.clone()),
                    ttl_ms: None,
                    content_type: "text/plain; charset=utf-8".to_string(),
                    payload: payload.to_string(),
                    extension_fields: Vec::new(),
                },
                now_ms.saturating_add(index as u64),
            )
            .unwrap();
        assert_eq!(
            service
                .deliver_pending_runtime_agent_messages(now_ms.saturating_add(index as u64))
                .unwrap(),
            1
        );
        let child_turn = service
            .agent_turn_ledger()
            .turns()
            .iter()
            .rev()
            .find(|turn| {
                turn.agent_id == child_agent_id
                    && turn.trigger == mez_agent::AgentTurnTrigger::LocalMessage
            })
            .cloned()
            .expect("MMP message should start a child turn");
        assert_eq!(child_turn.conversation_id, child_conversation_id);
        service.start_ready_agent_turns().unwrap();
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&child_turn.turn_id)
                .map(|turn| turn.state),
            Some(AgentTurnState::Running)
        );
        service
            .complete_running_agent_turn_and_start_ready(
                &child_turn,
                AgentTurnState::Completed,
                "persistent MMP child turn completed",
            )
            .unwrap();
        assert!(service.persistent_subagent(&child_agent_id).is_some());
        assert!(service.subagent_lineage(&child_agent_id).is_some());
        assert!(service.has_subagent_scope_declaration(&child_agent_id));
        assert!(!service.has_pending_terminal_subagent_pane_close(&child_pane_id));
        assert!(service.agent_shell_store().get(&child_pane_id).is_some());
    }

    let child_turn_count = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .filter(|turn| turn.agent_id == child_agent_id)
        .count();
    let replaced = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"persistent-owner-new","method":"agent/shell/command","params":{"idempotency_key":"persistent-owner-new","input":"/new"}}"#,
        &service.session().layout_owner_client_id().cloned().unwrap(),
    );
    assert!(replaced.contains(r#""kind":"mutated""#), "{replaced}");
    assert!(service.subagent_descendant_is_fenced(&child_agent_id));
    let sender = service
        .message_service()
        .registered_identity(&sender.agent_id)
        .cloned()
        .expect("parent identity remains registered after /new");
    let error = service
        .control
        .message_service_mut()
        .accept_at(
            &sender.agent_id,
            mez_agent::messaging::Envelope {
                protocol: "mmp/1",
                id: "persistent-message-after-owner-replacement".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{}", now_ms.saturating_add(10)),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(child_id),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "must remain fenced".to_string(),
                extension_fields: Vec::new(),
            },
            now_ms.saturating_add(10),
        )
        .unwrap_err();
    assert_eq!(
        error.kind(),
        mez_agent::messaging::MessageErrorKind::NotFound,
        "a fenced persistent child must be indistinguishable from an absent recipient"
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms.saturating_add(10))
            .unwrap(),
        0
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .filter(|turn| turn.agent_id == child_agent_id)
            .count(),
        child_turn_count
    );

    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a parent can close only its live persistent child and that the
/// forced pane-close lifecycle retires the child's authority, shell, and MMP
/// identity state rather than leaving it discoverable after the action settles.
#[test]
fn runtime_close_agent_retires_owned_persistent_child_runtime_state() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root(
        "runtime-close-owned-persistent-child",
    )));
    service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let parent = service
        .start_agent_prompt_turn("%1", "provision then retire a reusable MMP worker")
        .unwrap();
    service.remove_pending_agent_provider_task(&parent.turn_id);
    let parent_turn = service
        .agent_turn_ledger()
        .turn(&parent.turn_id)
        .cloned()
        .unwrap();
    let mut spawn = runtime_spawn_agent_action("persistent-spawn", "");
    let mez_agent::AgentActionPayload::SpawnAgent {
        lifetime,
        objective,
        ..
    } = &mut spawn.payload
    else {
        unreachable!("spawn fixture must contain spawn_agent");
    };
    *lifetime = mez_agent::SubagentLifetime::Persistent;
    *objective = Some("Handle reusable MMP work".to_string());
    let spawned = service
        .execute_spawn_action_for_turn(&parent_turn, &spawn)
        .unwrap();
    let spawned: serde_json::Value = serde_json::from_str(
        spawned
            .structured_content_json
            .as_deref()
            .expect("persistent spawn structured content"),
    )
    .unwrap();
    let child_agent_id = spawned["spawn"]["agent"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let child_pane_id = spawned["spawn"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let child_identity = AgentId::opaque(child_agent_id.clone()).unwrap();
    let close = mez_agent::AgentAction {
        id: "close-persistent-child".to_string(),
        payload: mez_agent::AgentActionPayload::CloseAgent {
            agent_id: child_agent_id.clone(),
        },
    };
    let mut foreign_turn = parent_turn.clone();
    foreign_turn.conversation_id = "foreign-parent-conversation".to_string();
    let foreign_planned = mez_agent::plan_action_result(
        &foreign_turn,
        &close,
        mez_agent::ActionPlanningInput::default(),
    )
    .unwrap();
    let mut foreign_execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(
            &foreign_turn.turn_id,
            &foreign_turn.agent_id,
        ),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "reject foreign persistent child close".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "reject foreign persistent child close".to_string(),
                actions: vec![close.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![foreign_planned],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    assert_eq!(
        service
            .execute_running_close_agent_actions_for_turn(&foreign_turn, &mut foreign_execution)
            .unwrap(),
        1
    );
    assert_eq!(
        foreign_execution.action_results[0].status,
        ActionStatus::Rejected
    );
    assert!(service.persistent_subagent(&child_agent_id).is_some());
    assert!(service.find_pane_descriptor(&child_pane_id).is_some());
    let unavailable = close_agent_denial_signature(&foreign_execution.action_results[0]);
    assert_eq!(unavailable.0, ActionStatus::Rejected);
    assert_eq!(unavailable.1.as_deref(), Some("unavailable"));
    assert_eq!(unavailable.3, None);

    for target in [parent_turn.agent_id.as_str(), "agent-%999"] {
        let denied = execute_close_agent_for_test(&mut service, &parent_turn, target);
        assert_eq!(close_agent_denial_signature(&denied), unavailable);
        assert!(service.persistent_subagent(&child_agent_id).is_some());
        assert!(service.find_pane_descriptor(&child_pane_id).is_some());
        assert!(
            service
                .message_service()
                .registered_identity(&child_identity)
                .is_some()
        );
    }
    let mut foreign_agent_turn = parent_turn.clone();
    foreign_agent_turn.agent_id = "agent-%998".to_string();
    let denied = execute_close_agent_for_test(&mut service, &foreign_agent_turn, &child_agent_id);
    assert_eq!(close_agent_denial_signature(&denied), unavailable);
    assert!(service.persistent_subagent(&child_agent_id).is_some());
    assert!(service.find_pane_descriptor(&child_pane_id).is_some());
    assert!(
        service
            .message_service()
            .registered_identity(&child_identity)
            .is_some()
    );

    let task_spawn = service
        .execute_spawn_action_for_turn(
            &parent_turn,
            &runtime_spawn_agent_action("task-child", "one-shot task child"),
        )
        .expect("task child spawn");
    let task_spawn: serde_json::Value = serde_json::from_str(
        task_spawn
            .structured_content_json
            .as_deref()
            .expect("task spawn structured content"),
    )
    .expect("task spawn JSON");
    let task_child_agent_id = task_spawn["spawn"]["agent"]["id"]
        .as_str()
        .expect("task child agent id")
        .to_string();
    let task_child_pane_id = task_spawn["spawn"]["pane"]["pane_id"]
        .as_str()
        .expect("task child pane id")
        .to_string();
    let denied = execute_close_agent_for_test(&mut service, &parent_turn, &task_child_agent_id);
    assert_eq!(close_agent_denial_signature(&denied), unavailable);
    assert!(service.persistent_subagent(&task_child_agent_id).is_none());
    assert!(service.find_pane_descriptor(&task_child_pane_id).is_some());

    service.set_persistent_subagent(
        "agent-%997",
        crate::runtime::RuntimePersistentSubagent {
            conversation_id: "stale-child-conversation".to_string(),
            parent_agent_id: parent_turn.agent_id.clone(),
            parent_conversation_id: parent_turn.conversation_id.clone(),
            objective: "stale persistent child".to_string(),
        },
    );
    let stale = execute_close_agent_for_test(&mut service, &parent_turn, "agent-%997");
    assert_eq!(stale.status, ActionStatus::Succeeded);
    assert_eq!(
        stale.structured_content_json.as_deref(),
        Some(r#"{"closed":false,"agent_id":"agent-%997"}"#),
        "a missing owned pane is an idempotently completed close rather than a retry loop"
    );
    assert!(service.persistent_subagent("agent-%997").is_none());
    assert!(service.persistent_subagent(&child_agent_id).is_some());
    assert!(service.find_pane_descriptor(&child_pane_id).is_some());
    assert!(
        service
            .message_service()
            .registered_identity(&child_identity)
            .is_some()
    );
    let planned = mez_agent::plan_action_result(
        &parent_turn,
        &close,
        mez_agent::ActionPlanningInput::default(),
    )
    .unwrap();
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(
            &parent_turn.turn_id,
            &parent_turn.agent_id,
        ),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "close persistent child".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "retire persistent child".to_string(),
                actions: vec![close],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![planned],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    assert_eq!(
        service
            .execute_running_close_agent_actions_for_turn(&parent_turn, &mut execution)
            .unwrap(),
        1
    );
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    assert!(service.persistent_subagent(&child_agent_id).is_none());
    assert!(service.subagent_lineage(&child_agent_id).is_none());
    assert!(service.agent_shell_store().get(&child_pane_id).is_none());
    assert!(service.find_pane_descriptor(&child_pane_id).is_none());
    assert!(
        service
            .message_service()
            .registered_identity(&child_identity)
            .is_none()
    );
    let denied = execute_close_agent_for_test(&mut service, &parent_turn, &child_agent_id);
    assert_eq!(close_agent_denial_signature(&denied), unavailable);
    assert!(service.persistent_subagent(&child_agent_id).is_none());
    assert!(service.find_pane_descriptor(&child_pane_id).is_none());
    service.terminate_all_pane_processes().unwrap();
}

/// Executes one planned `close_agent` action and returns its settled result.
fn execute_close_agent_for_test(
    service: &mut RuntimeSessionService,
    turn: &AgentTurnRecord,
    agent_id: &str,
) -> mez_agent::ActionResult {
    let action = mez_agent::AgentAction {
        id: "close-agent-denial".to_string(),
        payload: mez_agent::AgentActionPayload::CloseAgent {
            agent_id: agent_id.to_string(),
        },
    };
    let planned =
        mez_agent::plan_action_result(turn, &action, mez_agent::ActionPlanningInput::default())
            .expect("close_agent plan");
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "close agent denial".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "test close-agent denial".to_string(),
                actions: vec![action],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![planned],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    assert_eq!(
        service
            .execute_running_close_agent_actions_for_turn(turn, &mut execution)
            .expect("close_agent denial execution"),
        1
    );
    execution.action_results.remove(0)
}

/// Captures the opaque fields that must stay identical for every inaccessible target.
fn close_agent_denial_signature(
    result: &mez_agent::ActionResult,
) -> (ActionStatus, Option<String>, Option<String>, Option<String>) {
    (
        result.status,
        result.error.as_ref().map(|error| error.code.clone()),
        result.error.as_ref().map(|error| error.message.clone()),
        result.structured_content_json.clone(),
    )
}

/// Verifies persistent child bridge status and result messages commit exactly
/// once while JSON bridge payloads remain absent from the parent normal pane log.
///
/// One-task and reusable children both keep their bridge envelopes durable and
/// model-visible, but normal presentation filters the non-plaintext payloads
/// before terminal rows or presentation records are created.
#[test]
fn runtime_persistent_subagent_bridge_messages_log_once_in_normal_mode() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service.set_agent_transcript_store(AgentTranscriptStore::new(temp_root(
        "runtime-persistent-bridge-echo",
    )));
    let _primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let parent_conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let spawned = service
        .spawn_runtime_persistent_subagent_session_owned(
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "worker".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::OwnedWrite,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: SubagentSessionMode::New,
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
            &parent_conversation_id,
            "Handle durable MMP requests",
        )
        .unwrap();
    let spawned: serde_json::Value = serde_json::from_str(&spawned).unwrap();
    let child_pane_id = spawned["pane"]["pane_id"].as_str().unwrap();
    let child_agent_id = format!("agent-{child_pane_id}");
    let child_id = AgentId::opaque(child_agent_id.clone()).unwrap();
    let now_ms = crate::runtime::current_unix_millis();
    let parent = service
        .ensure_runtime_message_identity("agent-%1", None, "agent", &[], now_ms)
        .unwrap();
    let child = service
        .message_service()
        .registered_identity(&child_id)
        .cloned()
        .expect("persistent child identity should be registered");
    service
        .start_agent_prompt_turn("%1", "supervise persistent bridge traffic")
        .unwrap();

    for (id, message_type, payload) in [
        (
            "persistent-bridge-status",
            "task_status",
            r#"{"task_id":"persistent-assignment","state":"running","summary":"persistent assignment accepted"}"#,
        ),
        (
            "persistent-bridge-result",
            "task_result",
            r#"{"task_id":"persistent-reply","success":true,"summary":"persistent reply delivered","output":"persistent bridge reply"}"#,
        ),
    ] {
        service
            .control
            .message_service_mut()
            .accept_at_with_scope(
                &child.agent_id,
                mez_agent::messaging::Envelope {
                    protocol: "mmp/1",
                    id: id.to_string(),
                    message_type: message_type.to_string(),
                    time: format!("runtime:{now_ms}"),
                    sender: child.clone(),
                    recipient: mez_agent::messaging::Recipient::Agent(parent.agent_id.clone()),
                    correlation_id: None,
                    ttl_ms: None,
                    content_type: "application/json".to_string(),
                    payload: payload.to_string(),
                    extension_fields: crate::runtime::control::runtime_bridge_extension_fields(),
                },
                mez_agent::messaging::MessageScope::Session,
                now_ms,
            )
            .unwrap();
        assert_eq!(
            service
                .deliver_pending_runtime_agent_messages(now_ms)
                .unwrap(),
            1
        );
    }

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let committed = service.agent_turn_contexts().get("turn-1").unwrap();
    assert_eq!(
        committed
            .blocks()
            .iter()
            .filter(|block| {
                block.source == mez_agent::ContextSourceKind::PeerMessage
                    && (block.content.contains("persistent-assignment")
                        || block.content.contains("persistent-reply"))
            })
            .count(),
        2,
        "each persistent bridge envelope should commit exactly once"
    );
    for suppressed in ["persistent assignment accepted", "persistent bridge reply"] {
        assert_eq!(
            pane_text.matches(suppressed).count(),
            0,
            "normal mode must not render JSON bridge payloads: {pane_text}"
        );
    }
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies restart rehydrates a persistent child only alongside its exact
/// live parent binding, including durable scope and MMP subscription state.
#[test]
fn runtime_persistent_subagent_restore_rehydrates_owned_scope_and_mmp_identity() {
    let transcript_store =
        AgentTranscriptStore::new(temp_root("runtime-persistent-subagent-restore"));
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let parent_conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let spawned = service
        .spawn_runtime_persistent_subagent_session_owned(
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "worker".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::OwnedWrite,
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
            &parent_conversation_id,
            "Handle durable MMP requests",
        )
        .unwrap();
    let spawned: serde_json::Value = serde_json::from_str(&spawned).unwrap();
    let child_pane_id = spawned["pane"]["pane_id"].as_str().unwrap().to_string();
    let child_agent_id = format!("agent-{child_pane_id}");
    let child_conversation_id = service
        .agent_shell_store()
        .get(&child_pane_id)
        .unwrap()
        .session_id
        .clone();
    let expected_scope = service
        .subagent_scope_declaration(&child_agent_id)
        .expect("spawned persistent scope");
    service.checkpoint_agent_session_metadata().unwrap();

    let mut restarted = test_runtime_service();
    restarted.session.id = service.session().id.clone();
    restarted.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    restarted.set_agent_transcript_store(transcript_store);
    let restarted_primary = restarted
        .attach_primary("restarted", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    restarted.start_initial_pane_process(Some("cat")).unwrap();
    let restored_child = restarted
        .split_pane_with_process(
            &restarted_primary,
            SplitDirection::Vertical,
            Some("cat >/dev/null"),
        )
        .unwrap();
    assert_eq!(restored_child.pane_id.as_str(), child_pane_id);

    assert_eq!(
        restarted
            .restore_agent_sessions_from_transcript_store()
            .unwrap(),
        2
    );
    let restored = restarted
        .persistent_subagent(&child_agent_id)
        .expect("persistent ownership restored");
    assert_eq!(restored.conversation_id, child_conversation_id);
    assert_eq!(restored.parent_conversation_id, parent_conversation_id);
    assert!(restarted.subagent_lineage_has_live_parent_authority(&child_agent_id));
    assert_eq!(
        restarted.subagent_scope_declaration(&child_agent_id),
        Some(expected_scope)
    );
    let child_id = AgentId::opaque(child_agent_id).unwrap();
    assert!(
        restarted
            .message_service()
            .subscription(&child_id)
            .is_some()
    );
    assert_eq!(
        restarted
            .message_service()
            .registered_identity(&child_id)
            .and_then(|identity| identity.objective.as_deref()),
        Some("Handle durable MMP requests")
    );

    service.terminate_all_pane_processes().unwrap();
    restarted.terminate_all_pane_processes().unwrap();
}

/// Verifies a terminal profile is snapshotted when its child is spawned while
/// retaining the configured provider action set for execution-time validation.
#[test]
fn runtime_terminal_profile_spawn_removes_spawn_agent_surface() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[subagents.terminal-worker]\nterminal = true\ndefault_cooperation_mode = \"explore-only\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "terminal-worker".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: true,
                read_scopes: Vec::new(),
                read_scopes_defaulted: true,
                write_scopes: Vec::new(),
                write_scopes_defaulted: true,
                session_mode: mez_agent::SubagentSessionMode::New,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "finish without delegation".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    let turn_id = spawned["turn"]["id"].as_str().unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == turn_id)
        .cloned()
        .expect("spawned terminal child turn should exist");
    let allowed_actions = service
        .agent_provider_request_control_for_turn(&turn)
        .expect("provider control should capture the child action schema")
        .0
        .expect("spawned child should have a static action set");

    assert!(!allowed_actions.contains(mez_agent::AllowedAction::SpawnAgent));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a configured profile cannot broaden a frozen parent action catalog.
///
/// The denial is evaluated before pane or turn allocation and includes bounded
/// catalog diagnostics so configuration errors can be identified without
/// observing or cleaning up partial child state.
#[test]
fn runtime_subagent_profile_action_broadening_is_denied_before_child_allocation() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[subagents.restricted]\nallowed_actions = [\"say\", \"shell_command\"]\n"
                .to_string(),
        }])
        .unwrap();
    service.set_agent_enabled_actions(mez_agent::AllowedActionSet::say_only());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let window_count = service.session().windows().len();
    let turn_count = service.agent_turn_ledger().turns().len();

    let error = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "restricted".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: true,
                read_scopes: Vec::new(),
                read_scopes_defaulted: true,
                write_scopes: Vec::new(),
                write_scopes_defaulted: true,
                session_mode: SubagentSessionMode::New,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "attempt to broaden child actions".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap_err();

    assert_eq!(
        error.kind(),
        crate::error::MezErrorKind::Forbidden,
        "{error}"
    );
    assert!(error.message().contains("parent_actions=say"), "{error}");
    assert!(
        error
            .message()
            .contains("requested_actions=say,shell_command"),
        "{error}"
    );
    assert!(error.message().contains("schema_digest="), "{error}");
    assert_eq!(service.session().windows().len(), window_count);
    assert_eq!(service.agent_turn_ledger().turns().len(), turn_count);
    assert!(service.subagent_lineage("agent-%2").is_none());
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies the built-in explorer ceiling intersects a narrow parent catalog.
///
/// The explorer baseline is a structural upper bound rather than a user-authored
/// request, so a parent limited to `say` can delegate safely without gaining
/// the explorer profile's broader read-only actions.
#[test]
fn runtime_explorer_intrinsic_ceiling_intersects_narrow_parent_catalog() {
    let mut service = test_runtime_service();
    let parent_actions = mez_agent::AllowedActionSet::say_only();
    service.set_agent_enabled_actions(parent_actions.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: true,
                read_scopes: Vec::new(),
                read_scopes_defaulted: true,
                write_scopes: Vec::new(),
                write_scopes_defaulted: true,
                session_mode: SubagentSessionMode::New,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "inspect within the parent catalog".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let pane_id = serde_json::from_str::<serde_json::Value>(&spawned).unwrap()["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    assert_eq!(
        service
            .agent_shell_store()
            .get(&pane_id)
            .and_then(|session| session.allowed_actions.as_ref()),
        Some(&parent_actions)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a spawn whose declared parent has no live session fails before it
/// can synthesize a parent catalog or allocate a child pane, turn, or lineage.
#[test]
fn runtime_subagent_missing_parent_session_rejects_without_mutation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let window_count = service.session().windows().len();
    let turn_count = service.agent_turn_ledger().turns().len();

    let error = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%9".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: true,
                read_scopes: Vec::new(),
                read_scopes_defaulted: true,
                write_scopes: Vec::new(),
                write_scopes_defaulted: true,
                session_mode: SubagentSessionMode::New,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "attempt missing parent spawn".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap_err();

    assert_eq!(
        error.kind(),
        crate::error::MezErrorKind::InvalidState,
        "{error}"
    );
    assert!(
        error.message().contains("parent session is unavailable"),
        "{error}"
    );
    assert_eq!(service.session().windows().len(), window_count);
    assert_eq!(service.agent_turn_ledger().turns().len(), turn_count);
    assert!(service.agent_shell_store().get("%9").is_none());
    assert!(service.subagent_lineage("agent-%2").is_none());
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a POSIX pane-mode subagent remains queued until authenticated
/// prompt admission and environment bootstrap settle, without creating a
/// foreign-shell boundary or claiming provider capacity early.
#[test]
fn runtime_posix_subagent_startup_releases_queued_turn_after_bootstrap() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
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
                task_prompt: "inspect managed startup".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    let pane_id = spawned["pane"]["pane_id"].as_str().unwrap().to_string();
    let turn_id = spawned["turn"]["id"].as_str().unwrap().to_string();

    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(&pane_id),
        Some("managed-admitting")
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 1);
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    assert!(!service.agent_provider_task_is_pending(&turn_id));
    assert!(!service.pane_has_uncertified_foreign_shell_boundary(&pane_id));
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .all(|transaction| transaction.pane_id != pane_id)
    );

    let token = service
        .posix_startup_token_for_tests(&pane_id)
        .unwrap()
        .to_string();
    assert_eq!(
        service
            .observe_managed_shell_protocol_event(
                &pane_id,
                mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                mez_terminal::ManagedShellAdapter::Posix,
                &token,
                &mez_terminal::ManagedShellProtocolEvent::AdapterAvailable { trigger: None },
            )
            .unwrap(),
        1
    );
    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(&pane_id),
        Some("managed-bootstrapping")
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 1);

    let (marker, bootstrap_turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.pane_id == pane_id
                && transaction.kind == RunningShellTransactionKind::Bootstrap)
                .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .unwrap();
    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            &marker,
            &bootstrap_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
        )
        .unwrap();
    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\ttest-host\n\
env\tuser\ttest-user\n\
env\tshell_path\t/bin/sh\n\
env\tshell_class\tposix-sh\n\
env\tpath\t/usr/bin:/bin\n\
env\tcwd\t/tmp\n\
env\tgit_repo\t0\n\
bootstrap\tcomplete\t1714500000\n";
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&marker)
        .unwrap();
    transaction.observed_output_bytes = output.len();
    transaction.observed_output_preview = output.to_string();
    service
        .observe_agent_shell_transaction_end(
            &pane_id,
            &marker,
            &bootstrap_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
            0,
        )
        .unwrap();

    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(&pane_id),
        Some("ready")
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    assert!(service.agent_provider_task_is_pending(&turn_id));
    assert!(!service.pane_bootstrap_is_pending_for_tests(&pane_id));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a managed pane startup admission timeout fails the queued child
/// turn and releases scheduler ownership instead of leaving it bootstrapping.
#[test]
fn runtime_subagent_startup_timeout_settles_queued_turn() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
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
                task_prompt: "inspect startup timeout".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    let pane_id = spawned["pane"]["pane_id"].as_str().unwrap();
    let turn_id = spawned["turn"]["id"].as_str().unwrap();

    assert_eq!(
        service
            .recover_expired_runtime_agent_surface_startups(u64::MAX)
            .unwrap(),
        1
    );
    assert_eq!(
        service.runtime_agent_surface_startup_phase_for_tests(pane_id),
        Some("failed")
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(service.agent_scheduler().snapshot().running, 0);
    assert!(!service.agent_provider_task_is_pending(turn_id));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a spawned subagent pane records the exact parent prompt before the
/// child turn starts.
///
/// Parent-authored task text is the child agent's effective user instruction.
/// Showing it as a `parent>` log entry lets users inspect the child pane
/// without reconstructing the prompt from parent-pane status messages.
#[test]
fn runtime_subagent_spawn_logs_parent_prompt_in_child_pane() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "explorer".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::ExploreOnly,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: false,
        write_scopes: Vec::new(),
        write_scopes_defaulted: false,
        session_mode: SubagentSessionMode::New,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "inspect the renderer issue".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: false,
    };

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            spawn,
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    assert!(spawned.contains(r#""id":"turn-1""#), "{spawned}");
    let child_pane_id = serde_json::from_str::<serde_json::Value>(&spawned)
        .unwrap()
        .get("pane")
        .and_then(|pane| pane.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .expect("spawned pane id")
        .to_string();
    let child_text = service
        .pane_screen(&child_pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");

    assert!(
        child_text.contains("parent> inspect the renderer issue"),
        "{child_text}"
    );
    assert_eq!(
        child_text
            .matches("parent> inspect the renderer issue")
            .count(),
        1,
        "the initial bridge must not duplicate the already-rendered child prompt: {child_text}"
    );
    assert!(
        !child_text.contains("subagent task started"),
        "only the child-side duplicate is suppressed: {child_text}"
    );
    service
        .start_agent_prompt_turn("%1", "inspect child startup status")
        .unwrap();
    let parent_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(
        parent_text
            .matches("subagent task queued for agent surface startup")
            .count(),
        0,
        "normal mode suppresses the initial JSON task-status presentation: {parent_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies spawned subagents inherit the parent pane's plan-only mode and
/// pane-local latency override before their first turn is created.
///
/// Both settings are session preferences rather than child-role inputs. Losing
/// either at spawn time lets a child issue writes while the parent is planning
/// or silently changes the provider-visible latency selected by the user.
#[test]
fn runtime_subagent_inherits_parent_plan_and_latency_preferences() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\nreasoning_profile = \"high\"\nlatency_preference = \"default\"\n\n[model_profiles.default.provider_options]\nreasoning_effort = \"high\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![mez_agent::ProviderModelInfo {
            id: "gpt-5.5".to_string(),
            display_name: None,
            reasoning_levels: Some(vec!["high".to_string()]),
            context_window_tokens: Some(1_050_000),
            max_input_tokens: None,
            max_output_tokens: None,
            capabilities: None,
        }],
        vec!["high".to_string()],
    );
    service
        .execute_agent_shell_plan_command("%1", "/plan on")
        .unwrap();
    service
        .execute_agent_shell_latency_command("%1", "/latency slow")
        .unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
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
                task_prompt: "inspect the inherited preferences".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let child_pane_id =
        serde_json::from_str::<serde_json::Value>(&spawned).unwrap()["pane"]["pane_id"]
            .as_str()
            .expect("spawned pane id")
            .to_string();
    let child_agent_id = format!("agent-{child_pane_id}");

    assert!(service.agent_planning_enabled(&child_pane_id));
    let (_profile_name, profile) = service
        .active_model_profile_for_pane(&child_pane_id, &child_agent_id, None)
        .unwrap();
    assert_eq!(profile.latency_preference.as_deref(), Some("slow"));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies spawning from default parent preferences leaves the child in the
/// default plan mode and does not create an unnecessary model-profile override.
///
/// Inheritance snapshots explicit pane-local choices. Synthesizing a child
/// override for defaults would detach it from later session configuration
/// changes without preserving any user-selected preference.
#[test]
fn runtime_subagent_default_preferences_do_not_create_overrides() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
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
                task_prompt: "inspect the inherited defaults".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let child_pane_id =
        serde_json::from_str::<serde_json::Value>(&spawned).unwrap()["pane"]["pane_id"]
            .as_str()
            .expect("spawned pane id")
            .to_string();
    let child_agent_id = format!("agent-{child_pane_id}");

    assert!(!service.agent_planning_enabled(&child_pane_id));
    assert!(
        !service
            .integration
            .model_profile_overrides()
            .agent_profiles
            .contains_key(&child_agent_id)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies spawned subagent conversations are durable, classified separately,
/// and excluded from default `/resume` saved-session results.
///
/// Subagents retain delegated work for direct UUID recovery, but default resume
/// discovery must not offer child sessions that lack the parent interaction
/// context.
#[test]
fn runtime_subagent_sessions_are_durable_but_hidden_from_resume() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("subagent-not-resumable"));
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "explorer".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::ExploreOnly,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: false,
        write_scopes: Vec::new(),
        write_scopes_defaulted: false,
        session_mode: SubagentSessionMode::New,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "inspect the renderer issue".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: false,
    };

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            spawn,
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let child_pane_id = serde_json::from_str::<serde_json::Value>(&spawned)
        .unwrap()
        .get("pane")
        .and_then(|pane| pane.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .expect("spawned pane id")
        .to_string();
    let child_session = service.agent_shell_store().get(&child_pane_id).unwrap();

    assert!(!child_session.ephemeral);
    assert_eq!(
        child_session.conversation_kind,
        mez_agent::AgentConversationKind::Subagent
    );
    assert_eq!(
        transcript_store
            .conversation_kind(&child_session.session_id)
            .unwrap(),
        mez_agent::AgentConversationKind::Subagent
    );
    let child_conversation_id = child_session.session_id.clone();
    let saved_child = transcript_store
        .saved_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.summary.conversation_id == child_conversation_id)
        .expect("durable subagent session should be retained in storage");
    assert_eq!(
        saved_child.conversation_kind,
        mez_agent::AgentConversationKind::Subagent
    );
    assert!(
        service
            .saved_sessions_record_browser()
            .unwrap()
            .records()
            .iter()
            .all(|record| record.id != child_conversation_id)
    );
    service.checkpoint_agent_session_metadata().unwrap();
    assert!(
        transcript_store
            .load_agent_session_metadata(service.session().id.as_str())
            .unwrap()
            .iter()
            .any(|metadata| metadata.conversation_id == child_conversation_id)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies forked children copy the parent’s bounded durable transcript into
/// their own subagent conversation while new children remain isolated.
///
/// The copied snapshot must not observe a later parent append, and neither
/// mode may reuse the parent conversation identity or authority state.
#[test]
fn runtime_subagent_session_modes_fork_bounded_history_or_start_isolated() {
    let transcript_store = AgentTranscriptStore::new(temp_root("subagent-session-modes"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let parent = service.agent_shell_store().get("%1").unwrap().clone();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: parent.session_id.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "parent-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "captured parent decision".to_string(),
        })
        .unwrap();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: parent.session_id.clone(),
            sequence: 2,
            created_at_unix_seconds: 1,
            role: TranscriptRole::Assistant,
            turn_id: "parent-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "captured parent result".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 2)
        .unwrap();

    let forked = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: SubagentSessionMode::Fork,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "continue the parent task".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let forked_pane =
        serde_json::from_str::<serde_json::Value>(&forked).unwrap()["pane"]["pane_id"]
            .as_str()
            .unwrap()
            .to_string();
    let forked_session = service
        .agent_shell_store()
        .get(&forked_pane)
        .unwrap()
        .clone();

    assert_ne!(forked_session.session_id, parent.session_id);
    assert_eq!(
        forked_session.prompt_cache_lineage_id,
        parent.prompt_cache_lineage_id
    );
    assert_eq!(forked_session.transcript_entries, 2);
    assert_eq!(
        transcript_store
            .inspect(&forked_session.session_id)
            .unwrap()
            .iter()
            .map(|entry| entry.content.as_str())
            .collect::<Vec<_>>(),
        ["captured parent decision", "captured parent result"]
    );

    transcript_store
        .append(&TranscriptEntry {
            conversation_id: parent.session_id.clone(),
            sequence: 3,
            created_at_unix_seconds: 2,
            role: TranscriptRole::User,
            turn_id: "later-parent-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "later parent mutation".to_string(),
        })
        .unwrap();

    let forked_context = service
        .agent_context_for_pane_prompt(&forked_pane, "continue", 0)
        .unwrap();
    assert!(
        forked_context
            .blocks()
            .iter()
            .any(|block| block.content == "captured parent decision")
    );
    assert!(
        !forked_context
            .blocks()
            .iter()
            .any(|block| block.content == "later parent mutation")
    );

    let fresh = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: SubagentSessionMode::New,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "inspect a self-contained task".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let fresh_pane = serde_json::from_str::<serde_json::Value>(&fresh).unwrap()["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let fresh_session = service
        .agent_shell_store()
        .get(&fresh_pane)
        .unwrap()
        .clone();
    let fresh_context = service
        .agent_context_for_pane_prompt(&fresh_pane, "inspect", 0)
        .unwrap();

    assert_ne!(fresh_session.session_id, parent.session_id);
    assert_ne!(
        fresh_session.prompt_cache_lineage_id,
        parent.prompt_cache_lineage_id
    );
    assert_eq!(fresh_session.transcript_entries, 0);
    let forked_actions = service
        .agent_shell_store()
        .get(&forked_pane)
        .and_then(|session| session.allowed_actions.as_ref())
        .expect("forked child should retain its derived action catalog");
    let fresh_actions = service
        .agent_shell_store()
        .get(&fresh_pane)
        .and_then(|session| session.allowed_actions.as_ref())
        .expect("new child should retain its derived action catalog");
    assert_eq!(forked_actions, fresh_actions);
    assert!(!forked_actions.contains(mez_agent::AllowedAction::ApplyPatch));
    assert_eq!(
        transcript_store
            .conversation_allowed_actions(&forked_session.session_id)
            .unwrap(),
        Some(forked_actions.clone())
    );
    assert_eq!(
        transcript_store
            .conversation_allowed_actions(&fresh_session.session_id)
            .unwrap(),
        Some(fresh_actions.clone())
    );
    assert!(fresh_context.blocks().iter().all(|block| {
        block.content != "captured parent decision" && block.content != "later parent mutation"
    }));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a failed forked spawn fences its queued transcript prefix before
/// deferred effects drain, so persistence cannot recreate the deleted child as
/// an orphan root conversation.
#[test]
fn runtime_failed_forked_subagent_spawn_cancels_queued_transcript_persistence() {
    let transcript_store = AgentTranscriptStore::new(temp_root("failed-forked-subagent-spawn"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service.persistence.enable_transcript_adapter();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let parent = service.agent_shell_store().get("%1").unwrap().clone();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: parent.session_id,
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "parent-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "forked child must not survive rollback".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();
    let durable_conversations_before = fs::read_dir(transcript_store.root())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_type().is_ok_and(|file_type| file_type.is_dir())
                && !entry.file_name().to_string_lossy().starts_with('.')
        })
        .map(|entry| entry.file_name())
        .collect::<std::collections::BTreeSet<_>>();

    service.fail_next_subagent_spawn_after_fork_persistence_for_tests();
    let error = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: SubagentSessionMode::Fork,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "fail after queuing the fork snapshot".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap_err();

    assert!(
        error.message().contains("after fork persistence"),
        "{error}"
    );
    assert!(service.find_pane_descriptor("%2").is_none());
    assert!(service.agent_shell_store().get("%2").is_none());
    assert!(
        service
            .drain_deferred_effects_transition()
            .side_effects
            .iter()
            .all(|effect| !matches!(effect, RuntimeSideEffect::PersistTranscriptEntries { .. }))
    );
    let durable_conversations_after = fs::read_dir(transcript_store.root())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_type().is_ok_and(|file_type| file_type.is_dir())
                && !entry.file_name().to_string_lossy().starts_with('.')
        })
        .map(|entry| entry.file_name())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(durable_conversations_after, durable_conversations_before);
    service.terminate_all_pane_processes().unwrap();
}

/// Builds a Bubblewrap runtime whose root pane is inside one trusted project.
///
/// The helper intentionally leaves configured scope arrays empty so subagent
/// inheritance exercises the trusted-project default rather than explicit
/// permission configuration.
fn trusted_project_subagent_scope_service(
    test_name: &str,
) -> (
    RuntimeSessionService,
    mez_core::ids::ClientId,
    PathBuf,
    PathBuf,
) {
    let root = temp_root(test_name);
    let project_root = root.join("project");
    let working_directory = project_root.join("src");
    fs::create_dir_all(project_root.join(".git")).unwrap();
    fs::create_dir_all(&working_directory).unwrap();
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service.set_pane_current_working_directory("%1".to_string(), working_directory.clone());
    let configured =
        crate::runtime::config::runtime_configured_permissions_from_config(&serde_json::json!({
            "permissions": {"sandbox": "bubblewrap"}
        }))
        .unwrap();
    service
        .integration
        .replace_configured_permissions(configured);
    let mut trust_store = ProjectTrustStore::default();
    trust_store
        .decide_at(
            project_root.clone(),
            TrustDecision::Trusted,
            Some(project_root.join(".git")),
            1,
        )
        .unwrap();
    service.set_project_trust_store(trust_store, None);
    (service, primary, root, project_root)
}

/// Spawns one idle child and returns its retained effective scope declaration.
fn spawn_idle_subagent_scope(
    service: &mut RuntimeSessionService,
    primary: &mez_core::ids::ClientId,
    spawn: SubagentSpawnRequest,
) -> (String, mez_agent::SubagentScopeDeclaration) {
    let spawned = service
        .spawn_runtime_subagent(
            primary,
            spawn,
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let child_agent_id =
        serde_json::from_str::<serde_json::Value>(&spawned).unwrap()["agent"]["id"]
            .as_str()
            .unwrap()
            .to_string();
    let scope = service
        .subagent_scope_declaration(&child_agent_id)
        .expect("Bubblewrap child must retain effective parent authority");
    (child_agent_id, scope)
}

/// Verifies a sandboxed root cannot mint unrestricted authority from a child's
/// own cooperation-mode request.
///
/// Root authority materialization supplies filesystem bounds only. An
/// unapproved unrestricted request must therefore be denied before any window,
/// pane process, turn, lineage record, or scope declaration exists to
/// reconcile, and the denial must stay Forbidden rather than becoming a
/// correctable argument error.
#[test]
fn runtime_unapproved_unrestricted_spawn_is_denied_before_child_state() {
    let (mut service, primary, root, _) =
        trusted_project_subagent_scope_service("runtime-unapproved-unrestricted-denied");
    let audit_root = temp_root("runtime-unapproved-unrestricted-denied-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(crate::security::audit::AuditLog::new(
        crate::security::audit::AuditConfig {
            enabled: true,
            path: audit_path.clone(),
            hash_chain: false,
            required: true,
        },
    ));
    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::Unrestricted,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: true,
        write_scopes: Vec::new(),
        write_scopes_defaulted: true,
        session_mode: SubagentSessionMode::New,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "take unrestricted authority".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };
    let window_count = service.session().windows().len();
    let turn_count = service.agent_turn_ledger().turns().len();

    let error = service
        .spawn_runtime_subagent(
            &primary,
            spawn,
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap_err();

    assert_eq!(
        error.kind(),
        crate::error::MezErrorKind::Forbidden,
        "{error}"
    );
    assert_eq!(
        error.message(),
        "unrestricted subagent writes require explicit user approval"
    );
    assert_eq!(service.session().windows().len(), window_count);
    assert_eq!(service.agent_turn_ledger().turns().len(), turn_count);
    assert_eq!(service.joined_subagent_dependency_count(), 0);
    assert!(!service.has_subagent_scope_declaration("agent-%2"));
    assert!(service.subagent_lineage("agent-%2").is_none());
    let records = fs::read_to_string(&audit_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|record| record["event_type"] == "subagent")
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["action"], "spawn");
    assert_eq!(records[0]["outcome"], "denied");
    assert_eq!(records[0]["agent_id"], serde_json::Value::Null);
    assert!(records[0]["metadata"].get("subagent_id").is_none());
    assert_eq!(records[0]["metadata"]["parent_agent_id"], "agent-%1");
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies a scoped parent that forces unrestricted without genuine approval
/// is denied and audited exactly once before any child state exists.
///
/// The explicit requested-mode check cannot see a mode that a scoped parent
/// declaration forces onto the child. The contract validation denial must still
/// be audited once and must not allocate a child pane, turn, or lineage entry.
#[test]
fn runtime_forced_unrestricted_spawn_denial_is_audited_once() {
    let (mut service, primary, root, _) =
        trusted_project_subagent_scope_service("runtime-forced-unrestricted-denial");
    let audit_root = temp_root("runtime-forced-unrestricted-denial-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(crate::security::audit::AuditLog::new(
        crate::security::audit::AuditConfig {
            enabled: true,
            path: audit_path.clone(),
            hash_chain: false,
            required: true,
        },
    ));
    // A scoped parent declaration that records an unrestricted mode without
    // genuine approval provenance bounds the child to that unapproved mode.
    service.set_subagent_scope_declaration(
        "agent-%1",
        mez_agent::SubagentScopeDeclaration {
            cooperation_mode: CooperationMode::Unrestricted,
            approval_provenance: mez_agent::SubagentApprovalProvenance::Requested,
            current_directory: "/repo".to_string(),
            read_scopes: vec!["/repo".to_string()],
            write_scopes: vec!["/repo".to_string()],
            permission_preset: None,
        },
    );
    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: true,
        write_scopes: Vec::new(),
        write_scopes_defaulted: true,
        session_mode: SubagentSessionMode::New,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "inherit an unapproved mode".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };
    let window_count = service.session().windows().len();
    let turn_count = service.agent_turn_ledger().turns().len();

    let error = service
        .spawn_runtime_subagent(
            &primary,
            spawn,
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap_err();

    assert_eq!(
        error.kind(),
        crate::error::MezErrorKind::Forbidden,
        "{error}"
    );
    assert_eq!(
        error.message(),
        "unrestricted subagent writes require explicit user approval"
    );
    assert_eq!(service.session().windows().len(), window_count);
    assert_eq!(service.agent_turn_ledger().turns().len(), turn_count);
    assert!(!service.has_subagent_scope_declaration("agent-%2"));
    assert!(service.subagent_lineage("agent-%2").is_none());
    let records = fs::read_to_string(&audit_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|record| record["event_type"] == "subagent")
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["action"], "spawn");
    assert_eq!(records[0]["outcome"], "denied");
    assert_eq!(records[0]["agent_id"], serde_json::Value::Null);
    assert!(records[0]["metadata"].get("subagent_id").is_none());
    assert_eq!(records[0]["metadata"]["parent_agent_id"], "agent-%1");
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies genuine approval still authorizes unrestricted descendants.
///
/// An authenticated primary approval, and the approved unrestricted parent it
/// creates, remain valid sources of unrestricted authority, so an explicit
/// unrestricted request and a nested descendant request both continue to work
/// without re-deriving approval from a requested cooperation mode.
#[test]
fn runtime_approved_unrestricted_parent_authorizes_descendant() {
    let (mut service, primary, root, project_root) =
        trusted_project_subagent_scope_service("runtime-approved-unrestricted-descendant");
    let expected = project_root.to_string_lossy().into_owned();
    let approved = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::Unrestricted,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: true,
        write_scopes: Vec::new(),
        write_scopes_defaulted: true,
        session_mode: SubagentSessionMode::New,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "own the deliverable end to end".to_string(),
        explicit_user_approval: true,
        skip_initial_turn: true,
    };

    let (child_agent_id, scope) = spawn_idle_subagent_scope(&mut service, &primary, approved);
    assert_eq!(scope.cooperation_mode, CooperationMode::Unrestricted);
    assert_eq!(
        scope.approval_provenance,
        mez_agent::SubagentApprovalProvenance::ExplicitUserApproval
    );
    assert!(scope.carries_approved_unrestricted_authority());
    assert_eq!(scope.read_scopes, vec![expected.clone()]);
    assert_eq!(scope.write_scopes, vec![expected]);

    let descendant = SubagentSpawnRequest {
        parent_agent_id: child_agent_id,
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::Unrestricted,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: true,
        write_scopes: Vec::new(),
        write_scopes_defaulted: true,
        session_mode: SubagentSessionMode::New,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "continue the approved work".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };
    let (_, descendant_scope) = spawn_idle_subagent_scope(&mut service, &primary, descendant);

    assert_eq!(
        descendant_scope.cooperation_mode,
        CooperationMode::Unrestricted
    );
    assert!(descendant_scope.carries_approved_unrestricted_authority());
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies an authenticated primary control request still authorizes an
/// unrestricted child end to end.
///
/// The primary client role is the authenticated approval source, so the same
/// sandboxed root that rejects a child's unapproved request must accept this
/// request and record explicit approval provenance on the child declaration.
#[test]
fn runtime_primary_control_spawn_allows_approved_unrestricted_child() {
    let (mut service, primary, root, project_root) =
        trusted_project_subagent_scope_service("runtime-primary-unrestricted-control");
    let expected = project_root.to_string_lossy().into_owned();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"primary-unrestricted","method":"agent/spawn","params":{"parent_agent":{"agent_id":"agent-%1"},"placement":{"mode":"new-pane"},"role":"worker","cooperation_mode":"unrestricted","prompt":"own the deliverable end to end","idempotency_key":"primary-unrestricted-spawn"}}"#,
        &primary,
    );
    assert!(response.contains("\"result\""), "{response}");
    let child_agent_id =
        serde_json::from_str::<serde_json::Value>(&response).unwrap()["result"]["agent"]["id"]
            .as_str()
            .unwrap()
            .to_string();

    let scope = service
        .subagent_scope_declaration(&child_agent_id)
        .expect("approved unrestricted child must retain its scope declaration");
    assert_eq!(scope.cooperation_mode, CooperationMode::Unrestricted);
    assert_eq!(
        scope.approval_provenance,
        mez_agent::SubagentApprovalProvenance::ExplicitUserApproval
    );
    assert!(scope.carries_approved_unrestricted_authority());
    assert_eq!(scope.read_scopes, vec![expected.clone()]);
    assert_eq!(scope.write_scopes, vec![expected]);
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies ordinary narrowed worker spawns keep their mode and gain no
/// approval from root filesystem bounds.
///
/// Root bounds must not force explore-only and must not be reported as an
/// approval source, so an owned-write worker keeps its requested mode while
/// inheriting the parent's narrowed filesystem authority.
#[test]
fn runtime_root_bounds_keep_ordinary_worker_mode_without_approval() {
    let (mut service, primary, root, project_root) =
        trusted_project_subagent_scope_service("runtime-root-bounds-worker-mode");
    let expected = project_root.to_string_lossy().into_owned();
    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: true,
        write_scopes: Vec::new(),
        write_scopes_defaulted: true,
        session_mode: SubagentSessionMode::New,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "implement the bounded change".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };

    let (_, scope) = spawn_idle_subagent_scope(&mut service, &primary, spawn);

    assert_eq!(scope.cooperation_mode, CooperationMode::OwnedWrite);
    assert_eq!(
        scope.approval_provenance,
        mez_agent::SubagentApprovalProvenance::Requested
    );
    assert!(!scope.carries_approved_unrestricted_authority());
    assert_eq!(scope.read_scopes, vec![expected.clone()]);
    assert_eq!(scope.write_scopes, vec![expected]);
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies omitted child scopes inherit the root parent's trusted-project
/// Bubblewrap authority instead of independently deriving authority later.
#[test]
fn runtime_subagent_omitted_scopes_inherit_parent_bubblewrap_authority() {
    let (mut service, primary, root, project_root) =
        trusted_project_subagent_scope_service("runtime-subagent-inherit-bubblewrap");
    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: true,
        write_scopes: Vec::new(),
        write_scopes_defaulted: true,
        session_mode: SubagentSessionMode::New,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "implement the bounded change".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };

    let (_, scope) = spawn_idle_subagent_scope(&mut service, &primary, spawn);
    let expected = project_root.to_string_lossy().into_owned();

    assert_eq!(scope.read_scopes, vec![expected.clone()]);
    assert_eq!(scope.write_scopes, vec![expected]);
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies an explorer with omitted scopes inherits confined read authority
/// without inheriting the writable parent's write authority.
///
/// Compact explorer actions omit scope arrays, so runtime normalization must
/// clear inherited writes before validating the explore-only cooperation mode.
#[test]
fn runtime_explorer_omitted_scopes_clear_inherited_write_authority() {
    let (mut service, primary, root, project_root) =
        trusted_project_subagent_scope_service("runtime-explorer-inherit-bubblewrap");
    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "explorer".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::ExploreOnly,
        cooperation_mode_defaulted: true,
        read_scopes: Vec::new(),
        read_scopes_defaulted: true,
        write_scopes: Vec::new(),
        write_scopes_defaulted: true,
        session_mode: SubagentSessionMode::New,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "inspect the bounded change".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };

    let (_, scope) = spawn_idle_subagent_scope(&mut service, &primary, spawn);
    let expected = project_root.to_string_lossy().into_owned();

    assert_eq!(scope.read_scopes, vec![expected]);
    assert!(scope.write_scopes.is_empty());
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies host access retains child coordination metadata without
/// manufacturing inherited Bubblewrap authority.
#[test]
fn runtime_host_access_subagent_retains_coordination_scope() {
    let (mut service, primary, root, _) =
        trusted_project_subagent_scope_service("runtime-subagent-host-access");
    service.set_pane_approval_policy_override("%1", Some(ApprovalPolicy::HostAccess));
    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: true,
        write_scopes: Vec::new(),
        write_scopes_defaulted: true,
        session_mode: SubagentSessionMode::New,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "work with inherited host access".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };
    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            spawn,
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let child_agent_id =
        serde_json::from_str::<serde_json::Value>(&spawned).unwrap()["agent"]["id"]
            .as_str()
            .unwrap()
            .to_string();

    let scope = service
        .subagent_scope_declaration(&child_agent_id)
        .expect("host-access child must retain coordination metadata");
    assert_eq!(scope.cooperation_mode, CooperationMode::OwnedWrite);
    assert!(scope.read_scopes.is_empty());
    assert!(scope.write_scopes.is_empty());
    assert!(matches!(
        service.configured_permissions().sandbox,
        crate::runtime::SandboxConfig::Bubblewrap(_)
    ));
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies explicit empty child scope arrays remain empty and therefore deny
/// Bubblewrap filesystem authority rather than behaving like omitted fields.
#[test]
fn runtime_subagent_explicit_empty_scopes_do_not_inherit_parent_authority() {
    let (mut service, primary, root, _) =
        trusted_project_subagent_scope_service("runtime-subagent-explicit-empty-bubblewrap");
    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: false,
        write_scopes: Vec::new(),
        write_scopes_defaulted: false,
        session_mode: SubagentSessionMode::New,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "perform no filesystem work".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };

    let (_, scope) = spawn_idle_subagent_scope(&mut service, &primary, spawn);

    assert!(scope.read_scopes.is_empty());
    assert!(scope.write_scopes.is_empty());
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies spawned children receive a parent pane's explicit sandbox backend
/// as a child-local snapshot before their first agent turn is started.
#[test]
fn runtime_subagent_inherits_explicit_parent_sandbox_override() {
    let (mut service, primary, root, _) =
        trusted_project_subagent_scope_service("runtime-subagent-inherit-sandbox-override");
    service
        .integration
        .set_pane_sandbox_override("%1", Some(crate::runtime::SandboxConfig::PolicyOnly));
    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: true,
        write_scopes: Vec::new(),
        write_scopes_defaulted: true,
        session_mode: SubagentSessionMode::New,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "inherit sandbox state".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };

    let (child_agent_id, _) = spawn_idle_subagent_scope(&mut service, &primary, spawn);
    let child_pane_id = child_agent_id
        .strip_prefix("agent-")
        .expect("runtime subagent ids contain their pane id");

    assert!(service.pane_has_sandbox_override(child_pane_id));
    assert!(matches!(
        service.sandbox_config_for_pane(child_pane_id),
        crate::runtime::SandboxConfig::PolicyOnly
    ));
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies child-requested scopes may narrow inherited Bubblewrap authority
/// but cannot add a sibling path outside the trusted parent scope.
#[test]
fn runtime_subagent_requested_scopes_only_narrow_parent_authority() {
    let (mut service, primary, root, project_root) =
        trusted_project_subagent_scope_service("runtime-subagent-narrow-bubblewrap");
    let spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: vec!["generated".to_string(), "../../outside".to_string()],
        read_scopes_defaulted: false,
        write_scopes: vec!["generated".to_string(), "../../outside".to_string()],
        write_scopes_defaulted: false,
        session_mode: SubagentSessionMode::New,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "update generated files".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };

    let (_, scope) = spawn_idle_subagent_scope(&mut service, &primary, spawn);

    assert_eq!(scope.read_scopes, vec!["generated"]);
    assert_eq!(scope.write_scopes, vec!["generated"]);
    assert!(
        !scope
            .read_scopes
            .iter()
            .any(|scope| scope.contains("outside"))
    );
    assert!(project_root.join("src").starts_with(&project_root));
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies a nested child inherits its scoped parent's already narrowed
/// authority even when its own pane remains under a broader trusted project.
#[test]
fn runtime_nested_subagent_cannot_rediscover_broader_trusted_authority() {
    let (mut service, primary, root, _) =
        trusted_project_subagent_scope_service("runtime-nested-subagent-bubblewrap");
    let first_spawn = SubagentSpawnRequest {
        parent_agent_id: "agent-%1".to_string(),
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: vec!["generated".to_string()],
        read_scopes_defaulted: false,
        write_scopes: vec!["generated".to_string()],
        write_scopes_defaulted: false,
        session_mode: SubagentSessionMode::New,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "own generated files".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };
    let (parent_agent_id, parent_scope) =
        spawn_idle_subagent_scope(&mut service, &primary, first_spawn);
    let nested_spawn = SubagentSpawnRequest {
        parent_agent_id,
        requested_role: "worker".to_string(),
        placement: "new-pane".to_string(),
        cooperation_mode: CooperationMode::OwnedWrite,
        cooperation_mode_defaulted: false,
        read_scopes: Vec::new(),
        read_scopes_defaulted: true,
        write_scopes: Vec::new(),
        write_scopes_defaulted: true,
        session_mode: SubagentSessionMode::New,
        initial_model_size: None,
        initial_reasoning_effort: None,
        task_prompt: "continue generated work".to_string(),
        explicit_user_approval: false,
        skip_initial_turn: true,
    };

    let (_, nested_scope) = spawn_idle_subagent_scope(&mut service, &primary, nested_spawn);

    assert_eq!(nested_scope.read_scopes, parent_scope.read_scopes);
    assert_eq!(nested_scope.write_scopes, parent_scope.write_scopes);
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Verifies runtime agent shell prompt starts live turn lifecycle.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_agent_shell_prompt_starts_live_turn_lifecycle() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt","input":"summarize the pane"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"turn_started""#), "{response}");
    assert!(response.contains(r#""command":null"#), "{response}");
    assert!(response.contains(r#""body":null"#), "{response}");
    assert!(response.contains(r#""state":"running""#), "{response}");
    let response_json: serde_json::Value = serde_json::from_str(&response).unwrap();
    let turn = &response_json["result"]["turn"];
    assert_eq!(turn["id"], "turn-1", "{response}");
    assert_eq!(turn["version"], serde_json::json!(1), "{response}");
    assert_eq!(turn["agent_id"], "agent-%1", "{response}");
    assert_eq!(turn["state"], "running", "{response}");
    assert!(turn["created_at"].as_str().is_some(), "{response}");
    assert!(turn["started_at"].as_str().is_some(), "{response}");
    assert_eq!(turn["finished_at"], serde_json::Value::Null, "{response}");
    assert_eq!(turn["prompt_preview"], "summarize the pane", "{response}");
    assert_eq!(turn["approval_ids"], serde_json::json!([]), "{response}");
    assert_eq!(
        turn["result_summary"],
        serde_json::Value::Null,
        "{response}"
    );
    assert!(
        turn["extensions"]["context_blocks"].as_u64().is_some(),
        "{response}"
    );
    let tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"tasks","method":"agent/task/list","params":{"target":{"pane_id":"%1"}}}"#,
        &primary,
    );
    assert!(tasks.contains(r#""id":"turn-1""#), "{tasks}");
    assert!(tasks.contains(r#""state":"running""#), "{tasks}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].turn_id, "turn-1");
    assert_eq!(pending[0].model_profile.provider, "openai");
    assert_eq!(pending[0].model_profile.model, "gpt-5.6-terra");
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("agent: working on"), "{pane_text}");
}

/// Verifies that a user prompt and a non-command agent response are written
/// into the pane's normal terminal buffer instead of a transient prompt
/// overlay. This preserves the Codex-like interaction transcript as copyable
/// terminal text while still retaining terminal style spans for user-facing
/// color. Each injected line keeps the same Mezzanine UI prefix used by the
/// pane-local prompt so message boundaries are visible in the terminal buffer.
#[test]
fn runtime_agent_prompt_and_say_response_are_interleaved_in_pane_buffer() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-visible-say","input":"summarize visible output"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap say response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "test action batch rationale".to_string(),

                actions: vec![mez_agent::AgentAction {
                    id: "say-1".to_string(),

                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: "The pane is ready.".to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                    },
                }],
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

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("user> summarize visible output"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("▐ user> summarize visible output"),
        "{pane_text}"
    );
    assert!(pane_text.contains("mez> The pane is ready."), "{pane_text}");
    assert!(
        pane_text.contains("▐ mez> The pane is ready."),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("mez> answer in the pane"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("agent: turn turn-1"), "{pane_text}");
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let assistant_line = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("mez> The pane is ready."))
        .unwrap();
    assert!(assistant_line.text.starts_with("▐ "));
    assert!(!assistant_line.style_spans.is_empty());
    let assistant_body_start = "▐ mez> ".chars().count();
    assert!(
        assistant_line
            .style_spans
            .iter()
            .all(|span| span.start.saturating_add(span.length) <= assistant_body_start),
        "assistant body text should use default terminal color: {:?}",
        assistant_line.style_spans
    );
    assert!(
        assistant_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground
                    == Some(theme.colors.agent_transcript_assistant.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "assistant gutter and label should use themed foreground without a background: {:?}",
        assistant_line.style_spans
    );
    let user_line = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("user> summarize visible output"))
        .unwrap();
    let user_body_start = "▐ user> ".chars().count();
    assert!(
        user_line
            .style_spans
            .iter()
            .all(|span| span.start.saturating_add(span.length) <= user_body_start),
        "user prompt body text should use default terminal color: {:?}",
        user_line.style_spans
    );
    assert!(
        user_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground == Some(theme.colors.agent_transcript_user.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "user gutter and label should use themed foreground without a background: {:?}",
        user_line.style_spans
    );
    service
        .append_agent_error_text_to_terminal_buffer("%1", "agent error: failed")
        .unwrap();
    service
        .append_agent_command_preview_to_terminal_buffer("%1", "ls -la")
        .unwrap();
    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let error_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent error: failed"))
        .unwrap();
    assert!(
        error_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground == Some(theme.colors.agent_transcript_error.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "error transcript lines should use themed error foreground without a background: {:?}",
        error_line.style_spans
    );
    let command_line = styled_lines
        .iter()
        .find(|line| line.text.contains("$ ls -la"))
        .unwrap();
    assert!(
        command_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground
                    == Some(theme.colors.agent_transcript_command.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "command transcript lines should use themed command foreground without a background: {:?}",
        command_line.style_spans
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies visible-pane user prompt transcript lines wrap to the bounded pane
/// width with a sixth-column hanging indent for continuation rows.
///
/// Long user-entered transcript lines should use the same bounded renderer as
/// other visible pane logs so they stay within the pane width or the 120-column
/// cap. Wrapped continuation rows align with the `mez> ` continuation column
/// instead of repeating the `user> ` label so the copied transcript remains
/// readable.
#[test]
fn runtime_user_prompt_logs_wrap_with_sixth_column_hanging_indent() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(24, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(24, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    service
        .append_agent_user_prompt_to_terminal_buffer("%1", "alpha beta gamma delta epsilon")
        .unwrap();

    let user_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .into_iter()
        .filter(|line| line.starts_with("▐ "))
        .collect::<Vec<_>>();
    assert!(
        user_lines.iter().any(|line| line == "▐ user> alpha beta"),
        "{user_lines:#?}"
    );
    assert!(
        user_lines.iter().any(|line| line == "▐      gamma delta"),
        "{user_lines:#?}"
    );
    assert!(
        user_lines.iter().any(|line| line == "▐      epsilon"),
        "{user_lines:#?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies pasted provider diagnostics remain normal prompt text.
///
/// Users often paste the previous terminal failure back into the agent shell for
/// diagnosis. That text can contain JSON error payloads, wrapped words, and the
/// provider_error marker, but it is still user-authored prompt content. The
/// runtime should render it through the agent transcript presentation path
/// without surfacing a secondary terminal presentation failure.
#[test]
fn runtime_agent_user_prompt_renders_pasted_provider_error_without_terminal_failure() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 12).unwrap(), 120).unwrap(),
    );
    let prompt = "provider_error: InvalidState: OpenAI Responses-compatible provider `lmstudio` is not authenticated\nInvalidState: terminal step failed: {\"code\":-32004,\n\"data\":{\"mezzanine_code\":\"invalid_state\"},\"message\":\"agent terminal presentation feed panicked while appending styled agent\n lines\"}";

    service
        .append_agent_user_prompt_to_terminal_buffer("%1", prompt)
        .unwrap();

    let pane_text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("provider_error: InvalidState"),
        "{pane_text}"
    );
    assert!(pane_text.contains("terminal step failed"), "{pane_text}");
}

/// Verifies an explicit spawn size and reasoning pair pins the child’s durable
/// model identity while per-turn routing stays the pane’s configured policy.
///
/// The pair selects the child’s first profile before provider scheduling and
/// suppresses automatic routing for that turn, and it must also become the
/// child’s agent-scoped profile so later turns keep the requested model and
/// reasoning level instead of the inherited default. Routing remains a
/// separate, user-configured policy: when the pane enables it, later turns stay
/// eligible for router dispatch.
#[test]
fn runtime_subagent_explicit_selection_pins_child_model_identity() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"
[agents]
default_provider = "runtime-batch"
default_model_profile = "default"
routing = true

[agents.auto_sizing]
router_model_profile = "router"
small_model_profile = "small"
medium_model_profile = "medium"
large_model_profile = "large"
allowed_reasoning_efforts = ["low", "medium", "high", "xhigh"]

[providers.runtime-batch]
kind = "openai"
models = ["gpt-default", "gpt-router", "gpt-small", "gpt-medium", "gpt-large"]
default_model = "gpt-default"

[model_profiles.default]
provider = "runtime-batch"
model = "gpt-default"
reasoning_profile = "medium"

[model_profiles.router]
provider = "runtime-batch"
model = "gpt-router"
reasoning_profile = "low"

[model_profiles.small]
provider = "runtime-batch"
model = "gpt-small"
reasoning_profile = "medium"

[model_profiles.medium]
provider = "runtime-batch"
model = "gpt-medium"
reasoning_profile = "medium"

[model_profiles.large]
provider = "runtime-batch"
model = "gpt-large"
reasoning_profile = "high"

[subagents.terminal-worker]
terminal = true

[subagents.restricted-worker]
allowed_actions = ["say", "shell_command"]
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let mut parent_auto_sizing = service.agent_auto_sizing().clone();
    parent_auto_sizing.small_model_profile = "large".to_string();
    parent_auto_sizing.medium_model_profile = "large".to_string();
    parent_auto_sizing.large_model_profile = "large".to_string();
    parent_auto_sizing.allowed_reasoning_efforts = vec!["high".to_string()];
    service.set_agent_auto_sizing_override("%1", Some(parent_auto_sizing));
    service
        .capture_agent_session_allowed_actions_for_pane("%1")
        .expect("parent should freeze its advertised sizing catalog");
    let mut reloaded_auto_sizing = service.agent_auto_sizing().clone();
    reloaded_auto_sizing.small_model_profile = "small".to_string();
    reloaded_auto_sizing.medium_model_profile = "small".to_string();
    reloaded_auto_sizing.large_model_profile = "small".to_string();
    reloaded_auto_sizing.allowed_reasoning_efforts = vec!["low".to_string()];
    service.set_agent_auto_sizing_override("%1", Some(reloaded_auto_sizing));

    let spawned = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: SubagentSessionMode::New,
                initial_model_size: Some("large".to_string()),
                initial_reasoning_effort: Some("high".to_string()),
                task_prompt: "implement the bounded change".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap();
    let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
    let child_agent_id = spawned["agent"]["id"].as_str().unwrap().to_string();
    let child_pane_id = spawned["pane"]["pane_id"].as_str().unwrap().to_string();
    let first_turn_id = spawned["turn"]["id"].as_str().unwrap().to_string();
    let child_catalog = service
        .agent_shell_store()
        .get(&child_pane_id)
        .and_then(|session| session.allowed_actions.as_ref())
        .expect("child should capture its session catalog after inherited policy setup");
    let child_sizing = child_catalog
        .spawn_agent_sizing()
        .expect("child catalog should include inherited spawn sizing");
    assert!(
        child_sizing
            .sizes
            .iter()
            .all(|option| option.profile_name == "large")
    );
    let first_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == first_turn_id)
        .cloned()
        .unwrap();
    let first_profile = service.agent_turn_model_profile(&first_turn_id).unwrap();

    assert_eq!(spawned["agent"]["initial_model_size"], "large");
    assert_eq!(spawned["agent"]["initial_reasoning_effort"], "high");
    assert_eq!(spawned["agent"]["initial_model_profile"], "large");
    assert_eq!(first_profile.model, "gpt-large");
    assert_eq!(first_profile.reasoning_profile.as_deref(), Some("high"));
    assert!(service.agent_turn_routing_applied(&first_turn_id));
    assert!(
        service
            .runtime_auto_sizing_dispatch_for_turn(&first_turn, first_profile)
            .unwrap()
            .is_none()
    );

    service.stop_agent_turn_for_pane(&child_pane_id).unwrap();
    let second = service
        .start_agent_prompt_turn(&child_pane_id, "follow up on the change")
        .unwrap();
    let second_turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == second.turn_id)
        .cloned()
        .unwrap();
    let second_profile = service.agent_turn_model_profile(&second.turn_id).unwrap();

    let (durable_profile_name, durable_profile) = service
        .active_model_profile_for_pane(&child_pane_id, &child_agent_id, None)
        .unwrap();
    assert_eq!(second_turn.model_profile, durable_profile_name);
    assert_eq!(second_profile.model, "gpt-large");
    assert_eq!(second_profile.reasoning_profile.as_deref(), Some("high"));
    assert_eq!(durable_profile.model, "gpt-large");
    assert_eq!(durable_profile.reasoning_profile.as_deref(), Some("high"));
    assert!(!service.agent_turn_routing_applied(&second.turn_id));
    // Routing stays the pane's separate per-turn policy: this fixture enables
    // it, so a later child turn remains eligible for router dispatch even
    // though the explicit pair defines the child's model.
    assert!(
        service
            .runtime_auto_sizing_dispatch_for_turn(&second_turn, second_profile)
            .unwrap()
            .is_some()
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies explicit child sizing resolves from the parent catalog that
/// advertised it even when terminal, depth-limited, or profile-restricted
/// child catalogs intentionally omit `spawn_agent` and its sizing metadata.
///
/// The parent snapshot offers only the captured large/high pair. The live
/// parent sizing override changes afterward, so successful initial selections
/// prove child catalog pruning and live routing state cannot replace the
/// frozen parent contract.
#[test]
fn runtime_subagent_explicit_selection_uses_parent_catalog_when_child_hides_spawn() {
    for requested_role in ["terminal-worker", "explorer", "restricted-worker"] {
        let mut service = test_runtime_service();
        service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
        service
            .replace_config_layers(vec![ConfigLayer {
                name: "parent-frozen-sizing".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: r#"
[agents]
default_provider = "runtime-batch"
default_model_profile = "default"
max_depth = 1

[agents.auto_sizing]
router_model_profile = "large"
small_model_profile = "large"
medium_model_profile = "large"
large_model_profile = "large"
allowed_reasoning_efforts = ["high"]

[providers.runtime-batch]
kind = "openai"
models = ["gpt-default", "gpt-large", "gpt-small"]
default_model = "gpt-default"

[model_profiles.default]
provider = "runtime-batch"
model = "gpt-default"

[model_profiles.large]
provider = "runtime-batch"
model = "gpt-large"
reasoning_profile = "high"

[model_profiles.small]
provider = "runtime-batch"
model = "gpt-small"
reasoning_profile = "low"

[subagents.terminal-worker]
terminal = true

[subagents.restricted-worker]
allowed_actions = ["say", "shell_command"]
"#
                .to_string(),
            }])
            .unwrap();
        let primary = service
            .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
            .unwrap();
        service.start_initial_pane_process(Some("cat")).unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service
            .capture_agent_session_allowed_actions_for_pane("%1")
            .expect("parent should freeze the advertised sizing catalog");
        service
            .replace_config_layers(vec![ConfigLayer {
                name: "live-sizing-reload".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: r#"
[agents]
default_provider = "runtime-batch"
default_model_profile = "default"
max_depth = 1

[agents.auto_sizing]
router_model_profile = "small"
small_model_profile = "small"
medium_model_profile = "small"
large_model_profile = "small"
allowed_reasoning_efforts = ["low"]

[providers.runtime-batch]
kind = "openai"
models = ["gpt-default", "gpt-large", "gpt-small"]
default_model = "gpt-default"

[model_profiles.default]
provider = "runtime-batch"
model = "gpt-default"

[model_profiles.large]
provider = "runtime-batch"
model = "gpt-large"
reasoning_profile = "high"

[model_profiles.small]
provider = "runtime-batch"
model = "gpt-small"
reasoning_profile = "low"

[subagents.terminal-worker]
terminal = true

[subagents.restricted-worker]
allowed_actions = ["say", "shell_command"]
"#
                .to_string(),
            }])
            .expect("live sizing reload should succeed after parent capture");

        let spawned = service
            .spawn_runtime_subagent(
                &primary,
                SubagentSpawnRequest {
                    parent_agent_id: "agent-%1".to_string(),
                    requested_role: requested_role.to_string(),
                    placement: "new-pane".to_string(),
                    cooperation_mode: CooperationMode::ExploreOnly,
                    cooperation_mode_defaulted: false,
                    read_scopes: Vec::new(),
                    read_scopes_defaulted: false,
                    write_scopes: Vec::new(),
                    write_scopes_defaulted: false,
                    session_mode: SubagentSessionMode::New,
                    initial_model_size: Some("large".to_string()),
                    initial_reasoning_effort: Some("high".to_string()),
                    task_prompt: "complete the bounded child task".to_string(),
                    explicit_user_approval: false,
                    skip_initial_turn: false,
                },
                RuntimeSubagentPlacement::NewPane {
                    direction: SplitDirection::Vertical,
                    select: true,
                },
            )
            .unwrap_or_else(|error| panic!("{requested_role} child sizing failed: {error}"));
        let spawned = serde_json::from_str::<serde_json::Value>(&spawned).unwrap();
        let child_pane_id = spawned["pane"]["pane_id"].as_str().unwrap();
        let turn_id = spawned["turn"]["id"].as_str().unwrap();
        let child_catalog = service
            .agent_shell_store()
            .get(child_pane_id)
            .and_then(|session| session.allowed_actions.as_ref())
            .expect("child should retain its derived action catalog");

        assert!(
            !child_catalog.contains(mez_agent::AllowedAction::SpawnAgent),
            "{requested_role} should hide spawn_agent from the child catalog"
        );
        assert_eq!(child_catalog.spawn_agent_sizing(), None);
        if requested_role == "explorer" {
            for action in [
                mez_agent::AllowedAction::ApplyPatch,
                mez_agent::AllowedAction::ConfigChange,
                mez_agent::AllowedAction::MemoryStore,
                mez_agent::AllowedAction::IssueAdd,
                mez_agent::AllowedAction::IssueUpdate,
                mez_agent::AllowedAction::IssueDelete,
            ] {
                assert!(
                    !child_catalog.contains(action),
                    "explorer catalog must structurally omit {}",
                    action.action_type(),
                );
            }
            for action in [
                mez_agent::AllowedAction::ShellCommand,
                mez_agent::AllowedAction::McpCall,
                mez_agent::AllowedAction::SendMessage,
                mez_agent::AllowedAction::IssueQuery,
            ] {
                assert!(
                    child_catalog.contains(action),
                    "explorer catalog must retain {} for runtime-controlled execution",
                    action.action_type(),
                );
            }
        }
        assert_eq!(spawned["agent"]["initial_model_profile"], "large");
        assert_eq!(
            service.agent_turn_model_profile(turn_id).unwrap().model,
            "gpt-large"
        );
        service.terminate_all_pane_processes().unwrap();
    }
}

/// Verifies rejected explicit sizing pairs clean every child resource created
/// before the child turn selection is resolved.
///
/// Spawn setup must allocate the child pane before applying inherited policy,
/// but an invalid pair must still leave no pane, session, lineage, or model
/// override behind when deterministic selection rejects it.
#[test]
fn runtime_subagent_invalid_explicit_pair_cleans_allocated_child_state() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let turn_count = service.agent_turn_ledger().turns().len();

    let error = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: SubagentSessionMode::New,
                initial_model_size: Some("small".to_string()),
                initial_reasoning_effort: Some("invalid".to_string()),
                task_prompt: "inspect the bounded change".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap_err();

    assert!(error.message().contains("reasoning"), "{error}");
    assert!(service.find_pane_descriptor("%2").is_none());
    assert!(service.agent_shell_store().get("%2").is_none());
    assert!(!service.has_subagent_authority_state("agent-%2"));
    assert!(!service.has_subagent_lineage("agent-%2"));
    assert!(
        !service
            .integration
            .model_profile_overrides()
            .agent_profiles
            .contains_key("agent-%2")
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), turn_count);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a globally valid size and reasoning pair that was not advertised by
/// the frozen parent catalog is rejected before child pane allocation.
#[test]
fn runtime_subagent_rejects_valid_but_unadvertised_frozen_sizing_without_side_effects() {
    let mut service = test_runtime_service();
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let frozen = mez_agent::AllowedActionSet::all_enabled().with_spawn_agent_sizing(
        mez_agent::SpawnAgentSizing {
            sizes: vec![mez_agent::SpawnAgentSizeOption {
                size: "large".to_string(),
                profile_name: "default".to_string(),
                execution_profile: Some(runtime_model_profile("runtime-batch", "gpt-default")),
                allowed_reasoning_efforts: vec!["high".to_string()],
            }],
        },
    );
    service
        .agent_shell_store_mut()
        .restore_allowed_actions("%1", frozen)
        .unwrap();
    let pane_count = service
        .session()
        .windows()
        .iter()
        .flat_map(|window| window.panes())
        .count();
    let turn_count = service.agent_turn_ledger().turns().len();

    let error = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: SubagentSessionMode::New,
                initial_model_size: Some("small".to_string()),
                initial_reasoning_effort: Some("medium".to_string()),
                task_prompt: "reject the unadvertised frozen pair".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap_err();

    assert!(error.message().contains("frozen action catalog"), "{error}");
    assert_eq!(
        service
            .session()
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .count(),
        pane_count
    );
    assert!(service.find_pane_descriptor("%2").is_none());
    assert!(service.agent_shell_store().get("%2").is_none());
    assert!(!service.has_subagent_authority_state("agent-%2"));
    assert_eq!(service.agent_turn_ledger().turns().len(), turn_count);
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a configured but unavailable child profile rejects before pane,
/// process, fork-persistence, or durable child metadata allocation.
#[test]
fn runtime_subagent_unavailable_profile_cleans_allocated_child_state() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("missing-child-profile"));
    service.set_agent_transcript_store(transcript_store.clone());
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    let mut profiles = service.integration.subagent_profiles().clone();
    profiles
        .get_mut("explorer")
        .expect("built-in explorer profile")
        .model_profile = Some("unavailable-profile".to_string());
    service.integration.replace_subagent_profiles(profiles);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let turn_count = service.agent_turn_ledger().turns().len();

    let error = service
        .spawn_runtime_subagent(
            &primary,
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".to_string(),
                requested_role: "explorer".to_string(),
                placement: "new-pane".to_string(),
                cooperation_mode: CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: SubagentSessionMode::Fork,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: "inspect the bounded change".to_string(),
                explicit_user_approval: false,
                skip_initial_turn: false,
            },
            RuntimeSubagentPlacement::NewPane {
                direction: SplitDirection::Vertical,
                select: true,
            },
        )
        .unwrap_err();

    assert!(error.message().contains("unavailable-profile"), "{error}");
    assert!(service.find_pane_descriptor("%2").is_none());
    assert!(service.agent_shell_store().get("%2").is_none());
    assert!(!service.has_subagent_authority_state("agent-%2"));
    assert!(!service.has_subagent_lineage("agent-%2"));
    assert!(
        !service
            .integration
            .model_profile_overrides()
            .agent_profiles
            .contains_key("agent-%2")
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), turn_count);
    assert!(
        transcript_store.saved_sessions().unwrap().is_empty(),
        "missing child profile must not create a durable fork conversation"
    );
    service.terminate_all_pane_processes().unwrap();
}
