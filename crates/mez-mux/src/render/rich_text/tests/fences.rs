//! Specialized fence precedence, generic syntax and literal fallback regressions.
//!
//! Hidden delimiters remain available to raw-source copy in every generic path.

use super::*;

/// Verifies generic fenced Rust blocks hide their delimiters while retaining
/// a syntax-highlighted body and the complete authored source for copy.
#[test]
fn fenced_rust_blocks_use_theme_syntax_spans() {
    let lines = render_markdown("```RUST title\nfn main() {}\n```", &theme(), None);

    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].display, "fn main() {}");
    assert_eq!(
        lines
            .iter()
            .map(|line| line.copy_text.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("```RUST title\nfn main() {}\n```")]
    );
    assert!(
        lines[0].style_spans.iter().any(|span| {
            matches!(
                span.rendition.foreground,
                Some(
                    TerminalColor::Rgb(32, 33, 34)
                        | TerminalColor::Rgb(35, 36, 37)
                        | TerminalColor::Rgb(38, 39, 40)
                        | TerminalColor::Rgb(41, 42, 43)
                )
            )
        }),
        "{lines:?}"
    );
}

/// Verifies generic fenced blocks insert one presentation-only buffer after
/// prose while hiding delimiters for both recognized and literal languages.
#[test]
fn fenced_code_blocks_hide_delimiters_and_buffer_after_prose() {
    let rust = render_markdown("Before\n```rust\nfn main() {}\n```", &theme(), None);
    assert_eq!(
        rust.iter()
            .map(|line| line.display.as_str())
            .collect::<Vec<_>>(),
        ["Before", "", "fn main() {}"]
    );
    assert_eq!(rust[1].copy_text.as_deref(), Some(COPY_SKIP_LINE));
    assert_eq!(
        rust[2].copy_text.as_deref(),
        Some("```rust\nfn main() {}\n```")
    );

    let literal = render_markdown("```unknown\nbody\n```", &theme(), None);
    assert_eq!(literal.len(), 1, "{literal:?}");
    assert_eq!(literal[0].display, "body");
    assert_eq!(
        literal[0].copy_text.as_deref(),
        Some("```unknown\nbody\n```")
    );

    let empty_info = render_markdown("```\nplain\n```", &theme(), None);
    assert_eq!(empty_info.len(), 1, "{empty_info:?}");
    assert_eq!(empty_info[0].display, "plain");
    assert_eq!(empty_info[0].copy_text.as_deref(), Some("```\nplain\n```"));

    let followed_by_prose = render_markdown("```rust\nfn main() {}\n```\nAfter", &theme(), None);
    assert_eq!(
        followed_by_prose
            .iter()
            .map(|line| line.display.as_str())
            .collect::<Vec<_>>(),
        ["fn main() {}", "", "After"]
    );
    assert_eq!(
        followed_by_prose[1].copy_text.as_deref(),
        Some(COPY_SKIP_LINE)
    );
}

/// Verifies specialized fenced renderers run before generic highlighting
/// and can preserve a literal body when their own presentation fails.
#[test]
fn specialized_fenced_renderer_precedes_generic_highlighting() {
    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed_calls = std::rc::Rc::clone(&calls);
    let mut renderer = move |block: FencedCodeBlock<'_>| {
        observed_calls
            .borrow_mut()
            .push((block.info.to_string(), block.body.to_string()));
        FencedCodeBlockOutcome::PreserveLiteral
    };

    let lines = render_markdown_with_fenced_block_renderer(
        "```rust\nfn main() {}\n```",
        &theme(),
        None,
        &mut renderer,
    );

    assert_eq!(
        calls.borrow().as_slice(),
        [("rust".to_string(), "fn main() {}\n".to_string())]
    );
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].display, "fn main() {}");
    assert_eq!(
        lines[0].style_spans[0].rendition.foreground,
        Some(TerminalColor::Rgb(10, 11, 12))
    );
}
