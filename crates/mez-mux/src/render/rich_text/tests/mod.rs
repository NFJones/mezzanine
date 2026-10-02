//! Regression owners for rich-text layout, source association and Markdown.
//!
//! Behavior modules share only the semantic theme fixture; each exercises the
//! same public render facade and canonical source-to-display coordinates.

use super::*;

mod fences;
mod spacing;
mod tables;
mod wrapping;

/// Supplies distinctive semantic colors shared by multiple regression owners.
fn theme() -> RichTextTheme {
    RichTextTheme {
        heading: TerminalColor::Rgb(1, 2, 3),
        structural: TerminalColor::Rgb(4, 5, 6),
        link: TerminalColor::Rgb(7, 8, 9),
        inline_code: TerminalColor::Rgb(10, 11, 12),
        table_alternate_row: TerminalColor::Rgb(13, 14, 15),
        diff_addition: TerminalColor::Rgb(16, 17, 18),
        diff_deletion: TerminalColor::Rgb(19, 20, 21),
        syntax: Some(crate::render::SyntaxThemePalette {
            plain: TerminalColor::Rgb(20, 21, 22),
            background: None,
            comment: TerminalColor::Rgb(23, 24, 25),
            string: TerminalColor::Rgb(26, 27, 28),
            number: TerminalColor::Rgb(29, 30, 31),
            keyword: TerminalColor::Rgb(32, 33, 34),
            r#type: TerminalColor::Rgb(35, 36, 37),
            function: TerminalColor::Rgb(38, 39, 40),
            operator: TerminalColor::Rgb(41, 42, 43),
        }),
    }
}
