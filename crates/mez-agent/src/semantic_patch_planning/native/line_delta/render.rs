//! Three-line context windows over the complete script; storage clipping never
//! feeds back into search, exact counts, hunk numbering or endpoint identity.

use super::super::{NATIVE_PATCH_DIFF_BYTES, NativeUnifiedDiff};
use super::*;
use crate::semantic_patch_planning::{ApplyPatchFileChange, ApplyPatchOriginalState};

pub(in super::super) fn diff(
    change: &ApplyPatchFileChange,
    old: &[u8],
    new: &[u8],
    requested_budget: usize,
    work: &PatchWorkControl<'_>,
) -> Result<NativeUnifiedDiff> {
    let script = Script::new(old, new, work)?;
    let mut result = NativeUnifiedDiff {
        source: String::new(),
        truncated: false,
        old_lines: script.old.len(),
        new_lines: script.new.len(),
        added_lines: 0,
        removed_lines: 0,
    };
    for run in &script.runs {
        charge(work, size_of::<Run>())?;
        match run.kind {
            Kind::Insert => result.added_lines += run.len as usize,
            Kind::Delete => result.removed_lines += run.len as usize,
            Kind::Equal => {}
        }
    }
    if result.added_lines == 0
        && result.removed_lines == 0
        && matches!(change.original, ApplyPatchOriginalState::Regular(_))
        && change.final_bytes.is_some()
    {
        work.check()?;
        return Ok(result);
    }
    let budget = requested_budget.min(NATIVE_PATCH_DIFF_BYTES);
    let label = change.path.escape_default().to_string();
    let old_label = if matches!(change.original, ApplyPatchOriginalState::Missing) {
        "/dev/null".to_owned()
    } else {
        format!("a/{label}")
    };
    let new_label = if change.final_bytes.is_none() {
        "/dev/null".to_owned()
    } else {
        format!("b/{label}")
    };
    append(
        &mut result,
        &format!("diff -- {label}\n--- {old_label}\n+++ {new_label}\n"),
        budget,
        work,
    )?;
    // Empty create/delete is a visible endpoint even without text edits.
    if script.runs.is_empty() {
        append(&mut result, "@@ -0,0 +0,0 @@\n", budget, work)?;
    }
    let (mut index, mut a, mut b, mut leading) = (0, 0, 0, 0);
    while index < script.runs.len() {
        if script.runs[index].kind == Kind::Equal {
            let len = script.runs[index].len as usize;
            a += len;
            b += len;
            leading = len.min(3);
            index += 1;
            continue;
        }
        let (start_a, start_b) = (a - leading, b - leading);
        let (mut end_a, mut end_b, mut trailing) = (a, b, 0);
        let mut end = index;
        // Include up to three context lines before/after each changed group.
        // Equal gaps <=6 join touching windows; longer gaps split them.
        while end < script.runs.len() {
            charge(work, size_of::<Run>())?;
            let run = script.runs[end];
            let len = run.len as usize;
            match run.kind {
                Kind::Delete => end_a += len,
                Kind::Insert => end_b += len,
                Kind::Equal => {
                    if end + 1 == script.runs.len() || len > 6 {
                        trailing = len.min(3);
                        end_a += trailing;
                        end_b += trailing;
                        break;
                    }
                    end_a += len;
                    end_b += len;
                }
            }
            end += 1;
        }
        let count_a = end_a - start_a;
        let count_b = end_b - start_b;
        append(
            &mut result,
            &format!(
                "@@ -{},{} +{},{} @@\n",
                start_a + usize::from(count_a > 0),
                count_a,
                start_b + usize::from(count_b > 0),
                count_b
            ),
            budget,
            work,
        )?;
        for position in start_a..a {
            emit_line(&mut result, " ", script.old.get(position), budget, work)?;
        }
        let (mut old_pos, mut new_pos) = (a, b);
        for position in index..end {
            let run = script.runs[position];
            for _ in 0..run.len {
                charge(work, 1)?;
                let (prefix, line) = match run.kind {
                    Kind::Equal => {
                        let line = script.old.get(old_pos);
                        old_pos += 1;
                        new_pos += 1;
                        (" ", line)
                    }
                    Kind::Delete => {
                        let line = script.old.get(old_pos);
                        old_pos += 1;
                        ("-", line)
                    }
                    Kind::Insert => {
                        let line = script.new.get(new_pos);
                        new_pos += 1;
                        ("+", line)
                    }
                };
                emit_line(&mut result, prefix, line, budget, work)?;
            }
        }
        for position in old_pos..old_pos + trailing {
            emit_line(&mut result, " ", script.old.get(position), budget, work)?;
        }
        if end == script.runs.len() {
            break;
        }
        let gap = script.runs[end].len as usize;
        a = end_a + gap - trailing;
        b = end_b + gap - trailing;
        leading = gap.min(3);
        index = end + 1;
    }
    work.check()?;
    Ok(result)
}

/// Indexing validated exact bytes. Revalidate/decode at most one work quantum
/// at a time; even copying a single huge line never hides an unchecked scan.
fn emit_line(
    result: &mut NativeUnifiedDiff,
    prefix: &str,
    bytes: &[u8],
    budget: usize,
    work: &PatchWorkControl<'_>,
) -> Result<()> {
    charge(work, 1)?;
    append(result, prefix, budget, work)?;
    let mut offset = 0;
    while offset < bytes.len() && !result.truncated {
        let end = bytes.len().min(offset + PATCH_WORK_QUANTUM);
        charge(work, end - offset)?;
        let fragment = match std::str::from_utf8(&bytes[offset..end]) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() => {
                std::str::from_utf8(&bytes[offset..offset + error.valid_up_to()])
                    .map_err(|_| resource("invalid UTF-8"))?
            }
            Err(_) => return Err(resource("invalid UTF-8")),
        };
        append(result, fragment, budget, work)?;
        offset += fragment.len();
    }
    if !bytes.ends_with(b"\n") {
        append(result, "\n\\ No newline at end of file\n", budget, work)?;
    }
    Ok(())
}

fn append(
    result: &mut NativeUnifiedDiff,
    fragment: &str,
    budget: usize,
    work: &PatchWorkControl<'_>,
) -> Result<()> {
    if result.truncated {
        return Ok(());
    }
    let mut end = fragment
        .len()
        .min(budget.saturating_sub(result.source.len()));
    while !fragment.is_char_boundary(end) {
        end -= 1;
    }
    charge(work, end.max(1))?;
    result
        .source
        .try_reserve(end)
        .map_err(|_| resource("preview allocation unavailable"))?;
    result.source.push_str(&fragment[..end]);
    result.truncated = end < fragment.len();
    Ok(())
}
