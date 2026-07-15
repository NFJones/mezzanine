//! Agent Prompt implementation.
//!
//! This module owns the agent prompt boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use include_dir::{Dir, include_dir};

use mez_agent::{
    AgentPromptAssetSource, AgentPromptError, AgentPromptProfile, AgentPromptResult,
    assemble_agent_system_prompt,
};

/// Embedded static system-prompt fragments owned by this module.
static SYSTEM_PROMPTS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/agent/prompt/system");

/// Embedded provider-specific prompt fragments owned by this module.
static PROVIDER_PROMPTS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/agent/prompt/providers");

/// Product adapter that resolves lower assembly requests from embedded assets.
struct EmbeddedPromptAssets;

impl AgentPromptAssetSource for EmbeddedPromptAssets {
    /// Returns one embedded system fragment for lower prompt assembly.
    fn system_fragment<'a>(&'a self, path: &str) -> AgentPromptResult<&'a str> {
        system_prompt_fragment(path)
    }

    /// Returns one embedded provider fragment for lower prompt assembly.
    fn provider_fragment<'a>(&'a self, path: &str) -> AgentPromptResult<&'a str> {
        provider_prompt_fragment(path)
    }
}

/// Runs the build agent system prompt operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn build_agent_system_prompt(profile: &AgentPromptProfile) -> AgentPromptResult<String> {
    build_agent_system_prompt_with_repository_instructions(profile, &[])
}

/// Builds the provider-facing system prompt with active repository guidance.
///
/// # Parameters
/// - `profile`: The agent prompt profile that supplies pane, permission, and MCP
///   context.
/// - `repository_instruction_blocks`: The already discovered repository
///   instruction contents to embed directly into the system prompt.
pub fn build_agent_system_prompt_with_repository_instructions(
    profile: &AgentPromptProfile,
    repository_instruction_blocks: &[String],
) -> AgentPromptResult<String> {
    assemble_agent_system_prompt(
        profile,
        repository_instruction_blocks,
        &EmbeddedPromptAssets,
    )
}

/// Returns one required system prompt fragment from the embedded asset tree.
pub(super) fn system_prompt_fragment(path: &str) -> AgentPromptResult<&'static str> {
    embedded_prompt_fragment(&SYSTEM_PROMPTS, "system", path)
}

/// Returns one required provider prompt fragment from the embedded asset tree.
pub(super) fn provider_prompt_fragment(path: &str) -> AgentPromptResult<&'static str> {
    embedded_prompt_fragment(&PROVIDER_PROMPTS, "provider", path)
}

/// Looks up one UTF-8 prompt asset by explicit file name.
fn embedded_prompt_fragment(
    dir: &'static Dir<'static>,
    kind: &str,
    path: &str,
) -> AgentPromptResult<&'static str> {
    let file = dir.get_file(path).ok_or_else(|| {
        AgentPromptError::invalid_state(format!(
            "embedded {kind} prompt fragment `{path}` is missing"
        ))
    })?;
    let contents = file.contents_utf8().ok_or_else(|| {
        AgentPromptError::invalid_state(format!(
            "embedded {kind} prompt fragment `{path}` is not UTF-8"
        ))
    })?;
    Ok(contents.strip_suffix('\n').unwrap_or(contents))
}
