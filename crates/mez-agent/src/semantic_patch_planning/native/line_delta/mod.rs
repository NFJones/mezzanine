//! Exact raw-line insertion/deletion counts, independent of display retention.
//!
//! A Myers frontier finds the shortest edit distance without retaining a trace
//! or quadratic matrix. Equal prefixes/suffixes are removed first. Line owners,
//! frontier initialization, comparisons (including long lines), and diagonal
//! visits use the original action's cooperative work control. Exhaustion refuses
//! preparation before any commit; it must never fall back to inflated counts.

use super::super::work::{PATCH_WORK_QUANTUM, PatchWorkControl};
use crate::semantic_patch::SemanticPatchPlanningResult as Result;

/// Returns (added, removed) for exact raw lines, including their terminators.
/// CRLF/LF and final-newline changes are real replacements, not normalization.
pub(super) fn count(old: &str, new: &str, work: &PatchWorkControl<'_>) -> Result<(usize, usize)> {
    let old = raw_lines(old, work)?;
    let new = raw_lines(new, work)?;
    let mut prefix = 0;
    while prefix < old.len().min(new.len()) && equal(old[prefix], new[prefix], work)? {
        prefix += 1;
    }
    let (mut old, mut new) = (&old[prefix..], &new[prefix..]);
    while !old.is_empty() && !new.is_empty() && equal(old[old.len() - 1], new[new.len() - 1], work)?
    {
        old = &old[..old.len() - 1];
        new = &new[..new.len() - 1];
    }
    if old.is_empty() || new.is_empty() {
        work.check()?;
        return Ok((new.len(), old.len()));
    }
    let max = old.len() + new.len();
    let offset = max + 1;
    let capacity = 2 * max + 3;
    let mut frontier = Vec::<usize>::with_capacity(capacity);
    // Even zero initialization of the bookkeeping has bounded checkpoints.
    while frontier.len() < capacity {
        let units = (capacity - frontier.len()).min(PATCH_WORK_QUANTUM / size_of::<usize>());
        if !work.spend(units * size_of::<usize>()) {
            work.check()?;
        }
        frontier.resize(frontier.len() + units, 0);
    }
    for distance in 0..=max {
        let distance_signed = distance as isize;
        for diagonal in (-distance_signed..=distance_signed).step_by(2) {
            if !work.spend(3 * size_of::<usize>()) {
                work.check()?;
            }
            let index = (offset as isize + diagonal) as usize;
            let mut x = if diagonal == -distance_signed
                || (diagonal != distance_signed && frontier[index - 1] < frontier[index + 1])
            {
                frontier[index + 1]
            } else {
                frontier[index - 1] + 1
            };
            let mut y = (x as isize - diagonal) as usize;
            while x < old.len() && y < new.len() && equal(old[x], new[y], work)? {
                x += 1;
                y += 1;
            }
            frontier[index] = x;
            if x >= old.len() && y >= new.len() {
                work.check()?;
                let removed = (distance + old.len() - new.len()) / 2;
                return Ok((distance - removed, removed));
            }
        }
    }
    unreachable!("deleting all old lines and inserting all new lines is a path")
}

/// Preserves raw terminators rather than str::lines' CRLF normalization. Scan
/// chunks bound time between checkpoints even for a single multi-megabyte line.
fn raw_lines<'a>(text: &'a str, work: &PatchWorkControl<'_>) -> Result<Vec<&'a str>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (chunk_index, chunk) in text.as_bytes().chunks(PATCH_WORK_QUANTUM).enumerate() {
        if !work.spend(chunk.len()) {
            work.check()?;
        }
        for (index, byte) in chunk.iter().enumerate() {
            if *byte == b'\n' {
                if !work.line_allowed(lines.len()) {
                    work.check()?;
                }
                let end = chunk_index * PATCH_WORK_QUANTUM + index + 1;
                lines.push(&text[start..end]);
                start = end;
            }
        }
    }
    if start < text.len() {
        if !work.line_allowed(lines.len()) {
            work.check()?;
        }
        lines.push(&text[start..]);
    }
    work.check()?;
    Ok(lines)
}

/// Sticky failure must not look like inequality and select another edit path.
fn equal(left: &str, right: &str, work: &PatchWorkControl<'_>) -> Result<bool> {
    let equal = work.equal(left, right);
    if !work.running() {
        work.check()?;
    }
    Ok(equal)
}

#[cfg(test)]
mod tests;
