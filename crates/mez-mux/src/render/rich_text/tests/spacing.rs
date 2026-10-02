//! Authored versus presentation-only spacing around Markdown blocks.
//!
//! Visible buffers must not introduce source-copy rows or trailing spacers.

use super::*;

/// Verifies headings have exactly one presentation-only buffer before
/// following prose, list, and code blocks without changing copy source.
#[test]
fn markdown_headings_buffer_following_blocks() {
    for (markdown, expected) in [
        ("# Heading\nAfter", vec!["", "Heading", "", "After"]),
        ("# Heading\n- item", vec!["", "Heading", "", "• item"]),
        (
            "# Heading\n```text\nbody\n```",
            vec!["", "Heading", "", "body"],
        ),
    ] {
        let lines = render_markdown(markdown, &theme(), None);
        assert_eq!(
            lines
                .iter()
                .map(|line| line.display.as_str())
                .collect::<Vec<_>>(),
            expected,
            "{lines:?}"
        );
        assert_eq!(lines[2].copy_text.as_deref(), Some(COPY_SKIP_LINE));
    }

    let authored_blank = render_markdown("# Heading\n\nAfter", &theme(), None);
    assert_eq!(
        authored_blank
            .iter()
            .map(|line| line.display.as_str())
            .collect::<Vec<_>>(),
        ["", "Heading", "", "After"]
    );
    assert_eq!(authored_blank[2].copy_text.as_deref(), Some(""));
}

/// Verifies prose paragraphs have exactly one blank presentation row
/// before following block content without retaining a trailing spacer.
///
/// CommonMark requires an authored blank line between consecutive prose
/// paragraphs, while lists and fenced blocks can follow directly. The
/// presentation policy should normalize all three transitions to one
/// visible buffer and preserve whether that row came from source or was
/// synthesized only for display.
#[test]
fn markdown_paragraphs_buffer_following_blocks() {
    for (markdown, expected, blank_copy) in [
        ("First\n\nSecond", vec!["First", "", "Second"], ""),
        (
            "Before\n- item",
            vec!["Before", "", "• item"],
            COPY_SKIP_LINE,
        ),
        (
            "Before\n```text\nbody\n```",
            vec!["Before", "", "body"],
            COPY_SKIP_LINE,
        ),
    ] {
        let lines = render_markdown(markdown, &theme(), None);
        assert_eq!(
            lines
                .iter()
                .map(|line| line.display.as_str())
                .collect::<Vec<_>>(),
            expected,
            "{lines:?}"
        );
        assert_eq!(lines[1].copy_text.as_deref(), Some(blank_copy));
    }

    let paragraph_only = render_markdown("Only paragraph", &theme(), None);
    assert_eq!(
        paragraph_only
            .iter()
            .map(|line| line.display.as_str())
            .collect::<Vec<_>>(),
        ["Only paragraph"]
    );
}
