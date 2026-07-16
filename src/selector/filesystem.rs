//! Filesystem candidate discovery, path heuristics, escaping, and home expansion.

use super::{
    Path, PathBuf, SelectorCandidate, SelectorCandidateKind, SelectorSurface, SelectorTokenContext,
    canonical_agent_command, fs, unescape_selector_shell_token,
};

/// Builds filesystem path candidates for command arguments.
pub(super) fn path_candidates(
    surface: SelectorSurface,
    context: &SelectorTokenContext,
    working_directory: Option<&Path>,
) -> Vec<SelectorCandidate> {
    if !path_completion_allowed(surface, context) {
        return Vec::new();
    }
    let (directory, display_prefix, name_prefix) =
        path_completion_parts(&context.query, working_directory);
    let Ok(entries) = fs::read_dir(&directory) else {
        return Vec::new();
    };
    let include_hidden = name_prefix.starts_with('.');
    let mut candidates = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if !include_hidden && name.starts_with('.') {
                return None;
            }
            if !name.starts_with(&name_prefix) {
                return None;
            }
            let is_dir = entry.file_type().ok().is_some_and(|kind| kind.is_dir());
            let suffix = if is_dir { "/" } else { "" };
            let value = format!(
                "{display_prefix}{}{suffix}",
                escape_path_component_for_shell(&name)
            );
            Some(SelectorCandidate::new(
                value,
                SelectorCandidateKind::Value,
                !is_dir,
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.value.cmp(&right.value));
    candidates.truncate(200);
    candidates
}

/// Returns whether filesystem completion should be offered for this token.
///
/// # Parameters
/// - `surface`: Prompt surface requesting candidates.
/// - `context`: Token context at the current cursor.
pub(super) fn path_completion_allowed(
    surface: SelectorSurface,
    context: &SelectorTokenContext,
) -> bool {
    if context.query.starts_with('-') {
        return false;
    }
    if path_query_is_explicit(&context.query) {
        return true;
    }
    if surface == SelectorSurface::AgentCommand && agent_query_likely_targets_relative_path(context)
    {
        return true;
    }
    if surface == SelectorSurface::AgentCommand
        && context.tokens_before.is_empty()
        && agent_root_query_may_target_path(&context.query)
    {
        return true;
    }
    let Some(command) = context.tokens_before.first() else {
        return false;
    };
    command_accepts_path_argument(surface, command)
}

/// Returns whether a command commonly accepts filesystem paths.
///
/// # Parameters
/// - `surface`: Prompt surface requesting candidates.
/// - `command`: First command token in the active prompt segment.
pub(super) fn command_accepts_path_argument(surface: SelectorSurface, command: &str) -> bool {
    match surface {
        SelectorSurface::MezzanineCommand => matches!(
            command,
            "source-file"
                | "save-buffer"
                | "export-history"
                | "pipe-pane"
                | "new-window"
                | "new-group"
                | "split-window"
                | "save-layout"
                | "load-layout"
        ),
        SelectorSurface::AgentCommand => {
            let command = command.strip_prefix('/').unwrap_or(command);
            matches!(
                canonical_agent_command(command),
                "show-issues" | "show-memories"
            )
        }
    }
}

/// Returns whether a token explicitly looks like a path.
///
/// # Parameters
/// - `query`: Current completion query.
pub(super) fn path_query_is_explicit(query: &str) -> bool {
    query == "~"
        || query.starts_with("./")
        || query.starts_with("../")
        || query.starts_with("~/")
        || query.starts_with('/')
}

/// Returns whether an agent-shell token likely targets a relative path.
///
/// # Parameters
/// - `context`: Token context at the current cursor.
pub(super) fn agent_query_likely_targets_relative_path(context: &SelectorTokenContext) -> bool {
    relative_path_query_is_probable(&context.query)
        || context
            .tokens_before
            .last()
            .is_some_and(|token| agent_token_introduces_path(token))
}

/// Returns whether the agent prompt root token may reasonably target a path.
///
/// # Parameters
/// - `query`: Current completion query.
pub(super) fn agent_root_query_may_target_path(query: &str) -> bool {
    !query.is_empty() && !query.starts_with('$') && !query.starts_with('/')
}

/// Returns whether the current token looks like an unprefixed relative path.
///
/// # Parameters
/// - `query`: Current completion query.
pub(super) fn relative_path_query_is_probable(query: &str) -> bool {
    !query.is_empty() && query.contains('/') && !query.starts_with('/')
}

/// Returns whether one prior agent-shell token commonly introduces a path.
///
/// # Parameters
/// - `token`: Prior token before the current completion query.
pub(super) fn agent_token_introduces_path(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "--save"
            | "at"
            | "dir"
            | "directory"
            | "file"
            | "files"
            | "folder"
            | "from"
            | "in"
            | "into"
            | "path"
            | "paths"
            | "under"
    )
}

/// Splits a path query into lookup directory, displayed prefix, and basename.
///
/// # Parameters
/// - `query`: Current completion query.
pub(super) fn path_completion_parts(
    query: &str,
    working_directory: Option<&Path>,
) -> (PathBuf, String, String) {
    if query == "~" {
        return (expand_home_path("~"), "~/".to_string(), String::new());
    }
    let (mut directory, mut display_prefix, remainder) =
        if let Some(remainder) = query.strip_prefix("~/") {
            (expand_home_path("~"), "~/".to_string(), remainder)
        } else if let Some(remainder) = query.strip_prefix('/') {
            (PathBuf::from("/"), "/".to_string(), remainder)
        } else {
            (
                working_directory
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from(".")),
                String::new(),
                query,
            )
        };
    if remainder.is_empty() {
        return (directory, display_prefix, String::new());
    }
    let mut name_prefix = String::new();
    let mut components = remainder.split('/').peekable();
    while let Some(component) = components.next() {
        let has_more_components = components.peek().is_some();
        if !has_more_components && !query.ends_with('/') {
            name_prefix = unescape_selector_shell_token(component);
            break;
        }
        let lookup_component = unescape_selector_shell_token(component);
        let next_directory = directory.join(&lookup_component);
        if component.is_empty() || !next_directory.is_dir() {
            name_prefix = lookup_component;
            break;
        }
        directory = next_directory;
        display_prefix.push_str(component);
        display_prefix.push('/');
    }
    (directory, display_prefix, name_prefix)
}

/// Escapes one path component so shell completion inserts a single token.
pub(super) fn escape_path_component_for_shell(component: &str) -> String {
    let mut escaped = String::new();
    for ch in component.chars() {
        if ch.is_whitespace() || matches!(ch, '\\' | '\'' | '"' | ';') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// Expands a leading tilde in a path used only for completion lookup.
///
/// # Parameters
/// - `path`: Path text from the prompt token.
pub(super) fn expand_home_path(path: &str) -> PathBuf {
    if path == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return std::env::var_os("HOME")
            .map(|home| Path::new(&home).join(rest))
            .unwrap_or_else(|| PathBuf::from(path));
    }
    PathBuf::from(path)
}
