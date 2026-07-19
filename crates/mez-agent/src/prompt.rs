//! Provider-neutral system-prompt assembly.
//!
//! This module owns prompt profiles, deterministic section ordering, repository
//! guidance embedding, provider selection, and subagent scope formatting.
//! Product-owned Markdown assets are supplied through a narrow source port by
//! the composition crate.

use std::fmt;

/// Result type returned by provider-neutral prompt assembly contracts.
pub type AgentPromptResult<T> = Result<T, AgentPromptError>;

/// Stable categories for agent prompt assembly failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPromptErrorKind {
    /// A required provider-neutral prompt input was missing or malformed.
    InvalidArgs,
    /// A product-owned prompt asset was unavailable or invalid.
    InvalidState,
}

/// A typed failure returned while validating or assembling an agent prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPromptError {
    kind: AgentPromptErrorKind,
    message: String,
}

impl AgentPromptError {
    /// Creates an invalid-argument prompt failure.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            kind: AgentPromptErrorKind::InvalidArgs,
            message: message.into(),
        }
    }

    /// Creates an invalid-state prompt failure.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self {
            kind: AgentPromptErrorKind::InvalidState,
            message: message.into(),
        }
    }

    /// Returns the stable failure category.
    pub fn kind(&self) -> AgentPromptErrorKind {
        self.kind
    }

    /// Returns the diagnostic message without formatting the error.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentPromptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AgentPromptError {}

/// Validates one required prompt-profile field after trimming whitespace.
pub fn validate_agent_prompt_required(field: &str, value: &str) -> AgentPromptResult<()> {
    if value.trim().is_empty() {
        return Err(AgentPromptError::invalid_args(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

/// Stable name of the default agent prompt profile.
pub const AGENT_PROMPT_PROFILE_NAME: &str = "default";

/// Current version of the default agent prompt profile.
pub const AGENT_PROMPT_PROFILE_VERSION: u32 = 32;

/// Provider-neutral state used to assemble one agent system prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPromptProfile {
    /// Stable agent identifier included in the prompt.
    pub agent_id: String,
    /// Stable pane identifier included in the prompt.
    pub pane_id: String,
    /// Optional provider kind used for provider-specific guidance.
    pub provider: Option<String>,
    /// Optional subagent cooperation mode.
    pub cooperation_mode: Option<String>,
    /// Declared read scopes for a subagent.
    pub read_scopes: Vec<String>,
    /// Declared write scopes for a subagent.
    pub write_scopes: Vec<String>,
}

impl AgentPromptProfile {
    /// Creates the default prompt profile for one agent and pane.
    pub fn default_for(agent_id: impl Into<String>, pane_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            pane_id: pane_id.into(),
            provider: None,
            cooperation_mode: None,
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
        }
    }

    /// Sets the provider kind used for provider-specific prompt guidance.
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }
}

/// Supplies product-owned prompt fragments to provider-neutral assembly.
pub trait AgentPromptAssetSource {
    /// Returns one required system-prompt fragment by stable file name.
    fn system_fragment<'a>(&'a self, path: &str) -> AgentPromptResult<&'a str>;

    /// Returns one required provider-specific fragment by stable file name.
    fn provider_fragment<'a>(&'a self, path: &str) -> AgentPromptResult<&'a str>;
}

/// Assembles one provider-facing system prompt from injected product assets.
///
/// Repository instruction blocks must already be discovered and ordered by the
/// product adapter. This function appends them verbatim after invariant policy
/// without performing filesystem access.
pub fn assemble_agent_system_prompt(
    profile: &AgentPromptProfile,
    repository_instruction_blocks: &[String],
    assets: &impl AgentPromptAssetSource,
) -> AgentPromptResult<String> {
    validate_agent_prompt_required("agent id", &profile.agent_id)?;
    validate_agent_prompt_required("pane id", &profile.pane_id)?;

    let mut prompt = String::new();
    push_section(&mut prompt, "1. Identity", &identity_prompt(assets)?);
    push_section(
        &mut prompt,
        "2. Autonomy",
        assets.system_fragment("autonomy.md")?,
    );
    push_section(
        &mut prompt,
        "3. Repository Instructions",
        assets.system_fragment("repository_instructions.md")?,
    );
    push_section(
        &mut prompt,
        "4. Personality",
        assets.system_fragment("personality.md")?,
    );
    push_section(
        &mut prompt,
        "5. Judgment",
        assets.system_fragment("judgment.md")?,
    );
    push_section(
        &mut prompt,
        "6. Actions",
        assets.system_fragment("actions.md")?,
    );
    push_section(&mut prompt, "7. Edits", assets.system_fragment("edits.md")?);
    push_section(
        &mut prompt,
        "8. Validation",
        assets.system_fragment("validation.md")?,
    );
    push_section(&mut prompt, "9. Trust", assets.system_fragment("trust.md")?);
    push_section(
        &mut prompt,
        "10. Subagents",
        &subagent_prompt(profile, assets)?,
    );
    push_section(
        &mut prompt,
        "11. Runtime",
        assets.system_fragment("runtime.md")?,
    );
    push_section(
        &mut prompt,
        "12. Communication",
        assets.system_fragment("communication.md")?,
    );
    push_section(
        &mut prompt,
        "13. Format",
        assets.system_fragment("format.md")?,
    );
    push_section(&mut prompt, "14. MCP", assets.system_fragment("mcp.md")?);
    let provider_fragment = match profile.provider.as_deref() {
        Some("deepseek") => Some(("15. DeepSeek Provider", "deepseek.md")),
        Some("anthropic") => Some(("15. Anthropic Provider", "anthropic.md")),
        _ => None,
    };
    if let Some((title, path)) = provider_fragment {
        push_section(&mut prompt, title, assets.provider_fragment(path)?);
    }
    append_repository_instructions(&mut prompt, repository_instruction_blocks);
    Ok(prompt)
}

/// Builds the templated identity section.
fn identity_prompt(assets: &impl AgentPromptAssetSource) -> AgentPromptResult<String> {
    Ok(assets
        .system_fragment("identity.md")?
        .replace("{profile_name}", AGENT_PROMPT_PROFILE_NAME)
        .replace(
            "{profile_version}",
            &AGENT_PROMPT_PROFILE_VERSION.to_string(),
        ))
}

/// Appends active repository contents after all invariant prompt policy.
fn append_repository_instructions(prompt: &mut String, repository_instruction_blocks: &[String]) {
    if repository_instruction_blocks.is_empty() {
        return;
    }
    prompt.push_str("\n\nActive Repository Instructions\n");
    prompt.push_str("Embedded active repository instruction contents:");
    for block in repository_instruction_blocks {
        prompt.push_str("\n\n");
        prompt.push_str(block);
    }
}

/// Builds the subagent section with optional scope details.
fn subagent_prompt(
    profile: &AgentPromptProfile,
    assets: &impl AgentPromptAssetSource,
) -> AgentPromptResult<String> {
    let mut lines = vec![assets.system_fragment("subagents.md")?.to_string()];
    if let Some(mode) = &profile.cooperation_mode {
        lines.push(format!(
            "Subagent scope: cooperation_mode={mode}; Read scopes: {}; Write scopes: {}.",
            list_or_none(&profile.read_scopes),
            list_or_none(&profile.write_scopes)
        ));
    }
    Ok(lines.join(" "))
}

/// Appends one numbered section with stable blank-line separation.
fn push_section(prompt: &mut String, title: &str, body: &str) {
    if !prompt.is_empty() {
        prompt.push_str("\n\n");
    }
    prompt.push_str(title);
    prompt.push('\n');
    prompt.push_str(body);
}

/// Formats a scope list without exposing an empty field.
fn list_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentPromptAssetSource, AgentPromptError, AgentPromptErrorKind, AgentPromptProfile,
        AgentPromptResult, assemble_agent_system_prompt, validate_agent_prompt_required,
    };

    /// Synthetic prompt assets used to test deterministic assembly without
    /// depending on the product crate's embedded Markdown files.
    struct TestPromptAssets;

    impl AgentPromptAssetSource for TestPromptAssets {
        fn system_fragment<'a>(&'a self, path: &str) -> AgentPromptResult<&'a str> {
            Ok(match path {
                "identity.md" => "profile {profile_name} version {profile_version}",
                "repository_instructions.md" => "repository contract",
                "subagents.md" => "subagent contract",
                "mcp.md" => "mcp contract",
                _ => "generic system contract",
            })
        }

        fn provider_fragment<'a>(&'a self, path: &str) -> AgentPromptResult<&'a str> {
            Ok(match path {
                "anthropic.md" => "anthropic contract",
                "deepseek.md" => "deepseek contract",
                _ => {
                    return Err(AgentPromptError::invalid_state(
                        "unknown test provider asset",
                    ));
                }
            })
        }
    }

    /// Verifies provider-neutral assembly preserves section ordering, profile
    /// templating, and verbatim repository guidance injection.
    #[test]
    fn prompt_assembly_injects_assets_and_repository_guidance() {
        let prompt = assemble_agent_system_prompt(
            &AgentPromptProfile::default_for("agent-1", "%1"),
            &[
                "first repository rule".to_string(),
                "second rule".to_string(),
            ],
            &TestPromptAssets,
        )
        .unwrap();

        assert!(prompt.starts_with("1. Identity\nprofile default version 32"));
        assert!(prompt.contains("3. Repository Instructions\nrepository contract"));
        assert!(prompt.contains("Embedded active repository instruction contents:"));
        assert!(prompt.contains("first repository rule\n\nsecond rule"));
        let repository_contract = prompt.find("3. Repository Instructions").unwrap();
        let mcp_policy = prompt.find("14. MCP").unwrap();
        let active_repository = prompt.find("Active Repository Instructions").unwrap();
        assert!(repository_contract < mcp_policy);
        assert!(mcp_policy < active_repository);
        assert!(!prompt.contains("15. "));
    }

    /// Verifies provider and subagent profile fields select only the requested
    /// guidance and format empty and populated scopes deterministically.
    #[test]
    fn prompt_assembly_selects_provider_and_subagent_scope() {
        let mut profile =
            AgentPromptProfile::default_for("agent-1", "%1").with_provider("anthropic");
        profile.cooperation_mode = Some("isolated".to_string());
        profile.read_scopes = vec!["src".to_string()];

        let prompt = assemble_agent_system_prompt(&profile, &[], &TestPromptAssets).unwrap();

        assert!(prompt.contains("10. Subagents\nsubagent contract Subagent scope:"));
        assert!(prompt.contains("cooperation_mode=isolated"));
        assert!(prompt.contains("Read scopes: src; Write scopes: none."));
        assert!(prompt.contains("15. Anthropic Provider\nanthropic contract"));
        assert!(!prompt.contains("deepseek contract"));
    }

    #[test]
    /// Verifies a default profile contains only dependency-neutral prompt state.
    fn prompt_profile_defaults_are_dependency_neutral() {
        let profile = AgentPromptProfile::default_for("agent-1", "%1");

        assert_eq!(profile.agent_id, "agent-1");
        assert_eq!(profile.pane_id, "%1");
        assert_eq!(profile.provider, None);
        assert!(profile.read_scopes.is_empty());
        assert!(profile.write_scopes.is_empty());
    }

    #[test]
    /// Verifies builder methods preserve identity while replacing provider context.
    fn prompt_profile_builders_preserve_identity() {
        let profile = AgentPromptProfile::default_for("agent-1", "%1").with_provider("anthropic");

        assert_eq!(profile.agent_id, "agent-1");
        assert_eq!(profile.pane_id, "%1");
        assert_eq!(profile.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    /// Verifies required prompt identity fields reject whitespace while prompt
    /// asset failures retain their distinct invalid-state category.
    fn prompt_errors_preserve_validation_and_asset_categories() {
        let error = validate_agent_prompt_required("agent id", " \t ").unwrap_err();
        assert_eq!(error.kind(), AgentPromptErrorKind::InvalidArgs);
        assert_eq!(error.message(), "agent id must not be empty");

        let error = AgentPromptError::invalid_state("prompt asset is missing");
        assert_eq!(error.kind(), AgentPromptErrorKind::InvalidState);
    }
}
