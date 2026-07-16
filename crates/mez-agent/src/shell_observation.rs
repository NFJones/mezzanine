//! Provider-independent shell-wrapper filtering and model observations.
//!
//! This module owns byte-level filtering for Mezzanine shell wrapper echoes,
//! transport-frame decoding, and model-facing shell observation cleanup. The
//! product process adapter keeps pane lifecycle state, terminal OSC parsing,
//! and retained PTY fragments.

use crate::decode_shell_output_transport_with_diagnostics;

/// Runs the mez wrapper echo line is hidden operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn mez_wrapper_echo_line_is_hidden(line: &[u8], command_lines: &[String]) -> bool {
    if line.contains(&0x1b) {
        return false;
    }
    let text = String::from_utf8_lossy(line);
    let normalized = text.trim_matches(['\r', '\n']).trim();
    if normalized.is_empty() {
        return false;
    }
    mez_wrapper_echo_text_is_hidden(normalized, command_lines)
}

/// Runs the mez wrapper echo line visible bytes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn mez_wrapper_echo_line_visible_bytes(line: &[u8], command_lines: &[String]) -> Vec<u8> {
    let mut visible = Vec::with_capacity(line.len());
    let mut text_segment = Vec::new();
    let mut index = 0usize;
    while index < line.len() {
        if line[index] == 0x1b {
            append_visible_mez_wrapper_text_segment(&mut visible, &text_segment, command_lines);
            text_segment.clear();
            let escape_end = terminal_escape_sequence_end(line, index);
            visible.extend_from_slice(&line[index..escape_end]);
            index = escape_end;
        } else {
            text_segment.push(line[index]);
            index += 1;
        }
    }
    append_visible_mez_wrapper_text_segment(&mut visible, &text_segment, command_lines);
    visible
}

/// Runs the append visible mez wrapper text segment operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn append_visible_mez_wrapper_text_segment(
    visible: &mut Vec<u8>,
    segment: &[u8],
    command_lines: &[String],
) {
    let normalized = String::from_utf8_lossy(segment);
    let trimmed = normalized.trim_matches(['\r', '\n']).trim();
    let promptless = mez_wrapper_echo_text_without_inline_prompts(trimmed);
    let wrapped_transport_printf = trimmed.contains("__MEZ_SHELL_OUTPUT_BASE64_")
        && !trimmed.starts_with("__MEZ_SHELL_OUTPUT_BASE64_")
        && (trimmed.contains("printf") || promptless.contains("printf"));
    if segment.is_empty()
        || mez_wrapper_echo_line_is_hidden(segment, command_lines)
        || promptless.starts_with("if [ -n \"$MEZ_STTY_STATE\"")
        || wrapped_transport_printf
        || trimmed.starts_with("__mez_tx_")
        || trimmed.starts_with("MEZ_STTY_STATE=")
        || trimmed.starts_with("stty -echo")
        || trimmed.starts_with("stty \"")
        || promptless.starts_with("__mez_tx_")
        || promptless.starts_with("stty -echo")
        || promptless.starts_with("stty \"")
        || promptless == "done"
        || trimmed.starts_with("unset -f __mez_tx_")
        || promptless.starts_with("unset -f __mez_tx_")
    {
        return;
    }
    visible
        .extend_from_slice(mez_wrapper_echo_text_without_leading_prompts(&normalized).as_bytes());
}

/// Runs the terminal escape sequence end operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn terminal_escape_sequence_end(bytes: &[u8], escape_index: usize) -> usize {
    let Some(kind) = bytes.get(escape_index + 1).copied() else {
        return bytes.len();
    };
    match kind {
        b']' => {
            let mut index = escape_index + 2;
            while index < bytes.len() {
                if bytes[index] == 0x07 {
                    return index + 1;
                }
                if bytes[index] == 0x1b && bytes.get(index + 1).is_some_and(|byte| *byte == b'\\') {
                    return index + 2;
                }
                index += 1;
            }
            escape_index + 1
        }
        b'[' => {
            let mut index = escape_index + 2;
            while index < bytes.len() {
                if (0x40..=0x7e).contains(&bytes[index]) {
                    return index + 1;
                }
                index += 1;
            }
            escape_index + 1
        }
        _ => (escape_index + 2).min(bytes.len()),
    }
}

/// Runs the mez wrapper echo line is possible prefix operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn mez_wrapper_echo_line_is_possible_prefix(line: &[u8], command_lines: &[String]) -> bool {
    if line.contains(&0x1b) {
        return false;
    }
    let text = String::from_utf8_lossy(line);
    let normalized = text.trim_matches(['\r', '\n']).trim();
    if normalized.is_empty() {
        return true;
    }
    let promptless = mez_wrapper_echo_text_without_inline_prompts(normalized);
    if normalized.starts_with("if [ \"$M") || promptless.starts_with("if [ \"$M") {
        return true;
    }
    [
        "MEZ_MARKER_TOKEN",
        "MEZ_TURN",
        "MEZ_AGENT",
        "MEZ_PANE",
        "MEZ_STATUS",
        "MEZ_RESTORE_ERREXIT",
        "MEZ_RESTORE_NOUNSET",
        "MEZ_RESTORE_HISTORY",
        "MEZ_HISTORY_",
        "MEZ_COMMAND_FILE",
        "MEZ_COMMAND_B64",
        "MEZ_OUTPUT_DROPPED",
        "MEZ_WRITE_STATUS",
        "HISTFILE=/dev/null",
        "case $- in *e*)",
        "set +o history",
        "set -o history",
        "history -d",
        "if [ -n \"$MEZ_COMMAND_FILE\"",
        "if [ -n \"$MEZ_COMMAND_B64\"",
        "if [ \"$MEZ_WRITE_STATUS\"",
        "else",
        "elif command -v",
        "fi",
        "MEZ_WRITE_STATUS=$?",
        "MEZ_STATUS=$MEZ_WRITE_STATUS",
        "rm -f -- \"$MEZ_COMMAND_FILE\"",
        "rm -f -- \"$MEZ_COMMAND_B64\"",
        "unset MEZ_COMMAND_FILE",
        "unset MEZ_COMMAND_FILE MEZ_COMMAND_B64",
        "fish_private_mode",
        "history delete --",
        "printf",
        "f '\\n%s\\n' '__MEZ_SHELL_OUTPUT_BASE64_",
        "f '%s %s\\n'",
        "printf '\\033]133;",
        "env -u MEZ_MARKER_TOKEN",
        "unset MEZ_MARKER_TOKEN",
        "set -l MEZ_MARKER_TOKEN",
        "set -l MEZ_TURN",
        "set -l MEZ_AGENT",
        "set -l MEZ_PANE",
        "set -l MEZ_STATUS",
        "set -e MEZ_MARKER_TOKEN",
        "MEZ_COMMAND_",
        ">",
        "$",
        "begin",
        "end",
        "{",
        "}",
        "command ",
        "eval ",
    ]
    .iter()
    .any(|hidden| {
        hidden.starts_with(normalized)
            || normalized.starts_with(hidden)
            || hidden.starts_with(promptless.as_str())
            || promptless.starts_with(hidden)
    }) || command_lines.iter().any(|command| {
        let command = command.trim();
        !command.is_empty()
            && (command.starts_with(normalized)
                || mez_wrapper_echo_text_ends_with_command(normalized, command))
    })
}

/// Runs the mez wrapper filter bytes may contain boilerplate operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn mez_wrapper_filter_bytes_may_contain_boilerplate(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let promptless = mez_wrapper_echo_text_without_inline_prompts(&text);
    [
        "MEZ_MARKER_TOKEN",
        "MEZ_TURN",
        "MEZ_AGENT",
        "MEZ_PANE",
        "MEZ_STATUS",
        "MEZ_RESTORE_ERREXIT",
        "MEZ_RESTORE_NOUNSET",
        "MEZ_RESTORE_HISTORY",
        "MEZ_HISTORY_",
        "MEZ_COMMAND_FILE",
        "MEZ_OUTPUT_FILE",
        "MEZ_WRITE_STATUS",
        "HISTFILE=/dev/null",
        "MEZ_COMMAND_",
        "mez_marker=",
        "\\033]133",
        "env -u MEZ_MARKER_TOKEN",
        "unset MEZ_MARKER_TOKEN",
        "printf '%s\\n' '__MEZ_SHELL_OUTPUT_BASE64_",
        "printf '\\n%s\\n' '__MEZ_SHELL_OUTPUT_BASE64_",
        "set -l MEZ_MARKER_TOKEN",
        "set -e MEZ_MARKER_TOKEN",
    ]
    .iter()
    .any(|marker| text.contains(marker) || promptless.contains(marker))
}

/// Marker substrings that identify Mezzanine wrapper echo text.
///
/// Each marker is checked against both the normalized line and its
/// prompt-stripped variant so a single update here covers both branches.
const WRAPPER_MARKERS: &[&str] = &[
    "MEZ_MARKER_TOKEN",
    "MEZ_TURN",
    "MEZ_AGENT",
    "MEZ_PANE",
    "MEZ_STATUS",
    "MEZ_RESTORE_ERREXIT",
    "MEZ_RESTORE_NOUNSET",
    "MEZ_RESTORE_HISTORY",
    "MEZ_HISTORY_",
    "MEZ_COMMAND_FILE",
    "MEZ_COMMAND_B64",
    "MEZ_COMMAND_",
    "MEZ_OUTPUT_FILE",
    "MEZ_WRITE_STATUS",
    "HISTFILE=/dev/null",
    "set +o history",
    "set -o history",
    "history -d",
    "fish_private_mode",
    "history delete --",
    "case $- in *e*)",
    "mez_marker=",
    "printf '\\033]133;C;mez_marker",
    "printf '\\033]133;D;%s;mez_marker",
    "env -u MEZ_MARKER_TOKEN -u MEZ_TURN -u MEZ_AGENT -u MEZ_PANE",
    "cat > \"$MEZ_COMMAND_FILE\"",
    "if command -v",
    "elif command -v",
    "setsid(); exec @ARGV",
    "os.setsid()",
    "</dev/null",
    "unset MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS",
    "set -l MEZ_STATUS $status",
    "set -e MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS",
];

/// Exact-match tokens for wrapper text (case-sensitive).
const WRAPPER_EXACT_TOKENS: &[&str] = &["else", "fi", ">", "$", "begin", "end", "{", "}"];

/// Prefix patterns that identify wrapper lines.
const WRAPPER_PREFIXES: &[&str] = &["if [ \"$M", "eval "];

/// Runs the mez wrapper echo text is hidden operation for this subsystem.
///
/// Checks a normalized (trimmed, control-stripped) line against known
/// Mezzanine wrapper markers, exact tokens, and prefixes. Both the raw
/// line and a prompt-stripped variant are checked against each marker so
/// the definition only lives in one place.
pub fn mez_wrapper_echo_text_is_hidden(normalized: &str, command_lines: &[String]) -> bool {
    let promptless = mez_wrapper_echo_text_without_inline_prompts(normalized);
    if WRAPPER_MARKERS
        .iter()
        .any(|m| normalized.contains(m) || promptless.contains(m))
        || WRAPPER_EXACT_TOKENS
            .iter()
            .any(|t| normalized == *t || promptless == *t)
        || WRAPPER_PREFIXES
            .iter()
            .any(|p| normalized.starts_with(p) || promptless.starts_with(p))
    {
        return true;
    }
    if (normalized.starts_with("command ") && normalized.contains(" -c "))
        || (promptless.starts_with("command ") && promptless.contains(" -c "))
    {
        return true;
    }
    if normalized
        .split_whitespace()
        .all(|token| matches!(token, "$" | ">" | "#"))
    {
        return true;
    }
    // Only hide base64 markers when they appear in a printf wrapper command,
    // not the marker output lines themselves. The marker output lines must
    // survive so decode_shell_output_transport can find and strip them.
    if ((normalized.contains("printf '%s\\n'") || normalized.contains("printf '\\n%s\\n'"))
        && normalized.contains("__MEZ_SHELL_OUTPUT_BASE64_"))
        || ((promptless.contains("printf '%s\\n'") || promptless.contains("printf '\\n%s\\n'"))
            && promptless.contains("__MEZ_SHELL_OUTPUT_BASE64_"))
    {
        return true;
    }
    command_lines.iter().any(|command| {
        let command = command.trim();
        !command.is_empty() && mez_wrapper_echo_text_ends_with_command(normalized, command)
    })
}

/// Runs the mez wrapper echo text ends with command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn mez_wrapper_echo_text_ends_with_command(normalized: &str, command: &str) -> bool {
    if normalized == command {
        return true;
    }
    let normalized_without_prompts = mez_wrapper_echo_text_without_inline_prompts(normalized);
    let command_without_prompts = mez_wrapper_echo_text_without_inline_prompts(command);
    if normalized_without_prompts == command_without_prompts {
        return true;
    }
    if normalized_without_prompts
        .strip_suffix(&command_without_prompts)
        .and_then(|prefix| prefix.chars().next_back())
        .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '$' | '>' | '#'))
    {
        return true;
    }
    let Some(prefix) = normalized.strip_suffix(command) else {
        return false;
    };
    prefix
        .chars()
        .next_back()
        .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '$' | '>' | '#'))
}

/// Runs the mez wrapper echo text without inline prompts operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn mez_wrapper_echo_text_without_inline_prompts(value: &str) -> String {
    value
        .replace("$  ", "")
        .replace("$ ", "")
        .replace(">  ", "")
        .replace("> ", "")
}

/// Strips terminal control traffic from shell transaction bytes before they are
/// stored as model-visible command output.
///
/// The terminal renderer still receives OSC/ANSI data for state updates, but
/// model context should contain the useful textual command output rather than
/// prompt styling, cursor movement, or bracketed-paste toggles.
pub fn shell_observation_without_terminal_controls(bytes: &[u8]) -> Vec<u8> {
    let mut text = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            0x1b => {
                index = terminal_escape_sequence_end(bytes, index);
            }
            b'\n' | b'\r' | b'\t' => {
                text.push(bytes[index]);
                index += 1;
            }
            byte if byte.is_ascii_control() => {
                index += 1;
            }
            byte => {
                text.push(byte);
                index += 1;
            }
        }
    }
    text
}

/// Returns whether a cleaned transaction line is an interactive shell prompt
/// repaint rather than command output.
///
/// Prompt repaint text is especially common when a user's PS1 is styled with
/// powerline glyphs. Keeping it out of model-visible observations prevents a
/// small capture window from filling with prompts before the command output.
pub fn shell_observation_line_looks_like_prompt(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    ["\u{e0b6}", "\u{e0b0}", "\u{e0b4}", "\u{f412}", "❯", "➜"]
        .iter()
        .any(|marker| trimmed.contains(marker))
        || matches!(trimmed, "$" | ">" | "#")
        || shell_observation_line_has_common_prompt_suffix(trimmed)
}

/// Returns whether one stripped line looks like a plain shell prompt tail.
pub fn shell_observation_line_has_common_prompt_suffix(trimmed: &str) -> bool {
    let Some((prefix, suffix)) = trimmed.rsplit_once(' ') else {
        return false;
    };
    matches!(suffix, "$" | ">" | "#")
        && (prefix.starts_with('~')
            || prefix.starts_with('/')
            || prefix.contains('@')
            || prefix.contains(':'))
}

/// Produces model-visible command output for an agent shell transaction.
///
/// User-facing rendering keeps a richer stream so the terminal can update
/// state, but the model only needs command stdout/stderr. This removes
/// Mezzanine wrapper echo, shell prompt repaint, and terminal styling while
/// preserving the actual command output that should feed follow-up reasoning.
///
/// Command echo (the interactive shell echoing the command input) is only
/// hidden on the first occurrence before any real output appears. After
/// the first legitimate output line, matching lines are treated as
/// legitimate command output rather than shell echo.
pub fn agent_shell_transaction_observation_bytes(bytes: &[u8], command: &str) -> Vec<u8> {
    let stripped = shell_observation_without_terminal_controls(bytes);
    let text = String::from_utf8_lossy(&stripped);
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let command_lines = vec![command.to_string()];
    let mut output = String::new();
    let mut found_output = false;
    for line in normalized.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !output.ends_with('\n') && !output.is_empty() {
                output.push('\n');
            }
            continue;
        }
        if !found_output && mez_wrapper_echo_text_is_hidden(trimmed, &command_lines) {
            continue;
        }
        let cleaned = mez_wrapper_echo_text_without_leading_prompts(line);
        if cleaned.trim().is_empty() || shell_observation_line_looks_like_prompt(&cleaned) {
            if !found_output {
                continue;
            }
            output.push_str(cleaned.trim_end());
            output.push('\n');
            continue;
        }
        found_output = true;
        output.push_str(cleaned.trim_end());
        output.push('\n');
    }
    output.into_bytes()
}

/// Returns pane bytes that belong to the command before this transaction's end
/// marker.
///
/// A PTY read can contain the command's final output, Mezzanine's OSC 133 end
/// marker, and the parent shell's next prompt repaint in one chunk. The prompt
/// bytes are useful to readiness detection, but they must not replace the
/// transient latest-output line shown for the just-finished command.
pub fn agent_shell_transaction_bytes_before_end_marker<'a>(
    bytes: &'a [u8],
    marker: &str,
) -> &'a [u8] {
    let marker_field = format!("mez_marker={marker}");
    let Some(marker_index) = find_byte_subsequence(bytes, marker_field.as_bytes()) else {
        return bytes;
    };
    let Some(escape_index) = bytes[..marker_index].iter().rposition(|byte| *byte == 0x1b) else {
        return bytes;
    };
    if bytes.get(escape_index + 1) == Some(&b']')
        && bytes
            .get(escape_index + 2..marker_index)
            .is_some_and(|payload_prefix| payload_prefix.starts_with(b"133;D;"))
    {
        &bytes[..escape_index]
    } else {
        bytes
    }
}

/// Finds the first occurrence of a byte sequence inside another byte slice.
pub fn find_byte_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Returns the latest non-empty model-visible shell output lines.
pub fn latest_agent_shell_transaction_output_lines(output: &str, max_lines: usize) -> Vec<String> {
    if max_lines == 0 {
        return Vec::new();
    }
    let raw_output = output;
    let decoded = decode_shell_output_transport_with_diagnostics(output);
    let output = if decoded.diagnostics.saw_begin_marker {
        let mut output = decoded.output;
        if let Some((_before, tail)) = raw_output.rsplit_once("__MEZ_SHELL_OUTPUT_BASE64_END__")
            && !tail.trim().is_empty()
            && !tail.contains("__MEZ_SHELL_OUTPUT_BASE64_")
        {
            output.push_str(tail);
        }
        output
    } else {
        output.to_string()
    };
    let empty_commands: Vec<String> = Vec::new();
    let mut lines = output
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .rev()
        .map(sanitized_shell_output_status_line)
        .map(|line| line.trim_end().to_string())
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty()
                && !trimmed.starts_with("$ ")
                && !trimmed.starts_with("> ")
                && !trimmed.starts_with("__MEZ_SHELL_OUTPUT_BASE64_")
                && !mez_wrapper_echo_text_is_hidden(trimmed, &empty_commands)
                && !shell_observation_line_looks_like_prompt(trimmed)
        })
        .take(max_lines)
        .collect::<Vec<_>>();
    lines.reverse();
    lines
}

/// Returns terminal-renderable bytes for Mezzanine-owned shell transaction
/// output.
///
/// Shell wrappers encode command stdout/stderr between transport markers so the
/// runtime can recover output even when shell echo or prompt repaint surrounds
/// it. Visible terminal rendering should show the decoded command output, not
/// the private transport frame.
pub fn renderable_shell_transaction_bytes(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let decoded = decode_shell_output_transport_with_diagnostics(&text);
    if decoded.diagnostics.saw_begin_marker {
        decoded.output.into_bytes()
    } else {
        bytes.to_vec()
    }
}

/// Sanitizes one transient shell-output status line for pane rendering.
pub fn sanitized_shell_output_status_line(line: &str) -> String {
    line.chars()
        .map(|ch| {
            if ch == '\t' || !ch.is_control() {
                ch
            } else {
                ' '
            }
        })
        .collect()
}

/// Runs the mez wrapper echo text without leading prompts operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn mez_wrapper_echo_text_without_leading_prompts(value: &str) -> String {
    let mut remaining = value;
    loop {
        let trimmed = remaining.trim_start_matches(' ');
        if let Some(next) = trimmed.strip_prefix("$ ") {
            remaining = next;
            continue;
        }
        if let Some(next) = trimmed.strip_prefix("> ") {
            remaining = next;
            continue;
        }
        return trimmed.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that known Mezzanine wrapper echo text is correctly hidden.
    #[test]
    fn wrapper_echo_text_filtering_hides_known_markers() {
        let hidden_lines = [
            "MEZ_MARKER_TOKEN=t1 MEZ_TURN=1 MEZ_AGENT=a1 MEZ_PANE=%1",
            "MEZ_STATUS=0",
            "MEZ_RESTORE_ERREXIT=''",
            "MEZ_RESTORE_NOUNSET=''",
            "MEZ_RESTORE_HISTORY=''",
            "MEZ_HISTORY_STATE=''",
            "MEZ_COMMAND_FILE=/tmp/mez-XXXXXX",
            "MEZ_COMMAND_B64=AAAA",
            "MEZ_OUTPUT_FILE=/tmp/mez-output",
            "MEZ_WRITE_STATUS=0",
            "HISTFILE=/dev/null",
            "set +o history",
            "set -o history",
            "history -d 1",
            "fish_private_mode on",
            "history delete --prefix mez",
            "case $- in *e*)",
            "mez_marker=abc123",
            "printf '\\033]133;C;mez_marker=abc;mez_turn=t1'",
            "printf '\\033]133;D;%s;mez_marker=abc'",
            "printf '%s\\n' __MEZ_SHELL_OUTPUT_BASE64_BEGIN__",
            "printf '\\n%s\\n' __MEZ_SHELL_OUTPUT_BASE64_END__",
            "env -u MEZ_MARKER_TOKEN -u MEZ_TURN -u MEZ_AGENT -u MEZ_PANE",
            "cat > \"$MEZ_COMMAND_FILE\" <<\\MEZ_CMD",
            "else",
            "fi",
            ">",
            "$",
            "begin",
            "end",
            "{",
            "}",
            "if command -v bash",
            "elif command -v zsh",
            "setsid(); exec @ARGV",
            "os.setsid()",
            "</dev/null",
            "unset MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS",
            "set -l MEZ_STATUS $status",
            "set -e MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS",
        ];
        let empty_commands: Vec<String> = Vec::new();
        for line in &hidden_lines {
            assert!(
                mez_wrapper_echo_text_is_hidden(line, &empty_commands),
                "line should be hidden: {line}"
            );
        }
    }

    /// Verifies that legitimate command output is NOT incorrectly hidden.
    #[test]
    fn wrapper_echo_text_filtering_preserves_legitimate_output() {
        let visible_lines = [
            "hello world",
            "total 42",
            "file.txt",
            "error: compilation failed",
            "   Compiling mezzanine v0.1.0",
            "test result: ok. 42 passed; 0 failed",
            "Permission denied",
            "MEZ_ is not a real variable",
        ];
        let empty_commands: Vec<String> = Vec::new();
        for line in &visible_lines {
            assert!(
                !mez_wrapper_echo_text_is_hidden(line, &empty_commands),
                "line should be visible: {line}"
            );
        }
    }

    /// Verifies wrapper-only cleanup and transport lines stay hidden even when
    /// the shell wraps them with prompt fragments.
    #[test]
    fn wrapper_echo_text_filtering_hides_wrapped_cleanup_fragments() {
        let empty_commands: Vec<String> = Vec::new();
        let hidden_lines = [
            "if [ -n \"$MEZ_STTY_STATE\" ]; then stty -echo 2>/dev/null || :; fi",
            "printf>  '\\n%s\\n' '__MEZ_SHELL_OUTPUT_BASE64_BEGIN__'",
            "__mez_t> x_1766e8c197025c5c",
            "done",
        ];
        for line in &hidden_lines {
            let visible = mez_wrapper_echo_line_visible_bytes(line.as_bytes(), &empty_commands);
            assert!(visible.is_empty(), "line should be hidden: {line}");
        }
    }

    /// Verifies raw transport marker lines remain available so the visible
    /// renderer can decode command output instead of showing base64 payloads.
    #[test]
    fn wrapper_echo_text_filtering_preserves_transport_marker_lines_for_decode() {
        let empty_commands: Vec<String> = Vec::new();
        let begin = mez_wrapper_echo_line_visible_bytes(
            b"__MEZ_SHELL_OUTPUT_BASE64_BEGIN__\n",
            &empty_commands,
        );
        let end = mez_wrapper_echo_line_visible_bytes(
            b"__MEZ_SHELL_OUTPUT_BASE64_END__\n",
            &empty_commands,
        );

        assert_eq!(
            String::from_utf8_lossy(&begin),
            "__MEZ_SHELL_OUTPUT_BASE64_BEGIN__\n"
        );
        assert_eq!(
            String::from_utf8_lossy(&end),
            "__MEZ_SHELL_OUTPUT_BASE64_END__\n"
        );
    }

    /// Verifies command echo is only hidden on the first occurrence, not
    /// on subsequent lines that happen to end with the same command text.
    #[test]
    fn agent_shell_transaction_observation_hides_command_echo_only_once() {
        let output = agent_shell_transaction_observation_bytes(
            b"echo hello world\r\nhello world\r\nsome other output\r\necho hello world\r\n",
            "echo hello world",
        );
        let text = String::from_utf8_lossy(&output);
        // The first occurrence (command echo) should be hidden.
        // The second occurrence is legitimate output that should remain.
        assert!(text.contains("hello world"), "output should remain: {text}");
        assert!(
            text.contains("some other output"),
            "all output should remain: {text}"
        );
        assert!(
            text.contains("echo hello world"),
            "second echo hello world is legitimate output: {text}"
        );
        // Count occurrences: should be exactly one "echo hello world".
        assert_eq!(
            text.match_indices("echo hello world").count(),
            1,
            "exactly one echo hello world should remain: {text}"
        );
    }

    /// Verifies hidden-live shell output previews use decoded command output as
    /// the authoritative source. Wrapper framing and prompt repaint bytes after
    /// the transport frame must remain out of pane status lines.
    #[test]
    fn latest_agent_shell_transaction_output_lines_ignores_transport_framing_tail() {
        let output = "__MEZ_SHELL_OUTPUT_BASE64_BEGIN__\nQVNZTkNfUEFORV9TVElMTF9BTElWRQo=\n__MEZ_SHELL_OUTPUT_BASE64_END__\nMEZ_MARKER_TOKEN='abc'\n$ prompt repaint\n";
        let lines = latest_agent_shell_transaction_output_lines(output, 5);

        assert_eq!(lines, vec!["ASYNC_PANE_STILL_ALIVE".to_string()]);
    }

    /// Verifies prompt detection correctly identifies common prompt patterns.
    #[test]
    fn prompt_detection_identifies_common_prompts() {
        assert!(shell_observation_line_looks_like_prompt("~ $ "));
        assert!(shell_observation_line_looks_like_prompt("/tmp $ "));
        assert!(shell_observation_line_looks_like_prompt("user@host:~ $ "));
        assert!(shell_observation_line_looks_like_prompt(
            "~/projects:main $ "
        ));
        assert!(shell_observation_line_looks_like_prompt("$"));
        assert!(shell_observation_line_looks_like_prompt(">"));
        assert!(shell_observation_line_looks_like_prompt("#"));
    }

    /// Verifies that legitimate command output is not mistaken for a prompt.
    #[test]
    fn prompt_detection_does_not_match_legitimate_output() {
        assert!(!shell_observation_line_looks_like_prompt("hello world"));
        assert!(!shell_observation_line_looks_like_prompt("total 42"));
        assert!(!shell_observation_line_looks_like_prompt(
            "compilation successful"
        ));
        assert!(!shell_observation_line_looks_like_prompt("   $12.99   "));
        assert!(!shell_observation_line_looks_like_prompt(
            "path/to/repo/file.rs"
        ));
        assert!(!shell_observation_line_looks_like_prompt(""));
    }
}
