//! Provider-independent agent skill contracts and parsing.
//!
//! This module owns skill identities, deterministic catalog precedence,
//! model-facing catalog projection, `SKILL.md` metadata parsing, loaded-skill
//! context formatting, and explicit `$skill` invocation syntax. Filesystem
//! discovery, embedded product assets, managed-copy synchronization, and
//! product error projection remain in the composition crate.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

use crate::maap::{AgentAction, AgentActionPayload, is_valid_skill_name};
use crate::{ActionResult, ActionStatus, AgentTurnResultIdentity, ContextBlock, ContextSourceKind};

/// File name that carries one skill's metadata and markdown instructions.
pub const SKILL_FILE_NAME: &str = "SKILL.md";
/// Markdown heading used when caller-provided context is appended to a skill.
pub const SKILL_ADDITIONAL_CONTEXT_HEADING: &str = "## Additional context";

/// Provider-independent skill contract failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillContractError {
    message: String,
}

impl SkillContractError {
    /// Creates a skill contract failure with a stable diagnostic.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the human-readable contract diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SkillContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SkillContractError {}

/// Source scope for one effective skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    /// Skill shipped with Mezzanine.
    Builtin,
    /// Skill from the primary user configuration directory.
    User,
    /// Skill from a trusted project configuration directory.
    Project,
}

impl SkillSource {
    /// Returns the stable model-facing scope name for this source.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::User => "user",
            Self::Project => "project",
        }
    }

    /// Returns deterministic source precedence from lowest to highest.
    fn precedence(self) -> u8 {
        match self {
            Self::Builtin => 0,
            Self::User => 1,
            Self::Project => 2,
        }
    }
}

/// Catalog metadata for one available skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSummary {
    /// Skill identifier from `SKILL.md` front matter.
    pub name: String,
    /// Short usage description from `SKILL.md` front matter.
    pub description: String,
    /// Effective source scope for this skill.
    pub source: SkillSource,
    /// Path supplied by the product discovery adapter.
    pub path: PathBuf,
}

impl SkillSummary {
    /// Returns a human-facing source label.
    pub fn attribution_label(&self) -> String {
        self.source.as_str().to_string()
    }
}

/// Non-fatal discovery diagnostic for an invalid or unreadable skill path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiagnostic {
    /// Path whose skill metadata could not be used.
    pub path: PathBuf,
    /// Human-readable reason the path was skipped.
    pub message: String,
}

/// Effective skill catalog for one pane/project context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillCatalog {
    /// Deterministically ordered effective skills.
    pub skills: Vec<SkillSummary>,
    /// Non-fatal discovery diagnostics.
    pub diagnostics: Vec<SkillDiagnostic>,
}

/// Skill discovery and load state already visible in one active turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillActionContext {
    /// Whether a successful catalog result is already present.
    pub catalog_requested: bool,
    /// Skill names whose full documents are already in context.
    pub loaded_skills: BTreeSet<String>,
}

/// Extracts canonical skill action state from active model-context blocks.
///
/// Only explicit runtime-owned labels and action-result text are authoritative;
/// arbitrary repository or shell content cannot claim a loaded skill.
pub fn skill_action_context_from_blocks(blocks: &[ContextBlock]) -> SkillActionContext {
    let mut context = SkillActionContext::default();
    for block in blocks {
        if block.source == ContextSourceKind::ActionResult
            && block.content.lines().next().is_some_and(|line| {
                line.starts_with("[action_result ")
                    && line.contains(" request_skills ")
                    && line.ends_with(" succeeded]")
            })
        {
            context.catalog_requested = true;
        }
        if let Some(name) = block.label.strip_prefix("explicit skill ")
            && is_valid_skill_name(name)
        {
            context.loaded_skills.insert(name.to_string());
        }
        for line in block.content.lines() {
            let Some(name) = line.strip_prefix("# Skill: ") else {
                continue;
            };
            let name = name.trim();
            if is_valid_skill_name(name) {
                context.loaded_skills.insert(name.to_string());
            }
        }
    }
    context
}

/// Deterministic outcome of planning one non-effecting skill action.
#[derive(Debug, Clone, PartialEq)]
pub enum SkillActionPlan {
    /// The lower crate produced the complete action result without I/O.
    Result(ActionResult),
    /// The product must load the selected document before result completion.
    Load {
        /// Catalog summary identifying the concrete document to load.
        summary: SkillSummary,
        /// Optional model-authored semantic argument for the skill.
        additional_context: Option<String>,
    },
}

/// Plans one skill action using the effective catalog and turn-visible state.
///
/// Catalog requests complete immediately. Valid skill calls return a load plan
/// so the product can perform filesystem I/O; invalid, missing, and redundant
/// requests become model-correctable action failures without I/O.
pub fn plan_skill_action(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    catalog: &SkillCatalog,
    context: &mut SkillActionContext,
) -> Result<SkillActionPlan, SkillContractError> {
    match &action.payload {
        AgentActionPayload::RequestSkills => {
            if !context.loaded_skills.is_empty() {
                return skill_action_failure(
                    turn,
                    action,
                    "skill_context_already_loaded",
                    format!(
                        "skill context is already loaded for this turn: {}; use the loaded skill guidance or request the missing action capability instead of discovering skills again",
                        context
                            .loaded_skills
                            .iter()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                );
            }
            if context.catalog_requested {
                return skill_action_failure(
                    turn,
                    action,
                    "skill_catalog_already_requested",
                    "the effective skill catalog has already been returned for this turn; use an available skill or request the missing action capability instead of requesting the catalog again",
                );
            }
            context.catalog_requested = true;
            Ok(SkillActionPlan::Result(ActionResult::succeeded(
                turn,
                action,
                vec![catalog.model_catalog_text()],
                Some(catalog.structured_json()),
            )))
        }
        AgentActionPayload::CallSkill {
            name,
            additional_context,
        } => {
            if !is_valid_skill_name(name) {
                return skill_action_failure(
                    turn,
                    action,
                    "invalid_skill_name",
                    "skill name must contain only lowercase letters, digits, and hyphens",
                );
            }
            let Some(summary) = catalog.get(name) else {
                let available = if catalog.skills.is_empty() {
                    "none".to_string()
                } else {
                    catalog.names().join(",")
                };
                return skill_action_failure(
                    turn,
                    action,
                    "skill_not_found",
                    format!("skill {name:?} is not available; available skills: {available}"),
                );
            };
            if context.loaded_skills.contains(name) {
                return skill_action_failure(
                    turn,
                    action,
                    "skill_context_already_loaded",
                    format!(
                        "skill {name:?} is already loaded for this turn; use the loaded skill guidance or request the missing action capability instead of loading it again"
                    ),
                );
            }
            Ok(SkillActionPlan::Load {
                summary: summary.clone(),
                additional_context: additional_context.clone(),
            })
        }
        _ => Err(SkillContractError::new(
            "skill execution requires request_skills or call_skill action",
        )),
    }
}

/// Completes a successful skill load after the product supplies document text.
pub fn skill_load_action_result(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    document: &SkillDocument,
    additional_context: Option<&str>,
    content: String,
) -> ActionResult {
    ActionResult::succeeded(
        turn,
        action,
        vec![content],
        Some(
            serde_json::json!({
                "name": &document.summary.name,
                "source": document.summary.source.as_str(),
                "path": document.summary.path.to_string_lossy(),
                "skill_bytes": document.text.len(),
                "additional_context_bytes": additional_context.map(str::len).unwrap_or(0),
            })
            .to_string(),
        ),
    )
}

/// Builds one model-correctable deterministic skill action failure.
fn skill_action_failure(
    turn: &(impl AgentTurnResultIdentity + ?Sized),
    action: &AgentAction,
    code: &'static str,
    message: impl Into<String>,
) -> Result<SkillActionPlan, SkillContractError> {
    ActionResult::failed(turn, action, ActionStatus::Failed, code, message.into())
        .map(SkillActionPlan::Result)
        .map_err(|error| SkillContractError::new(error.to_string()))
}

impl SkillCatalog {
    /// Returns a skill summary by exact name.
    pub fn get(&self, name: &str) -> Option<&SkillSummary> {
        self.skills.iter().find(|skill| skill.name == name)
    }

    /// Returns skill names in deterministic catalog order.
    pub fn names(&self) -> Vec<String> {
        self.skills.iter().map(|skill| skill.name.clone()).collect()
    }

    /// Inserts one discovered summary using deterministic source precedence.
    pub fn insert(&mut self, summary: SkillSummary) {
        if let Some(index) = self
            .skills
            .iter()
            .position(|existing| existing.name == summary.name)
        {
            let existing = &self.skills[index];
            let replace = summary.source.precedence() > existing.source.precedence();
            self.diagnostics.push(SkillDiagnostic {
                path: summary.path.clone(),
                message: if replace {
                    format!(
                        "skill name {:?} from {} overrides existing {} entry",
                        summary.name,
                        summary.attribution_label(),
                        existing.attribution_label()
                    )
                } else {
                    format!(
                        "skill name {:?} from {} ignored because existing {} entry has precedence",
                        summary.name,
                        summary.attribution_label(),
                        existing.attribution_label()
                    )
                },
            });
            if !replace {
                return;
            }
            self.skills[index] = summary;
        } else {
            self.skills.push(summary);
        }
        self.skills
            .sort_by(|left, right| left.name.cmp(&right.name));
    }

    /// Builds compact model-facing catalog text.
    pub fn model_catalog_text(&self) -> String {
        let mut lines = Vec::new();
        if self.skills.is_empty() {
            lines.push("No skills are currently available.".to_string());
        } else {
            lines.push("Available skills:".to_string());
            lines.extend(self.skills.iter().map(|skill| {
                format!(
                    "- {} ({}) - {}",
                    skill.name,
                    skill.source.as_str(),
                    skill.description
                )
            }));
        }
        if !self.diagnostics.is_empty() {
            lines.push(String::new());
            lines.push("Skipped skill diagnostics:".to_string());
            lines.extend(self.diagnostics.iter().map(|diagnostic| {
                format!("- {} - {}", diagnostic.path.display(), diagnostic.message)
            }));
        }
        lines.join("\n")
    }

    /// Builds structured JSON for action-result metadata.
    pub fn structured_json(&self) -> String {
        serde_json::json!({
            "skills": self.skills.iter().map(|skill| {
                serde_json::json!({
                    "name": skill.name,
                    "description": skill.description,
                    "source": skill.source.as_str(),
                    "path": skill.path.to_string_lossy(),
                })
            }).collect::<Vec<_>>(),
            "diagnostics": self.diagnostics.iter().map(|diagnostic| {
                serde_json::json!({
                    "path": diagnostic.path.to_string_lossy(),
                    "message": diagnostic.message,
                })
            }).collect::<Vec<_>>(),
        })
        .to_string()
    }
}

/// Full skill document loaded for an explicit invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDocument {
    /// Catalog metadata for the loaded skill.
    pub summary: SkillSummary,
    /// Complete raw `SKILL.md` text.
    pub text: String,
}

/// Parsed explicit `$<skill-name>` agent prompt invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPromptInvocation {
    /// Skill name after the leading `$`.
    pub name: String,
    /// Optional trailing prompt text used as a semantic skill argument.
    pub additional_context: Option<String>,
}

/// Parsed dependency-neutral `SKILL.md` metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSkillDocument {
    /// Validated front-matter skill name.
    pub name: String,
    /// Trimmed non-empty front-matter description.
    pub description: String,
}

#[derive(Debug, Deserialize)]
struct SkillFrontMatter {
    name: String,
    description: String,
}

/// Parses and validates one complete `SKILL.md` document.
pub fn parse_skill_document(text: &str) -> Result<ParsedSkillDocument, SkillContractError> {
    let (front_matter, _body) = split_skill_front_matter(text)?;
    let front_matter: SkillFrontMatter = serde_norway::from_str(front_matter).map_err(|error| {
        SkillContractError::new(format!("failed to parse SKILL.md front matter: {error}"))
    })?;
    if !is_valid_skill_name(&front_matter.name) {
        return Err(SkillContractError::new(format!(
            "skill name {:?} is invalid",
            front_matter.name
        )));
    }
    if front_matter.description.trim().is_empty() {
        return Err(SkillContractError::new(
            "skill description must not be empty",
        ));
    }
    Ok(ParsedSkillDocument {
        name: front_matter.name,
        description: front_matter.description.trim().to_string(),
    })
}

/// Formats a loaded skill for model context.
pub fn skill_context_text(document: &SkillDocument, additional_context: Option<&str>) -> String {
    let mut text = format!(
        "# Skill: {}\n\nSource: {}\nPath: {}\n\nInvocation state: this skill is already loaded for the current turn. Do not call `request_skills` or `call_skill` merely to discover, confirm, or reload this skill; follow the workflow below with the currently available actions, or request a missing action family with `request_capability`.\n\n{}",
        document.summary.name,
        document.summary.source.as_str(),
        document.summary.path.display(),
        document.text.trim_end()
    );
    if let Some(additional_context) = additional_context
        && !additional_context.trim().is_empty()
    {
        text.push_str("\n\n");
        text.push_str(SKILL_ADDITIONAL_CONTEXT_HEADING);
        text.push_str("\n\n");
        text.push_str(additional_context.trim());
    }
    text
}

/// Parses explicit `$<skill-name>` agent prompt syntax.
pub fn parse_skill_prompt_invocation(input: &str) -> Option<SkillPromptInvocation> {
    let trimmed = input.trim_start();
    let remainder = trimmed.strip_prefix('$')?;
    let name_end = remainder
        .char_indices()
        .find_map(|(index, ch)| ch.is_whitespace().then_some(index))
        .unwrap_or(remainder.len());
    let name = remainder[..name_end].trim();
    if name.is_empty() {
        return None;
    }
    let argument = remainder[name_end..].trim();
    let additional_context = (!argument.is_empty()).then(|| argument.to_string());
    Some(SkillPromptInvocation {
        name: name.to_string(),
        additional_context,
    })
}

/// Splits a markdown skill file into YAML front matter and body text.
pub fn split_skill_front_matter(text: &str) -> Result<(&str, &str), SkillContractError> {
    let normalized = text
        .strip_prefix("---\r\n")
        .or_else(|| text.strip_prefix("---\n"));
    let Some(after_open) = normalized else {
        return Err(SkillContractError::new(
            "SKILL.md must start with YAML front matter",
        ));
    };
    for marker in ["\n---\n", "\n---\r\n", "\r\n---\r\n", "\r\n---\n"] {
        if let Some(index) = after_open.find(marker) {
            let front_matter = &after_open[..index];
            let body = &after_open[index + marker.len()..];
            return Ok((front_matter, body));
        }
    }
    Err(SkillContractError::new(
        "SKILL.md front matter is not closed",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentTurnRecord, AgentTurnState, AgentTurnTrigger};

    /// Builds one valid summary fixture for catalog and context tests.
    fn summary(name: &str, source: SkillSource) -> SkillSummary {
        SkillSummary {
            name: name.to_string(),
            description: format!("{name} workflow"),
            source,
            path: PathBuf::from(format!("/{name}/SKILL.md")),
        }
    }

    /// Builds one running turn fixture for skill action planning.
    fn turn() -> AgentTurnRecord {
        AgentTurnRecord {
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: "%1".to_string(),
            trigger: AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            state: AgentTurnState::Running,
            cooperation_mode: None,
            initial_capability: None,
        }
    }

    /// Builds one skill action with stable identity.
    fn action(payload: AgentActionPayload) -> AgentAction {
        AgentAction {
            id: "skill-1".to_string(),
            rationale: "load guidance".to_string(),
            payload,
        }
    }

    /// Verifies catalog insertion applies source precedence, deterministic
    /// ordering, and explicit collision diagnostics without filesystem input.
    #[test]
    fn skill_catalog_applies_source_precedence() {
        let mut catalog = SkillCatalog::default();
        catalog.insert(summary("review", SkillSource::User));
        catalog.insert(summary("audit", SkillSource::User));
        catalog.insert(summary("review", SkillSource::Project));

        assert_eq!(catalog.names(), vec!["audit", "review"]);
        assert_eq!(catalog.get("review").unwrap().source, SkillSource::Project);
        assert_eq!(catalog.diagnostics.len(), 1);
        assert!(catalog.diagnostics[0].message.contains("overrides"));
    }

    /// Verifies catalog requests complete deterministically and a repeated
    /// request becomes a model-correctable failure without product I/O.
    #[test]
    fn skill_action_planning_suppresses_redundant_catalog_requests() {
        let mut catalog = SkillCatalog::default();
        catalog.insert(summary("review", SkillSource::User));
        let action = action(AgentActionPayload::RequestSkills);
        let mut context = SkillActionContext::default();

        let SkillActionPlan::Result(first) =
            plan_skill_action(&turn(), &action, &catalog, &mut context).unwrap()
        else {
            panic!("catalog request must complete without I/O");
        };
        assert!(!first.is_error);
        assert!(context.catalog_requested);

        let SkillActionPlan::Result(repeated) =
            plan_skill_action(&turn(), &action, &catalog, &mut context).unwrap()
        else {
            panic!("repeated catalog request must fail without I/O");
        };
        assert_eq!(
            repeated.error.unwrap().code,
            "skill_catalog_already_requested"
        );
    }

    /// Verifies valid skill calls yield an explicit product load plan and the
    /// lower completion preserves canonical document metadata.
    #[test]
    fn skill_action_planning_and_completion_bound_product_loading() {
        let mut catalog = SkillCatalog::default();
        catalog.insert(summary("review", SkillSource::Project));
        let action = action(AgentActionPayload::CallSkill {
            name: "review".to_string(),
            additional_context: Some("focus tests".to_string()),
        });
        let mut context = SkillActionContext::default();

        let SkillActionPlan::Load {
            summary,
            additional_context,
        } = plan_skill_action(&turn(), &action, &catalog, &mut context).unwrap()
        else {
            panic!("valid call must request product document loading");
        };
        let document = SkillDocument {
            summary,
            text: "Review workflow".to_string(),
        };
        let result = skill_load_action_result(
            &turn(),
            &action,
            &document,
            additional_context.as_deref(),
            skill_context_text(&document, additional_context.as_deref()),
        );
        let structured: serde_json::Value =
            serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();

        assert!(!result.is_error);
        assert_eq!(structured["name"], "review");
        assert_eq!(structured["source"], "project");
        assert_eq!(structured["additional_context_bytes"], 11);
    }

    /// Verifies only explicit lower-owned labels and skill headings restore
    /// catalog and loaded-skill state from active turn context.
    #[test]
    fn skill_action_context_extracts_runtime_owned_state() {
        let context = skill_action_context_from_blocks(&[
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: crate::ContextPlacement::EphemeralTail,
                label: "action result catalog".to_string(),
                content: "[action_result catalog request_skills succeeded]".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::SkillInstruction,
                placement: crate::ContextPlacement::EphemeralTail,
                label: "explicit skill review".to_string(),
                content: "# Skill: review\n\nWorkflow".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "untrusted".to_string(),
                content: "# Skill: ../escape".to_string(),
            },
        ]);

        assert!(context.catalog_requested);
        assert_eq!(
            context.loaded_skills.into_iter().collect::<Vec<_>>(),
            ["review"]
        );
    }

    /// Verifies skill metadata parsing accepts quoted YAML descriptions and
    /// rejects malformed names before product discovery records a summary.
    #[test]
    fn skill_document_parser_validates_front_matter() {
        let parsed = parse_skill_document(
            "---\nname: review\ndescription: \"Review workflow: parser coverage\"\n---\nBody\n",
        )
        .unwrap();
        assert_eq!(parsed.name, "review");
        assert_eq!(parsed.description, "Review workflow: parser coverage");

        let error =
            parse_skill_document("---\nname: ../review\ndescription: invalid path\n---\nBody\n")
                .unwrap_err();
        assert!(error.message().contains("is invalid"));
    }

    /// Verifies loaded skill context includes the canonical loaded-state
    /// guidance and appends a trimmed semantic argument under one heading.
    #[test]
    fn skill_context_appends_additional_context_heading() {
        let document = SkillDocument {
            summary: summary("review", SkillSource::Project),
            text: "Do review.\n".to_string(),
        };
        let context = skill_context_text(&document, Some("  Focus on tests.  "));

        assert!(context.contains("# Skill: review"));
        assert!(context.contains("Invocation state: this skill is already loaded"));
        assert!(context.contains("## Additional context\n\nFocus on tests."));
    }

    /// Verifies explicit skill invocation parsing keeps the identifier distinct
    /// from its optional trailing semantic argument.
    #[test]
    fn skill_prompt_invocation_parses_name_and_argument() {
        let invocation = parse_skill_prompt_invocation("  $review focus src/lib.rs").unwrap();

        assert_eq!(invocation.name, "review");
        assert_eq!(
            invocation.additional_context.as_deref(),
            Some("focus src/lib.rs")
        );
        assert!(parse_skill_prompt_invocation("$").is_none());
    }
}
