//! Agent macro discovery, parsing, and prompt invocation helpers.
//!
//! Agent macros are ordered prompt workflows stored below the user
//! configuration root or below a trusted project's `.mezzanine` directory. This
//! module keeps catalog discovery deterministic and side-effect free: it reads
//! `MACRO.md` metadata, validates the configured layout, parses ordered prompt
//! steps, and never executes macro content.

use crate::{MezError, MezErrorKind, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Directory name that contains user or project macros.
pub const MACROS_DIRECTORY_NAME: &str = "macros";
/// File name that carries one macro's metadata and ordered prompt steps.
pub const MACRO_FILE_NAME: &str = "MACRO.md";
/// Markdown heading that starts the required ordered prompt-step section.
pub const MACRO_STEPS_HEADING: &str = "## Steps";
/// Maximum accepted `MACRO.md` size in bytes.
pub const MAX_MACRO_FILE_BYTES: u64 = 256 * 1024;
/// Maximum accepted prompt steps in one macro definition.
pub const MAX_MACRO_STEPS: usize = 128;

/// Source scope for one effective macro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroSource {
    /// Macro from the primary user configuration directory.
    User,
    /// Macro from a trusted project configuration directory.
    Project,
}

impl MacroSource {
    /// Returns the stable model-facing scope name for this source.
    pub fn as_str(self) -> &'static str {
        match self {
            MacroSource::User => "user",
            MacroSource::Project => "project",
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
    /// Absolute or caller-supplied path to the backing `MACRO.md` file.
    pub path: PathBuf,
    /// Number of prompt steps parsed from the `## Steps` section.
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
    ///
    /// # Parameters
    /// - `name`: Macro name to resolve from the effective catalog.
    pub fn get(&self, name: &str) -> Option<&MacroSummary> {
        self.macros
            .iter()
            .find(|macro_summary| macro_summary.name == name)
    }

    /// Returns macro names in deterministic catalog order.
    pub fn names(&self) -> Vec<String> {
        self.macros
            .iter()
            .map(|macro_summary| macro_summary.name.clone())
            .collect()
    }

    /// Builds compact model-facing catalog text.
    pub fn model_catalog_text(&self) -> String {
        let mut lines = Vec::new();
        if self.macros.is_empty() {
            lines.push("No macros are currently available.".to_string());
        } else {
            lines.push("Available macros:".to_string());
            lines.extend(self.macros.iter().map(|macro_summary| {
                format!(
                    "- {} ({}, {} steps) - {}",
                    macro_summary.name,
                    macro_summary.source.as_str(),
                    macro_summary.step_count,
                    macro_summary.description
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
            "macros": self.macros.iter().map(|macro_summary| {
                serde_json::json!({
                    "name": macro_summary.name,
                    "description": macro_summary.description,
                    "source": macro_summary.source.as_str(),
                    "path": macro_summary.path.to_string_lossy(),
                    "step_count": macro_summary.step_count,
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
    /// One-based step index from the ordered `## Steps` list.
    pub index: usize,
    /// Prompt text submitted to the macro subagent, possibly adapted later.
    pub prompt: String,
}

/// Parsed explicit `#<macro-name>` agent prompt invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroPromptInvocation {
    /// Macro name after the leading `#`.
    pub name: String,
    /// Optional trailing prompt text used as user-stated invocation context.
    pub additional_context: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MacroFrontMatter {
    /// Stable macro identifier declared by the macro author.
    name: String,
    /// Short model-facing description of when the macro should be used.
    description: String,
}

/// Discovers the effective macro catalog for one user/project context.
///
/// # Parameters
/// - `user_config_root`: Primary Mezzanine configuration root, when known.
/// - `project_root`: Trusted project root for the active pane, when known.
pub fn discover_macro_catalog(
    user_config_root: Option<&Path>,
    project_root: Option<&Path>,
) -> MacroCatalog {
    let mut macros = BTreeMap::<String, MacroSummary>::new();
    let mut diagnostics = Vec::new();
    if let Some(root) = user_config_root {
        discover_macros_under_root(
            &root.join(MACROS_DIRECTORY_NAME),
            MacroSource::User,
            &mut macros,
            &mut diagnostics,
        );
    }
    if let Some(root) = project_root {
        discover_macros_under_root(
            &root.join(".mezzanine").join(MACROS_DIRECTORY_NAME),
            MacroSource::Project,
            &mut macros,
            &mut diagnostics,
        );
    }
    MacroCatalog {
        macros: macros.into_values().collect(),
        diagnostics,
    }
}

/// Loads the full markdown and parsed steps for one macro summary.
///
/// # Parameters
/// - `summary`: Macro metadata returned by `discover_macro_catalog`.
pub fn load_macro_definition(summary: &MacroSummary) -> Result<MacroDefinition> {
    let text = read_macro_text(&summary.path).map_err(|error| {
        MezError::new(
            MezErrorKind::Io,
            format!(
                "failed to read macro {} from {}: {}",
                summary.name,
                summary.path.display(),
                error
            ),
        )
    })?;
    let (_front_matter, body) = split_macro_front_matter(&text).map_err(|error| {
        MezError::invalid_args(format!(
            "failed to parse macro {} from {}: {}",
            summary.name,
            summary.path.display(),
            error
        ))
    })?;
    let steps = parse_macro_steps(body).map_err(|error| {
        MezError::invalid_args(format!(
            "failed to parse macro steps for {} from {}: {}",
            summary.name,
            summary.path.display(),
            error
        ))
    })?;
    Ok(MacroDefinition {
        summary: summary.clone(),
        text,
        steps,
    })
}

/// Parses explicit `#<macro-name>` agent prompt syntax.
///
/// # Parameters
/// - `input`: User-submitted pane-local agent prompt text.
pub fn parse_macro_prompt_invocation(input: &str) -> Option<MacroPromptInvocation> {
    let trimmed = input.trim_start();
    let remainder = trimmed.strip_prefix('#')?;
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
    Some(MacroPromptInvocation {
        name: name.to_string(),
        additional_context,
    })
}

/// Discovers valid direct child macro directories below one `macros` root.
///
/// # Parameters
/// - `root`: Directory containing one subdirectory per macro.
/// - `source`: Source scope assigned to discovered macro summaries.
/// - `macros`: Effective macro map updated by macro name.
/// - `diagnostics`: Non-fatal discovery diagnostics appended for skipped paths.
fn discover_macros_under_root(
    root: &Path,
    source: MacroSource,
    macros: &mut BTreeMap<String, MacroSummary>,
    diagnostics: &mut Vec<MacroDiagnostic>,
) {
    let metadata = match fs::metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            diagnostics.push(MacroDiagnostic {
                path: root.to_path_buf(),
                message: format!("macro root is unreadable: {error}"),
            });
            return;
        }
    };
    if !metadata.is_dir() {
        diagnostics.push(MacroDiagnostic {
            path: root.to_path_buf(),
            message: "macro root is not a directory".to_string(),
        });
        return;
    }
    let mut entries = match fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .collect::<Vec<_>>(),
        Err(error) => {
            diagnostics.push(MacroDiagnostic {
                path: root.to_path_buf(),
                message: format!("macro root could not be listed: {error}"),
            });
            return;
        }
    };
    entries.sort();
    for path in entries {
        let macro_path = path.join(MACRO_FILE_NAME);
        match read_macro_summary(&path, &macro_path, source) {
            Ok(summary) => insert_macro_summary(macros, diagnostics, summary),
            Err(message) => diagnostics.push(MacroDiagnostic {
                path: macro_path,
                message,
            }),
        }
    }
}

/// Inserts one discovered macro while reporting name collisions and enforcing
/// source precedence.
fn insert_macro_summary(
    macros: &mut BTreeMap<String, MacroSummary>,
    diagnostics: &mut Vec<MacroDiagnostic>,
    summary: MacroSummary,
) {
    if let Some(existing) = macros.get(&summary.name) {
        let replace =
            macro_source_precedence(summary.source) > macro_source_precedence(existing.source);
        diagnostics.push(MacroDiagnostic {
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
    }
    macros.insert(summary.name.clone(), summary);
}

/// Returns deterministic macro-source precedence from lowest to highest.
fn macro_source_precedence(source: MacroSource) -> u8 {
    match source {
        MacroSource::User => 1,
        MacroSource::Project => 2,
    }
}

/// Reads and validates one candidate macro directory.
///
/// # Parameters
/// - `directory`: Macro directory whose basename must match the declared name.
/// - `macro_path`: Path to the candidate `MACRO.md` file.
/// - `source`: Source scope assigned to the resulting macro.
fn read_macro_summary(
    directory: &Path,
    macro_path: &Path,
    source: MacroSource,
) -> std::result::Result<MacroSummary, String> {
    if !directory.is_dir() {
        return Err("macro entry is not a directory".to_string());
    }
    let directory_name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "macro directory name is not valid UTF-8".to_string())?;
    if !is_valid_macro_name(directory_name) {
        return Err(format!(
            "macro directory name {directory_name:?} is invalid"
        ));
    }
    let text = read_macro_text(macro_path)?;
    let (front_matter, body) = split_macro_front_matter(&text)?;
    let front_matter: MacroFrontMatter = serde_norway::from_str(front_matter)
        .map_err(|error| format!("failed to parse MACRO.md front matter: {error}"))?;
    if !is_valid_macro_name(&front_matter.name) {
        return Err(format!("macro name {:?} is invalid", front_matter.name));
    }
    if front_matter.name != directory_name {
        return Err(format!(
            "macro name {:?} does not match directory {:?}",
            front_matter.name, directory_name
        ));
    }
    if front_matter.description.trim().is_empty() {
        return Err("macro description must not be empty".to_string());
    }
    let steps = parse_macro_steps(body)?;
    Ok(MacroSummary {
        name: front_matter.name,
        description: front_matter.description.trim().to_string(),
        source,
        path: macro_path.to_path_buf(),
        step_count: steps.len(),
    })
}

/// Reads one macro file after bounding its size.
///
/// # Parameters
/// - `macro_path`: Path to the candidate `MACRO.md` file.
fn read_macro_text(macro_path: &Path) -> std::result::Result<String, String> {
    let metadata =
        fs::metadata(macro_path).map_err(|error| format!("failed to inspect MACRO.md: {error}"))?;
    if !metadata.is_file() {
        return Err("MACRO.md is not a regular file".to_string());
    }
    if metadata.len() > MAX_MACRO_FILE_BYTES {
        return Err(format!(
            "MACRO.md is {} bytes, which exceeds the {} byte limit",
            metadata.len(),
            MAX_MACRO_FILE_BYTES
        ));
    }
    fs::read_to_string(macro_path).map_err(|error| format!("failed to read MACRO.md: {error}"))
}

/// Splits a markdown macro file into YAML front matter and body text.
///
/// # Parameters
/// - `text`: Complete `MACRO.md` contents.
fn split_macro_front_matter(text: &str) -> std::result::Result<(&str, &str), String> {
    let normalized = text
        .strip_prefix("---\r\n")
        .or_else(|| text.strip_prefix("---\n"));
    let Some(after_open) = normalized else {
        return Err("MACRO.md must start with YAML front matter".to_string());
    };
    for marker in ["\n---\n", "\n---\r\n", "\r\n---\r\n", "\r\n---\n"] {
        if let Some(index) = after_open.find(marker) {
            let front_matter = &after_open[..index];
            let body = &after_open[index + marker.len()..];
            return Ok((front_matter, body));
        }
    }
    Err("MACRO.md front matter is not closed".to_string())
}

/// Parses the required `## Steps` ordered list from a macro body.
///
/// # Parameters
/// - `body`: Markdown body text after `MACRO.md` front matter.
fn parse_macro_steps(body: &str) -> std::result::Result<Vec<MacroStep>, String> {
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
        if trimmed.starts_with('#') {
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
            return Err("Steps section must contain ordered-list prompt steps".to_string());
        }
        if trimmed.is_empty() {
            current_lines.push(String::new());
        } else if leading_ascii_whitespace_len(line) > current_indent {
            current_lines.push(trimmed.to_string());
        } else {
            return Err(format!(
                "line {:?} in Steps is not an ordered item or indented continuation",
                trimmed
            ));
        }
    }
    if !in_steps {
        return Err("MACRO.md must contain a ## Steps section".to_string());
    }
    push_current_macro_step(&mut steps, &mut current_lines)?;
    if steps.is_empty() {
        return Err("Steps section must contain at least one prompt step".to_string());
    }
    Ok(steps)
}

/// Pushes accumulated step lines into the parsed step vector.
///
/// # Parameters
/// - `steps`: Parsed step accumulator receiving the next step.
/// - `current_lines`: Accumulated lines for the current step prompt.
fn push_current_macro_step(
    steps: &mut Vec<MacroStep>,
    current_lines: &mut Vec<String>,
) -> std::result::Result<(), String> {
    if current_lines.is_empty() {
        return Ok(());
    }
    if steps.len() == MAX_MACRO_STEPS {
        return Err(format!(
            "macro contains more than {MAX_MACRO_STEPS} prompt steps"
        ));
    }
    let prompt = normalized_step_prompt(current_lines);
    if prompt.trim().is_empty() {
        return Err(format!(
            "macro step {} prompt must not be empty",
            steps.len() + 1
        ));
    }
    steps.push(MacroStep {
        index: steps.len() + 1,
        prompt,
    });
    current_lines.clear();
    Ok(())
}

/// Returns a step prompt with surrounding blank lines removed.
///
/// # Parameters
/// - `lines`: Accumulated ordered-list item and continuation lines.
fn normalized_step_prompt(lines: &[String]) -> String {
    let mut start = 0usize;
    let mut end = lines.len();
    while start < end && lines[start].trim().is_empty() {
        start += 1;
    }
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    lines[start..end].join("\n")
}

/// Parses one markdown ordered-list item marker.
///
/// # Parameters
/// - `line`: Candidate markdown line from the `## Steps` section.
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

/// Counts leading ASCII indentation bytes in one line.
///
/// # Parameters
/// - `line`: Candidate markdown line.
fn leading_ascii_whitespace_len(line: &str) -> usize {
    line.as_bytes()
        .iter()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count()
}

/// Returns whether a string satisfies Mezzanine's macro-name grammar.
///
/// # Parameters
/// - `name`: Candidate macro name.
pub fn is_valid_macro_name(name: &str) -> bool {
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
        MacroSource, discover_macro_catalog, is_valid_macro_name, load_macro_definition,
        parse_macro_prompt_invocation,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Creates a unique temporary root for macro-discovery tests without
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
            "mez-macros-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// Writes one valid macro fixture into the requested root.
    ///
    /// # Parameters
    /// - `root`: The macros root receiving the fixture directory.
    /// - `name`: Macro name and directory basename.
    /// - `description`: Front matter description to store.
    /// - `steps`: Ordered-list item bodies to store under `## Steps`.
    fn write_macro(root: &Path, name: &str, description: &str, steps: &[&str]) {
        let directory = root.join(name);
        fs::create_dir_all(&directory).unwrap();
        let list = steps
            .iter()
            .enumerate()
            .map(|(index, step)| format!("{}. {step}", index + 1))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(
            directory.join("MACRO.md"),
            format!(
                "---\nname: {name}\ndescription: {description}\n---\n\n# Macro: {name}\n\n## Steps\n\n{list}\n"
            ),
        )
        .unwrap();
    }

    /// Verifies user and project macro roots share the same layout while
    /// project macros override user macros with the same name. This covers the
    /// precedence rule runtime invocation will rely on before creating macro
    /// subagent sessions.
    #[test]
    fn macro_catalog_discovers_roots_and_project_precedence() {
        let root = test_temp_root("precedence");
        let user_root = root.join("user");
        let project_root = root.join("repo");
        write_macro(
            &user_root.join("macros"),
            "ship-it",
            "User release workflow",
            &["Summarize user release notes."],
        );
        write_macro(
            &project_root.join(".mezzanine/macros"),
            "ship-it",
            "Project release workflow",
            &[
                "Summarize project release notes.",
                "Run /loop release checks.",
            ],
        );
        write_macro(
            &project_root.join(".mezzanine/macros"),
            "audit",
            "Audit workflow",
            &["Inspect the risky files."],
        );

        let catalog = discover_macro_catalog(Some(&user_root), Some(&project_root));

        assert_eq!(catalog.names(), vec!["audit", "ship-it"]);
        let overridden = catalog.get("ship-it").unwrap();
        assert_eq!(overridden.description, "Project release workflow");
        assert_eq!(overridden.source, MacroSource::Project);
        assert_eq!(overridden.step_count, 2);
        assert!(
            catalog
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("overrides existing"))
        );
    }

    /// Verifies the catalog rejects malformed macro entries without preventing
    /// valid sibling macros from being discovered. Macro definitions are
    /// user/project configuration content, so bad metadata must be isolated as
    /// diagnostics instead of crashing discovery.
    #[test]
    fn macro_catalog_reports_invalid_entries_without_failing_catalog() {
        let root = test_temp_root("invalid");
        let user_root = root.join("user");
        let invalid_directory = user_root.join("macros/BadName");
        fs::create_dir_all(&invalid_directory).unwrap();
        fs::write(
            invalid_directory.join("MACRO.md"),
            "---\nname: BadName\ndescription: Broken\n---\n\n## Steps\n\n1. Bad step.\n",
        )
        .unwrap();
        write_macro(
            &user_root.join("macros"),
            "valid-one",
            "Valid workflow",
            &["Do the valid work."],
        );

        let catalog = discover_macro_catalog(Some(&user_root), None);

        assert_eq!(catalog.names(), vec!["valid-one"]);
        assert_eq!(catalog.diagnostics.len(), 1);
        assert!(catalog.diagnostics[0].message.contains("is invalid"));
    }

    /// Verifies the required `## Steps` section accepts indented multiline
    /// ordered-list items. This protects macro prompt sequences whose steps are
    /// full prompts rather than short single-line labels.
    #[test]
    fn macro_definition_parses_multiline_ordered_steps() {
        let root = test_temp_root("multiline");
        let user_root = root.join("user");
        let directory = user_root.join("macros/release-check");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("MACRO.md"),
            "---\nname: release-check\ndescription: Check a release.\n---\n\n## Steps\n\n1. Inspect the release notes.\n   Summarize blockers and missing evidence.\n\n2. /loop run the release validation until stable.\n",
        )
        .unwrap();

        let catalog = discover_macro_catalog(Some(&user_root), None);
        let summary = catalog.get("release-check").unwrap();
        let definition = load_macro_definition(summary).unwrap();

        assert_eq!(summary.step_count, 2);
        assert_eq!(definition.steps[0].index, 1);
        assert_eq!(
            definition.steps[0].prompt,
            "Inspect the release notes.\nSummarize blockers and missing evidence."
        );
        assert!(definition.steps[1].prompt.starts_with("/loop run"));
        assert!(definition.text.contains("name: release-check"));
    }

    /// Verifies empty or missing prompt steps are rejected at catalog time.
    /// The runtime macro loop must never receive a definition that would start
    /// a persistent subagent session without at least one actionable prompt.
    #[test]
    fn macro_catalog_rejects_missing_or_empty_steps() {
        let root = test_temp_root("empty-steps");
        let user_root = root.join("user");
        let missing_steps = user_root.join("macros/missing-steps");
        fs::create_dir_all(&missing_steps).unwrap();
        fs::write(
            missing_steps.join("MACRO.md"),
            "---\nname: missing-steps\ndescription: Missing steps.\n---\n\nNo steps here.\n",
        )
        .unwrap();
        let empty_steps = user_root.join("macros/empty-steps");
        fs::create_dir_all(&empty_steps).unwrap();
        fs::write(
            empty_steps.join("MACRO.md"),
            "---\nname: empty-steps\ndescription: Empty steps.\n---\n\n## Steps\n\n1.   \n",
        )
        .unwrap();

        let catalog = discover_macro_catalog(Some(&user_root), None);

        assert!(catalog.macros.is_empty());
        assert_eq!(catalog.diagnostics.len(), 2);
        assert!(catalog.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("must contain a ## Steps section")
        }));
        assert!(catalog.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("macro step 1 prompt must not be empty")
        }));
    }

    /// Verifies model-facing and structured catalog representations include
    /// source, description, path, and step count details required by listing and
    /// prompt-expansion surfaces.
    #[test]
    fn macro_catalog_formats_text_and_structured_json() {
        let root = test_temp_root("format");
        let user_root = root.join("user");
        write_macro(
            &user_root.join("macros"),
            "review-release",
            "Review a release",
            &["Inspect release notes.", "Summarize release risk."],
        );

        let catalog = discover_macro_catalog(Some(&user_root), None);
        let text = catalog.model_catalog_text();
        let json = catalog.structured_json();

        assert!(text.contains("- review-release (user, 2 steps) - Review a release"));
        assert!(json.contains(r#""name":"review-release""#));
        assert!(json.contains(r#""step_count":2"#));
    }

    /// Verifies macro prompt invocation is recognized only at the start of the
    /// agent prompt and preserves trailing context as the user-stated argument
    /// for later main-model step adaptation.
    #[test]
    fn macro_prompt_invocation_parses_name_and_context() {
        let invocation = parse_macro_prompt_invocation("  #release-check for v1.2").unwrap();

        assert_eq!(invocation.name, "release-check");
        assert_eq!(invocation.additional_context.as_deref(), Some("for v1.2"));
        assert!(parse_macro_prompt_invocation("hello #release-check").is_none());
        assert!(parse_macro_prompt_invocation("#").is_none());
    }

    /// Verifies macro-name validation matches the specified safe identifier
    /// shape and rejects path-like or uppercase names before filesystem paths
    /// can be used as macro identities.
    #[test]
    fn macro_name_validation_rejects_paths_and_uppercase() {
        assert!(is_valid_macro_name("release-check-2"));
        assert!(!is_valid_macro_name("Release"));
        assert!(!is_valid_macro_name("release/check"));
        assert!(!is_valid_macro_name(".."));
        assert!(!is_valid_macro_name("---"));
    }
}
