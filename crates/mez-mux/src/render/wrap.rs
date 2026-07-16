//! Generic terminal-cell text wrapping.
//!
//! Wrapping preserves explicit line breaks, prefers whitespace boundaries, and
//! falls back to grapheme boundaries for unbroken text. Callers provide the
//! display-cell width; product-specific width caps remain outside this module.

use mez_terminal::{terminal_emoji_width, terminal_grapheme_width};
use unicode_segmentation::UnicodeSegmentation;

/// Wraps a text block to a positive terminal display-cell width.
pub fn wrap_text(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    value
        .split('\n')
        .flat_map(|line| wrap_physical_line(line, width))
        .collect()
}

/// Wraps multiple logical rows while preserving each explicit row boundary.
pub fn wrap_lines(lines: &[String], width: usize) -> Vec<String> {
    lines
        .iter()
        .flat_map(|line| wrap_text(line, width))
        .collect()
}

/// Wraps one newline-free logical row.
fn wrap_physical_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut remaining = line;
    while !remaining.is_empty() {
        let mut used = 0usize;
        let mut end_byte = 0usize;
        let mut saw_content = false;
        let mut last_break = None;
        for (byte_index, grapheme) in remaining.grapheme_indices(true) {
            let grapheme_width = terminal_grapheme_width(grapheme, terminal_emoji_width());
            if used.saturating_add(grapheme_width) > width {
                break;
            }
            if grapheme.chars().all(char::is_whitespace) {
                if saw_content {
                    last_break = Some((byte_index, byte_index.saturating_add(grapheme.len())));
                }
            } else {
                saw_content = true;
            }
            used = used.saturating_add(grapheme_width);
            end_byte = byte_index.saturating_add(grapheme.len());
        }
        if end_byte >= remaining.len() {
            rows.push(remaining.to_string());
            break;
        }
        if end_byte == 0
            && let Some(grapheme) = remaining.graphemes(true).next()
        {
            end_byte = grapheme.len();
        }
        if remaining[end_byte..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            rows.push(remaining[..end_byte].to_string());
            remaining = remaining[end_byte..].trim_start_matches(char::is_whitespace);
            continue;
        }
        if let Some((break_byte, next_byte)) = last_break
            && break_byte > 0
        {
            rows.push(remaining[..break_byte].to_string());
            remaining = remaining[next_byte..].trim_start_matches(char::is_whitespace);
        } else {
            rows.push(remaining[..end_byte].to_string());
            remaining = &remaining[end_byte..];
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::char_count;

    /// Verifies wrapping preserves explicit rows and prefers word boundaries.
    #[test]
    fn wrapping_preserves_newlines_and_words() {
        assert_eq!(
            wrap_text("alpha beta gamma\n\nbottom", 10),
            vec!["alpha beta", "gamma", "", "bottom"]
        );
    }

    /// Verifies unbroken tokens split at terminal grapheme boundaries.
    #[test]
    fn wrapping_hard_splits_unbroken_tokens() {
        assert_eq!(wrap_text("abcdefghijkl", 4), vec!["abcd", "efgh", "ijkl"]);
    }

    /// Verifies wide graphemes count by terminal display width.
    #[test]
    fn wrapping_counts_wide_graphemes() {
        let rows = wrap_text("✅✅✅", 4);
        assert_eq!(rows, vec!["✅✅", "✅"]);
        assert!(rows.iter().all(|line| char_count(line) <= 4));
    }
}
