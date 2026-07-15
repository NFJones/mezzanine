//! Agent Prompt implementation.
//!
//! This module owns the agent prompt boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use include_dir::{Dir, include_dir};

use mez_agent::{
    AGENT_PROMPT_PROFILE_NAME, AGENT_PROMPT_PROFILE_VERSION, AgentPromptError, AgentPromptProfile,
    AgentPromptResult, validate_agent_prompt_required,
};

/// Embedded static system-prompt fragments owned by this module.
static SYSTEM_PROMPTS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/agent/prompt/system");

/// Embedded provider-specific prompt fragments owned by this module.
static PROVIDER_PROMPTS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/agent/prompt/providers");

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
    validate_agent_prompt_required("agent id", &profile.agent_id)?;
    validate_agent_prompt_required("pane id", &profile.pane_id)?;

    let mut prompt = String::new();
    push_section(&mut prompt, "1. Identity", &identity_prompt(profile)?);
    push_section(
        &mut prompt,
        "2. Autonomy",
        system_prompt_fragment("autonomy.md")?,
    );
    push_section(
        &mut prompt,
        "3. Repository Instructions",
        &repository_instructions_prompt(repository_instruction_blocks)?,
    );
    push_section(
        &mut prompt,
        "4. Personality",
        system_prompt_fragment("personality.md")?,
    );
    push_section(
        &mut prompt,
        "5. Judgment",
        system_prompt_fragment("judgment.md")?,
    );
    push_section(
        &mut prompt,
        "6. Actions",
        system_prompt_fragment("actions.md")?,
    );
    push_section(&mut prompt, "7. Edits", system_prompt_fragment("edits.md")?);
    push_section(
        &mut prompt,
        "8. Validation",
        system_prompt_fragment("validation.md")?,
    );
    push_section(&mut prompt, "9. Trust", system_prompt_fragment("trust.md")?);
    push_section(&mut prompt, "10. Subagents", &subagent_prompt(profile)?);
    push_section(
        &mut prompt,
        "11. Runtime",
        system_prompt_fragment("runtime.md")?,
    );
    push_section(
        &mut prompt,
        "12. Communication",
        system_prompt_fragment("communication.md")?,
    );
    push_section(
        &mut prompt,
        "13. Format",
        system_prompt_fragment("format.md")?,
    );
    push_section(&mut prompt, "14. MCP", &mcp_prompt(profile)?);
    if profile.provider.as_deref() == Some("deepseek") {
        push_section(
            &mut prompt,
            "15. DeepSeek Provider",
            provider_prompt_fragment("deepseek.md")?,
        );
    }
    if profile.provider.as_deref() == Some("anthropic") {
        push_section(
            &mut prompt,
            "15. Anthropic Provider",
            provider_prompt_fragment("anthropic.md")?,
        );
    }
    if profile.provider.as_deref() == Some("claude-code") {
        push_section(
            &mut prompt,
            "15. Claude Code Provider",
            provider_prompt_fragment("claude_code.md")?,
        );
    }
    Ok(prompt)
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

/// Builds the persona and scope section of the provider-facing system prompt.
pub(super) fn identity_prompt(_profile: &AgentPromptProfile) -> AgentPromptResult<String> {
    Ok(system_prompt_fragment("identity.md")?
        .replace("{profile_name}", AGENT_PROMPT_PROFILE_NAME)
        .replace(
            "{profile_version}",
            &AGENT_PROMPT_PROFILE_VERSION.to_string(),
        ))
}

/// Builds the repository-instruction section of the provider-facing prompt.
pub(super) fn repository_instructions_prompt(
    repository_instruction_blocks: &[String],
) -> AgentPromptResult<String> {
    let mut prompt = system_prompt_fragment("repository_instructions.md")?.to_string();
    if !repository_instruction_blocks.is_empty() {
        prompt.push_str("\n\nEmbedded active repository instruction contents:");
        for block in repository_instruction_blocks {
            prompt.push_str("\n\n");
            prompt.push_str(block);
        }
    }
    Ok(prompt)
}

/// Runs the push section operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn push_section(prompt: &mut String, title: &str, body: &str) {
    if !prompt.is_empty() {
        prompt.push_str("\n\n");
    }
    prompt.push_str(title);
    prompt.push('\n');
    prompt.push_str(body);
}

/// Runs the subagent prompt operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn subagent_prompt(profile: &AgentPromptProfile) -> AgentPromptResult<String> {
    let mut lines = vec![system_prompt_fragment("subagents.md")?.to_string()];
    if let Some(mode) = &profile.cooperation_mode {
        lines.push(format!(
            "Subagent scope: cooperation_mode={mode}; Read scopes: {}; Write scopes: {}.",
            list_or_none(&profile.read_scopes),
            list_or_none(&profile.write_scopes)
        ));
    }
    Ok(lines.join(" "))
}

/// Runs the mcp prompt operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mcp_prompt(profile: &AgentPromptProfile) -> AgentPromptResult<String> {
    let _ = profile;
    Ok(system_prompt_fragment("mcp.md")?.to_string())
}

/// Runs the list or none operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn list_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(", ")
    }
}
