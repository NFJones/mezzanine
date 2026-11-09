//! Raw membership through actual restart, mode changes and both fork owners.
//! Source history comes from real task completion and canonical publication;
//! neither flat block imports nor synthetic ACKs supply compaction authority.

use super::*;
use mez_agent::{ReplayConversationId, ReplayEpochContext, ReplayHeadPolicy};

fn owner(service: &RuntimeSessionService, turn: &str) -> ReplayEpochContext {
    ReplayEpochContext::from_owned_context(
        ReplayConversationId::new(
            &service
                .agent_turn_ledger()
                .turn(turn)
                .unwrap()
                .conversation_id,
        )
        .unwrap(),
        service.agent_turn_contexts()[turn].clone(),
    )
    .unwrap()
}

fn stored(store: &AgentTranscriptStore, conversation: &str) -> ReplayEpochContext {
    // The checkpoint alone excludes later ordinary append receipts. The real
    // history reader qualifies those arrivals against this same epoch source.
    replay_copies::history(store, conversation)
}

fn publish_raw(service: &mut RuntimeSessionService, turn: &str) -> ReplayEpochContext {
    let record = service.agent_turn_ledger().turn(turn).unwrap().clone();
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    assert!(
        service
            .recover_agent_provider_context_limit_failure(
                &AgentId::opaque(&record.agent_id).unwrap(),
                turn,
                &error,
                1
            )
            .unwrap()
    );
    let task = service
        .pending_agent_compaction_task_for_tests(&record.pane_id)
        .unwrap()
        .clone();
    assert!(task.request.is_none(), "raw compactor request constructed");
    assert!(
        service
            .claim_agent_compaction_task(&record.pane_id, task.task_generation)
            .unwrap()
            .is_none()
    );
    assert!(!service.retains_compaction_expense_for_tests(&record.pane_id, task.task_generation));
    let receipt = replay_publication::execute_pending_publication(service);
    service.apply_replay_epoch_completion(receipt).unwrap();
    let live = owner(service, turn);
    assert!(live.head().is_none());
    assert_eq!(live.head_policy(), ReplayHeadPolicy::IntentionalOmission);
    live
}

fn published_raw() -> (RuntimeSessionService, AgentTranscriptStore, String, String) {
    let (mut service, store, turn, conversation) = replay_copies::published_source();
    raw_tail::enable_raw(&mut service);
    let archive = store.inspect(&conversation).unwrap();
    publish_raw(&mut service, &turn);
    assert_eq!(store.inspect(&conversation).unwrap(), archive);
    (service, store, turn, conversation)
}

fn finish(service: &mut RuntimeSessionService, turn: &str) {
    crate::test_support::compaction_source::complete_task(
        service,
        turn,
        "RAW_CONTINUITY_FINAL",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    service.checkpoint_agent_session_metadata().unwrap();
}

fn change_mode(service: &mut RuntimeSessionService, next: &str) {
    let previous = service.agent_compaction_mode().name();
    let mut layers = service.config_layers().to_vec();
    let authored = format!("compaction_mode={previous:?}");
    assert!(layers[0].text.contains(&authored));
    layers[0].text = layers[0]
        .text
        .replacen(&authored, &format!("compaction_mode={next:?}"), 1);
    service.replace_config_layers(layers).unwrap();
    assert_eq!(service.agent_compaction_mode().name(), next);
}

/// Each subsequent mode consumes only the latest accepted membership. Config
/// changes themselves must neither publish an epoch nor reconstruct old source.
#[test]
fn runtime_raw_tail_summary_raw_transition_never_resurrects_source() {
    let (mut service, store, turn, conversation) = published_raw();
    let first_raw = owner(&service, &turn);
    let raw_revision = first_raw.revision();
    let next = crate::test_support::compaction_source::advance_task(
        &mut service,
        &turn,
        "RAW_TRANSITION_EXACT_PROMPT",
    );
    let before = owner(&service, &next);
    let manifest = fs::read(
        store
            .root()
            .join(&conversation)
            .join("compaction-epoch.json"),
    )
    .unwrap();
    change_mode(&mut service, "summary");
    assert_eq!(owner(&service, &next).context(), before.context());
    assert_eq!(
        fs::read(
            store
                .root()
                .join(&conversation)
                .join("compaction-epoch.json")
        )
        .unwrap(),
        manifest
    );
    let record = service.agent_turn_ledger().turn(&next).unwrap().clone();
    let error = MezError::invalid_state("provider context length exceeded")
        .with_provider_failure_json(
            r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
        );
    assert!(
        service
            .recover_agent_provider_context_limit_failure(
                &AgentId::opaque(&record.agent_id).unwrap(),
                &next,
                &error,
                1
            )
            .unwrap()
    );
    let task = service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert!(task.request.is_some());
    for event in task
        .replay_source
        .as_ref()
        .unwrap()
        .frozen_plan()
        .summary_input()
    {
        assert!(
            before
                .context()
                .chronology()
                .iter()
                .any(|original| original.record() == event.record())
        );
        assert!(!event.block().content.contains("COVERED_PARENT_SOURCE"));
        assert!(
            !event
                .block()
                .content
                .contains("original model-authored fork head")
        );
    }
    complete_runtime_test_compaction(&mut service, "%1", "RAW_TRANSITION_SUMMARY");
    let receipt = replay_publication::execute_pending_publication(&mut service);
    service.apply_replay_epoch_completion(receipt).unwrap();
    let summary = owner(&service, &next);
    assert_eq!(summary.revision().get(), raw_revision.get() + 1);
    assert_eq!(summary.head_policy(), ReplayHeadPolicy::Summary);
    let summary_head = summary.head().unwrap().occurrence_id();
    let retained = summary
        .members()
        .iter()
        .map(|member| member.occurrence_id())
        .collect::<Vec<_>>();
    change_mode(&mut service, "raw_tail");
    assert_eq!(owner(&service, &next).context(), summary.context());
    let archive = store.inspect(&conversation).unwrap();
    let final_raw = publish_raw(&mut service, &next);
    assert_eq!(final_raw.revision().get(), raw_revision.get() + 2);
    assert!(
        final_raw
            .members()
            .iter()
            .all(|member| retained.contains(&member.occurrence_id()))
    );
    assert!(
        !final_raw
            .context()
            .chronology()
            .iter()
            .any(|event| event.record().occurrence_id() == summary_head)
    );
    assert!(
        final_raw
            .context()
            .blocks()
            .iter()
            .any(|block| block.content == "RAW_TRANSITION_EXACT_PROMPT")
    );
    assert_eq!(store.inspect(&conversation).unwrap(), archive);
    assert_inherited(
        &final_raw,
        &stored(&AgentTranscriptStore::new(store.root()), &conversation),
    );
}

/// Current epoch IDs remain ordered and appear once even when a new task or
/// current instructions follow them. Historic archive-only source cannot return.
fn assert_inherited(expected: &ReplayEpochContext, actual: &ReplayEpochContext) {
    assert!(actual.head().is_none());
    assert_eq!(actual.head_policy(), ReplayHeadPolicy::IntentionalOmission);
    assert_eq!(actual.revision(), expected.revision());
    let expected_ids = expected
        .members()
        .iter()
        .map(|member| member.occurrence_id())
        .collect::<Vec<_>>();
    let actual_ids = actual
        .members()
        .iter()
        .map(|member| member.occurrence_id())
        .filter(|id| expected_ids.contains(id))
        .collect::<Vec<_>>();
    assert_eq!(actual_ids, expected_ids);
    for original in expected.context().chronology() {
        let matches = actual
            .context()
            .chronology()
            .iter()
            .filter(|event| event.record().occurrence_id() == original.record().occurrence_id())
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].record(), original.record());
        assert_eq!(matches[0].block(), original.block());
    }
    for marker in [
        "COVERED_PARENT_SOURCE",
        "original model-authored fork head",
        "first model-authored fork head",
    ] {
        assert!(
            !actual
                .context()
                .blocks()
                .iter()
                .any(|block| block.content.contains(marker)),
            "resurrected source: {marker}"
        );
    }
}

/// The real repository serializes/restores daemon topology. A fresh runtime's
/// default summary policy cannot reinterpret the saved raw membership or head.
#[test]
fn runtime_raw_tail_daemon_snapshot_restore_preserves_membership() {
    let (mut service, store, turn, conversation) = published_raw();
    finish(&mut service, &turn);
    let expected = stored(&store, &conversation);
    let snapshots = SnapshotRepository::new(temp_root("raw-daemon-snapshot"));
    service
        .create_live_snapshot(&snapshots, "raw-continuity", None)
        .unwrap();
    let restored = snapshots
        .restore_session(
            "raw-continuity",
            ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        )
        .unwrap();
    let mut reader = RuntimeServiceFixture::new().build_with_session(restored.session);
    reader.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    assert_eq!(reader.agent_compaction_mode().name(), "summary");
    reader
        .restore_agent_sessions_for_restored_snapshot(false)
        .unwrap();
    assert_eq!(
        reader.agent_shell_store().get("%1").unwrap().session_id,
        conversation
    );
    let actual = execute_runtime_agent_prompt_history_work(
        reader.prepare_runtime_agent_prompt_history_work("%1"),
    )
    .unwrap()
    .replay_owner()
    .unwrap()
    .unwrap();
    assert_inherited(&expected, &actual);
    assert_eq!(actual.members(), expected.members());
}

/// `/fork` exercises its actual command worker, independent initial receipt and
/// actor adoption rather than merely calling the storage copy helper.
#[test]
fn runtime_raw_tail_saved_fork_command_keeps_headless_membership() {
    let (mut service, store, turn, conversation) = published_raw();
    finish(&mut service, &turn);
    replay_copies::native_scenario(&mut service, |service| {
        service.start_initial_pane_process(Some("cat")).unwrap();
        let expected = stored(&store, &conversation);
        let primary = service.session.layout_owner_client_id().unwrap().clone();
        service
            .execute_agent_shell_command(&primary, "/fork raw-command-child")
            .unwrap();
        let result = service
            .run_pending_deferred_agent_command_for_tests()
            .unwrap()
            .unwrap();
        assert!(result.contains("forked=true"), "{result}");
        let child = service
            .agent_shell_store()
            .sessions()
            .find(|session| session.session_id == "raw-command-child")
            .unwrap();
        let actual = execute_runtime_agent_prompt_history_work(
            service.prepare_runtime_agent_prompt_history_work(&child.pane_id),
        )
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
        assert_inherited(&expected, &actual);
        assert_eq!(actual.members(), expected.members());
        assert_inherited(
            &expected,
            &stored(
                &AgentTranscriptStore::new(store.root()),
                "raw-command-child",
            ),
        );
    });
}

/// The subagent's planned typed root and real initial persistence/bootstrap
/// preserve the raw epoch even with no head. Its new prompt is distinct source.
#[test]
fn runtime_raw_tail_subagent_fork_keeps_headless_membership() {
    let (mut service, store, turn, _conversation) = published_raw();
    let expected = owner(&service, &turn);
    replay_copies::native_scenario(&mut service, |service| {
        service.use_transcript_effect_adapter();
        service.start_initial_pane_process(Some("cat")).unwrap();
        let primary = service.session.layout_owner_client_id().unwrap().clone();
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
                    task_prompt: "RAW_CHILD_EXACT_PROMPT".into(),
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
        let conversation = service
            .agent_shell_store()
            .get(pane)
            .unwrap()
            .session_id
            .clone();
        assert_inherited(&expected, &owner(service, child_turn));
        replay_copies::admit_and_bootstrap_child(service, pane, &conversation);
        assert_inherited(&expected, &owner(service, child_turn));
        assert_inherited(
            &expected,
            &stored(&AgentTranscriptStore::new(store.root()), &conversation),
        );
    });
}
