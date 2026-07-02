//! Skill discovery and loading.
//!
//! Skills are reusable markdown workflow descriptions stored as built-ins,
//! below the user configuration root, or below a trusted project's `.mezzanine`
//! directory. This module keeps discovery deterministic and side-effect free:
//! it reads `SKILL.md` metadata for catalogs, loads full skill text only on
//! explicit invocation, and never executes auxiliary skill files.

use crate::agent::baseline_slash_commands;
use crate::command::baseline_commands;
use crate::config::{
    CONFIG_CHANGE_OPERATION_NAMES, CONFIG_CHANGE_VALUE_DESCRIPTION,
    config_change_setting_path_annotations_markdown, config_change_setting_path_description,
};
use crate::terminal::UI_COLOR_SLOT_NAMES;
use crate::{MezError, MezErrorKind, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Directory name that contains user or project skills.
pub const SKILLS_DIRECTORY_NAME: &str = "skills";
/// File name that carries one skill's metadata and markdown instructions.
pub const SKILL_FILE_NAME: &str = "SKILL.md";
/// Markdown heading used when caller-provided context is appended to a skill.
pub const SKILL_ADDITIONAL_CONTEXT_HEADING: &str = "## Additional context";
/// Stable name for the built-in skill-authoring workflow.
pub const BUILTIN_CREATE_SKILL_NAME: &str = "create-skill";
/// Stable name for the built-in documentation memory workflow.
pub const BUILTIN_ADD_DOC_SKILL_NAME: &str = "add-doc";
/// Stable name for the built-in issue filing workflow.
pub const BUILTIN_ADD_ISSUES_SKILL_NAME: &str = "add-issues";
/// Stable name for the built-in research memory workflow.
pub const BUILTIN_ADD_RESEARCH_SKILL_NAME: &str = "add-research";
/// Stable name for the built-in issue fixing workflow.
pub const BUILTIN_FIX_ISSUES_SKILL_NAME: &str = "fix-issues";
/// Stable name for the built-in Mezzanine reference workflow.
pub const BUILTIN_MEZ_REFERENCE_SKILL_NAME: &str = "mez-reference";
/// Virtual path prefix used for built-in skills that do not live on disk.
pub const BUILTIN_SKILL_PATH_PREFIX: &str = "<builtin>";

const BUILTIN_CREATE_SKILL_DESCRIPTION: &str = "Create or modify concise Mezzanine skills in user or project scope. Use when the user asks to add, update, refactor, or repair a skill, SKILL.md, or skill resources.";
const BUILTIN_CREATE_SKILL_TEXT: &str = include_str!("builtin/create-skill/SKILL.md");
const BUILTIN_ADD_DOC_SKILL_DESCRIPTION: &str =
    "Use when the user asks to save durable documentation or reference content into memory.";
const BUILTIN_ADD_DOC_SKILL_TEXT: &str = include_str!("builtin/add-doc/SKILL.md");
const BUILTIN_ADD_ISSUES_SKILL_DESCRIPTION: &str =
    "Use when recent findings should be turned into Mezzanine project issue tracker entries.";
const BUILTIN_ADD_ISSUES_SKILL_TEXT: &str = include_str!("builtin/add-issues/SKILL.md");
const BUILTIN_ADD_RESEARCH_SKILL_DESCRIPTION: &str =
    "Use when the user asks to save durable research findings into memory.";
const BUILTIN_ADD_RESEARCH_SKILL_TEXT: &str = include_str!("builtin/add-research/SKILL.md");
const BUILTIN_FIX_ISSUES_SKILL_DESCRIPTION: &str = "Use when you need to query the current project's Mez issue tracker, fix the returned issues, keep progress notes current, and remove verified fixed issues from the tracker.";
const BUILTIN_FIX_ISSUES_SKILL_TEXT: &str = include_str!("builtin/fix-issues/SKILL.md");
const BUILTIN_MEZ_REFERENCE_SKILL_DESCRIPTION: &str = "Use Mezzanine terminal commands, agent slash commands, skill invocation, common workflows, and live config_change schema guidance without rediscovering the command or config surface.";
const BUILTIN_MEZ_REFERENCE_SKILL_TEXT: &str = include_str!("builtin/mez-reference/SKILL.md");

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
            SkillSource::Builtin => "builtin",
            SkillSource::User => "user",
            SkillSource::Project => "project",
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
    /// Absolute or caller-supplied path to the backing `SKILL.md` file.
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

impl SkillCatalog {
    /// Returns a skill summary by exact name.
    ///
    /// # Parameters
    /// - `name`: Skill name to resolve from the effective catalog.
    pub fn get(&self, name: &str) -> Option<&SkillSummary> {
        self.skills.iter().find(|skill| skill.name == name)
    }

    /// Returns skill names in deterministic catalog order.
    pub fn names(&self) -> Vec<String> {
        self.skills.iter().map(|skill| skill.name.clone()).collect()
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

#[derive(Debug, Deserialize)]
struct SkillFrontMatter {
    /// Stable skill identifier declared by the skill author.
    name: String,
    /// Short model-facing description of when the skill should be used.
    description: String,
}

/// Discovers the effective skill catalog for one user/project context.
///
/// # Parameters
/// - `user_config_root`: Primary Mezzanine configuration root, when known.
/// - `project_root`: Trusted project root for the active pane, when known.
pub fn discover_skill_catalog(
    user_config_root: Option<&Path>,
    project_root: Option<&Path>,
) -> SkillCatalog {
    let mut skills = BTreeMap::<String, SkillSummary>::new();
    let mut diagnostics = Vec::new();
    for summary in builtin_skill_summaries() {
        skills.insert(summary.name.clone(), summary);
    }
    if let Some(root) = user_config_root {
        discover_skills_under_root(
            &root.join(SKILLS_DIRECTORY_NAME),
            SkillSource::User,
            &mut skills,
            &mut diagnostics,
        );
    }
    if let Some(root) = project_root {
        discover_skills_under_root(
            &root.join(".mezzanine").join(SKILLS_DIRECTORY_NAME),
            SkillSource::Project,
            &mut skills,
            &mut diagnostics,
        );
    }
    SkillCatalog {
        skills: skills.into_values().collect(),
        diagnostics,
    }
}

/// Loads the full markdown for one skill summary.
///
/// # Parameters
/// - `summary`: Skill metadata returned by `discover_skill_catalog`.
pub fn load_skill_document(summary: &SkillSummary) -> Result<SkillDocument> {
    if summary.source == SkillSource::Builtin {
        let Some(text) = builtin_skill_text(&summary.name) else {
            return Err(MezError::invalid_args(format!(
                "unknown built-in skill {:?}",
                summary.name
            )));
        };
        return Ok(SkillDocument {
            summary: summary.clone(),
            text,
        });
    }
    let text = fs::read_to_string(&summary.path).map_err(|error| {
        MezError::new(
            MezErrorKind::Io,
            format!(
                "failed to read skill {} from {}: {}",
                summary.name,
                summary.path.display(),
                error
            ),
        )
    })?;
    Ok(SkillDocument {
        summary: summary.clone(),
        text,
    })
}

/// Formats a loaded skill for model context.
///
/// # Parameters
/// - `document`: Loaded skill document.
/// - `additional_context`: Optional semantic argument to append.
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

/// Returns the built-in skills shipped with Mezzanine.
fn builtin_skill_summaries() -> Vec<SkillSummary> {
    [
        (BUILTIN_CREATE_SKILL_NAME, BUILTIN_CREATE_SKILL_DESCRIPTION),
        (
            BUILTIN_ADD_DOC_SKILL_NAME,
            BUILTIN_ADD_DOC_SKILL_DESCRIPTION,
        ),
        (
            BUILTIN_ADD_ISSUES_SKILL_NAME,
            BUILTIN_ADD_ISSUES_SKILL_DESCRIPTION,
        ),
        (
            BUILTIN_ADD_RESEARCH_SKILL_NAME,
            BUILTIN_ADD_RESEARCH_SKILL_DESCRIPTION,
        ),
        (
            BUILTIN_FIX_ISSUES_SKILL_NAME,
            BUILTIN_FIX_ISSUES_SKILL_DESCRIPTION,
        ),
        (
            BUILTIN_MEZ_REFERENCE_SKILL_NAME,
            BUILTIN_MEZ_REFERENCE_SKILL_DESCRIPTION,
        ),
    ]
    .into_iter()
    .map(|(name, description)| SkillSummary {
        name: name.to_string(),
        description: description.to_string(),
        source: SkillSource::Builtin,
        path: builtin_skill_path(name),
    })
    .collect()
}

/// Returns the virtual path used for a built-in skill.
///
/// # Parameters
/// - `name`: Built-in skill name.
fn builtin_skill_path(name: &str) -> PathBuf {
    PathBuf::from(BUILTIN_SKILL_PATH_PREFIX)
        .join(name)
        .join(SKILL_FILE_NAME)
}

/// Returns the static markdown for one built-in skill.
///
/// # Parameters
/// - `name`: Built-in skill name.
fn builtin_skill_text(name: &str) -> Option<String> {
    match name {
        BUILTIN_CREATE_SKILL_NAME => Some(BUILTIN_CREATE_SKILL_TEXT.to_string()),
        BUILTIN_ADD_DOC_SKILL_NAME => Some(BUILTIN_ADD_DOC_SKILL_TEXT.to_string()),
        BUILTIN_ADD_ISSUES_SKILL_NAME => Some(BUILTIN_ADD_ISSUES_SKILL_TEXT.to_string()),
        BUILTIN_ADD_RESEARCH_SKILL_NAME => Some(BUILTIN_ADD_RESEARCH_SKILL_TEXT.to_string()),
        BUILTIN_FIX_ISSUES_SKILL_NAME => Some(BUILTIN_FIX_ISSUES_SKILL_TEXT.to_string()),
        BUILTIN_MEZ_REFERENCE_SKILL_NAME => Some(format_builtin_mez_reference_skill()),
        _ => None,
    }
}

/// Formats the built-in reference skill with the live command and config schema.
fn format_builtin_mez_reference_skill() -> String {
    let theme_slots = UI_COLOR_SLOT_NAMES
        .iter()
        .map(|slot| format!("- `theme.colors.{slot}`"))
        .collect::<Vec<_>>()
        .join("\n");
    let terminal_commands = baseline_commands()
        .into_iter()
        .map(|command| format!("- `:{}`", command.name))
        .collect::<Vec<_>>()
        .join("\n");
    let agent_commands = baseline_slash_commands()
        .into_iter()
        .map(|command| {
            if command.aliases.is_empty() {
                format!("- `/{}`", command.name)
            } else {
                let aliases = command
                    .aliases
                    .iter()
                    .map(|alias| format!("`/{alias}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("- `/{}` (aliases: {})", command.name, aliases,)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{}\n\n## Terminal command index\n\n{}\n\n## Agent shell slash command index\n\n{}\n\n## Live config_change reference\n\nAllowed operations: `{}`.\n\nValue shape: {}\n\nSupported setting paths:\n{}\n\nTheme color slots:\n{}\n\nAnnotated setting paths:\n\n{}",
        BUILTIN_MEZ_REFERENCE_SKILL_TEXT.trim_end(),
        terminal_commands,
        agent_commands,
        CONFIG_CHANGE_OPERATION_NAMES.join("`, `"),
        CONFIG_CHANGE_VALUE_DESCRIPTION,
        config_change_setting_path_description(),
        theme_slots,
        config_change_setting_path_annotations_markdown(),
    )
}

/// Parses explicit `$<skill-name>` agent prompt syntax.
///
/// # Parameters
/// - `input`: User-submitted pane-local agent prompt text.
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

/// Discovers valid direct child skill directories below one `skills` root.
///
/// # Parameters
/// - `root`: Directory containing one subdirectory per skill.
/// - `source`: Source scope assigned to discovered skill summaries.
/// - `skills`: Effective skill map updated by skill name.
/// - `diagnostics`: Non-fatal discovery diagnostics appended for skipped paths.
fn discover_skills_under_root(
    root: &Path,
    source: SkillSource,
    skills: &mut BTreeMap<String, SkillSummary>,
    diagnostics: &mut Vec<SkillDiagnostic>,
) {
    let metadata = match fs::metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            diagnostics.push(SkillDiagnostic {
                path: root.to_path_buf(),
                message: format!("skill root is unreadable: {error}"),
            });
            return;
        }
    };
    if !metadata.is_dir() {
        diagnostics.push(SkillDiagnostic {
            path: root.to_path_buf(),
            message: "skill root is not a directory".to_string(),
        });
        return;
    }
    let mut entries = match fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .collect::<Vec<_>>(),
        Err(error) => {
            diagnostics.push(SkillDiagnostic {
                path: root.to_path_buf(),
                message: format!("skill root could not be listed: {error}"),
            });
            return;
        }
    };
    entries.sort();
    for path in entries {
        let skill_path = path.join(SKILL_FILE_NAME);
        match read_skill_summary(&path, &skill_path, source) {
            Ok(summary) => {
                insert_skill_summary(skills, diagnostics, summary);
            }
            Err(message) => diagnostics.push(SkillDiagnostic {
                path: skill_path,
                message,
            }),
        }
    }
}

/// Inserts one discovered skill while reporting name collisions and enforcing
/// source precedence.
fn insert_skill_summary(
    skills: &mut BTreeMap<String, SkillSummary>,
    diagnostics: &mut Vec<SkillDiagnostic>,
    summary: SkillSummary,
) {
    if let Some(existing) = skills.get(&summary.name) {
        let replace =
            skill_source_precedence(summary.source) > skill_source_precedence(existing.source);
        diagnostics.push(SkillDiagnostic {
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
    }
    skills.insert(summary.name.clone(), summary);
}

/// Returns deterministic skill-source precedence from lowest to highest.
fn skill_source_precedence(source: SkillSource) -> u8 {
    match source {
        SkillSource::Builtin => 0,
        SkillSource::User => 1,
        SkillSource::Project => 2,
    }
}

/// Reads and validates one candidate skill directory.
///
/// # Parameters
/// - `directory`: Skill directory whose basename must match the declared name.
/// - `skill_path`: Path to the candidate `SKILL.md` file.
/// - `source`: Source scope assigned to the resulting skill.
fn read_skill_summary(
    directory: &Path,
    skill_path: &Path,
    source: SkillSource,
) -> std::result::Result<SkillSummary, String> {
    if !directory.is_dir() {
        return Err("skill entry is not a directory".to_string());
    }
    let directory_name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "skill directory name is not valid UTF-8".to_string())?;
    if !is_valid_skill_name(directory_name) {
        return Err(format!(
            "skill directory name {directory_name:?} is invalid"
        ));
    }
    let text = fs::read_to_string(skill_path)
        .map_err(|error| format!("failed to read SKILL.md: {error}"))?;
    let (front_matter, _body) = split_skill_front_matter(&text)?;
    let front_matter: SkillFrontMatter = serde_norway::from_str(front_matter)
        .map_err(|error| format!("failed to parse SKILL.md front matter: {error}"))?;
    if !is_valid_skill_name(&front_matter.name) {
        return Err(format!("skill name {:?} is invalid", front_matter.name));
    }
    if front_matter.name != directory_name {
        return Err(format!(
            "skill name {:?} does not match directory {:?}",
            front_matter.name, directory_name
        ));
    }
    if front_matter.description.trim().is_empty() {
        return Err("skill description must not be empty".to_string());
    }
    Ok(SkillSummary {
        name: front_matter.name,
        description: front_matter.description.trim().to_string(),
        source,
        path: skill_path.to_path_buf(),
    })
}

/// Splits a markdown skill file into YAML front matter and body text.
///
/// # Parameters
/// - `text`: Complete `SKILL.md` contents.
fn split_skill_front_matter(text: &str) -> std::result::Result<(&str, &str), String> {
    let normalized = text
        .strip_prefix("---\r\n")
        .or_else(|| text.strip_prefix("---\n"));
    let Some(after_open) = normalized else {
        return Err("SKILL.md must start with YAML front matter".to_string());
    };
    for marker in ["\n---\n", "\n---\r\n", "\r\n---\r\n", "\r\n---\n"] {
        if let Some(index) = after_open.find(marker) {
            let front_matter = &after_open[..index];
            let body = &after_open[index + marker.len()..];
            return Ok((front_matter, body));
        }
    }
    Err("SKILL.md front matter is not closed".to_string())
}

/// Returns whether a string satisfies Mezzanine's skill-name grammar.
///
/// # Parameters
/// - `name`: Candidate skill name.
pub fn is_valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && name
            .bytes()
            .any(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::{
        BUILTIN_ADD_DOC_SKILL_NAME, BUILTIN_ADD_ISSUES_SKILL_NAME, BUILTIN_ADD_RESEARCH_SKILL_NAME,
        BUILTIN_CREATE_SKILL_NAME, BUILTIN_FIX_ISSUES_SKILL_NAME, BUILTIN_MEZ_REFERENCE_SKILL_NAME,
        BUILTIN_SKILL_PATH_PREFIX, SkillSource, discover_skill_catalog, is_valid_skill_name,
        load_skill_document, parse_skill_prompt_invocation, skill_context_text,
        split_skill_front_matter,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Creates a unique temporary root for skill-discovery tests without
    /// adding test-only dependencies to the production crate graph.
    ///
    /// # Parameters
    /// - `label`: Human-readable suffix used to identify the fixture root.
    fn test_temp_root(label: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "mez-skills-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// Writes one valid skill fixture into the requested root.
    ///
    /// # Parameters
    /// - `root`: The skills root receiving the fixture directory.
    /// - `name`: Skill name and directory basename.
    /// - `description`: Front matter description to store.
    /// - `body`: Markdown instruction body.
    fn write_skill(root: &Path, name: &str, description: &str, body: &str) {
        let directory = root.join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
        )
        .unwrap();
    }

    /// Verifies user and project skill roots share the same layout while
    /// project skills override user skills with the same name.
    #[test]
    fn skill_catalog_discovers_roots_and_project_precedence() {
        let root = test_temp_root("precedence");
        let user_root = root.join("user");
        let project_root = root.join("repo");
        write_skill(
            &user_root.join("skills"),
            "ship-it",
            "User workflow",
            "user body",
        );
        write_skill(
            &project_root.join(".mezzanine/skills"),
            "ship-it",
            "Project workflow",
            "project body",
        );
        write_skill(
            &project_root.join(".mezzanine/skills"),
            "audit",
            "Audit workflow",
            "audit body",
        );

        let catalog = discover_skill_catalog(Some(&user_root), Some(&project_root));

        assert_eq!(
            catalog.names(),
            vec![
                "add-doc",
                "add-issues",
                "add-research",
                "audit",
                "create-skill",
                "fix-issues",
                "mez-reference",
                "ship-it",
            ]
        );
        let overridden = catalog.get("ship-it").unwrap();
        assert_eq!(overridden.description, "Project workflow");
        assert_eq!(overridden.source, SkillSource::Project);
    }

    /// Verifies skill front matter parsing still accepts YAML quoted scalars.
    ///
    /// This regression scenario covers the maintained YAML parser replacement
    /// at the skill catalog boundary so descriptions containing punctuation are
    /// preserved when catalog entries are discovered.
    #[test]
    fn skill_catalog_parses_yaml_front_matter_with_replacement_parser() {
        let root = test_temp_root("yaml-front-matter");
        let user_root = root.join("user");
        let directory = user_root.join("skills/review");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("SKILL.md"),
            "---\nname: review\ndescription: \"Review workflow: parser coverage\"\n---\n\nReview body.\n",
        )
        .unwrap();

        let catalog = discover_skill_catalog(Some(&user_root), None);
        let summary = catalog.get("review").unwrap();
        assert_eq!(summary.description, "Review workflow: parser coverage");
        assert_eq!(summary.source, SkillSource::User);
    }

    /// Verifies built-in workflow references are always discoverable
    /// from the effective catalog without requiring user or project files. The
    /// catalog entries should be loadable even though their paths are virtual
    /// built-in paths rather than filesystem paths.
    #[test]
    fn skill_catalog_includes_loadable_builtins() {
        let catalog = discover_skill_catalog(None, None);
        let summary = catalog.get(BUILTIN_CREATE_SKILL_NAME).unwrap();

        assert_eq!(summary.source, SkillSource::Builtin);
        assert!(
            catalog
                .model_catalog_text()
                .contains("- create-skill (builtin)")
        );
        assert!(catalog.structured_json().contains(r#""source":"builtin""#));
        assert!(
            summary
                .path
                .starts_with(PathBuf::from(BUILTIN_SKILL_PATH_PREFIX))
        );
        assert!(catalog.model_catalog_text().contains("- add-doc (builtin)"));
        assert!(
            catalog
                .model_catalog_text()
                .contains("- add-issues (builtin)")
        );
        assert!(
            catalog
                .model_catalog_text()
                .contains("- add-research (builtin)")
        );
        assert!(
            catalog
                .model_catalog_text()
                .contains("- fix-issues (builtin)")
        );

        let document = load_skill_document(summary).unwrap();
        let (front_matter, body) = split_skill_front_matter(&document.text).unwrap();
        assert!(front_matter.contains("name: create-skill"));
        assert!(front_matter.contains("description: Create or modify concise"));
        assert!(body.contains("Create the smallest skill that satisfies the user's intent."));
        assert!(body.contains("User scope: `<config-root>/skills/<skill-name>/SKILL.md`"));
        assert!(body.contains("Default to user scope."));
        assert!(
            document
                .summary
                .description
                .contains("Create or modify concise Mezzanine skills")
        );

        let add_doc_document =
            load_skill_document(catalog.get(BUILTIN_ADD_DOC_SKILL_NAME).unwrap()).unwrap();
        assert!(add_doc_document.text.contains("name: add-doc"));
        assert!(add_doc_document.text.contains("kind` to `documentation`"));
        assert!(add_doc_document.text.contains("readable Markdown"));

        let add_issues_document =
            load_skill_document(catalog.get(BUILTIN_ADD_ISSUES_SKILL_NAME).unwrap()).unwrap();
        assert!(add_issues_document.text.contains("name: add-issues"));
        assert!(
            add_issues_document
                .text
                .contains("Use the local issue-tracker MAAP actions directly")
        );
        assert!(
            add_issues_document
                .text
                .contains("request the `issues` capability before proceeding")
        );

        let add_research_document =
            load_skill_document(catalog.get(BUILTIN_ADD_RESEARCH_SKILL_NAME).unwrap()).unwrap();
        assert!(add_research_document.text.contains("name: add-research"));
        assert!(add_research_document.text.contains("kind` to `research`"));
        assert!(
            add_research_document
                .text
                .contains("effectively non-expiring retention horizon")
        );

        let fix_issues_document =
            load_skill_document(catalog.get(BUILTIN_FIX_ISSUES_SKILL_NAME).unwrap()).unwrap();
        assert!(fix_issues_document.text.contains("name: fix-issues"));
        assert!(
            fix_issues_document
                .text
                .contains("Store the plan in the issue notes field")
        );
        assert!(
            fix_issues_document.text.contains(
                "Do not delete an issue until implementation and verification are complete"
            )
        );

        let reference_document =
            load_skill_document(catalog.get(BUILTIN_MEZ_REFERENCE_SKILL_NAME).unwrap()).unwrap();
        assert!(reference_document.text.contains("name: mez-reference"));
        assert!(reference_document.text.contains("Terminal command index"));
        assert!(
            reference_document
                .text
                .contains("Agent shell slash command index")
        );
        assert!(
            reference_document
                .text
                .contains("Allowed operations: `set`, `unset`, `reset`")
        );
        assert!(reference_document.text.contains("Supported setting paths:"));
        assert!(reference_document.text.contains("Theme color slots:"));
        assert!(
            reference_document
                .text
                .contains("- `theme.colors.agent_prompt_bg`")
        );
        assert!(reference_document.text.contains("Annotated setting paths:"));
        assert!(
            reference_document
                .text
                .contains("| `theme.active` | Switch the active")
        );
        assert!(
            reference_document
                .text
                .contains("| `theme.aliases.<alias>` | Override")
        );
        assert!(
            reference_document
                .text
                .contains("| `model_profiles.<name>.<key>` | Create or adjust")
        );
        assert!(reference_document.text.contains("Value/type"));
        assert!(reference_document.text.contains("Format requirements"));
    }

    /// Verifies filesystem skills can intentionally override the built-in
    /// `create-skill` entry. Built-ins are the lowest-precedence source so
    /// advanced users and trusted projects can customize the authoring workflow.
    #[test]
    fn skill_catalog_allows_user_skill_to_override_builtin_create_skill() {
        let root = test_temp_root("override-builtin");
        let user_root = root.join("user");
        write_skill(
            &user_root.join("skills"),
            "create-skill",
            "Custom skill workflow",
            "Do custom skill work.",
        );

        let catalog = discover_skill_catalog(Some(&user_root), None);
        let summary = catalog.get("create-skill").unwrap();
        let document = load_skill_document(summary).unwrap();

        assert_eq!(summary.source, SkillSource::User);
        assert_eq!(summary.description, "Custom skill workflow");
        assert!(document.text.contains("Do custom skill work."));
    }

    /// Verifies loaded skill context contains the full SKILL.md text and appends
    /// explicit semantic arguments under the required heading.
    #[test]
    fn skill_context_appends_additional_context_heading() {
        let root = test_temp_root("context");
        let user_root = root.join("user");
        write_skill(
            &user_root.join("skills"),
            "review",
            "Review workflow",
            "Do review.",
        );
        let catalog = discover_skill_catalog(Some(&user_root), None);
        let document = load_skill_document(catalog.get("review").unwrap()).unwrap();
        let context = skill_context_text(&document, Some("Focus on tests."));

        assert!(context.contains("name: review"));
        assert!(context.contains("Do review."));
        assert!(context.contains("Invocation state: this skill is already loaded"));
        assert!(context.contains("## Additional context\n\nFocus on tests."));
    }

    /// Verifies explicit skill prompt parsing keeps the skill token distinct
    /// from the trailing semantic argument.
    #[test]
    fn skill_prompt_invocation_parses_name_and_argument() {
        let invocation = parse_skill_prompt_invocation("  $review focus src/lib.rs").unwrap();

        assert_eq!(invocation.name, "review");
        assert_eq!(
            invocation.additional_context.as_deref(),
            Some("focus src/lib.rs")
        );
    }

    /// Verifies skill names follow the OpenAI-compatible lowercase hyphenated
    /// grammar used by discovery, prompt syntax, and MAAP calls.
    #[test]
    fn skill_name_validation_rejects_paths_and_uppercase() {
        assert!(is_valid_skill_name("openai-docs"));
        assert!(is_valid_skill_name("a1"));
        assert!(!is_valid_skill_name("../skill"));
        assert!(!is_valid_skill_name("OpenAI"));
        assert!(!is_valid_skill_name("-"));
    }
}
