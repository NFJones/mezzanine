//! Private inference stages cover one frozen canonical prefix and publish once.
//! Real completed tasks provide source; controlled provider rejections split
//! only temporary input. Exact current instructions and later peer occurrences
//! retain their original custody through inference, physical write and adoption.

use super::*;
use std::sync::Arc;

struct StagedFixture {
    service: RuntimeSessionService,
    store: AgentTranscriptStore,
    turn: String,
    original: crate::runtime::RuntimeAgentCompactionTask,
    selected: Vec<mez_agent::ConversationEvent>,
    tail: Vec<mez_agent::ConversationEvent>,
    archive: Vec<mez_agent::transcript::TranscriptEntry>,
    baseline: Option<Vec<mez_agent::ConversationEvent>>,
    requests: Vec<mez_agent::ModelRequest>,
}

impl StagedFixture {
    fn new(prompts: &[String], retention: u8) -> Self {
        Self::with_legacy(prompts, retention, None)
    }

    fn with_legacy(prompts: &[String], retention: u8, prefix: Option<bool>) -> Self {
        let mut service = test_runtime_service();
        let store = AgentTranscriptStore::new(temp_root("canonical-observed-stages"));
        service.set_agent_transcript_store(store.clone());
        service
            .replace_config_layers(vec![ConfigLayer {
                name: "canonical-observed-stages".into(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: format!(
                    r#"[agents]
default_provider="runtime-batch"
default_model_profile="stages"
shell_mode="pane"
compaction_raw_retention_percent={retention}
[providers.runtime-batch]
kind="openai"
models=["test"]
default_model="test"
[model_profiles.stages]
provider="runtime-batch"
model="test"
context_window_tokens=40000
max_input_tokens=40000
"#
                ),
            }])
            .unwrap();
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        if let Some(prefix) = prefix {
            let conversation = service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .session_id
                .clone();
            // Literal reference history supplies no inferred execution/task
            // completion. The optional genuine prefix format is imported by
            // ordinary prompt admission before this operation is frozen.
            store
                .append(&mez_agent::TranscriptEntry {
                    conversation_id: conversation.clone(),
                    sequence: 1,
                    created_at_unix_seconds: 1,
                    role: mez_agent::TranscriptRole::User,
                    turn_id: "legacy-reference".into(),
                    agent_id: "agent-%1".into(),
                    pane_id: "%1".into(),
                    content: "LEGACY_REFERENCE_SOURCE ".repeat(80),
                })
                .unwrap();
            service
                .agent_shell_store_mut()
                .record_transcript_entries("%1", 1)
                .unwrap();
            if prefix {
                store
                    .save_compaction_epoch(&conversation, 0, "prior authoritative summary")
                    .unwrap();
            }
        }
        for prompt in prompts {
            let start = service
                .execute_agent_shell_command(&primary, prompt)
                .unwrap();
            assert!(start.contains(r#""state":"running""#), "{start}");
            let turn = service
                .agent_shell_store()
                .get("%1")
                .unwrap()
                .running_turn_id
                .clone()
                .unwrap();
            let mut response = runtime_say_response(&turn, "original completed answer", true);
            response.action_batch.as_mut().unwrap().actions[0].id = "action-1".into();
            // Native text has no call identity to fabricate or replay. Real
            // completed responses supply equal source/label/content in their
            // independently owned groups, retaining different original IDs.
            response.provider_transcript_events.push(
                mez_agent::ProviderTranscriptEvent::validated_openai_response_output(vec![
                    serde_json::json!({"type":"message", "role":"assistant", "content":[
                        {"type":"output_text", "text":"identical original native response"}
                    ]}),
                ])
                .unwrap(),
            );
            let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
            let execution = service
                .execute_agent_turn_with_provider(
                    &turn,
                    &RuntimeBatchProvider { response },
                    profile,
                )
                .unwrap();
            assert_eq!(execution.terminal_state, AgentTurnState::Completed);
        }
        let start = service
            .execute_agent_shell_command(
                &primary,
                &format!("CURRENT_USER_PROMPT {}", "protected input ".repeat(200)),
            )
            .unwrap();
        assert!(start.contains(r#""state":"running""#), "{start}");
        let turn = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id
            .clone()
            .unwrap();
        for index in 1..5 {
            service
                .agent_turn_contexts_mut()
                .get_mut(&turn)
                .unwrap()
                .append_user_event(
                    format!("steering {index}"),
                    format!("EXACT_STEERING_{index}"),
                )
                .unwrap();
        }
        let mut response = runtime_say_response(&turn, "continue", false);
        response.action_batch.as_mut().unwrap().actions[0].id = "action-1".into();
        response.usage.input_tokens = 40_000;
        response.usage.output_tokens = 1;
        let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
        service
            .execute_agent_turn_with_provider(&turn, &RuntimeBatchProvider { response }, profile)
            .unwrap();
        let original = service
            .pending_agent_compaction_task_for_tests("%1")
            .expect("actual observed usage must queue canonical work")
            .clone();
        assert!(original.replay_source.is_some());
        let conversation = &original.conversation_id;
        let owner = mez_agent::ReplayEpochContext::from_owned_context(
            mez_agent::ReplayConversationId::new(conversation).unwrap(),
            service.agent_turn_contexts()[&turn].clone(),
        )
        .unwrap();
        let plan = match owner
            .prepare_compaction(mez_agent::ReplayTailPolicy::new(40_000, retention).unwrap())
            .unwrap()
        {
            mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
            other => panic!("actual completed source must be eligible: {other:?}"),
        };
        let selected = plan.summary_input().to_vec();
        let tail = plan.retained_tail().to_vec();
        assert!(!selected.is_empty());
        assert!(!selected.iter().any(
            |event| event.block().content.contains("CURRENT_USER_PROMPT")
                || event.block().content.contains("EXACT_STEERING_")
        ));
        let archive = store.inspect(conversation).unwrap();
        let baseline = store
            .load_replay_epoch(conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .map(|(_, owner)| owner.context().chronology().to_vec());
        Self {
            service,
            store,
            turn,
            original,
            selected,
            tail,
            archive,
            baseline,
            requests: Vec::new(),
        }
    }

    fn task(&self) -> &crate::runtime::RuntimeAgentCompactionTask {
        self.service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
    }

    fn assert_unpublished(&mut self) {
        assert!(
            !self
                .task()
                .request
                .as_ref()
                .unwrap()
                .messages
                .iter()
                .any(|message| message.content.contains("within 0 ")),
            "eligible source cannot inherit zero-output guidance"
        );
        assert_eq!(
            self.store
                .load_replay_epoch(
                    &self.original.conversation_id,
                    mez_agent::AgentContext::empty()
                )
                .unwrap()
                .map(|(_, owner)| owner.context().chronology().to_vec()),
            self.baseline
        );
        assert!(self.service.take_replay_epoch_attempt_for_tests().is_none());
        // Peer delivery may retain a wake intent. Actual dispatch, including a
        // direct claim, must remain fenced until accepted publication.
        assert!(self.service.pending_agent_provider_tasks().is_empty());
        assert!(
            self.service
                .claim_configured_agent_provider_task(
                    &AgentId::opaque("agent-%1").unwrap(),
                    &self.turn,
                )
                .unwrap()
                .is_none()
        );
        assert!(
            self.store
                .inspect(&self.original.conversation_id)
                .unwrap()
                .starts_with(&self.archive)
        );
        assert!(Arc::ptr_eq(
            self.task().replay_source.as_ref().unwrap(),
            self.original.replay_source.as_ref().unwrap()
        ));
    }

    /// A real provider result rejects admitted work through the common port.
    /// The next actor generation retains the original source and private graph.
    fn reject(&mut self) {
        let requests = run_runtime_test_compaction_wave(
            &mut self.service,
            "%1",
            Err(MezError::invalid_state("provider context length exceeded")
                .with_provider_failure_json(
                    r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
                )),
        );
        assert!(!requests.is_empty());
        self.requests.extend(requests);
        self.assert_unpublished();
    }

    fn temporary(&mut self, summary: &str) {
        let before = self.service.agent_turn_contexts()[&self.turn].clone();
        self.requests.extend(run_runtime_test_compaction_wave(
            &mut self.service,
            "%1",
            Ok(runtime_test_compaction_response(summary)),
        ));
        assert_eq!(self.service.agent_turn_contexts()[&self.turn], before);
        self.assert_unpublished();
    }

    fn publish(&mut self, summary: &str) {
        let before = self.service.agent_turn_contexts()[&self.turn].clone();
        for _ in 0..128 {
            if self.service.pending_agent_compaction_tasks().is_empty() {
                break;
            }
            self.requests.extend(run_runtime_test_compaction_wave(
                &mut self.service,
                "%1",
                Ok(runtime_test_compaction_response(summary)),
            ));
        }
        assert!(
            self.service.pending_agent_compaction_tasks().is_empty(),
            "finite source did not finish"
        );
        assert_eq!(self.service.agent_turn_contexts()[&self.turn], before);
        replay_observed::adopt(&mut self.service, &self.turn);
        replay_observed::assert_head(&self.service, &self.store, &self.turn, summary);
        let context = &self.service.agent_turn_contexts()[&self.turn];
        assert_eq!(&context.chronology()[1..], self.tail);
        for covered in &self.selected {
            assert!(!context.chronology().iter().any(|event| event.record().occurrence_id() == covered.record().occurrence_id()));
        }
        assert!(!self.service.agent_is_compacting("%1"));
        assert!(self.service.agent_provider_task_is_pending(&self.turn));
        assert!(
            self.store
                .inspect(&self.original.conversation_id)
                .unwrap()
                .starts_with(&self.archive)
        );
    }
}

fn prompts() -> Vec<String> {
    vec![
        "FIRST_RANGE_SOURCE ".repeat(80),
        "SECOND_RANGE_SOURCE ".repeat(80),
    ]
}

/// Legacy raw references and prefix heads use their original import contract,
/// not inferred assistant completion. Temporary outputs stay private; late
/// live steering and an independently committed original task remain raw.
pub(super) fn legacy_source_survives_publication(prefix: bool) {
    let mut fixture = StagedFixture::with_legacy(&prompts(), 1, Some(prefix));
    assert!(
        fixture
            .selected
            .iter()
            .any(|event| event.block().content.contains("LEGACY_REFERENCE_SOURCE"))
    );
    assert_eq!(
        fixture
            .selected
            .iter()
            .any(|event| event.block().content == "prior authoritative summary"),
        prefix
    );
    fixture.reject();
    fixture.temporary("first private summary");
    let body = fixture
        .service
        .agent_turn_contexts_mut()
        .get_mut(&fixture.turn)
        .unwrap();
    let start = body.chronology().len();
    body.append_user_event("late steering", "EXACT_MIXED_LATE_STEERING")
        .unwrap();
    fixture.tail.extend_from_slice(&body.chronology()[start..]);
    let external = observed_history::append_external_task(
        &fixture.service,
        &fixture.store,
        &fixture.turn,
        "EXACT_EXTERNAL_LATE_USER",
    );
    // The immutable manifest covers frozen live members. External archive
    // arrivals are joined by a fresh ordinary history read, not by inference.
    fixture.publish("one combined authoritative head");
    let mut fresh = test_runtime_service();
    fresh.set_agent_transcript_store(AgentTranscriptStore::new(fixture.store.root()));
    fresh.agent_shell_store_mut().enter_or_resume("%1").unwrap();
    fresh
        .agent_shell_store_mut()
        .bind_conversation("%1", &fixture.original.conversation_id, 0)
        .unwrap();
    let cold = fresh
        .runtime_agent_history_epoch_context("%1")
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
    assert_eq!(cold.context().chronology().last().unwrap(), &external);
    assert_eq!(
        cold.context()
            .chronology()
            .iter()
            .filter(|event| event.record() == external.record())
            .count(),
        1
    );
    assert_eq!(
        cold.context()
            .chronology()
            .iter()
            .filter(|event| event.block().source == ContextSourceKind::CompactionPrefix)
            .count(),
        1
    );
    assert!(
        cold.context()
            .chronology()
            .iter()
            .any(|event| event.block().content == "EXACT_MIXED_LATE_STEERING")
    );
    assert!(!cold.context().blocks().iter().any(|block| {
        [
            "first private summary",
            "second private summary",
            "prior authoritative summary",
        ]
        .contains(&block.content.as_str())
    }));
}

/// A competing historical authority is installed only after actual temporary
/// inference. The original writer must reject it without overwriting that
/// authority, adopting temporary text or releasing an ordinary continuation.
pub(super) fn changed_epoch_rejects_publication() {
    let mut fixture = StagedFixture::new(&prompts(), 1);
    let original = fixture.service.agent_turn_contexts()[&fixture.turn].clone();
    fixture.reject();
    fixture.temporary("first private summary");
    fixture
        .store
        .install_legacy_compaction_ranges_fixture(
            &fixture.original.conversation_id,
            0,
            "externally changed epoch",
            Vec::new(),
        )
        .unwrap();
    let changed = fixture
        .store
        .compaction_epoch(&fixture.original.conversation_id)
        .unwrap();
    complete_runtime_test_compaction(&mut fixture.service, "%1", "stale combined summary");
    assert_eq!(
        fixture.service.agent_turn_contexts()[&fixture.turn],
        original
    );
    let completion = fixture
        .service
        .take_replay_epoch_attempt_for_tests()
        .unwrap()
        .execute();
    assert!(
        completion.error().is_some(),
        "changed authority must reject the original writer"
    );
    assert!(
        fixture
            .service
            .apply_replay_epoch_completion(completion)
            .unwrap()
            .applied
    );
    assert_eq!(
        fixture
            .store
            .compaction_epoch(&fixture.original.conversation_id)
            .unwrap(),
        changed
    );
    assert!(
        !fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn)
    );
    assert!(!fixture.service.agent_is_compacting("%1"));
    assert_eq!(
        fixture
            .service
            .agent_turn_ledger()
            .turn(&fixture.turn)
            .unwrap()
            .state,
        AgentTurnState::Failed
    );
    assert!(
        fixture
            .store
            .inspect(&fixture.original.conversation_id)
            .unwrap()
            .starts_with(&fixture.archive)
    );
}

/// The original live owner admits later pending work, but its current task
/// root remains exact. Only the earlier frozen prefix can be summarized;
/// publication durably backs the live tail without pretending ordinary append.
pub(super) fn pending_live_source_survives_publication() {
    let mut fixture = StagedFixture::new(&prompts(), 1);
    fixture.reject();
    fixture.temporary("first private summary");
    let body = fixture
        .service
        .agent_turn_contexts_mut()
        .get_mut(&fixture.turn)
        .unwrap();
    let start = body.chronology().len();
    body.append_user_event("steering", "Keep this instruction exact")
        .unwrap();
    let group = mez_agent::ContextExecutionGroupId::new("live-closed-group").unwrap();
    body.append_assistant_event(
        "live decision",
        "LIVE_RANGE_SOURCE ".repeat(1_200),
        group.clone(),
    )
    .unwrap();
    body.append_evidence_event(
        ContextSourceKind::ActionResult,
        "live result",
        "settled",
        group.clone(),
        None,
        true,
    )
    .unwrap();
    assert!(
        body.execution_group_is_pending(&group),
        "a result-shaped block is not completion authority"
    );
    let arrivals = body.chronology()[start..].to_vec();
    fixture.tail.extend(arrivals.clone());
    assert_eq!(
        fixture
            .store
            .inspect(&fixture.original.conversation_id)
            .unwrap(),
        fixture.archive
    );
    assert!(
        !fixture
            .task()
            .request
            .as_ref()
            .unwrap()
            .messages
            .iter()
            .any(|message| message.content.contains("LIVE_RANGE_SOURCE"))
    );
    fixture.publish("one head over the original prefix");
    let body = &fixture.service.agent_turn_contexts()[&fixture.turn];
    assert!(body.chronology().ends_with(&arrivals));
    assert!(
        body.chronology()
            .iter()
            .any(|event| event.block().content == "Keep this instruction exact")
    );
    assert_eq!(
        fixture
            .store
            .inspect(&fixture.original.conversation_id)
            .unwrap(),
        fixture.archive
    );
}

/// A provider refusal after accepted private siblings leaves all original
/// authority and late exact instructions intact, without publishing a partial head.
pub(super) fn second_range_failure_keeps_original_epoch() {
    let mut fixture = StagedFixture::new(&prompts(), 1);
    fixture.reject();
    fixture.temporary("first private summary");
    let steering = "EXACT_OVERSIZED_STAGED_STEERING ".repeat(8000);
    fixture
        .service
        .agent_turn_contexts_mut()
        .get_mut(&fixture.turn)
        .unwrap()
        .append_user_event("late exact steering", steering.clone())
        .unwrap();
    run_runtime_test_compaction_wave(
        &mut fixture.service,
        "%1",
        Err(MezError::new(
            crate::error::MezErrorKind::Forbidden,
            "private synthesis refused",
        )),
    );
    assert!(!fixture.service.agent_is_compacting("%1"));
    assert!(
        !fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn)
    );
    assert!(
        fixture
            .service
            .take_replay_epoch_attempt_for_tests()
            .is_none()
    );
    assert!(
        fixture
            .store
            .load_replay_epoch(
                &fixture.original.conversation_id,
                mez_agent::AgentContext::empty()
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture
            .service
            .agent_turn_ledger()
            .turn(&fixture.turn)
            .unwrap()
            .state,
        AgentTurnState::Failed
    );
    let rows = fixture
        .store
        .inspect(&fixture.original.conversation_id)
        .unwrap();
    assert!(rows.starts_with(&fixture.archive));
    assert!(
        rows.iter()
            .any(|row| row.content.contains("SECOND_RANGE_SOURCE"))
    );
    assert!(rows.iter().any(|row| row.content.contains(&steering)));
}

/// More than 32 actual model requests complete a real large source. No
/// synthetic response counter or arbitrary semantic retry ceiling is injected.
#[test]
fn runtime_compaction_completes_source_requiring_more_than_32_actual_calls() {
    let mut fixture = StagedFixture::new(&["finite source evidence ".repeat(300_000)], 1);
    fixture.publish("one accepted head covering the entire finite source");
    assert!(
        fixture.requests.len() > 32,
        "actual calls={}",
        fixture.requests.len()
    );
}

/// Equal historical text from different tasks remains two canonical occurrences
/// and reaches the model twice, without source-kind/text deduplication.
#[test]
fn runtime_compaction_accepts_equal_content_sibling_chunks() {
    let text = "REPEATED_SOURCE ".repeat(80);
    let mut fixture = StagedFixture::new(&[text.clone(), text.clone()], 1);
    let equal = fixture
        .selected
        .iter()
        .filter(|event| event.block().content == text)
        .collect::<Vec<_>>();
    assert_eq!(equal.len(), 2);
    assert_ne!(
        equal[0].record().occurrence_id(),
        equal[1].record().occurrence_id()
    );
    fixture.publish("one head covering both original equal occurrences");
    assert_eq!(fixture.requests.len(), 1);
    assert_eq!(
        fixture.requests[0]
            .messages
            .last()
            .unwrap()
            .content
            .matches("REPEATED_SOURCE")
            .count(),
        160
    );
}

/// A fitting source accepts a usable long/nonreducing final in one inference;
/// it never asks the model to optimize a successful summary's length.
#[test]
fn runtime_compaction_accepts_nonreducing_active_final_once() {
    let mut fixture = StagedFixture::new(&prompts(), 1);
    let summary = format!("{}END", "usable continuation ".repeat(5000));
    fixture.publish(&summary);
    assert_eq!(fixture.requests.len(), 1);
}

/// Equal native projections in genuinely different completed execution groups
/// remain separate source occurrences. Neither selection nor private staging
/// may deduplicate them by source kind, label or content.
#[test]
fn runtime_observed_compaction_stages_repeated_content_by_group_identity() {
    let mut fixture = StagedFixture::new(&vec!["REPEATED_RANGE_SOURCE ".repeat(80); 2], 1);
    let assistants = fixture
        .selected
        .iter()
        .filter(|event| event.block().source == ContextSourceKind::TranscriptTool)
        .collect::<Vec<_>>();
    assert_eq!(assistants.len(), 2);
    assert_eq!(assistants[0].block(), assistants[1].block());
    assert_ne!(
        assistants[0].execution_group_id(),
        assistants[1].execution_group_id()
    );
    assert_ne!(
        assistants[0].record().occurrence_id(),
        assistants[1].record().occurrence_id()
    );
    fixture.reject();
    fixture.temporary("first summary");
    fixture.publish("one summary covering both original occurrences");
}

/// A raw reservation that fits all closed history still selects its oldest
/// eligible unit. A fitting candidate publishes that frozen selection once;
/// later closed history stays in the exact tail rather than staging more input.
#[test]
fn runtime_observed_compaction_fits_before_staging_another_range() {
    let mut fixture = StagedFixture::new(&prompts(), 90);
    assert!(
        fixture
            .selected
            .iter()
            .any(|event| event.block().content.contains("FIRST_RANGE_SOURCE"))
    );
    assert!(
        !fixture
            .selected
            .iter()
            .any(|event| event.block().content.contains("SECOND_RANGE_SOURCE"))
    );
    assert!(
        fixture
            .tail
            .iter()
            .any(|event| event.block().content.contains("SECOND_RANGE_SOURCE"))
    );
    fixture.publish("short first summary");
    assert!(fixture.service.pending_agent_compaction_tasks().is_empty());
}

#[test]
fn runtime_staged_compaction_preserves_late_peer_occurrences() {
    let mut fixture = StagedFixture::new(&prompts(), 1);
    fixture.reject();
    fixture.temporary("first summary");
    let now_ms = current_unix_seconds().saturating_mul(1000);
    let sender = fixture
        .service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    let recipient = AgentId::opaque("agent-%1").unwrap();
    let start = fixture.service.agent_turn_contexts()[&fixture.turn]
        .chronology()
        .len();
    let mut last_sequence = 0;
    for (index, message_type) in ["send", "task_result", "send"].into_iter().enumerate() {
        let bridge = message_type == "task_result";
        let envelope = Envelope {
            protocol: "mmp/1", id: format!("late-mail-{index}"), message_type: message_type.into(),
            time: format!("runtime:{now_ms}"), sender: sender.clone(),
            recipient: mez_agent::messaging::Recipient::Agent(recipient.clone()),
            correlation_id: Some(fixture.turn.clone()), ttl_ms: None,
            content_type: if bridge { "application/json" } else { "text/plain; charset=utf-8" }.into(),
            payload: if bridge { r#"{"task_id":"synthetic-child","success":true,"summary":"bridge result","output":"SYNTHETIC_LATE_MAIL"}"# } else { "SYNTHETIC_LATE_MAIL" }.into(),
            extension_fields: if bridge { crate::runtime::control::runtime_bridge_extension_fields() } else { Vec::new() },
        };
        last_sequence = fixture
            .service
            .control
            .message_service_mut()
            .accept_at_with_scope(&sender.agent_id, envelope, MessageScope::Session, now_ms)
            .unwrap()
            .sequence;
        assert_eq!(
            fixture
                .service
                .deliver_pending_runtime_agent_messages(now_ms)
                .unwrap(),
            1
        );
        assert_eq!(
            fixture
                .service
                .deliver_pending_runtime_agent_messages(now_ms)
                .unwrap(),
            0
        );
        if index == 0 {
            fixture
                .service
                .agent_turn_contexts_mut()
                .get_mut(&fixture.turn)
                .unwrap()
                .append_user_event("steering", "EXACT_LATE_STEERING")
                .unwrap();
        }
    }
    let suffix =
        fixture.service.agent_turn_contexts()[&fixture.turn].chronology()[start..].to_vec();
    assert_eq!(suffix.len(), 4);
    assert_eq!(
        suffix[0].semantic_kind(),
        mez_agent::ContextSemanticKind::ReferenceEvent
    );
    assert_eq!(
        suffix[0].retention(),
        mez_agent::ContextRetention::Summarizable
    );
    assert_eq!(
        suffix[0].block().trust_domain(),
        mez_agent::TrustDomain::WebContent
    );
    assert_ne!(
        suffix[0].record().occurrence_id(),
        suffix[3].record().occurrence_id()
    );
    fixture.tail.extend_from_slice(&suffix);
    fixture.temporary("");
    assert_eq!(
        &fixture.service.agent_turn_contexts()[&fixture.turn].chronology()[start..],
        suffix
    );
    fixture.publish("one head preserving original peer arrivals");
    assert_eq!(
        fixture
            .service
            .control
            .message_service()
            .subscription(&recipient)
            .unwrap()
            .last_sequence,
        last_sequence
    );
    assert_eq!(
        fixture
            .service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0
    );
    let context = &fixture.service.agent_turn_contexts()[&fixture.turn];
    assert_eq!(
        &context.chronology()[context.chronology().len() - 4..],
        suffix
    );
}

/// A zero-sized legacy advisory is not source admission. Canonical capture
/// independently selects its real closed prefix and publishes one fitting result.
#[test]
fn runtime_observed_zero_budget_cannot_veto_canonical_source() {
    let mut fixture = StagedFixture::new(&prompts(), 1);
    let trigger = fixture
        .original
        .replay_source
        .as_ref()
        .unwrap()
        .trigger()
        .unwrap();
    let context = &fixture.service.agent_turn_contexts()[&fixture.turn];
    let advisory =
        mez_agent::plan_model_context_compaction_at_consumed_sequence(context, 40_000, 0, 0)
            .unwrap();
    assert_eq!(advisory.summary_budget_words(), 0);
    assert!(!advisory.changes_context());
    fixture.service.fail_current_agent_compaction_task("%1");
    assert!(
        fixture
            .service
            .queue_agent_active_turn_compaction(
                &fixture.turn,
                fixture.original.model_profile_name.clone(),
                fixture.original.model_profile.clone(),
                trigger
            )
            .unwrap()
    );
    fixture.original = fixture.task().clone();
    fixture.publish("one canonical head independent of the old advisory");
    assert_eq!(fixture.requests.len(), 1);
}

/// Five fitting closed tasks use one frozen input and one final publication.
/// Exact active steering is retained outside source and survives unchanged.
#[test]
fn runtime_context_recovery_covers_five_closed_tasks_in_one_fitting_request() {
    let history = (0..5)
        .map(|index| format!("decision-{index} outcome-{index} ").repeat(80))
        .collect::<Vec<_>>();
    let mut fixture = StagedFixture::new(&history, 1);
    fixture.publish("one summary covering all five completed tasks");
    assert_eq!(fixture.requests.len(), 1);
    let input = &fixture.requests[0].messages.last().unwrap().content;
    for index in 0..5 {
        assert!(input.contains(&format!("decision-{index}")));
    }
    let context = &fixture.service.agent_turn_contexts()[&fixture.turn];
    for index in 1..5 {
        assert!(!input.contains(&format!("EXACT_STEERING_{index}")));
        assert!(
            context
                .blocks()
                .iter()
                .any(|block| block.content == format!("EXACT_STEERING_{index}"))
        );
    }
}

/// First manual publication from genuinely accounted saved raw history must
/// retain historical user-root semantics. No existing head or synthetic source
/// snapshot supplies custody, and the command must not start an ordinary turn.
#[test]
fn runtime_manual_compaction_first_head_keeps_completed_task_roots_historical() {
    let mut fixture = StagedFixture::new(&prompts(), 1);
    fixture.service.fail_current_agent_compaction_task("%1");
    crate::test_support::compaction_source::complete_task(
        &mut fixture.service,
        &fixture.turn,
        "manual source task completed",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    let archive = fixture
        .store
        .inspect(&fixture.original.conversation_id)
        .unwrap();
    assert!(
        fixture
            .store
            .load_replay_epoch(
                &fixture.original.conversation_id,
                mez_agent::AgentContext::empty()
            )
            .unwrap()
            .is_none()
    );
    let turns = fixture.service.agent_turn_ledger().turns().len();
    let primary = fixture
        .service
        .session
        .layout_owner_client_id()
        .unwrap()
        .clone();
    let result = fixture
        .service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    assert!(result.contains("state=queued"), "{result}");
    let task = fixture
        .service
        .pending_agent_compaction_task_for_tests("%1")
        .unwrap();
    assert!(task.replay_source.is_some());
    let input = crate::test_support::compaction_source::rendered_source(task).unwrap();
    assert!(input.contains("FIRST_RANGE_SOURCE") && input.contains("SECOND_RANGE_SOURCE"));
    assert!(!input.contains("CURRENT_USER_PROMPT"));
    let summary = "one manual head over completed task roots";
    complete_runtime_test_compaction(&mut fixture.service, "%1", summary);
    let receipt = replay_publication::execute_pending_publication(&mut fixture.service);
    assert!(
        fixture
            .service
            .apply_replay_epoch_completion(receipt)
            .unwrap()
            .applied
    );
    assert_eq!(fixture.service.agent_turn_ledger().turns().len(), turns);
    assert!(fixture.service.pending_agent_provider_tasks().is_empty());
    assert!(!fixture.service.agent_is_compacting("%1"));
    let (_, cold) = AgentTranscriptStore::new(fixture.store.root())
        .load_replay_epoch(
            &fixture.original.conversation_id,
            mez_agent::AgentContext::empty(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(cold.revision().get(), 1);
    assert_eq!(cold.context().chronology()[0].block().content, summary);
    assert!(
        !cold
            .context()
            .blocks()
            .iter()
            .any(|block| block.content.contains("FIRST_RANGE_SOURCE")
                || block.content.contains("SECOND_RANGE_SOURCE"))
    );
    assert!(
        cold.context()
            .blocks()
            .iter()
            .any(|block| block.content.contains("CURRENT_USER_PROMPT"))
    );
    assert!(
        fixture
            .store
            .inspect(&fixture.original.conversation_id)
            .unwrap()
            .starts_with(&archive)
    );
}
