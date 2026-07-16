//! Agent tests for provider contract behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies model provider trait returns model response.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn model_provider_trait_returns_model_response() {
    let request = assemble_model_request(
        &ModelProfile {
            provider: "echo".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();

    let response = EchoProvider.send_request(&request).unwrap();

    assert_eq!(response.provider, "echo");
    assert_eq!(response.model, "test");
    assert_eq!(response.raw_text, "ok");
}
