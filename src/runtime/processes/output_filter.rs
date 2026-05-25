//! Pane PTY output filtering and shell-observation helpers.
//!
//! This module owns byte-level filtering for Mezzanine shell wrapper echoes,
//! bounded OSC transaction marker scanning, and model-facing shell observation
//! cleanup. The process facade keeps pane lifecycle state while this module
//! keeps terminal byte parsing isolated and testable.

use super::*;

/// Carries Pane Output Render Mode state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneOutputRenderMode {
    /// Represents the Normal case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Normal,
    /// Represents the Hidden Live Agent Shell case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HiddenLiveAgentShell,
    /// Represents the Hidden Retained Agent Shell case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HiddenRetainedAgentShell,
    /// Represents the Verbose Agent Action case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    VerboseAgentAction,
    /// Represents the Trace case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Trace,
}

/// Runs the mez wrapper echo line is hidden operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mez_wrapper_echo_line_is_hidden(line: &[u8], command_lines: &[String]) -> bool {
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
pub(super) fn mez_wrapper_echo_line_visible_bytes(
    line: &[u8],
    command_lines: &[String],
) -> Vec<u8> {
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
pub(super) fn append_visible_mez_wrapper_text_segment(
    visible: &mut Vec<u8>,
    segment: &[u8],
    command_lines: &[String],
) {
    if segment.is_empty() || mez_wrapper_echo_line_is_hidden(segment, command_lines) {
        return;
    }
    let text = String::from_utf8_lossy(segment);
    visible.extend_from_slice(mez_wrapper_echo_text_without_leading_prompts(&text).as_bytes());
}

/// Runs the terminal escape sequence end operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn terminal_escape_sequence_end(bytes: &[u8], escape_index: usize) -> usize {
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
            bytes.len()
        }
        b'[' => {
            let mut index = escape_index + 2;
            while index < bytes.len() {
                if (0x40..=0x7e).contains(&bytes[index]) {
                    return index + 1;
                }
                index += 1;
            }
            bytes.len()
        }
        _ => (escape_index + 2).min(bytes.len()),
    }
}

/// Runs the mez wrapper echo line is possible prefix operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mez_wrapper_echo_line_is_possible_prefix(
    line: &[u8],
    command_lines: &[String],
) -> bool {
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
pub(super) fn mez_wrapper_filter_bytes_may_contain_boilerplate(bytes: &[u8]) -> bool {
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

/// Runs the mez wrapper echo text is hidden operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mez_wrapper_echo_text_is_hidden(normalized: &str, command_lines: &[String]) -> bool {
    let promptless = mez_wrapper_echo_text_without_inline_prompts(normalized);
    if normalized.starts_with("if [ \"$M")
        || promptless.starts_with("if [ \"$M")
        || normalized.contains("MEZ_MARKER_TOKEN")
        || promptless.contains("MEZ_MARKER_TOKEN")
        || normalized.contains("MEZ_TURN")
        || promptless.contains("MEZ_TURN")
        || normalized.contains("MEZ_AGENT")
        || promptless.contains("MEZ_AGENT")
        || normalized.contains("MEZ_PANE")
        || promptless.contains("MEZ_PANE")
        || normalized.contains("MEZ_STATUS")
        || promptless.contains("MEZ_STATUS")
        || normalized.contains("MEZ_RESTORE_ERREXIT")
        || promptless.contains("MEZ_RESTORE_ERREXIT")
        || normalized.contains("MEZ_RESTORE_NOUNSET")
        || promptless.contains("MEZ_RESTORE_NOUNSET")
        || normalized.contains("MEZ_RESTORE_HISTORY")
        || promptless.contains("MEZ_RESTORE_HISTORY")
        || normalized.contains("MEZ_HISTORY_")
        || promptless.contains("MEZ_HISTORY_")
        || normalized.contains("MEZ_COMMAND_FILE")
        || promptless.contains("MEZ_COMMAND_FILE")
        || normalized.contains("MEZ_COMMAND_B64")
        || promptless.contains("MEZ_COMMAND_B64")
        || normalized.contains("MEZ_OUTPUT_FILE")
        || promptless.contains("MEZ_OUTPUT_FILE")
        || normalized.contains("MEZ_WRITE_STATUS")
        || promptless.contains("MEZ_WRITE_STATUS")
        || normalized.contains("HISTFILE=/dev/null")
        || promptless.contains("HISTFILE=/dev/null")
        || normalized.contains("set +o history")
        || promptless.contains("set +o history")
        || normalized.contains("set -o history")
        || promptless.contains("set -o history")
        || normalized.contains("history -d")
        || promptless.contains("history -d")
        || normalized.contains("fish_private_mode")
        || promptless.contains("fish_private_mode")
        || normalized.contains("history delete --")
        || promptless.contains("history delete --")
        || normalized.contains("MEZ_COMMAND_")
        || promptless.contains("MEZ_COMMAND_")
        || normalized.contains("case $- in *e*)")
        || promptless.contains("case $- in *e*)")
        || normalized.contains("mez_marker=")
        || promptless.contains("mez_marker=")
        || normalized.contains("printf '\\033]133;C;mez_marker")
        || promptless.contains("printf '\\033]133;C;mez_marker")
        || normalized.contains("printf '\\033]133;D;%s;mez_marker")
        || promptless.contains("printf '\\033]133;D;%s;mez_marker")
        || ((normalized.contains("printf '%s\\n'") || normalized.contains("printf '\\n%s\\n'"))
            && normalized.contains("__MEZ_SHELL_OUTPUT_BASE64_"))
        || ((promptless.contains("printf '%s\\n'") || promptless.contains("printf '\\n%s\\n'"))
            && promptless.contains("__MEZ_SHELL_OUTPUT_BASE64_"))
        || normalized.contains("env -u MEZ_MARKER_TOKEN -u MEZ_TURN -u MEZ_AGENT -u MEZ_PANE")
        || promptless.contains("env -u MEZ_MARKER_TOKEN -u MEZ_TURN -u MEZ_AGENT -u MEZ_PANE")
        || normalized.contains("cat > \"$MEZ_COMMAND_FILE\"")
        || promptless.contains("cat > \"$MEZ_COMMAND_FILE\"")
        || matches!(normalized, "else" | "fi")
        || matches!(promptless.as_str(), "else" | "fi")
        || normalized.starts_with("if command -v")
        || promptless.starts_with("if command -v")
        || normalized.starts_with("elif command -v")
        || promptless.starts_with("elif command -v")
        || normalized.contains("setsid(); exec @ARGV")
        || promptless.contains("setsid(); exec @ARGV")
        || normalized.contains("os.setsid()")
        || promptless.contains("os.setsid()")
        || normalized.contains("</dev/null")
        || promptless.contains("</dev/null")
        || normalized.contains("unset MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS")
        || promptless.contains("unset MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS")
        || normalized.contains("set -l MEZ_STATUS $status")
        || promptless.contains("set -l MEZ_STATUS $status")
        || normalized.contains("set -e MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS")
        || promptless.contains("set -e MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS")
        || normalized == ">"
        || normalized == "$"
        || promptless == ">"
        || promptless == "$"
        || normalized == "begin"
        || normalized == "end"
        || normalized == "{"
        || normalized == "}"
        || (normalized.starts_with("command ") && normalized.contains(" -c "))
        || normalized.starts_with("eval ")
    {
        return true;
    }
    if normalized
        .split_whitespace()
        .all(|token| matches!(token, "$" | ">" | "#"))
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
pub(super) fn mez_wrapper_echo_text_ends_with_command(normalized: &str, command: &str) -> bool {
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
pub(super) fn mez_wrapper_echo_text_without_inline_prompts(value: &str) -> String {
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
pub(super) fn shell_observation_without_terminal_controls(bytes: &[u8]) -> Vec<u8> {
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
pub(super) fn shell_observation_line_looks_like_prompt(line: &str) -> bool {
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
pub(super) fn shell_observation_line_has_common_prompt_suffix(trimmed: &str) -> bool {
    let Some((prefix, suffix)) = trimmed.rsplit_once(' ') else {
        return false;
    };
    matches!(suffix, "$" | ">" | "#")
        && (prefix.starts_with('~')
            || prefix.starts_with('/')
            || prefix.contains('@')
            || prefix.contains(':')
            || prefix.contains("repo"))
}

/// Produces model-visible command output for an agent shell transaction.
///
/// User-facing rendering keeps a richer stream so the terminal can update
/// state, but the model only needs command stdout/stderr. This removes
/// Mezzanine wrapper echo, shell prompt repaint, and terminal styling while
/// preserving the actual command output that should feed follow-up reasoning.
pub(super) fn agent_shell_transaction_observation_bytes(bytes: &[u8], command: &str) -> Vec<u8> {
    let stripped = shell_observation_without_terminal_controls(bytes);
    let text = String::from_utf8_lossy(&stripped);
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let command_lines = vec![command.to_string()];
    let mut output = String::new();
    for line in normalized.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !output.ends_with('\n') && !output.is_empty() {
                output.push('\n');
            }
            continue;
        }
        if mez_wrapper_echo_text_is_hidden(trimmed, &command_lines) {
            continue;
        }
        let cleaned = mez_wrapper_echo_text_without_leading_prompts(line);
        if cleaned.trim().is_empty() || shell_observation_line_looks_like_prompt(&cleaned) {
            continue;
        }
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
pub(super) fn agent_shell_transaction_bytes_before_end_marker<'a>(
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
pub(super) fn find_byte_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Scans bytes for bounded Mezzanine-owned OSC 133 transaction events.
///
/// # Parameters
/// - `bytes`: The hidden agent-shell bytes plus any retained fragment from the
///   previous PTY read.
pub(super) fn scan_mezzanine_osc_transaction_events(
    bytes: &[u8],
) -> (Vec<TerminalOscEvent>, Vec<u8>) {
    let mut events = Vec::new();
    let mut cursor = 0usize;
    let mut retained_start = None;
    while let Some(relative_start) = find_byte_subsequence(&bytes[cursor..], RUNTIME_MEZ_OSC_PREFIX)
    {
        let osc_start = cursor + relative_start;
        let payload_start = osc_start + 2;
        match find_bounded_osc_terminator(bytes, payload_start) {
            Some((payload_end, terminator_end)) => {
                if let Ok(payload) = std::str::from_utf8(&bytes[payload_start..payload_end])
                    && let Some(event) = parse_mez_shell_transaction_osc(payload)
                {
                    events.push(event);
                }
                cursor = terminator_end;
            }
            None => {
                if bytes.len().saturating_sub(payload_start) >= RUNTIME_MEZ_OSC_SCAN_LIMIT_BYTES {
                    cursor = osc_start.saturating_add(1);
                } else {
                    retained_start = Some(osc_start);
                    break;
                }
            }
        }
    }
    let retained = if let Some(start) = retained_start {
        bounded_osc_pending_fragment(&bytes[start..])
    } else {
        trailing_mez_osc_prefix_fragment(bytes)
    };
    (events, retained)
}

/// Finds an OSC string terminator within the bounded Mezzanine marker window.
///
/// # Parameters
/// - `bytes`: The byte slice being scanned.
/// - `payload_start`: The byte offset immediately after `ESC ]`.
pub(super) fn find_bounded_osc_terminator(
    bytes: &[u8],
    payload_start: usize,
) -> Option<(usize, usize)> {
    let search_end = bytes
        .len()
        .min(payload_start.saturating_add(RUNTIME_MEZ_OSC_SCAN_LIMIT_BYTES));
    let mut index = payload_start;
    while index < search_end {
        match bytes[index] {
            0x07 => return Some((index, index + 1)),
            0x1b if bytes.get(index + 1) == Some(&b'\\') => return Some((index, index + 2)),
            _ => index += 1,
        }
    }
    None
}

/// Bounds one retained OSC parser fragment to the maximum marker window.
///
/// # Parameters
/// - `fragment`: The potential partial OSC marker fragment to retain.
pub(super) fn bounded_osc_pending_fragment(fragment: &[u8]) -> Vec<u8> {
    if fragment.len() <= RUNTIME_MEZ_OSC_SCAN_LIMIT_BYTES {
        fragment.to_vec()
    } else {
        fragment[fragment.len() - RUNTIME_MEZ_OSC_SCAN_LIMIT_BYTES..].to_vec()
    }
}

/// Returns a trailing byte prefix that could start a future Mezzanine marker.
///
/// # Parameters
/// - `bytes`: The complete scanned byte slice.
pub(super) fn trailing_mez_osc_prefix_fragment(bytes: &[u8]) -> Vec<u8> {
    let max_len = bytes
        .len()
        .min(RUNTIME_MEZ_OSC_PREFIX.len().saturating_sub(1));
    for len in (1..=max_len).rev() {
        if bytes[bytes.len() - len..] == RUNTIME_MEZ_OSC_PREFIX[..len] {
            return bytes[bytes.len() - len..].to_vec();
        }
    }
    Vec::new()
}

/// Returns the latest non-empty model-visible shell output line.
pub(super) fn latest_agent_shell_transaction_output_line(output: &str) -> Option<String> {
    decode_shell_output_transport(output)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .rev()
        .map(sanitized_shell_output_status_line)
        .map(|line| line.trim_end().to_string())
        .find(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !shell_observation_line_looks_like_prompt(trimmed)
        })
}

/// Sanitizes one transient shell-output status line for pane rendering.
pub(super) fn sanitized_shell_output_status_line(line: &str) -> String {
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
pub(super) fn mez_wrapper_echo_text_without_leading_prompts(value: &str) -> String {
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
