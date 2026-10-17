//! Genuine saved-root work, provider preparation and local MAAP message flows.
//!
//! Native child IDs and roles come from actual delegation; the only model is
//! the existing deterministic in-memory batch provider. Saved ROOT metadata is
//! derived from an actual nonauthorizing root checkpoint, never a child grant.
//! Approval decisions use the existing primary-bound path, not AutoAllow or a
//! forged planned result. No live credential, MCP or provider transport runs.

use super::*;
use crate::runtime::current_unix_seconds;
use mez_agent::messaging::{Envelope, MessageScope, ProjectMembership, Recipient};
use std::time::{Duration, Instant};

/// Installs one generated canonical startup project before native registration.
pub(super) fn project(fixture: &mut CleanupFixture, name: &str) -> PathBuf {
    let path = fixture.root.join(name);
    fs::create_dir(&path).unwrap();
    let path = path.canonicalize().unwrap();
    fixture
        .service
        .agent_shell_store_mut()
        .install_project_scope("%1", ProjectMembership::from_canonical_root(path.clone()))
        .unwrap();
    path
}

/// Establishes the actual parent pane's ordinary runtime registration/receiver.
/// No sender fields are fabricated, and this does not start a parent model call.
pub(super) fn parent_receiver(fixture: &mut CleanupFixture) -> AgentId {
    let identity = fixture
        .service
        .ensure_runtime_message_identity(
            "agent-%1",
            None,
            "agent",
            &["agent-harness"],
            current_unix_seconds().saturating_mul(1000),
        )
        .unwrap();
    fixture
        .service
        .message_service_mut()
        .subscribe_from_retained_start(&identity.agent_id)
        .unwrap();
    identity.agent_id
}

/// Delegates a real worker/explorer with no synthetic initial ledger work.
pub(super) fn spawn_role(fixture: &mut CleanupFixture, role: &str) -> String {
    let parent_conversation = fixture
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let result = fixture
        .service
        .spawn_runtime_persistent_subagent_session_owned(
            SubagentSpawnRequest {
                parent_agent_id: "agent-%1".into(),
                requested_role: role.into(),
                placement: "new-pane".into(),
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
            &parent_conversation,
            "generated c433 real native registration fixture",
        )
        .unwrap();
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    result["agent"]["id"].as_str().unwrap().to_owned()
}

/// Saves an independent conversation with a real root-shaped checkpoint and
/// generated canonical scope. The saved-session name/cache are fixture archive
/// metadata, NOT a replacement native MMP identity. All native IDs stay fixed.
pub(super) fn saved_root(
    fixture: &mut CleanupFixture,
    target: &str,
    project: &std::path::Path,
    marker: &str,
) {
    fixture.save(target, None);
    fixture
        .store
        .append(&TranscriptEntry {
            conversation_id: target.into(),
            sequence: 2,
            created_at_unix_seconds: 2,
            role: TranscriptRole::User,
            turn_id: format!("{target}-history"),
            agent_id: "generated-history".into(),
            pane_id: "%99".into(),
            content: marker.into(),
        })
        .unwrap();
    fixture.service.checkpoint_agent_session_metadata().unwrap();
    let parent_conversation = fixture
        .service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let mut record = fixture
        .store
        .load_agent_session_metadata(fixture.service.session().id.as_str())
        .unwrap()
        .into_iter()
        .find(|record| record.conversation_id == parent_conversation)
        .unwrap();
    assert!(!record.persistent_subagent_owned);
    assert!(record.steering_recovery.is_empty());
    let archive = format!("c433-saved-{target}");
    record.mezzanine_session_id = archive.clone();
    record.conversation_id = target.into();
    record.primary_display_name = None;
    record.prompt_cache_lineage_id = format!("c433-cache-{target}");
    record.running_turn_id = None;
    record.running_turn_kind = None;
    record.transcript_entries = 2;
    record.working_directory = Some(project.to_str().unwrap().into());
    record.project_root = record.working_directory.clone();
    record.allowed_actions = None;
    record.pane_model_profile = None;
    record.pane_model_profile_selection = None;
    record.pane_permission_preset_override = None;
    record.pane_approval_policy_override = None;
    record.approval_policy = None;
    fixture
        .store
        .save_agent_session_metadata(&archive, &[record])
        .unwrap();
}

/// Requires fresh shell-written evidence from the original owned PID, not echo.
/// Bounds and exact native handles match the validated persistent-close fixture.
pub(super) fn input_receipt(fixture: &mut CleanupFixture, child: &str, pid: u32, marker: &str) {
    let pane = child.strip_prefix("agent-").unwrap();
    let receipt = fixture.root.join("c433-input-receipts");
    let path = mez_agent::shell::shell_quote(receipt.to_str().expect("owned UTF-8 TMPDIR"));
    let marker_word = mez_agent::shell::shell_quote(marker);
    fixture
        .service
        .write_runtime_pane_input(
            pane,
            format!("printf '%s:%s\\n' \"$$\" {marker_word} >> {path}\n").as_bytes(),
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
            "owned native input receipt unavailable"
        );
        wait_for_pane_process_activity(&fixture.service, pane, Duration::from_millis(10));
    }
}

/// Executes unchanged real preparation only after proving every external-I/O
/// branch empty. Never overwrite a populated work item to manufacture safety.
pub(super) async fn prepare_without_io(fixture: &mut CleanupFixture, turn: &str) {
    let work = fixture.service.prepare_agent_provider_work(turn).unwrap();
    assert!(work.auth_store.is_none());
    assert!(work.mcp_plans.is_empty());
    assert_eq!(work.attempted_mcp_servers, 0);
    assert!(work.accounting_projects.is_none());
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    fixture
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
}

/// Runs ordinary MAAP from the actual admitted turn with a fixed fake provider.
/// If normal policy prompts, approve only its real pending record as the actual
/// attached primary; do not replace policy or bypass action planning.
async fn run_actions(
    fixture: &mut CleanupFixture,
    turn_id: &str,
    actions: Vec<mez_agent::AgentAction>,
) -> mez_agent::AgentTurnExecution {
    prepare_without_io(fixture, turn_id).await;
    let turn = fixture
        .service
        .agent_turn_ledger()
        .turn(turn_id)
        .unwrap()
        .clone();
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(),
            model: "test".into(),
            raw_text: "generated local c433 actions".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "exercise real local message owners".into(),
                actions,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let mut execution = fixture
        .service
        .execute_agent_turn_with_provider(
            turn_id,
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    if execution
        .action_results
        .iter()
        .any(|result| result.status == ActionStatus::Blocked)
    {
        let approvals = fixture
            .service
            .blocked_approvals()
            .pending()
            .into_iter()
            .filter(|request| {
                request.requesting_agent_id == turn.agent_id && request.pane_id == turn.pane_id
            })
            .map(|request| request.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            approvals.len(),
            1,
            "only the actual local send may need approval"
        );
        let id = &approvals[0];
        fixture
            .service
            .integration
            .blocked_approvals_mut()
            .decide_with_client_at(
                id,
                mez_agent::permissions::ApprovalDecision::Approve,
                None,
                Some(fixture.primary.to_string()),
                current_unix_seconds(),
            )
            .unwrap();
        let decided = fixture.service.blocked_approvals().get(id).unwrap().clone();
        assert_eq!(
            fixture
                .service
                .resume_approved_blocked_agent_action(id, &decided, &fixture.primary)
                .unwrap(),
            Some(1)
        );
        execution = fixture
            .service
            .agent_turn_executions()
            .get(turn_id)
            .unwrap()
            .clone();
    }
    fixture.service.remove_pending_agent_provider_task(turn_id);
    execution
}

/// Constructs only ordinary action payloads; the real turn supplies the sender.
fn send(id: &str, target: &AgentId, payload: &str) -> mez_agent::AgentAction {
    mez_agent::AgentAction {
        id: id.into(),
        payload: mez_agent::AgentActionPayload::SendMessage {
            recipient: format!("agent:{target}"),
            scope: Some("project".into()),
            content_type: "text/plain; charset=utf-8".into(),
            payload: payload.into(),
            correlation_id: None,
        },
    }
}

/// Both actual worker and explorer native instances can resume ROOT, retain
/// kernel/input identity, prepare real context, and execute normal discovery
/// then a direct-recipient send. The retained role/subagent labels cannot revive
/// structural kind, ownership, permissions or persistent-close qualification.
#[tokio::test(flavor = "current_thread")]
async fn runtime_saved_root_role_runs_real_maap_message_and_discovery() {
    for role in ["worker", "explorer"] {
        let mut fixture = Box::new(CleanupFixture::new());
        let scope = project(&mut fixture, "project-a");
        let parent = parent_receiver(&mut fixture);
        let child = spawn_role(&mut fixture, role);
        let child_id = AgentId::opaque(child.clone()).unwrap();
        let pane = child.strip_prefix("agent-").unwrap().to_owned();
        let native = fixture.native_identity(&child);
        let original = fixture
            .service
            .message_service()
            .registered_identity(&child_id)
            .unwrap()
            .clone();
        input_receipt(&mut fixture, &child, native.0, "before-root");
        let target = format!("c433-root-{role}");
        let marker = format!("C433_HISTORY_{role}");
        saved_root(&mut fixture, &target, &scope, &marker);
        fixture.resume(&child, &target);
        fixture.assert_native_identity(&child, native);
        input_receipt(&mut fixture, &child, native.0, "after-root");
        assert!(!fixture.service.has_subagent_authority_state(&child));
        assert!(fixture.service.persistent_subagent(&child).is_none());
        let select = serde_json::json!({"jsonrpc":"2.0","id":"c433-select-root",
            "method":"pane/select","params":{"target":{"pane_id":pane},
            "idempotency_key":"c433-select-root"}})
        .to_string();
        let selected = fixture
            .service
            .dispatch_runtime_control_body(&select, &fixture.primary);
        let selected: serde_json::Value = serde_json::from_str(&selected).unwrap();
        assert!(selected.get("error").is_none(), "{selected}");
        fixture
            .service
            .execute_agent_shell_control_command(
                &fixture.primary,
                "/objective generated c433 root objective",
            )
            .unwrap();
        assert_eq!(
            fixture.store.user_objective(&target).unwrap().as_deref(),
            Some("generated c433 root objective")
        );
        fixture
            .service
            .execute_agent_shell_control_command(&fixture.primary, "/objective --clear")
            .unwrap();
        assert!(fixture.store.user_objective(&target).unwrap().is_none());
        let started = fixture
            .service
            .start_agent_prompt_turn(&pane, "generated independent root action task")
            .unwrap();
        let payload = format!("C433_DIRECT_{role}");
        let execution = run_actions(
            &mut fixture,
            &started.turn_id,
            vec![
                mez_agent::AgentAction {
                    id: "discover-native-root".into(),
                    payload: mez_agent::AgentActionPayload::ListAgents {
                        agent_type: Some("all".into()),
                        scope: Some("project".into()),
                    },
                },
                send("direct-native-root", &parent, &payload),
            ],
        )
        .await;
        assert_eq!(execution.action_results.len(), 2);
        assert!(
            execution
                .action_results
                .iter()
                .all(|result| result.status == ActionStatus::Succeeded),
            "{execution:#?}"
        );
        let rows: serde_json::Value = serde_json::from_str(
            execution.action_results[0]
                .structured_content_json
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        let row = rows["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["agent_id"] == child)
            .unwrap();
        assert_eq!(row["kind"], "primary");
        assert_eq!(row["role"], role);
        assert_eq!(
            row["capabilities"],
            serde_json::to_value(&original.capabilities).unwrap()
        );
        assert_eq!(row["persistent"], false);
        assert_eq!(row["owned_by_self"], false);
        assert!(row["parent_agent_id"].is_null());
        let received = fixture
            .service
            .message_service()
            .receive_for(&parent, u64::MAX)
            .into_iter()
            .filter(|message| message.payload == payload)
            .collect::<Vec<_>>();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].sender.agent_id, child_id);
        assert_eq!(received[0].sender.role, original.role);
        assert_eq!(received[0].sender.capabilities, original.capabilities);
        let prepared = runtime_prepared_context_for_turn(&fixture.service, &started.turn_id);
        assert_eq!(
            prepared
                .blocks()
                .iter()
                .filter(|block| block.content.contains(&marker))
                .count(),
            1
        );
        fixture.assert_native_identity(&child, native);
        input_receipt(&mut fixture, &child, native.0, "after-message");
        assert!(!fixture.service.has_subagent_authority_state(&child));
        fixture.service.stop_agent_turn_for_pane(&pane).unwrap();
    }
}

/// An ordinary message from a real parent-pane turn starts a genuine local-mail
/// turn on independent ROOT C2. Its context is lower-priority mail, its role is
/// stable, and repeat delivery cannot create another turn or advance twice.
#[tokio::test(flavor = "current_thread")]
async fn runtime_saved_root_role_admits_actual_peer_message_turn_once() {
    let mut fixture = Box::new(CleanupFixture::new());
    let scope = project(&mut fixture, "project-a");
    parent_receiver(&mut fixture);
    let child = spawn_role(&mut fixture, "worker");
    let pane = child.strip_prefix("agent-").unwrap().to_owned();
    let child_id = AgentId::opaque(child.clone()).unwrap();
    let native = fixture.native_identity(&child);
    saved_root(&mut fixture, "c433-peer-root", &scope, "C433_PEER_HISTORY");
    fixture.resume(&child, "c433-peer-root");
    let parent = fixture
        .service
        .start_agent_prompt_turn("%1", "generated local peer delivery")
        .unwrap();
    let execution = run_actions(
        &mut fixture,
        &parent.turn_id,
        vec![send("peer-root-once", &child_id, "C433_PEER_PAYLOAD")],
    )
    .await;
    assert_eq!(execution.action_results[0].status, ActionStatus::Succeeded);
    let now = current_unix_seconds().saturating_mul(1000);
    fixture
        .service
        .deliver_pending_runtime_agent_messages(now)
        .unwrap();
    let peer = fixture
        .service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| {
            turn.agent_id == child
                && turn.conversation_id == "c433-peer-root"
                && turn.trigger == mez_agent::AgentTurnTrigger::LocalMessage
        })
        .unwrap()
        .clone();
    assert!(peer.parent_turn_id.is_none());
    let context = fixture
        .service
        .agent_turn_contexts()
        .get(&peer.turn_id)
        .unwrap();
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.content.contains("C433_PEER_PAYLOAD"))
            .count(),
        1
    );
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.source == mez_agent::ContextSourceKind::PeerMessage)
    );
    let cursor = fixture
        .service
        .message_service()
        .subscription(&child_id)
        .unwrap()
        .clone();
    assert!(cursor.last_sequence > 0);
    fixture
        .service
        .deliver_pending_runtime_agent_messages(now)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .agent_turn_ledger()
            .turns()
            .iter()
            .filter(|turn| turn.agent_id == child
                && turn.conversation_id == "c433-peer-root"
                && turn.trigger == mez_agent::AgentTurnTrigger::LocalMessage)
            .count(),
        1
    );
    assert_eq!(
        fixture.service.message_service().subscription(&child_id),
        Some(&cursor)
    );
    prepare_without_io(&mut fixture, &peer.turn_id).await;
    fixture.assert_native_identity(&child, native);
    assert!(!fixture.service.has_subagent_authority_state(&child));
    fixture.service.stop_agent_turn_for_pane(&pane).unwrap();
    fixture.service.stop_agent_turn_for_pane("%1").unwrap();
}

/// Populated accepted mail keeps its exact original envelope, frozen project
/// audience and receipt while actual saved ROOT resume changes canonical scope.
/// The new context cannot consume old-project mail or reset its existing cursor.
#[test]
fn runtime_saved_root_role_keeps_frozen_mail_and_cursor_across_project_resume() {
    let mut fixture = CleanupFixture::new();
    let _a = project(&mut fixture, "project-a");
    let parent = parent_receiver(&mut fixture);
    let child = spawn_role(&mut fixture, "worker");
    let child_id = AgentId::opaque(child.clone()).unwrap();
    let pane = child.strip_prefix("agent-").unwrap().to_owned();
    let b = fixture.root.join("project-b");
    fs::create_dir(&b).unwrap();
    let b = b.canonicalize().unwrap();
    saved_root(
        &mut fixture,
        "c433-project-root",
        &b,
        "C433_PROJECT_B_HISTORY",
    );
    let parent_turn = fixture
        .service
        .start_agent_prompt_turn("%1", "generated frozen audience author")
        .unwrap();
    let parent_turn = fixture
        .service
        .agent_turn_ledger()
        .turn(&parent_turn.turn_id)
        .unwrap()
        .clone();
    let sender = fixture
        .service
        .runtime_message_sender_identity(&parent_turn)
        .unwrap();
    fixture
        .service
        .message_service_mut()
        .subscribe_from_retained_start(&child_id)
        .unwrap();
    let envelope = Envelope {
        protocol: "mmp/1",
        id: "c433-frozen-before-root".into(),
        message_type: "send".into(),
        time: "runtime:1".into(),
        sender,
        recipient: Recipient::Agent(child_id.clone()),
        correlation_id: Some(parent_turn.turn_id.clone()),
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".into(),
        payload: "C433_OLD_PROJECT_A_MAIL".into(),
        extension_fields: Vec::new(),
    };
    fixture
        .service
        .message_service_mut()
        .accept_at_with_scope(&parent, envelope, MessageScope::Project, 1)
        .unwrap();
    let before = fixture.service.message_service().snapshot_state();
    assert_eq!(before.accepted_messages.len(), 1);
    assert_eq!(before.retained_messages.len(), 1);
    let cursor = fixture
        .service
        .message_service()
        .subscription(&child_id)
        .unwrap()
        .clone();
    fixture.resume(&child, "c433-project-root");
    let expected = ProjectMembership::from_canonical_root(b).scope_id();
    assert_eq!(
        fixture
            .service
            .message_service()
            .registered_identity(&child_id)
            .unwrap()
            .project_scope,
        Some(expected)
    );
    let started = fixture
        .service
        .start_agent_prompt_turn(&pane, "generated project B task")
        .unwrap();
    let context = fixture
        .service
        .agent_turn_contexts()
        .get(&started.turn_id)
        .unwrap();
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains("C433_PROJECT_B_HISTORY"))
    );
    assert!(
        !context
            .blocks()
            .iter()
            .any(|block| block.content.contains("C433_OLD_PROJECT_A_MAIL"))
    );
    let after = fixture.service.message_service().snapshot_state();
    assert_eq!(after.retained_messages, before.retained_messages);
    assert_eq!(after.accepted_messages, before.accepted_messages);
    assert_eq!(after.next_sequence, before.next_sequence);
    assert_eq!(
        fixture.service.message_service().subscription(&child_id),
        Some(&cursor)
    );
    assert!(!fixture.service.has_subagent_authority_state(&child));
    fixture.service.stop_agent_turn_for_pane(&pane).unwrap();
    fixture.service.stop_agent_turn_for_pane("%1").unwrap();
}
