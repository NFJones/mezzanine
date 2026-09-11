//! Product-independent prompt selector contracts and selection state.
//!
//! This module owns candidate records, shell-like token parsing, candidate
//! normalization and ranking, replacement plans, candidate application, and
//! cycling through an immutable plan. Product crates remain responsible for
//! command catalogs, dynamic candidates, and filesystem I/O.

use crate::command::plans::{ShellCommandFlag, ShellCommandSource};
use crate::command::{flag_takes_value, scan_segment_around_cursor};

/// Category for one selectable candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorCandidateKind {
    /// A top-level command.
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

/// Role the active prompt token plays once the command is parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorTokenRole {
    /// The command token itself.
    CommandName,
    /// A flag or option token.
    Flag,
    /// The value of a preceding value-taking flag.
    FlagValue {
        /// Flag whose value this token is.
        flag: String,
    },
    /// The `--shell-command`/`--command` value, run as raw shell source.
    ShellSourceValue {
        /// Flag spelling that introduced the raw shell source.
        flag: ShellCommandFlag,
    },
    /// Positional words `pipe-pane` joins into raw shell source.
    ShellSourceWords,
    /// A word the pane plan re-quotes into the spawned shell command.
    ShellSourceTail,
    /// A plain positional argument.
    Positional {
        /// Argument index inside the command segment.
        index: usize,
    },
    /// A token the pane plan never consumes.
    IgnoredTrailing,
    /// A token whose role cannot be determined safely.
    Incomplete,
}

/// Parsed token context for one prompt cursor position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorTokenContext {
    /// Cursor byte offset clamped to the preceding character boundary.
    pub cursor: usize,
    /// Query text between the token start and cursor.
    pub query: String,
    /// Start byte of the token containing the cursor.
    pub token_start: usize,
    /// End byte of the token containing the cursor.
    pub token_end: usize,
    /// Unescaped tokens before the active token in this command segment.
    pub tokens_before: Vec<String>,
    /// Start byte of the semicolon-delimited command segment.
    pub segment_start: usize,
    /// End byte of the semicolon-delimited command segment.
    pub segment_end: usize,
    /// Role the active token plays once the command is parsed.
    pub role: SelectorTokenRole,
}

impl SelectorTokenContext {
    /// Returns the active query text after outer quote and escape removal.
    ///
    /// The outer command parser removes quoting and escapes before a value is
    /// used, so this is the literal text the active argument contains.
    pub fn literal_query(&self) -> String {
        unescape_selector_shell_token(&self.query)
    }
}

/// Stateful selection over an immutable base line and product surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSelector<S> {
    /// Product surface used to produce this selection.
    pub surface: S,
    /// Prompt line before the selector inserted any candidate.
    pub base_line: String,
    /// Cursor byte offset before the selector inserted any candidate.
    pub base_cursor: usize,
    /// Current replacement plan.
    pub plan: SelectorPlan,
    /// Currently selected candidate index.
    pub selected_index: usize,
}

impl<S> ActiveSelector<S> {
    /// Creates selection state from one product-authored plan.
    pub fn new(surface: S, line: &str, cursor: usize, plan: SelectorPlan, reverse: bool) -> Self {
        let selected_index = if reverse {
            plan.candidates.len().saturating_sub(1)
        } else {
            0
        };
        Self {
            surface,
            base_line: line.to_string(),
            base_cursor: cursor,
            plan,
            selected_index,
        }
    }

    /// Moves to the next candidate, wrapping at the end.
    pub fn select_next(&mut self) {
        if !self.plan.candidates.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.plan.candidates.len();
        }
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

    /// Returns whether a selected directory should start a fresh selector.
    pub fn should_refresh_from_selected_directory(&self, line: &str, cursor: usize) -> bool {
        let Some(candidate) = self.plan.candidates.get(self.selected_index) else {
            return false;
        };
        if candidate.append_space
            || !unescape_selector_shell_token(&candidate.value).ends_with('/')
            || !unescape_selector_shell_token(&self.plan.query).ends_with('/')
        {
            return false;
        }
        self.selected_line()
            .is_some_and(|(selected_line, selected_cursor)| {
                selected_line == line && selected_cursor == cursor
            })
    }
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

/// Parses the active shell-like token and surrounding command-segment tokens.
pub fn selector_token_context(line: &str, cursor: usize) -> SelectorTokenContext {
    let cursor = clamp_to_char_boundary(line, cursor);
    let segment_start = current_command_segment_start(line, cursor);
    let scan = scan_segment_around_cursor(&line[segment_start..], cursor - segment_start);
    let token_start = segment_start + scan.token_start;
    let token_end = segment_start + scan.token_end;
    let query = line[token_start..cursor].to_string();
    let incomplete = scan.inside_quote || scan.after_escape || cursor != token_end;
    let role = selector_token_role(
        &scan.tokens_before,
        &scan.tokens_after,
        &scan.active_value,
        incomplete,
    );
    SelectorTokenContext {
        cursor,
        query,
        token_start,
        token_end,
        tokens_before: scan.tokens_before,
        segment_start,
        segment_end: segment_start + scan.segment_end,
        role,
    }
}

/// Removes duplicate candidate values while preserving provider order.
pub fn dedupe_selector_candidates(candidates: Vec<SelectorCandidate>) -> Vec<SelectorCandidate> {
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

/// Filters and stably ranks product-authored candidates for one query.
pub fn filter_and_sort_selector_candidates(
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

/// Returns the untyped suffix for a prefix-matching candidate.
pub fn selector_candidate_prefix_suffix(candidate: &str, query: &str) -> Option<String> {
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

/// Quote state used while scanning a shell-like prompt segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteState {
    None,
    Single,
    Double,
}

/// Classifies the active token from the parsed tokens around it.
///
/// The whole command segment participates because the pane plan readers scan
/// every argument, so a token the plan never consumes must not complete.
fn selector_token_role(
    tokens_before: &[String],
    tokens_after: &[String],
    active_value: &str,
    incomplete: bool,
) -> SelectorTokenRole {
    if incomplete {
        return SelectorTokenRole::Incomplete;
    }
    let Some(command) = tokens_before.first() else {
        return SelectorTokenRole::CommandName;
    };
    let args_before = &tokens_before[1..];
    let active_index = args_before.len();
    let mut args = args_before.to_vec();
    args.push(active_value.to_string());
    args.extend(tokens_after.iter().cloned());
    let source = pane_shell_command_source(command, &args);
    if let ShellCommandSource::ExplicitFlagValue { flag, value_index } = source
        && value_index == active_index
    {
        return SelectorTokenRole::ShellSourceValue { flag };
    }
    if is_flag_token(active_value) {
        return SelectorTokenRole::Flag;
    }
    if let Some(previous) = args_before.last()
        && flag_takes_value(previous)
    {
        return SelectorTokenRole::FlagValue {
            flag: previous.clone(),
        };
    }
    if let ShellCommandSource::ExplicitFlagValue { value_index, .. } = source
        && value_index != active_index
    {
        return SelectorTokenRole::IgnoredTrailing;
    }
    if is_pipe_pane_command(command) {
        return pipe_pane_token_role(&args, active_index, active_value);
    }
    match source {
        ShellCommandSource::DoubleDashTail { start } => {
            if active_index >= start {
                SelectorTokenRole::ShellSourceTail
            } else {
                SelectorTokenRole::IgnoredTrailing
            }
        }
        ShellCommandSource::PositionalWords => SelectorTokenRole::ShellSourceTail,
        ShellCommandSource::None if is_pane_spawning_command(command) => {
            if positional_slot_index(&args, active_index) == 0 {
                SelectorTokenRole::Positional {
                    index: active_index,
                }
            } else {
                SelectorTokenRole::IgnoredTrailing
            }
        }
        _ => SelectorTokenRole::Positional {
            index: active_index,
        },
    }
}

/// Classifies one `pipe-pane` token whose positional words are raw shell source.
///
/// `pipe-pane` joins its positional words with single spaces and runs the
/// result through the resolved shell, so those words use the same conservative
/// literal gate as an explicit pane shell command.
fn pipe_pane_token_role(
    args: &[String],
    active_index: usize,
    active_value: &str,
) -> SelectorTokenRole {
    if let Some(previous) = active_index
        .checked_sub(1)
        .and_then(|index| args.get(index))
        && pipe_pane_flag_takes_value(previous)
    {
        return SelectorTokenRole::FlagValue {
            flag: previous.clone(),
        };
    }
    if active_value.starts_with('-') {
        return SelectorTokenRole::Flag;
    }
    SelectorTokenRole::ShellSourceWords
}

/// Returns whether one `pipe-pane` flag consumes the following argument.
///
/// This mirrors the runtime positional-word scan so prompt completion and
/// execution agree on which words are joined into shell source.
fn pipe_pane_flag_takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "-t" | "-b" | "--buffer" | "-o" | "--output" | "-c" | "-s" | "--content"
    )
}

/// Returns whether `pipe-pane` is the active command spelling.
fn is_pipe_pane_command(command: &str) -> bool {
    command == "pipe-pane"
}

/// Classifies the shell source of the pane-creation commands.
fn pane_shell_command_source(command: &str, args: &[String]) -> ShellCommandSource {
    match command {
        "new-window" | "neww" | "new-group" | "newg" => ShellCommandSource::for_new_window(args),
        "split-window" | "splitw" => ShellCommandSource::for_split_window(args),
        _ => ShellCommandSource::None,
    }
}

/// Returns whether one command shares the pane-spawning shell readers.
fn is_pane_spawning_command(command: &str) -> bool {
    matches!(command, "new-window" | "neww" | "new-group" | "newg")
}

/// Returns whether one token is a flag or option spelling.
fn is_flag_token(value: &str) -> bool {
    value.starts_with('-') && value != "-"
}

/// Returns the positional slot of one argument index.
fn positional_slot_index(args: &[String], active_index: usize) -> usize {
    let mut slot = 0usize;
    let mut index = 0usize;
    while index < active_index {
        let argument = args[index].as_str();
        if flag_takes_value(argument) {
            index = index.saturating_add(2);
            continue;
        }
        if is_flag_token(argument) {
            index += 1;
            continue;
        }
        slot += 1;
        index += 1;
    }
    slot
}

/// Returns the start of the semicolon-delimited command containing `cursor`.
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

/// Removes shell quoting and escaping from one selector token.
pub fn unescape_selector_shell_token(value: &str) -> String {
    let mut unescaped = String::new();
    let mut quote = QuoteState::None;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            unescaped.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quote != QuoteState::Single => escaped = true,
            '\'' if quote == QuoteState::None => quote = QuoteState::Single,
            '\'' if quote == QuoteState::Single => quote = QuoteState::None,
            '"' if quote == QuoteState::None => quote = QuoteState::Double,
            '"' if quote == QuoteState::Double => quote = QuoteState::None,
            _ => unescaped.push(ch),
        }
    }
    if escaped {
        unescaped.push('\\');
    }
    unescaped
}

/// Returns a stable ordering key for equally good matches.
fn selector_order_key(candidate: &SelectorCandidate, position: usize) -> usize {
    if candidate.kind == SelectorCandidateKind::Command {
        position
    } else {
        usize::MAX
    }
}

/// Returns one fuzzy match score, where lower values rank first.
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

/// Returns whether `query` appears as an ordered subsequence in `value`.
fn is_subsequence(query: &str, value: &str) -> bool {
    let mut chars = value.chars();
    query.chars().all(|query_ch| chars.any(|ch| ch == query_ch))
}

/// Clamps a byte cursor to a valid character boundary.
fn clamp_to_char_boundary(value: &str, cursor: usize) -> usize {
    let mut cursor = cursor.min(value.len());
    while cursor > 0 && !value.is_char_boundary(cursor) {
        cursor -= 1;
    }
    cursor
}

/// Returns whether candidate insertion needs a trailing separator.
fn should_append_separator(line: &str, plan: &SelectorPlan) -> bool {
    if plan.replacement_end >= line.len() {
        return true;
    }
    line[plan.replacement_end..]
        .chars()
        .next()
        .is_none_or(|ch| !ch.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies candidate application replaces only the planned token and
    /// places one separator before adjacent non-whitespace suffix text.
    #[test]
    fn selector_candidate_application_preserves_surrounding_text() {
        let candidate = SelectorCandidate::new("new-window", SelectorCandidateKind::Command, true);
        let plan = SelectorPlan {
            replacement_start: 5,
            replacement_end: 8,
            query: "new".into(),
            candidates: vec![candidate.clone()],
        };

        assert_eq!(
            apply_selector_candidate("run: new; next", &plan, &candidate),
            ("run: new-window ; next".into(), 16)
        );
    }

    /// Verifies generic active selection wraps in both directions and applies
    /// the selected candidate through the canonical replacement function.
    #[test]
    fn active_selector_cycles_product_authored_candidates() {
        let plan = SelectorPlan {
            replacement_start: 0,
            replacement_end: 1,
            query: "a".into(),
            candidates: vec![
                SelectorCandidate::new("alpha", SelectorCandidateKind::Value, true),
                SelectorCandidate::new("alpine", SelectorCandidateKind::Value, true),
            ],
        };
        let mut selector = ActiveSelector::new("surface", "a", 1, plan, false);

        assert_eq!(selector.selected_line(), Some(("alpha ".into(), 6)));
        selector.select_previous();
        assert_eq!(selector.selected_line(), Some(("alpine ".into(), 7)));
        selector.select_next();
        assert_eq!(selector.selected_index, 0);
    }

    /// Verifies token parsing respects quoted and escaped separators while
    /// resetting preceding arguments at an unquoted command separator.
    #[test]
    fn token_context_tracks_shell_quoting_and_current_command_segment() {
        let line = r#"first "semi; colon" escaped\ value; next "two words" pa"#;

        let context = selector_token_context(line, line.len());

        assert_eq!(context.cursor, line.len());
        assert_eq!(context.query, "pa");
        assert_eq!(context.token_start, line.len() - 2);
        assert_eq!(context.token_end, line.len());
        assert_eq!(context.tokens_before, ["next", "two words"]);
    }

    /// Verifies token parsing clamps a cursor inside a multibyte character to
    /// a valid byte boundary before deriving query and replacement offsets.
    #[test]
    fn token_context_clamps_cursor_to_utf8_boundary() {
        let line = "\u{03b1}beta";

        let context = selector_token_context(line, 1);

        assert_eq!(context.cursor, 0);
        assert_eq!(context.query, "");
        assert_eq!(context.token_start, 0);
        assert_eq!(context.token_end, line.len());
    }

    /// Verifies generic candidate normalization removes duplicate values and
    /// ranks exact, command-prefix, and substring matches deterministically.
    #[test]
    fn candidate_filtering_deduplicates_and_ranks_matches() {
        let candidates = dedupe_selector_candidates(vec![
            SelectorCandidate::new("new-session", SelectorCandidateKind::Command, true),
            SelectorCandidate::new("new-window", SelectorCandidateKind::Command, true),
            SelectorCandidate::new("new", SelectorCandidateKind::Value, true),
            SelectorCandidate::new("renew", SelectorCandidateKind::Alias, true),
            SelectorCandidate::new("new", SelectorCandidateKind::Value, false),
        ]);

        let ranked = filter_and_sort_selector_candidates(candidates, "new");

        assert_eq!(
            ranked
                .iter()
                .map(|candidate| candidate.value.as_str())
                .collect::<Vec<_>>(),
            ["new", "new-session", "new-window", "renew"]
        );
        assert_eq!(
            selector_candidate_prefix_suffix("New-Window", "new"),
            Some("-Window".into())
        );
    }

    /// Verifies token contexts report plan-compatible roles for pane
    /// creation arguments, including dead and shell-source positions.
    #[test]
    fn token_context_roles_follow_pane_shell_source_precedence() {
        let command = "split-window";
        assert_eq!(
            selector_token_context(command, command.len()).role,
            SelectorTokenRole::CommandName
        );

        let flag = "split-window --";
        assert_eq!(
            selector_token_context(flag, flag.len()).role,
            SelectorTokenRole::Flag
        );

        let value = "split-window -c /tmp";
        assert_eq!(
            selector_token_context(value, value.len()).role,
            SelectorTokenRole::FlagValue {
                flag: "-c".to_string(),
            }
        );

        let explicit = "split-window --shell-command ./fi";
        assert_eq!(
            selector_token_context(explicit, explicit.len()).role,
            SelectorTokenRole::ShellSourceValue {
                flag: ShellCommandFlag::ShellCommand,
            }
        );

        // The explicit flag value is one token, so words after it are dead.
        let dead_value_tail = "split-window --shell-command cat ./fi";
        assert_eq!(
            selector_token_context(dead_value_tail, dead_value_tail.len()).role,
            SelectorTokenRole::IgnoredTrailing
        );

        let tail = "split-window -- cat ./fi";
        assert_eq!(
            selector_token_context(tail, tail.len()).role,
            SelectorTokenRole::ShellSourceTail
        );

        let positional = "split-window cat ./fi";
        assert_eq!(
            selector_token_context(positional, positional.len()).role,
            SelectorTokenRole::ShellSourceTail
        );

        let name = "new-window work";
        assert_eq!(
            selector_token_context(name, name.len()).role,
            SelectorTokenRole::Positional { index: 0 }
        );

        let dead = "new-window work extra";
        assert_eq!(
            selector_token_context(dead, dead.len()).role,
            SelectorTokenRole::IgnoredTrailing
        );

        let ignored_tail = "split-window --shell-command cat -- dead";
        assert_eq!(
            selector_token_context(ignored_tail, ignored_tail.len()).role,
            SelectorTokenRole::IgnoredTrailing
        );

        let other_command = "source-file ./fi";
        assert_eq!(
            selector_token_context(other_command, other_command.len()).role,
            SelectorTokenRole::Positional { index: 0 }
        );
    }

    /// Verifies pane shell-source classification covers every command spelling
    /// that shares the plan readers, plus `pipe-pane` positional shell words.
    #[test]
    fn token_context_roles_cover_shared_pane_shell_readers() {
        let explicit = "new-group --shell-command ./fi";
        assert_eq!(
            selector_token_context(explicit, explicit.len()).role,
            SelectorTokenRole::ShellSourceValue {
                flag: ShellCommandFlag::ShellCommand,
            }
        );

        let command = "newg --command ./fi";
        assert_eq!(
            selector_token_context(command, command.len()).role,
            SelectorTokenRole::ShellSourceValue {
                flag: ShellCommandFlag::Command,
            }
        );

        let tail = "new-group -- cat ./fi";
        assert_eq!(
            selector_token_context(tail, tail.len()).role,
            SelectorTokenRole::ShellSourceTail
        );

        let named = "newg -n group cat ./fi";
        assert_eq!(
            selector_token_context(named, named.len()).role,
            SelectorTokenRole::ShellSourceTail
        );

        // Without -n/--name the positional words are the group name, not
        // shell source, so a second positional word is never consumed.
        let unnamed = "newg cat ./fi";
        assert_eq!(
            selector_token_context(unnamed, unnamed.len()).role,
            SelectorTokenRole::IgnoredTrailing
        );

        // A dangling explicit flag still owns the value slot typed next.
        let empty = "new-group --shell-command ";
        assert_eq!(
            selector_token_context(empty, empty.len()).role,
            SelectorTokenRole::ShellSourceValue {
                flag: ShellCommandFlag::ShellCommand,
            }
        );

        // pipe-pane joins positional words into raw shell source.
        let pipe = "pipe-pane cat ./fi";
        assert_eq!(
            selector_token_context(pipe, pipe.len()).role,
            SelectorTokenRole::ShellSourceWords
        );

        let pipe_value = "pipe-pane -o ./out.log";
        assert_eq!(
            selector_token_context(pipe_value, pipe_value.len()).role,
            SelectorTokenRole::FlagValue {
                flag: "-o".to_string(),
            }
        );
    }

    /// Verifies role classification scans the whole command line so a token
    /// the pane plan never consumes is ignored even under the cursor.
    #[test]
    fn token_context_ignores_tokens_the_whole_line_plan_never_consumes() {
        for (line, cursor) in [
            ("new-window work -- tail", "new-window work".len()),
            (
                "split-window -- cat --shell-command rm",
                "split-window -- cat".len(),
            ),
            ("new-window foo --shell-command rm", "new-window foo".len()),
        ] {
            assert_eq!(
                selector_token_context(line, cursor).role,
                SelectorTokenRole::IgnoredTrailing,
                "{line}"
            );
        }

        // The words the whole-line plan does consume still classify correctly.
        let tail = "new-window work -- tail";
        assert_eq!(
            selector_token_context(tail, tail.len()).role,
            SelectorTokenRole::ShellSourceTail
        );
        let explicit = "split-window -- cat --shell-command rm";
        assert_eq!(
            selector_token_context(explicit, explicit.len()).role,
            SelectorTokenRole::ShellSourceValue {
                flag: ShellCommandFlag::ShellCommand,
            }
        );
    }

    /// Verifies partial input reports an incomplete role while the active
    /// token span still covers the whole token instead of overrunning.
    #[test]
    fn token_context_marks_partial_input_incomplete_without_overrun() {
        let line = "source-file fi";
        let inside_word = selector_token_context(line, "source-file f".len());
        assert_eq!(inside_word.query, "f");
        assert_eq!(inside_word.token_start, "source-file ".len());
        assert_eq!(inside_word.token_end, line.len());
        assert_eq!(inside_word.role, SelectorTokenRole::Incomplete);

        let quoted = "run \"foo bar\" baz";
        let inside_quote = selector_token_context(quoted, 6);
        assert_eq!(inside_quote.query, "\"f");
        assert_eq!(inside_quote.token_start, 4);
        assert_eq!(inside_quote.token_end, 13);
        assert_eq!(inside_quote.role, SelectorTokenRole::Incomplete);

        let escape = "source-file fi\\";
        let after_escape = selector_token_context(escape, escape.len());
        assert_eq!(after_escape.token_end, escape.len());
        assert_eq!(after_escape.role, SelectorTokenRole::Incomplete);

        let open_quote = "source-file 'fi";
        assert_eq!(
            selector_token_context(open_quote, open_quote.len()).role,
            SelectorTokenRole::Incomplete
        );
    }

    /// Verifies token contexts expose the current command segment span and
    /// the literal query left after outer quote and escape removal.
    #[test]
    fn token_context_reports_segment_span_and_literal_query() {
        let line = "rename-window one; source-file fi";
        let context = selector_token_context(line, line.len());

        assert_eq!(context.segment_start, "rename-window one; ".len());
        assert_eq!(context.segment_end, line.len());
        assert_eq!(context.tokens_before, ["source-file"]);
        assert_eq!(context.role, SelectorTokenRole::Positional { index: 0 });
        assert_eq!(context.literal_query(), "fi");

        let quoted = "source-file './dir with spaces/fi";
        let quoted_context = selector_token_context(quoted, quoted.len());
        assert_eq!(quoted_context.literal_query(), "./dir with spaces/fi");
    }
}
