//! Product adapter coverage for model-request assembly.
//!
//! Provider-independent request shaping runs in `mez-agent`. This leaf verifies
//! the remaining root adapter projects product turn identity and supplies the
//! embedded system-prompt asset source.

use super::*;

#[test]
/// Verifies root request assembly adapts turn identity and embedded assets.
///
/// The product boundary must preserve runtime identifiers while supplying a
/// nonempty embedded system prompt to lower request assembly.
fn model_request_assembly_adapts_product_turn_and_prompt_assets() {
    let turn = turn();
    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "default".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn,
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "user".to_string(),
            content: "continue".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();

    assert_eq!(request.turn_id, turn.turn_id);
    assert_eq!(request.agent_id, turn.agent_id);
    assert_eq!(request.messages[0].role, ModelMessageRole::System);
    assert!(request.messages[0].content.contains("Mezzanine"));
}
