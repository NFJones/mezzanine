//! Shared selector planning for command prompt surfaces.
//!
//! The selector is intentionally UI-agnostic: it determines the editable token,
//! candidate list, and replacement text for a prompt line, while readline and
//! terminal rendering decide how users cycle and display those candidates. This
//! keeps Mezzanine command selection, agent slash-command selection, and
//! argument-value selection on one deterministic code path.

use crate::agent::baseline_slash_commands;
use crate::command::baseline_commands;
use std::fs;
use std::path::{Path, PathBuf};

/// Interactive prompt surface requesting selector candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorSurface {
    /// The Mezzanine command prompt or configuration command prompt.
    MezzanineCommand,
    /// The pane-local agent prompt when slash-command input is active.
    AgentCommand,
}

/// Category for one selectable candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorCandidateKind {
    /// A top-level Mezzanine or agent command.
    Command,
    /// An accepted command alias.
    Alias,
    /// A command-line flag or option.
    Flag,
    /// A value for the preceding or current argument.
    Value,
}

/// A selectable value with optional display metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorCandidate {
    /// Text inserted into the prompt when selected.
    pub value: String,
    /// User-facing text shown in selector UIs.
    pub label: String,
    /// Short explanation for selector UIs that have room for details.
    pub detail: Option<String>,
    /// Candidate category.
    pub kind: SelectorCandidateKind,
    /// Whether selecting this candidate should leave a trailing separator.
    pub append_space: bool,
}

impl SelectorCandidate {
    /// Builds a candidate whose display label is the inserted value.
    pub fn new(value: impl Into<String>, kind: SelectorCandidateKind, append_space: bool) -> Self {
        let value = value.into();
        Self {
            label: value.clone(),
            value,
            detail: None,
            kind,
            append_space,
        }
    }

    /// Attaches a short detail string to a selector candidate.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// A runtime-supplied candidate scoped to one prompt surface and command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorExtraCandidate {
    /// Prompt surface that may display this candidate.
    pub surface: SelectorSurface,
    /// Canonical command name whose argument list receives this candidate.
    pub command: String,
    /// Candidate value and display metadata.
    pub candidate: SelectorCandidate,
}

impl SelectorExtraCandidate {
    /// Builds a command-scoped selector candidate for dynamic runtime values.
    pub fn new(
        surface: SelectorSurface,
        command: impl Into<String>,
        candidate: SelectorCandidate,
    ) -> Self {
        Self {
            surface,
            command: command.into(),
            candidate,
        }
    }
}

/// Replacement plan for one selector invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorPlan {
    /// Start byte of the token to replace.
    pub replacement_start: usize,
    /// End byte of the token to replace.
    pub replacement_end: usize,
    /// User query extracted from the token being replaced.
    pub query: String,
    /// Sorted candidates matching `query`.
    pub candidates: Vec<SelectorCandidate>,
}

/// Non-mutating completion hint rendered as shadow text in a prompt line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorShadowHint {
    /// Byte offset in the prompt buffer where the hint should be inserted.
    pub insert_at: usize,
    /// Shadow text to render without adding it to the editable buffer.
    pub text: String,
    /// Candidate category represented by the hint.
    pub kind: SelectorCandidateKind,
}

/// Stateful selection over an immutable base line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSelector {
    /// Surface used to produce this selection.
    pub surface: SelectorSurface,
    /// Prompt line before the selector inserted any candidate.
    pub base_line: String,
    /// Cursor byte offset before the selector inserted any candidate.
    pub base_cursor: usize,
    /// Current replacement plan.
    pub plan: SelectorPlan,
    /// Currently selected candidate index.
    pub selected_index: usize,
}

impl ActiveSelector {
    /// Starts a selector from the current prompt line.
    pub fn start(
        surface: SelectorSurface,
        line: &str,
        cursor: usize,
        reverse: bool,
    ) -> Option<Self> {
        Self::start_with_extra(surface, line, cursor, reverse, &[])
    }

    /// Starts a selector from the current prompt line with runtime candidates.
    pub fn start_with_extra(
        surface: SelectorSurface,
        line: &str,
        cursor: usize,
        reverse: bool,
        extra_candidates: &[SelectorExtraCandidate],
    ) -> Option<Self> {
        let plan = plan_selector_with_extra(surface, line, cursor, extra_candidates)?;
        let selected_index = if reverse {
            plan.candidates.len().saturating_sub(1)
        } else {
            0
        };
        Some(Self {
            surface,
            base_line: line.to_string(),
            base_cursor: cursor,
            plan,
            selected_index,
        })
    }

    /// Moves to the next candidate, wrapping at the end.
    pub fn select_next(&mut self) {
        if self.plan.candidates.is_empty() {
            return;
        }
        self.selected_index = (self.selected_index + 1) % self.plan.candidates.len();
    }

    /// Moves to the previous candidate, wrapping at the beginning.
    pub fn select_previous(&mut self) {
        if self.plan.candidates.is_empty() {
            return;
        }
        self.selected_index = if self.selected_index == 0 {
            self.plan.candidates.len() - 1
        } else {
            self.selected_index - 1
        };
    }

    /// Returns the prompt line after applying the current candidate.
    pub fn selected_line(&self) -> Option<(String, usize)> {
        let candidate = self.plan.candidates.get(self.selected_index)?;
        Some(apply_selector_candidate(
            &self.base_line,
            &self.plan,
            candidate,
        ))
    }
}

/// Builds a selector plan for the token at `cursor`.
pub fn plan_selector(surface: SelectorSurface, line: &str, cursor: usize) -> Option<SelectorPlan> {
    plan_selector_with_extra(surface, line, cursor, &[])
}

/// Builds a selector plan for the token at `cursor` with runtime candidates.
pub fn plan_selector_with_extra(
    surface: SelectorSurface,
    line: &str,
    cursor: usize,
    extra_candidates: &[SelectorExtraCandidate],
) -> Option<SelectorPlan> {
    let cursor = clamp_to_char_boundary(line, cursor);
    let context = token_context(line, cursor);
    let candidates = selector_candidates(surface, &context, extra_candidates);
    let candidates = filter_and_sort_candidates(candidates, &context.query);
    (!candidates.is_empty()).then_some(SelectorPlan {
        replacement_start: context.token_start,
        replacement_end: context.token_end,
        query: context.query,
        candidates,
    })
}

/// Builds the current prefix or parameter shadow hint without editing `line`.
pub fn shadow_hint(
    surface: SelectorSurface,
    line: &str,
    cursor: usize,
) -> Option<SelectorShadowHint> {
    shadow_hint_with_extra(surface, line, cursor, &[])
}

/// Builds the current prefix or parameter shadow hint with runtime candidates.
pub fn shadow_hint_with_extra(
    surface: SelectorSurface,
    line: &str,
    cursor: usize,
    extra_candidates: &[SelectorExtraCandidate],
) -> Option<SelectorShadowHint> {
    let cursor = clamp_to_char_boundary(line, cursor);
    let context = token_context(line, cursor);
    prefix_shadow_hint(surface, &context, cursor, extra_candidates)
        .or_else(|| parameter_shadow_hint(surface, &context, cursor))
}

/// Applies a selected candidate to a line according to a selector plan.
pub fn apply_selector_candidate(
    line: &str,
    plan: &SelectorPlan,
    candidate: &SelectorCandidate,
) -> (String, usize) {
    let mut next = String::new();
    next.push_str(&line[..plan.replacement_start]);
    next.push_str(&candidate.value);
    let mut cursor = plan.replacement_start.saturating_add(candidate.value.len());
    if candidate.append_space && should_append_separator(line, plan) {
        next.push(' ');
        cursor = cursor.saturating_add(1);
    }
    next.push_str(&line[plan.replacement_end..]);
    (next, cursor)
}

/// Runs the prefix shadow hint operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn prefix_shadow_hint(
    surface: SelectorSurface,
    context: &TokenContext,
    cursor: usize,
    extra_candidates: &[SelectorExtraCandidate],
) -> Option<SelectorShadowHint> {
    if cursor != context.token_end {
        return None;
    }
    if context.query.is_empty() {
        return None;
    }
    let candidates = selector_candidates(surface, context, extra_candidates);
    let candidate = filter_and_sort_candidates(candidates, &context.query)
        .into_iter()
        .find(|candidate| {
            candidate_prefix_suffix(candidate.value.as_str(), &context.query).is_some()
        })?;
    let text = candidate_prefix_suffix(candidate.value.as_str(), &context.query)?;
    (!text.is_empty()).then_some(SelectorShadowHint {
        insert_at: cursor,
        text,
        kind: candidate.kind,
    })
}

/// Runs the parameter shadow hint operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parameter_shadow_hint(
    surface: SelectorSurface,
    context: &TokenContext,
    cursor: usize,
) -> Option<SelectorShadowHint> {
    if !context.query.is_empty() || context.tokens_before.len() != 1 {
        return None;
    }
    let command = context.tokens_before[0].as_str();
    let text = match surface {
        SelectorSurface::MezzanineCommand => mezzanine_parameter_hint(command)?,
        SelectorSurface::AgentCommand => {
            let command = command.strip_prefix('/').unwrap_or(command);
            agent_parameter_hint(canonical_agent_command(command))?
        }
    };
    Some(SelectorShadowHint {
        insert_at: cursor,
        text: text.to_string(),
        kind: SelectorCandidateKind::Value,
    })
}

/// Runs the candidate prefix suffix operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn candidate_prefix_suffix(candidate: &str, query: &str) -> Option<String> {
    let candidate_lower = candidate.to_ascii_lowercase();
    let query_lower = query.to_ascii_lowercase();
    if !candidate_lower.starts_with(&query_lower) {
        return None;
    }
    let suffix = candidate
        .chars()
        .skip(query.chars().count())
        .collect::<String>();
    (!suffix.is_empty()).then_some(suffix)
}

/// Carries Token Context state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TokenContext {
    /// Stores the query value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    query: String,
    /// Stores the token start value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    token_start: usize,
    /// Stores the token end value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    token_end: usize,
    /// Stores the tokens before value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    tokens_before: Vec<String>,
}

/// Runs the token context operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn token_context(line: &str, cursor: usize) -> TokenContext {
    let segment_start = current_command_segment_start(line, cursor);
    let token_start = line[segment_start..cursor]
        .char_indices()
        .rev()
        .find_map(|(offset, ch)| {
            ch.is_whitespace()
                .then_some(segment_start + offset + ch.len_utf8())
        })
        .unwrap_or(segment_start);
    let token_end = line[cursor..]
        .char_indices()
        .find_map(|(offset, ch)| ch.is_whitespace().then_some(cursor + offset))
        .unwrap_or(line.len());
    let tokens_before = whitespace_tokens(&line[segment_start..token_start]);
    let query = line[token_start..cursor].to_string();
    TokenContext {
        query,
        token_start,
        token_end,
        tokens_before,
    }
}

/// Runs the current command segment start operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn current_command_segment_start(line: &str, cursor: usize) -> usize {
    let mut quote = QuoteState::None;
    let mut escaped = false;
    let mut start = 0usize;
    for (index, ch) in line[..cursor].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quote != QuoteState::Single => escaped = true,
            '\'' if quote == QuoteState::None => quote = QuoteState::Single,
            '\'' if quote == QuoteState::Single => quote = QuoteState::None,
            '"' if quote == QuoteState::None => quote = QuoteState::Double,
            '"' if quote == QuoteState::Double => quote = QuoteState::None,
            ';' if quote == QuoteState::None => start = index.saturating_add(1),
            _ => {}
        }
    }
    while line[start..cursor]
        .chars()
        .next()
        .is_some_and(char::is_whitespace)
    {
        start += line[start..cursor]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
    }
    start
}

/// Carries Quote State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteState {
    /// Represents the None case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    None,
    /// Represents the Single case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Single,
    /// Represents the Double case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Double,
}

/// Runs the whitespace tokens operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn whitespace_tokens(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>()
}

/// Runs the mezzanine candidates operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mezzanine_candidates(context: &TokenContext) -> Vec<SelectorCandidate> {
    if context.tokens_before.is_empty() {
        return baseline_commands()
            .into_iter()
            .map(|command| {
                SelectorCandidate::new(command.name, SelectorCandidateKind::Command, true)
                    .with_detail(command.status.as_str())
            })
            .collect();
    }
    let command = context.tokens_before[0].as_str();
    mezzanine_argument_candidates(command)
}

/// Runs the agent candidates operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn agent_candidates(context: &TokenContext) -> Vec<SelectorCandidate> {
    if context.tokens_before.is_empty() {
        if !context.query.is_empty() && !context.query.starts_with('/') {
            return Vec::new();
        }
        return baseline_slash_commands()
            .into_iter()
            .flat_map(|spec| {
                let canonical = SelectorCandidate::new(
                    format!("/{}", spec.name),
                    SelectorCandidateKind::Command,
                    true,
                )
                .with_detail(format!("{:?}", spec.effect));
                let aliases = spec.aliases.iter().map(move |alias| {
                    SelectorCandidate::new(format!("/{alias}"), SelectorCandidateKind::Alias, true)
                        .with_detail(format!("alias for /{}", spec.name))
                });
                std::iter::once(canonical)
                    .chain(aliases)
                    .collect::<Vec<_>>()
            })
            .collect();
    }
    let command = context.tokens_before[0]
        .strip_prefix('/')
        .unwrap_or(context.tokens_before[0].as_str());
    agent_argument_candidates(canonical_agent_command(command))
}

/// Builds selector candidates from static command metadata plus runtime values.
fn selector_candidates(
    surface: SelectorSurface,
    context: &TokenContext,
    extra_candidates: &[SelectorExtraCandidate],
) -> Vec<SelectorCandidate> {
    let mut candidates = match surface {
        SelectorSurface::MezzanineCommand => mezzanine_candidates(context),
        SelectorSurface::AgentCommand => agent_candidates(context),
    };
    let Some(command) = selector_context_command(surface, context) else {
        if surface == SelectorSurface::AgentCommand
            && context.tokens_before.is_empty()
            && context.query.starts_with('$')
        {
            candidates.extend(
                extra_candidates
                    .iter()
                    .filter(|extra| extra.surface == surface && extra.command == "$")
                    .map(|extra| extra.candidate.clone()),
            );
        }
        return candidates;
    };
    candidates.extend(
        extra_candidates
            .iter()
            .filter(|extra| extra.surface == surface && extra.command == command)
            .map(|extra| extra.candidate.clone()),
    );
    candidates.extend(path_candidates(surface, context));
    candidates
}

/// Returns the canonical command receiving argument candidates.
fn selector_context_command(surface: SelectorSurface, context: &TokenContext) -> Option<String> {
    let command = context.tokens_before.first()?.as_str();
    Some(match surface {
        SelectorSurface::MezzanineCommand => command.to_string(),
        SelectorSurface::AgentCommand => {
            let command = command.strip_prefix('/').unwrap_or(command);
            canonical_agent_command(command).to_string()
        }
    })
}

/// Runs the canonical agent command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn canonical_agent_command(command: &str) -> &str {
    for spec in baseline_slash_commands() {
        if spec.name == command || spec.aliases.contains(&command) {
            return spec.name;
        }
    }
    command
}

/// Runs the mezzanine argument candidates operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mezzanine_argument_candidates(command: &str) -> Vec<SelectorCandidate> {
    let mut candidates = Vec::new();
    candidates.extend(flag_candidates(common_target_flags()));
    match command {
        "new-window" | "new-group" => {
            candidates.extend(flag_candidates(&[
                "-n",
                "--name",
                "-c",
                "--start-directory",
                "-d",
                "--select",
                "--no-select",
                "--",
            ]));
        }
        "split-window" => {
            candidates.extend(flag_candidates(&[
                "-h",
                "-v",
                "-d",
                "-c",
                "--start-directory",
                "--size",
                "--percent",
                "--select",
                "--no-select",
                "--",
            ]));
            candidates.extend(value_candidates(&["horizontal", "vertical"]));
        }
        "select-window" | "select-group" | "attach-session" | "kill-session" | "rename-session" => {
            candidates.extend(value_candidates(&["next", "previous", "last"]));
        }
        "select-pane" | "swap-pane" => {
            candidates.extend(flag_candidates(&[
                "-U", "--up", "-D", "--down", "-L", "--left", "-R", "--right",
            ]));
            candidates.extend(value_candidates(&["next", "previous", "prev", "last"]));
        }
        "resize-pane" => {
            candidates.extend(flag_candidates(&[
                "-L",
                "-R",
                "-U",
                "-D",
                "-x",
                "--columns",
                "-y",
                "--rows",
                "--percent",
                "--axis",
                "--delta",
                "--edge",
                "--amount",
            ]));
            candidates.extend(value_candidates(&[
                "columns",
                "rows",
                "horizontal",
                "vertical",
                "both",
                "left",
                "right",
                "up",
                "down",
            ]));
        }
        "select-layout" => {
            candidates.extend(value_candidates(&[
                "even-horizontal",
                "even-grid",
                "even-vertical",
                "main-horizontal",
                "main-vertical",
                "tiled",
            ]));
        }
        "copy-mode" | "search-history" | "export-history" | "capture-pane" => {
            candidates.extend(flag_candidates(&[
                "-t",
                "--target-pane",
                "--start",
                "--end",
                "--search",
                "--output",
            ]));
            candidates.extend(value_candidates(&["start", "end"]));
        }
        "paste-buffer" | "create-buffer" | "delete-buffer" | "save-buffer" | "choose-buffer" => {
            candidates.extend(flag_candidates(&[
                "-b",
                "--buffer",
                "-t",
                "--target-pane",
                "--delete",
                "--content",
                "--select",
                "--replace",
            ]));
        }
        "bind-key" | "unbind-key" => {
            candidates.extend(flag_candidates(&["-T", "--table", "-n", "--repeat"]));
            candidates.extend(value_candidates(&["prefix", "root"]));
        }
        "set-option" | "show-options" => {
            candidates.extend(flag_candidates(&[
                "-g", "--global", "-w", "--window", "-p", "--pane", "-u", "--unset",
            ]));
            candidates.extend(value_candidates(&[
                "session",
                "terminal",
                "frames",
                "theme",
                "history",
                "agents",
                "permissions",
            ]));
        }
        "auth-login" => {
            candidates.extend(flag_candidates(&[
                "--browser",
                "--device-code",
                "--api-key",
                "--api-key-file",
                "--model-profile",
            ]));
            candidates.extend(value_candidates(&["default"]));
        }
        "mcp-add" | "mcp-remove" | "mcp-retry" => {
            candidates.extend(flag_candidates(&[
                "--transport",
                "--command",
                "--url",
                "--env",
                "--disabled",
            ]));
            candidates.extend(value_candidates(&["stdio", "streamable-http"]));
        }
        "snapshot-session" | "resume-session" => {
            candidates.extend(flag_candidates(&["--snapshot", "--session", "--latest"]));
        }
        "list-themes" | "set-theme" => {
            candidates.extend(value_candidates(crate::terminal::BUILTIN_UI_THEME_NAMES));
        }
        "agent-shell" => {
            candidates.extend(value_candidates(&["show", "hide", "toggle"]));
        }
        _ => {}
    }
    dedupe_candidates(candidates)
}

/// Runs the common target flags operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn common_target_flags() -> &'static [&'static str] {
    &["-t", "--target", "-s", "--source"]
}

/// Runs the agent argument candidates operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn agent_argument_candidates(command: &str) -> Vec<SelectorCandidate> {
    let candidates = match command {
        "latency" => value_candidates(&["slow", "default", "fast"]),
        "log-level" => value_candidates(&["normal", "verbose", "debug", "trace"]),
        "approval" | "permissions" => {
            let mut candidates = value_candidates(&["ask", "auto-allow", "full-access"]);
            candidates.extend(value_candidates(&[
                "add", "remove", "list", "rules", "allow", "deny", "prompt", "bypass",
            ]));
            candidates
        }
        "approve" => value_candidates(&["latest", "once", "session", "project", "global"]),
        "trust" => value_candidates(&["latest", "list", "pending"]),
        "model" => {
            let mut candidates = value_candidates(&[
                "list",
                "default",
                "gpt-5.5",
                "gpt-5.4",
                "gpt-5.4-mini",
                "gpt-5.3-codex",
                "gpt-5.3-codex-spark",
                "gpt-5.2",
                "low",
                "medium",
                "high",
                "xhigh",
            ]);
            candidates.extend(flag_candidates(&[
                "--secondary",
                "--router",
                "--reasoning",
                "--clear",
                "--show",
            ]));
            candidates
        }
        "list-mcp" => Vec::new(),
        "resume" => flag_candidates(&["--latest"]),
        "auto-reasoning" => value_candidates(&["on", "off", "toggle", "status"]),
        "personality" => value_candidates(&["list", "status", "show", "clear", "default"]),
        "copy" => value_candidates(&["pane", "buffer", "clipboard"]),
        "copy-context" => value_candidates(&["pane", "buffer", "clipboard"]),
        "copy-trace-log" => value_candidates(&["pane", "buffer", "clipboard"]),
        "copy-patches" => value_candidates(&["pane", "buffer", "clipboard"]),
        "statusline" => value_candidates(&["on", "off", "toggle"]),
        "title" => value_candidates(&["default", "agent", "off"]),
        _ => Vec::new(),
    };
    dedupe_candidates(candidates)
}

/// Runs the mezzanine parameter hint operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mezzanine_parameter_hint(command: &str) -> Option<&'static str> {
    match command {
        "new-window" => Some(" [-n name] [-c dir] [-- command]"),
        "new-group" => Some(" [-n name] [-c dir] [-- command]"),
        "rename-group" => Some(" <name>"),
        "kill-group" => Some(" [-t target-group]"),
        "select-group" => Some(" <target>"),
        "rename-window" => Some(" <name>"),
        "kill-window" => Some(" [-t target-window]"),
        "select-window" | "attach-session" | "kill-session" => Some(" <target>"),
        "split-window" => Some(" [-h|-v] [-d] [-c dir] [-- command]"),
        "kill-pane" | "zoom-pane" | "display-panes" => Some(" [-t target-pane]"),
        "select-pane" | "swap-pane" => Some(" <-U|-D|-L|-R|next|previous|last>"),
        "resize-pane" => Some(" <-L|-R|-U|-D|--percent n|--amount n>"),
        "select-layout" => Some(" <layout>"),
        "detach-client" => Some(" [-t target-client]"),
        "rename-session" => Some(" <name>"),
        "copy-mode" => Some(" [-t target-pane]"),
        "paste-buffer" | "delete-buffer" | "choose-buffer" => Some(" [-b buffer] [-t target-pane]"),
        "create-buffer" => Some(" [-b buffer] [--content text] [--select] [--replace]"),
        "bind-key" => Some(" [-T table] <key> <command>"),
        "unbind-key" => Some(" [-T table] <key>"),
        "show-options" => Some(" [-g|-w|-p] [option]"),
        "set-option" => Some(" [-g|-w|-p] <option> <value>"),
        "set-theme" => Some(" <theme>"),
        "source-file" => Some(" <path>"),
        "agent-shell" => Some(" <show|hide|toggle>"),
        "auth-login" => Some(" [--provider <openai|deepseek>] [--browser|--device-code|--api-key]"),
        "mcp-add" => Some(" <name> --transport <stdio|streamable-http>"),
        "mcp-remove" | "mcp-retry" => Some(" <name>"),
        "snapshot-session" | "resume-session" => Some(" [--snapshot id|--latest]"),
        "capture-pane" => Some(" [-t target-pane] [--start n] [--end n]"),
        "save-buffer" => Some(" [-b buffer] <path>"),
        "search-history" => Some(" [-t target-pane] <query>"),
        "export-history" | "pipe-pane" => Some(" [-t target-pane] <target>"),
        "mark-pane-ready" => Some(" <ready|unknown|blocked>"),
        "approve-observer" | "reject-observer" | "revoke-observer" => Some(" <observer-id>"),
        _ => None,
    }
}

/// Runs the agent parameter hint operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn agent_parameter_hint(command: &str) -> Option<&'static str> {
    match command {
        "permissions" => {
            Some(" <status|preset|approval-policy|list|allow|deny|prompt|remove|bypass>")
        }
        "approval" => Some(" <ask|auto-allow|full-access>"),
        "approve" => Some(" <approval-id|latest> [once|session|project|global]"),
        "trust" => Some(" <project-root|latest|list>"),
        "model" => Some(" [--secondary] <list|model> [reasoning]"),
        "auto-reasoning" => Some(" <on|off|toggle|status>"),
        "statusline" => Some(" <on|off|toggle>"),
        "log-level" => Some(" <normal|verbose|debug|trace>"),
        "copy" => Some(" <pane|buffer [name]|clipboard>"),
        "copy-context" => Some(" <pane|buffer [name]|clipboard>"),
        "copy-trace-log" => Some(" <pane|buffer [name]|clipboard>"),
        "copy-patches" => Some(" <pane|buffer [name]|clipboard>"),
        "personality" => Some(" <profile|style|list|clear>"),
        "resume" => Some(" <session-uuid|--latest>"),
        "list-mcp" => Some(" [server-name]"),
        "title" => Some(" <title|default|off>"),
        _ => None,
    }
}

/// Runs the flag candidates operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn flag_candidates(flags: &[&str]) -> Vec<SelectorCandidate> {
    flags
        .iter()
        .map(|flag| SelectorCandidate::new(*flag, SelectorCandidateKind::Flag, true))
        .collect()
}

/// Runs the value candidates operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn value_candidates(values: &[&str]) -> Vec<SelectorCandidate> {
    values
        .iter()
        .map(|value| SelectorCandidate::new(*value, SelectorCandidateKind::Value, true))
        .collect()
}

/// Builds filesystem path candidates for command arguments.
///
/// # Parameters
/// - `surface`: Prompt surface requesting candidates.
/// - `context`: Token context at the current cursor.
fn path_candidates(surface: SelectorSurface, context: &TokenContext) -> Vec<SelectorCandidate> {
    if !path_completion_allowed(surface, context) {
        return Vec::new();
    }
    let (directory, display_prefix, name_prefix) = path_completion_parts(&context.query);
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
            let value = format!("{display_prefix}{name}{suffix}");
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
fn path_completion_allowed(surface: SelectorSurface, context: &TokenContext) -> bool {
    if context.query.starts_with('-') {
        return false;
    }
    if path_query_is_explicit(&context.query) {
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
fn command_accepts_path_argument(surface: SelectorSurface, command: &str) -> bool {
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
                | "auth-login"
                | "snapshot-session"
                | "resume-session"
        ),
        SelectorSurface::AgentCommand => false,
    }
}

/// Returns whether a token explicitly looks like a path.
///
/// # Parameters
/// - `query`: Current completion query.
fn path_query_is_explicit(query: &str) -> bool {
    query.starts_with("./")
        || query.starts_with("../")
        || query.starts_with("~/")
        || query.starts_with('/')
}

/// Splits a path query into lookup directory, displayed prefix, and basename.
///
/// # Parameters
/// - `query`: Current completion query.
fn path_completion_parts(query: &str) -> (PathBuf, String, String) {
    let (raw_directory, name_prefix) = match query.rsplit_once('/') {
        Some((directory, name)) => {
            let directory = if directory.is_empty() { "/" } else { directory };
            (directory.to_string(), name.to_string())
        }
        None => (".".to_string(), query.to_string()),
    };
    let directory = expand_home_path(&raw_directory);
    let display_prefix = match query.rsplit_once('/') {
        Some(("", _)) => "/".to_string(),
        Some((prefix, _)) => format!("{prefix}/"),
        None => String::new(),
    };
    (directory, display_prefix, name_prefix)
}

/// Expands a leading tilde in a path used only for completion lookup.
///
/// # Parameters
/// - `path`: Path text from the prompt token.
fn expand_home_path(path: &str) -> PathBuf {
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

/// Runs the dedupe candidates operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn dedupe_candidates(candidates: Vec<SelectorCandidate>) -> Vec<SelectorCandidate> {
    let mut deduped = Vec::new();
    for candidate in candidates {
        if !deduped
            .iter()
            .any(|existing: &SelectorCandidate| existing.value == candidate.value)
        {
            deduped.push(candidate);
        }
    }
    deduped
}

/// Runs the filter and sort candidates operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn filter_and_sort_candidates(
    candidates: Vec<SelectorCandidate>,
    query: &str,
) -> Vec<SelectorCandidate> {
    let normalized_query = query.trim_start_matches('/');
    let mut scored = candidates
        .into_iter()
        .enumerate()
        .filter_map(|(position, candidate)| {
            selector_score(normalized_query, &candidate).map(|score| {
                (
                    score,
                    selector_order_key(&candidate, position),
                    candidate.value.len(),
                    candidate,
                )
            })
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.cmp(&right.2))
            .then(left.3.value.cmp(&right.3.value))
    });
    scored
        .into_iter()
        .map(|(_, _, _, candidate)| candidate)
        .collect()
}

/// Returns a stable ordering key for equally good selector matches.
fn selector_order_key(candidate: &SelectorCandidate, position: usize) -> usize {
    if candidate.kind == SelectorCandidateKind::Command {
        position
    } else {
        usize::MAX
    }
}

/// Runs the selector score operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn selector_score(query: &str, candidate: &SelectorCandidate) -> Option<usize> {
    if query.is_empty() {
        return Some(0);
    }
    let candidate_value = candidate.value.trim_start_matches('/');
    let query = query.to_ascii_lowercase();
    let value = candidate_value.to_ascii_lowercase();
    let label = candidate.label.to_ascii_lowercase();
    if value == query {
        Some(0)
    } else if value
        .strip_prefix(&query)
        .is_some_and(|suffix| suffix.starts_with('-'))
    {
        Some(5)
    } else if value.starts_with(&query) {
        Some(10 + value.len().saturating_sub(query.len()))
    } else if let Some(index) = value.find(&query) {
        Some(100 + index)
    } else if label.contains(&query) || is_subsequence(&query, &value) {
        Some(200 + value.len())
    } else {
        None
    }
}

/// Runs the is subsequence operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_subsequence(query: &str, value: &str) -> bool {
    let mut chars = value.chars();
    query.chars().all(|query_ch| chars.any(|ch| ch == query_ch))
}

/// Runs the should append separator operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn should_append_separator(line: &str, plan: &SelectorPlan) -> bool {
    line[plan.replacement_end..]
        .chars()
        .next()
        .is_none_or(|ch| !ch.is_whitespace())
}

/// Runs the clamp to char boundary operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn clamp_to_char_boundary(value: &str, cursor: usize) -> usize {
    let mut cursor = cursor.min(value.len());
    while cursor > 0 && !value.is_char_boundary(cursor) {
        cursor -= 1;
    }
    cursor
}

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests {
    use super::{
        SelectorCandidate, SelectorCandidateKind, SelectorExtraCandidate, SelectorSurface,
        apply_selector_candidate, plan_selector, plan_selector_with_extra, shadow_hint,
        shadow_hint_with_extra,
    };
    use std::fs;
    use std::sync::Mutex;

    static CWD_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Verifies selector plans mezzanine command candidates from prefix.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn selector_plans_mezzanine_command_candidates_from_prefix() {
        let plan = plan_selector(SelectorSurface::MezzanineCommand, "new", 3).unwrap();

        assert_eq!(plan.replacement_start, 0);
        assert_eq!(plan.replacement_end, 3);
        assert_eq!(plan.candidates[0].value, "new-window");
        assert_eq!(plan.candidates[0].kind, SelectorCandidateKind::Command);
        assert!(
            plan.candidates
                .iter()
                .any(|candidate| candidate.value == "new-group"
                    && candidate.kind == SelectorCandidateKind::Command)
        );
    }

    /// Verifies selector plans agent slash candidates from empty prompt.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn selector_plans_agent_slash_candidates_from_empty_prompt() {
        let plan = plan_selector(SelectorSurface::AgentCommand, "", 0).unwrap();

        assert!(plan.candidates.iter().any(|candidate| {
            candidate.value == "/help" && candidate.kind == SelectorCandidateKind::Command
        }));
    }

    /// Verifies selector plans mezzanine command argument candidates.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn selector_plans_mezzanine_command_argument_candidates() {
        let plan = plan_selector(SelectorSurface::MezzanineCommand, "mcp-add st", 10).unwrap();

        assert_eq!(plan.replacement_start, "mcp-add ".len());
        assert_eq!(plan.candidates[0].value, "stdio");
        assert_eq!(plan.candidates[0].kind, SelectorCandidateKind::Value);

        let theme_plan =
            plan_selector(SelectorSurface::MezzanineCommand, "set-theme to", 12).unwrap();
        assert_eq!(theme_plan.replacement_start, "set-theme ".len());
        assert_eq!(theme_plan.candidates[0].value, "tokyo_night");
        assert_eq!(theme_plan.candidates[0].kind, SelectorCandidateKind::Value);
    }

    /// Verifies selector plans agent argument candidates.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn selector_plans_agent_argument_candidates() {
        let plan = plan_selector(SelectorSurface::AgentCommand, "/log-level de", 13).unwrap();

        assert_eq!(plan.candidates[0].value, "debug");

        let auto_reasoning_plan =
            plan_selector(SelectorSurface::AgentCommand, "/auto-reasoning t", 18).unwrap();
        assert_eq!(auto_reasoning_plan.candidates[0].value, "toggle");

        let copy_plan = plan_selector(SelectorSurface::AgentCommand, "/copy c", 7).unwrap();
        assert_eq!(copy_plan.candidates[0].value, "clipboard");
    }

    /// Verifies selector plans filesystem path candidates for command
    /// arguments in the Mezzanine and agent prompt surfaces.
    #[test]
    fn selector_plans_path_candidates_for_prompt_arguments() {
        let _guard = CWD_TEST_LOCK.lock().unwrap();
        let original = std::env::current_dir().unwrap();
        let root = std::env::temp_dir().join(format!("mez-selector-paths-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("fixtures")).unwrap();
        fs::write(root.join("fixture.toml"), "value = true\n").unwrap();
        std::env::set_current_dir(&root).unwrap();

        let command_plan = plan_selector(
            SelectorSurface::MezzanineCommand,
            "source-file fi",
            "source-file fi".len(),
        )
        .unwrap();
        let agent_plan = plan_selector(
            SelectorSurface::AgentCommand,
            "/list-mcp ./fi",
            "/list-mcp ./fi".len(),
        )
        .unwrap();

        std::env::set_current_dir(original).unwrap();
        let _ = fs::remove_dir_all(&root);

        assert!(
            command_plan
                .candidates
                .iter()
                .any(|candidate| candidate.value == "fixture.toml")
        );
        assert!(
            command_plan
                .candidates
                .iter()
                .any(|candidate| candidate.value == "fixtures/")
        );
        assert!(
            agent_plan
                .candidates
                .iter()
                .any(|candidate| candidate.value == "./fixture.toml")
        );
    }

    /// Verifies dynamic agent argument candidates are scoped to their command.
    #[test]
    fn selector_plans_dynamic_agent_resume_candidates() {
        let extra = vec![SelectorExtraCandidate::new(
            SelectorSurface::AgentCommand,
            "resume",
            SelectorCandidate::new(
                "018f6b3a-1b2c-7000-9000-cafebabefeed",
                SelectorCandidateKind::Value,
                true,
            ),
        )];

        let plan = plan_selector_with_extra(
            SelectorSurface::AgentCommand,
            "/resume 018f",
            "/resume 018f".len(),
            &extra,
        )
        .unwrap();
        let model_plan = plan_selector_with_extra(
            SelectorSurface::AgentCommand,
            "/model 018f",
            "/model 018f".len(),
            &extra,
        );

        assert_eq!(
            plan.candidates[0].value,
            "018f6b3a-1b2c-7000-9000-cafebabefeed"
        );
        assert!(model_plan.is_none());
    }

    /// Verifies explicit skill syntax can use runtime-provided `$skill`
    /// candidates at the agent prompt root.
    #[test]
    fn selector_plans_dynamic_agent_skill_candidates() {
        let extra = vec![SelectorExtraCandidate::new(
            SelectorSurface::AgentCommand,
            "$",
            SelectorCandidate::new("$openai-docs", SelectorCandidateKind::Value, true),
        )];

        let plan = plan_selector_with_extra(
            SelectorSurface::AgentCommand,
            "$open",
            "$open".len(),
            &extra,
        )
        .unwrap();

        assert_eq!(plan.candidates[0].value, "$openai-docs");
    }

    /// Verifies selector applies candidate to current segment only.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn selector_applies_candidate_to_current_segment_only() {
        let line = "list-windows; mcp-";
        let plan = plan_selector(SelectorSurface::MezzanineCommand, line, line.len()).unwrap();
        let candidate = plan
            .candidates
            .iter()
            .find(|candidate| candidate.value == "mcp-add")
            .unwrap();

        let (line, cursor) = apply_selector_candidate(line, &plan, candidate);

        assert_eq!(line, "list-windows; mcp-add ");
        assert_eq!(cursor, line.len());
    }

    /// Verifies that non-mutating command-name shadow hints reuse selector
    /// candidates without inserting text into the prompt buffer.
    #[test]
    fn selector_shadow_hint_completes_mezzanine_command_prefix() {
        let hint = shadow_hint(SelectorSurface::MezzanineCommand, "new", 3).unwrap();

        assert_eq!(hint.insert_at, 3);
        assert_eq!(hint.text, "-window");
        assert_eq!(hint.kind, SelectorCandidateKind::Command);
    }

    /// Verifies that commands with known arguments show a placeholder only until
    /// the user starts typing an argument value.
    #[test]
    fn selector_shadow_hint_hides_placeholder_after_param_input() {
        let placeholder = shadow_hint(
            SelectorSurface::MezzanineCommand,
            "mcp-add ",
            "mcp-add ".len(),
        )
        .unwrap();
        let value_suffix = shadow_hint(
            SelectorSurface::MezzanineCommand,
            "mcp-add st",
            "mcp-add st".len(),
        )
        .unwrap();

        assert_eq!(
            placeholder.text,
            " <name> --transport <stdio|streamable-http>"
        );
        assert_eq!(value_suffix.text, "dio");

        let theme_placeholder = shadow_hint(
            SelectorSurface::MezzanineCommand,
            "set-theme ",
            "set-theme ".len(),
        )
        .unwrap();
        assert_eq!(theme_placeholder.text, " <theme>");
    }

    /// Verifies that agent slash commands expose the same prefix-completion
    /// shadow hints as the Mezzanine command prompt.
    #[test]
    fn selector_shadow_hint_completes_agent_slash_prefix() {
        let hint = shadow_hint(SelectorSurface::AgentCommand, "/log", 4).unwrap();

        assert_eq!(hint.insert_at, 4);
        assert_eq!(hint.text, "-level");
        assert_eq!(hint.kind, SelectorCandidateKind::Command);
    }

    /// Verifies dynamic argument candidates can provide shadow completion text.
    #[test]
    fn selector_shadow_hint_completes_dynamic_resume_candidate() {
        let extra = vec![SelectorExtraCandidate::new(
            SelectorSurface::AgentCommand,
            "resume",
            SelectorCandidate::new(
                "018f6b3a-1b2c-7000-9000-cafebabefeed",
                SelectorCandidateKind::Value,
                true,
            ),
        )];

        let hint = shadow_hint_with_extra(
            SelectorSurface::AgentCommand,
            "/resume 018f",
            "/resume 018f".len(),
            &extra,
        )
        .unwrap();

        assert_eq!(hint.insert_at, "/resume 018f".len());
        assert_eq!(hint.text, "6b3a-1b2c-7000-9000-cafebabefeed");
    }

    /// Verifies skill-name shadow hints do not insert completion text in the
    /// middle of an existing token.
    ///
    /// Cursor navigation inside multi-line prompts should not cause the
    /// completion renderer to duplicate part of a `$skill` token or shift the
    /// visible cursor row while the user edits surrounding text.
    #[test]
    fn selector_shadow_hint_suppresses_dynamic_skill_suffix_inside_token() {
        let extra = vec![SelectorExtraCandidate::new(
            SelectorSurface::AgentCommand,
            "$",
            SelectorCandidate::new("$review-codebase", SelectorCandidateKind::Value, true),
        )];
        let line = "$rev-codebase produce a report";

        let hint =
            shadow_hint_with_extra(SelectorSurface::AgentCommand, line, "$rev".len(), &extra);

        assert!(hint.is_none());
    }
}
