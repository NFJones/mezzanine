//! First canonical publication from the original committed legacy raw window.
//!
//! These tests enter through `/compact`, execute both preparation workers and
//! the actual epoch writer, then adopt its receipt. Plain historical user text
//! remains a reference; importing it cannot manufacture a new task or turn.

use super::*;
use mez_core::ids::ClientId;

const CONVERSATION: &str = "legacy-raw-normalization";

struct Fixture {
    service: RuntimeSessionService,
    primary: ClientId,
    store: AgentTranscriptStore,
    rows: Vec<TranscriptEntry>,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let mut service = test_runtime_service();
        service.replace_config_layers(vec![ConfigLayer {
            name: "legacy-raw-normalization".into(), path: None,
            format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
            text: "[agents]\ndefault_provider=\"openai\"\ndefault_model_profile=\"default\"\ncompaction_raw_retention_percent=1\n[providers.openai]\nkind=\"openai\"\nmodels=[\"fixture-model\"]\ndefault_model=\"fixture-model\"\n[model_profiles.default]\nprovider=\"openai\"\nmodel=\"fixture-model\"\ncontext_window_tokens=128000\n".into(),
        }]).unwrap();
        let store = AgentTranscriptStore::new(temp_root(label));
        let rows = [
            (
                TranscriptRole::User,
                "original old reference ".repeat(8_000),
            ),
            (
                TranscriptRole::Tool,
                "original ungrouped tool evidence".into(),
            ),
            (
                TranscriptRole::User,
                "literal historical user reference".into(),
            ),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (role, content))| TranscriptEntry {
            conversation_id: CONVERSATION.into(),
            sequence: index as u64 + 1,
            created_at_unix_seconds: index as u64 + 1,
            role,
            turn_id: format!("original-legacy-{index}"),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content,
        })
        .collect::<Vec<_>>();
        for row in &rows {
            store.append(row).unwrap();
        }
        service.set_agent_transcript_store(store.clone());
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service
            .agent_shell_store_mut()
            .bind_conversation("%1", CONVERSATION, rows.len() as u64)
            .unwrap();
        let primary = service
            .attach_primary("legacy raw", true, Size::new(80, 24).unwrap(), 1)
            .unwrap();
        service.use_agent_source_preparation_adapter();
        Self {
            service,
            primary,
            store,
            rows,
        }
    }

    /// Constructs a supported old physical layout before compaction admission.
    /// A warm case retains actual durable row identities and complete runtime
    /// metadata, so incidental first-read/checkpoint writes cannot migrate it.
    fn flat_layout(&mut self, indexed: bool) -> std::path::PathBuf {
        if indexed {
            let history = self
                .service
                .runtime_agent_history_epoch_context("%1")
                .unwrap();
            assert!(history.replay_owner().unwrap().is_some());
            assert!(
                self.store
                    .root()
                    .join(CONVERSATION)
                    .join("row-origins.sqlite3")
                    .is_file()
            );
            self.service
                .capture_agent_session_allowed_actions_for_pane("%1")
                .unwrap();
            self.service.checkpoint_agent_session_metadata().unwrap();
        }
        let original = self.store.transcript_path(CONVERSATION).unwrap();
        let flat = self.store.root().join(format!("{CONVERSATION}.tsv"));
        std::fs::rename(&original, &flat).unwrap();
        assert_eq!(self.store.inspect(CONVERSATION).unwrap(), self.rows);
        flat
    }

    fn queue(&mut self) -> crate::runtime::RuntimeAgentCompactionTask {
        let response = self
            .service
            .execute_agent_shell_command(&self.primary, "/compact")
            .unwrap();
        assert!(response.contains("state=preparing"), "{response}");
        let source = self
            .service
            .take_manual_compaction_preparations()
            .pop()
            .unwrap();
        let result = source.execute_source();
        assert!(
            self.service
                .complete_manual_compaction_preparation(&source, result)
                .unwrap()
        );
        let request = self
            .service
            .take_manual_compaction_requests()
            .pop()
            .unwrap();
        let result = request.execute_request();
        assert!(
            self.service
                .complete_manual_compaction_request(&request, result)
                .unwrap()
        );
        let task = self
            .service
            .pending_agent_compaction_task_for_tests("%1")
            .unwrap()
            .clone();
        assert!(
            task.replay_source.is_some(),
            "legacy source bypassed the canonical writer"
        );
        assert!(self.service.agent_turn_ledger().turns().is_empty());
        task
    }

    fn publish(&mut self, summary: &str) {
        complete_runtime_test_compaction(&mut self.service, "%1", summary);
        let receipt = crate::test_support::compaction_publication::execute_pending_publication(
            &mut self.service,
        );
        assert!(
            self.service
                .apply_replay_epoch_completion(receipt)
                .unwrap()
                .applied
        );
        assert!(!self.service.agent_is_compacting("%1"));
        assert!(self.service.pending_agent_provider_tasks().is_empty());
        assert!(self.service.agent_turn_ledger().turns().is_empty());
    }
}

/// A real resume and manual compaction must also preserve the old root-TSV
/// layout's history. Construct that layout before any source work is admitted;
/// migration must use the existing storage owner, keeping exact audit rows and
/// making the resulting single head readable from a newly opened store.
#[test]
fn runtime_resumed_legacy_flat_archive_compacts_through_single_head_writer() {
    for prior_head in [false, true] {
        for indexed in [false, true] {
            let mut fixture = Fixture::new(&format!(
                "manual-legacy-flat-archive-{prior_head}-{indexed}"
            ));
            if prior_head {
                // The persisted prefix is normalized by the actual history owner;
                // a warm case enters model publication with an existing typed parent.
                fixture
                    .store
                    .save_compaction_epoch(CONVERSATION, 0, "prior flat summary head")
                    .unwrap();
            }
            let original = fixture.store.transcript_path(CONVERSATION).unwrap();
            let flat = fixture.flat_layout(indexed);
            fixture
                .service
                .agent_shell_store_mut()
                .start_new_conversation("%1")
                .unwrap();
            let accepted = fixture
                .service
                .execute_agent_shell_command(&fixture.primary, &format!("/resume {CONVERSATION}"))
                .unwrap();
            let resumed = fixture
                .service
                .run_pending_deferred_agent_command_for_tests()
                .unwrap()
                .unwrap_or(accepted);
            assert!(resumed.contains("resumed=true"), "{resumed}");
            fixture.queue();
            let before = fixture
                .store
                .load_replay_epoch(CONVERSATION, mez_agent::AgentContext::empty())
                .unwrap();
            assert_eq!(before.is_some(), prior_head);
            let previous_revision = before
                .as_ref()
                .map_or(0, |(_, owner)| owner.revision().get());
            fixture.publish("one head from the original flat archive");
            assert!(!flat.exists());
            assert!(original.is_file());
            assert_eq!(fixture.store.inspect(CONVERSATION).unwrap(), fixture.rows);
            let (_, cold) = AgentTranscriptStore::new(fixture.store.root())
                .load_replay_epoch(CONVERSATION, mez_agent::AgentContext::empty())
                .unwrap()
                .unwrap();
            assert_eq!(cold.revision().get(), previous_revision + 1);
            assert_eq!(
                cold.context().chronology()[0].block().content,
                "one head from the original flat archive"
            );
            assert!(fixture.service.agent_turn_ledger().turns().is_empty());
        }
    }
}

/// Metadata promotion has its original rollback owner even when invoked from
/// the single-head publisher. Neither permission nor metadata replacement
/// failure can strand the raw payload, change its metadata or publish a head.
#[test]
fn runtime_manual_legacy_flat_promotion_failure_preserves_original_source() {
    for permissions in [false, true] {
        let mut fixture = Fixture::new(&format!("legacy-flat-rollback-{permissions}"));
        let flat = fixture.flat_layout(true);
        let original = fixture.store.transcript_path(CONVERSATION).unwrap();
        let bytes = std::fs::read(&flat).unwrap();
        let metadata_path = fixture
            .store
            .root()
            .join(CONVERSATION)
            .join("metadata.json");
        let metadata = std::fs::read(&metadata_path).unwrap();
        fixture.queue();
        if permissions {
            fixture
                .store
                .fail_next_legacy_promotion_permissions_after_rename();
        } else {
            fixture.store.fail_next_metadata_write_after_promotion();
        }
        complete_runtime_test_compaction(&mut fixture.service, "%1", "unpublished flat summary");
        let completion = fixture
            .service
            .take_replay_epoch_attempt_for_tests()
            .unwrap()
            .execute();
        let error = completion
            .error()
            .expect("existing promotion fault must be reached");
        assert!(
            error.message().contains(if permissions {
                "legacy promotion permission"
            } else {
                "metadata write failure"
            }),
            "{error:?}"
        );
        assert!(
            fixture
                .service
                .apply_replay_epoch_completion(completion)
                .unwrap()
                .applied
        );
        assert!(!fixture.service.agent_is_compacting("%1"));
        assert!(fixture.service.pending_agent_compaction_tasks().is_empty());
        assert!(fixture.service.pending_agent_provider_tasks().is_empty());
        assert!(fixture.service.agent_turn_ledger().turns().is_empty());
        assert_eq!(std::fs::read(&flat).unwrap(), bytes);
        assert!(!original.exists());
        assert_eq!(std::fs::read(&metadata_path).unwrap(), metadata);
        assert!(
            fixture
                .store
                .load_replay_epoch(CONVERSATION, mez_agent::AgentContext::empty())
                .unwrap()
                .is_none()
        );
        // This explicit new user request follows a definite pre-publication
        // refusal. It does not recover an ambiguous head or replay an action.
        fixture.queue();
        fixture.publish("one head after explicit promotion retry");
        assert!(!flat.exists());
        assert_eq!(std::fs::read(original).unwrap(), bytes);
        assert_eq!(fixture.store.inspect(CONVERSATION).unwrap(), fixture.rows);
    }
}

/// Publication changes only the active projection. Its original audit rows and
/// the exact retained members, IDs and cached costs survive a cold renderer.
#[test]
fn runtime_manual_legacy_raw_publishes_one_head_and_exact_reference_tail() {
    let mut fixture = Fixture::new("manual-legacy-raw-cold");
    let history = fixture
        .service
        .runtime_agent_history_epoch_context("%1")
        .unwrap();
    let original = history
        .replay_owner()
        .unwrap()
        .expect("qualified original raw owner");
    assert!(original.head().is_none());
    assert_eq!(original.revision().get(), 0);
    assert_eq!(
        original
            .context()
            .chronology()
            .last()
            .unwrap()
            .block()
            .source,
        ContextSourceKind::TranscriptUser
    );
    assert_eq!(
        original
            .context()
            .chronology()
            .last()
            .unwrap()
            .semantic_kind(),
        mez_agent::ContextSemanticKind::UserEvent
    );
    let task = fixture.queue();
    let candidate = fixture
        .service
        .manual_compaction_candidate_context(&task, "one actual model summary")
        .unwrap();
    assert_eq!(candidate.chronology().len(), 3);
    let retained = &original.context().chronology()[1..];
    assert_eq!(&candidate.chronology()[1..1 + retained.len()], retained);
    // Full ordinary request sizing includes a temporary continuation prompt;
    // it is not part of the published historical head or retained raw source.
    assert_eq!(
        candidate.chronology().last().unwrap().block().source,
        ContextSourceKind::UserInstruction
    );
    fixture.publish("one actual model summary");
    assert_eq!(fixture.store.inspect(CONVERSATION).unwrap(), fixture.rows);
    let (identity, cold) = AgentTranscriptStore::new(fixture.store.root())
        .load_replay_epoch(CONVERSATION, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(cold.revision().get(), 1);
    assert_eq!(cold.context().chronology().len(), 1 + retained.len());
    assert_eq!(
        cold.context().chronology()[0].block(),
        candidate.chronology()[0].block()
    );
    // Preview and actual publication allocate independent summary occurrences.
    // The original retained occurrences must remain identical across both.
    assert_eq!(&cold.context().chronology()[1..], retained);
    let mut fresh = test_runtime_service();
    fresh.set_agent_transcript_store(AgentTranscriptStore::new(fixture.store.root()));
    fresh.agent_shell_store_mut().enter_or_resume("%1").unwrap();
    fresh
        .agent_shell_store_mut()
        .bind_conversation("%1", CONVERSATION, 3)
        .unwrap();
    let projected = fresh
        .runtime_agent_history_epoch_context("%1")
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
    assert_eq!(
        projected.context().chronology(),
        cold.context().chronology()
    );
    assert_eq!(
        fixture
            .store
            .load_replay_epoch(CONVERSATION, mez_agent::AgentContext::empty())
            .unwrap()
            .unwrap()
            .0,
        identity
    );
}

/// Manual preparation deterministically imports a saved one-prefix authority
/// through the ordinary history owner before asking for a model rewrite. No
/// legacy request factory, intermediate model summary or ordinary turn appears.
#[test]
fn runtime_manual_legacy_prefix_uses_canonical_source_and_publication() {
    let mut fixture = Fixture::new("manual-legacy-prefix-canonical");
    fixture
        .store
        .install_legacy_compaction_ranges_fixture(
            CONVERSATION,
            0,
            "original historical prefix",
            Vec::new(),
        )
        .unwrap();
    let original_record = fixture
        .store
        .compaction_epoch(CONVERSATION)
        .unwrap()
        .unwrap()
        .summary_records
        .unwrap()
        .prefix
        .unwrap();
    let task = fixture.queue();
    let source = crate::test_support::compaction_source::rendered_source(&task).unwrap();
    assert!(source.contains("original historical prefix"));
    assert!(source.contains("original old reference"));
    let (_, imported) = fixture
        .store
        .load_replay_epoch(CONVERSATION, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(imported.revision().get(), 1);
    assert_eq!(imported.context().chronology()[0].record(), original_record);
    assert_eq!(
        imported.context().chronology()[0].block().content,
        "original historical prefix"
    );
    let retained = imported.context().chronology()[2..].to_vec();
    assert_eq!(fixture.store.inspect(CONVERSATION).unwrap(), fixture.rows);
    fixture.publish("new actual model head");
    let (_, cold) = AgentTranscriptStore::new(fixture.store.root())
        .load_replay_epoch(CONVERSATION, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_eq!(cold.revision().get(), 2);
    assert_eq!(
        cold.context().chronology()[0].block().content,
        "new actual model head"
    );
    assert_eq!(&cold.context().chronology()[1..], retained);
    assert_eq!(fixture.store.inspect(CONVERSATION).unwrap(), fixture.rows);
}

/// A real edit after source capture invalidates that source. Neither a valid
/// model result nor actor adoption may replace the changed physical history.
#[test]
fn runtime_manual_legacy_raw_refuses_changed_original_source() {
    let mut fixture = Fixture::new("manual-legacy-raw-cas");
    fixture.queue();
    let row = &fixture.rows[0];
    fixture
        .store
        .compare_and_swap_entry_content(
            CONVERSATION,
            row.sequence,
            &row.pane_id,
            &fixture.store.transcript_entry_revision(row).unwrap(),
            "actual edited original source".into(),
        )
        .unwrap();
    let changed = fixture.store.inspect(CONVERSATION).unwrap();
    assert_eq!(changed[0].content, "actual edited original source");
    complete_runtime_test_compaction(
        &mut fixture.service,
        "%1",
        "must not overwrite edited history",
    );
    let attempt = fixture
        .service
        .take_replay_epoch_attempt_for_tests()
        .expect("original physical attempt");
    let completion = attempt.execute();
    assert!(
        completion.error().is_some(),
        "changed raw source was published"
    );
    fixture
        .service
        .apply_replay_epoch_completion(completion)
        .unwrap();
    assert!(!fixture.service.agent_is_compacting("%1"));
    assert!(
        fixture
            .service
            .take_replay_epoch_attempt_for_tests()
            .is_none()
    );
    assert!(
        fixture
            .store
            .compaction_epoch(CONVERSATION)
            .unwrap()
            .is_none()
    );
    assert_eq!(fixture.store.inspect(CONVERSATION).unwrap(), changed);
    assert!(fixture.service.pending_agent_provider_tasks().is_empty());
    assert!(fixture.service.agent_turn_ledger().turns().is_empty());
}

/// A paired native wire image establishes descriptive grouping, never closure.
/// The first normalization must keep both original members pending and exact.
#[test]
fn runtime_manual_legacy_raw_keeps_native_group_protected() {
    let mut fixture = Fixture::new("manual-legacy-raw-native");
    let group = mez_agent::ContextExecutionGroupId::new("original-legacy-native").unwrap();
    let provider = mez_agent::ProviderContinuityOwner::new(
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        "openai",
    )
    .unwrap();
    let call = mez_agent::ProviderTranscriptEvent::validated_openai_response_output(vec![
        serde_json::json!({"type":"function_call","id":"fc_legacy","call_id":"call_legacy",
            "name":"submit_maap_action_batch","arguments":"{}"}),
    ])
    .unwrap();
    let result = mez_agent::ProviderTranscriptEvent::OpenAiFunctionCallOutput {
        call_id: "call_legacy".into(),
        output: "original legacy native result".into(),
    };
    let events = [
        (
            ContextSourceKind::TranscriptAssistant,
            "original assistant anchor".into(),
            None,
        ),
        (
            ContextSourceKind::TranscriptTool,
            call.to_transcript_content(),
            Some(provider.clone()),
        ),
        (
            ContextSourceKind::TranscriptTool,
            result.to_transcript_content(),
            Some(provider),
        ),
    ];
    for (index, (source, content, provider)) in events.into_iter().enumerate() {
        let row = TranscriptEntry {
            conversation_id: CONVERSATION.into(),
            sequence: index as u64 + 4,
            created_at_unix_seconds: 10,
            role: TranscriptRole::System,
            turn_id: "original-native-producer".into(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content: mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                source,
                "original native source",
                content,
                group.clone(),
                index as u64 + 1,
                provider,
            )
            .unwrap()
            .to_transcript_content(),
        };
        fixture.store.append(&row).unwrap();
        fixture.rows.push(row);
    }
    fixture
        .service
        .agent_shell_store_mut()
        .bind_conversation("%1", CONVERSATION, 6)
        .unwrap();
    let original = fixture
        .service
        .runtime_agent_history_epoch_context("%1")
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
    assert!(original.context().execution_group_is_pending(&group));
    let expected = original
        .context()
        .chronology()
        .iter()
        .filter(|event| event.execution_group_id() == Some(&group))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(expected.len(), 3);
    let task = fixture.queue();
    let candidate = fixture
        .service
        .manual_compaction_candidate_context(&task, "older historical reference summary")
        .unwrap();
    assert_eq!(
        candidate
            .chronology()
            .iter()
            .filter(|event| event.execution_group_id() == Some(&group))
            .cloned()
            .collect::<Vec<_>>(),
        expected
    );
    fixture.publish("older historical reference summary");
    let (_, cold) = fixture
        .store
        .load_replay_epoch(CONVERSATION, mez_agent::AgentContext::empty())
        .unwrap()
        .unwrap();
    assert!(cold.context().execution_group_is_pending(&group));
    assert_eq!(
        cold.context()
            .chronology()
            .iter()
            .filter(|event| event.execution_group_id() == Some(&group))
            .cloned()
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(fixture.store.inspect(CONVERSATION).unwrap(), fixture.rows);
}

/// A real loop source can be copied before a head exists. Only the child's
/// original initial receipt may establish durability; user roles cannot become
/// task admissions while projecting its independent physical rows.
#[test]
fn runtime_legacy_raw_frozen_copy_preserves_original_semantics() {
    let fixture = Fixture::new("legacy-raw-memory-copy");
    let (history, frozen) = fixture
        .service
        .prepare_runtime_agent_prompt_history_work("%1")
        .execute_loop_history(3)
        .unwrap();
    let original = history.replay_owner().unwrap().unwrap();
    let source = frozen
        .expect("original checked loop selection")
        .fork_source()
        .unwrap();
    let batch = source.initial_batch("legacy-raw-child", 100).unwrap();
    assert!(fixture.store.summary("legacy-raw-child").unwrap().is_none());
    fixture
        .store
        .accept_transcript_append_receipt(&batch, 1)
        .unwrap();
    fixture
        .store
        .settle_transcript_append_receipt(&batch, 1)
        .unwrap();
    let mut fresh = test_runtime_service();
    fresh.set_agent_transcript_store(AgentTranscriptStore::new(fixture.store.root()));
    fresh.agent_shell_store_mut().enter_or_resume("%2").unwrap();
    fresh
        .agent_shell_store_mut()
        .bind_conversation("%2", "legacy-raw-child", batch.entries().len() as u64)
        .unwrap();
    let child = fresh
        .runtime_agent_history_epoch_context("%2")
        .unwrap()
        .replay_owner()
        .unwrap()
        .unwrap();
    assert!(child.head().is_none());
    assert_eq!(
        child.context().chronology(),
        original.context().chronology()
    );
    assert_eq!(fixture.store.inspect(CONVERSATION).unwrap(), fixture.rows);
}

/// The original proposal survives failures both before and after atomic swap.
/// Recovery adopts exactly that first head without another model request or
/// treating the captured raw window as an already committed parent manifest.
#[test]
fn runtime_manual_legacy_raw_recovers_original_publication_at_fault_boundaries() {
    use crate::storage::transcript::replay_epoch::Phase;
    for flat in [false, true] {
        for phase in [
            Phase::BeforePayloadSync,
            Phase::BeforeSourceReceiptSync,
            Phase::BeforeManifestSwap,
            Phase::AfterManifestSwap,
            Phase::BeforeRequiredMarker,
            Phase::BeforeAcknowledgment,
        ] {
            let mut fixture = Fixture::new(&format!("legacy-raw-publication-{flat}-{phase:?}"));
            if flat {
                fixture.flat_layout(true);
            }
            let task = fixture.queue();
            fixture.store.inject_replay_epoch_fault(phase, 1, false);
            complete_runtime_test_compaction(
                &mut fixture.service,
                "%1",
                "original accepted model summary",
            );
            let first = fixture
                .service
                .take_replay_epoch_attempt_for_tests()
                .unwrap()
                .execute();
            assert!(first.error().is_some(), "fault was not reached: {phase:?}");
            fixture
                .service
                .apply_replay_epoch_completion(first)
                .unwrap();
            assert!(fixture.service.agent_is_compacting("%1"));
            let committed = fixture
                .store
                .load_replay_epoch(CONVERSATION, mez_agent::AgentContext::empty())
                .unwrap();
            fixture
                .service
                .poll_replay_epoch_publication("%1", task.task_generation)
                .unwrap();
            let receipt = crate::test_support::compaction_publication::execute_pending_publication(
                &mut fixture.service,
            );
            assert!(
                fixture
                    .service
                    .apply_replay_epoch_completion(receipt)
                    .unwrap()
                    .applied
            );
            let (identity, cold) = fixture
                .store
                .load_replay_epoch(CONVERSATION, mez_agent::AgentContext::empty())
                .unwrap()
                .unwrap();
            assert_eq!(cold.revision().get(), 1);
            assert_eq!(
                cold.context().chronology()[0].block().content,
                "original accepted model summary"
            );
            if let Some((published, _)) = committed {
                assert_eq!(identity, published);
            }
            assert!(!fixture.service.agent_is_compacting("%1"));
            assert!(fixture.service.pending_agent_provider_tasks().is_empty());
            assert!(
                fixture
                    .service
                    .pending_agent_compaction_task_ids()
                    .is_empty()
            );
            assert!(fixture.service.agent_turn_ledger().turns().is_empty());
            assert_eq!(fixture.store.inspect(CONVERSATION).unwrap(), fixture.rows);
        }
    }
}
