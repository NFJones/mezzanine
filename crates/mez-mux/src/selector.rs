//! Product-independent prompt selector contracts and selection state.
//!
//! This module owns candidate records, replacement plans, candidate
//! application, and cycling through an immutable plan. Product crates remain
//! responsible for command catalogs, dynamic candidates, and filesystem I/O.

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
}
