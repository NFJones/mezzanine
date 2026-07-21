//! Unified-diff parsing and syntax-highlight span generation.
//!
//! This module owns file/hunk structure, stable line-number formatting, syntax
//! lookup, neutral syntax-theme construction, and terminal span generation.
//! Product code supplies palette colors and removes transport-specific noise.

use super::{RichTextLine, char_count, push_or_extend_style_span};
use mez_terminal::{GraphicRendition, TerminalColor, TerminalStyleSpan};
use std::{str::FromStr, sync::LazyLock};
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color as SyntectColor, FontStyle, ScopeSelectors, Style as SyntectStyle, StyleModifier, Theme,
    ThemeItem, ThemeSettings,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// Built-in syntax definitions for file and shell highlighting.
static DIFF_SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

/// Caller-selected colors for language-aware syntax highlighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxThemePalette {
    /// Default source text foreground.
    pub plain: TerminalColor,
    /// Optional full-surface background.
    pub background: Option<TerminalColor>,
    /// Comment foreground.
    pub comment: TerminalColor,
    /// String foreground.
    pub string: TerminalColor,
    /// Numeric and constant foreground.
    pub number: TerminalColor,
    /// Keyword foreground.
    pub keyword: TerminalColor,
    /// Type foreground.
    pub r#type: TerminalColor,
    /// Function foreground.
    pub function: TerminalColor,
    /// Operator and punctuation foreground.
    pub operator: TerminalColor,
}

/// Opaque syntax theme that keeps parser internals out of adapter signatures.
#[derive(Debug, Clone)]
pub struct SyntaxTheme(Theme);

/// Opaque line highlighter borrowing one mux-owned syntax theme.
pub struct SyntaxHighlighter<'a>(HighlightLines<'a>);

/// Builds a syntax theme from caller-owned semantic colors.
pub fn syntax_theme(name: &str, palette: SyntaxThemePalette) -> SyntaxTheme {
    SyntaxTheme(Theme {
        name: Some(name.to_string()),
        author: None,
        settings: ThemeSettings {
            foreground: Some(syntect_color_from_terminal_color(palette.plain)),
            background: palette.background.map(syntect_color_from_terminal_color),
            accent: Some(syntect_color_from_terminal_color(palette.keyword)),
            ..ThemeSettings::default()
        },
        scopes: syntax_theme_items(palette),
    })
}

/// Builds scope rules from a neutral syntax palette.
fn syntax_theme_items(palette: SyntaxThemePalette) -> Vec<ThemeItem> {
    [
        ("source", palette.plain, None),
        ("comment", palette.comment, Some(FontStyle::ITALIC)),
        ("string", palette.string, None),
        ("constant.numeric, constant.character, constant.language, constant.other", palette.number, None),
        ("keyword, storage, storage.modifier", palette.keyword, Some(FontStyle::BOLD)),
        ("storage.type, support.type, entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, entity.name.trait, entity.name.interface, meta.type", palette.r#type, None),
        ("entity.name.function, support.function, meta.function-call, variable.function", palette.function, None),
        ("keyword.operator, punctuation", palette.operator, None),
    ]
    .into_iter()
    .filter_map(|(selector, foreground, font_style)| {
        syntax_theme_item(selector, foreground, font_style)
    })
    .collect()
}

/// One parsed line from a unified diff hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffDisplayLine {
    /// Source-side line number when present.
    pub old_line: Option<usize>,
    /// Destination-side line number when present.
    pub new_line: Option<usize>,
    /// Unified-diff marker.
    pub marker: char,
    /// Hunk text without the marker.
    pub text: String,
}

/// One parsed file-level diff display section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffDisplaySection {
    /// Source-side file label.
    pub old_label: String,
    /// Destination-side file label.
    pub new_label: String,
    /// Parsed hunk rows.
    pub lines: Vec<DiffDisplayLine>,
    /// Hunk headers keyed by the following row index.
    pub hunk_headers: Vec<(usize, String)>,
}

/// Parses unified diff sections from cleaned shell output.
pub fn parse_unified_diff_sections(lines: &[String]) -> Vec<DiffDisplaySection> {
    let mut sections = Vec::new();
    let mut index = 0usize;
    while index + 1 < lines.len() {
        if !lines[index].starts_with("--- ") || !lines[index + 1].starts_with("+++ ") {
            index += 1;
            continue;
        }
        let old_label = clean_diff_label(&lines[index][4..]);
        let new_label = clean_diff_label(&lines[index + 1][4..]);
        index += 2;
        let mut section = DiffDisplaySection {
            old_label,
            new_label,
            lines: Vec::new(),
            hunk_headers: Vec::new(),
        };
        while index < lines.len() {
            if index + 1 < lines.len()
                && lines[index].starts_with("--- ")
                && lines[index + 1].starts_with("+++ ")
            {
                break;
            }
            let Some((mut old_line, mut new_line)) = parse_diff_hunk_header(&lines[index]) else {
                index += 1;
                continue;
            };
            section
                .hunk_headers
                .push((section.lines.len(), lines[index].to_string()));
            index += 1;
            while index < lines.len() {
                let line = &lines[index];
                if line.starts_with("@@ ")
                    || (index + 1 < lines.len()
                        && line.starts_with("--- ")
                        && lines[index + 1].starts_with("+++ "))
                {
                    break;
                }
                if line.starts_with("\\ ") {
                    index += 1;
                    continue;
                }
                if let Some(text) = line.strip_prefix('+') {
                    section.lines.push(DiffDisplayLine {
                        old_line: None,
                        new_line: Some(new_line),
                        marker: '+',
                        text: text.to_string(),
                    });
                    new_line = new_line.saturating_add(1);
                } else if let Some(text) = line.strip_prefix('-') {
                    section.lines.push(DiffDisplayLine {
                        old_line: Some(old_line),
                        new_line: None,
                        marker: '-',
                        text: text.to_string(),
                    });
                    old_line = old_line.saturating_add(1);
                } else if let Some(text) = line.strip_prefix(' ') {
                    section.lines.push(DiffDisplayLine {
                        old_line: Some(old_line),
                        new_line: Some(new_line),
                        marker: ' ',
                        text: text.to_string(),
                    });
                    old_line = old_line.saturating_add(1);
                    new_line = new_line.saturating_add(1);
                }
                index += 1;
            }
        }
        if !section.lines.is_empty() {
            sections.push(section);
        }
    }
    sections
}

/// Parses the old/new start line numbers from a unified diff hunk header.
pub fn parse_diff_hunk_header(line: &str) -> Option<(usize, usize)> {
    let mut parts = line.split_whitespace();
    if parts.next()? != "@@" {
        return None;
    }
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    Some((parse_diff_range_start(old)?, parse_diff_range_start(new)?))
}

/// Parses the start line from a unified diff range.
pub fn parse_diff_range_start(value: &str) -> Option<usize> {
    value
        .split(',')
        .next()?
        .parse::<usize>()
        .ok()
        .map(|line| line.max(1))
}

/// Cleans a unified diff file label for display.
pub fn clean_diff_label(value: &str) -> String {
    let label = value.split('\t').next().unwrap_or(value).trim();
    label
        .strip_prefix("a/")
        .or_else(|| label.strip_prefix("b/"))
        .unwrap_or(label)
        .to_string()
}

/// Returns the display path for a parsed diff section.
pub fn diff_section_path(section: &DiffDisplaySection) -> &str {
    if section.new_label == "/dev/null" {
        &section.old_label
    } else {
        &section.new_label
    }
}

/// Formats one parsed hunk line with a stable line-number gutter.
pub fn format_diff_display_line(line: &DiffDisplayLine) -> String {
    let old_line = line
        .old_line
        .map(|line| line.to_string())
        .unwrap_or_default();
    let new_line = line
        .new_line
        .map(|line| line.to_string())
        .unwrap_or_default();
    format!("{old_line:>6} {new_line:>6} {}{}", line.marker, line.text)
}

/// Creates a syntax highlighter for a displayed file path.
pub fn diff_highlighter_for_path<'a>(
    path: &str,
    theme: &'a SyntaxTheme,
) -> Option<SyntaxHighlighter<'a>> {
    let syntax = diff_syntax_for_path(path)?;
    Some(SyntaxHighlighter(HighlightLines::new(syntax, &theme.0)))
}

/// Resolves a syntax definition from a diff display path.
pub fn diff_syntax_for_path(path: &str) -> Option<&'static SyntaxReference> {
    if path == "/dev/null" {
        return None;
    }
    let syntax_set = &*DIFF_SYNTAX_SET;
    syntax_set
        .find_syntax_for_file(path)
        .ok()
        .flatten()
        .filter(|syntax| syntax.name != "Plain Text")
}

/// Builds one safe syntect theme item from a constant scope selector.
fn syntax_theme_item(
    selector: &str,
    foreground: TerminalColor,
    font_style: Option<FontStyle>,
) -> Option<ThemeItem> {
    ScopeSelectors::from_str(selector)
        .ok()
        .map(|scope| ThemeItem {
            scope,
            style: StyleModifier {
                foreground: Some(syntect_color_from_terminal_color(foreground)),
                background: None,
                font_style,
            },
        })
}

/// Converts a terminal color into a syntect RGB color.
fn syntect_color_from_terminal_color(color: TerminalColor) -> SyntectColor {
    match color {
        TerminalColor::Rgb(red, green, blue) => SyntectColor {
            r: red,
            g: green,
            b: blue,
            a: 0xff,
        },
        TerminalColor::Indexed(index) => syntect_color_from_indexed_terminal_color(index),
    }
}

/// Converts an indexed terminal color into a conservative RGB approximation.
fn syntect_color_from_indexed_terminal_color(index: u8) -> SyntectColor {
    const ANSI_16: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0x80, 0x00, 0x00),
        (0x00, 0x80, 0x00),
        (0x80, 0x80, 0x00),
        (0x00, 0x00, 0x80),
        (0x80, 0x00, 0x80),
        (0x00, 0x80, 0x80),
        (0xc0, 0xc0, 0xc0),
        (0x80, 0x80, 0x80),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x00, 0x00, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    let (red, green, blue) = ANSI_16
        .get(usize::from(index))
        .copied()
        .unwrap_or((0xe4, 0xef, 0xe8));
    SyntectColor {
        r: red,
        g: green,
        b: blue,
        a: 0xff,
    }
}

/// Appends syntax color spans to a rendered line after its presentation gutter.
pub fn append_syntax_spans(
    rendered: &mut RichTextLine,
    text_start: usize,
    text: &str,
    highlighter: &mut SyntaxHighlighter<'_>,
) {
    let Ok(highlighted) = highlighter.0.highlight_line(text, &DIFF_SYNTAX_SET) else {
        return;
    };
    let mut column = text_start;
    for (style, segment) in highlighted {
        let rendition = diff_syntect_rendition(style);
        let width = char_count(segment);
        push_or_extend_style_span(
            &mut rendered.style_spans,
            TerminalStyleSpan {
                start: column,
                length: width,
                rendition,
            },
        );
        column = column.saturating_add(width);
    }
}

/// Converts syntect token style into Mezzanine's terminal rendition model.
fn diff_syntect_rendition(style: SyntectStyle) -> GraphicRendition {
    GraphicRendition {
        bold: style.font_style.contains(FontStyle::BOLD),
        italic: style.font_style.contains(FontStyle::ITALIC),
        underline: style.font_style.contains(FontStyle::UNDERLINE),
        foreground: Some(TerminalColor::Rgb(
            style.foreground.r,
            style.foreground.g,
            style.foreground.b,
        )),
        ..GraphicRendition::default()
    }
}

/// Resolves a non-plain syntax by file extension.
pub fn syntax_for_extension(extension: &str) -> Option<&'static SyntaxReference> {
    DIFF_SYNTAX_SET
        .find_syntax_by_extension(extension)
        .filter(|syntax| syntax.name != "Plain Text")
}

/// Creates a syntax highlighter from a file extension when available.
pub fn syntax_highlighter_for_extension<'a>(
    extension: &str,
    theme: &'a SyntaxTheme,
) -> Option<SyntaxHighlighter<'a>> {
    let syntax = syntax_for_extension(extension)?;
    Some(SyntaxHighlighter(HighlightLines::new(syntax, &theme.0)))
}

/// Resolves a non-plain syntax from the first token of a fenced-code info string.
pub fn syntax_for_fence(info: &str) -> Option<&'static SyntaxReference> {
    let token = info.split_whitespace().next()?.to_ascii_lowercase();
    let token = match token.as_str() {
        "rs" => "rust",
        "js" => "javascript",
        "ts" => "typescript",
        "py" => "python",
        "yml" => "yaml",
        "shell" | "shellscript" => "sh",
        "c++" => "cpp",
        "c#" => "cs",
        _ => token.as_str(),
    };
    DIFF_SYNTAX_SET
        .find_syntax_by_token(token)
        .or_else(|| DIFF_SYNTAX_SET.find_syntax_by_extension(token))
        .filter(|syntax| syntax.name != "Plain Text")
}

/// Creates a stateful syntax highlighter for a fenced Markdown code block.
pub fn syntax_highlighter_for_fence<'a>(
    info: &str,
    theme: &'a SyntaxTheme,
) -> Option<SyntaxHighlighter<'a>> {
    let syntax = syntax_for_fence(info)?;
    Some(SyntaxHighlighter(HighlightLines::new(syntax, &theme.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::RichTextLineKind;

    /// Verifies unified-diff parsing preserves file labels, hunk numbering, and
    /// stable display gutters as a mux-owned presentation contract.
    #[test]
    fn unified_diff_parser_builds_numbered_file_sections() {
        let lines = vec![
            "--- a/src/lib.rs".to_string(),
            "+++ b/src/lib.rs".to_string(),
            "@@ -2,2 +2,2 @@".to_string(),
            " old".to_string(),
            "+new".to_string(),
        ];
        let sections = parse_unified_diff_sections(&lines);
        assert_eq!(sections.len(), 1);
        assert_eq!(diff_section_path(&sections[0]), "src/lib.rs");
        assert_eq!(sections[0].lines[0].old_line, Some(2));
        assert_eq!(sections[0].lines[1].new_line, Some(3));
        assert!(format_diff_display_line(&sections[0].lines[1]).contains("+new"));
    }

    /// Verifies language highlighting produces terminal spans while accepting
    /// only neutral palette and rich-text view-model inputs.
    #[test]
    fn syntax_highlighting_appends_terminal_spans() {
        let palette = SyntaxThemePalette {
            plain: TerminalColor::Rgb(220, 220, 220),
            background: None,
            comment: TerminalColor::Rgb(100, 100, 100),
            string: TerminalColor::Rgb(80, 180, 100),
            number: TerminalColor::Rgb(180, 140, 80),
            keyword: TerminalColor::Rgb(180, 80, 120),
            r#type: TerminalColor::Rgb(80, 140, 180),
            function: TerminalColor::Rgb(140, 120, 220),
            operator: TerminalColor::Rgb(160, 160, 160),
        };
        let theme = syntax_theme("test", palette);
        let mut highlighter = diff_highlighter_for_path("src/lib.rs", &theme).unwrap();
        let mut line = RichTextLine {
            display: "fn main() {}".to_string(),
            style_spans: Vec::new(),
            copy_text: None,
            kind: RichTextLineKind::Normal,
        };
        append_syntax_spans(&mut line, 0, "fn main() {}", &mut highlighter);
        assert!(!line.style_spans.is_empty());
    }
}
