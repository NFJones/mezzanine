//! Shared synthetic durable epoch for history-import and real prompt tests.
//! Three real publications leave sparse original sequences, protected user and
//! native source, no ordinary archive rows, and no provider/action dispatch.

use crate::storage::transcript::{
    AgentTranscriptStore,
    replay_epoch::{ReplayEpochIdentity, ReplayEpochPublication},
};
use mez_agent::{
    AgentContext, ContextBlock, ContextConversationAppend, ContextEventSequence,
    ContextExecutionGroupId, ContextSourceKind, ProviderApiCompatibility, ProviderContinuityOwner,
    ProviderTranscriptEvent, ReplayCompactionPreparation, ReplayConversationId, ReplayEpochContext,
    ReplayHeadPublication, ReplayTailPolicy,
};
use std::path::PathBuf;

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
}

impl Fixture {
    pub(crate) fn new() -> Self {
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
        for revision in 1..=3 {
            source
                .append_fact("source", format!("COVERED_SOURCE_SENTINEL_{revision}"))
                .unwrap();
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
                            ContextBlock::user_event("user prompt", OLD_PROMPT),
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
            let proposal = ReplayEpochPublication::prepare(
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
            group,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
