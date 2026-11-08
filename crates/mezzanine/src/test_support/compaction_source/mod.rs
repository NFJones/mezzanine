//! Real completed-task source shared by runtime recovery and actor lease tests.
//!
//! A deterministic offline provider completes the original admitted task. Only
//! the actual successor admission retires its instruction and makes it eligible
//! for compaction. No fixture inserts closed flags, execution-owner witnesses,
//! publication acknowledgements, or replacement context into a running turn.

use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
use crate::integrations::agent::provider::ModelProvider;
use crate::runtime::RuntimeSessionService;
use crate::storage::transcript::AgentTranscriptStore;
use mez_agent::{AgentTurnState, ContextRetention, ContextSourceKind};
use mez_mux::layout::Size;
use std::path::PathBuf;

mod model_work;
pub(crate) use model_work::{
    execute_wave, execute_with_inference, model_result_event, rendered_source,
};

/// Completes a historical task and admits its successor under a real 40k-token
/// profile. The caller owns the service, isolated store location and any later
/// compaction/lease/publication assertions. No provider network is contacted.
pub(crate) fn recovery_source(
    mut service: RuntimeSessionService,
    root: PathBuf,
    historical_prompt: &str,
    answer: &str,
    retained_percent: u8,
) -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let store = AgentTranscriptStore::new(root);
    service.set_agent_transcript_store(store.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "provider-context-limit-recovery".into(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                r#"[agents]
default_provider = "runtime-batch"
default_model_profile = "provider-context-limit-test"
shell_mode = "pane"
compaction_raw_retention_percent = {retained_percent}
[providers.runtime-batch]
kind = "openai"
models = ["test"]
default_model = "test"
[model_profiles.provider-context-limit-test]
provider = "runtime-batch"
model = "test"
context_window_tokens = 40000
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
    let turn = complete_task_and_admit_successor(
        &mut service,
        &primary,
        historical_prompt,
        answer,
        "continue with the large observation",
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
    );
    (service, store, turn)
}

/// Uses an already configured, bound service and its original provider profile.
/// HTTP tests can retain their own loopback endpoint and limits while obtaining
/// real task completion and successor admission without extra network requests.
/// `api` is the caller's configured wire contract, not a guess from its label.
pub(crate) fn complete_task_and_admit_successor(
    service: &mut RuntimeSessionService,
    primary: &mez_core::ids::ClientId,
    historical_prompt: &str,
    answer: &str,
    next_prompt: &str,
    api: mez_agent::ProviderApiCompatibility,
) -> String {
    let start = service
        .execute_agent_shell_command(primary, historical_prompt)
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let predecessor = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .clone()
        .unwrap();
    let original_root = service.agent_turn_contexts()[&predecessor]
        .chronology()
        .iter()
        .find(|event| event.block().source == ContextSourceKind::UserInstruction)
        .unwrap()
        .record();
    complete_task(service, &predecessor, answer, api);
    let start = service
        .execute_agent_shell_command(primary, next_prompt)
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let turn = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .running_turn_id
        .clone()
        .unwrap();
    let context = &service.agent_turn_contexts()[&turn];
    assert!(context.has_replay_owner());
    assert_eq!(
        context
            .chronology()
            .iter()
            .find(|event| event.record() == original_root)
            .unwrap()
            .retention(),
        ContextRetention::Summarizable
    );
    turn
}

/// Completes an actually admitted fixture task through its original profile
/// and the normal synchronous provider/execution/persistence owners. Manual
/// tests use this to become idle without discarding the last exact user root.
pub(crate) fn complete_task(
    service: &mut RuntimeSessionService,
    turn: &str,
    answer: &str,
    api: mez_agent::ProviderApiCompatibility,
) {
    let profile = service.agent_turn_model_profile(turn).unwrap().clone();
    let provider = FinalAnswer {
        provider: profile.provider.clone(),
        answer,
        api,
    };
    assert_eq!(
        service
            .execute_agent_turn_with_provider(turn, &provider, profile)
            .unwrap()
            .terminal_state,
        AgentTurnState::Completed
    );
}

/// A normal final `say` response exercises the real provider execution owner.
/// It requests no executable capability and performs no external model call.
struct FinalAnswer<'a> {
    provider: String,
    answer: &'a str,
    api: mez_agent::ProviderApiCompatibility,
}
impl ModelProvider for FinalAnswer<'_> {
    fn provider_id(&self) -> &str {
        &self.provider
    }

    fn api_compatibility(&self) -> mez_agent::ProviderApiCompatibility {
        self.api
    }

    fn send_request(
        &self,
        request: &mez_agent::ModelRequest,
    ) -> crate::Result<mez_agent::ModelResponse> {
        Ok(mez_agent::ModelResponse {
            provider: self.provider_id().into(),
            model: request.model.clone(),
            raw_text: self.answer.into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            provider_transcript_events: Vec::new(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "complete original fixture task".into(),
                actions: vec![mez_agent::AgentAction {
                    id: "say-1".into(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: self.answer.into(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
                    },
                }],
            }),
        })
    }
}
