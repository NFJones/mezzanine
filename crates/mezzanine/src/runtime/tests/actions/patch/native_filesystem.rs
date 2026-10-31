//! Actor filesystem-attempt ownership against real canonical execution slots.
//!
//! These fixtures register running results without invoking legacy shell
//! lowering. They exercise actor queue/claim/cancellation/outcome ownership,
//! not the still-pending asynchronous dispatch, commit leases or durability.

use super::*;
use crate::runtime::processes::native_patch::worker::NativePatchWorkerOutcome;
#[cfg(target_os = "linux")]
use crate::runtime::processes::native_patch::worker::{
    NativePatchWorkerRequest, execute_native_patch,
};
use mez_agent::native_action::NativeEffectCertainty;
use std::sync::Arc;

/// Exact canonical actor fixture, without a native effect producer at setup.
struct Fixture {
    /// Runtime actor state holding canonical groups and action slots.
    service: RuntimeSessionService,
    /// Original running turn, independent of later current projections.
    turn: AgentTurnRecord,
    /// Actual authored action in the original execution group.
    action: mez_agent::AgentAction,
    /// Complete original group supplied to native queue admission.
    execution: mez_agent::AgentTurnExecution,
    /// Unique private fixture subtree.
    root: PathBuf,
}

impl Fixture {
    /// Installs a real canonical pending patch slot and explicit path authority.
    /// The pane root is a fixture process; queue/claim starts no patch child.
    fn new() -> Self {
        Self::with_owned_directory(false)
    }

    /// Establishes optional owned navigation before starting the original turn.
    fn with_owned_directory(owned_directory: bool) -> Self {
        let root = temp_root("native-filesystem-attempt");
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let mut service = test_runtime_service();
        service
            .start_initial_pane_process_with_launch_context(Some("cat"), Some(&root), None)
            .unwrap();
        service.set_agent_native_shell_mode_for_tests("%1");
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        if owned_directory {
            let owned = root.join("owned");
            fs::create_dir(&owned).unwrap();
            service
                .agent_shell_store_mut()
                .ensure_session("%1")
                .unwrap()
                .record_native_working_directory(owned, 0)
                .unwrap();
        }
        let permissions = crate::runtime::config::runtime_configured_permissions_from_config(
            &serde_json::json!({"permissions":{"read_scopes":[root],"write_scopes":[root]}}),
        )
        .unwrap();
        service
            .integration
            .replace_configured_permissions(permissions);
        service.permission_policy_mut().set_approval_bypass(true);
        let started = service
            .start_agent_prompt_turn("%1", "create a native file")
            .unwrap();
        let turn = service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == started.turn_id)
            .cloned()
            .unwrap();
        service.remove_pending_agent_provider_task(&turn.turn_id);
        let action = mez_agent::AgentAction {
            id: "native-file".into(),
            payload: mez_agent::AgentActionPayload::ApplyPatch {
                patch: format!(
                    "*** Begin Patch\n*** Add File: {}\n+native data\n*** End Patch",
                    root.join("file").display()
                ),
                strip: None,
            },
        };
        let execution = mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".into(),
                model: "test".into(),
                raw_text: "native filesystem fixture".into(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: "native actor ownership".into(),
                    actions: vec![action.clone()],
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: Default::default(),
            action_results: vec![mez_agent::ActionResult::running(
                &turn,
                &action,
                Vec::new(),
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        };
        append_test_execution_assistant_context(&mut service, &turn, &execution);
        service
            .agent_turn_executions_mut()
            .insert(turn.turn_id.clone(), execution.clone());
        Self {
            service,
            turn,
            action,
            execution,
            root,
        }
    }

    /// Registers the actual original canonical slot, without shell lowering.
    fn queue(&mut self) -> crate::runtime::agent::native_filesystem::NativeFilesystemAttempt {
        self.service
            .queue_native_filesystem_action(&self.turn, &self.action, &self.execution)
            .unwrap()
    }

    /// Installs a distinct current response and presentation CID while the
    /// original turn/registered filesystem owner remains live. This isolates
    /// projection changes from actual cancellation or a replacement actor.
    fn advance_projection(&mut self) {
        let mut next = self.execution.clone();
        let action = mez_agent::AgentAction {
            id: "later-view".into(),
            payload: mez_agent::AgentActionPayload::Say {
                text: "later response".into(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                status: mez_agent::SayStatus::Progress,
            },
        };
        next.response.raw_text = "later provider response".into();
        next.response.action_batch.as_mut().unwrap().actions = vec![action.clone()];
        next.action_results = vec![mez_agent::ActionResult::running(
            &self.turn,
            &action,
            Vec::new(),
            None,
        )];
        append_test_execution_assistant_context(&mut self.service, &self.turn, &next);
        self.service
            .agent_turn_executions_mut()
            .insert(self.turn.turn_id.clone(), next);
        self.service
            .agent_shell_store_mut()
            .ensure_session("%1")
            .unwrap()
            .session_id = "later-presentation-cid".into();
    }
}
impl Drop for Fixture {
    /// Stops only the fixture root and removes its unique owned subtree.
    fn drop(&mut self) {
        let _ = self.service.terminate_all_pane_processes();
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A real admitted canonical slot can be claimed once; a duplicate claim and
/// duplicate original-slot queue cannot create another producer or transaction.
/// Physical worker integration below is conditional on supported root evidence.
#[test]
fn native_filesystem_actor_claims_exact_original_slot_once() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    assert!(
        fixture
            .service
            .queue_native_filesystem_action(&fixture.turn, &fixture.action, &fixture.execution)
            .is_err()
    );
    let dispatch = fixture
        .service
        .claim_native_filesystem_action(&attempt)
        .unwrap()
        .unwrap();
    assert_eq!(dispatch.work.owner.turn.turn_id, fixture.turn.turn_id);
    assert_eq!(dispatch.work.owner.action, fixture.action);
    assert!(
        fixture
            .service
            .claim_native_filesystem_action(&attempt)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .service
            .take_unissued_native_filesystem_owner(&attempt)
            .is_err()
    );
    assert!(!fixture.root.join("file").exists());
}

/// Matching numeric labels from another actor allocation cannot claim work or
/// read its scopes. The real original owner remains independently claimable.
#[test]
fn native_filesystem_actor_rejects_foreign_allocation_attempts() {
    let mut first = Fixture::new();
    let first_attempt = first.queue();
    let mut second = Fixture::new();
    let second_attempt = second.queue();
    assert_ne!(first_attempt, second_attempt);
    assert!(
        second
            .service
            .claim_native_filesystem_action(&first_attempt)
            .unwrap()
            .is_none()
    );
    assert!(
        second
            .service
            .claim_native_filesystem_action(&second_attempt)
            .unwrap()
            .is_some()
    );
    assert!(
        first
            .service
            .claim_native_filesystem_action(&first_attempt)
            .unwrap()
            .is_some()
    );
}

/// Changed authored payloads cannot reuse an old canonical slot at queue time.
/// Changing the current execution before claim rejects that queued owner with
/// explicit unissued failure evidence instead of silently rebasing its effects.
#[test]
fn native_filesystem_actor_rejects_changed_original_action_and_group() {
    let mut fixture = Fixture::new();
    let mut different = fixture.action.clone();
    if let mez_agent::AgentActionPayload::ApplyPatch { patch, .. } = &mut different.payload {
        patch.push('\n');
    }
    assert!(
        fixture
            .service
            .queue_native_filesystem_action(&fixture.turn, &different, &fixture.execution)
            .is_err()
    );
    let attempt = fixture.queue();
    fixture
        .service
        .agent_turn_executions_mut()
        .get_mut(&fixture.turn.turn_id)
        .unwrap()
        .response
        .action_batch
        .as_mut()
        .unwrap()
        .actions[0] = different;
    assert!(
        fixture
            .service
            .claim_native_filesystem_action(&attempt)
            .is_err()
    );
    assert!(
        fixture
            .service
            .native_filesystem_attempt_failure(&attempt)
            .is_some()
    );
    let owner = fixture
        .service
        .take_unissued_native_filesystem_owner(&attempt)
        .unwrap();
    assert_eq!(owner.owner.action, fixture.action);
    assert!(!fixture.root.join("file").exists());
}

/// The ordinary actor cancellation prefix reaches a queued filesystem fence.
/// No claim can start after that cancellation, and only the known unissued
/// metadata may transfer for refusal settlement; no worker outcome is invented.
#[test]
fn native_filesystem_actor_cancellation_prevents_unissued_work() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    fixture
        .service
        .request_agent_worker_cancellation_for_turn(&fixture.turn.turn_id);
    assert!(
        fixture
            .service
            .claim_native_filesystem_action(&attempt)
            .is_err()
    );
    assert!(
        fixture
            .service
            .take_unissued_native_filesystem_owner(&attempt)
            .is_ok()
    );
    assert!(
        fixture
            .service
            .claim_native_filesystem_action(&attempt)
            .unwrap()
            .is_none()
    );
    assert!(!fixture.root.join("file").exists());
}

/// Actual descriptor-worker results retain their original immutable allocation
/// once. Cancellation after positive completion does not erase known effects;
/// repeated delivery of the same producer object is idempotent, not replay.
#[cfg(target_os = "linux")]
#[test]
fn native_filesystem_actor_retains_actual_worker_outcome_after_cancellation() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let dispatch = fixture
        .service
        .claim_native_filesystem_action(&attempt)
        .unwrap()
        .unwrap();
    let request = NativePatchWorkerRequest {
        identity: dispatch.work.identity.clone(),
        root: dispatch.work.source.capture_root().unwrap(),
        scopes: dispatch.scopes,
        fence: dispatch.fence,
        coordinator: dispatch.coordinator,
    };
    let outcome = Arc::new(execute_native_patch(
        request,
        |identity, _, _| {
            fixture
                .service
                .native_filesystem_scope_snapshot(&attempt, identity)
        },
        |_| {},
    ));
    assert_eq!(outcome.result.certainty, NativeEffectCertainty::Confirmed);
    fixture.advance_projection();
    fixture
        .service
        .request_agent_worker_cancellation_for_turn(&fixture.turn.turn_id);
    assert!(
        fixture
            .service
            .retain_native_filesystem_outcome(&attempt, Arc::clone(&outcome))
            .unwrap()
    );
    assert!(
        !fixture
            .service
            .retain_native_filesystem_outcome(&attempt, Arc::clone(&outcome))
            .unwrap()
    );
    let retained = fixture
        .service
        .retained_native_filesystem_outcome(&attempt)
        .unwrap();
    assert!(Arc::ptr_eq(&retained, &outcome));
    assert_eq!(retained.receipts.len(), 1);
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
    assert!(
        fixture
            .service
            .take_unissued_native_filesystem_owner(&attempt)
            .is_err()
    );
}

/// A malformed worker identity cannot replace its admitted semantic owner, but
/// original raw evidence remains retained for diagnosis. Retrying that exact
/// failed delivery must preserve the first failure rather than become a pass.
#[test]
fn native_filesystem_actor_preserves_mismatched_result_and_failure() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    let dispatch = fixture
        .service
        .claim_native_filesystem_action(&attempt)
        .unwrap()
        .unwrap();
    let mut identity = dispatch.work.identity.clone();
    identity.transaction.push_str("-wrong");
    let outcome = Arc::new(NativePatchWorkerOutcome {
        result: mez_agent::native_action::NativePatchOutcome {
            identity,
            phase: mez_agent::native_action::NativePatchPhase::Settle,
            certainty: NativeEffectCertainty::Unknown,
            confirmed_effects: Vec::new(),
            diagnostic: Some("untrusted outcome identity".into()),
        },
        receipts: Vec::new(),
        completed_operations: Vec::new(),
    });
    assert!(
        fixture
            .service
            .retain_native_filesystem_outcome(&attempt, Arc::clone(&outcome))
            .is_err()
    );
    assert!(
        fixture
            .service
            .retain_native_filesystem_outcome(&attempt, Arc::clone(&outcome))
            .is_err()
    );
    assert!(Arc::ptr_eq(
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap(),
        &outcome
    ));
    assert!(
        fixture
            .service
            .native_filesystem_attempt_failure(&attempt)
            .is_some()
    );
    assert!(!fixture.root.join("file").exists());
}

/// A previously accepted queued slot retains its original canonical owner when
/// another response becomes current. Claiming must not replace its group or
/// payload with that later response, nor discard it merely due to projection.
#[test]
fn native_filesystem_actor_claims_original_queue_after_projection_changes() {
    let mut fixture = Fixture::new();
    let attempt = fixture.queue();
    fixture.advance_projection();
    let dispatch = fixture
        .service
        .claim_native_filesystem_action(&attempt)
        .unwrap()
        .unwrap();
    assert_eq!(dispatch.work.owner.action, fixture.action);
    assert_eq!(
        dispatch.work.owner.turn.conversation_id,
        fixture.turn.conversation_id
    );
}

/// An issued owner retains its frozen owned operation cwd across a later
/// presentation CID/response. Only its actual registered attempt can request
/// clipped current scopes; copied display labels never grant this authority.
#[test]
fn native_filesystem_actor_keeps_issued_source_after_projection_changes() {
    let mut fixture = Fixture::with_owned_directory(true);
    let owned = fixture.root.join("owned");
    let attempt = fixture.queue();
    let dispatch = fixture
        .service
        .claim_native_filesystem_action(&attempt)
        .unwrap()
        .unwrap();
    fixture.advance_projection();
    let scopes = fixture
        .service
        .native_filesystem_scope_snapshot(&attempt, &dispatch.work.identity)
        .unwrap();
    assert_eq!(PathBuf::from(&scopes.current_directory), owned);
    #[cfg(target_os = "linux")]
    {
        let request = NativePatchWorkerRequest {
            identity: dispatch.work.identity.clone(),
            root: dispatch.work.source.capture_root().unwrap(),
            scopes: dispatch.scopes,
            fence: dispatch.fence,
            coordinator: dispatch.coordinator,
        };
        let outcome = Arc::new(execute_native_patch(
            request,
            |identity, _, _| {
                fixture
                    .service
                    .native_filesystem_scope_snapshot(&attempt, identity)
            },
            |_| {},
        ));
        assert_eq!(outcome.result.certainty, NativeEffectCertainty::Confirmed);
        assert!(
            fixture
                .service
                .retain_native_filesystem_outcome(&attempt, Arc::clone(&outcome))
                .unwrap()
        );
        assert_eq!(
            fs::read(fixture.root.join("file")).unwrap(),
            b"native data\n"
        );
    }
}

/// Admission remains bounded by live/untransferred original owners, not by
/// growing historical result lists. A known unissued ownership transfer frees
/// its slot, while no overflow path creates a producer or mutates fixture files.
#[test]
fn native_filesystem_actor_bounds_attempts_before_producer_admission() {
    let mut fixture = Fixture::new();
    let actions = (0..5)
        .map(|index| mez_agent::AgentAction {
            id: format!("bounded-{index}"),
            payload: mez_agent::AgentActionPayload::ApplyPatch {
                patch: format!(
                    "*** Begin Patch\n*** Add File: {}\n+bounded\n*** End Patch",
                    fixture.root.join(format!("bounded-{index}")).display()
                ),
                strip: None,
            },
        })
        .collect::<Vec<_>>();
    fixture.execution.response.raw_text = "bounded actor group".into();
    fixture
        .execution
        .response
        .action_batch
        .as_mut()
        .unwrap()
        .actions = actions.clone();
    fixture.execution.action_results = actions
        .iter()
        .map(|action| mez_agent::ActionResult::running(&fixture.turn, action, Vec::new(), None))
        .collect();
    append_test_execution_assistant_context(
        &mut fixture.service,
        &fixture.turn,
        &fixture.execution,
    );
    fixture
        .service
        .agent_turn_executions_mut()
        .insert(fixture.turn.turn_id.clone(), fixture.execution.clone());
    let mut attempts = Vec::new();
    for action in &actions[..4] {
        attempts.push(
            fixture
                .service
                .queue_native_filesystem_action(&fixture.turn, action, &fixture.execution)
                .unwrap(),
        );
    }
    assert!(
        fixture
            .service
            .queue_native_filesystem_action(&fixture.turn, &actions[4], &fixture.execution)
            .is_err()
    );
    fixture
        .service
        .take_unissued_native_filesystem_owner(&attempts[0])
        .unwrap();
    assert!(
        fixture
            .service
            .queue_native_filesystem_action(&fixture.turn, &actions[4], &fixture.execution)
            .is_ok()
    );
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
}
