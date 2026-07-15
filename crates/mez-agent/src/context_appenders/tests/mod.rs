//! Direct context-appender policy tests.
//!
//! Guidance and integration leaves preserve the exact regression names moved
//! from the product crate. Synthetic prompt assets keep combined request
//! assembly cases independent from Mezzanine's embedded Markdown files.

use super::*;
use crate::instructions::DiscoveredInstructionFile;
use crate::{
    AgentPromptAssetSource, AgentPromptResult, MemoryContextRecord, MemoryContextScope,
    ModelMessageRole, ModelProfile, ModelRequest, ModelRequestIdentity,
    assemble_model_request_from_context,
};

mod guidance;
mod integrations;

/// Synthetic prompt source for appender tests that include request assembly.
struct TestPromptAssets;

impl AgentPromptAssetSource for TestPromptAssets {
    fn system_fragment<'a>(&'a self, path: &str) -> AgentPromptResult<&'a str> {
        Ok(match path {
            "identity.md" => "profile {profile_name} version {profile_version}",
            "repository_instructions.md" => {
                "Embedded active repository instruction contents:\n{repository_instructions}"
            }
            "subagents.md" => "subagent contract",
            "mcp.md" => "mcp contract",
            _ => "generic contract",
        })
    }

    fn provider_fragment<'a>(&'a self, _path: &str) -> AgentPromptResult<&'a str> {
        Ok("provider contract")
    }
}

/// Assembles one OpenAI request around lower-owned appender output.
fn assemble_test_model_request(context: &AgentContext) -> ModelRequest {
    assemble_model_request_from_context(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "default".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        ModelRequestIdentity {
            turn_id: "turn-1",
            agent_id: "agent-1",
            pane_id: "%1",
        },
        context,
        &TestPromptAssets,
    )
    .unwrap()
}
