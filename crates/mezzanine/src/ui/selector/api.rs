//! Public product selector adapter API and shadow-hint orchestration.

use super::{
    ActiveSelector, Path, SelectorCandidate, SelectorCandidateKind, SelectorPlan,
    SelectorShadowHint, SelectorTokenContext, agent_parameter_hint, canonical_agent_command,
    filter_and_sort_selector_candidates, mezzanine_parameter_hint,
    selector_candidate_prefix_suffix, selector_candidates, selector_token_context,
};

/// Interactive prompt surface requesting selector candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorSurface {
    /// The Mezzanine command prompt or configuration command prompt.
    MezzanineCommand,
    /// The pane-local agent prompt when slash-command input is active.
    AgentCommand,
}

/// A runtime-supplied candidate scoped to one prompt surface and command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorExtraCandidate {
    /// Prompt surface that may display this candidate.
    pub surface: SelectorSurface,
    /// Canonical command name whose argument list receives this candidate.
    pub command: String,
    /// Required preceding option when the candidate is valid only as its value.
    pub preceding_option: Option<String>,
    /// Optional subcommand argument slot restricting this dynamic candidate.
    pub subcommand_slot: Option<SelectorExtraCandidateSubcommandSlot>,
    /// Candidate value and display metadata.
    pub candidate: SelectorCandidate,
}

/// Positional scope for one runtime-provided subcommand argument candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorExtraCandidateSubcommandSlot {
    /// Required first argument after the canonical command.
    pub subcommand: String,
    /// Optional required nested argument immediately after the subcommand.
    pub nested_subcommand: Option<String>,
    /// Minimum number of completed tokens, including the command token.
    pub minimum_tokens_before: usize,
    /// Optional maximum number of completed tokens, including the command.
    pub maximum_tokens_before: Option<usize>,
    /// Optional terminal token after which the candidate is suppressed.
    pub terminal_token: Option<String>,
}

impl SelectorExtraCandidate {
    /// Builds a command-scoped selector candidate for dynamic runtime values.
    pub fn new(
        surface: SelectorSurface,
        command: impl Into<String>,
        candidate: SelectorCandidate,
    ) -> Self {
        Self {
            surface,
            command: command.into(),
            preceding_option: None,
            subcommand_slot: None,
            candidate,
        }
    }

    /// Builds a command-scoped candidate restricted to an option value slot.
    pub fn after_option(
        surface: SelectorSurface,
        command: impl Into<String>,
        option: impl Into<String>,
        candidate: SelectorCandidate,
    ) -> Self {
        Self {
            surface,
            command: command.into(),
            preceding_option: Some(option.into()),
            subcommand_slot: None,
            candidate,
        }
    }

    /// Builds a candidate restricted to a nested subcommand argument.
    pub fn after_nested_subcommand(
        surface: SelectorSurface,
        command: impl Into<String>,
        subcommand: impl Into<String>,
        nested_subcommand: impl Into<String>,
        token_bounds: (usize, Option<usize>),
        terminal_token: Option<&str>,
        candidate: SelectorCandidate,
    ) -> Self {
        Self {
            surface,
            command: command.into(),
            preceding_option: None,
            subcommand_slot: Some(SelectorExtraCandidateSubcommandSlot {
                subcommand: subcommand.into(),
                nested_subcommand: Some(nested_subcommand.into()),
                minimum_tokens_before: token_bounds.0,
                maximum_tokens_before: token_bounds.1,
                terminal_token: terminal_token.map(str::to_string),
            }),
            candidate,
        }
    }
}

/// Starts active selection from one product-authored plan.
#[cfg(test)]
pub fn start_active_selector(
    surface: SelectorSurface,
    line: &str,
    cursor: usize,
    reverse: bool,
) -> Option<ActiveSelector<SelectorSurface>> {
    start_active_selector_with_extra_in_working_directory(surface, line, cursor, reverse, &[], None)
}

/// Starts active selection with runtime candidates and explicit path context.
pub fn start_active_selector_with_extra_in_working_directory(
    surface: SelectorSurface,
    line: &str,
    cursor: usize,
    reverse: bool,
    extra_candidates: &[SelectorExtraCandidate],
    working_directory: Option<&Path>,
) -> Option<ActiveSelector<SelectorSurface>> {
    let plan = plan_selector_with_extra_in_working_directory(
        surface,
        line,
        cursor,
        extra_candidates,
        working_directory,
    )?;
    Some(ActiveSelector::new(surface, line, cursor, plan, reverse))
}

/// Builds a selector plan for the token at `cursor`.
#[cfg(test)]
pub fn plan_selector(surface: SelectorSurface, line: &str, cursor: usize) -> Option<SelectorPlan> {
    plan_selector_with_extra(surface, line, cursor, &[])
}

/// Builds a selector plan for the token at `cursor` with runtime candidates.
#[cfg(test)]
pub fn plan_selector_with_extra(
    surface: SelectorSurface,
    line: &str,
    cursor: usize,
    extra_candidates: &[SelectorExtraCandidate],
) -> Option<SelectorPlan> {
    plan_selector_with_extra_in_working_directory(surface, line, cursor, extra_candidates, None)
}

/// Builds a selector plan for the token at `cursor` with runtime candidates
/// resolved relative to one explicit working directory.
pub fn plan_selector_with_extra_in_working_directory(
    surface: SelectorSurface,
    line: &str,
    cursor: usize,
    extra_candidates: &[SelectorExtraCandidate],
    working_directory: Option<&Path>,
) -> Option<SelectorPlan> {
    let context = selector_token_context(line, cursor);
    let candidates = selector_candidates(surface, &context, extra_candidates, working_directory);
    let candidates = filter_and_sort_selector_candidates(candidates, &context.query);
    (!candidates.is_empty()).then_some(SelectorPlan {
        replacement_start: context.token_start,
        replacement_end: context.token_end,
        query: context.query,
        candidates,
    })
}

/// Builds the current prefix or parameter shadow hint without editing `line`.
#[cfg(test)]
pub fn shadow_hint(
    surface: SelectorSurface,
    line: &str,
    cursor: usize,
) -> Option<SelectorShadowHint> {
    shadow_hint_with_extra(surface, line, cursor, &[])
}

/// Builds the current prefix or parameter shadow hint with runtime candidates.
#[cfg(test)]
pub fn shadow_hint_with_extra(
    surface: SelectorSurface,
    line: &str,
    cursor: usize,
    extra_candidates: &[SelectorExtraCandidate],
) -> Option<SelectorShadowHint> {
    shadow_hint_with_extra_in_working_directory(surface, line, cursor, extra_candidates, None)
}

/// Builds the current prefix or parameter shadow hint with runtime candidates
/// resolved relative to one explicit working directory.
pub fn shadow_hint_with_extra_in_working_directory(
    surface: SelectorSurface,
    line: &str,
    cursor: usize,
    extra_candidates: &[SelectorExtraCandidate],
    working_directory: Option<&Path>,
) -> Option<SelectorShadowHint> {
    let context = selector_token_context(line, cursor);
    let cursor = context.cursor;
    prefix_shadow_hint(
        surface,
        &context,
        cursor,
        extra_candidates,
        working_directory,
    )
    .or_else(|| parameter_shadow_hint(surface, &context, cursor))
}

/// Builds a candidate-prefix shadow hint at the active cursor.
fn prefix_shadow_hint(
    surface: SelectorSurface,
    context: &SelectorTokenContext,
    cursor: usize,
    extra_candidates: &[SelectorExtraCandidate],
    working_directory: Option<&Path>,
) -> Option<SelectorShadowHint> {
    if cursor != context.token_end {
        return None;
    }
    if context.query.is_empty() {
        return None;
    }
    let candidates = selector_candidates(surface, context, extra_candidates, working_directory);
    let candidate = filter_and_sort_selector_candidates(candidates, &context.query)
        .into_iter()
        .find(|candidate| {
            selector_candidate_prefix_suffix(candidate.value.as_str(), &context.query).is_some()
        })?;
    let text = selector_candidate_prefix_suffix(candidate.value.as_str(), &context.query)?;
    (!text.is_empty()).then_some(SelectorShadowHint {
        insert_at: cursor,
        text,
        kind: candidate.kind,
    })
}

/// Runs the parameter shadow hint operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parameter_shadow_hint(
    surface: SelectorSurface,
    context: &SelectorTokenContext,
    cursor: usize,
) -> Option<SelectorShadowHint> {
    if !context.query.is_empty() {
        return None;
    }
    if surface == SelectorSurface::AgentCommand
        && context
            .tokens_before
            .first()
            .is_some_and(|command| command.trim_start_matches('/') == "sandbox")
        && let Some(text) = sandbox_parameter_shadow_text(context)
    {
        return Some(SelectorShadowHint {
            insert_at: cursor,
            text,
            kind: SelectorCandidateKind::Value,
        });
    }
    if surface == SelectorSurface::AgentCommand
        && matches!(context.tokens_before.len(), 2 | 3)
        && context.tokens_before[0].trim_start_matches('/') == "routing"
        && context.tokens_before[1] == "policy"
    {
        let text = if context.tokens_before.len() == 3 && context.tokens_before[2] == "--global" {
            " <subagent|in-place>"
        } else if context.tokens_before.len() == 2 {
            " [--global] <subagent|in-place>"
        } else {
            return None;
        };
        return Some(SelectorShadowHint {
            insert_at: cursor,
            text: text.to_string(),
            kind: SelectorCandidateKind::Value,
        });
    }
    if context.tokens_before.len() != 1 {
        return None;
    }
    let command = context.tokens_before[0].as_str();
    let text = match surface {
        SelectorSurface::MezzanineCommand => mezzanine_parameter_hint(command)?,
        SelectorSurface::AgentCommand => {
            let command = command.strip_prefix('/').unwrap_or(command);
            agent_parameter_hint(canonical_agent_command(command))?
        }
    };
    Some(SelectorShadowHint {
        insert_at: cursor,
        text: text.to_string(),
        kind: SelectorCandidateKind::Value,
    })
}

/// Returns a position-sensitive hint for the strict `/sandbox` hierarchy.
fn sandbox_parameter_shadow_text(context: &SelectorTokenContext) -> Option<String> {
    let arguments = context.tokens_before.get(1..)?;
    match arguments {
        [] => Some(" <status|enable|disable|trust|toolchains>".to_string()),
        [operation] if operation == "status" => Some(" [--global]".to_string()),
        [operation] if operation == "enable" || operation == "disable" => {
            Some(" --yes [--global]".to_string())
        }
        [operation, rest @ ..]
            if (operation == "enable" || operation == "disable")
                && !rest.iter().any(|token| token == "--yes") =>
        {
            Some(" --yes".to_string())
        }
        [operation] if operation == "trust" => {
            Some(" [project-root|latest|list|pending]".to_string())
        }
        [operation, ..] if operation == "toolchains" => toolchain_parameter_shadow_text(context, 2),
        _ => None,
    }
}

/// Returns a position-sensitive hint for nested sandbox toolchain grammar.
fn toolchain_parameter_shadow_text(
    context: &SelectorTokenContext,
    operation_index: usize,
) -> Option<String> {
    let arguments = context.tokens_before.get(operation_index..)?;
    match arguments {
        [] => Some(
            " <status|list|detect|define|enable|disable|remove|reload>".to_string(),
        ),
        [operation] if operation == "status" || operation == "detect" => {
            Some(" [SELECTOR]".to_string())
        }
        [operation] if operation == "enable" || operation == "disable" => {
            Some(" <SELECTOR...> --yes".to_string())
        }
        [operation, rest @ ..]
            if (operation == "enable" || operation == "disable")
                && !rest.iter().any(|token| token == "--yes") =>
        {
            Some(" [SELECTOR...] --yes".to_string())
        }
        [operation] if operation == "define" => Some(
            " <NAME> --root <PATH> --path <REF> [--require <REF>] [--env-root <NAME=REF>] [--description <TEXT>] --yes"
                .to_string(),
        ),
        [operation, _name, rest @ ..] if operation == "define" => {
            match rest.last().map(String::as_str) {
                Some("--root") => Some(" <absolute-path>".to_string()),
                Some("--path" | "--require") => {
                    Some(" <root-index:relative-path>".to_string())
                }
                Some("--env-root") => {
                    Some(" <NAME=root-index:relative-path>".to_string())
                }
                Some("--description") => Some(" <text>".to_string()),
                Some("--yes") => None,
                _ => {
                    let mut flags = vec!["--root", "--path", "--require", "--env-root"];
                    if !rest.iter().any(|token| token == "--description") {
                        flags.push("--description");
                    }
                    if !rest.iter().any(|token| token == "--yes") {
                        flags.push("--yes");
                    }
                    (!flags.is_empty()).then(|| format!(" <{}>", flags.join("|")))
                }
            }
        }
        [operation] if operation == "remove" => {
            Some(" <custom:NAME> [--disable] --yes".to_string())
        }
        [operation, _selector, rest @ ..] if operation == "remove" => {
            let disable = !rest.iter().any(|token| token == "--disable");
            let yes = !rest.iter().any(|token| token == "--yes");
            match (disable, yes) {
                (true, true) => Some(" [--disable] --yes".to_string()),
                (true, false) => Some(" [--disable]".to_string()),
                (false, true) => Some(" --yes".to_string()),
                (false, false) => None,
            }
        }
        [operation] if operation == "list" || operation == "reload" => None,
        _ => None,
    }
}
