//! Actual prompt regressions for retained completion context and draft ownership.
//!
//! Each fixture uses distinct temporary pane roots without changing process cwd.
//! Discovery is completed through the prompt's existing worker; only explicit
//! Tab/Shift-Tab may edit the draft, and cwd changes must retire old selections.

use super::*;
use std::path::{Path, PathBuf};

/// Isolated roots whose identically prefixed entries identify their source cwd.
struct Roots {
    root: PathBuf,
    a: PathBuf,
    b: PathBuf,
}

impl Roots {
    /// Creates independent directory candidates for forward/reverse cycling.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mez-prompt-selector-context-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let a = root.join("a");
        let b = root.join("b");
        for (cwd, names) in [
            (&a, ["entry-a1", "entry-a2"]),
            (&b, ["entry-b1", "entry-b2"]),
        ] {
            for name in names {
                std::fs::create_dir_all(cwd.join(name)).unwrap();
            }
        }
        Self { root, a, b }
    }
}

impl Drop for Roots {
    /// Removes only this fixture's uniquely allocated tree after worker use.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Completes the exact current prompt lookup, preserving production ownership.
fn complete(prompt: &ReadlinePrompt) {
    prompt.filesystem_selector_candidates.complete_for_tests(
        match prompt.kind {
            ReadlinePromptKind::Agent => SelectorSurface::AgentCommand,
            ReadlinePromptKind::Command => SelectorSurface::MezzanineCommand,
        },
        prompt.buffer.line(),
        prompt.buffer.cursor(),
        prompt.selector_working_directory.as_deref(),
    );
}

/// Opens a real retained selector from root A at a prefix shared with root B.
fn opened(cwd: &Path) -> ReadlinePrompt {
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    prompt.set_selector_working_directory(Some(cwd.to_path_buf()));
    prompt.buffer.set_line("inspect ./entry-");
    complete(&prompt);
    assert_eq!(prompt_outcome(&mut prompt, b"\t"), ReadlineOutcome::Edited);
    assert_eq!(prompt.buffer.line(), "inspect ./entry-a1/");
    assert!(prompt.selector.is_some());
    prompt
}

/// A cwd change must retire already-open A candidates before either cycle key
/// can apply them. Invalidation leaves the explicitly selected draft unchanged;
/// at identical prefix/text/cursor a new lookup and shadow hint must use B only.
#[test]
fn readline_selector_context_changed_cwd_retires_active_candidates_and_hints() {
    let roots = Roots::new();
    for key in [b"\t".as_slice(), b"\x1b[Z".as_slice()] {
        let mut prompt = opened(&roots.a);
        let draft = prompt.buffer.draft_snapshot();
        let cursor = prompt.buffer.cursor();
        prompt.set_selector_working_directory(Some(roots.b.clone()));
        assert_eq!(prompt.buffer.draft_snapshot(), draft);
        assert_eq!(prompt.buffer.cursor(), cursor);
        assert_eq!(prompt_outcome(&mut prompt, key), ReadlineOutcome::Noop);
        assert_eq!(prompt.buffer.draft_snapshot(), draft);

        prompt.buffer.set_line("inspect ./entry-");
        complete(&prompt);
        assert_eq!(prompt.render_with_shadow_hint(), "⟩ inspect ./entry-b1/");
        assert_eq!(prompt.buffer.line(), "inspect ./entry-");
        assert_eq!(prompt_outcome(&mut prompt, key), ReadlineOutcome::Edited);
        assert!(prompt.buffer.line().starts_with("inspect ./entry-b"));
    }
}

/// Reinstalling the identical cwd is common before every input batch and must
/// preserve immutable selector base text and ordinary forward/reverse wrapping.
#[test]
fn readline_selector_context_same_cwd_preserves_cycle_and_wrap() {
    let roots = Roots::new();
    let mut prompt = opened(&roots.a);
    let revision = prompt.selector_revision;
    prompt.set_selector_working_directory(Some(roots.a.clone()));
    assert_eq!(prompt.selector_revision, revision);
    assert_eq!(prompt_outcome(&mut prompt, b"\t"), ReadlineOutcome::Edited);
    assert_eq!(prompt.buffer.line(), "inspect ./entry-a2/");
    assert_eq!(prompt_outcome(&mut prompt, b"\t"), ReadlineOutcome::Edited);
    assert_eq!(prompt.buffer.line(), "inspect ./entry-a1/");
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[Z"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "inspect ./entry-a2/");
}

/// Unknown or missing roots invalidate retained candidates without rewriting
/// user input. Restoring a known root must start fresh rather than revive A.
#[test]
fn readline_selector_context_unknown_and_missing_cwd_retire_selection() {
    let roots = Roots::new();
    for cwd in [None, Some(roots.root.join("missing"))] {
        let mut prompt = opened(&roots.a);
        let draft = prompt.buffer.draft_snapshot();
        prompt.set_selector_working_directory(cwd);
        assert!(prompt.selector.is_none());
        assert_eq!(prompt.buffer.draft_snapshot(), draft);
        prompt.set_selector_working_directory(Some(roots.b.clone()));
        prompt.buffer.set_line("inspect ./entry-");
        complete(&prompt);
        assert_eq!(prompt_outcome(&mut prompt, b"\t"), ReadlineOutcome::Edited);
        assert_eq!(prompt.buffer.line(), "inspect ./entry-b1/");
    }
}

/// Runtime candidates are prompt-local eligibility inputs, not authority. A
/// changed list must retire its old retained plan, while identical refreshes
/// preserve cycling and no setter may rewrite the explicitly selected draft.
#[test]
fn readline_selector_context_runtime_candidates_refresh_retained_selection() {
    let candidate = |name| {
        SelectorExtraCandidate::new(
            SelectorSurface::AgentCommand,
            "@",
            SelectorCandidate::new(name, SelectorCandidateKind::Value, true),
        )
    };
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    let old = vec![candidate("@context-a1"), candidate("@context-a2")];
    prompt.set_selector_extra_candidates(old.clone());
    prompt.buffer.set_line("ask @context-");
    assert_eq!(prompt_outcome(&mut prompt, b"\t"), ReadlineOutcome::Edited);
    prompt.set_selector_extra_candidates(old);
    assert_eq!(prompt_outcome(&mut prompt, b"\t"), ReadlineOutcome::Edited);
    assert_eq!(prompt.buffer.line(), "ask @context-a2 ");
    let draft = prompt.buffer.draft_snapshot();
    prompt.set_selector_extra_candidates([candidate("@context-b")]);
    assert!(prompt.selector.is_none());
    assert_eq!(prompt.buffer.draft_snapshot(), draft);
    prompt.buffer.set_line("ask @context-");
    assert_eq!(prompt.render_with_shadow_hint(), "⟩ ask @context-b");
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[Z"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "ask @context-b ");
    prompt.set_selector_extra_candidates([]);
    assert!(prompt.selector.is_none());
}

/// Whole-buffer replacement and cursor-only movement can occur outside raw
/// terminal input (external edits and idle Ctrl+C). Neither may resurrect the
/// immutable base draft of a retained selector on the next cycle key.
#[test]
fn readline_selector_context_replaced_draft_or_cursor_cannot_reuse_old_plan() {
    let roots = Roots::new();
    let mut prompt = opened(&roots.a);
    prompt.buffer.set_line("");
    let mut fresh = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    assert_eq!(
        prompt_outcome(&mut prompt, b"\t"),
        prompt_outcome(&mut fresh, b"\t")
    );
    assert_eq!(prompt.buffer.line(), fresh.buffer.line());

    let mut prompt = opened(&roots.a);
    prompt
        .buffer
        .set_line_and_cursor("unrelated draft".to_string(), 3);
    let draft = prompt.buffer.draft_snapshot();
    let _ = prompt_outcome(&mut prompt, b"\x1b[Z");
    assert_eq!(prompt.buffer.draft_snapshot(), draft);

    let mut prompt = opened(&roots.a);
    let line = prompt.buffer.line().to_string();
    prompt
        .buffer
        .set_line_and_cursor(line.clone(), "inspect ".len());
    let _ = prompt_outcome(&mut prompt, b"\t");
    assert_eq!(prompt.buffer.line(), line);
    assert_eq!(prompt.buffer.cursor(), "inspect ".len());
}

/// Render clones share an operational cache but not necessarily context values.
/// Equal local revision numbers must not allow B to render A's runtime hint.
#[test]
fn readline_selector_context_divergent_clones_do_not_share_wrong_hint() {
    let mut a = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    a.buffer.set_line("ask @context-");
    let mut b = a.clone();
    for (prompt, name) in [(&mut a, "@context-a"), (&mut b, "@context-b")] {
        prompt.set_selector_extra_candidates([SelectorExtraCandidate::new(
            SelectorSurface::AgentCommand,
            "@",
            SelectorCandidate::new(name, SelectorCandidateKind::Value, true),
        )]);
    }
    assert_eq!(a.render_with_shadow_hint(), "⟩ ask @context-a");
    assert_eq!(b.render_with_shadow_hint(), "⟩ ask @context-b");
}

/// The command surface uses parser-encoded path values, but must retire A's
/// retained plan exactly like agent prose when the authoritative cwd changes.
#[test]
fn readline_selector_context_command_cwd_change_retires_encoded_plan() {
    let roots = Roots::new();
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Command);
    prompt.set_selector_working_directory(Some(roots.a.clone()));
    prompt.buffer.set_line("source-file entry-");
    complete(&prompt);
    assert_eq!(prompt_outcome(&mut prompt, b"\t"), ReadlineOutcome::Edited);
    let draft = prompt.buffer.draft_snapshot();
    prompt.set_selector_working_directory(Some(roots.b.clone()));
    assert!(prompt.selector.is_none());
    assert_eq!(prompt.buffer.draft_snapshot(), draft);
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[Z"),
        ReadlineOutcome::Noop
    );
    prompt.buffer.set_line("source-file entry-");
    complete(&prompt);
    assert_eq!(prompt_outcome(&mut prompt, b"\t"), ReadlineOutcome::Edited);
    assert_eq!(prompt.buffer.line(), "source-file 'entry-b1/'");
}

/// Explicit directory selection must continue into children; ordinary cycling
/// still uses the immutable base cursor and preserves UTF-8 prefix and suffix.
/// Reorganizing retained-plan checks must not confuse deliberate descent with
/// unrelated whole-buffer replacement or byte-offset cursor movement.
#[test]
fn readline_selector_context_directory_descent_and_utf8_middle_preserve_draft() {
    let roots = Roots::new();
    std::fs::create_dir_all(roots.a.join("entry-a1/child-one")).unwrap();
    std::fs::create_dir_all(roots.a.join("entry-a1/child-two")).unwrap();
    std::fs::create_dir_all(roots.a.join("entry-a1/child-one/grandchild")).unwrap();
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    prompt.set_selector_working_directory(Some(roots.a.clone()));
    prompt.buffer.set_line("inspect ./entry-a1/");
    complete(&prompt);
    assert_eq!(prompt_outcome(&mut prompt, b"\t"), ReadlineOutcome::Edited);
    complete(&prompt);
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[Z"),
        ReadlineOutcome::Edited
    );
    assert_eq!(
        prompt.buffer.line(),
        "inspect ./entry-a1/child-one/grandchild/"
    );

    let line = "é界 inspect ./entry- suffix";
    let cursor = "é界 inspect ./entry-".len();
    prompt.clear_transient_editing_state();
    prompt.buffer.set_line_and_cursor(line, cursor);
    complete(&prompt);
    assert_eq!(prompt_outcome(&mut prompt, b"\t"), ReadlineOutcome::Edited);
    assert_eq!(prompt.buffer.line(), "é界 inspect ./entry-a1/ suffix");
    assert_eq!(prompt.buffer.cursor(), "é界 inspect ./entry-a1/".len());
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[Z"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "é界 inspect ./entry-a2/ suffix");
    assert_eq!(prompt.buffer.cursor(), "é界 inspect ./entry-a2/".len());
}

/// An actual A lookup is paused after acquiring its request. At identical
/// text/cursor, B becomes authoritative before A returns. The existing exact
/// key/generation fence must keep A out of B's hints and candidate application;
/// a separately recreated prompt must not inherit the old worker snapshot.
#[test]
fn readline_selector_context_late_a_lookup_cannot_repopulate_b_or_new_prompt() {
    let roots = Roots::new();
    let mut prompt = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    prompt.buffer.set_line("inspect ./entry-");
    prompt.set_selector_working_directory(Some(roots.a.clone()));
    let gate = prompt
        .filesystem_selector_candidates
        .hold_next_lookup_for_tests();
    assert_eq!(prompt.render_with_shadow_hint(), "⟩ inspect ./entry-");
    gate.wait_started();
    prompt.set_selector_working_directory(Some(roots.b.clone()));
    assert_eq!(prompt.render_with_shadow_hint(), "⟩ inspect ./entry-");
    let draft = prompt.buffer.draft_snapshot();
    gate.release_and_wait();
    complete(&prompt);
    assert_eq!(prompt.render_with_shadow_hint(), "⟩ inspect ./entry-b1/");
    assert_eq!(prompt.buffer.draft_snapshot(), draft);
    assert_eq!(
        prompt_outcome(&mut prompt, b"\x1b[Z"),
        ReadlineOutcome::Edited
    );
    assert_eq!(prompt.buffer.line(), "inspect ./entry-b2/");

    let mut replacement = ReadlinePrompt::new(ReadlinePromptKind::Agent);
    replacement.buffer.set_line("inspect ./entry-");
    replacement.set_selector_working_directory(Some(roots.b.clone()));
    complete(&replacement);
    assert_eq!(
        replacement.render_with_shadow_hint(),
        "⟩ inspect ./entry-b1/"
    );
    assert!(replacement.selector.is_none());
}
