//! Shared synthetic durable epoch for history-import and real prompt tests.
//! Three real publications leave sparse original sequences, protected user and
//! native source, no ordinary archive rows, and no provider/action dispatch.

use crate::storage::transcript::{
    AgentTranscriptStore,
    replay_epoch::{ReplayEpochIdentity, ReplaySourceOrigin},
};
use mez_agent::transcript::{TranscriptContextEvent, TranscriptContextRecord};
use mez_agent::{
    AgentContext, ContextBlock, ContextConversationAppend, ContextEventSequence,
    ContextExecutionGroupId, ContextSourceKind, ProviderApiCompatibility, ProviderContinuityOwner,
    ProviderTranscriptEvent, ReplayCompactionPreparation, ReplayConversationId, ReplayEpochContext,
    ReplayHeadPublication, ReplayTailPolicy,
};
use mez_agent::{TranscriptEntry, TranscriptRole};
use std::path::PathBuf;

/// Synthetic original producer retained by every publication in this fixture.
pub(crate) fn source_origin() -> ReplaySourceOrigin {
    ReplaySourceOrigin::new("original-turn", "original-agent", "original-pane").unwrap()
}

pub(crate) const CONVERSATION: &str = "550e8400-e29b-41d4-a716-446655440000";
pub(crate) const OLD_PROMPT: &str = "original exact task instruction";
pub(crate) const SAVED_POLICY: &str = "SAVED_POLICY_MUST_NEVER_REPLACE_CURRENT_PROMPT";

/// Owns only a fresh physical test directory and synthetic immutable source.
pub(crate) struct Fixture {
    pub(crate) root: PathBuf,
    pub(crate) store: AgentTranscriptStore,
    pub(crate) source: ReplayEpochContext,
    pub(crate) identity: ReplayEpochIdentity,
    pub(crate) instruction: ContextEventSequence,
    pub(crate) group: ContextExecutionGroupId,
    pub(crate) covered: Vec<mez_agent::ConversationEvent>,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        Self::with_instruction(OLD_PROMPT)
    }

    /// Controls use a longer original task to exercise real configured raw-tail
    /// retention without invalid test-only percentages or tiny model windows.
    pub(crate) fn with_instruction(instruction_text: &str) -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "mez-runtime-epoch-{}",
            String::from(mez_agent::ContextBlockOccurrenceId::allocate())
        ));
        let store = AgentTranscriptStore::new(&root);
        let prefix = || {
            AgentContext::new(vec![ContextBlock::stable_instruction(
                ContextSourceKind::System,
                "policy",
                SAVED_POLICY,
            )])
            .unwrap()
        };
        let mut source =
            ReplayEpochContext::new(ReplayConversationId::new(CONVERSATION).unwrap(), prefix())
                .unwrap();
        let group = ContextExecutionGroupId::new("original-native-group").unwrap();
        let mut parent = None;
        let mut instruction = None;
        let mut covered = Vec::new();
        for revision in 1..=3 {
            let closed =
                ContextExecutionGroupId::new(format!("closed-fixture-{revision}")).unwrap();
            let sequence = source
                .append_execution_events([ContextConversationAppend::assistant(
                    "source",
                    format!("COVERED_SOURCE_SENTINEL_{revision}"),
                    closed.clone(),
                )])
                .unwrap()[0];
            let capture = source.capture_execution_owner(&closed, sequence).unwrap();
            source.finish_execution_group(&capture).unwrap();
            covered.push(source.context().chronology().last().unwrap().clone());
            let plan = match source
                .prepare_compaction(ReplayTailPolicy::new(1000, 0).unwrap())
                .unwrap()
            {
                ReplayCompactionPreparation::Ready(plan) => plan,
                other => panic!("expected source, got {other:?}"),
            };
            if revision == 3 {
                let task = source.begin_instruction_task().unwrap();
                instruction = Some(
                    source
                        .append_instruction(
                            &task,
                            ContextBlock::user_event("user prompt", instruction_text),
                        )
                        .unwrap(),
                );
                let provider = ProviderContinuityOwner::new(
                    ProviderApiCompatibility::OpenAiChatCompletions,
                    "original-provider",
                )
                .unwrap();
                let call = ProviderTranscriptEvent::validated_openai_chat_completions_assistant_tool_call(
                    "original-provider".into(), "original declaration".into(),
                    vec![serde_json::json!({"id":"original-call", "type":"function", "function":{"name":mez_agent::MAAP_ACTION_BATCH_TOOL_NAME, "arguments":"{}"}})],
                ).unwrap();
                source
                    .append_execution_events([ContextConversationAppend::provider_assistant(
                        "original native declaration",
                        call.to_transcript_content(),
                        group.clone(),
                        provider,
                    )])
                    .unwrap();
            }
            let proposal = store
                .prepare_replay_epoch(
                    source_origin().capture(None),
                    CONVERSATION,
                    parent,
                    &source,
                    plan,
                    ReplayHeadPublication::Summary(format!("model head {revision}")),
                )
                .unwrap();
            store.publish_replay_epoch(&proposal).unwrap();
            let (identity, restored) = store
                .load_replay_epoch(CONVERSATION, prefix())
                .unwrap()
                .unwrap();
            parent = Some(identity);
            source = restored;
        }
        Self {
            root,
            store,
            source,
            identity: parent.unwrap(),
            instruction: instruction.unwrap(),
            covered,
            group,
        }
    }
    /// Produces actual accounted ordinary rows for original source, plus ordinary
    /// display/system rows that the existing renderer must prove contribute zero
    /// semantic blocks. No append is claimed until the real store accepts them.
    pub(crate) fn delayed_original_rows(&self) -> Vec<TranscriptEntry> {
        let row = |role, content: String| TranscriptEntry {
            conversation_id: CONVERSATION.into(),
            sequence: 1,
            created_at_unix_seconds: 123,
            role,
            turn_id: "original-turn".into(),
            agent_id: "original-agent".into(),
            pane_id: "original-pane".into(),
            content,
        };
        let mut rows = vec![
            row(TranscriptRole::System, "cwd=/synthetic/original".into()),
            row(
                TranscriptRole::Assistant,
                "DISPLAY_COPY_MUST_NOT_REENTER".into(),
            ),
            row(TranscriptRole::Tool, "DISPLAY_TOOL_MUST_NOT_REENTER".into()),
        ];
        for event in &self.covered {
            let semantic = TranscriptContextEvent::execution_block_with_metadata(
                event.block().source,
                event.block().label.clone(),
                event.block().content.clone(),
                event.execution_group_id().unwrap().clone(),
                1,
                event.provider_owner().cloned(),
            )
            .unwrap();
            rows.push(row(
                TranscriptRole::System,
                TranscriptContextRecord::new(semantic, event.record(), None)
                    .unwrap()
                    .to_transcript_content()
                    .unwrap(),
            ));
        }
        let user = &self.source.context().chronology()[1];
        rows.push(row(
            TranscriptRole::System,
            TranscriptContextRecord::new(
                TranscriptContextEvent::user_event(
                    user.sequence().get(),
                    user.block().label.clone(),
                    user.block().content.clone(),
                )
                .unwrap(),
                user.record(),
                Some(Vec::new()),
            )
            .unwrap()
            .to_transcript_content()
            .unwrap(),
        ));
        let native = &self.source.context().chronology()[2];
        rows.push(row(
            TranscriptRole::System,
            TranscriptContextRecord::new(
                TranscriptContextEvent::execution_block_with_metadata(
                    native.block().source,
                    native.block().label.clone(),
                    native.block().content.clone(),
                    self.group.clone(),
                    1,
                    native.provider_owner().cloned(),
                )
                .unwrap(),
                native.record(),
                None,
            )
            .unwrap()
            .to_transcript_content()
            .unwrap(),
        ));
        for (index, row) in rows.iter_mut().enumerate() {
            row.sequence = index as u64 + 1;
        }
        rows
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
