//! Agent macro discovery, parsing, and prompt invocation helpers.
//!
//! Agent macros are ordered prompt workflows stored below the user
//! configuration root or below a trusted project's `.mezzanine` directory. This
//! module keeps catalog discovery deterministic and side-effect free: it reads
//! `MACRO.md` metadata, validates the configured layout, parses ordered prompt
//! steps, and never executes macro content.

use crate::{MezError, MezErrorKind, Result};
use include_dir::{Dir, include_dir};
use std::fs;
use std::path::{Path, PathBuf};

/// Directory name that contains user or project macros.
pub const MACROS_DIRECTORY_NAME: &str = "macros";
/// Virtual path prefix used for built-in macros embedded in the binary.
pub const BUILTIN_MACRO_PATH_PREFIX: &str = "<builtin>";

/// Embedded built-in macro directory shipped with the crate.
static BUILTIN_MACROS: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/src/integrations/macros/builtin");

use mez_agent::parse_macro_document;
use mez_agent::{
    MACRO_FILE_NAME, MAX_MACRO_FILE_BYTES, MacroCatalog, MacroDefinition, MacroDiagnostic,
    MacroSource, MacroSummary, is_valid_macro_name,
};

/// Discovers the effective macro catalog for one user/project context.
///
/// # Parameters
/// - `user_config_root`: Primary Mezzanine configuration root, when known.
/// - `project_root`: Trusted project root for the active pane, when known.
pub fn discover_macro_catalog(
    user_config_root: Option<&Path>,
    project_root: Option<&Path>,
) -> MacroCatalog {
    let mut catalog = MacroCatalog::default();
    discover_builtin_macros(&mut catalog);
    if let Some(root) = user_config_root {
        discover_macros_under_root(
            &root.join(MACROS_DIRECTORY_NAME),
            MacroSource::User,
            &mut catalog,
        );
    }
    if let Some(root) = project_root {
        discover_macros_under_root(
            &root.join(".mezzanine").join(MACROS_DIRECTORY_NAME),
            MacroSource::Project,
            &mut catalog,
        );
    }
    catalog
}

/// Loads the full markdown and parsed steps for one macro summary.
///
/// # Parameters
/// - `summary`: Macro metadata returned by `discover_macro_catalog`.
pub fn load_macro_definition(summary: &MacroSummary) -> Result<MacroDefinition> {
    let text = match summary.source {
        MacroSource::Builtin => read_builtin_macro_text(&summary.name).map_err(|error| {
            MezError::new(
                MezErrorKind::Io,
                format!(
                    "failed to read built-in macro {} from {}: {}",
                    summary.name,
                    summary.path.display(),
                    error
                ),
            )
        })?,
        MacroSource::User | MacroSource::Project => {
            read_macro_text(&summary.path).map_err(|error| {
                MezError::new(
                    MezErrorKind::Io,
                    format!(
                        "failed to read macro {} from {}: {}",
                        summary.name,
                        summary.path.display(),
                        error
                    ),
                )
            })?
        }
    };
    let document = parse_macro_document(&text).map_err(|error| {
        MezError::invalid_args(format!(
            "failed to parse macro {} from {}: {}",
            summary.name,
            summary.path.display(),
            error
        ))
    })?;
    Ok(MacroDefinition {
        summary: summary.clone(),
        text,
        steps: document.steps,
    })
}

/// Discovers valid direct child macro directories below one `macros` root.
///
/// # Parameters
/// - `root`: Directory containing one subdirectory per macro.
/// - `source`: Source scope assigned to discovered macro summaries.
/// - `catalog`: Effective lower-owned catalog updated with summaries and diagnostics.
fn discover_macros_under_root(root: &Path, source: MacroSource, catalog: &mut MacroCatalog) {
    let metadata = match fs::metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            catalog.diagnostics.push(MacroDiagnostic {
                path: root.to_path_buf(),
                message: format!("macro root is unreadable: {error}"),
            });
            return;
        }
    };
    if !metadata.is_dir() {
        catalog.diagnostics.push(MacroDiagnostic {
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
            catalog.diagnostics.push(MacroDiagnostic {
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
            Ok(summary) => catalog.insert(summary),
            Err(message) => catalog.diagnostics.push(MacroDiagnostic {
                path: macro_path,
                message,
            }),
        }
    }
}

/// Discovers macros embedded below `src/macros/builtin`.
fn discover_builtin_macros(catalog: &mut MacroCatalog) {
    let mut directories = BUILTIN_MACROS.dirs().collect::<Vec<_>>();
    directories.sort_by(|left, right| left.path().cmp(right.path()));
    for directory in directories {
        match read_builtin_macro_summary(directory) {
            Ok(summary) => catalog.insert(summary),
            Err(message) => catalog.diagnostics.push(MacroDiagnostic {
                path: PathBuf::from(BUILTIN_MACRO_PATH_PREFIX)
                    .join(directory.path())
                    .join(MACRO_FILE_NAME),
                message,
            }),
        }
    }
}

/// Reads and validates one embedded built-in macro directory.
fn read_builtin_macro_summary(directory: &Dir<'_>) -> std::result::Result<MacroSummary, String> {
    let directory_name = directory
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "macro directory name is not valid UTF-8".to_string())?;
    if !is_valid_macro_name(directory_name) {
        return Err(format!(
            "macro directory name {directory_name:?} is invalid"
        ));
    }
    let text = read_builtin_macro_text(directory_name)?;
    let document = parse_macro_document(&text).map_err(|error| error.message().to_string())?;
    if document.name != directory_name {
        return Err(format!(
            "macro name {:?} does not match directory {:?}",
            document.name, directory_name
        ));
    }
    let name = document.name;
    Ok(MacroSummary {
        path: builtin_macro_path(&name),
        name,
        description: document.description,
        source: MacroSource::Builtin,
        step_count: document.steps.len(),
    })
}

/// Returns the virtual path used for an embedded built-in macro.
fn builtin_macro_path(name: &str) -> PathBuf {
    PathBuf::from(BUILTIN_MACRO_PATH_PREFIX)
        .join(name)
        .join(MACRO_FILE_NAME)
}

/// Returns one embedded built-in macro's `MACRO.md` contents.
fn read_builtin_macro_text(name: &str) -> std::result::Result<String, String> {
    if !is_valid_macro_name(name) {
        return Err(format!("macro name {name:?} is invalid"));
    }
    let path = Path::new(name).join(MACRO_FILE_NAME);
    let file = BUILTIN_MACROS
        .get_file(path)
        .ok_or_else(|| "failed to inspect MACRO.md: embedded file is missing".to_string())?;
    if file.contents().len() as u64 > MAX_MACRO_FILE_BYTES {
        return Err(format!(
            "MACRO.md is {} bytes, which exceeds the {} byte limit",
            file.contents().len(),
            MAX_MACRO_FILE_BYTES
        ));
    }
    file.contents_utf8()
        .map(ToString::to_string)
        .ok_or_else(|| "failed to read MACRO.md: embedded file is not valid UTF-8".to_string())
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
    let document = parse_macro_document(&text).map_err(|error| error.message().to_string())?;
    if document.name != directory_name {
        return Err(format!(
            "macro name {:?} does not match directory {:?}",
            document.name, directory_name
        ));
    }
    Ok(MacroSummary {
        name: document.name,
        description: document.description,
        source,
        path: macro_path.to_path_buf(),
        step_count: document.steps.len(),
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

#[cfg(test)]
mod tests {
    use super::{BUILTIN_MACROS, MacroSource, discover_macro_catalog, load_macro_definition};
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

    /// Verifies the built-in macro asset directory is embedded even while it
    /// carries only documentation. This locks the `include_dir` root without
    /// changing the effective catalog until a real built-in macro is added.
    #[test]
    fn macro_catalog_embeds_empty_builtin_macro_directory_without_catalog_entries() {
        assert!(BUILTIN_MACROS.get_file("README.md").is_some());

        let catalog = discover_macro_catalog(None, None);

        assert!(catalog.macros.is_empty());
        assert!(catalog.diagnostics.is_empty());
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
}
