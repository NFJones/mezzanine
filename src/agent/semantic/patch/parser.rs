//! Parser for model-authored Mezzanine patch blocks.
//!
//! This module owns only source-text normalization and parsing into the typed
//! patch representation consumed by the matcher and shell-transaction planner
//! in the parent module. It intentionally performs no filesystem reads or hunk
//! matching so parse failures remain deterministic and side-effect free.

use crate::error::Result;
use std::borrow::Cow;
use std::collections::BTreeSet;

use super::apply_patch_parse_error;

/// Parsed representation of one Mezzanine patch block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MezPatch {
    /// Ordered operations from the patch body.
    pub(super) operations: Vec<MezPatchOperation>,
}

impl MezPatch {
    /// Returns the sorted set of relative paths touched by this patch.
    pub(super) fn touched_paths(&self) -> BTreeSet<String> {
        let mut paths = BTreeSet::new();
        for operation in &self.operations {
            match operation {
                MezPatchOperation::Add { path, .. }
                | MezPatchOperation::Delete { path }
                | MezPatchOperation::Update { path, .. } => {
                    paths.insert(path.clone());
                }
            }
            if let MezPatchOperation::Update {
                move_to: Some(move_to),
                ..
            } = operation
            {
                paths.insert(move_to.clone());
            }
        }
        paths
    }
}

/// One file operation parsed from a Mezzanine patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MezPatchOperation {
    /// Add a new file with the provided lines.
    Add {
        /// Relative path to create.
        path: String,
        /// File content lines without trailing newline markers.
        content: Vec<String>,
    },
    /// Delete an existing file.
    Delete {
        /// Relative path to remove.
        path: String,
    },
    /// Update an existing file, optionally moving it afterward.
    Update {
        /// Relative path to patch.
        path: String,
        /// Optional relative destination path.
        move_to: Option<String>,
        /// Parsed hunks to apply in order.
        hunks: Vec<MezPatchHunk>,
        /// Optional final trailing-newline override.
        trailing_newline: Option<bool>,
    },
}

/// One update hunk parsed from a Mezzanine patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MezPatchHunk {
    /// Optional ordered header anchors.
    pub(super) anchors: Vec<String>,
    /// Optional unified-diff old-line hint.
    pub(super) range_hint: Option<MezPatchRangeHint>,
    /// Raw hunk lines in parsed form.
    pub(super) lines: Vec<MezPatchHunkLine>,
    /// Old-side lines that must match the current file.
    pub(super) old: Vec<String>,
    /// New-side replacement lines.
    pub(super) new: Vec<String>,
}

/// Old-side line hint parsed from a unified hunk header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MezPatchRangeHint {
    /// One-based old-file start line from the hunk header.
    pub(super) old_start: usize,
}

/// One parsed hunk line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MezPatchHunkLine {
    /// Context line that must be preserved.
    Context(String),
    /// Old line that must be removed.
    Remove(String),
    /// New line that must be inserted.
    Add(String),
}

/// Parses one model-authored Mezzanine patch block.
///
/// # Parameters
/// - `text`: The raw patch text, optionally wrapped in a Markdown fence or
///   heredoc shell snippet.
pub(super) fn parse_mez_patch(text: &str) -> Result<MezPatch> {
    let text = normalize_mez_patch_text(text);
    let lines = text.lines().collect::<Vec<_>>();
    let mut index = 0usize;
    while index < lines.len() && lines[index].trim().is_empty() {
        index += 1;
    }
    if lines.get(index).map(|line| line.trim()) != Some("*** Begin Patch") {
        return apply_patch_parse_error("Mezzanine patch must start with *** Begin Patch");
    }
    index += 1;
    let mut operations = Vec::new();
    while index < lines.len() {
        let line = lines[index].trim();
        if line == "*** End Patch" {
            index += 1;
            break;
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            let path = clean_mez_patch_path(path)?;
            index += 1;
            let mut content = Vec::new();
            while index < lines.len() && !is_mez_patch_directive_line(lines[index]) {
                let Some(line) = lines[index].strip_prefix('+') else {
                    return apply_patch_parse_error("add-file lines must start with +");
                };
                content.push(line.to_string());
                index += 1;
            }
            operations.push(MezPatchOperation::Add { path, content });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            operations.push(MezPatchOperation::Delete {
                path: clean_mez_patch_path(path)?,
            });
            index += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            let path = clean_mez_patch_path(path)?;
            index += 1;
            let mut move_to = None;
            if let Some(target) = lines
                .get(index)
                .map(|line| line.trim())
                .and_then(|line| line.strip_prefix("*** Move to: "))
            {
                move_to = Some(clean_mez_patch_path(target)?);
                index += 1;
            }
            let mut hunks = Vec::new();
            let mut trailing_newline = None;
            while index < lines.len() && !is_mez_patch_directive_line(lines[index]) {
                if lines[index].trim().is_empty() {
                    index += 1;
                    continue;
                }
                let (next_index, hunk, hunk_trailing_newline) =
                    parse_mez_patch_hunk(&lines, index, hunks.is_empty())?;
                if let Some(value) = hunk_trailing_newline {
                    trailing_newline = Some(value);
                }
                hunks.push(hunk);
                index = next_index;
            }
            if hunks.is_empty() {
                return apply_patch_parse_error(&format!(
                    "update-file operation for path {path} must contain at least one hunk"
                ));
            }
            operations.push(MezPatchOperation::Update {
                path,
                move_to,
                hunks,
                trailing_newline,
            });
            continue;
        }
        return apply_patch_parse_error(&format!("unsupported patch directive: {line}"));
    }
    if lines.get(index.saturating_sub(1)).map(|line| line.trim()) != Some("*** End Patch") {
        return apply_patch_parse_error("Mezzanine patch must end with *** End Patch");
    }
    if lines[index..].iter().any(|line| !line.trim().is_empty()) {
        return apply_patch_parse_error("unexpected content after *** End Patch");
    }
    if operations.is_empty() {
        return apply_patch_parse_error("Mezzanine patch must contain at least one file operation");
    }
    Ok(MezPatch { operations })
}

pub fn try_convert_unified_diff_to_mez_patch(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.is_empty() {
        return None;
    }
    let has_diff_header = lines
        .iter()
        .any(|line| line.starts_with("--- ") || *line == "---")
        && lines
            .iter()
            .any(|line| line.starts_with("+++ ") || *line == "+++");
    let has_hunk = lines.iter().any(|line| line.starts_with("@@"));
    if !has_diff_header || !has_hunk {
        return None;
    }
    if trimmed.contains("*** Begin Patch") || trimmed.contains("*** Update File") {
        return None;
    }
    let sections = split_unified_diff_into_file_sections(&lines);
    if sections.is_empty() || sections.iter().all(|section| section.hunks.is_empty()) {
        return None;
    }
    let mut result = String::from("*** Begin Patch\n");
    for section in &sections {
        let path = if let Some(path) = &section.path {
            path.clone()
        } else {
            continue;
        };
        match section.operation {
            UnifiedDiffFileOperation::Add => {
                result.push_str("*** Add File: ");
                result.push_str(&path);
                result.push('\n');
                for hunk in &section.hunks {
                    for line in hunk {
                        if let Some(content) = line.strip_prefix('+') {
                            result.push('+');
                            result.push_str(content);
                            result.push('\n');
                        }
                    }
                }
            }
            UnifiedDiffFileOperation::Delete => {
                result.push_str("*** Delete File: ");
                result.push_str(&path);
                result.push('\n');
            }
            UnifiedDiffFileOperation::Update => {
                result.push_str("*** Update File: ");
                result.push_str(&path);
                result.push('\n');
                for hunk in &section.hunks {
                    for line in hunk {
                        result.push_str(line);
                        result.push('\n');
                    }
                }
            }
        }
    }
    result.push_str("*** End Patch\n");
    if result.lines().count() <= 3 {
        return None;
    }
    Some(result)
}

#[derive(Debug)]
struct UnifiedDiffFileSection {
    path: Option<String>,
    operation: UnifiedDiffFileOperation,
    hunks: Vec<Vec<String>>,
}

#[derive(Debug, PartialEq, Eq)]
enum UnifiedDiffFileOperation {
    Add,
    Delete,
    Update,
}

fn split_unified_diff_into_file_sections(lines: &[&str]) -> Vec<UnifiedDiffFileSection> {
    let mut sections = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        while index < lines.len()
            && !lines[index].starts_with("--- ")
            && lines[index] != "---"
            && !lines[index].starts_with("diff --git ")
        {
            index += 1;
        }
        if index >= lines.len() {
            break;
        }
        let section_start = index;
        while index < lines.len() && !lines[index].starts_with("@@") {
            index += 1;
        }
        if index >= lines.len() {
            index = section_start + 1;
            continue;
        }
        let header_lines = &lines[section_start..index];
        let (path, operation) = parse_unified_diff_file_header(header_lines);
        let hunk_start = index;
        while index < lines.len() {
            let line = lines[index];
            if line.starts_with("--- ") || line == "---" || line.starts_with("diff --git ") {
                break;
            }
            index += 1;
        }
        let hunks = split_into_unified_hunk_blocks(&lines[hunk_start..index]);
        if !hunks.is_empty() {
            sections.push(UnifiedDiffFileSection {
                path,
                operation,
                hunks,
            });
        }
    }
    sections
}

fn parse_unified_diff_file_header(
    header_lines: &[&str],
) -> (Option<String>, UnifiedDiffFileOperation) {
    let mut path: Option<String> = None;
    let mut operation = UnifiedDiffFileOperation::Update;
    for line in header_lines {
        if line.starts_with("new file") {
            operation = UnifiedDiffFileOperation::Add;
        } else if line.starts_with("deleted file") {
            operation = UnifiedDiffFileOperation::Delete;
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            let raw = if let Some(stripped) = rest.trim().strip_prefix("b/") {
                stripped
            } else if let Some(stripped) = rest.trim().strip_prefix("a/") {
                stripped
            } else {
                rest.trim()
            };
            if raw != "/dev/null" {
                path = Some(raw.to_string());
            }
        } else if path.is_none()
            && let Some(rest) = line.strip_prefix("--- ")
        {
            let raw = if let Some(stripped) = rest.trim().strip_prefix("a/") {
                stripped
            } else if let Some(stripped) = rest.trim().strip_prefix("b/") {
                stripped
            } else {
                rest.trim()
            };
            if raw != "/dev/null" {
                path = Some(raw.to_string());
            }
        }
    }
    (path, operation)
}

fn split_into_unified_hunk_blocks(lines: &[&str]) -> Vec<Vec<String>> {
    let mut hunks = Vec::new();
    let mut current = Vec::new();
    for line in lines {
        if line.starts_with("@@") {
            if !current.is_empty() {
                hunks.push(std::mem::take(&mut current));
            }
            current.push((*line).to_string());
        } else if !current.is_empty() {
            current.push((*line).to_string());
        }
    }
    if !current.is_empty() {
        hunks.push(current);
    }
    hunks
}

fn normalize_mez_patch_text(text: &str) -> Cow<'_, str> {
    let trimmed = text.trim();
    if let Some(converted) = try_convert_unified_diff_to_mez_patch(trimmed) {
        return Cow::Owned(converted);
    }
    if let Some(fenced) = mez_patch_fenced_body(trimmed) {
        if let Some(heredoc) = mez_patch_heredoc_body(&fenced) {
            return Cow::Owned(dedent_mez_patch_text(&heredoc).into_owned());
        }
        return Cow::Owned(dedent_mez_patch_text(&fenced).into_owned());
    }
    if let Some(heredoc) = mez_patch_heredoc_body(trimmed) {
        return Cow::Owned(dedent_mez_patch_text(&heredoc).into_owned());
    }
    dedent_mez_patch_text(text)
}

fn dedent_mez_patch_text(text: &str) -> Cow<'_, str> {
    let Some(first_nonblank) = text.lines().find(|line| !line.trim().is_empty()) else {
        return Cow::Borrowed(text);
    };
    let trimmed_first = first_nonblank.trim_start();
    if trimmed_first != "*** Begin Patch" || trimmed_first == first_nonblank {
        return Cow::Borrowed(text);
    }
    let indent = &first_nonblank[..first_nonblank.len() - trimmed_first.len()];
    let mut dedented = String::with_capacity(text.len());
    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            dedented.push('\n');
        }
        if let Some(stripped) = line.strip_prefix(indent) {
            dedented.push_str(stripped);
        } else {
            dedented.push_str(line);
        }
    }
    if text.ends_with('\n') {
        dedented.push('\n');
    }
    Cow::Owned(dedented)
}

fn mez_patch_fenced_body(trimmed: &str) -> Option<String> {
    let newline = trimmed.find('\n')?;
    let first_line = trimmed[..newline].trim();
    if !first_line.starts_with("```") {
        return None;
    }
    let body = &trimmed[newline + 1..];
    let mut body_lines = body.lines().collect::<Vec<_>>();
    if body_lines.last().is_some_and(|line| line.trim() == "```") {
        body_lines.pop();
        return Some(trim_mez_patch_wrapper_body(&body_lines));
    }
    None
}

fn mez_patch_heredoc_body(trimmed: &str) -> Option<String> {
    let newline = trimmed.find('\n')?;
    let first_line = trimmed[..newline].trim();
    let delimiter = mez_patch_heredoc_delimiter(first_line)?;
    let body = &trimmed[newline + 1..];
    let mut body_lines = body.lines().collect::<Vec<_>>();
    if body_lines
        .last()
        .is_some_and(|line| line.trim() == delimiter)
    {
        body_lines.pop();
        return Some(trim_mez_patch_wrapper_body(&body_lines));
    }
    None
}

fn trim_mez_patch_wrapper_body(lines: &[&str]) -> String {
    let mut start = 0usize;
    let mut end = lines.len();
    while start < end && lines[start].trim().is_empty() {
        start += 1;
    }
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    lines[start..end].join("\n")
}

fn mez_patch_heredoc_delimiter(line: &str) -> Option<&str> {
    let line = line.trim();
    let redirect = if let Some(redirect) = line.strip_prefix("<<") {
        redirect
    } else {
        line.strip_prefix("apply_patch")?
            .trim_start()
            .strip_prefix("<<")?
    };
    let delimiter = redirect.strip_prefix('-').unwrap_or(redirect).trim();
    let delimiter = delimiter
        .strip_prefix('\'')
        .and_then(|delimiter| delimiter.strip_suffix('\''))
        .or_else(|| {
            delimiter
                .strip_prefix('"')
                .and_then(|delimiter| delimiter.strip_suffix('"'))
        })
        .unwrap_or(delimiter);
    (!delimiter.is_empty()
        && delimiter
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_'))
    .then_some(delimiter)
}

fn is_mez_patch_directive_line(line: &str) -> bool {
    matches!(line.trim(), "*** End Patch" | "*** End of File")
        || line.trim().starts_with("*** Add File: ")
        || line.trim().starts_with("*** Delete File: ")
        || line.trim().starts_with("*** Update File: ")
        || line.trim().starts_with("*** Move to: ")
}

fn parse_mez_patch_hunk(
    lines: &[&str],
    mut index: usize,
    allow_missing_header: bool,
) -> Result<(usize, MezPatchHunk, Option<bool>)> {
    if index >= lines.len() {
        return apply_patch_parse_error("expected hunk header");
    }
    let (anchors, range_hint) = if is_mez_hunk_header_line(lines[index]) {
        let (anchors, range_hint) = parse_mez_patch_hunk_header(lines[index].trim());
        index += 1;
        (anchors, range_hint)
    } else if allow_missing_header {
        (Vec::new(), None)
    } else {
        return apply_patch_parse_error("expected hunk header");
    };
    let mut hunk_lines = Vec::new();
    let mut old = Vec::new();
    let mut new = Vec::new();
    let mut trailing_newline = None;
    while index < lines.len() {
        let line = lines[index];
        if is_mez_hunk_header_line(line)
            || (is_mez_patch_directive_line(line) && line.trim() != "*** End of File")
        {
            break;
        }
        if line.trim() == "*** End of File" {
            trailing_newline = Some(false);
            index += 1;
            continue;
        }
        let Some(prefix) = line.chars().next() else {
            old.push(String::new());
            new.push(String::new());
            hunk_lines.push(MezPatchHunkLine::Context(String::new()));
            index += 1;
            continue;
        };
        let content = &line[prefix.len_utf8()..];
        match prefix {
            ' ' => {
                old.push(content.to_string());
                new.push(content.to_string());
                hunk_lines.push(MezPatchHunkLine::Context(content.to_string()));
            }
            '-' => {
                old.push(content.to_string());
                hunk_lines.push(MezPatchHunkLine::Remove(content.to_string()));
            }
            '+' => {
                new.push(content.to_string());
                hunk_lines.push(MezPatchHunkLine::Add(content.to_string()));
            }
            _ => return apply_patch_parse_error("patch hunk lines must start with space, +, or -"),
        }
        index += 1;
    }
    if hunk_lines.is_empty() {
        return apply_patch_parse_error("update hunk does not contain any lines");
    }
    Ok((
        index,
        MezPatchHunk {
            anchors,
            range_hint,
            lines: hunk_lines,
            old,
            new,
        },
        trailing_newline,
    ))
}

fn is_mez_hunk_header_line(line: &str) -> bool {
    line.starts_with("@@")
        || (!line.starts_with([' ', '+', '-']) && line.trim_start().starts_with("@@"))
}

fn parse_mez_patch_hunk_header(header: &str) -> (Vec<String>, Option<MezPatchRangeHint>) {
    let body = header.strip_prefix("@@").unwrap_or(header).trim();
    let (body, range_hint) = strip_unified_hunk_range_metadata(body);
    let anchors = body
        .split("@@")
        .map(str::trim)
        .filter(|anchor| !anchor.is_empty())
        .map(ToString::to_string)
        .collect();
    (anchors, range_hint)
}

fn strip_unified_hunk_range_metadata(body: &str) -> (&str, Option<MezPatchRangeHint>) {
    let Some(after_old_prefix) = body.strip_prefix('-') else {
        return (body, None);
    };
    let Some((old_start, after_old_range)) = consume_unified_hunk_range(after_old_prefix) else {
        return (body, None);
    };
    let Some(after_new_prefix) = after_old_range.trim_start().strip_prefix('+') else {
        return (body, None);
    };
    let Some((_, after_new_range)) = consume_unified_hunk_range(after_new_prefix) else {
        return (body, None);
    };
    let after_ranges = after_new_range.trim_start();
    let Some(after_closing_marker) = after_ranges.strip_prefix("@@") else {
        return (body, None);
    };
    (
        after_closing_marker.trim_start(),
        Some(MezPatchRangeHint { old_start }),
    )
}

fn consume_unified_hunk_range(text: &str) -> Option<(usize, &str)> {
    let (start, text) = consume_ascii_number(text)?;
    let text = if let Some(after_comma) = text.strip_prefix(',') {
        consume_ascii_number(after_comma)?.1
    } else {
        text
    };
    if text.is_empty()
        || text
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_whitespace() || character == '@')
    {
        Some((start, text))
    } else {
        None
    }
}

fn consume_ascii_number(text: &str) -> Option<(usize, &str)> {
    let mut end = 0usize;
    for (index, character) in text.char_indices() {
        if !character.is_ascii_digit() {
            break;
        }
        end = index + character.len_utf8();
    }
    if end == 0 {
        return None;
    }
    Some((text[..end].parse().ok()?, &text[end..]))
}

fn clean_mez_patch_path(raw: &str) -> Result<String> {
    let mut raw = raw.trim();
    raw = raw
        .strip_prefix("a/")
        .or_else(|| raw.strip_prefix("b/"))
        .unwrap_or(raw);
    while let Some(stripped) = raw.strip_prefix("./") {
        raw = stripped;
    }
    if raw.is_empty() || raw.starts_with('/') {
        return apply_patch_parse_error(&format!("unsafe patch path: {raw}"));
    }
    let mut parts = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | ".." => return apply_patch_parse_error(&format!("unsafe patch path: {raw}")),
            "." => {}
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        return apply_patch_parse_error(&format!("unsafe patch path: {raw}"));
    }
    Ok(parts.join("/"))
}
