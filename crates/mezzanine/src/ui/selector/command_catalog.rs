//! Mezzanine and agent command catalogs plus command-specific candidates.

use super::{
    Path, SelectorCandidate, SelectorCandidateKind, SelectorExtraCandidate, SelectorSurface,
    SelectorTokenContext, baseline_commands, baseline_slash_commands, dedupe_selector_candidates,
    flag_candidates, path_candidates, value_candidates,
};

/// Builds command candidates for the Mezzanine prompt surface.
pub(super) fn mezzanine_candidates(context: &SelectorTokenContext) -> Vec<SelectorCandidate> {
    if context.tokens_before.is_empty() {
        return baseline_commands()
            .into_iter()
            .map(|command| {
                SelectorCandidate::new(command.name, SelectorCandidateKind::Command, true)
                    .with_detail(command.status.as_str())
            })
            .collect();
    }
    let command = context.tokens_before[0].as_str();
    mezzanine_argument_candidates(command)
}

/// Runs the agent candidates operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_candidates(context: &SelectorTokenContext) -> Vec<SelectorCandidate> {
    if context.tokens_before.is_empty() {
        if !context.query.is_empty() && !context.query.starts_with('/') {
            return path_candidates(SelectorSurface::AgentCommand, context, None);
        }
        return baseline_slash_commands()
            .into_iter()
            .flat_map(|spec| {
                let canonical = SelectorCandidate::new(
                    format!("/{}", spec.name),
                    SelectorCandidateKind::Command,
                    true,
                )
                .with_detail(format!("{:?}", spec.effect));
                let aliases = spec.aliases.iter().map(move |alias| {
                    SelectorCandidate::new(format!("/{alias}"), SelectorCandidateKind::Alias, true)
                        .with_detail(format!("alias for /{}", spec.name))
                });
                std::iter::once(canonical)
                    .chain(aliases)
                    .collect::<Vec<_>>()
            })
            .collect();
    }
    let command = context.tokens_before[0]
        .strip_prefix('/')
        .unwrap_or(context.tokens_before[0].as_str());
    agent_argument_candidates(canonical_agent_command(command), context)
}

/// Builds selector candidates from static command metadata plus runtime values.
pub(super) fn selector_candidates(
    surface: SelectorSurface,
    context: &SelectorTokenContext,
    extra_candidates: &[SelectorExtraCandidate],
    working_directory: Option<&Path>,
) -> Vec<SelectorCandidate> {
    let mut candidates = match surface {
        SelectorSurface::MezzanineCommand => mezzanine_candidates(context),
        SelectorSurface::AgentCommand => agent_candidates(context),
    };
    if surface == SelectorSurface::AgentCommand && context.query.starts_with('$') {
        candidates.extend(
            extra_candidates
                .iter()
                .filter(|extra| extra.surface == surface && extra.command == "$")
                .map(|extra| extra.candidate.clone()),
        );
    }
    if surface == SelectorSurface::AgentCommand && context.query.starts_with('@') {
        candidates.extend(
            extra_candidates
                .iter()
                .filter(|extra| extra.surface == surface && extra.command == "@")
                .map(|extra| extra.candidate.clone()),
        );
    }
    if surface == SelectorSurface::AgentCommand && context.query.starts_with('#') {
        candidates.extend(
            extra_candidates
                .iter()
                .filter(|extra| extra.surface == surface && extra.command == "#")
                .map(|extra| extra.candidate.clone()),
        );
    }
    let Some(command) = selector_context_command(surface, context) else {
        return candidates;
    };
    candidates.extend(
        extra_candidates
            .iter()
            .filter(|extra| {
                extra.surface == surface
                    && extra.command == command
                    && extra.preceding_option.as_deref().is_none_or(|option| {
                        context
                            .tokens_before
                            .last()
                            .is_some_and(|token| token == option)
                    })
            })
            .map(|extra| extra.candidate.clone()),
    );
    candidates.extend(path_candidates(surface, context, working_directory));
    candidates
}

/// Returns the canonical command receiving argument candidates.
pub(super) fn selector_context_command(
    surface: SelectorSurface,
    context: &SelectorTokenContext,
) -> Option<String> {
    let command = context.tokens_before.first()?.as_str();
    Some(match surface {
        SelectorSurface::MezzanineCommand => command.to_string(),
        SelectorSurface::AgentCommand => {
            let command = command.strip_prefix('/').unwrap_or(command);
            canonical_agent_command(command).to_string()
        }
    })
}

/// Runs the canonical agent command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn canonical_agent_command(command: &str) -> &str {
    for spec in baseline_slash_commands() {
        if spec.name == command || spec.aliases.contains(&command) {
            return spec.name;
        }
    }
    command
}

/// Runs the mezzanine argument candidates operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mezzanine_argument_candidates(command: &str) -> Vec<SelectorCandidate> {
    let mut candidates = Vec::new();
    candidates.extend(flag_candidates(common_target_flags()));
    match command {
        "new-window" | "new-group" => {
            candidates.extend(flag_candidates(&[
                "-n",
                "--name",
                "-c",
                "--start-directory",
                "-d",
                "--select",
                "--no-select",
                "--",
            ]));
        }
        "split-window" => {
            candidates.extend(flag_candidates(&[
                "-h",
                "-v",
                "-d",
                "-c",
                "--start-directory",
                "--size",
                "--percent",
                "--select",
                "--no-select",
                "--",
            ]));
            candidates.extend(value_candidates(&["horizontal", "vertical"]));
        }
        "select-window" | "select-group" | "attach-session" | "kill-session" => {
            candidates.extend(value_candidates(&["next", "previous", "last"]));
        }
        "select-pane" | "swap-pane" => {
            candidates.extend(flag_candidates(&[
                "-U", "--up", "-D", "--down", "-L", "--left", "-R", "--right",
            ]));
            candidates.extend(value_candidates(&["next", "previous", "prev", "last"]));
        }
        "resize-pane" => {
            candidates.extend(flag_candidates(&[
                "-L",
                "-R",
                "-U",
                "-D",
                "-x",
                "--columns",
                "-y",
                "--rows",
                "--percent",
                "--axis",
                "--delta",
                "--edge",
                "--amount",
            ]));
            candidates.extend(value_candidates(&[
                "columns",
                "rows",
                "horizontal",
                "vertical",
                "both",
                "left",
                "right",
                "up",
                "down",
            ]));
        }
        "select-layout" => {
            candidates.extend(value_candidates(&[
                "even-horizontal",
                "even-grid",
                "even-vertical",
                "main-horizontal",
                "main-vertical",
                "tiled",
            ]));
        }
        "copy-mode" | "search-history" | "export-history" | "capture-pane" => {
            candidates.extend(flag_candidates(&[
                "-t",
                "--target-pane",
                "--start",
                "--end",
                "--search",
                "--output",
            ]));
            candidates.extend(value_candidates(&["start", "end"]));
        }
        "paste-buffer" | "create-buffer" | "delete-buffer" | "save-buffer" | "choose-buffer" => {
            candidates.extend(flag_candidates(&[
                "-b",
                "--buffer",
                "-t",
                "--target-pane",
                "--delete",
                "--content",
                "--select",
                "--replace",
            ]));
        }
        "bind-key" | "unbind-key" => {
            candidates.extend(flag_candidates(&["-T", "--table", "-n", "--repeat"]));
            candidates.extend(value_candidates(&["prefix", "root"]));
        }
        "set-option" | "show-options" => {
            candidates.extend(flag_candidates(&[
                "-g", "--global", "-w", "--window", "-p", "--pane", "-u", "--unset",
            ]));
            candidates.extend(value_candidates(&[
                "session",
                "terminal",
                "frames",
                "theme",
                "history",
                "agents",
                "permissions",
            ]));
        }
        "save-layout" => {
            candidates.extend(flag_candidates(&["--name"]));
        }
        "load-layout" => {
            candidates.extend(flag_candidates(&["--name"]));
        }
        "set-theme" => {
            candidates.extend(value_candidates(mez_mux::theme::BUILTIN_UI_THEME_NAMES));
        }
        "agent-shell" => {
            candidates.extend(value_candidates(&["show", "hide", "toggle"]));
        }
        _ => {}
    }
    dedupe_selector_candidates(candidates)
}

/// Runs the common target flags operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn common_target_flags() -> &'static [&'static str] {
    &["-t", "--target", "-s", "--source"]
}

/// Runs the agent argument candidates operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn agent_argument_candidates(
    command: &str,
    _context: &SelectorTokenContext,
) -> Vec<SelectorCandidate> {
    let candidates = match command {
        "directive" => value_candidates(&["status", "show", "clear", "default", "none"]),
        "loop" => flag_candidates(&["--fork", "--new", "--limit"]),
        "memory" => value_candidates(&["on", "off", "toggle", "status", "show"]),
        "issue" => value_candidates(&[
            "add", "query", "delete", "--kind", "--title", "--body", "--text", "--limit",
        ]),
        "show-issues" => flag_candidates(&[
            "--project",
            "--project-glob",
            "--all-projects",
            "--kind",
            "--state",
            "--text",
            "--query",
            "--limit",
            "--save",
        ]),
        "latency" => value_candidates(&["slow", "default", "fast"]),
        "log-level" => value_candidates(&["normal", "verbose", "debug", "trace"]),
        "approval" | "permissions" => {
            let mut candidates = value_candidates(&["ask", "auto-allow", "full-access"]);
            candidates.extend(value_candidates(&[
                "add", "remove", "list", "rules", "allow", "deny", "prompt", "bypass",
            ]));
            candidates
        }
        "approve" => value_candidates(&["latest", "once", "session", "project", "global"]),
        "trust" => value_candidates(&["latest", "list", "pending"]),
        "model" => {
            let mut candidates = value_candidates(&[
                "list",
                "default",
                "gpt-5.5",
                "gpt-5.4",
                "gpt-5.4-mini",
                "gpt-5.3-codex",
                "gpt-5.3-codex-spark",
                "gpt-5.2",
                "low",
                "medium",
                "high",
                "xhigh",
            ]);
            candidates.extend(flag_candidates(&[
                "--routing",
                "--router",
                "--reasoning",
                "--clear",
                "--show",
            ]));
            candidates
        }
        "list-mcp" => Vec::new(),
        "resume" => flag_candidates(&["--latest"]),
        "routing" => value_candidates(&["on", "off", "toggle", "status"]),
        "thinking" => value_candidates(&["on", "off", "toggle", "status"]),
        "personality" => value_candidates(&["list", "status", "show", "clear", "default"]),
        "copy" => value_candidates(&["pane", "buffer", "clipboard"]),
        "copy-context" => value_candidates(&["pane", "buffer", "clipboard"]),
        "copy-trace-log" => value_candidates(&["pane", "buffer", "clipboard"]),
        "copy-patches" => value_candidates(&["pane", "buffer", "clipboard"]),
        "title" => value_candidates(&["default", "agent", "off"]),
        "debug-config" => value_candidates(&[
            "providers",
            "model_profiles",
            "mcp_servers",
            "hooks",
            "subagents",
            "permissions",
            "memory",
        ]),
        _ => Vec::new(),
    };
    dedupe_selector_candidates(candidates)
}
