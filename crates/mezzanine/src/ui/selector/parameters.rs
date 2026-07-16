//! Command parameter hints and finite flag or value candidate builders.

use super::{SelectorCandidate, SelectorCandidateKind};

/// Returns the parameter hint for a canonical Mezzanine command.
pub(super) fn mezzanine_parameter_hint(command: &str) -> Option<&'static str> {
    match command {
        "new-window" => Some(" [-n name] [-c dir] [-- command]"),
        "new-group" => Some(" [-n name] [-c dir] [-- command]"),
        "rename-group" => Some(" <name>"),
        "kill-group" => Some(" [-t target-group]"),
        "select-group" => Some(" <target>"),
        "rename-window" => Some(" <name>"),
        "kill-window" => Some(" [-t target-window]"),
        "select-window" | "attach-session" | "kill-session" => Some(" <target>"),
        "exit" => Some(""),
        "split-window" => Some(" [-h|-v] [-d] [-c dir] [-- command]"),
        "kill-pane" | "zoom-pane" | "display-panes" => Some(" [-t target-pane]"),
        "select-pane" | "swap-pane" => Some(" <-U|-D|-L|-R|next|previous|last>"),
        "resize-pane" => Some(" <-L|-R|-U|-D|--percent n|--amount n>"),
        "select-layout" => Some(" <layout>"),
        "detach-client" => Some(" [-t target-client]"),
        "rename-session" => Some(" <name>"),
        "copy-mode" => Some(" [-t target-pane]"),
        "paste-buffer" | "delete-buffer" | "choose-buffer" => Some(" [-b buffer] [-t target-pane]"),
        "create-buffer" => Some(" [-b buffer] [--content text] [--select] [--replace]"),
        "bind-key" => Some(" [-T table] <key> <command>"),
        "unbind-key" => Some(" [-T table] <key>"),
        "show-options" => Some(" [-g|-w|-p] [option]"),
        "set-option" => Some(" [-g|-w|-p] <option> <value>"),
        "set-theme" => Some(" <theme>"),
        "source-file" => Some(" <path>"),
        "agent-shell" => Some(" <show|hide|toggle>"),
        "mcp" => Some(" <list|inspect|add|remove|enable|disable|set|unset|tools|approval|retry>"),
        "mcp-status" => Some(" <name>"),
        "save-layout" => Some(" [--name name]"),
        "load-layout" => Some(" [--name name]"),
        "capture-pane" => Some(" [-t target-pane] [--start n] [--end n]"),
        "save-buffer" => Some(" [-b buffer] <path>"),
        "search-history" => Some(" [-t target-pane] <query>"),
        "export-history" | "pipe-pane" => Some(" [-t target-pane] <target>"),
        "mark-pane-ready" => Some(" <ready|unknown|blocked>"),
        "approve-observer" | "reject-observer" | "revoke-observer" => Some(" <observer-id>"),
        _ => None,
    }
}

/// Runs the agent parameter hint operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_parameter_hint(command: &str) -> Option<&'static str> {
    match command {
        "directive" => Some(" <status|show|clear|default|none|text>"),
        "loop" => Some(" [--fork|--new] [--limit <int>] <prompt>"),
        "memory" => Some(" <on|off|toggle|status|show>"),
        "issue" => Some(" <add|query|delete> [--kind defect|task] [--title text]"),
        "permissions" => {
            Some(" <status|preset|approval-policy|list|allow|deny|prompt|remove|bypass>")
        }
        "approval" => Some(" <ask|auto-allow|full-access>"),
        "approve" => Some(" <approval-id|latest> [once|session|project|global]"),
        "trust" => Some(" <project-root|latest|list|pending>"),
        "model" => Some(" [--routing] <list|model> [reasoning]"),
        "latency" => Some(" <slow|default|fast>"),
        "routing" => Some(" <on|off|toggle|status>"),
        "thinking" => Some(" <on|off|toggle|status>"),
        "log-level" => Some(" <normal|verbose|debug|trace>"),
        "copy" => Some(" <pane|buffer [name]|clipboard>"),
        "copy-context" => Some(" <pane|buffer [name]|clipboard>"),
        "copy-trace-log" => Some(" <pane|buffer [name]|clipboard>"),
        "copy-patches" => Some(" <pane|buffer [name]|clipboard>"),
        "personality" => Some(" <profile|style|list|status|show|clear|default>"),
        "remember" => Some(" [statement]"),
        "resume" => Some(" <session-uuid|--latest>"),
        "fork" => Some(" [conversation-id]"),
        "list-mcp" => Some(" [server-name]"),
        "title" => Some(" <title|default|off>"),
        "debug-config" => Some(" [filter]"),
        _ => None,
    }
}

/// Runs the flag candidates operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn flag_candidates(flags: &[&str]) -> Vec<SelectorCandidate> {
    flags
        .iter()
        .map(|flag| SelectorCandidate::new(*flag, SelectorCandidateKind::Flag, true))
        .collect()
}

/// Runs the value candidates operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn value_candidates(values: &[&str]) -> Vec<SelectorCandidate> {
    values
        .iter()
        .map(|value| SelectorCandidate::new(*value, SelectorCandidateKind::Value, true))
        .collect()
}
