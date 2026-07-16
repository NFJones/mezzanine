//! Escaping and platform browser-launch adapters.

use super::*;

/// Escapes text for insertion into the local callback HTML document.
pub(super) fn html_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Runs the json escape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push(' '),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Runs the open browser operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn open_browser(url: &str) -> bool {
    browser_open_commands(url).into_iter().any(|mut command| {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    })
}

/// Runs the browser open commands operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(target_os = "macos")]
pub(super) fn browser_open_commands(url: &str) -> Vec<Command> {
    let mut open = Command::new("open");
    open.arg(url);
    vec![open]
}

/// Runs the browser open commands operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(target_os = "windows")]
pub(super) fn browser_open_commands(url: &str) -> Vec<Command> {
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", "start", "", url]);
    vec![cmd]
}

/// Runs the browser open commands operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn browser_open_commands(url: &str) -> Vec<Command> {
    ["xdg-open", "gio", "sensible-browser"]
        .into_iter()
        .map(|program| {
            let mut command = Command::new(program);
            if program == "gio" {
                command.arg("open");
            }
            command.arg(url);
            command
        })
        .collect()
}
