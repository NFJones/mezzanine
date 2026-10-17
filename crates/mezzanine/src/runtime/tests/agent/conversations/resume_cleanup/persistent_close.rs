//! Explicit persistent close authority across independent binding replacement.
//!
//! Every process belongs to the enclosing generated CleanupFixture. Tests call
//! the product executor directly, never the live session's agent tools. Native
//! identity and shell-written receipts prove rejected close has no hidden effect;
//! positive controls retain real retirement and bounded completed-close reuse.

use super::*;
use mez_agent::ActionResult;

mod binding_controls;

/// Starts a synthetic parent turn without executing any provider request.
fn parent_turn(fixture: &mut CleanupFixture) -> AgentTurnRecord {
    let started = fixture
        .service
        .start_agent_prompt_turn("%1", "fixture close authority")
        .unwrap();
    fixture
        .service
        .remove_pending_agent_provider_task(&started.turn_id);
    fixture
        .service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone()
}

/// Plans and executes the actual close action against this isolated runtime.
fn close_action(
    service: &mut RuntimeSessionService,
    turn: &AgentTurnRecord,
    child: &str,
) -> ActionResult {
    let action = mez_agent::AgentAction {
        id: "fixture-persistent-close".to_string(),
        payload: mez_agent::AgentActionPayload::CloseAgent {
            agent_id: child.to_string(),
        },
    };
    let planned =
        mez_agent::plan_action_result(turn, &action, mez_agent::ActionPlanningInput::default())
            .unwrap();
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "isolated close fixture".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "verify exact close ownership".to_string(),
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
            .unwrap(),
        1
    );
    execution.action_results.remove(0)
}

/// All pre-effect inaccessible identities must retain the same opaque response.
fn denial(result: &ActionResult) -> (ActionStatus, Option<String>, Option<String>, Option<String>) {
    (
        result.status,
        result.error.as_ref().map(|e| e.code.clone()),
        result.error.as_ref().map(|e| e.message.clone()),
        result.structured_content_json.clone(),
    )
}

/// Requires a shell-generated receipt containing its own PID, not input echo.
/// The fixture explicitly requires a UTF-8 temporary path and quotes it as one
/// POSIX shell word; arbitrary apostrophes cannot masquerade as a close failure.
fn assert_input(fixture: &mut CleanupFixture, child: &str, pid: u32, marker: &str) {
    let pane = child.strip_prefix("agent-").unwrap();
    let receipt = fixture.root.join("close-input-receipt");
    let quoted_receipt = mez_agent::shell::shell_quote(
        receipt
            .to_str()
            .expect("input receipt fixture requires a UTF-8 TMPDIR"),
    );
    let quoted_marker = mez_agent::shell::shell_quote(marker);
    fixture
        .service
        .write_runtime_pane_input(
            pane,
            format!("printf '%s:%s\\n' \"$$\" {quoted_marker} >> {quoted_receipt}\n").as_bytes(),
        )
        .unwrap();
    let expected = format!("{pid}:{marker}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        fixture.service.poll_pane_outputs(4096).unwrap();
        if fs::read_to_string(&receipt)
            .unwrap_or_default()
            .lines()
            .any(|line| line == expected)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owned child did not acknowledge {expected}"
        );
        wait_for_pane_process_activity(&fixture.service, pane, Duration::from_millis(10));
    }
}

/// Real persistent C1 -> saved subagent/root C2 must revoke original-parent
/// close authority. Returning independently to the SAME saved C1 cannot mint it
/// again. PID/start, input, task, conversation, MMP and scopes survive rejection.
#[test]
fn runtime_persistent_close_rejects_independently_resumed_child() {
    for mode in ["subagent", "root", "same-id-return"] {
        let mut fixture = CleanupFixture::new();
        let child = fixture.spawn("agent-%1");
        let pane = child.strip_prefix("agent-").unwrap();
        let original = fixture.service.persistent_subagent(&child).unwrap().clone();
        let native = fixture.native_identity(&child);
        let parent = parent_turn(&mut fixture);
        let unavailable = denial(&close_action(
            &mut fixture.service,
            &parent,
            "agent-unknown-close-fixture",
        ));
        fixture.save(
            "close-independent-target",
            if mode == "root" {
                None
            } else {
                Some("agent-%1")
            },
        );
        fixture.resume(&child, "close-independent-target");
        if mode == "same-id-return" {
            // Actual spawn already saved this immutable persistent contract.
            fixture.save(&original.conversation_id, None);
            fixture.resume(&child, &original.conversation_id);
        }
        let session = fixture
            .service
            .agent_shell_store()
            .get(pane)
            .unwrap()
            .session_id
            .clone();
        let child_turn_id = if mode == "root" {
            // Ordinary root prompt admission after delegated-pane resume hits
            // the separate MMP sender-role conflict. Isolate close ownership
            // using synthetic queued root work, retaining actual saved resume,
            // native process and MMP identity. Root prompt startup is NOT
            // qualified by this fixture; subagent modes use real admission.
            let mut queued = parent.clone();
            queued.turn_id = "synthetic-replacement-root-task".to_string();
            queued.conversation_id = session.clone();
            queued.agent_id = child.clone();
            queued.pane_id = pane.to_string();
            queued.parent_turn_id = None;
            fixture
                .service
                .agent_turn_ledger_mut()
                .queue_turn(queued.clone())
                .unwrap();
            queued.turn_id
        } else {
            let started = fixture
                .service
                .start_agent_prompt_turn(pane, "replacement task must survive")
                .unwrap();
            fixture
                .service
                .remove_pending_agent_provider_task(&started.turn_id);
            started.turn_id
        };
        let task = fixture
            .service
            .agent_turn_ledger()
            .turn(&child_turn_id)
            .unwrap()
            .clone();
        let context = fixture.store.inspect(&session).unwrap();
        let shell = fixture
            .service
            .agent_shell_store()
            .get(pane)
            .unwrap()
            .clone();
        let identity_id = mez_core::ids::AgentId::opaque(child.clone()).unwrap();
        let identity = fixture
            .service
            .message_service()
            .registered_identity(&identity_id)
            .unwrap()
            .clone();
        let lineage = fixture.service.subagent_lineage(&child).cloned();
        let scopes = fixture.service.active_subagent_write_scopes_for(&child);
        let declaration = fixture.service.subagent_scope_declaration(&child);
        assert_input(&mut fixture, &child, native.0, "before-rejected-close");
        let rejected = close_action(&mut fixture.service, &parent, &child);
        assert_eq!(denial(&rejected), unavailable, "replacement mode={mode}");
        fixture.assert_native_identity(&child, native);
        assert_eq!(fixture.service.agent_shell_store().get(pane), Some(&shell));
        assert_eq!(
            fixture
                .service
                .agent_shell_store()
                .get(pane)
                .unwrap()
                .session_id,
            session
        );
        assert_eq!(
            fixture.service.agent_turn_ledger().turn(&child_turn_id),
            Some(&task)
        );
        assert_eq!(fixture.store.inspect(&session).unwrap(), context);
        assert_eq!(
            fixture
                .service
                .message_service()
                .registered_identity(&identity_id),
            Some(&identity)
        );
        assert_eq!(fixture.service.subagent_lineage(&child), lineage.as_ref());
        assert_eq!(
            fixture.service.active_subagent_write_scopes_for(&child),
            scopes
        );
        assert_eq!(
            fixture.service.subagent_scope_declaration(&child),
            declaration
        );
        assert!(fixture.service.persistent_subagent(&child).is_none());
        assert_input(&mut fixture, &child, native.0, "after-rejected-close");
        // Ordinary later removal of C2 must not launder old C1 metadata into
        // a successful original-parent completed-close tombstone.
        fixture.close(&child);
        assert!(
            fixture
                .service
                .retired_persistent_subagent(&child)
                .is_none()
        );
        assert_eq!(
            denial(&close_action(&mut fixture.service, &parent, &child)),
            unavailable
        );
    }
}

/// A preplanned/discovered active record is not executor authority: even if a
/// lower binding seam leaves it present, independently replacing the child
/// invalidates the exact native cleanup edge before any forced-close effects.
#[test]
fn runtime_persistent_close_executor_rechecks_current_child_binding() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let pane = child.strip_prefix("agent-").unwrap();
    let native = fixture.native_identity(&child);
    let original = fixture.service.persistent_subagent(&child).unwrap().clone();
    let parent = parent_turn(&mut fixture);
    fixture
        .service
        .agent_shell_store_mut()
        .bind_conversation(pane, "independent-low-level-binding", 0)
        .unwrap();
    assert_eq!(fixture.service.persistent_subagent(&child), Some(&original));
    let result = close_action(&mut fixture.service, &parent, &child);
    assert_eq!(result.status, ActionStatus::Rejected);
    assert_eq!(result.error.unwrap().code, "unavailable");
    fixture.assert_native_identity(&child, native);
    assert_eq!(
        fixture
            .service
            .agent_shell_store()
            .get(pane)
            .unwrap()
            .session_id,
        "independent-low-level-binding"
    );
    assert_eq!(
        fixture.service.persistent_subagent(&child),
        Some(&original),
        "rejection itself must not mutate replacement metadata"
    );
    assert_input(&mut fixture, &child, native.0, "executor-rejected-close");
}

/// A failed post-authority replacement must restore the exact active record,
/// cleanup capability and scopes. A later legitimate close still retires the
/// actual child and repeat close stays an idempotent successful no-op.
#[test]
fn runtime_persistent_close_failed_resume_restores_original_authority() {
    let mut fixture = CleanupFixture::new();
    let child = fixture.spawn("agent-%1");
    let pane = child.strip_prefix("agent-").unwrap();
    let original = fixture.service.persistent_subagent(&child).unwrap().clone();
    let native = fixture.native_identity(&child);
    let parent = parent_turn(&mut fixture);
    let scopes = fixture.service.active_subagent_write_scopes_for(&child);
    let declaration = fixture.service.subagent_scope_declaration(&child);
    fixture.save("close-failed-replacement", Some("agent-%1"));
    fixture
        .service
        .fail_next_agent_resume_after_authority_restore_for_tests();
    assert!(
        fixture
            .service
            .execute_agent_shell_resume_command(pane, "/resume close-failed-replacement")
            .unwrap_err()
            .message()
            .contains("post-authority restoration")
    );
    assert_eq!(fixture.service.persistent_subagent(&child), Some(&original));
    assert_eq!(
        fixture
            .service
            .agent_shell_store()
            .get(pane)
            .unwrap()
            .session_id,
        original.conversation_id
    );
    assert_eq!(
        fixture.service.active_subagent_write_scopes_for(&child),
        scopes
    );
    assert_eq!(
        fixture.service.subagent_scope_declaration(&child),
        declaration
    );
    assert!(
        fixture
            .service
            .subagent_cleanup_child_is_owned_by_turn(&child, &parent)
    );
    fixture.assert_native_identity(&child, native);
    let closed = close_action(&mut fixture.service, &parent, &child);
    assert_eq!(closed.status, ActionStatus::Succeeded);
    assert!(!fixture.present(&child));
    assert_ne!(
        mez_mux::process::process_start_token_for_pid(native.0),
        Some(native.1)
    );
    let repeated = close_action(&mut fixture.service, &parent, &child);
    assert_eq!(repeated.status, ActionStatus::Succeeded);
    assert!(
        repeated
            .structured_content_json
            .unwrap()
            .contains("\"closed\":false")
    );
}

/// Genuine same-conversation resume and sealed child-owned first-loop rebind
/// preserve exact native cleanup authority; they are not independent resume.
#[test]
fn runtime_persistent_close_preserves_same_owned_child_and_loop() {
    for mode in ["same-conversation", "--fork", "--new"] {
        let mut fixture = CleanupFixture::new();
        let child = fixture.spawn("agent-%1");
        let pane = child.strip_prefix("agent-").unwrap();
        let original = fixture.service.persistent_subagent(&child).unwrap().clone();
        let native = fixture.native_identity(&child);
        let parent = parent_turn(&mut fixture);
        if mode == "same-conversation" {
            fixture.save(&original.conversation_id, None);
            fixture.resume(&child, &original.conversation_id);
        } else {
            fixture
                .service
                .execute_agent_shell_loop_command(
                    pane,
                    &format!("/loop {mode} --limit 1 fixture-owned iteration"),
                )
                .unwrap();
            assert_eq!(fixture.service.agent_loop_state(pane).unwrap().iteration, 1);
            assert_ne!(
                fixture
                    .service
                    .agent_shell_store()
                    .get(pane)
                    .unwrap()
                    .session_id,
                original.conversation_id
            );
        }
        assert!(
            fixture
                .service
                .subagent_cleanup_child_is_owned_by_turn(&child, &parent)
        );
        assert_eq!(fixture.service.persistent_subagent(&child), Some(&original));
        assert_eq!(
            close_action(&mut fixture.service, &parent, &child).status,
            ActionStatus::Succeeded
        );
        assert!(!fixture.present(&child));
        assert_ne!(
            mez_mux::process::process_start_token_for_pid(native.0),
            Some(native.1)
        );
    }
}
