//! Manual compaction candidate replay parity for retained native executions.
//!
//! Candidate sizing must retain the same execution ownership as published
//! history, including rows accepted while the compactor is running.

use super::*;
use crate::integrations::agent::context::assemble_model_request;

/// Original pending native source uses either a real typed publication or the
/// actual headless NoStore binding. No bare row or fabricated closure grants
/// native authority. Both candidate modules use the actual command driver.
pub(super) struct CanonicalManualFixture {
    pub(super) service: RuntimeSessionService,
    pub(super) store: AgentTranscriptStore,
    pub(super) conversation: String,
    pub(super) group: mez_agent::ContextExecutionGroupId,
    pub(super) owner: mez_agent::ProviderContinuityOwner,
    pub(super) original: mez_agent::ReplayEpochContext,
    pub(super) source_records: Vec<mez_agent::ContextBlockRecord>,
}

impl CanonicalManualFixture {
    pub(super) fn new(input_limit: Option<usize>, retention: u8, references: bool) -> Self {
        Self::with_storage(input_limit, retention, references, true)
    }

    pub(super) fn headless(references: bool) -> Self {
        Self::with_storage(None, 1, references, false)
    }

    fn with_storage(
        input_limit: Option<usize>,
        retention: u8,
        references: bool,
        saved: bool,
    ) -> Self {
        let mut service = test_runtime_service();
        let limit =
            input_limit.map_or_else(String::new, |limit| format!("max_input_tokens={limit}\n"));
        service.replace_config_layers(vec![ConfigLayer {
            name: "canonical-manual-candidate".into(), path: None, format: ConfigFormat::Toml,
            scope: ConfigScope::Primary, trusted: true,
            text: format!("[agents]\ndefault_provider=\"openai\"\ndefault_model_profile=\"candidate\"\ncompaction_raw_retention_percent={retention}\n[providers.openai]\nkind=\"openai\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.candidate]\nprovider=\"openai\"\nmodel=\"test\"\ncontext_window_tokens=128000\n{limit}"),
        }]).unwrap();
        let conversation = "canonical-manual-candidate".to_string();
        let store = AgentTranscriptStore::new(temp_root("canonical-manual-candidate"));
        let mut original = mez_agent::ReplayEpochContext::new(
            mez_agent::ReplayConversationId::new(&conversation).unwrap(),
            mez_agent::AgentContext::empty(),
        )
        .unwrap();
        original
            .append_fact("initial covered source", "COVERED_RAW_MUST_NOT_REENTER")
            .unwrap();
        let first = match original
            .prepare_compaction(mez_agent::ReplayTailPolicy::new(128_000, 0).unwrap())
            .unwrap()
        {
            mez_agent::ReplayCompactionPreparation::Ready(plan) => plan,
            other => panic!("expected original source: {other:?}"),
        };
        original
            .append_fact("eligible old prefix", "ELIGIBLE_OLD_PREFIX ".repeat(1_000))
            .unwrap();
        original
            .append_fact("retained fact", "RAW_RETAINED_FACT ".repeat(100))
            .unwrap();
        if references {
            let mut body = original.into_owned_context();
            body.append_reference_event(
                ContextSourceKind::Memory,
                format!("memory {} (note)", mez_agent::memory::new_memory_uuid()),
                "ordinary optional memory",
            )
            .unwrap();
            body.append_reference_event(
                ContextSourceKind::PersistedContextDocument,
                "candidate exact reference",
                "preserve exact reference bytes",
            )
            .unwrap();
            original = mez_agent::ReplayEpochContext::from_owned_context(
                mez_agent::ReplayConversationId::new(&conversation).unwrap(),
                body,
            )
            .unwrap();
        }
        let group = mez_agent::ContextExecutionGroupId::new("retained-native").unwrap();
        let owner = mez_agent::ProviderContinuityOwner::new(
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
            "openai",
        )
        .unwrap();
        let call = mez_agent::ProviderTranscriptEvent::validated_openai_response_output(vec![
            serde_json::json!({
                "type":"function_call", "id":"fc_retained", "call_id":"call_retained",
                "name":"submit_maap_action_batch", "arguments":"{}"
            }),
        ])
        .unwrap()
        .to_transcript_content();
        let result = mez_agent::ProviderTranscriptEvent::OpenAiFunctionCallOutput {
            call_id: "call_retained".into(),
            output: "exact native result".into(),
        }
        .to_transcript_content();
        original
            .append_execution_events([
                mez_agent::ContextConversationAppend::assistant(
                    "assistant",
                    "retained assistant",
                    group.clone(),
                ),
                mez_agent::ContextConversationAppend::evidence(
                    ContextSourceKind::TranscriptTool,
                    "native call",
                    call,
                    group.clone(),
                    Some(owner.clone()),
                    true,
                ),
                mez_agent::ContextConversationAppend::evidence(
                    ContextSourceKind::TranscriptTool,
                    "native result",
                    result,
                    group.clone(),
                    Some(owner.clone()),
                    true,
                ),
                mez_agent::ContextConversationAppend::evidence(
                    ContextSourceKind::ActionResult,
                    "result",
                    "exact action result",
                    group.clone(),
                    None,
                    true,
                ),
            ])
            .unwrap();
        assert!(original.context().execution_group_is_pending(&group));
        let source_records = original
            .context()
            .chronology()
            .iter()
            .map(|event| event.record())
            .collect();
        if saved {
            let proposal = store
                .prepare_replay_epoch(
                    crate::storage::transcript::replay_epoch::ReplaySourceOrigin::new(
                        "original-turn",
                        "agent-%1",
                        "%1",
                    )
                    .unwrap()
                    .capture(None),
                    &conversation,
                    None,
                    &original,
                    first,
                    mez_agent::ReplayHeadPublication::Summary("previous canonical head".into()),
                )
                .unwrap();
            store.publish_replay_epoch(&proposal).unwrap();
        }
        service.set_agent_transcript_store(store.clone());
        service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        if saved {
            service
                .agent_shell_store_mut()
                .bind_conversation("%1", &conversation, 0)
                .unwrap();
        } else {
            service
                .agent_shell_store_mut()
                .bind_ephemeral_conversation_with_lineage("%1", &conversation, 0, None)
                .unwrap();
            service
                .retain_ephemeral_replay_body(
                    "%1",
                    &conversation,
                    &original.clone().into_owned_context(),
                    None,
                    Some((
                        crate::storage::transcript::replay_epoch::ReplaySourceOrigin::new(
                            "original-turn",
                            "agent-%1",
                            "%1",
                        )
                        .unwrap(),
                        0,
                    )),
                    None,
                )
                .unwrap();
        }
        Self {
            service,
            store,
            conversation,
            group,
            owner,
            original,
            source_records,
        }
    }

    /// Runs actual inline or two-phase source/request workers; no synthetic
    /// task or publication completion is inserted into the command path.
    pub(super) fn queue(&mut self, worker: bool) -> crate::runtime::RuntimeAgentCompactionTask {
        if worker {
            self.service.use_agent_source_preparation_adapter();
        }
        let primary = self
            .service
            .session
            .layout_owner_client_id()
            .unwrap()
            .clone();
        let result = self
            .service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        if worker {
            assert!(result.contains("state=preparing"), "{result}");
            let source = self
                .service
                .take_manual_compaction_preparations()
                .pop()
                .unwrap();
            let captured = source.execute_source();
            assert!(
                self.service
                    .complete_manual_compaction_preparation(&source, captured)
                    .unwrap()
            );
            let request = self
                .service
                .take_manual_compaction_requests()
                .pop()
                .unwrap();
            let rendered = request.execute_request();
            assert!(
                self.service
                    .complete_manual_compaction_request(&request, rendered)
                    .unwrap()
            );
        } else {
            assert!(result.contains("state=queued"), "{result}");
        }
        self.service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .clone()
    }

    pub(super) fn publish(&mut self, summary: &str) {
        complete_runtime_test_compaction(&mut self.service, "%1", summary);
        let receipt = crate::test_support::compaction_publication::execute_pending_publication(
            &mut self.service,
        );
        assert!(self.service.pending_agent_provider_tasks().is_empty());
        assert!(
            self.service
                .apply_replay_epoch_completion(receipt)
                .unwrap()
                .applied
        );
        assert!(self.service.pending_agent_provider_tasks().is_empty());
        assert!(!self.service.agent_is_compacting("%1"));
    }
}

/// Delayed ordinary rows for an epoch-owned native group remain exact in the
/// preview and actual replay. Pending bytes never become an append receipt or
/// another source occurrence, and source/cost wrappers never enter wire bodies.
#[test]
fn runtime_manual_compaction_candidate_retains_native_execution_ownership() {
    for worker in [false, true] {
        let mut fixture = CanonicalManualFixture::new(None, 1, false);
        let task = fixture.queue(worker);
        assert!(task.replay_source.is_some());
        let before_identity = fixture
            .store
            .load_replay_epoch(&fixture.conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap()
            .0;
        let native = fixture
            .original
            .context()
            .chronology()
            .iter()
            .filter(|event| event.execution_group_id() == Some(&fixture.group))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(native.len(), 4);
        let rows = native
            .iter()
            .enumerate()
            .map(|(index, event)| {
                let semantic = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                    event.block().source,
                    event.block().label.clone(),
                    event.block().content.clone(),
                    fixture.group.clone(),
                    index as u64 + 1,
                    event.provider_owner().cloned(),
                )
                .unwrap();
                TranscriptEntry {
                    conversation_id: fixture.conversation.clone(),
                    sequence: index as u64 + 1,
                    created_at_unix_seconds: 1,
                    role: TranscriptRole::System,
                    turn_id: "original-turn".into(),
                    agent_id: "agent-%1".into(),
                    pane_id: "%1".into(),
                    content: mez_agent::transcript::TranscriptContextRecord::new(
                        semantic,
                        event.record(),
                        None,
                    )
                    .unwrap()
                    .to_transcript_content()
                    .unwrap(),
                }
            })
            .collect::<Vec<_>>();
        let append =
            crate::storage::transcript::TranscriptAppendBatch::owned_rows(rows.clone()).unwrap();
        let path = fixture
            .store
            .transcript_path(&fixture.conversation)
            .unwrap();
        fixture.service.use_transcript_effect_adapter();
        fixture
            .service
            .persistence
            .queue_transcript(RuntimeSideEffect::PersistTranscriptEntries {
                store: fixture.store.clone(),
                path: path.clone(),
                batch: append.clone(),
            });
        // These are delayed physical copies of already captured source. There
        // is no new logical arrival or guessed pane-count admission to apply.
        assert!(
            fixture
                .store
                .inspect(&fixture.conversation)
                .unwrap()
                .is_empty()
        );
        let candidate = fixture
            .service
            .manual_compaction_candidate_context(&task, "short summary")
            .unwrap();
        candidate.validate_durable().unwrap();
        let events = candidate
            .chronology()
            .iter()
            .filter(|event| event.execution_group_id() == Some(&fixture.group))
            .collect::<Vec<_>>();
        assert_eq!(events.len(), native.len());
        for (actual, original) in events.iter().zip(&native) {
            assert_eq!(*actual, original);
        }
        assert!(candidate.execution_group_is_pending(&fixture.group));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.provider_owner() == Some(&fixture.owner))
                .count(),
            2
        );
        assert!(
            fixture
                .service
                .manual_compaction_candidate_context(&task, "two valid words")
                .is_ok()
        );
        assert!(
            fixture
                .service
                .manual_compaction_candidate_context(&task, "")
                .is_err()
        );
        let mut stale = task.clone();
        stale.task_generation += 1;
        assert!(
            fixture
                .service
                .manual_compaction_candidate_context(&stale, "short summary")
                .is_err()
        );
        assert_eq!(
            fixture
                .store
                .load_replay_epoch(&fixture.conversation, mez_agent::AgentContext::empty())
                .unwrap()
                .unwrap()
                .0,
            before_identity
        );
        fixture.publish("short summary");
        let mut published = fixture
            .service
            .agent_context_for_pane_prompt("%1", "Continue after conversation compaction.", 100)
            .unwrap();
        published.set_metadata(candidate.metadata().clone());
        assert_eq!(candidate.blocks(), published.blocks());
        for (expected, actual) in candidate.chronology().iter().zip(published.chronology()) {
            assert_eq!(expected.semantic_kind(), actual.semantic_kind());
            assert_eq!(expected.retention(), actual.retention());
            assert_eq!(expected.execution_group_id(), actual.execution_group_id());
            assert_eq!(expected.provider_owner(), actual.provider_owner());
        }
        let (_, cold) = AgentTranscriptStore::new(fixture.store.root())
            .load_replay_epoch(&fixture.conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(cold.revision().get(), 2);
        assert_eq!(
            cold.context().chronology()[0].block().content,
            "short summary"
        );
        let turn = AgentTurnRecord {
            turn_id: "parity-turn".into(),
            conversation_id: fixture.conversation.clone(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            deadline_at_unix_millis: 0,
            policy_profile: "runtime".into(),
            model_profile: task.model_profile_name.clone(),
            parent_turn_id: None,
            state: AgentTurnState::Queued,
            cooperation_mode: None,
            initial_capability: None,
        };
        for (provider, model) in [
            ("openai", "test"),
            ("openai", "switched-model"),
            ("switched-openai", "test"),
            ("switched-openai", "switched-model"),
        ] {
            let mut profile = task.model_profile.clone();
            profile.provider = provider.into();
            profile.model = model.into();
            let api = mez_agent::ProviderApiCompatibility::OpenAiResponses;
            let before = assemble_model_request(&profile, api, &turn, &candidate).unwrap();
            let after = assemble_model_request(&profile, api, &turn, &published).unwrap();
            let before = mez_agent::openai_responses_request_body(&before).unwrap();
            let after = mez_agent::openai_responses_request_body(&after).unwrap();
            assert_eq!(before, after, "wire parity for {provider}/{model}");
            assert_eq!(
                mez_agent::provider_request_input_estimate_from_body(&before),
                mez_agent::provider_request_input_estimate_from_body(&after)
            );
            for record in &fixture.source_records {
                assert!(!before.contains(&String::from(record.occurrence_id())));
            }
            for marker in [
                "mez-transcript-context-record/",
                "rendered_sha256",
                "framed_input_tokens",
            ] {
                assert!(!before.contains(marker));
            }
            assert_eq!(before.contains("call_retained"), provider == "openai");
        }
        // Deliver the actual original append completion after publication. Its
        // physical copy cannot duplicate or renumber the retained native source.
        let mut writes = 0;
        for effect in fixture
            .service
            .drain_transcript_persistence_transition()
            .side_effects
        {
            match effect {
                RuntimeSideEffect::PersistTranscriptEntries { store, batch, path } => {
                    assert_eq!(
                        batch, append,
                        "dispatch must retain the original append owner"
                    );
                    let bytes = store.commit_accounted_transcript_batch(&batch, 1).unwrap();
                    assert!(
                        fixture
                            .service
                            .apply_persistence_transition(
                                crate::runtime::PersistenceEvent::TranscriptCompleted {
                                    conversation_id: fixture.conversation.clone(),
                                    first_sequence: batch.first_sequence(),
                                    batch,
                                    path,
                                    bytes,
                                }
                            )
                            .unwrap()
                            .applied
                    );
                    writes += 1;
                }
                RuntimeSideEffect::PersistAgentSessionMetadata {
                    store,
                    mezzanine_session_id,
                    generation,
                    records,
                    ..
                } => {
                    let bytes = store
                        .save_agent_session_metadata_checkpoint(&mezzanine_session_id, &records)
                        .unwrap();
                    fixture
                        .service
                        .apply_persistence_transition(
                            crate::runtime::PersistenceEvent::AgentSessionMetadataCompleted {
                                path: store
                                    .agent_session_metadata_checkpoint_file(&mezzanine_session_id),
                                mezzanine_session_id,
                                generation,
                                records: records.len(),
                                bytes,
                            },
                        )
                        .unwrap();
                }
                RuntimeSideEffect::PersistPresentationEntries {
                    store,
                    path,
                    entries,
                } => {
                    let bytes = store.append_presentation_many(&entries).unwrap();
                    fixture
                        .service
                        .apply_persistence_transition(
                            crate::runtime::PersistenceEvent::PresentationCompleted {
                                conversation_id: fixture.conversation.clone(),
                                path,
                                entries: entries.len(),
                                bytes,
                            },
                        )
                        .unwrap();
                }
                other => panic!("unexpected manual fixture persistence work: {other:?}"),
            }
        }
        assert_eq!(writes, 1);
        let after = fixture
            .service
            .agent_context_for_pane_prompt("%1", "Continue after conversation compaction.", 100)
            .unwrap();
        assert_eq!(after.blocks(), candidate.blocks());
        let auxiliary = fixture.queue(worker);
        let mut request = auxiliary.request.clone();
        let mut messages = request.messages.iter().cloned().collect::<Vec<_>>();
        messages.last_mut().unwrap().content =
            crate::test_support::compaction_source::rendered_source(&auxiliary).unwrap();
        request.messages = messages.into();
        let body = mez_agent::openai_responses_request_body(&request).unwrap();
        assert!(body.contains("RAW_RETAINED_FACT"));
        assert!(
            !body.contains("call_retained"),
            "protected native tail must stay outside selected input"
        );
        for record in &fixture.source_records {
            assert!(!body.contains(&String::from(record.occurrence_id())));
        }
        for marker in [
            "mez-transcript-context-record/",
            "rendered_sha256",
            "framed_input_tokens",
        ] {
            assert!(!body.contains(marker));
        }
    }
}

/// A complete manual final above the next ordinary request estimate publishes
/// immediately on both inline and prepared command paths, preserving native tail.
#[test]
fn runtime_canonical_manual_candidate_accepts_usable_output_without_fit_retry() {
    let large = format!("{}END", "large-summary ".repeat(8000));
    let mut calibration = CanonicalManualFixture::new(None, 90, true);
    let task = calibration.queue(false);
    let preview = calibration
        .service
        .manual_compaction_candidate_context(&task, &large)
        .unwrap();
    let (maximum, _, _) = calibration
        .service
        .manual_compaction_candidate_size_for_tests(&task, preview)
        .unwrap();
    let cap = maximum.saturating_sub(1000);
    for worker in [false, true] {
        let mut fixture = CanonicalManualFixture::new(Some(cap), 90, true);
        let task = fixture.queue(worker);
        let preview = fixture
            .service
            .manual_compaction_candidate_context(&task, &large)
            .unwrap();
        let (estimate, limit, _) = fixture
            .service
            .manual_compaction_candidate_size_for_tests(&task, preview)
            .unwrap();
        assert!(estimate > limit);
        fixture.publish(&large);
        let (_, cold) = fixture
            .store
            .load_replay_epoch(&fixture.conversation, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap();
        assert_eq!(cold.context().chronology()[0].block().content, large);
        assert!(cold.context().execution_group_is_pending(&fixture.group));
        assert!(fixture.service.agent_turn_ledger().turns().is_empty());
        assert!(fixture.service.pending_agent_provider_tasks().is_empty());
    }
}

/// Legacy reset decoding still excludes malformed/retired groups. Canonical
/// manual preview and actual receipt adoption revoke the same original scoped
/// retrieval while preserving its evidence and pending native source.
#[test]
fn runtime_compaction_candidate_mcp_epoch_matches_published_projection() {
    let group = mez_agent::ContextExecutionGroupId::new("manifest-group").unwrap();
    let malformed = mez_agent::ContextExecutionGroupId::new("malformed-group").unwrap();
    let contents = [
        mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            ContextSourceKind::TranscriptAssistant,
            "assistant",
            "retrieve manifest",
            group.clone(),
            1,
            None,
        )
        .unwrap()
        .to_transcript_content(),
        mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            ContextSourceKind::McpRetrievedManifest,
            "manifest",
            "obsolete manifest evidence",
            group,
            2,
            None,
        )
        .unwrap()
        .to_transcript_content(),
        mez_agent::TranscriptContextEvent::execution_block_with_metadata(
            ContextSourceKind::ActionResult,
            "orphan",
            "malformed evidence",
            malformed,
            2,
            None,
        )
        .unwrap()
        .to_transcript_content(),
        mez_agent::TranscriptContextEvent::user_event(
            9,
            "exact steering",
            "Keep this exact barrier.",
        )
        .unwrap()
        .to_transcript_content(),
    ];
    let mut rows = contents
        .into_iter()
        .enumerate()
        .map(|(index, content)| TranscriptEntry {
            conversation_id: "projection-parity".to_string(),
            sequence: index as u64 + 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::System,
            turn_id: "retained-turn".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content,
        })
        .collect::<Vec<_>>();
    rows.push(TranscriptEntry {
        conversation_id: "projection-parity".to_string(),
        sequence: 5,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "compact-turn".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: mez_agent::TranscriptContextEvent::McpCompactionEpoch.to_transcript_content(),
    });
    let published = crate::runtime::control::runtime_agent_transcript_context("%1", &rows);
    assert!(published.execution_events.is_empty());
    assert_eq!(published.blocks.len(), 1);
    assert_eq!(published.blocks[0].content, "Keep this exact barrier.");
    for worker in [false, true] {
        let mut fixture = CanonicalManualFixture::headless(true);
        let mut body = fixture.original.clone().into_owned_context();
        let result = mez_agent::ActionResult {
            protocol: "maap/1".into(), turn_id: "original-turn".into(), agent_id: "agent-%1".into(),
            action_id: "original-discovery".into(), action_type: "mcp_server_get", status: mez_agent::ActionStatus::Succeeded,
            content: Vec::new(), structured_content_json: Some(serde_json::json!({"server":{
                "server_id":"manual-scope-fixture", "state":"available", "display_name":"Fixture",
                "purpose":"manual preview parity", "usage_instructions":"Use echo", "tools":[{
                    "name":"echo", "state":"available", "description":"fixture echo",
                    "input_schema":{"type":"object","properties":{}}
                }]
            }}).to_string()), permission_evaluation: None, is_error: false, error: None,
        };
        let (server, content) =
            mez_agent::mcp_retrieved_manifest_for_action_result(&result, &body).unwrap();
        let group = mez_agent::ContextExecutionGroupId::new("original-discovery").unwrap();
        body.append_assistant_event("discovery", "retrieve original manifest", group.clone())
            .unwrap();
        body.append_evidence_event(
            ContextSourceKind::McpRetrievedManifest,
            format!(
                "{}{server}",
                mez_agent::MCP_RETRIEVED_MANIFEST_CONTEXT_LABEL_PREFIX
            ),
            content,
            group,
            None,
            true,
        )
        .unwrap();
        let original = body.chronology().last().unwrap().clone();
        fixture
            .service
            .retain_ephemeral_replay_body(
                "%1",
                &fixture.conversation,
                &body,
                None,
                Some((
                    crate::storage::transcript::replay_epoch::ReplaySourceOrigin::new(
                        "original-turn",
                        "agent-%1",
                        "%1",
                    )
                    .unwrap(),
                    0,
                )),
                None,
            )
            .unwrap();
        let available = |context: &mez_agent::AgentContext| {
            mez_agent::invoked_mcp_tools_for_context(
                context,
                &mez_agent::McpPromptSummary {
                    available_servers: Vec::new(),
                    available_tools: Vec::new(),
                    unavailable_servers: Vec::new(),
                },
            )
        };
        assert_eq!(available(&body).len(), 1);
        let task = fixture.queue(worker);
        let candidate = fixture
            .service
            .manual_compaction_candidate_context(&task, "new manual head")
            .unwrap();
        assert!(available(&candidate).is_empty());
        assert!(candidate.chronology().contains(&original));
        let unchanged = fixture
            .service
            .runtime_agent_history_epoch_context("%1")
            .unwrap()
            .replay_owner()
            .unwrap()
            .unwrap();
        assert_eq!(available(unchanged.context()).len(), 1);
        fixture.publish("new manual head");
        let accepted = fixture
            .service
            .agent_context_for_pane_prompt("%1", "Continue after conversation compaction.", 100)
            .unwrap();
        assert!(available(&accepted).is_empty());
        assert!(accepted.chronology().contains(&original));
        assert_eq!(candidate.blocks(), accepted.blocks());
        assert!(accepted.execution_group_is_pending(&fixture.group));
        assert!(fixture.service.agent_turn_ledger().turns().is_empty());
    }
}
