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
pub(super) fn mez_wrapper_echo_text_is_hidden(normalized: &str, command_lines: &[String]) -> bool {
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
pub(super) fn agent_shell_transaction_observation_bytes(bytes: &[u8], command: &str) -> Vec<u8> {
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

/// Returns the latest non-empty model-visible shell output lines.
pub(super) fn latest_agent_shell_transaction_output_lines(
    output: &str,
    max_lines: usize,
) -> Vec<String> {
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
pub(super) fn renderable_shell_transaction_bytes(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let decoded = decode_shell_output_transport_with_diagnostics(&text);
    if decoded.diagnostics.saw_begin_marker {
        decoded.output.into_bytes()
    } else {
        bytes.to_vec()
    }
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

/// Defines the RUNTIME FOREGROUND TITLE IDLE SYNC POLL INTERVAL const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const RUNTIME_FOREGROUND_TITLE_IDLE_SYNC_POLL_INTERVAL: usize = 16;
impl RuntimeSessionService {
    /// Runs the apply pane process output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_pane_process_output(
        &mut self,
        output: PaneProcessOutput,
        terminal_title_panes: &mut BTreeSet<String>,
    ) -> Result<PaneOutputUpdate> {
        let descriptor = self.find_pane_descriptor(&output.pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane output has no matching pane",
            )
        })?;
        let descriptor_size = descriptor.size;
        let descriptor_window_id = descriptor.window_id.to_string();
        let background = self
            .session
            .active_window()
            .is_none_or(|window| window.active_pane().id.as_str() != descriptor.pane_id.as_str());
        let transaction_bytes =
            self.visible_pane_output_bytes(output.pane_id.as_str(), &output.bytes);
        let render_bytes =
            self.renderable_pane_output_bytes(output.pane_id.as_str(), &transaction_bytes);
        let (osc_events, transaction_alternate_active) = self.terminal_osc_events_for_pane_bytes(
            output.pane_id.as_str(),
            descriptor_size,
            &transaction_bytes,
        )?;
        let (title, activity_events, bell_events, render_alternate_active) = {
            let screen = self.pane_screens.entry(output.pane_id.clone()).or_insert(
                TerminalScreen::new_with_history_config(
                    descriptor_size,
                    self.terminal_history_limit,
                    self.terminal_history_rotate_lines,
                )?,
            );
            let previous_activity_events = screen.activity_events();
            let previous_bell_events = screen.bell_events();
            screen.feed(&render_bytes);
            let _ = screen.drain_osc_events();
            (
                screen.title().map(ToOwned::to_owned),
                screen
                    .activity_events()
                    .saturating_sub(previous_activity_events),
                screen.bell_events().saturating_sub(previous_bell_events),
                screen.alternate_screen_active(),
            )
        };
        let alternate_active = transaction_alternate_active || render_alternate_active;
        let terminal_title = osc_events.iter().rev().find_map(|event| match event {
            TerminalOscEvent::TitleChanged { title } => Some(title.clone()),
            _ => None,
        });
        if terminal_title.is_some() {
            terminal_title_panes.insert(output.pane_id.clone());
        }
        self.apply_terminal_osc_events(&osc_events)?;
        if alternate_active {
            self.pane_readiness_overrides.revoke(
                output.pane_id.as_str(),
                ReadinessOverrideRevocation::AlternateScreenEntry,
            );
            self.set_pane_readiness(
                output.pane_id.as_str(),
                PaneReadinessState::InteractiveBlocked,
            );
        }
        self.record_running_shell_transaction_output(output.pane_id.as_str(), &transaction_bytes);
        self.observe_agent_shell_transaction_events(output.pane_id.as_str(), &osc_events)?;
        self.write_active_pane_pipe(output.pane_id.as_str(), &render_bytes)?;
        let title_changed = if let Some(title) = terminal_title.or(title) {
            self.session
                .set_pane_title_from_terminal(output.pane_id.as_str(), title)?
        } else {
            false
        };

        let update = PaneOutputUpdate {
            session_id: self.session.id.to_string(),
            window_id: descriptor_window_id,
            pane_id: output.pane_id,
            primary_pid: output.primary_pid,
            bytes_read: output.bytes.len(),
            activity_events,
            bell_events,
            background,
        };
        self.append_pane_output_event(&update)?;
        if title_changed {
            self.append_pane_title_event(&update)?;
        }
        Ok(update)
    }

    /// Returns pane bytes that should be retained for active Mezzanine-owned
    /// shell transactions after filtering wrapper echo that is irrelevant to the
    /// model and the runtime state machine.
    ///
    /// Interactive shells echo the wrapper lines that Mezzanine writes around
    /// agent actions, readiness probes, and bootstrap probes. Those lines are
    /// implementation traffic, not user commands, so normal transaction
    /// observation hides them while preserving command output and the OSC
    /// transaction markers that drive the runtime state machine. Trace logging
    /// disables this filter for diagnosis.
    pub(in crate::runtime) fn visible_pane_output_bytes(
        &mut self,
        pane_id: &str,
        bytes: &[u8],
    ) -> Vec<u8> {
        if bytes.is_empty() {
            return Vec::new();
        }
        let active_transaction = self
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.pane_id == pane_id);
        let filter_commands = self.mez_wrapper_filter_commands_for_pane(pane_id);
        if self.agent_trace_enabled(pane_id)
            || (filter_commands.is_empty()
                && !mez_wrapper_filter_bytes_may_contain_boilerplate(bytes))
        {
            let mut visible = self
                .pane_mez_wrapper_filter_pending
                .remove(pane_id)
                .unwrap_or_default();
            visible.extend_from_slice(bytes);
            if !active_transaction {
                self.tick_mez_wrapper_filter_retention(pane_id);
            }
            return visible;
        }

        let mut pending = self
            .pane_mez_wrapper_filter_pending
            .remove(pane_id)
            .unwrap_or_default();
        pending.extend_from_slice(bytes);
        let mut visible = Vec::with_capacity(pending.len());
        let mut filtered_wrapper_echo = false;
        let mut line_start = 0usize;
        while let Some(relative_terminator) = pending[line_start..]
            .iter()
            .position(|byte| *byte == b'\n' || *byte == b'\r')
        {
            let terminator = line_start + relative_terminator;
            let line_end = if pending[terminator] == b'\r'
                && pending
                    .get(terminator + 1)
                    .is_some_and(|byte| *byte == b'\n')
            {
                terminator + 2
            } else {
                terminator + 1
            };
            let line = &pending[line_start..line_end];
            let filtered_line = mez_wrapper_echo_line_visible_bytes(line, &filter_commands);
            if filtered_line.len() != line.len() {
                filtered_wrapper_echo = true;
            }
            visible.extend_from_slice(&filtered_line);
            line_start = line_end;
        }

        if line_start < pending.len() {
            let tail = &pending[line_start..];
            if tail.contains(&0x1b) {
                let filtered_tail = mez_wrapper_echo_line_visible_bytes(tail, &filter_commands);
                if filtered_tail.len() != tail.len() {
                    filtered_wrapper_echo = true;
                }
                visible.extend_from_slice(&filtered_tail);
            } else if mez_wrapper_echo_line_is_hidden(tail, &filter_commands) {
                filtered_wrapper_echo = true;
            } else if !mez_wrapper_echo_line_is_possible_prefix(tail, &filter_commands) {
                visible.extend_from_slice(tail);
            } else {
                filtered_wrapper_echo = true;
                self.pane_mez_wrapper_filter_pending
                    .insert(pane_id.to_string(), tail.to_vec());
            }
        }
        if !active_transaction {
            if filtered_wrapper_echo {
                self.pane_mez_wrapper_filter_recent_polls.insert(
                    pane_id.to_string(),
                    RUNTIME_SHELL_WRAPPER_FILTER_RETENTION_POLLS,
                );
            } else {
                self.tick_mez_wrapper_filter_retention(pane_id);
            }
        }
        visible
    }

    /// Runs the pane output render mode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pane_output_render_mode(&self, pane_id: &str) -> PaneOutputRenderMode {
        if self.agent_trace_enabled(pane_id) {
            return PaneOutputRenderMode::Trace;
        }
        let shell_view_enabled = self.agent_shell_view_enabled(pane_id);
        let mut has_agent_action = false;
        for transaction in self
            .running_shell_transactions
            .values()
            .filter(|transaction| transaction.pane_id == pane_id)
        {
            match &transaction.kind {
                RunningShellTransactionKind::AgentAction { .. } => {
                    has_agent_action = true;
                }
                RunningShellTransactionKind::ReadinessProbe
                | RunningShellTransactionKind::Bootstrap => {
                    return PaneOutputRenderMode::HiddenLiveAgentShell;
                }
            }
        }
        if has_agent_action {
            if shell_view_enabled {
                PaneOutputRenderMode::VerboseAgentAction
            } else {
                PaneOutputRenderMode::HiddenLiveAgentShell
            }
        } else if !shell_view_enabled
            && (self.pane_has_running_agent_turn(pane_id)
                || self.pane_agent_subshell_active(pane_id))
        {
            PaneOutputRenderMode::HiddenLiveAgentShell
        } else if !shell_view_enabled
            && self
                .pane_hidden_shell_render_recent_polls
                .contains_key(pane_id)
        {
            PaneOutputRenderMode::HiddenRetainedAgentShell
        } else {
            PaneOutputRenderMode::Normal
        }
    }

    /// Runs the renderable pane output bytes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn renderable_pane_output_bytes(
        &mut self,
        pane_id: &str,
        transaction_bytes: &[u8],
    ) -> Vec<u8> {
        match self.pane_output_render_mode(pane_id) {
            PaneOutputRenderMode::Normal
            | PaneOutputRenderMode::VerboseAgentAction
            | PaneOutputRenderMode::Trace => renderable_shell_transaction_bytes(transaction_bytes),
            PaneOutputRenderMode::HiddenLiveAgentShell => {
                if !transaction_bytes.is_empty() {
                    self.remember_hidden_shell_render_suppression(pane_id);
                }
                Vec::new()
            }
            PaneOutputRenderMode::HiddenRetainedAgentShell => Vec::new(),
        }
    }

    /// Reports whether the pane has a runtime agent turn currently occupying
    /// the pane's agent shell session.
    fn pane_has_running_agent_turn(&self, pane_id: &str) -> bool {
        self.agent_shell_store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            .is_some()
    }

    /// Reports whether a pane currently owns a child shell for agent mode.
    ///
    /// The child shell's prompt and setup repaint are implementation traffic
    /// unless shell-view diagnostics are enabled.
    fn pane_agent_subshell_active(&self, pane_id: &str) -> bool {
        self.agent_subshell_panes.contains(pane_id)
    }

    /// Retains short-lived shell-output suppression after a hidden agent shell
    /// transaction so delayed prompt repaint bytes do not leak into the pane.
    pub(in crate::runtime) fn remember_hidden_shell_render_suppression(&mut self, pane_id: &str) {
        self.pane_hidden_shell_render_recent_polls.insert(
            pane_id.to_string(),
            RUNTIME_HIDDEN_SHELL_RENDER_RETENTION_POLLS,
        );
    }

    /// Clears retained shell-output filters for explicit foreground input.
    ///
    /// Hidden-shell and wrapper-echo retention suppress delayed implementation
    /// prompt repaint bytes after agent-owned shell work. Once foreground
    /// control returns to the pane, following PTY output belongs to the user's
    /// interaction and must not be swallowed or reduced to cursor-control
    /// remnants by the previous agent turn's cleanup window.
    pub(in crate::runtime) fn clear_shell_output_filters_for_foreground_input(
        &mut self,
        pane_id: &str,
    ) {
        self.pane_hidden_shell_render_recent_polls.remove(pane_id);
        self.pane_mez_wrapper_filter_pending.remove(pane_id);
        self.pane_mez_wrapper_filter_recent_commands.remove(pane_id);
        self.pane_mez_wrapper_filter_recent_polls.remove(pane_id);
    }

    /// Ages out retained shell-output suppression for panes whose agent turn and
    /// Mezzanine-owned shell transaction have both settled.
    pub(in crate::runtime) fn tick_hidden_shell_render_retention(&mut self) -> usize {
        let mut aged = 0usize;
        let retained = self
            .pane_hidden_shell_render_recent_polls
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for pane_id in retained {
            if self.pane_has_running_agent_turn(&pane_id)
                || self
                    .running_shell_transactions
                    .values()
                    .any(|transaction| transaction.pane_id == pane_id)
            {
                continue;
            }
            let Some(remaining) = self.pane_hidden_shell_render_recent_polls.get_mut(&pane_id)
            else {
                continue;
            };
            *remaining = remaining.saturating_sub(1);
            aged = aged.saturating_add(1);
            if *remaining == 0 {
                self.pane_hidden_shell_render_recent_polls.remove(&pane_id);
            }
        }
        aged
    }

    /// Applies runtime idle-cleanup timer work that does not require polling PTY
    /// or process state.
    ///
    /// Migrated cleanup targets include hidden-shell render suppression
    /// retention, stranded agent dispatch recovery, and unreachable running
    /// turn failure. These operations
    /// are driven by actor timer events in the daemon path so idle sessions do
    /// not need to scan them through the compatibility tick.
    pub fn apply_idle_cleanup_timer_event(&mut self) -> Result<usize> {
        self.apply_idle_cleanup_timer_event_with_actor_progress(&BTreeSet::new())
    }

    /// Applies runtime idle-cleanup timer work while honoring actor-owned
    /// progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns with progress represented by
    ///   async actor state rather than service-owned queues.
    pub fn apply_idle_cleanup_timer_event_with_actor_progress(
        &mut self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> Result<usize> {
        match self.lifecycle_state {
            RuntimeLifecycleState::Killed | RuntimeLifecycleState::Failed => Ok(0),
            RuntimeLifecycleState::Running
            | RuntimeLifecycleState::Detached
            | RuntimeLifecycleState::Stopping => {
                let hidden_shell_retention_aged = self.tick_hidden_shell_render_retention();
                let reconciled = self.reconcile_agent_runtime_progress_paths_with_actor_progress(
                    actor_progress_turn_ids,
                )?;
                Ok(hidden_shell_retention_aged.saturating_add(reconciled))
            }
        }
    }

    /// Reconciles running agent turns after actor-owned state transitions.
    ///
    /// The runtime specification requires every running turn to retain an
    /// observable progress path. Calling this after event application prevents
    /// stranded turns from depending on a later idle timer before they are
    /// requeued or failed.
    pub fn reconcile_agent_runtime_progress_paths(&mut self) -> Result<usize> {
        self.reconcile_agent_runtime_progress_paths_with_actor_progress(&BTreeSet::new())
    }

    /// Reconciles running agent turns while honoring actor-owned progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Turns waiting on progress owned by the
    ///   async actor, such as provider retry timers that are not represented in
    ///   service-owned queues.
    pub fn reconcile_agent_runtime_progress_paths_with_actor_progress(
        &mut self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> Result<usize> {
        if matches!(
            self.lifecycle_state,
            RuntimeLifecycleState::Killed | RuntimeLifecycleState::Failed
        ) {
            return Ok(0);
        }
        let stranded_shell_recoveries = self.recover_stranded_agent_shell_dispatches()?;
        let unreachable_turn_failures =
            self.fail_unreachable_running_agent_turns_with_actor_progress(actor_progress_turn_ids)?;
        Ok(stranded_shell_recoveries.saturating_add(unreachable_turn_failures))
    }

    /// Reports whether actor-owned idle cleanup should remain scheduled.
    ///
    /// Hidden agent-shell render suppression retention must age out after the
    /// shell transaction and running turn settle so delayed prompt bytes do not
    /// leak into normal pane rendering. Stranded shell-dispatch recovery must
    /// also retry while a pending shell command is blocked behind stale
    /// readiness state.
    pub fn idle_cleanup_timer_needed(&self) -> bool {
        self.idle_cleanup_timer_needed_with_actor_progress(&BTreeSet::new())
    }

    /// Reports whether actor-owned idle cleanup should remain scheduled while
    /// honoring actor-owned progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns with progress represented by
    ///   async actor state rather than service-owned queues.
    pub fn idle_cleanup_timer_needed_with_actor_progress(
        &self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> bool {
        self.hidden_shell_render_retention_timer_needed()
            || self.stranded_agent_shell_dispatch_recovery_timer_needed()
            || self.unreachable_running_agent_turn_timer_needed_with_actor_progress(
                actor_progress_turn_ids,
            )
    }

    /// Reports whether hidden shell-render suppression still needs to age out.
    pub fn hidden_shell_render_retention_timer_needed(&self) -> bool {
        !self.pane_hidden_shell_render_recent_polls.is_empty()
    }

    /// Reports whether any pending agent shell dispatch may need recovery.
    pub fn stranded_agent_shell_dispatch_recovery_timer_needed(&self) -> bool {
        !self
            .stranded_agent_shell_dispatch_recovery_candidates()
            .is_empty()
    }

    /// Reports whether any running turn has no remaining runtime progress path.
    pub fn unreachable_running_agent_turn_timer_needed(&self) -> bool {
        self.unreachable_running_agent_turn_timer_needed_with_actor_progress(&BTreeSet::new())
    }

    /// Reports whether any running turn has no remaining runtime progress path
    /// after accounting for async actor-owned progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns with progress represented by
    ///   async actor state rather than service-owned queues.
    pub fn unreachable_running_agent_turn_timer_needed_with_actor_progress(
        &self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> bool {
        !self
            .unreachable_running_agent_turn_candidates(actor_progress_turn_ids)
            .is_empty()
    }

    /// Runs the terminal osc events for pane bytes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn terminal_osc_events_for_pane_bytes(
        &mut self,
        pane_id: &str,
        size: Size,
        bytes: &[u8],
    ) -> Result<(Vec<TerminalOscEvent>, bool)> {
        if bytes.is_empty() {
            return Ok((Vec::new(), false));
        }
        if matches!(
            self.pane_output_render_mode(pane_id),
            PaneOutputRenderMode::HiddenLiveAgentShell
                | PaneOutputRenderMode::HiddenRetainedAgentShell
        ) {
            return Ok((
                self.hidden_agent_shell_osc_events_for_pane_bytes(pane_id, bytes),
                false,
            ));
        }
        let screen = if let Some(screen) = self.pane_transaction_osc_screens.get_mut(pane_id) {
            screen.resize(size);
            screen
        } else {
            self.pane_transaction_osc_screens.insert(
                pane_id.to_string(),
                TerminalScreen::new_with_history_config(
                    size,
                    self.terminal_history_limit,
                    self.terminal_history_rotate_lines,
                )?,
            );
            self.pane_transaction_osc_screens
                .get_mut(pane_id)
                .ok_or_else(|| {
                    MezError::invalid_state("transaction OSC parser was not retained for pane")
                })?
        };
        screen.feed(bytes);
        Ok((screen.drain_osc_events(), screen.alternate_screen_active()))
    }

    /// Scans hidden agent-shell bytes for Mezzanine-owned OSC transaction
    /// markers without feeding arbitrary command output into a terminal parser.
    ///
    /// Hidden agent-shell output is command data for the model. Treating long
    /// file bodies or embedded escape sequences as terminal traffic can
    /// monopolize the runtime actor and mutate parser state. This scanner keeps
    /// only a bounded fragment that may contain a split `ESC ] 133` marker and
    /// ignores all other hidden bytes.
    fn hidden_agent_shell_osc_events_for_pane_bytes(
        &mut self,
        pane_id: &str,
        bytes: &[u8],
    ) -> Vec<TerminalOscEvent> {
        let mut pending = self
            .pane_transaction_osc_pending
            .remove(pane_id)
            .unwrap_or_default();
        pending.extend_from_slice(bytes);
        let (events, retained) = scan_mezzanine_osc_transaction_events(&pending);
        if retained.is_empty() {
            self.pane_transaction_osc_pending.remove(pane_id);
        } else {
            self.pane_transaction_osc_pending
                .insert(pane_id.to_string(), retained);
        }
        events
    }

    /// Runs the remember mez wrapper filter command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn remember_mez_wrapper_filter_command(
        &mut self,
        pane_id: &str,
        command: &str,
    ) {
        let retained = self
            .pane_mez_wrapper_filter_recent_commands
            .entry(pane_id.to_string())
            .or_default();
        for line in command
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            if !retained.iter().any(|existing| existing == line) {
                retained.push(line.to_string());
            }
        }
        let extra = retained
            .len()
            .saturating_sub(RUNTIME_SHELL_WRAPPER_FILTER_RECENT_COMMAND_LIMIT);
        if extra > 0 {
            retained.drain(0..extra);
        }
        self.pane_mez_wrapper_filter_recent_polls.insert(
            pane_id.to_string(),
            RUNTIME_SHELL_WRAPPER_FILTER_RETENTION_POLLS,
        );
    }

    /// Runs the mez wrapper filter commands for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn mez_wrapper_filter_commands_for_pane(&self, pane_id: &str) -> Vec<String> {
        let mut commands = self
            .running_shell_transactions
            .values()
            .filter(|transaction| transaction.pane_id == pane_id)
            .flat_map(|transaction| {
                transaction
                    .command
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        if let Some(retained) = self.pane_mez_wrapper_filter_recent_commands.get(pane_id) {
            for command in retained {
                if !commands.iter().any(|existing| existing == command) {
                    commands.push(command.clone());
                }
            }
        }
        commands
    }

    /// Runs the tick mez wrapper filter retention operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn tick_mez_wrapper_filter_retention(&mut self, pane_id: &str) {
        let Some(remaining) = self.pane_mez_wrapper_filter_recent_polls.get_mut(pane_id) else {
            return;
        };
        *remaining = remaining.saturating_sub(1);
        if *remaining == 0 {
            self.pane_mez_wrapper_filter_recent_polls.remove(pane_id);
            self.pane_mez_wrapper_filter_recent_commands.remove(pane_id);
            self.pane_mez_wrapper_filter_pending.remove(pane_id);
        }
    }

    /// Runs the sync pane titles from foreground processes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn sync_pane_titles_from_foreground_processes(
        &mut self,
        skipped_panes: &BTreeSet<String>,
    ) -> Result<usize> {
        let mut changed = 0usize;
        for pane_id in self.pane_processes.tracked_pane_ids() {
            if skipped_panes.contains(&pane_id) {
                continue;
            }
            let Some(title) = self.foreground_process_pane_title(&pane_id) else {
                continue;
            };
            if !self
                .session
                .set_pane_title_from_terminal(pane_id.as_str(), title)?
            {
                continue;
            }
            let Some(update) = self.pane_title_only_update(&pane_id) else {
                continue;
            };
            self.append_pane_title_event(&update)?;
            changed = changed.saturating_add(1);
        }
        Ok(changed)
    }

    /// Runs the should sync pane titles from foreground processes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn should_sync_pane_titles_from_foreground_processes(
        &mut self,
        observed_output: bool,
    ) -> bool {
        if observed_output {
            self.foreground_title_idle_sync_polls = 0;
            return true;
        }
        let should_sync = self.foreground_title_idle_sync_polls == 0;
        self.foreground_title_idle_sync_polls =
            self.foreground_title_idle_sync_polls.saturating_add(1)
                % RUNTIME_FOREGROUND_TITLE_IDLE_SYNC_POLL_INTERVAL;
        should_sync
    }

    /// Runs the foreground process pane title operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn foreground_process_pane_title(&self, pane_id: &str) -> Option<String> {
        let foreground_name = self.pane_processes.foreground_process_name(pane_id)?;
        let foreground_group = self.pane_processes.foreground_process_group_id(pane_id)?;
        let primary_pid = self.pane_processes.primary_pid(pane_id)?;
        self.title_from_foreground_process_metadata(
            pane_id,
            foreground_name,
            foreground_group,
            primary_pid,
        )
    }

    /// Applies foreground process metadata delivered by an async pane worker.
    pub fn apply_pane_foreground_process_event(
        &mut self,
        pane_id: impl Into<String>,
        process_name: impl Into<String>,
        process_group_id: u32,
        current_working_directory: Option<String>,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        let Some(primary_pid) = self.primary_pid_for_live_pane_process(&pane_id) else {
            return Ok(false);
        };
        if let Some(current_working_directory) = current_working_directory
            && !current_working_directory.trim().is_empty()
        {
            self.pane_current_working_directories
                .insert(pane_id.clone(), PathBuf::from(current_working_directory));
        }
        self.pane_foreground_process_groups
            .insert(pane_id.clone(), process_group_id);
        let Some(title) = self.title_from_foreground_process_metadata(
            &pane_id,
            process_name.into(),
            process_group_id,
            primary_pid,
        ) else {
            return Ok(false);
        };
        if !self
            .session
            .set_pane_title_from_terminal(pane_id.as_str(), title)?
        {
            return Ok(false);
        }
        let Some(update) = self.pane_title_only_update(&pane_id) else {
            return Ok(false);
        };
        self.append_pane_title_event(&update)?;
        Ok(true)
    }

    /// Returns the best known current working directory for a live pane.
    ///
    /// Async pane workers publish foreground metadata into
    /// `pane_current_working_directories`; prefer that actor-owned snapshot so
    /// command planning does not synchronously probe host process metadata when
    /// an async observation is already available.
    pub(in crate::runtime) fn pane_current_working_directory(
        &self,
        pane_id: &str,
    ) -> Option<PathBuf> {
        self.pane_current_working_directories
            .get(pane_id)
            .cloned()
            .or_else(|| self.pane_processes.current_working_directory(pane_id))
    }

    /// Runs the title from foreground process metadata operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn title_from_foreground_process_metadata(
        &self,
        pane_id: &str,
        foreground_name: String,
        foreground_group: u32,
        primary_pid: u32,
    ) -> Option<String> {
        if foreground_group == primary_pid
            && Some(foreground_name.as_str()) == self.session.shell.path().file_name()?.to_str()
        {
            return self
                .pane_screens
                .get(pane_id)
                .and_then(TerminalScreen::title)
                .filter(|title| !title.trim().is_empty())
                .map(ToOwned::to_owned)
                .or_else(|| Some("shell".to_string()));
        }
        Some(foreground_name)
    }

    /// Runs the pane title only update operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pane_title_only_update(&self, pane_id: &str) -> Option<PaneOutputUpdate> {
        let descriptor = self.find_pane_descriptor(pane_id)?;
        Some(PaneOutputUpdate {
            session_id: self.session.id.to_string(),
            window_id: descriptor.window_id.to_string(),
            pane_id: pane_id.to_string(),
            primary_pid: self.primary_pid_for_live_pane_process(pane_id)?,
            bytes_read: 0,
            activity_events: 0,
            bell_events: 0,
            background: !self.session.active_window().is_some_and(|window| {
                window.active_pane().id.as_str() == descriptor.pane_id.as_str()
            }),
        })
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
