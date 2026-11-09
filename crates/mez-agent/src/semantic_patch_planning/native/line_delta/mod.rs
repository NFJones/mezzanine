//! One exact raw-line edit script for both receipt statistics and preview.
//!
//! Iterative Myers bidirectional bisection retains no frontier history or matrix.
//! Borrowed byte spans preserve terminators; UTF-8 validation and long-line scans
//! are cooperative. All scratch is fallibly reserved once, checked against 32 MiB,
//! and released per endpoint. A finite 4096-task DFS stack refuses pathological
//! subdivision rather than recursing or approximating. Original work exhaustion
//! is sticky and preparation must finish before any mutation.

use super::super::work::{NATIVE_PATCH_TEXT_LINES, PATCH_WORK_QUANTUM, PatchWorkControl};
use crate::semantic_patch::{SemanticPatchPlanningError, SemanticPatchPlanningResult as Result};

mod render;
pub(super) use render::diff;

const SCRATCH_BYTES: usize = 32 * 1024 * 1024;
const STACK_TASKS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Equal,
    Delete,
    Insert,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    kind: Kind,
    len: u32,
}

#[derive(Clone, Copy)]
struct Region {
    a: u32,
    end_a: u32,
    b: u32,
    end_b: u32,
}

#[derive(Clone, Copy)]
enum Task {
    Diff(Region),
    Equal(u32),
}

/// Compact offsets avoid copying/normalizing line text or hashing authority.
struct Lines<'a> {
    bytes: &'a [u8],
    ends: Vec<u32>,
}
impl<'a> Lines<'a> {
    fn new(bytes: &'a [u8], work: &PatchWorkControl<'_>, scratch: &mut usize) -> Result<Self> {
        let mut count = 0;
        for chunk in bytes.chunks(PATCH_WORK_QUANTUM) {
            charge(work, chunk.len())?;
            count += chunk.iter().filter(|byte| **byte == b'\n').count();
        }
        count += usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"));
        if count > NATIVE_PATCH_TEXT_LINES {
            return Err(resource("text line budget exceeded"));
        }
        let mut ends = reserved::<u32>(count + 1, scratch)?;
        ends.push(0);
        let mut offset = 0;
        while offset < bytes.len() {
            let mut end = bytes.len().min(offset + PATCH_WORK_QUANTUM);
            charge(work, end - offset)?;
            match std::str::from_utf8(&bytes[offset..end]) {
                Ok(_) => {}
                Err(error) if error.error_len().is_none() && end < bytes.len() => {
                    end = offset + error.valid_up_to();
                }
                Err(_) => return Err(resource("invalid UTF-8")),
            }
            for (index, byte) in bytes[offset..end].iter().enumerate() {
                if *byte == b'\n' {
                    charge(work, size_of::<u32>())?;
                    ends.push((offset + index + 1) as u32);
                }
            }
            offset = end;
        }
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            charge(work, size_of::<u32>())?;
            ends.push(bytes.len() as u32);
        }
        work.check()?;
        Ok(Self { bytes, ends })
    }
    fn len(&self) -> usize {
        self.ends.len() - 1
    }
    fn get(&self, index: usize) -> &[u8] {
        &self.bytes[self.ends[index] as usize..self.ends[index + 1] as usize]
    }
}

struct Script<'a> {
    old: Lines<'a>,
    new: Lines<'a>,
    runs: Vec<Run>,
}
impl<'a> Script<'a> {
    fn new(old: &'a [u8], new: &'a [u8], work: &PatchWorkControl<'_>) -> Result<Self> {
        Self::with_limits(old, new, work, SCRATCH_BYTES, STACK_TASKS)
    }

    /// Private limits permit deterministic exhaustion tests without new knobs.
    fn with_limits(
        old: &'a [u8],
        new: &'a [u8],
        work: &PatchWorkControl<'_>,
        scratch_limit: usize,
        stack_limit: usize,
    ) -> Result<Self> {
        let result = Self::build(old, new, work, scratch_limit, stack_limit);
        if let Err(error) = &result {
            work.stop(error.clone());
        }
        result
    }

    fn build(
        old: &'a [u8],
        new: &'a [u8],
        work: &PatchWorkControl<'_>,
        scratch_limit: usize,
        stack_limit: usize,
    ) -> Result<Self> {
        work.check()?;
        let mut scratch = scratch_limit.min(SCRATCH_BYTES);
        let old = Lines::new(old, work, &mut scratch)?;
        let new = Lines::new(new, work, &mut scratch)?;
        let total = old
            .len()
            .checked_add(new.len())
            .ok_or_else(|| resource("scratch size overflow"))?;
        let runs = reserved::<Run>(total + 1, &mut scratch)?;
        let mut tasks = reserved::<Task>(stack_limit.min(STACK_TASKS), &mut scratch)?;
        let frontier_len = total + 4;
        let mut forward = reserved::<i32>(frontier_len, &mut scratch)?;
        let mut reverse = reserved::<i32>(frontier_len, &mut scratch)?;
        let mut script = Self { old, new, runs };
        push(
            &mut tasks,
            Task::Diff(Region {
                a: 0,
                end_a: script.old.len() as u32,
                b: 0,
                end_b: script.new.len() as u32,
            }),
            work,
        )?;
        while let Some(task) = tasks.pop() {
            charge(work, size_of::<Task>())?;
            let Task::Diff(mut region) = task else {
                let Task::Equal(len) = task else {
                    unreachable!()
                };
                script.emit(Kind::Equal, len, work)?;
                continue;
            };
            let mut prefix = 0;
            while region.a < region.end_a
                && region.b < region.end_b
                && equal(
                    script.old.get(region.a as usize),
                    script.new.get(region.b as usize),
                    work,
                )?
            {
                region.a += 1;
                region.b += 1;
                prefix += 1;
            }
            script.emit(Kind::Equal, prefix, work)?;
            let mut suffix = 0;
            while region.a < region.end_a
                && region.b < region.end_b
                && equal(
                    script.old.get(region.end_a as usize - 1),
                    script.new.get(region.end_b as usize - 1),
                    work,
                )?
            {
                region.end_a -= 1;
                region.end_b -= 1;
                suffix += 1;
            }
            if suffix > 0 {
                push(&mut tasks, Task::Equal(suffix), work)?;
            }
            if region.a == region.end_a {
                script.emit(Kind::Insert, region.end_b - region.b, work)?;
            } else if region.b == region.end_b {
                script.emit(Kind::Delete, region.end_a - region.a, work)?;
            } else {
                let (x, y) = script.bisect(region, &mut forward, &mut reverse, work)?;
                if (x == region.a && y == region.b) || (x == region.end_a && y == region.end_b) {
                    return Err(resource("nonshrinking edit subdivision"));
                }
                push(
                    &mut tasks,
                    Task::Diff(Region {
                        a: x,
                        b: y,
                        ..region
                    }),
                    work,
                )?;
                push(
                    &mut tasks,
                    Task::Diff(Region {
                        end_a: x,
                        end_b: y,
                        ..region
                    }),
                    work,
                )?;
            }
        }
        work.check()?;
        Ok(script)
    }

    fn emit(&mut self, kind: Kind, len: u32, work: &PatchWorkControl<'_>) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        charge(work, size_of::<Run>())?;
        if let Some(last) = self.runs.last_mut()
            && last.kind == kind
        {
            last.len += len;
        } else {
            if self.runs.len() == self.runs.capacity() {
                return Err(resource("run budget exceeded"));
            }
            self.runs.push(Run { kind, len });
        }
        Ok(())
    }

    /// Furthest-reaching frontiers meet on a shortest path. Equal choices use
    /// the deletion predecessor (strict < selects insertion), in stable diagonal
    /// order. Reverse traversal uses the same rule. No randomized matching.
    fn bisect(
        &self,
        r: Region,
        forward: &mut Vec<i32>,
        reverse: &mut Vec<i32>,
        work: &PatchWorkControl<'_>,
    ) -> Result<(u32, u32)> {
        let n = (r.end_a - r.a) as i32;
        let m = (r.end_b - r.b) as i32;
        let max = (n + m + 1) / 2;
        let offset = max + 1;
        let length = (2 * max + 3) as usize;
        reset(forward, length, work)?;
        reset(reverse, length, work)?;
        forward[(offset + 1) as usize] = 0;
        reverse[(offset + 1) as usize] = 0;
        let delta = n - m;
        let odd = delta % 2 != 0;
        let (mut fstart, mut fend, mut rstart, mut rend) = (0, 0, 0, 0);
        for d in 0..=max {
            for k in ((-d + fstart)..=(d - fend)).step_by(2) {
                charge(work, 3 * size_of::<i32>())?;
                let index = (offset + k) as usize;
                let mut x = if k == -d || (k != d && forward[index - 1] < forward[index + 1]) {
                    forward[index + 1]
                } else {
                    forward[index - 1] + 1
                };
                let mut y = x - k;
                while x < n
                    && y < m
                    && equal(
                        self.old.get((r.a as i32 + x) as usize),
                        self.new.get((r.b as i32 + y) as usize),
                        work,
                    )?
                {
                    x += 1;
                    y += 1;
                }
                forward[index] = x;
                if x > n {
                    fend += 2;
                } else if y > m {
                    fstart += 2;
                } else if odd {
                    let other = offset + delta - k;
                    if other >= 0
                        && (other as usize) < length
                        && reverse[other as usize] >= 0
                        && x >= n - reverse[other as usize]
                    {
                        return Ok((r.a + x as u32, r.b + y as u32));
                    }
                }
            }
            for k in ((-d + rstart)..=(d - rend)).step_by(2) {
                charge(work, 3 * size_of::<i32>())?;
                let index = (offset + k) as usize;
                let mut x = if k == -d || (k != d && reverse[index - 1] < reverse[index + 1]) {
                    reverse[index + 1]
                } else {
                    reverse[index - 1] + 1
                };
                let mut y = x - k;
                while x < n
                    && y < m
                    && equal(
                        self.old.get((r.end_a as i32 - x - 1) as usize),
                        self.new.get((r.end_b as i32 - y - 1) as usize),
                        work,
                    )?
                {
                    x += 1;
                    y += 1;
                }
                reverse[index] = x;
                if x > n {
                    rend += 2;
                } else if y > m {
                    rstart += 2;
                } else if !odd {
                    let other = offset + delta - k;
                    if other >= 0
                        && (other as usize) < length
                        && forward[other as usize] >= 0
                        && forward[other as usize] >= n - x
                    {
                        let x = forward[other as usize];
                        return Ok((r.a + x as u32, r.b + (x - delta + k) as u32));
                    }
                }
            }
        }
        Err(resource("edit frontiers did not meet"))
    }
}

fn reset(frontier: &mut Vec<i32>, length: usize, work: &PatchWorkControl<'_>) -> Result<()> {
    if length > frontier.capacity() {
        return Err(resource("frontier budget exceeded"));
    }
    frontier.clear();
    while frontier.len() < length {
        let count = (length - frontier.len()).min(PATCH_WORK_QUANTUM / size_of::<i32>());
        charge(work, count * size_of::<i32>())?;
        frontier.resize(frontier.len() + count, -1);
    }
    Ok(())
}

fn push(tasks: &mut Vec<Task>, task: Task, work: &PatchWorkControl<'_>) -> Result<()> {
    charge(work, size_of::<Task>())?;
    if tasks.len() == tasks.capacity() {
        return Err(resource("task stack budget exceeded"));
    }
    tasks.push(task);
    Ok(())
}

fn reserved<T>(capacity: usize, remaining: &mut usize) -> Result<Vec<T>> {
    let bytes = capacity
        .checked_mul(size_of::<T>())
        .ok_or_else(|| resource("scratch size overflow"))?;
    *remaining = remaining
        .checked_sub(bytes)
        .ok_or_else(|| resource("scratch budget exceeded"))?;
    let mut result = Vec::new();
    result
        .try_reserve_exact(capacity)
        .map_err(|_| resource("scratch allocation unavailable"))?;
    let extra = result
        .capacity()
        .checked_sub(capacity)
        .and_then(|count| count.checked_mul(size_of::<T>()))
        .ok_or_else(|| resource("scratch size overflow"))?;
    *remaining = remaining
        .checked_sub(extra)
        .ok_or_else(|| resource("scratch budget exceeded"))?;
    Ok(result)
}

fn charge(work: &PatchWorkControl<'_>, units: usize) -> Result<()> {
    if !work.spend(units) {
        work.check()?;
    }
    Ok(())
}

fn equal(left: &[u8], right: &[u8], work: &PatchWorkControl<'_>) -> Result<bool> {
    charge(work, 1)?;
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left
        .chunks(PATCH_WORK_QUANTUM)
        .zip(right.chunks(PATCH_WORK_QUANTUM))
    {
        charge(work, left.len())?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

fn resource(message: &str) -> SemanticPatchPlanningError {
    SemanticPatchPlanningError::invalid_args(format!("apply_patch: native diff {message}"))
}

#[cfg(test)]
fn count(old: &str, new: &str, work: &PatchWorkControl<'_>) -> Result<(usize, usize)> {
    let script = Script::new(old.as_bytes(), new.as_bytes(), work)?;
    Ok(script
        .runs
        .iter()
        .fold((0, 0), |(a, d), run| match run.kind {
            Kind::Insert => (a + run.len as usize, d),
            Kind::Delete => (a, d + run.len as usize),
            Kind::Equal => (a, d),
        }))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod focused_tests;
