//! Product-independent prompt selector contracts and selection state.
//!
//! This module owns candidate records, shell-like token parsing, candidate
//! normalization and ranking, replacement plans, candidate application, and
//! cycling through an immutable plan. Product crates remain responsible for
//! command catalogs, dynamic candidates, and filesystem I/O.

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
            || !candidate.value.ends_with('/')
            || !self.plan.query.ends_with('/')
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

/// Parses the active shell-like token and preceding command-segment tokens.
pub fn selector_token_context(line: &str, cursor: usize) -> SelectorTokenContext {
    let cursor = clamp_to_char_boundary(line, cursor);
    let segment_start = current_command_segment_start(line, cursor);
    let token_start = segment_start + current_token_start(&line[segment_start..cursor]);
    let token_end = cursor + current_token_end(&line[cursor..]);
    SelectorTokenContext {
        cursor,
        query: line[token_start..cursor].to_string(),
        token_start,
        token_end,
        tokens_before: shell_tokens(&line[segment_start..token_start]),
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

/// Returns the current token start inside one command segment.
fn current_token_start(segment: &str) -> usize {
    let mut quote = QuoteState::None;
    let mut escaped = false;
    let mut token_start = segment.len();
    let mut token_open = false;
    for (index, ch) in segment.char_indices() {
        if quote == QuoteState::None && !escaped && ch.is_whitespace() {
            token_start = index + ch.len_utf8();
            token_open = false;
            continue;
        }
        if !token_open {
            token_start = index;
            token_open = true;
        }
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
            _ => {}
        }
    }
    token_start
}

/// Returns the current token end inside the trailing prompt slice.
fn current_token_end(segment: &str) -> usize {
    let mut quote = QuoteState::None;
    let mut escaped = false;
    for (index, ch) in segment.char_indices() {
        if quote == QuoteState::None && !escaped && (ch.is_whitespace() || ch == ';') {
            return index;
        }
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
            _ => {}
        }
    }
    segment.len()
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

/// Parses shell-like tokens while removing quotes and escapes.
fn shell_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut quote = QuoteState::None;
    let mut escaped = false;
    let mut token_start = None;
    for (index, ch) in value.char_indices() {
        if quote == QuoteState::None && !escaped && ch.is_whitespace() {
            if let Some(start) = token_start.take() {
                tokens.push(unescape_selector_shell_token(&value[start..index]));
            }
            continue;
        }
        token_start.get_or_insert(index);
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
            _ => {}
        }
    }
    if let Some(start) = token_start {
        tokens.push(unescape_selector_shell_token(&value[start..]));
    }
    tokens
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
}
