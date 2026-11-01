//! Bounded model-facing mismatch evidence for the shared hunk matcher.
//!
//! This module derives replacement hints, candidate excerpts and recovery advice
//! from the parent's exact search facts. It cannot grant authority or alter a
//! match. Diagnostic searches retain the same explicit work control, and their
//! first terminal error is checked by the parent before any diagnostic escapes.
//! Excerpts inspect only their displayed prefix, even for very long lines.

use super::{
    APPLY_PATCH_MATCH_CANDIDATE_LIMIT, APPLY_PATCH_MATCH_MODES, APPLY_PATCH_MISMATCH_ANCHOR_LIMIT,
    ApplyPatchCandidateSpan, ApplyPatchHunkMatchProblem, ApplyPatchMatchAttempt,
    ApplyPatchMatchMode, ApplyPatchSearchScope, ApplyPatchTextFile, MezError, MezPatchHunk,
    MezPatchHunkLine, PatchWorkControl, apply_patch_anchor_line_numbers,
    find_line_sequence_matches, line_sequence_matches, ordered_anchor_chains,
    ordered_anchor_search_ranges, patch_line_matches, structural_anchor_ranges,
};

/// Builds bounded recovery evidence; the hunk owner checks terminal work
/// failure before exposing this ordinary diagnostic to a caller.
pub(super) fn apply_patch_hunk_mismatch_error(
    work: &PatchWorkControl<'_>,
    path: &str,
    file: &ApplyPatchTextFile,
    hunk: &MezPatchHunk,
    problem: ApplyPatchHunkMatchProblem,
) -> MezError {
    let (
        failure_code,
        reason,
        candidate_spans,
        context_center,
        missing_anchor,
        mode,
        attempts,
        scope,
        range_rejection,
    ) = match problem {
        ApplyPatchHunkMatchProblem::Missing {
            context_center,
            missing_anchor,
            attempts,
            scope,
            range_rejection,
        } => (
            "HUNK_CONTEXT_MISMATCH",
            "hunk context was not found in the current file",
            Vec::new(),
            context_center,
            missing_anchor,
            None,
            attempts,
            scope,
            range_rejection,
        ),
        ApplyPatchHunkMatchProblem::Ambiguous {
            candidate_spans,
            mode,
            attempts,
            scope,
            range_rejection,
        } => {
            let reason = match mode {
                Some(ApplyPatchMatchMode::Exact) | None => {
                    "exact hunk context is ambiguous in the current file"
                }
                Some(ApplyPatchMatchMode::TrimEnd) => {
                    "trim_end hunk context is ambiguous in the current file"
                }
                Some(ApplyPatchMatchMode::Trim) => {
                    "trim hunk context is ambiguous in the current file"
                }
                Some(ApplyPatchMatchMode::Normalized) => {
                    "normalized hunk context is ambiguous in the current file"
                }
            };
            (
                "HUNK_CONTEXT_AMBIGUOUS",
                reason,
                candidate_spans,
                None,
                None,
                mode,
                attempts,
                scope,
                range_rejection,
            )
        }
    };
    let candidate_lines = candidate_spans
        .iter()
        .map(|span| span.start_line)
        .collect::<Vec<_>>();
    let mut message = format!(
        "apply_patch: hunk did not match: {path}\n\
         apply_patch: {reason}\n\
         apply_patch: failure_code={failure_code}\n\
         apply_patch: affected_path={path}\n\
         apply_patch: failed old-context line count: {}",
        hunk.old.len()
    );
    if !attempts.is_empty() {
        message.push_str(&format!(
            "\napply_patch: matching_attempts={}",
            apply_patch_match_attempts_summary(&attempts)
        ));
    }
    if let Some(mode) = mode {
        message.push_str(&format!(
            "\napply_patch: ambiguous_matching_mode={}",
            mode.as_str()
        ));
    }
    message.push_str(&format!("\napply_patch: matching_scope={}", scope.as_str()));
    if !hunk.anchors.is_empty() {
        message.push_str(&format!(
            "\napply_patch: hunk header anchor(s): {}",
            hunk.anchors.join(" -> ")
        ));
    }
    if let Some(range_hint) = hunk.range_hint {
        message.push_str(&format!(
            "\napply_patch: hunk header old-line hint: {}",
            range_hint.old_start
        ));
    }
    if let Some(anchor) = &missing_anchor {
        message.push_str(&format!(
            "\napply_patch: hunk header anchor was not found in order: {}",
            apply_patch_mismatch_excerpt(anchor)
        ));
    }
    if !candidate_lines.is_empty() {
        message.push_str(&format!(
            "\napply_patch: candidate match line(s): {}",
            candidate_lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !candidate_spans.is_empty() {
        message.push_str(&format!(
            "\napply_patch: candidate match span(s): {}",
            candidate_spans
                .iter()
                .map(|span| {
                    if span.start_line == span.end_line {
                        span.start_line.to_string()
                    } else {
                        format!("{}-{}", span.start_line, span.end_line)
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let candidate_context_ranges = apply_patch_candidate_context_ranges(file, &candidate_spans);
    if !candidate_context_ranges.is_empty() {
        message.push_str(&format!(
            "\napply_patch: suggested_candidate_read_range(s): {}",
            candidate_context_ranges
                .iter()
                .map(|(start, end)| format!("{path}:{start}-{end}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(rejection) = range_rejection {
        message.push_str(&format!(
            "\napply_patch: range_hint_disambiguation=rejected reason={} hint_line={}",
            rejection.reason.as_str(),
            rejection.hint_line
        ));
        if let Some(distance) = rejection.nearest_distance {
            message.push_str(&format!(" nearest_distance={distance}"));
        }
        if let Some(distance) = rejection.next_distance {
            message.push_str(&format!(" next_distance={distance}"));
        }
    }
    let replacement_hint = apply_patch_replacement_presence_hint(work, file, hunk, scope);
    if let Some(hint) = &replacement_hint {
        message.push_str(&format!(
            "\napply_patch: replacement_hint={} span(s): {}",
            hint.kind.as_str(),
            hint.spans
                .iter()
                .map(|span| {
                    if span.start_line == span.end_line {
                        span.start_line.to_string()
                    } else {
                        format!("{}-{}", span.start_line, span.end_line)
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        ));
        message.push_str(
            "\napply_patch: replacement_hint_next_step=skip_or_reconcile_already_applied_change",
        );
    }
    if let Some(first_line) = hunk.old.first() {
        let anchor_lines = apply_patch_anchor_line_numbers(work, &file.lines, first_line);
        if anchor_lines.is_empty() {
            message.push_str("\napply_patch: first old-context line was not found anywhere");
            if let Some((mode, nearby_lines)) =
                apply_patch_non_exact_anchor_line_numbers(work, &file.lines, first_line)
            {
                message.push_str(&format!(
                    "\napply_patch: first old-context line nearest non-exact match mode={} current line(s): {}",
                    mode.as_str(),
                    nearby_lines
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        } else {
            message.push_str(&format!(
                "\napply_patch: first old-context line appears at current line(s): {}",
                anchor_lines
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    let context_center = context_center
        .or_else(|| {
            candidate_lines
                .first()
                .and_then(|line| line.checked_sub(1))
                .or_else(|| {
                    hunk.old.first().and_then(|line| {
                        apply_patch_first_old_context_center(work, &file.lines, line)
                    })
                })
        })
        .or_else(|| (!file.lines.is_empty()).then_some(0));
    if replacement_hint.is_some() {
        message.push_str(
            "\napply_patch: suggested_next_step=skip_or_reconcile_already_applied_change\
             \napply_patch: retry_without_reread=false",
        );
    } else if missing_anchor.is_some() {
        message.push_str(
            "\napply_patch: suggested_next_step=fix_or_refresh_header_anchor\
             \napply_patch: retry_without_reread=false",
        );
    } else if !candidate_context_ranges.is_empty() {
        message.push_str(
            "\napply_patch: suggested_next_step=reread_candidate_regions\
             \napply_patch: retry_without_reread=false",
        );
    } else if let Some(center) = context_center
        && let Some((start, end)) = apply_patch_current_context_range(file, center)
    {
        message.push_str(
            "\napply_patch: suggested_next_step=reread_region\
             \napply_patch: retry_without_reread=false",
        );
        message.push_str(&format!(
            "\napply_patch: suggested_read_range={path}:{start}-{end}"
        ));
    } else {
        message.push_str(
            "\napply_patch: suggested_next_step=reread_target_file\
             \napply_patch: retry_without_reread=false",
        );
    }
    if let Some(center) = context_center {
        message.push_str(&apply_patch_current_context_message(file, center));
    }
    message.push_str("\napply_patch: failed old context follows:");
    for line in hunk.old.iter().take(APPLY_PATCH_MISMATCH_CONTEXT_LINES) {
        message.push_str("\napply_patch:   ");
        message.push_str(&apply_patch_mismatch_excerpt(line));
    }
    if hunk.old.len() > APPLY_PATCH_MISMATCH_CONTEXT_LINES {
        message.push_str(&format!(
            "\napply_patch:   ... ({} more old-context lines omitted)",
            hunk.old.len() - APPLY_PATCH_MISMATCH_CONTEXT_LINES
        ));
    }
    if replacement_hint.is_some() {
        message.push_str(
            "\napply_patch: next step: inspect the reported replacement span(s); if the intended change is already present, skip this hunk or reconcile the surrounding edit instead of forcing another retry",
        );
    } else if missing_anchor.is_some() {
        message.push_str(&format!(
            "\napply_patch: next step: refresh or correct the missing @@ header anchor for {path}, then retry with current anchor context"
        ));
    } else if candidate_context_ranges.is_empty() {
        message.push_str(&format!(
            "\napply_patch: next step: read {path} around the reported line(s), then retry with a smaller fresh Mezzanine patch using a distinctive @@ header anchor"
        ));
    } else {
        message.push_str(&format!(
            "\napply_patch: next step: read {path} around the reported candidate range(s), then retry with a smaller fresh Mezzanine patch using a distinctive @@ header anchor"
        ));
    }
    message.push_str(
        "\napply_patch: do not retry substantially the same patch without fresh target context",
    );
    MezError::invalid_args(message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyPatchReplacementPresenceKind {
    FullReplacementBlockPresent,
    DistinctiveAddedLinesPresent,
}

impl ApplyPatchReplacementPresenceKind {
    fn as_str(self) -> &'static str {
        match self {
            ApplyPatchReplacementPresenceKind::FullReplacementBlockPresent => {
                "full_replacement_block_present"
            }
            ApplyPatchReplacementPresenceKind::DistinctiveAddedLinesPresent => {
                "distinctive_added_lines_present"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyPatchReplacementPresenceHint {
    kind: ApplyPatchReplacementPresenceKind,
    spans: Vec<ApplyPatchCandidateSpan>,
}

/// Finds bounded already-present replacement evidence without changing matching authority.
fn apply_patch_replacement_presence_hint(
    work: &PatchWorkControl<'_>,
    file: &ApplyPatchTextFile,
    hunk: &MezPatchHunk,
    scope: ApplyPatchSearchScope,
) -> Option<ApplyPatchReplacementPresenceHint> {
    let ranges = apply_patch_replacement_search_ranges(work, file, hunk, scope);
    if !hunk.new.is_empty()
        && !line_sequence_matches(work, &hunk.new, &hunk.old, ApplyPatchMatchMode::Exact)
    {
        let spans = apply_patch_exact_sequence_spans(work, &file.lines, &hunk.new, &ranges);
        if !spans.is_empty() {
            return Some(ApplyPatchReplacementPresenceHint {
                kind: ApplyPatchReplacementPresenceKind::FullReplacementBlockPresent,
                spans,
            });
        }
    }

    let distinctive_added_lines = hunk
        .lines
        .iter()
        .take_while(|_| work.spend(1))
        .filter_map(|line| match line {
            MezPatchHunkLine::Add(text) => Some(text),
            MezPatchHunkLine::Context(_) | MezPatchHunkLine::Remove(_) => None,
        })
        .filter(|line| apply_patch_added_line_is_distinctive(work, line, &hunk.old))
        .fold(Vec::<&String>::new(), |mut lines, line| {
            if !lines
                .iter()
                .take_while(|_| work.running())
                .any(|previous| work.equal(previous, line))
            {
                lines.push(line);
            }
            lines
        });
    if distinctive_added_lines.is_empty() {
        return None;
    }

    let mut spans = Vec::new();
    for line in distinctive_added_lines {
        let line_spans = apply_patch_exact_sequence_spans(
            work,
            &file.lines,
            std::slice::from_ref(line),
            &ranges,
        );
        if line_spans.is_empty() {
            return None;
        }
        spans.extend(line_spans);
        if spans.len() >= APPLY_PATCH_MATCH_CANDIDATE_LIMIT {
            spans.truncate(APPLY_PATCH_MATCH_CANDIDATE_LIMIT);
            break;
        }
    }
    Some(ApplyPatchReplacementPresenceHint {
        kind: ApplyPatchReplacementPresenceKind::DistinctiveAddedLinesPresent,
        spans,
    })
}

/// Reuses exact matcher scopes for diagnostic replacement searches.
fn apply_patch_replacement_search_ranges(
    work: &PatchWorkControl<'_>,
    file: &ApplyPatchTextFile,
    hunk: &MezPatchHunk,
    scope: ApplyPatchSearchScope,
) -> Vec<(usize, usize)> {
    if !hunk.anchors.is_empty() {
        let chains = ordered_anchor_chains(work, &file.lines, &hunk.anchors, 0);
        if !chains.is_empty() {
            if scope == ApplyPatchSearchScope::StructuralAnchorScope {
                let structural_ranges = structural_anchor_ranges(work, &file.lines, &chains);
                if !structural_ranges.is_empty() {
                    return structural_ranges;
                }
            }
            let ordered_ranges = ordered_anchor_search_ranges(&file.lines, &chains, 0);
            if !ordered_ranges.is_empty() {
                return ordered_ranges;
            }
        }
    }
    vec![(0, file.lines.len())]
}

/// Collects capped exact diagnostic spans under the same finite search allowance.
fn apply_patch_exact_sequence_spans(
    work: &PatchWorkControl<'_>,
    lines: &[String],
    needle: &[String],
    ranges: &[(usize, usize)],
) -> Vec<ApplyPatchCandidateSpan> {
    let mut spans = Vec::new();
    for (start, end) in ranges {
        let matches = find_line_sequence_matches(
            work,
            lines,
            needle,
            *start,
            *end,
            ApplyPatchMatchMode::Exact,
        );
        spans.extend(matches.lines.into_iter().map(|position| {
            ApplyPatchCandidateSpan::from_position_and_len(position, needle.len())
        }));
        if spans.len() >= APPLY_PATCH_MATCH_CANDIDATE_LIMIT {
            spans.truncate(APPLY_PATCH_MATCH_CANDIDATE_LIMIT);
            break;
        }
    }
    spans
}

/// Recognizes useful replacement evidence with controlled whitespace and old-line comparisons.
fn apply_patch_added_line_is_distinctive(
    work: &PatchWorkControl<'_>,
    line: &str,
    old: &[String],
) -> bool {
    let trimmed = work.trim(line, true);
    !trimmed.is_empty()
        && !matches!(trimmed, "{" | "}" | ");" | "," | ".")
        && trimmed
            .chars()
            .take_while(|character| work.spend(character.len_utf8()))
            .any(|character| character.is_ascii_alphanumeric() || character == '_')
        && !old
            .iter()
            .take_while(|_| work.spend(1))
            .any(|old_line| work.equal(old_line, line))
}

fn apply_patch_match_attempts_summary(attempts: &[ApplyPatchMatchAttempt]) -> String {
    attempts
        .iter()
        .map(|attempt| {
            let count = if attempt.capped {
                format!(">={}", attempt.candidate_count)
            } else {
                attempt.candidate_count.to_string()
            };
            format!("{}:{count}", attempt.mode.as_str())
        })
        .collect::<Vec<_>>()
        .join(",")
}

const APPLY_PATCH_MISMATCH_CONTEXT_LINES: usize = 8;
const APPLY_PATCH_MISMATCH_LINE_CHARS: usize = 160;

/// Chooses bounded context evidence from exact or tolerant anchor scans.
fn apply_patch_first_old_context_center(
    work: &PatchWorkControl<'_>,
    lines: &[String],
    anchor: &str,
) -> Option<usize> {
    apply_patch_anchor_line_numbers(work, lines, anchor)
        .first()
        .copied()
        .or_else(|| {
            apply_patch_non_exact_anchor_line_numbers(work, lines, anchor)
                .and_then(|(_, line_numbers)| line_numbers.first().copied())
        })
        .and_then(|line| line.checked_sub(1))
}

/// Collects capped tolerant context evidence without hiding long-line work.
fn apply_patch_non_exact_anchor_line_numbers(
    work: &PatchWorkControl<'_>,
    lines: &[String],
    anchor: &str,
) -> Option<(ApplyPatchMatchMode, Vec<usize>)> {
    APPLY_PATCH_MATCH_MODES
        .iter()
        .copied()
        .filter(|mode| *mode != ApplyPatchMatchMode::Exact)
        .find_map(|mode| {
            let line_numbers = lines
                .iter()
                .enumerate()
                .take_while(|_| work.spend(1))
                .filter_map(|(index, line)| {
                    patch_line_matches(work, line, anchor, mode).then_some(index + 1)
                })
                .take(APPLY_PATCH_MISMATCH_ANCHOR_LIMIT)
                .collect::<Vec<_>>();
            (!line_numbers.is_empty()).then_some((mode, line_numbers))
        })
}

fn apply_patch_mismatch_excerpt(line: &str) -> String {
    let mut excerpt: String = line.chars().take(APPLY_PATCH_MISMATCH_LINE_CHARS).collect();
    if line.chars().nth(APPLY_PATCH_MISMATCH_LINE_CHARS).is_some() {
        excerpt.push_str("...");
    }
    excerpt
}

fn apply_patch_current_context_message(file: &ApplyPatchTextFile, center: usize) -> String {
    if file.lines.is_empty() {
        return "\napply_patch: current file is empty".to_string();
    }
    let Some((start_line, end_line)) = apply_patch_current_context_range(file, center) else {
        return "\napply_patch: current file is empty".to_string();
    };
    let mut message = format!(
        "\napply_patch: current file context near line {} follows:",
        center + 1
    );
    for index in start_line.saturating_sub(1)..end_line {
        message.push_str(&format!(
            "\napply_patch:   {:>4}: {}",
            index + 1,
            apply_patch_mismatch_excerpt(&file.lines[index])
        ));
    }
    message
}

fn apply_patch_current_context_range(
    file: &ApplyPatchTextFile,
    center: usize,
) -> Option<(usize, usize)> {
    if file.lines.is_empty() {
        return None;
    }
    let start = center.saturating_sub(APPLY_PATCH_MISMATCH_CONTEXT_LINES / 2);
    let end = (center + (APPLY_PATCH_MISMATCH_CONTEXT_LINES / 2) + 1).min(file.lines.len());
    Some((start + 1, end))
}

fn apply_patch_candidate_context_ranges(
    file: &ApplyPatchTextFile,
    candidate_spans: &[ApplyPatchCandidateSpan],
) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for span in candidate_spans {
        let center = span.start_line.saturating_sub(1);
        if let Some(range) = apply_patch_current_context_range(file, center)
            && !ranges.contains(&range)
        {
            ranges.push(range);
        }
    }
    ranges
}
