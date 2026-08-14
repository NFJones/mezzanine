//! Filesystem candidate discovery, path heuristics, escaping, and home expansion.

use super::{
    Path, PathBuf, SelectorCandidate, SelectorCandidateKind, SelectorSurface, SelectorTokenContext,
    canonical_agent_command, fs, selector_token_context, unescape_selector_shell_token,
};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};

/// Maximum filesystem candidates retained for one selector query.
const MAX_FILESYSTEM_SELECTOR_CANDIDATES: usize = 200;

/// Nonblocking filesystem candidate discovery owned by one readline prompt.
///
/// Cloned prompts share one worker and one exact-key result snapshot. Rapid
/// revisions overwrite pending work, while generation checks prevent an older
/// directory scan from replacing candidates for newer prompt text or cwd.
#[derive(Clone)]
pub struct AsyncFilesystemSelectorCandidates {
    state: Arc<Mutex<AsyncFilesystemSelectorState>>,
    wake: Arc<OnceLock<Option<mpsc::SyncSender<()>>>>,
}

impl fmt::Debug for AsyncFilesystemSelectorCandidates {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AsyncFilesystemSelectorCandidates")
            .finish_non_exhaustive()
    }
}

impl PartialEq for AsyncFilesystemSelectorCandidates {
    fn eq(&self, _other: &Self) -> bool {
        // Worker/cache state is operational and does not change prompt value semantics.
        true
    }
}

impl Eq for AsyncFilesystemSelectorCandidates {}

#[derive(Debug, Default)]
struct AsyncFilesystemSelectorState {
    generation: u64,
    pending: Option<AsyncFilesystemSelectorRequest>,
    active: Option<AsyncFilesystemSelectorKey>,
    completed: Option<AsyncFilesystemSelectorCompletion>,
}

#[derive(Debug, Clone)]
struct AsyncFilesystemSelectorRequest {
    generation: u64,
    key: AsyncFilesystemSelectorKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AsyncFilesystemSelectorKey {
    surface: SelectorSurface,
    line: String,
    cursor: usize,
    working_directory: Option<PathBuf>,
}

#[derive(Debug)]
struct AsyncFilesystemSelectorCompletion {
    generation: u64,
    key: AsyncFilesystemSelectorKey,
    candidates: Arc<[SelectorCandidate]>,
}

/// Exact-key nonblocking filesystem candidate snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AsyncFilesystemSelectorSnapshot {
    completion_generation: Option<u64>,
    candidates: Arc<[SelectorCandidate]>,
}

impl AsyncFilesystemSelectorSnapshot {
    /// Returns the generation of the completed exact-key scan, when available.
    pub fn completion_generation(&self) -> Option<u64> {
        self.completion_generation
    }

    /// Returns the immutable candidates discovered for this exact prompt key.
    pub fn candidates(&self) -> &[SelectorCandidate] {
        &self.candidates
    }
}

impl Default for AsyncFilesystemSelectorCandidates {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(AsyncFilesystemSelectorState::default())),
            wake: Arc::new(OnceLock::new()),
        }
    }
}

impl AsyncFilesystemSelectorCandidates {
    /// Returns a completed exact-key snapshot or starts bounded background discovery.
    pub fn snapshot(
        &self,
        surface: SelectorSurface,
        line: &str,
        cursor: usize,
        working_directory: Option<&Path>,
    ) -> AsyncFilesystemSelectorSnapshot {
        let context = selector_token_context(line, cursor);
        if !path_completion_allowed(surface, &context) {
            return AsyncFilesystemSelectorSnapshot::default();
        }
        let key = AsyncFilesystemSelectorKey {
            surface,
            line: line.to_string(),
            cursor: context.cursor,
            working_directory: working_directory.map(Path::to_path_buf),
        };
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(completed) = state.completed.as_ref().filter(|entry| entry.key == key) {
            return AsyncFilesystemSelectorSnapshot {
                completion_generation: Some(completed.generation),
                candidates: completed.candidates.clone(),
            };
        }
        if state.active.as_ref() == Some(&key)
            || state
                .pending
                .as_ref()
                .is_some_and(|request| request.key == key)
        {
            return AsyncFilesystemSelectorSnapshot::default();
        }
        state.generation = state.generation.wrapping_add(1);
        state.pending = Some(AsyncFilesystemSelectorRequest {
            generation: state.generation,
            key,
        });
        drop(state);
        if let Some(wake) = self.wake.get_or_init(|| {
            let (wake, receiver) = mpsc::sync_channel(1);
            let weak_state = Arc::downgrade(&self.state);
            std::thread::Builder::new()
                .name("mez-filesystem-selector".to_string())
                .spawn(move || run_async_filesystem_selector_worker(weak_state, receiver))
                .ok()
                .map(|_| wake)
        }) {
            let _ = wake.try_send(());
        }
        AsyncFilesystemSelectorSnapshot::default()
    }

    /// Waits for one exact request to complete in focused tests.
    #[cfg(test)]
    pub fn complete_for_tests(
        &self,
        surface: SelectorSurface,
        line: &str,
        cursor: usize,
        working_directory: Option<&Path>,
    ) -> Vec<SelectorCandidate> {
        for _ in 0..1_000 {
            let _ = self.snapshot(surface, line, cursor, working_directory);
            let completed_candidates = {
                let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                state
                    .completed
                    .as_ref()
                    .filter(|entry| {
                        entry.key.surface == surface
                            && entry.key.line == line
                            && entry.key.cursor == cursor
                            && entry.key.working_directory.as_deref() == working_directory
                    })
                    .map(|entry| entry.candidates.to_vec())
            };
            if let Some(candidates) = completed_candidates {
                return candidates;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("filesystem selector discovery did not complete");
    }
}

fn run_async_filesystem_selector_worker(
    weak_state: Weak<Mutex<AsyncFilesystemSelectorState>>,
    receiver: mpsc::Receiver<()>,
) {
    while receiver.recv().is_ok() {
        loop {
            let Some(state) = weak_state.upgrade() else {
                return;
            };
            let request = {
                let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                let request = state.pending.take();
                state.active = request.as_ref().map(|request| request.key.clone());
                request
            };
            let Some(request) = request else {
                break;
            };
            let context = selector_token_context(&request.key.line, request.key.cursor);
            let candidates = path_candidates(
                request.key.surface,
                &context,
                request.key.working_directory.as_deref(),
            );
            let Some(state) = weak_state.upgrade() else {
                return;
            };
            let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
            if state.generation == request.generation {
                state.completed = Some(AsyncFilesystemSelectorCompletion {
                    generation: request.generation,
                    key: request.key,
                    candidates: Arc::from(candidates),
                });
            }
            state.active = None;
            if state.pending.is_none() {
                break;
            }
        }
    }
}

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
    let mut candidates = BTreeMap::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().to_string();
        if !include_hidden && name.starts_with('.') {
            continue;
        }
        if !name.starts_with(&name_prefix) {
            continue;
        }
        let is_dir = entry.file_type().ok().is_some_and(|kind| kind.is_dir());
        let suffix = if is_dir { "/" } else { "" };
        let value = format!(
            "{display_prefix}{}{suffix}",
            escape_path_component_for_shell(&name)
        );
        let candidate = SelectorCandidate::new(value, SelectorCandidateKind::Value, !is_dir);
        candidates.insert(candidate.value.clone(), candidate);
        if candidates.len() > MAX_FILESYSTEM_SELECTOR_CANDIDATES {
            candidates.pop_last();
        }
    }
    candidates.into_values().collect()
}

/// Builds literal filesystem candidates for one standalone save-path field.
///
/// Record-browser save prompts submit a path directly to filesystem APIs rather
/// than through a shell. Their candidates therefore retain spaces and quoting
/// characters verbatim while preserving the normal path lookup, hidden-entry,
/// ordering, directory-suffix, and candidate-limit behavior.
pub fn record_browser_save_path_candidates(
    query: &str,
    working_directory: Option<&Path>,
) -> Vec<SelectorCandidate> {
    let (directory, display_prefix, name_prefix) = path_completion_parts(query, working_directory);
    let Ok(entries) = fs::read_dir(&directory) else {
        return Vec::new();
    };
    let include_hidden = name_prefix.starts_with('.');
    let mut candidates = BTreeMap::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().to_string();
        if (!include_hidden && name.starts_with('.')) || !name.starts_with(&name_prefix) {
            continue;
        }
        let is_dir = entry.file_type().ok().is_some_and(|kind| kind.is_dir());
        let suffix = if is_dir { "/" } else { "" };
        let candidate = SelectorCandidate::new(
            format!("{display_prefix}{name}{suffix}"),
            SelectorCandidateKind::Value,
            false,
        );
        candidates.insert(candidate.value.clone(), candidate);
        if candidates.len() > MAX_FILESYSTEM_SELECTOR_CANDIDATES {
            candidates.pop_last();
        }
    }
    candidates.into_values().collect()
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
