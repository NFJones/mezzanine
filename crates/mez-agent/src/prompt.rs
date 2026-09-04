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

/// Model identity used to assemble one agent system prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPromptProfile {
    /// Selected provider model name. This is the only variable profile value
    /// included in the durable system-prompt prefix.
    pub model: String,
}

impl AgentPromptProfile {
    /// Creates a prompt profile for the selected model.
    pub fn for_model(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
        }
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
    validate_agent_prompt_required("model", &profile.model)?;

    let mut prompt = String::new();
    push_section(
        &mut prompt,
        "1. Identity",
        &identity_prompt(profile, assets)?,
    );
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
        assets.system_fragment("subagents.md")?,
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
    append_repository_instructions(&mut prompt, repository_instruction_blocks);
    Ok(prompt)
}

/// Builds the templated identity section.
fn identity_prompt(
    profile: &AgentPromptProfile,
    assets: &impl AgentPromptAssetSource,
) -> AgentPromptResult<String> {
    Ok(assets
        .system_fragment("identity.md")?
        .replace("{profile_name}", AGENT_PROMPT_PROFILE_NAME)
        .replace(
            "{profile_version}",
            &AGENT_PROMPT_PROFILE_VERSION.to_string(),
        )
        .replace("{model}", &profile.model))
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

/// Appends one numbered section with stable blank-line separation.
fn push_section(prompt: &mut String, title: &str, body: &str) {
    if !prompt.is_empty() {
        prompt.push_str("\n\n");
    }
    prompt.push_str(title);
    prompt.push('\n');
    prompt.push_str(body);
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
                "identity.md" => "profile {profile_name} version {profile_version} model {model}",
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
            &AgentPromptProfile::for_model("test-model"),
            &[
                "first repository rule".to_string(),
                "second rule".to_string(),
            ],
            &TestPromptAssets,
        )
        .unwrap();

        assert!(prompt.starts_with("1. Identity\nprofile default version 32 model test-model"));
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

    /// Verifies model identity is the only variable prompt-profile field and
    /// provider or subagent state cannot alter the durable prompt shape.
    #[test]
    fn prompt_assembly_varies_only_by_model() {
        let first = assemble_agent_system_prompt(
            &AgentPromptProfile::for_model("model-a"),
            &[],
            &TestPromptAssets,
        )
        .unwrap();
        let second = assemble_agent_system_prompt(
            &AgentPromptProfile::for_model("model-b"),
            &[],
            &TestPromptAssets,
        )
        .unwrap();

        assert_eq!(first.replace("model-a", "model-b"), second);
        assert!(first.contains("10. Subagents\nsubagent contract"));
        assert!(!first.contains("Subagent scope:"));
        assert!(!first.contains("anthropic contract"));
        assert!(!first.contains("deepseek contract"));
    }

    #[test]
    /// Verifies the prompt profile contains only the selected model identity.
    fn prompt_profile_contains_only_model_identity() {
        let profile = AgentPromptProfile::for_model("model-a");

        assert_eq!(profile.model, "model-a");
    }

    #[test]
    /// Verifies required prompt identity fields reject whitespace while prompt
    /// asset failures retain their distinct invalid-state category.
    fn prompt_errors_preserve_validation_and_asset_categories() {
        let error = validate_agent_prompt_required("model", " \t ").unwrap_err();
        assert_eq!(error.kind(), AgentPromptErrorKind::InvalidArgs);
        assert_eq!(error.message(), "model must not be empty");

        let error = AgentPromptError::invalid_state("prompt asset is missing");
        assert_eq!(error.kind(), AgentPromptErrorKind::InvalidState);
    }
}
