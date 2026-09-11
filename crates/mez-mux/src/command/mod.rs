//! Multiplexer command-language grammar.
//!
//! This module owns dependency-neutral command invocations, flag and
//! positional argument queries, quoting, tokenization, and semicolon sequence
//! parsing. Product command registries, dispatch, persistence, and error
//! projection remain in the composition crate.

use crate::{MuxError, Result};

pub mod plans;
pub mod presentation;

/// Parsed command name and ordered arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInvocation {
    /// Canonical or user-supplied command name.
    pub name: String,
    /// Ordered command arguments after tokenization.
    pub args: Vec<String>,
}

impl CommandInvocation {
    /// Returns command arguments that are not flags or flag values.
    pub fn positional_args(&self) -> Vec<&str> {
        positional_args_from_slice(&self.args)
    }

    /// Returns the value immediately following an arbitrary flag spelling.
    pub fn flag_value(&self, flag: &str) -> Option<&str> {
        flag_value(&self.args, flag)
    }

    /// Returns the value following `-t`, when present.
    pub fn target_arg(&self) -> Option<&str> {
        flag_value(&self.args, "-t")
    }

    /// Returns the value following `-s`, when present.
    pub fn source_arg(&self) -> Option<&str> {
        flag_value(&self.args, "-s")
    }

    /// Returns the value following `-c`, when present.
    pub fn start_directory_arg(&self) -> Option<&str> {
        flag_value(&self.args, "-c")
    }

    /// Returns whether either supplied spelling is present.
    pub fn has_flag(&self, short: &str, long: &str) -> bool {
        self.args
            .iter()
            .any(|argument| argument == short || argument == long)
    }
}

/// Parses one or more semicolon-separated command invocations.
pub fn parse_command_sequence(input: &str) -> Result<Vec<CommandInvocation>> {
    let segments = split_semicolon_sequence(input)?;
    let mut commands = Vec::new();
    for segment in segments {
        let tokens = tokenize_command(&segment)?;
        if tokens.is_empty() {
            continue;
        }
        commands.push(CommandInvocation {
            name: tokens[0].clone(),
            args: tokens[1..].to_vec(),
        });
    }
    if commands.is_empty() {
        return Err(MuxError::invalid_args(
            "command input did not contain a command",
        ));
    }
    Ok(commands)
}

/// Encodes one selector value as a single argument of the outer parser.
///
/// The value is wrapped in outer single quotes, where the outer parser treats
/// every other byte literally, and an embedded single quote is spliced with
/// `'\''`. The outer parser performs no shell expansion, so the encoded text
/// parses back to exactly `value` as one token.
pub fn encode_selector_argument(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len().saturating_add(2));
    encoded.push('\'');
    for character in value.chars() {
        if character == '\'' {
            encoded.push_str("'\\''");
        } else {
            encoded.push(character);
        }
    }
    encoded.push('\'');
    encoded
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].as_str())
}

/// Returns whether a flag consumes the argument that follows it.
pub(crate) fn flag_takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "-t" | "-s"
            | "-c"
            | "-n"
            | "--name"
            | "-x"
            | "--columns"
            | "-y"
            | "--rows"
            | "--percent"
            | "--axis"
            | "--delta"
            | "--edge"
            | "--amount"
            | "--scope"
            | "--match"
            | "--exact-sha256"
            | "--shell-classification"
            | "--justification"
            | "--reason"
            | "--epoch"
            | "--content"
    )
}

fn positional_args_from_slice(args: &[String]) -> Vec<&str> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        if flag_takes_value(argument) {
            index += 2;
            continue;
        }
        if argument.starts_with('-') {
            index += 1;
            continue;
        }
        values.push(argument);
        index += 1;
    }
    values
}

/// One outer-parser token with its raw byte span inside a command segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandTokenSpan {
    /// Start byte offset of the token.
    pub start: usize,
    /// Exclusive end byte offset of the token.
    pub end: usize,
    /// Token text after outer quote and escape removal.
    pub value: String,
}

/// Prompt-oriented scan of one command segment around the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentCursorScan {
    /// Start byte offset of the token containing the cursor.
    pub token_start: usize,
    /// Exclusive end byte offset of the token containing the cursor.
    pub token_end: usize,
    /// Unquoted tokens strictly before the token containing the cursor.
    pub tokens_before: Vec<String>,
    /// Unquoted tokens strictly after the token containing the cursor.
    pub tokens_after: Vec<String>,
    /// Unquoted text of the token containing the cursor.
    pub active_value: String,
    /// End byte offset of the scanned command segment.
    pub segment_end: usize,
    /// Whether the cursor sits inside an open quote.
    pub inside_quote: bool,
    /// Whether the cursor directly follows an escape byte.
    pub after_escape: bool,
}

/// Raw state produced by scanning one command segment.
struct SegmentScan {
    tokens: Vec<CommandTokenSpan>,
    semicolon: Option<usize>,
    end_quote: QuoteState,
    end_escaped: bool,
    cursor: Option<CursorScanState>,
}

/// Quote and escape state observed at one cursor offset.
struct CursorScanState {
    inside_quote: bool,
    after_escape: bool,
}

/// Scans one command segment into span-bearing tokens.
///
/// The scan is tolerant: an unterminated quote or dangling escape is reported
/// through the end state instead of failing, so prompt completion can inspect
/// partially typed input. When `stop_at_semicolon` is set, the first unquoted
/// `;` ends the scan so callers can inspect one prompt segment at a time.
fn scan_command_segment(
    input: &str,
    cursor: Option<usize>,
    stop_at_semicolon: bool,
) -> SegmentScan {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = QuoteState::None;
    let mut escaped = false;
    let mut token_start = None::<usize>;
    let mut semicolon = None::<usize>;
    let mut cursor_state = cursor.map(|_| CursorScanState {
        inside_quote: false,
        after_escape: false,
    });
    let mut cursor_recorded = false;
    for (index, character) in input.char_indices() {
        if let (Some(cursor_offset), Some(state)) = (cursor, cursor_state.as_mut())
            && !cursor_recorded
            && index >= cursor_offset
        {
            state.inside_quote = quote != QuoteState::None;
            state.after_escape = escaped;
            cursor_recorded = true;
        }
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match character {
            '\\' if quote != QuoteState::Single => {
                token_start.get_or_insert(index);
                escaped = true;
            }
            '\'' if quote == QuoteState::None => {
                token_start.get_or_insert(index);
                quote = QuoteState::Single;
            }
            '\'' if quote == QuoteState::Single => quote = QuoteState::None,
            '"' if quote == QuoteState::None => {
                token_start.get_or_insert(index);
                quote = QuoteState::Double;
            }
            '"' if quote == QuoteState::Double => quote = QuoteState::None,
            character if character.is_whitespace() && quote == QuoteState::None => {
                if let Some(start) = token_start.take() {
                    tokens.push(CommandTokenSpan {
                        start,
                        end: index,
                        value: std::mem::take(&mut current),
                    });
                }
            }
            ';' if quote == QuoteState::None && stop_at_semicolon => {
                if let Some(start) = token_start.take() {
                    tokens.push(CommandTokenSpan {
                        start,
                        end: index,
                        value: std::mem::take(&mut current),
                    });
                }
                semicolon = Some(index);
                break;
            }
            _ => {
                token_start.get_or_insert(index);
                current.push(character);
            }
        }
    }
    if !cursor_recorded && let Some(state) = cursor_state.as_mut() {
        state.inside_quote = quote != QuoteState::None;
        state.after_escape = escaped;
    }
    if let Some(start) = token_start.take() {
        tokens.push(CommandTokenSpan {
            start,
            end: input.len(),
            value: current,
        });
    }
    SegmentScan {
        tokens,
        semicolon,
        end_quote: quote,
        end_escaped: escaped,
        cursor: cursor_state,
    }
}

/// Scans one command segment around a cursor for prompt completion.
pub(crate) fn scan_segment_around_cursor(segment: &str, cursor: usize) -> SegmentCursorScan {
    let cursor = cursor.min(segment.len());
    let scan = scan_command_segment(segment, Some(cursor), true);
    let active = scan
        .tokens
        .iter()
        .position(|token| token.start <= cursor && cursor <= token.end);
    let (token_start, token_end, tokens_before, tokens_after, active_value) = match active {
        Some(index) => (
            scan.tokens[index].start,
            scan.tokens[index].end,
            scan.tokens[..index]
                .iter()
                .map(|token| token.value.clone())
                .collect(),
            scan.tokens[index.saturating_add(1)..]
                .iter()
                .map(|token| token.value.clone())
                .collect(),
            scan.tokens[index].value.clone(),
        ),
        None => (
            cursor,
            cursor,
            scan.tokens
                .iter()
                .take_while(|token| token.end <= cursor)
                .map(|token| token.value.clone())
                .collect(),
            Vec::new(),
            String::new(),
        ),
    };
    let cursor_state = scan.cursor.unwrap_or(CursorScanState {
        inside_quote: false,
        after_escape: false,
    });
    SegmentCursorScan {
        token_start,
        token_end,
        tokens_before,
        tokens_after,
        active_value,
        segment_end: scan.semicolon.unwrap_or(segment.len()),
        inside_quote: cursor_state.inside_quote,
        after_escape: cursor_state.after_escape,
    }
}

fn split_semicolon_sequence(input: &str) -> Result<Vec<String>> {
    let mut segments = Vec::new();
    let mut offset = 0usize;
    loop {
        let scan = scan_command_segment(&input[offset..], None, true);
        let Some(separator) = scan.semicolon else {
            if scan.end_escaped {
                return Err(MuxError::invalid_args("command sequence ends with escape"));
            }
            if scan.end_quote != QuoteState::None {
                return Err(MuxError::invalid_args(
                    "unterminated quoted command argument",
                ));
            }
            segments.push(input[offset..].trim().to_string());
            return Ok(segments);
        };
        segments.push(input[offset..offset + separator].trim().to_string());
        offset += separator.saturating_add(1);
    }
}

/// Splits one command segment into ordered tokens with raw byte spans.
///
/// A dangling escape reports `command ends with escape` and an open quote
/// reports `unterminated quoted command argument`, matching
/// [`tokenize_command`] exactly.
pub(crate) fn tokenize_command_spans(input: &str) -> Result<Vec<CommandTokenSpan>> {
    let scan = scan_command_segment(input, None, false);
    if scan.end_escaped {
        return Err(MuxError::invalid_args("command ends with escape"));
    }
    if scan.end_quote != QuoteState::None {
        return Err(MuxError::invalid_args(
            "unterminated quoted command argument",
        ));
    }
    Ok(scan.tokens)
}

fn tokenize_command(input: &str) -> Result<Vec<String>> {
    Ok(tokenize_command_spans(input)?
        .into_iter()
        .map(|token| token.value)
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteState {
    None,
    Single,
    Double,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies quoted arguments and target flags survive command parsing.
    #[test]
    fn parses_command_with_quotes_and_target_flag() {
        let commands = parse_command_sequence("rename-window -t @1 \"work tree\"").unwrap();

        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "rename-window");
        assert_eq!(commands[0].target_arg(), Some("@1"));
        assert_eq!(commands[0].args[2], "work tree");
    }

    /// Verifies explicit empty quoted arguments remain present and ordered.
    #[test]
    fn preserves_explicit_empty_quoted_arguments() {
        let commands = parse_command_sequence("send --body \"\" '' keep").unwrap();

        assert_eq!(
            commands[0].args,
            vec!["--body", "", "", "keep"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    /// Verifies semicolons split commands only outside quoted arguments.
    #[test]
    fn splits_semicolon_sequence_outside_quotes() {
        let commands = parse_command_sequence("select-window -t @1; rename-window 'a;b'").unwrap();

        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].name, "select-window");
        assert_eq!(commands[1].args[0], "a;b");
    }

    /// Verifies unterminated quoted arguments fail with the mux invalid-args
    /// category and stable diagnostic.
    #[test]
    fn rejects_unterminated_quotes() {
        let error = parse_command_sequence("rename-window \"unterminated").unwrap_err();

        assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);
        assert!(error.message().contains("unterminated quoted"));
    }

    /// Verifies the selector encoder round-trips hostile byte sequences
    /// through the real outer parser as one literal argument.
    #[test]
    fn encodes_selector_arguments_back_to_literal_values() {
        let values = [
            "",
            "plain.txt",
            "two words.txt",
            "it's here",
            "double\"quote",
            "semi;colon",
            "$(touch PWNED)",
            "`touch PWNED`",
            "*?.txt",
            "[brackets]",
            "line\nbreak.txt",
            "carriage\rreturn.txt",
            "~/notes.txt",
            "#hash",
            "bang!",
            "amp&ersand",
            "pipe|operator",
            "angle<in>out",
            "(parens)",
            "{braces}",
            "back\\slash",
            "-leading-dash",
            "\u{03b1}\u{03b2}.txt",
            "quote'\\\"mix;$(x)`y`",
            "'",
            "\\",
        ];
        for value in values {
            let encoded = encode_selector_argument(value);
            let commands = parse_command_sequence(&format!("source-file {encoded} tail")).unwrap();

            assert_eq!(commands.len(), 1, "value {value:?}");
            assert_eq!(commands[0].name, "source-file");
            assert_eq!(commands[0].args[0], value, "value {value:?}");
            assert_eq!(commands[0].args[1], "tail", "value {value:?}");
        }
        assert_eq!(encode_selector_argument(""), "''");
    }

    /// Verifies span-bearing tokenization reports raw token spans that map
    /// back to the exact source text.
    #[test]
    fn tokenizes_command_segments_with_spans() {
        let input = "rename-window \"work tree\" tail";
        let spans = tokenize_command_spans(input).unwrap();

        assert_eq!(spans.len(), 3);
        assert_eq!(&input[spans[0].start..spans[0].end], "rename-window");
        assert_eq!(spans[0].value, "rename-window");
        assert_eq!(&input[spans[1].start..spans[1].end], "\"work tree\"");
        assert_eq!(spans[1].value, "work tree");
        assert_eq!(&input[spans[2].start..spans[2].end], "tail");
        assert_eq!(spans[2].value, "tail");
        assert!(tokenize_command_spans("escaped\\").is_err());
    }
}
