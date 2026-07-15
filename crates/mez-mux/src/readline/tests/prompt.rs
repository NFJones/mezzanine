//! Intrinsic regression tests for mux-owned prompt search behavior.

use std::ops::{Deref, DerefMut};

use crate::readline::{ReadlineOutcome, ReadlinePromptState};

/// Minimal prompt wrapper that composes lower reverse-search and baseline input.
struct TestPrompt(ReadlinePromptState);

impl TestPrompt {
    /// Creates empty lower-owned prompt state.
    fn new() -> Self {
        Self(ReadlinePromptState::new())
    }

    /// Applies input in the same lower-owned order exposed to product adapters.
    fn apply_terminal_input(&mut self, input: &[u8]) -> crate::Result<ReadlineOutcome> {
        if let Some(outcome) = self.0.apply_reverse_search_input(input)? {
            return Ok(outcome);
        }
        self.0.apply_terminal_input(input)
    }

    /// Renders lower-owned reverse-search state or the editable buffer.
    fn render(&self) -> String {
        self.0
            .rendered_reverse_search()
            .unwrap_or_else(|| self.0.buffer.rendered_line())
    }
}

impl Deref for TestPrompt {
    type Target = ReadlinePromptState;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for TestPrompt {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Verifies agent prompts use an interactive Ctrl+R reverse history search.
#[test]
fn readline_agent_prompt_ctrl_r_opens_reverse_i_search() {
    let mut prompt = TestPrompt::new();
    prompt.buffer.set_history(vec![
        "list sessions".to_string(),
        "show list sessions".to_string(),
    ]);
    prompt.buffer.set_line("li");

    assert_eq!(
        prompt.apply_terminal_input(b"\x12").unwrap(),
        ReadlineOutcome::Edited
    );

    assert_eq!(prompt.buffer.line(), "show list sessions");
    assert_eq!(
        prompt.render(),
        "(reverse-i-search'li'): show list sessions"
    );
    assert!(prompt.reverse_search_active());
}

/// Verifies reverse search accepts a typed substring after it is opened.
#[test]
fn readline_reverse_i_search_updates_query_from_typed_input() {
    let mut prompt = TestPrompt::new();
    prompt.buffer.set_history(vec![
        "list files".to_string(),
        "build project".to_string(),
        "project status".to_string(),
    ]);

    assert_eq!(
        prompt.apply_terminal_input(b"\x12").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt.apply_terminal_input(b"proj").unwrap(),
        ReadlineOutcome::Edited
    );

    assert_eq!(prompt.buffer.line(), "project status");
    assert_eq!(prompt.render(), "(reverse-i-search'proj'): project status");
}

/// Verifies Ctrl+R skips newer fuzzy-only candidates and lands on the nearest
/// history entry that contains the typed substring.
#[test]
fn readline_reverse_i_search_requires_substring_matches() {
    let mut prompt = TestPrompt::new();
    prompt.buffer.set_history(vec![
        "show /loop status".to_string(),
        "show /long/output".to_string(),
    ]);
    prompt.buffer.set_line("/loop");

    assert_eq!(
        prompt.apply_terminal_input(b"\x12").unwrap(),
        ReadlineOutcome::Edited
    );

    assert_eq!(prompt.buffer.line(), "show /loop status");
    assert_eq!(
        prompt.render(),
        "(reverse-i-search'/loop'): show /loop status"
    );
}

/// Verifies repeated Ctrl+R walks backward, Tab walks forward, and Shift-Tab
/// walks backward through matching reverse-search history entries.
#[test]
fn readline_reverse_i_search_cycles_backward_and_forward() {
    let mut prompt = TestPrompt::new();
    prompt.buffer.set_history(vec![
        "list files".to_string(),
        "build project".to_string(),
        "list tests".to_string(),
    ]);
    prompt.buffer.set_line("list");

    assert_eq!(
        prompt.apply_terminal_input(b"\x12").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "list tests");
    assert_eq!(
        prompt.apply_terminal_input(b"\x12").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "list files");
    assert_eq!(
        prompt.apply_terminal_input(b"\t").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "list tests");
    assert_eq!(
        prompt.apply_terminal_input(b"\x1b[Z").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "list files");
}

/// Verifies reverse search accepts matches without submitting the prompt.
///
/// Users often use Enter or Right arrow to choose a found item and then edit it
/// before submission. Accepting the match must therefore leave the buffer in
/// normal editing mode instead of immediately sending the command or prompt.
#[test]
fn readline_reverse_i_search_accepts_without_submit() {
    let mut prompt = TestPrompt::new();
    prompt.buffer.set_history(vec![
        "list files".to_string(),
        "build project".to_string(),
        "project status".to_string(),
    ]);

    assert_eq!(
        prompt.apply_terminal_input(b"\x12").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt.apply_terminal_input(b"proj").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt.apply_terminal_input(b"\r").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "project status");
    assert!(!prompt.reverse_search_active());

    assert_eq!(
        prompt.apply_terminal_input(b" --json").unwrap(),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "project status --json");
}

/// Verifies reverse search cancel/navigation restores the original draft.
///
/// Left, Up, Down, Escape, and Ctrl+C should get users out of incremental
/// search without leaking escape bytes into the search query or submitting the
/// currently displayed history match.
#[test]
fn readline_reverse_i_search_navigation_cancels_to_draft() {
    let inputs: [&[u8]; 5] = [b"\x1b[D", b"\x1b[A", b"\x1b[B", b"\x1b", b"\x03"];
    for input in inputs {
        let mut prompt = TestPrompt::new();
        prompt.buffer.set_history(vec![
            "list files".to_string(),
            "build project".to_string(),
            "project status".to_string(),
        ]);
        prompt.buffer.set_line("proj");

        assert_eq!(
            prompt.apply_terminal_input(b"\x12").unwrap(),
            ReadlineOutcome::Edited
        );
        assert_eq!(prompt.buffer.line(), "project status");
        assert_eq!(
            prompt.apply_terminal_input(input).unwrap(),
            ReadlineOutcome::Edited
        );
        assert_eq!(prompt.buffer.line(), "proj");
        assert!(!prompt.reverse_search_active());
    }
}
