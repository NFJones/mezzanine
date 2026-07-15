//! Hunk matching and mismatch diagnostics for semantic patches.
//!
//! This module owns the current-file matching strategy for parsed Mezzanine
//! patch hunks. It keeps tolerant matching, range-hint disambiguation,
//! structural-anchor scoping, replacement construction, and model-facing
//! mismatch diagnostics out of the patch facade.

use super::snapshot::ApplyPatchTextFile;
use crate::semantic_patch::{
    MezPatchHunk, MezPatchHunkLine, MezPatchRangeHint, SemanticPatchPlanningError as MezError,
    SemanticPatchPlanningResult as Result,
};

/// Applies parsed update hunks to one text file.
///
/// # Parameters
/// - `path`: The logical patch path, used for diagnostics.
/// - `file`: Current text file state before applying the hunks.
/// - `hunks`: Parsed update hunks to apply in order.
pub(super) fn apply_patch_hunks_to_file(
    path: &str,
    mut file: ApplyPatchTextFile,
    hunks: &[MezPatchHunk],
) -> Result<ApplyPatchTextFile> {
    if let Some(hunk) = hunks.iter().find(|hunk| hunk.replace_whole_file) {
        if hunks.len() != 1 {
            return Err(MezError::invalid_args(format!(
                "apply_patch: whole-file replacement for {path} must be the only update hunk"
            )));
        }
        if !hunk.old.is_empty() {
            return Err(MezError::invalid_args(format!(
                "apply_patch: whole-file replacement hunk for {path} must contain only added lines"
            )));
        }
        file.lines = hunk.new.clone();
        return Ok(file);
    }
    let mut cursor = 0usize;
    for hunk in hunks {
        let hunk_match = find_hunk_position(&file, hunk, cursor)
            .map_err(|problem| apply_patch_hunk_mismatch_error(path, &file, hunk, problem))?;
        let replacement = replacement_lines(hunk, &file, &hunk_match)?;
        let replacement_len = replacement.len();
        file.lines.splice(
            hunk_match.position..hunk_match.position + hunk_match.span_len(),
            replacement,
        );
        cursor = hunk_match.position + replacement_len;
    }
    Ok(file)
}

fn replacement_lines(
    hunk: &MezPatchHunk,
    file: &ApplyPatchTextFile,
    hunk_match: &ApplyPatchHunkMatch,
) -> Result<Vec<String>> {
    let mut old_index = 0usize;
    let mut next_source_offset = 0usize;
    let mut lines = Vec::new();
    let gap_policies = old_line_gap_policies(hunk);
    for line in &hunk.lines {
        match line {
            MezPatchHunkLine::Context(_) => {
                let gap_policy = *gap_policies
                    .get(old_index)
                    .unwrap_or(&ApplyPatchBlankGapPolicy::Disallow);
                let offset = *hunk_match.old_line_offsets.get(old_index).ok_or_else(|| {
                    MezError::invalid_args(
                        "apply_patch: internal hunk replacement range was invalid",
                    )
                })?;
                append_skipped_blank_context_lines(
                    &mut lines,
                    file,
                    hunk_match.position,
                    next_source_offset,
                    offset,
                    gap_policy,
                )?;
                let source = file
                    .lines
                    .get(hunk_match.position + offset)
                    .ok_or_else(|| {
                        MezError::invalid_args(
                            "apply_patch: internal hunk replacement range was invalid",
                        )
                    })?;
                lines.push(source.clone());
                old_index += 1;
                next_source_offset = offset.saturating_add(1);
            }
            MezPatchHunkLine::Remove(_) => {
                let gap_policy = *gap_policies
                    .get(old_index)
                    .unwrap_or(&ApplyPatchBlankGapPolicy::Disallow);
                let offset = *hunk_match.old_line_offsets.get(old_index).ok_or_else(|| {
                    MezError::invalid_args(
                        "apply_patch: internal hunk replacement range was invalid",
                    )
                })?;
                append_skipped_blank_context_lines(
                    &mut lines,
                    file,
                    hunk_match.position,
                    next_source_offset,
                    offset,
                    gap_policy,
                )?;
                next_source_offset = offset.saturating_add(1);
                old_index += 1;
            }
            MezPatchHunkLine::Add(text) => lines.push(text.clone()),
        }
    }
    Ok(lines)
}

fn old_line_gap_policies(hunk: &MezPatchHunk) -> Vec<ApplyPatchBlankGapPolicy> {
    let mut policies = Vec::with_capacity(hunk.old.len());
    let mut previous_old_kind = None;
    for line in &hunk.lines {
        match line {
            MezPatchHunkLine::Context(_) => {
                let policy = match previous_old_kind {
                    Some(MezPatchOldLineKind::Context | MezPatchOldLineKind::Remove) => {
                        ApplyPatchBlankGapPolicy::Preserve
                    }
                    _ => ApplyPatchBlankGapPolicy::Disallow,
                };
                policies.push(policy);
                previous_old_kind = Some(MezPatchOldLineKind::Context);
            }
            MezPatchHunkLine::Remove(_) => {
                let policy = match previous_old_kind {
                    Some(MezPatchOldLineKind::Context | MezPatchOldLineKind::Remove) => {
                        ApplyPatchBlankGapPolicy::Delete
                    }
                    _ => ApplyPatchBlankGapPolicy::Disallow,
                };
                policies.push(policy);
                previous_old_kind = Some(MezPatchOldLineKind::Remove);
            }
            MezPatchHunkLine::Add(_) => {}
        }
    }
    policies
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MezPatchOldLineKind {
    Context,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyPatchBlankGapPolicy {
    Disallow,
    Preserve,
    Delete,
}

fn append_skipped_blank_context_lines(
    output: &mut Vec<String>,
    file: &ApplyPatchTextFile,
    position: usize,
    start_offset: usize,
    end_offset: usize,
    policy: ApplyPatchBlankGapPolicy,
) -> Result<()> {
    if end_offset <= start_offset {
        return Ok(());
    }
    if policy == ApplyPatchBlankGapPolicy::Disallow {
        return Err(MezError::invalid_args(
            "apply_patch: internal hunk replacement range was invalid",
        ));
    }
    for offset in start_offset..end_offset {
        let source = file.lines.get(position + offset).ok_or_else(|| {
            MezError::invalid_args("apply_patch: internal hunk replacement range was invalid")
        })?;
        if !source.trim().is_empty() {
            return Err(MezError::invalid_args(
                "apply_patch: internal hunk replacement range was invalid",
            ));
        }
        if policy == ApplyPatchBlankGapPolicy::Preserve {
            output.push(source.clone());
        }
    }
    Ok(())
}

fn apply_patch_hunk_mismatch_error(
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
    let replacement_hint = apply_patch_replacement_presence_hint(file, hunk, scope);
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
        let anchor_lines = apply_patch_anchor_line_numbers(&file.lines, first_line);
        if anchor_lines.is_empty() {
            message.push_str("\napply_patch: first old-context line was not found anywhere");
            if let Some((mode, nearby_lines)) =
                apply_patch_non_exact_anchor_line_numbers(&file.lines, first_line)
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
                    hunk.old
                        .first()
                        .and_then(|line| apply_patch_first_old_context_center(&file.lines, line))
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

fn apply_patch_replacement_presence_hint(
    file: &ApplyPatchTextFile,
    hunk: &MezPatchHunk,
    scope: ApplyPatchSearchScope,
) -> Option<ApplyPatchReplacementPresenceHint> {
    let ranges = apply_patch_replacement_search_ranges(file, hunk, scope);
    if !hunk.new.is_empty() && hunk.new != hunk.old {
        let spans = apply_patch_exact_sequence_spans(&file.lines, &hunk.new, &ranges);
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
        .filter_map(|line| match line {
            MezPatchHunkLine::Add(text) => Some(text),
            MezPatchHunkLine::Context(_) | MezPatchHunkLine::Remove(_) => None,
        })
        .filter(|line| apply_patch_added_line_is_distinctive(line, &hunk.old))
        .fold(Vec::<&String>::new(), |mut lines, line| {
            if !lines.contains(&line) {
                lines.push(line);
            }
            lines
        });
    if distinctive_added_lines.is_empty() {
        return None;
    }

    let mut spans = Vec::new();
    for line in distinctive_added_lines {
        let line_spans =
            apply_patch_exact_sequence_spans(&file.lines, std::slice::from_ref(line), &ranges);
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

fn apply_patch_replacement_search_ranges(
    file: &ApplyPatchTextFile,
    hunk: &MezPatchHunk,
    scope: ApplyPatchSearchScope,
) -> Vec<(usize, usize)> {
    if !hunk.anchors.is_empty() {
        let chains = ordered_anchor_chains(&file.lines, &hunk.anchors, 0);
        if !chains.is_empty() {
            if scope == ApplyPatchSearchScope::StructuralAnchorScope {
                let structural_ranges = structural_anchor_ranges(&file.lines, &chains);
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

fn apply_patch_exact_sequence_spans(
    lines: &[String],
    needle: &[String],
    ranges: &[(usize, usize)],
) -> Vec<ApplyPatchCandidateSpan> {
    let mut spans = Vec::new();
    for (start, end) in ranges {
        let matches =
            find_line_sequence_matches(lines, needle, *start, *end, ApplyPatchMatchMode::Exact);
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

fn apply_patch_added_line_is_distinctive(line: &str, old: &[String]) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && !matches!(trimmed, "{" | "}" | ");" | "," | ".")
        && trimmed
            .chars()
            .any(|character| character.is_ascii_alphanumeric() || character == '_')
        && !old.iter().any(|old_line| old_line == line)
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
const APPLY_PATCH_MISMATCH_ANCHOR_LIMIT: usize = 5;
const APPLY_PATCH_MATCH_CANDIDATE_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyPatchMatchMode {
    Exact,
    TrimEnd,
    Trim,
    Normalized,
}

impl ApplyPatchMatchMode {
    fn as_str(self) -> &'static str {
        match self {
            ApplyPatchMatchMode::Exact => "exact",
            ApplyPatchMatchMode::TrimEnd => "trim_end",
            ApplyPatchMatchMode::Trim => "trim",
            ApplyPatchMatchMode::Normalized => "normalized",
        }
    }
}

const APPLY_PATCH_MATCH_MODES: &[ApplyPatchMatchMode] = &[
    ApplyPatchMatchMode::Exact,
    ApplyPatchMatchMode::TrimEnd,
    ApplyPatchMatchMode::Trim,
    ApplyPatchMatchMode::Normalized,
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyPatchHunkMatch {
    position: usize,
    mode: ApplyPatchMatchMode,
    old_line_offsets: Vec<usize>,
}

impl ApplyPatchHunkMatch {
    fn span_len(&self) -> usize {
        self.old_line_offsets
            .last()
            .map(|offset| offset.saturating_add(1))
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyPatchLineSequenceMatch {
    position: usize,
    old_line_offsets: Vec<usize>,
}

impl ApplyPatchLineSequenceMatch {
    fn exact(position: usize, old_line_count: usize) -> Self {
        Self {
            position,
            old_line_offsets: (0..old_line_count).collect(),
        }
    }

    fn into_hunk_match(self, mode: ApplyPatchMatchMode) -> ApplyPatchHunkMatch {
        ApplyPatchHunkMatch {
            position: self.position,
            mode,
            old_line_offsets: self.old_line_offsets,
        }
    }

    fn span_len(&self) -> usize {
        self.old_line_offsets
            .last()
            .map(|offset| offset.saturating_add(1))
            .unwrap_or(0)
    }

    fn span(&self) -> ApplyPatchCandidateSpan {
        ApplyPatchCandidateSpan::from_position_and_len(self.position, self.span_len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyPatchMatchAttempt {
    mode: ApplyPatchMatchMode,
    candidate_count: usize,
    capped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyPatchCandidateMatches {
    lines: Vec<usize>,
    capped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApplyPatchLineSequenceMatches {
    lines: Vec<ApplyPatchLineSequenceMatch>,
    capped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyPatchSearchScope {
    FullFile,
    OrderedAnchorRange,
    StructuralAnchorScope,
}

impl ApplyPatchSearchScope {
    fn as_str(self) -> &'static str {
        match self {
            ApplyPatchSearchScope::FullFile => "full_file",
            ApplyPatchSearchScope::OrderedAnchorRange => "ordered_anchor_range",
            ApplyPatchSearchScope::StructuralAnchorScope => "structural_anchor_scope",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ApplyPatchCandidateSpan {
    start_line: usize,
    end_line: usize,
}

impl ApplyPatchCandidateSpan {
    fn from_position_and_len(position: usize, len: usize) -> Self {
        let start_line = position + 1;
        let end_line = position + len.max(1);
        Self {
            start_line,
            end_line,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyPatchRangeHintRejectionReason {
    CandidateListCapped,
    Tie,
    NearTie,
    Distant,
}

impl ApplyPatchRangeHintRejectionReason {
    fn as_str(self) -> &'static str {
        match self {
            ApplyPatchRangeHintRejectionReason::CandidateListCapped => "candidate_list_capped",
            ApplyPatchRangeHintRejectionReason::Tie => "tie",
            ApplyPatchRangeHintRejectionReason::NearTie => "near_tie",
            ApplyPatchRangeHintRejectionReason::Distant => "distant",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ApplyPatchRangeHintRejection {
    hint_line: usize,
    nearest_distance: Option<usize>,
    next_distance: Option<usize>,
    reason: ApplyPatchRangeHintRejectionReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ApplyPatchHunkMatchProblem {
    Missing {
        context_center: Option<usize>,
        missing_anchor: Option<String>,
        attempts: Vec<ApplyPatchMatchAttempt>,
        scope: ApplyPatchSearchScope,
        range_rejection: Option<ApplyPatchRangeHintRejection>,
    },
    Ambiguous {
        candidate_spans: Vec<ApplyPatchCandidateSpan>,
        mode: Option<ApplyPatchMatchMode>,
        attempts: Vec<ApplyPatchMatchAttempt>,
        scope: ApplyPatchSearchScope,
        range_rejection: Option<ApplyPatchRangeHintRejection>,
    },
}

fn find_hunk_position(
    file: &ApplyPatchTextFile,
    hunk: &MezPatchHunk,
    cursor: usize,
) -> std::result::Result<ApplyPatchHunkMatch, ApplyPatchHunkMatchProblem> {
    if hunk.anchors.is_empty() {
        return find_unanchored_hunk_position(
            &file.lines,
            &hunk.old,
            &old_line_gap_policies(hunk),
            cursor,
            hunk.range_hint,
        );
    }

    let chains = ordered_anchor_chains(&file.lines, &hunk.anchors, cursor);
    if chains.is_empty() {
        return Err(ApplyPatchHunkMatchProblem::Missing {
            context_center: None,
            missing_anchor: first_missing_ordered_anchor(&file.lines, &hunk.anchors, cursor),
            attempts: Vec::new(),
            scope: ApplyPatchSearchScope::OrderedAnchorRange,
            range_rejection: None,
        });
    }

    if hunk.old.is_empty() {
        if chains.len() == 1 {
            return Ok(ApplyPatchHunkMatch {
                position: chains[0]
                    .last()
                    .copied()
                    .map(|line| (line + 1).min(file.lines.len()))
                    .unwrap_or_else(|| cursor.min(file.lines.len())),
                mode: ApplyPatchMatchMode::Exact,
                old_line_offsets: Vec::new(),
            });
        }
        return Err(ApplyPatchHunkMatchProblem::Ambiguous {
            candidate_spans: chains
                .iter()
                .filter_map(|chain| chain.last())
                .map(|line| ApplyPatchCandidateSpan::from_position_and_len(line + 1, 0))
                .take(APPLY_PATCH_MATCH_CANDIDATE_LIMIT)
                .collect(),
            mode: None,
            attempts: Vec::new(),
            scope: ApplyPatchSearchScope::OrderedAnchorRange,
            range_rejection: None,
        });
    }

    let structural_ranges = structural_anchor_ranges(&file.lines, &chains);
    if !structural_ranges.is_empty() {
        match find_hunk_position_in_ranges(
            &file.lines,
            &hunk.old,
            &old_line_gap_policies(hunk),
            &structural_ranges,
            hunk.range_hint,
            ApplyPatchSearchScope::StructuralAnchorScope,
        ) {
            Ok(hunk_match) => return Ok(hunk_match),
            Err(ApplyPatchLineSequenceFailure::Ambiguous {
                candidate_spans,
                mode,
                attempts,
                scope,
                range_rejection,
            }) => {
                return Err(ApplyPatchHunkMatchProblem::Ambiguous {
                    candidate_spans,
                    mode: Some(mode),
                    attempts,
                    scope,
                    range_rejection,
                });
            }
            Err(ApplyPatchLineSequenceFailure::Missing { .. }) => {}
        }
    }

    let ranges = ordered_anchor_search_ranges(&file.lines, &chains, cursor);
    find_hunk_position_in_ranges(
        &file.lines,
        &hunk.old,
        &old_line_gap_policies(hunk),
        &ranges,
        hunk.range_hint,
        ApplyPatchSearchScope::OrderedAnchorRange,
    )
    .map_err(|failure| match failure {
        ApplyPatchLineSequenceFailure::Missing {
            attempts,
            range_rejection,
            ..
        } => ApplyPatchHunkMatchProblem::Missing {
            context_center: chains.first().and_then(|chain| chain.last()).copied(),
            missing_anchor: None,
            attempts,
            scope: ApplyPatchSearchScope::OrderedAnchorRange,
            range_rejection,
        },
        ApplyPatchLineSequenceFailure::Ambiguous {
            candidate_spans,
            mode,
            attempts,
            scope,
            range_rejection,
        } => ApplyPatchHunkMatchProblem::Ambiguous {
            candidate_spans,
            mode: Some(mode),
            attempts,
            scope,
            range_rejection,
        },
    })
}

fn ordered_anchor_search_ranges(
    lines: &[String],
    chains: &[Vec<usize>],
    cursor: usize,
) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for (chain_index, chain) in chains.iter().enumerate() {
        let search_start = chain.first().copied().unwrap_or(cursor);
        let search_end = chains
            .get(chain_index + 1)
            .and_then(|next| next.first().copied())
            .unwrap_or(lines.len());
        ranges.push((search_start, search_end));
    }
    ranges
}

fn find_unanchored_hunk_position(
    lines: &[String],
    old: &[String],
    blank_gap_policies: &[ApplyPatchBlankGapPolicy],
    cursor: usize,
    range_hint: Option<MezPatchRangeHint>,
) -> std::result::Result<ApplyPatchHunkMatch, ApplyPatchHunkMatchProblem> {
    if old.is_empty() {
        return Ok(ApplyPatchHunkMatch {
            position: apply_patch_preferred_position(range_hint, lines.len())
                .unwrap_or(lines.len()),
            mode: ApplyPatchMatchMode::Exact,
            old_line_offsets: Vec::new(),
        });
    }
    find_unanchored_hunk_position_layered(lines, old, blank_gap_policies, cursor, range_hint)
        .map_err(|failure| match failure {
            ApplyPatchLineSequenceFailure::Missing {
                attempts,
                range_rejection,
                ..
            } => ApplyPatchHunkMatchProblem::Missing {
                context_center: old.first().and_then(|line| {
                    apply_patch_anchor_line_numbers(lines, line)
                        .first()
                        .and_then(|line| line.checked_sub(1))
                }),
                missing_anchor: None,
                attempts,
                scope: ApplyPatchSearchScope::FullFile,
                range_rejection,
            },
            ApplyPatchLineSequenceFailure::Ambiguous {
                candidate_spans,
                mode,
                attempts,
                scope,
                range_rejection,
            } => ApplyPatchHunkMatchProblem::Ambiguous {
                candidate_spans,
                mode: Some(mode),
                attempts,
                scope,
                range_rejection,
            },
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ApplyPatchLineSequenceFailure {
    Missing {
        attempts: Vec<ApplyPatchMatchAttempt>,
        scope: ApplyPatchSearchScope,
        range_rejection: Option<ApplyPatchRangeHintRejection>,
    },
    Ambiguous {
        candidate_spans: Vec<ApplyPatchCandidateSpan>,
        mode: ApplyPatchMatchMode,
        attempts: Vec<ApplyPatchMatchAttempt>,
        scope: ApplyPatchSearchScope,
        range_rejection: Option<ApplyPatchRangeHintRejection>,
    },
}

fn find_hunk_position_in_ranges(
    lines: &[String],
    old: &[String],
    blank_gap_policies: &[ApplyPatchBlankGapPolicy],
    ranges: &[(usize, usize)],
    range_hint: Option<MezPatchRangeHint>,
    scope: ApplyPatchSearchScope,
) -> std::result::Result<ApplyPatchHunkMatch, ApplyPatchLineSequenceFailure> {
    let mut attempts = Vec::new();
    for mode in APPLY_PATCH_MATCH_MODES {
        let mut candidates = Vec::new();
        let mut capped = false;
        for (start, end) in ranges {
            let matches = find_line_sequence_matches(lines, old, *start, *end, *mode);
            capped |= matches.capped;
            candidates.extend(
                matches
                    .lines
                    .into_iter()
                    .map(|line| ApplyPatchLineSequenceMatch::exact(line, old.len())),
            );
            if candidates.len() >= APPLY_PATCH_MATCH_CANDIDATE_LIMIT {
                candidates.truncate(APPLY_PATCH_MATCH_CANDIDATE_LIMIT);
                capped = true;
                break;
            }
        }
        attempts.push(ApplyPatchMatchAttempt {
            mode: *mode,
            candidate_count: candidates.len(),
            capped,
        });
        if let Some(hunk_match) =
            resolve_hunk_candidates(candidates, capped, *mode, range_hint, &attempts, scope)?
        {
            return Ok(hunk_match);
        }
        let tolerant_matches = find_line_sequence_matches_omitting_blank_context(
            lines,
            old,
            blank_gap_policies,
            ranges,
            *mode,
        );
        if !tolerant_matches.lines.is_empty() {
            attempts.push(ApplyPatchMatchAttempt {
                mode: *mode,
                candidate_count: tolerant_matches.lines.len(),
                capped: tolerant_matches.capped,
            });
        }
        if let Some(hunk_match) = resolve_hunk_candidates(
            tolerant_matches.lines,
            tolerant_matches.capped,
            *mode,
            range_hint,
            &attempts,
            scope,
        )? {
            return Ok(hunk_match);
        }
    }
    Err(ApplyPatchLineSequenceFailure::Missing {
        attempts,
        scope,
        range_rejection: None,
    })
}

fn find_unanchored_hunk_position_layered(
    lines: &[String],
    old: &[String],
    blank_gap_policies: &[ApplyPatchBlankGapPolicy],
    cursor: usize,
    range_hint: Option<MezPatchRangeHint>,
) -> std::result::Result<ApplyPatchHunkMatch, ApplyPatchLineSequenceFailure> {
    let mut attempts = Vec::new();
    let scope = ApplyPatchSearchScope::FullFile;
    for mode in APPLY_PATCH_MATCH_MODES {
        let mut matches = find_line_sequence_matches(lines, old, cursor, lines.len(), *mode);
        if matches.lines.is_empty() && cursor > 0 {
            matches = find_line_sequence_matches(lines, old, 0, lines.len(), *mode);
        }
        let candidates = matches
            .lines
            .into_iter()
            .map(|line| ApplyPatchLineSequenceMatch::exact(line, old.len()))
            .collect::<Vec<_>>();
        attempts.push(ApplyPatchMatchAttempt {
            mode: *mode,
            candidate_count: candidates.len(),
            capped: matches.capped,
        });
        if let Some(hunk_match) = resolve_hunk_candidates(
            candidates,
            matches.capped,
            *mode,
            range_hint,
            &attempts,
            scope,
        )? {
            return Ok(hunk_match);
        }
        let tolerant_ranges = if cursor > 0 {
            vec![
                (cursor.min(lines.len()), lines.len()),
                (0, cursor.min(lines.len())),
            ]
        } else {
            vec![(0, lines.len())]
        };
        let tolerant_matches = find_line_sequence_matches_omitting_blank_context(
            lines,
            old,
            blank_gap_policies,
            &tolerant_ranges,
            *mode,
        );
        if !tolerant_matches.lines.is_empty() {
            attempts.push(ApplyPatchMatchAttempt {
                mode: *mode,
                candidate_count: tolerant_matches.lines.len(),
                capped: tolerant_matches.capped,
            });
        }
        if let Some(hunk_match) = resolve_hunk_candidates(
            tolerant_matches.lines,
            tolerant_matches.capped,
            *mode,
            range_hint,
            &attempts,
            scope,
        )? {
            return Ok(hunk_match);
        }
    }
    Err(ApplyPatchLineSequenceFailure::Missing {
        attempts,
        scope,
        range_rejection: None,
    })
}

const APPLY_PATCH_RANGE_HINT_MAX_DISTANCE: usize = 20;
const APPLY_PATCH_RANGE_HINT_MIN_DISTANCE_GAP: usize = 3;

fn resolve_hunk_candidates(
    candidates: Vec<ApplyPatchLineSequenceMatch>,
    capped: bool,
    mode: ApplyPatchMatchMode,
    range_hint: Option<MezPatchRangeHint>,
    attempts: &[ApplyPatchMatchAttempt],
    scope: ApplyPatchSearchScope,
) -> std::result::Result<Option<ApplyPatchHunkMatch>, ApplyPatchLineSequenceFailure> {
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates
            .into_iter()
            .next()
            .map(|candidate| candidate.into_hunk_match(mode))),
        _ => match range_hint_candidate(&candidates, capped, range_hint) {
            ApplyPatchRangeHintSelection::Selected(index) => Ok(Some(
                candidates
                    .into_iter()
                    .nth(index)
                    .expect("selected candidate index should be valid")
                    .into_hunk_match(mode),
            )),
            ApplyPatchRangeHintSelection::Unavailable { rejection } => {
                Err(ApplyPatchLineSequenceFailure::Ambiguous {
                    candidate_spans: candidates
                        .iter()
                        .map(ApplyPatchLineSequenceMatch::span)
                        .collect(),
                    mode,
                    attempts: attempts.to_vec(),
                    scope,
                    range_rejection: rejection,
                })
            }
        },
    }
}

enum ApplyPatchRangeHintSelection {
    Selected(usize),
    Unavailable {
        rejection: Option<ApplyPatchRangeHintRejection>,
    },
}

fn range_hint_candidate(
    candidates: &[ApplyPatchLineSequenceMatch],
    capped: bool,
    range_hint: Option<MezPatchRangeHint>,
) -> ApplyPatchRangeHintSelection {
    let Some(range_hint) = range_hint else {
        return ApplyPatchRangeHintSelection::Unavailable { rejection: None };
    };
    if capped {
        return ApplyPatchRangeHintSelection::Unavailable {
            rejection: Some(ApplyPatchRangeHintRejection {
                hint_line: range_hint.old_start,
                nearest_distance: None,
                next_distance: None,
                reason: ApplyPatchRangeHintRejectionReason::CandidateListCapped,
            }),
        };
    }
    let hint_position = range_hint.old_start.saturating_sub(1);
    let mut distances = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            (
                index,
                range_hint_distance_to_candidate(hint_position, candidate),
            )
        })
        .collect::<Vec<_>>();
    distances.sort_by_key(|(_, distance)| *distance);
    let Some((nearest_index, nearest_distance)) = distances.first().copied() else {
        return ApplyPatchRangeHintSelection::Unavailable { rejection: None };
    };
    let next_distance = distances.get(1).map(|(_, distance)| *distance);
    if nearest_distance > APPLY_PATCH_RANGE_HINT_MAX_DISTANCE {
        return ApplyPatchRangeHintSelection::Unavailable {
            rejection: Some(ApplyPatchRangeHintRejection {
                hint_line: range_hint.old_start,
                nearest_distance: Some(nearest_distance),
                next_distance,
                reason: ApplyPatchRangeHintRejectionReason::Distant,
            }),
        };
    }
    if next_distance == Some(nearest_distance) {
        return ApplyPatchRangeHintSelection::Unavailable {
            rejection: Some(ApplyPatchRangeHintRejection {
                hint_line: range_hint.old_start,
                nearest_distance: Some(nearest_distance),
                next_distance,
                reason: ApplyPatchRangeHintRejectionReason::Tie,
            }),
        };
    }
    if let Some(next_distance) = next_distance
        && next_distance.saturating_sub(nearest_distance) < APPLY_PATCH_RANGE_HINT_MIN_DISTANCE_GAP
    {
        return ApplyPatchRangeHintSelection::Unavailable {
            rejection: Some(ApplyPatchRangeHintRejection {
                hint_line: range_hint.old_start,
                nearest_distance: Some(nearest_distance),
                next_distance: Some(next_distance),
                reason: ApplyPatchRangeHintRejectionReason::NearTie,
            }),
        };
    }
    ApplyPatchRangeHintSelection::Selected(nearest_index)
}

fn range_hint_distance_to_candidate(
    hint_position: usize,
    candidate: &ApplyPatchLineSequenceMatch,
) -> usize {
    let start = candidate.position;
    let end = candidate
        .span_len()
        .saturating_sub(1)
        .saturating_add(candidate.position);
    if hint_position < start {
        start - hint_position
    } else if hint_position > end {
        hint_position.saturating_sub(end)
    } else {
        0
    }
}

fn apply_patch_preferred_position(
    range_hint: Option<MezPatchRangeHint>,
    line_count: usize,
) -> Option<usize> {
    range_hint.map(|hint| hint.old_start.saturating_sub(1).min(line_count))
}

fn structural_anchor_ranges(lines: &[String], chains: &[Vec<usize>]) -> Vec<(usize, usize)> {
    let ranges = chains
        .iter()
        .filter_map(|chain| {
            let anchor_line = chain.last().copied()?;
            rust_like_block_scope(lines, anchor_line)
        })
        .collect::<Vec<_>>();
    merge_overlapping_ranges(ranges)
}

fn merge_overlapping_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable();
    let mut merged = Vec::<(usize, usize)>::new();
    for (start, end) in ranges {
        if let Some((_, previous_end)) = merged.last_mut()
            && start <= *previous_end
        {
            *previous_end = (*previous_end).max(end);
            continue;
        }
        merged.push((start, end));
    }
    merged
}

fn rust_like_block_scope(lines: &[String], anchor_line: usize) -> Option<(usize, usize)> {
    let anchor_text = lines.get(anchor_line)?;
    if !looks_like_rust_structural_anchor(anchor_text) {
        return None;
    }
    let mut depth = 0isize;
    let mut saw_open = false;
    let mut in_block_comment = false;
    for (line_index, line) in lines
        .iter()
        .enumerate()
        .skip(anchor_line)
        .take(APPLY_PATCH_STRUCTURAL_ANCHOR_SCAN_LINES)
    {
        let (opens, closes) = rust_like_brace_counts(line, &mut in_block_comment)?;
        if opens > 0 {
            saw_open = true;
        }
        depth += opens as isize;
        depth -= closes as isize;
        if depth < 0 {
            return None;
        }
        if saw_open && depth == 0 {
            return Some((anchor_line, line_index + 1));
        }
    }
    None
}

const APPLY_PATCH_STRUCTURAL_ANCHOR_SCAN_LINES: usize = 400;

fn looks_like_rust_structural_anchor(line: &str) -> bool {
    let line = line.trim_start();
    RUST_STRUCTURAL_ANCHOR_PREFIXES
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

const RUST_STRUCTURAL_ANCHOR_PREFIXES: &[&str] = &[
    "fn ",
    "pub fn ",
    "pub(crate) fn ",
    "pub(super) fn ",
    "async fn ",
    "pub async fn ",
    "const fn ",
    "pub const fn ",
    "impl ",
    "trait ",
    "pub trait ",
    "struct ",
    "pub struct ",
    "enum ",
    "pub enum ",
    "mod ",
    "pub mod ",
];

fn rust_like_brace_counts(line: &str, in_block_comment: &mut bool) -> Option<(usize, usize)> {
    let mut opens = 0usize;
    let mut closes = 0usize;
    let mut chars = line.chars().peekable();
    let mut in_string = false;
    let mut in_char = false;
    let mut escaped = false;
    while let Some(character) = chars.next() {
        if *in_block_comment {
            if character == '*' && chars.peek() == Some(&'/') {
                chars.next();
                *in_block_comment = false;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        if in_char {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '\'' {
                in_char = false;
            }
            continue;
        }
        match character {
            '/' if chars.peek() == Some(&'/') => break,
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                *in_block_comment = true;
            }
            'r' if rust_like_raw_string_literal_start(&mut chars) => return None,
            '"' => in_string = true,
            '\'' => in_char = true,
            '{' => opens += 1,
            '}' => closes += 1,
            _ => {}
        }
    }
    Some((opens, closes))
}

fn rust_like_raw_string_literal_start(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> bool {
    let mut clone = chars.clone();
    if clone.peek() == Some(&'"') {
        return true;
    }
    let mut saw_hash = false;
    while clone.peek() == Some(&'#') {
        saw_hash = true;
        clone.next();
    }
    saw_hash && clone.peek() == Some(&'"')
}

fn ordered_anchor_chains(lines: &[String], anchors: &[String], cursor: usize) -> Vec<Vec<usize>> {
    if anchors.is_empty() {
        return vec![Vec::new()];
    }

    let mut chains = Vec::new();
    for (index, line) in lines.iter().enumerate().skip(cursor.min(lines.len())) {
        if !line.contains(&anchors[0]) {
            continue;
        }
        let mut chain = vec![index];
        let mut next_start = index + 1;
        let mut complete = true;
        for anchor in &anchors[1..] {
            if let Some((next_index, _)) = lines
                .iter()
                .enumerate()
                .skip(next_start)
                .find(|(_, line)| line.contains(anchor))
            {
                chain.push(next_index);
                next_start = next_index + 1;
            } else {
                complete = false;
                break;
            }
        }
        if complete {
            chains.push(chain);
            if chains.len() >= APPLY_PATCH_MATCH_CANDIDATE_LIMIT {
                break;
            }
        }
    }
    chains
}

fn first_missing_ordered_anchor(
    lines: &[String],
    anchors: &[String],
    cursor: usize,
) -> Option<String> {
    let mut next_start = cursor.min(lines.len());
    for anchor in anchors {
        if let Some((next_index, _)) = lines
            .iter()
            .enumerate()
            .skip(next_start)
            .find(|(_, line)| line.contains(anchor))
        {
            next_start = next_index + 1;
        } else {
            return Some(anchor.clone());
        }
    }
    None
}

fn find_line_sequence_matches(
    lines: &[String],
    needle: &[String],
    start: usize,
    end: usize,
    mode: ApplyPatchMatchMode,
) -> ApplyPatchCandidateMatches {
    if needle.is_empty() {
        return ApplyPatchCandidateMatches {
            lines: vec![start.min(lines.len())],
            capped: false,
        };
    }
    let start = start.min(lines.len());
    let end = end.min(lines.len());
    if end < start || end.saturating_sub(start) < needle.len() {
        return ApplyPatchCandidateMatches {
            lines: Vec::new(),
            capped: false,
        };
    }
    let last_start = end - needle.len();
    let mut matches = Vec::new();
    let mut capped = false;
    for index in start..=last_start {
        if line_sequence_matches(&lines[index..index + needle.len()], needle, mode) {
            matches.push(index);
            if matches.len() >= APPLY_PATCH_MATCH_CANDIDATE_LIMIT {
                capped = true;
                break;
            }
        }
    }
    ApplyPatchCandidateMatches {
        lines: matches,
        capped,
    }
}

fn find_line_sequence_matches_omitting_blank_context(
    lines: &[String],
    needle: &[String],
    blank_gap_policies: &[ApplyPatchBlankGapPolicy],
    ranges: &[(usize, usize)],
    mode: ApplyPatchMatchMode,
) -> ApplyPatchLineSequenceMatches {
    if !blank_gap_policies
        .iter()
        .any(|policy| *policy != ApplyPatchBlankGapPolicy::Disallow)
        || blank_gap_policies.len() != needle.len()
    {
        return ApplyPatchLineSequenceMatches {
            lines: Vec::new(),
            capped: false,
        };
    }
    let mut matches = Vec::new();
    let mut capped = false;
    for (start, end) in ranges {
        let start = (*start).min(lines.len());
        let end = (*end).min(lines.len());
        if end < start {
            continue;
        }
        for index in start..end {
            if let Some(match_result) = line_sequence_match_omitting_blank_context_at(
                lines,
                needle,
                blank_gap_policies,
                index,
                end,
                mode,
            ) {
                matches.push(match_result);
                if matches.len() >= APPLY_PATCH_MATCH_CANDIDATE_LIMIT {
                    capped = true;
                    return ApplyPatchLineSequenceMatches {
                        lines: matches,
                        capped,
                    };
                }
            }
        }
    }
    ApplyPatchLineSequenceMatches {
        lines: matches,
        capped,
    }
}

fn line_sequence_match_omitting_blank_context_at(
    lines: &[String],
    needle: &[String],
    blank_gap_policies: &[ApplyPatchBlankGapPolicy],
    position: usize,
    end: usize,
    mode: ApplyPatchMatchMode,
) -> Option<ApplyPatchLineSequenceMatch> {
    if needle.is_empty()
        || blank_gap_policies.len() != needle.len()
        || !blank_gap_policies
            .iter()
            .any(|policy| *policy != ApplyPatchBlankGapPolicy::Disallow)
    {
        return None;
    }
    let end = end.min(lines.len());
    if position >= end {
        return None;
    }
    let mut actual_index = position;
    let mut old_line_offsets = Vec::with_capacity(needle.len());
    let mut skipped_blank = false;
    for (needle_index, expected) in needle.iter().enumerate() {
        if actual_index >= end {
            return None;
        }
        if patch_line_matches(&lines[actual_index], expected, mode) {
            old_line_offsets.push(actual_index.saturating_sub(position));
            actual_index += 1;
            continue;
        }
        if blank_gap_policies[needle_index] == ApplyPatchBlankGapPolicy::Disallow {
            return None;
        }
        let blank_start = actual_index;
        while actual_index < end && lines[actual_index].trim().is_empty() {
            actual_index += 1;
        }
        if actual_index == blank_start || actual_index >= end {
            return None;
        }
        if !patch_line_matches(&lines[actual_index], expected, mode) {
            return None;
        }
        skipped_blank = true;
        old_line_offsets.push(actual_index.saturating_sub(position));
        actual_index += 1;
    }
    skipped_blank.then_some(ApplyPatchLineSequenceMatch {
        position,
        old_line_offsets,
    })
}

fn line_sequence_matches(
    actual: &[String],
    expected: &[String],
    mode: ApplyPatchMatchMode,
) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| patch_line_matches(actual, expected, mode))
}

fn patch_line_matches(actual: &str, expected: &str, mode: ApplyPatchMatchMode) -> bool {
    match mode {
        ApplyPatchMatchMode::Exact => actual == expected,
        ApplyPatchMatchMode::TrimEnd => actual.trim_end() == expected.trim_end(),
        ApplyPatchMatchMode::Trim => actual.trim() == expected.trim(),
        ApplyPatchMatchMode::Normalized => {
            normalized_patch_line(actual) == normalized_patch_line(expected)
        }
    }
}

fn normalized_patch_line(line: &str) -> String {
    line.trim()
        .chars()
        .map(|character| match character {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

fn apply_patch_anchor_line_numbers(lines: &[String], anchor: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| (line == anchor).then_some(index + 1))
        .take(APPLY_PATCH_MISMATCH_ANCHOR_LIMIT)
        .collect()
}

fn apply_patch_first_old_context_center(lines: &[String], anchor: &str) -> Option<usize> {
    apply_patch_anchor_line_numbers(lines, anchor)
        .first()
        .copied()
        .or_else(|| {
            apply_patch_non_exact_anchor_line_numbers(lines, anchor)
                .and_then(|(_, line_numbers)| line_numbers.first().copied())
        })
        .and_then(|line| line.checked_sub(1))
}

fn apply_patch_non_exact_anchor_line_numbers(
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
                .filter_map(|(index, line)| {
                    patch_line_matches(line, anchor, mode).then_some(index + 1)
                })
                .take(APPLY_PATCH_MISMATCH_ANCHOR_LIMIT)
                .collect::<Vec<_>>();
            (!line_numbers.is_empty()).then_some((mode, line_numbers))
        })
}

fn apply_patch_mismatch_excerpt(line: &str) -> String {
    let mut excerpt: String = line.chars().take(APPLY_PATCH_MISMATCH_LINE_CHARS).collect();
    if line.chars().count() > APPLY_PATCH_MISMATCH_LINE_CHARS {
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

#[cfg(test)]
mod tests {
    use super::{
        ApplyPatchBlankGapPolicy, find_unanchored_hunk_position_layered, rust_like_brace_counts,
        structural_anchor_ranges,
    };

    /// Verifies tolerant unanchored search uses non-overlapping cursor-before
    /// and cursor-after ranges. A match at or after the cursor used to be
    /// discovered once in the cursor-forward range and again in the full-file
    /// fallback range, which made a unique blank-gap match look ambiguous.
    #[test]
    fn unanchored_tolerant_search_does_not_duplicate_cursor_forward_match() {
        let lines = ["before", "target", "", "after", "tail"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let old = ["target", "after"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let blank_gap_policies = [
            ApplyPatchBlankGapPolicy::Disallow,
            ApplyPatchBlankGapPolicy::Preserve,
        ];

        let result =
            find_unanchored_hunk_position_layered(&lines, &old, &blank_gap_policies, 1, None);

        assert!(result.is_ok());
    }

    /// Verifies raw-string detection does not disable structural brace counts
    /// merely because ordinary string or comment text contains the byte
    /// sequence `r\"`. Structural anchor scoping depends on this helper
    /// returning counts for ordinary Rust-like code.
    #[test]
    fn rust_like_brace_counts_ignores_raw_string_markers_inside_strings_and_comments() {
        let mut in_block_comment = false;

        let counts = rust_like_brace_counts(
            r#"fn target() { let text = "mentions r\" without raw syntax"; } // r\" comment"#,
            &mut in_block_comment,
        );

        assert_eq!(counts, Some((1, 1)));
    }

    /// Verifies real Rust raw string literals still disable structural brace
    /// counting. The brace scanner intentionally bails out for raw strings so
    /// braces inside raw-string bodies cannot corrupt anchor scope detection.
    #[test]
    fn rust_like_brace_counts_still_rejects_actual_raw_string_literals() {
        let mut in_block_comment = false;

        let counts = rust_like_brace_counts(
            r##"fn target() { let text = r#"{"#; }"##,
            &mut in_block_comment,
        );

        assert_eq!(counts, None);
    }

    /// Verifies structural anchor scope ranges are merged before hunk matching
    /// searches them. Nested anchors can otherwise produce overlapping ranges,
    /// causing a unique old-context position inside the overlap to be collected
    /// twice and reported as an ambiguous hunk match.
    #[test]
    fn structural_anchor_ranges_merge_nested_scope_overlaps() {
        let lines = [
            "fn outer() {",
            "    impl Thing {",
            "        fn update(&self) {",
            "            println!(\"old\");",
            "        }",
            "    }",
            "}",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();

        let ranges = structural_anchor_ranges(&lines, &[vec![0], vec![1]]);

        assert_eq!(ranges, vec![(0, 7)]);
    }
}
