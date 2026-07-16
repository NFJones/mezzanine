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
    /// Candidate value and display metadata.
    pub candidate: SelectorCandidate,
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
            candidate,
        }
    }
}

/// Starts active selection from one product-authored plan.
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
pub fn plan_selector(surface: SelectorSurface, line: &str, cursor: usize) -> Option<SelectorPlan> {
    plan_selector_with_extra(surface, line, cursor, &[])
}

/// Builds a selector plan for the token at `cursor` with runtime candidates.
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
pub fn shadow_hint(
    surface: SelectorSurface,
    line: &str,
    cursor: usize,
) -> Option<SelectorShadowHint> {
    shadow_hint_with_extra(surface, line, cursor, &[])
}

/// Builds the current prefix or parameter shadow hint with runtime candidates.
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
    if !context.query.is_empty() || context.tokens_before.len() != 1 {
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
