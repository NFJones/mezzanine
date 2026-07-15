//! Provider-independent agent macro contracts and parsing.
//!
//! This module owns macro identities, deterministic catalog precedence,
//! model-facing catalog projection, `MACRO.md` parsing, ordered prompt-step
//! validation, and explicit `#macro` invocation syntax. Filesystem discovery,
//! embedded product assets, and product error projection remain in the
//! composition crate.

use serde::Deserialize;
use std::fmt;
use std::path::PathBuf;

/// File name that carries one macro's metadata and ordered prompt steps.
pub const MACRO_FILE_NAME: &str = "MACRO.md";
/// Markdown heading that starts the required ordered prompt-step section.
pub const MACRO_STEPS_HEADING: &str = "## Steps";
/// Maximum accepted `MACRO.md` size in bytes.
pub const MAX_MACRO_FILE_BYTES: u64 = 256 * 1024;
/// Maximum accepted prompt steps in one macro definition.
pub const MAX_MACRO_STEPS: usize = 128;

/// Provider-independent macro contract failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroContractError {
    message: String,
}

impl MacroContractError {
    /// Creates a macro contract failure with a stable diagnostic.
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

impl fmt::Display for MacroContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MacroContractError {}

/// Source scope for one effective macro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroSource {
    /// Macro shipped with Mezzanine.
    Builtin,
    /// Macro from the primary user configuration directory.
    User,
    /// Macro from a trusted project configuration directory.
    Project,
}

impl MacroSource {
    /// Returns the stable model-facing scope name for this source.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::User => "user",
            Self::Project => "project",
        }
    }

    fn precedence(self) -> u8 {
        match self {
            Self::Builtin => 0,
            Self::User => 1,
            Self::Project => 2,
        }
    }
}

/// Catalog metadata for one available macro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroSummary {
    /// Macro identifier from `MACRO.md` front matter.
    pub name: String,
    /// Short usage description from `MACRO.md` front matter.
    pub description: String,
    /// Effective source scope for this macro.
    pub source: MacroSource,
    /// Path supplied by the product discovery adapter.
    pub path: PathBuf,
    /// Number of parsed prompt steps.
    pub step_count: usize,
}

impl MacroSummary {
    /// Returns a human-facing source label.
    pub fn attribution_label(&self) -> String {
        self.source.as_str().to_string()
    }
}

/// Non-fatal discovery diagnostic for an invalid or unreadable macro path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroDiagnostic {
    /// Path whose macro metadata could not be used.
    pub path: PathBuf,
    /// Human-readable reason the path was skipped.
    pub message: String,
}

/// Effective macro catalog for one pane/project context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MacroCatalog {
    /// Deterministically ordered effective macros.
    pub macros: Vec<MacroSummary>,
    /// Non-fatal discovery diagnostics.
    pub diagnostics: Vec<MacroDiagnostic>,
}

impl MacroCatalog {
    /// Returns a macro summary by exact name.
    pub fn get(&self, name: &str) -> Option<&MacroSummary> {
        self.macros.iter().find(|summary| summary.name == name)
    }

    /// Returns macro names in deterministic catalog order.
    pub fn names(&self) -> Vec<String> {
        self.macros
            .iter()
            .map(|summary| summary.name.clone())
            .collect()
    }

    /// Inserts one discovered summary using deterministic source precedence.
    pub fn insert(&mut self, summary: MacroSummary) {
        if let Some(index) = self
            .macros
            .iter()
            .position(|existing| existing.name == summary.name)
        {
            let existing = &self.macros[index];
            let replace = summary.source.precedence() > existing.source.precedence();
            self.diagnostics.push(MacroDiagnostic {
                path: summary.path.clone(),
                message: if replace {
                    format!(
                        "macro name {:?} from {} overrides existing {} entry",
                        summary.name,
                        summary.attribution_label(),
                        existing.attribution_label()
                    )
                } else {
                    format!(
                        "macro name {:?} from {} ignored because existing {} entry has precedence",
                        summary.name,
                        summary.attribution_label(),
                        existing.attribution_label()
                    )
                },
            });
            if !replace {
                return;
            }
            self.macros[index] = summary;
        } else {
            self.macros.push(summary);
        }
        self.macros
            .sort_by(|left, right| left.name.cmp(&right.name));
    }

    /// Builds compact model-facing catalog text.
    pub fn model_catalog_text(&self) -> String {
        let mut lines = Vec::new();
        if self.macros.is_empty() {
            lines.push("No macros are currently available.".to_string());
        } else {
            lines.push("Available macros:".to_string());
            lines.extend(self.macros.iter().map(|summary| {
                format!(
                    "- {} ({}, {} steps) - {}",
                    summary.name,
                    summary.source.as_str(),
                    summary.step_count,
                    summary.description
                )
            }));
        }
        if !self.diagnostics.is_empty() {
            lines.push(String::new());
            lines.push("Skipped macro diagnostics:".to_string());
            lines.extend(self.diagnostics.iter().map(|diagnostic| {
                format!("- {} - {}", diagnostic.path.display(), diagnostic.message)
            }));
        }
        lines.join("\n")
    }

    /// Builds structured JSON for action-result metadata.
    pub fn structured_json(&self) -> String {
        serde_json::json!({
            "macros": self.macros.iter().map(|summary| {
                serde_json::json!({
                    "name": summary.name,
                    "description": summary.description,
                    "source": summary.source.as_str(),
                    "path": summary.path.to_string_lossy(),
                    "step_count": summary.step_count,
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

/// Full macro document loaded for an explicit invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroDefinition {
    /// Catalog metadata for the loaded macro.
    pub summary: MacroSummary,
    /// Complete raw `MACRO.md` text.
    pub text: String,
    /// Parsed ordered prompt steps.
    pub steps: Vec<MacroStep>,
}

/// One parsed macro prompt step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroStep {
    /// One-based step index from the ordered list.
    pub index: usize,
    /// Prompt text submitted to the macro subagent.
    pub prompt: String,
}

/// Parsed explicit `#<macro-name>` agent prompt invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroPromptInvocation {
    /// Macro name after the leading `#`.
    pub name: String,
    /// Optional trailing user-stated invocation context.
    pub additional_context: Option<String>,
}

/// Parsed dependency-neutral `MACRO.md` content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMacroDocument {
    /// Validated front-matter macro name.
    pub name: String,
    /// Trimmed non-empty front-matter description.
    pub description: String,
    /// Validated ordered prompt steps.
    pub steps: Vec<MacroStep>,
}

#[derive(Debug, Deserialize)]
struct MacroFrontMatter {
    name: String,
    description: String,
}

/// Parses and validates one complete `MACRO.md` document.
pub fn parse_macro_document(text: &str) -> Result<ParsedMacroDocument, MacroContractError> {
    let (front_matter, body) = split_macro_front_matter(text)?;
    let front_matter: MacroFrontMatter = serde_norway::from_str(front_matter).map_err(|error| {
        MacroContractError::new(format!("failed to parse MACRO.md front matter: {error}"))
    })?;
    if !is_valid_macro_name(&front_matter.name) {
        return Err(MacroContractError::new(format!(
            "macro name {:?} is invalid",
            front_matter.name
        )));
    }
    if front_matter.description.trim().is_empty() {
        return Err(MacroContractError::new(
            "macro description must not be empty",
        ));
    }
    Ok(ParsedMacroDocument {
        name: front_matter.name,
        description: front_matter.description.trim().to_string(),
        steps: parse_macro_steps(body)?,
    })
}

/// Parses explicit `#<macro-name>` agent prompt syntax.
pub fn parse_macro_prompt_invocation(input: &str) -> Option<MacroPromptInvocation> {
    let trimmed = input.trim_start();
    let remainder = trimmed.strip_prefix('#')?;
    let name_end = remainder
        .char_indices()
        .find_map(|(index, character)| character.is_whitespace().then_some(index))
        .unwrap_or(remainder.len());
    let name = remainder[..name_end].trim();
    if name.is_empty() {
        return None;
    }
    let argument = remainder[name_end..].trim();
    Some(MacroPromptInvocation {
        name: name.to_string(),
        additional_context: (!argument.is_empty()).then(|| argument.to_string()),
    })
}

/// Parses the required ordered prompt list from a macro body.
pub fn parse_macro_steps(body: &str) -> Result<Vec<MacroStep>, MacroContractError> {
    let mut in_steps = false;
    let mut steps = Vec::new();
    let mut current_indent = 0usize;
    let mut current_lines = Vec::<String>::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if !in_steps {
            if trimmed == MACRO_STEPS_HEADING {
                in_steps = true;
            }
            continue;
        }
        if line.starts_with('#') && is_markdown_section_heading(line) {
            break;
        }
        if let Some((indent, content)) = parse_ordered_list_item(line) {
            push_current_macro_step(&mut steps, &mut current_lines)?;
            current_indent = indent;
            current_lines.push(content);
            continue;
        }
        if current_lines.is_empty() {
            if trimmed.is_empty() {
                continue;
            }
            return Err(MacroContractError::new(
                "Steps section must contain ordered-list prompt steps",
            ));
        }
        if trimmed.is_empty() {
            current_lines.push(String::new());
        } else if leading_ascii_whitespace_len(line) > current_indent {
            current_lines.push(trimmed.to_string());
        } else {
            return Err(MacroContractError::new(format!(
                "line {:?} in Steps is not an ordered item or indented continuation",
                trimmed
            )));
        }
    }
    if !in_steps {
        return Err(MacroContractError::new(
            "MACRO.md must contain a ## Steps section",
        ));
    }
    push_current_macro_step(&mut steps, &mut current_lines)?;
    if steps.is_empty() {
        return Err(MacroContractError::new(
            "Steps section must contain at least one prompt step",
        ));
    }
    Ok(steps)
}

/// Returns whether a macro name is a safe lowercase identifier.
pub fn is_valid_macro_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && name
            .bytes()
            .any(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn split_macro_front_matter(text: &str) -> Result<(&str, &str), MacroContractError> {
    let normalized = text
        .strip_prefix("---\r\n")
        .or_else(|| text.strip_prefix("---\n"));
    let Some(after_open) = normalized else {
        return Err(MacroContractError::new(
            "MACRO.md must start with YAML front matter",
        ));
    };
    for marker in ["\n---\n", "\n---\r\n", "\r\n---\r\n", "\r\n---\n"] {
        if let Some(index) = after_open.find(marker) {
            return Ok((&after_open[..index], &after_open[index + marker.len()..]));
        }
    }
    Err(MacroContractError::new(
        "MACRO.md front matter is not closed",
    ))
}

fn is_markdown_section_heading(line: &str) -> bool {
    let trimmed = line.trim_end();
    let hash_count = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if hash_count == 0 || hash_count > 6 {
        return false;
    }
    let after_hashes = &trimmed[hash_count..];
    after_hashes.is_empty() || after_hashes.starts_with(' ')
}

fn push_current_macro_step(
    steps: &mut Vec<MacroStep>,
    current_lines: &mut Vec<String>,
) -> Result<(), MacroContractError> {
    if current_lines.is_empty() {
        return Ok(());
    }
    if steps.len() == MAX_MACRO_STEPS {
        return Err(MacroContractError::new(format!(
            "macro contains more than {MAX_MACRO_STEPS} prompt steps"
        )));
    }
    let prompt = normalized_step_prompt(current_lines);
    if prompt.trim().is_empty() {
        return Err(MacroContractError::new(format!(
            "macro step {} prompt must not be empty",
            steps.len() + 1
        )));
    }
    steps.push(MacroStep {
        index: steps.len() + 1,
        prompt,
    });
    current_lines.clear();
    Ok(())
}

fn normalized_step_prompt(lines: &[String]) -> String {
    let first = lines.iter().position(|line| !line.trim().is_empty());
    let last = lines.iter().rposition(|line| !line.trim().is_empty());
    match (first, last) {
        (Some(first), Some(last)) => lines[first..=last].join("\n"),
        _ => String::new(),
    }
}

fn parse_ordered_list_item(line: &str) -> Option<(usize, String)> {
    let indent = leading_ascii_whitespace_len(line);
    let rest = &line[indent..];
    let bytes = rest.as_bytes();
    let mut digit_end = 0usize;
    while digit_end < bytes.len() && bytes[digit_end].is_ascii_digit() {
        digit_end += 1;
    }
    if digit_end == 0 || digit_end >= bytes.len() || !matches!(bytes[digit_end], b'.' | b')') {
        return None;
    }
    let content_start = digit_end + 1;
    if content_start < bytes.len() && !bytes[content_start].is_ascii_whitespace() {
        return None;
    }
    Some((indent, rest[content_start..].trim_start().to_string()))
}

fn leading_ascii_whitespace_len(line: &str) -> usize {
    line.bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(name: &str, source: MacroSource, description: &str) -> MacroSummary {
        MacroSummary {
            name: name.to_string(),
            description: description.to_string(),
            source,
            path: PathBuf::from(format!("/{}/{name}/{MACRO_FILE_NAME}", source.as_str())),
            step_count: 2,
        }
    }

    #[test]
    /// Verifies catalog insertion applies project-over-user-over-builtin
    /// precedence and records the winning collision diagnostic.
    fn macro_catalog_applies_source_precedence() {
        let mut catalog = MacroCatalog::default();
        catalog.insert(summary("ship-it", MacroSource::Builtin, "Built in"));
        catalog.insert(summary("ship-it", MacroSource::User, "User"));
        catalog.insert(summary("ship-it", MacroSource::Project, "Project"));

        assert_eq!(catalog.macros.len(), 1);
        assert_eq!(catalog.get("ship-it").unwrap().description, "Project");
        assert_eq!(catalog.diagnostics.len(), 2);
        assert!(
            catalog
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.message.contains("overrides existing"))
        );
    }

    #[test]
    /// Verifies model-facing and structured catalog representations include
    /// source, description, path, and step-count details.
    fn macro_catalog_formats_text_and_structured_json() {
        let mut catalog = MacroCatalog::default();
        catalog.insert(summary(
            "review-release",
            MacroSource::User,
            "Review a release",
        ));

        let text = catalog.model_catalog_text();
        let json = catalog.structured_json();

        assert!(text.contains("- review-release (user, 2 steps) - Review a release"));
        assert!(json.contains(r#""name":"review-release""#));
        assert!(json.contains(r#""step_count":2"#));
    }

    #[test]
    /// Verifies macro prompt invocation is recognized only at the start and
    /// preserves trailing user-stated context.
    fn macro_prompt_invocation_parses_name_and_context() {
        let invocation = parse_macro_prompt_invocation("  #release-check for v1.2").unwrap();

        assert_eq!(invocation.name, "release-check");
        assert_eq!(invocation.additional_context.as_deref(), Some("for v1.2"));
        assert!(parse_macro_prompt_invocation("hello #release-check").is_none());
        assert!(parse_macro_prompt_invocation("#").is_none());
    }

    #[test]
    /// Verifies macro-name validation accepts safe identifiers and rejects
    /// uppercase or path-like names.
    fn macro_name_validation_rejects_paths_and_uppercase() {
        assert!(is_valid_macro_name("release-check-2"));
        assert!(!is_valid_macro_name("Release"));
        assert!(!is_valid_macro_name("release/check"));
        assert!(!is_valid_macro_name(".."));
        assert!(!is_valid_macro_name("---"));
    }

    #[test]
    /// Verifies hash-prefixed continuation lines remain prompt text rather than
    /// being mistaken for section headings.
    fn macro_steps_accept_hash_prefixed_continuation_lines() {
        let body = "## Steps\n\n1. First step\n   # nested macro call\n2. Second step\n";
        let steps = parse_macro_steps(body).unwrap();

        assert_eq!(steps.len(), 2);
        assert!(steps[0].prompt.contains("# nested macro call"));
        assert_eq!(steps[1].prompt, "Second step");
    }

    #[test]
    /// Verifies an actual Markdown section heading terminates ordered step
    /// parsing before subsequent sections can be ingested.
    fn macro_steps_heading_terminates_parsing() {
        let body = "## Steps\n\n1. First step\n2. Second step\n\n## Next Section\n\n3. Should not appear\n";
        let steps = parse_macro_steps(body).unwrap();

        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].prompt, "First step");
        assert_eq!(steps[1].prompt, "Second step");
    }
}
