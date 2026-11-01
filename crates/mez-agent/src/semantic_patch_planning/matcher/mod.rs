//! Hunk matching and mismatch diagnostics for semantic patches.
//!
//! This module owns the current-file matching strategy for parsed Mezzanine
//! patch hunks. It keeps tolerant matching, range-hint disambiguation,
//! structural-anchor scoping, replacement construction, and model-facing
//! mismatch diagnostics out of the patch facade. Search helpers receive an
//! explicit work control and abandon scans after its sticky terminal failure;
//! the hunk owner checks that failure before generating ordinary diagnostics.

mod diagnostics;
use diagnostics::apply_patch_hunk_mismatch_error;

use super::snapshot::ApplyPatchTextFile;
use super::work::PatchWorkControl;
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
    work: &PatchWorkControl<'_>,
    path: &str,
    mut file: ApplyPatchTextFile,
    hunks: &[MezPatchHunk],
) -> Result<ApplyPatchTextFile> {
    work.check()?;
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
        file.lines = work.copy_lines(&hunk.new)?;
        return Ok(file);
    }
    let mut cursor = 0usize;
    for hunk in hunks {
        work.check()?;
        let found = find_hunk_position(work, &file, hunk, cursor);
        // A cancelled search is terminal, never an ordinary mismatch diagnostic.
        work.check()?;
        let hunk_match = match found {
            Ok(found) => found,
            Err(problem) => {
                let diagnostic = apply_patch_hunk_mismatch_error(work, path, &file, hunk, problem);
                work.check()?;
                return Err(diagnostic);
            }
        };
        let replacement = replacement_lines(work, hunk, &file, &hunk_match)?;
        let replacement_len = replacement.len();
        work.replace_lines(
            &mut file.lines,
            hunk_match.position..hunk_match.position + hunk_match.span_len(),
            replacement,
        )?;
        cursor = hunk_match.position + replacement_len;
    }
    work.check()?;
    Ok(file)
}

/// Constructs replacement text under the same work control; errors preserve the original file owner.
fn replacement_lines(
    work: &PatchWorkControl<'_>,
    hunk: &MezPatchHunk,
    file: &ApplyPatchTextFile,
    hunk_match: &ApplyPatchHunkMatch,
) -> Result<Vec<String>> {
    let mut old_index = 0usize;
    let mut next_source_offset = 0usize;
    let mut lines = Vec::new();
    let gap_policies = old_line_gap_policies(work, hunk);
    for line in &hunk.lines {
        if !work.spend(1) {
            work.check()?;
        }
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
                    work,
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
                if !work.line_allowed(lines.len()) {
                    work.check()?;
                }
                lines.push(work.copy_text(source)?);
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
                    work,
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
            MezPatchHunkLine::Add(text) => {
                if !work.line_allowed(lines.len()) {
                    work.check()?;
                }
                lines.push(work.copy_text(text)?);
            }
        }
    }
    Ok(lines)
}

/// Derives blank-context policy from bounded source lines, abandoning terminal work.
fn old_line_gap_policies(
    work: &PatchWorkControl<'_>,
    hunk: &MezPatchHunk,
) -> Vec<ApplyPatchBlankGapPolicy> {
    let mut policies = Vec::with_capacity(hunk.old.len());
    let mut previous_old_kind = None;
    for line in &hunk.lines {
        if !work.spend(1) {
            return Vec::new();
        }
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

/// Preserves only accepted blank context through controlled copies; rejects inconsistent offsets.
fn append_skipped_blank_context_lines(
    work: &PatchWorkControl<'_>,
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
        if !work.spend(1) {
            work.check()?;
        }
        let source = file.lines.get(position + offset).ok_or_else(|| {
            MezError::invalid_args("apply_patch: internal hunk replacement range was invalid")
        })?;
        if !work.trim(source, true).is_empty() {
            return Err(MezError::invalid_args(
                "apply_patch: internal hunk replacement range was invalid",
            ));
        }
        if policy == ApplyPatchBlankGapPolicy::Preserve {
            if !work.line_allowed(output.len()) {
                work.check()?;
            }
            output.push(work.copy_text(source)?);
        }
    }
    Ok(())
}

/// Maximum exact anchor positions retained for mismatch context.
const APPLY_PATCH_MISMATCH_ANCHOR_LIMIT: usize = 5;

/// Finds bounded exact anchor evidence under the same original search control.
fn apply_patch_anchor_line_numbers(
    work: &PatchWorkControl<'_>,
    lines: &[String],
    anchor: &str,
) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .take_while(|_| work.spend(1))
        .filter_map(|(index, line)| work.equal(line, anchor).then_some(index + 1))
        .take(APPLY_PATCH_MISMATCH_ANCHOR_LIMIT)
        .collect()
}

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

/// Resolves an authored hunk through shared scopes; its caller prioritizes sticky work failure.
fn find_hunk_position(
    work: &PatchWorkControl<'_>,
    file: &ApplyPatchTextFile,
    hunk: &MezPatchHunk,
    cursor: usize,
) -> std::result::Result<ApplyPatchHunkMatch, ApplyPatchHunkMatchProblem> {
    if hunk.anchors.is_empty() {
        return find_unanchored_hunk_position(
            work,
            &file.lines,
            &hunk.old,
            &old_line_gap_policies(work, hunk),
            cursor,
            hunk.range_hint,
        );
    }

    let chains = ordered_anchor_chains(work, &file.lines, &hunk.anchors, cursor);
    if chains.is_empty() {
        return Err(ApplyPatchHunkMatchProblem::Missing {
            context_center: None,
            missing_anchor: first_missing_ordered_anchor(work, &file.lines, &hunk.anchors, cursor),
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

    let structural_ranges = structural_anchor_ranges(work, &file.lines, &chains);
    if !structural_ranges.is_empty() {
        match find_hunk_position_in_ranges(
            work,
            &file.lines,
            &hunk.old,
            &old_line_gap_policies(work, hunk),
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
        work,
        &file.lines,
        &hunk.old,
        &old_line_gap_policies(work, hunk),
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

/// Builds ordered search intervals from the bounded set of accepted anchor chains.
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

/// Selects an unanchored match and context evidence without renewing work admission.
fn find_unanchored_hunk_position(
    work: &PatchWorkControl<'_>,
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
    find_unanchored_hunk_position_layered(work, lines, old, blank_gap_policies, cursor, range_hint)
        .map_err(|failure| match failure {
            ApplyPatchLineSequenceFailure::Missing {
                attempts,
                range_rejection,
                ..
            } => ApplyPatchHunkMatchProblem::Missing {
                context_center: old.first().and_then(|line| {
                    apply_patch_anchor_line_numbers(work, lines, line)
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

/// Searches scoped candidates across matching modes using the original work control.
fn find_hunk_position_in_ranges(
    work: &PatchWorkControl<'_>,
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
            let matches = find_line_sequence_matches(work, lines, old, *start, *end, *mode);
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
            work,
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

/// Preserves cursor-first tolerant matching while sharing one terminal work owner.
fn find_unanchored_hunk_position_layered(
    work: &PatchWorkControl<'_>,
    lines: &[String],
    old: &[String],
    blank_gap_policies: &[ApplyPatchBlankGapPolicy],
    cursor: usize,
    range_hint: Option<MezPatchRangeHint>,
) -> std::result::Result<ApplyPatchHunkMatch, ApplyPatchLineSequenceFailure> {
    let mut attempts = Vec::new();
    let scope = ApplyPatchSearchScope::FullFile;
    for mode in APPLY_PATCH_MATCH_MODES {
        let mut matches = find_line_sequence_matches(work, lines, old, cursor, lines.len(), *mode);
        if matches.lines.is_empty() && cursor > 0 {
            matches = find_line_sequence_matches(work, lines, old, 0, lines.len(), *mode);
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
            work,
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

/// Derives merged structural scopes from bounded chains under original scan admission.
fn structural_anchor_ranges(
    work: &PatchWorkControl<'_>,
    lines: &[String],
    chains: &[Vec<usize>],
) -> Vec<(usize, usize)> {
    let ranges = chains
        .iter()
        .filter_map(|chain| {
            let anchor_line = chain.last().copied()?;
            rust_like_block_scope(work, lines, anchor_line)
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

/// Scans a bounded structural region; malformed syntax or terminal work abandons scope inference.
fn rust_like_block_scope(
    work: &PatchWorkControl<'_>,
    lines: &[String],
    anchor_line: usize,
) -> Option<(usize, usize)> {
    let anchor_text = lines.get(anchor_line)?;
    if !looks_like_rust_structural_anchor(work, anchor_text) {
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
        let (opens, closes) = rust_like_brace_counts(work, line, &mut in_block_comment)?;
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

/// Recognizes structural prefixes with controlled leading-whitespace scans.
fn looks_like_rust_structural_anchor(work: &PatchWorkControl<'_>, line: &str) -> bool {
    let line = work.trim_start(line);
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

/// Counts supported Rust-like braces with cooperative character scans; uncertain literals return None.
fn rust_like_brace_counts(
    work: &PatchWorkControl<'_>,
    line: &str,
    in_block_comment: &mut bool,
) -> Option<(usize, usize)> {
    let mut opens = 0usize;
    let mut closes = 0usize;
    let mut chars = line.chars().peekable();
    let mut in_string = false;
    let mut in_char = false;
    let mut escaped = false;
    while let Some(character) = chars.next() {
        if !work.spend(character.len_utf8()) {
            return None;
        }
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
            'r' if rust_like_raw_string_literal_start(work, &mut chars) => return None,
            '"' => in_string = true,
            '\'' => in_char = true,
            '{' => opens += 1,
            '}' => closes += 1,
            _ => {}
        }
    }
    Some((opens, closes))
}

/// Detects raw literal prefixes while bounding repeated hash-marker scans.
fn rust_like_raw_string_literal_start(
    work: &PatchWorkControl<'_>,
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> bool {
    let mut clone = chars.clone();
    if clone.peek() == Some(&'"') {
        return true;
    }
    let mut saw_hash = false;
    while clone.peek() == Some(&'#') {
        if !work.spend(1) {
            return false;
        }
        saw_hash = true;
        clone.next();
    }
    saw_hash && clone.peek() == Some(&'"')
}

/// Finds ordered literal anchors with cooperative linear substring scans and a fixed candidate cap.
fn ordered_anchor_chains(
    work: &PatchWorkControl<'_>,
    lines: &[String],
    anchors: &[String],
    cursor: usize,
) -> Vec<Vec<usize>> {
    if anchors.is_empty() {
        return vec![Vec::new()];
    }

    let mut chains = Vec::new();
    for (index, line) in lines.iter().enumerate().skip(cursor.min(lines.len())) {
        if !work.spend(1) {
            return Vec::new();
        }
        if !work.contains(line, &anchors[0]) {
            continue;
        }
        let mut chain = vec![index];
        let mut next_start = index + 1;
        let mut complete = true;
        for anchor in &anchors[1..] {
            if !work.spend(1) {
                return Vec::new();
            }
            if let Some((next_index, _)) = lines
                .iter()
                .enumerate()
                .skip(next_start)
                .take_while(|_| work.spend(1))
                .find(|(_, line)| work.contains(line, anchor))
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

/// Finds the first unavailable ordered anchor under the original search control.
fn first_missing_ordered_anchor(
    work: &PatchWorkControl<'_>,
    lines: &[String],
    anchors: &[String],
    cursor: usize,
) -> Option<String> {
    let mut next_start = cursor.min(lines.len());
    for anchor in anchors {
        if !work.spend(1) {
            return None;
        }
        if let Some((next_index, _)) = lines
            .iter()
            .enumerate()
            .skip(next_start)
            .take_while(|_| work.spend(1))
            .find(|(_, line)| work.contains(line, anchor))
        {
            next_start = next_index + 1;
        } else {
            return Some(anchor.clone());
        }
    }
    None
}

/// Scans candidate starts under finite byte/visit admission, abandoning terminal work.
fn find_line_sequence_matches(
    work: &PatchWorkControl<'_>,
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
        if !work.spend(1) {
            break;
        }
        if line_sequence_matches(work, &lines[index..index + needle.len()], needle, mode) {
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

/// Searches allowed blank-context gaps without changing normal candidate ambiguity rules.
fn find_line_sequence_matches_omitting_blank_context(
    work: &PatchWorkControl<'_>,
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
            if !work.spend(1) {
                break;
            }
            if let Some(match_result) = line_sequence_match_omitting_blank_context_at(
                work,
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

/// Checks one tolerant candidate and its exact retained source offsets under work admission.
fn line_sequence_match_omitting_blank_context_at(
    work: &PatchWorkControl<'_>,
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
        if !work.spend(1) {
            return None;
        }
        if actual_index >= end {
            return None;
        }
        if patch_line_matches(work, &lines[actual_index], expected, mode) {
            old_line_offsets.push(actual_index.saturating_sub(position));
            actual_index += 1;
            continue;
        }
        if blank_gap_policies[needle_index] == ApplyPatchBlankGapPolicy::Disallow {
            return None;
        }
        let blank_start = actual_index;
        while actual_index < end
            && work.running()
            && work.trim(&lines[actual_index], true).is_empty()
        {
            if !work.spend(1) {
                return None;
            }
            actual_index += 1;
        }
        if actual_index == blank_start || actual_index >= end {
            return None;
        }
        if !patch_line_matches(work, &lines[actual_index], expected, mode) {
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

/// Compares candidate lines with short-circuiting terminal work propagation.
fn line_sequence_matches(
    work: &PatchWorkControl<'_>,
    actual: &[String],
    expected: &[String],
    mode: ApplyPatchMatchMode,
) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| patch_line_matches(work, actual, expected, mode))
}

/// Applies the selected whitespace/Unicode policy through controlled long-line comparisons.
fn patch_line_matches(
    work: &PatchWorkControl<'_>,
    actual: &str,
    expected: &str,
    mode: ApplyPatchMatchMode,
) -> bool {
    if !work.spend(1) {
        return false;
    }
    match mode {
        ApplyPatchMatchMode::Exact => work.equal(actual, expected),
        ApplyPatchMatchMode::TrimEnd => {
            work.equal(work.trim(actual, false), work.trim(expected, false))
        }
        ApplyPatchMatchMode::Trim => work.equal(work.trim(actual, true), work.trim(expected, true)),
        ApplyPatchMatchMode::Normalized => work.equal(
            &normalized_patch_line(work, actual),
            &normalized_patch_line(work, expected),
        ),
    }
}

/// Normalizes the established Unicode substitutions with cooperative character processing.
fn normalized_patch_line(work: &PatchWorkControl<'_>, line: &str) -> String {
    work.trim(line, true)
        .chars()
        .take_while(|character| work.spend(character.len_utf8()))
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

#[cfg(test)]
mod tests;
