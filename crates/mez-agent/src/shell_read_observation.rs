//! Structured shell-read observation extraction for model-facing action results.
//!
//! This module captures coarse file-read and text-search coverage from shell
//! commands at action-result creation time so later context assembly can reuse
//! structured observations instead of reparsing shell command strings.

use serde::{Deserialize, Serialize};

/// One observed file line range from a shell read command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellReadRange {
    /// Inclusive starting line number.
    pub start_line: usize,
    /// Inclusive ending line number.
    pub end_line: usize,
}

/// The coarse read/search kind observed from one shell command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellReadObservationKind {
    /// A bounded file-content read such as `sed -n` or `cat`.
    Read,
    /// A text search such as `rg`.
    Search,
}

/// Structured read/search coverage observed from one shell command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellReadObservation {
    /// Read or search classifier.
    pub kind: ShellReadObservationKind,
    /// Best-effort path or target scope.
    pub target: String,
    /// Best-effort line ranges covered by the command.
    #[serde(default)]
    pub ranges: Vec<ShellReadRange>,
    /// Best-effort query string when the command performed a search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

/// Returns structured read/search observations for one shell command.
pub fn shell_read_observations_for_command(command: &str) -> Vec<ShellReadObservation> {
    let tokens = shell_like_tokens(command);
    let mut observations = Vec::new();
    for segment in shell_command_segments(&tokens) {
        let mut segment_observations = Vec::new();
        if let Some(observation) = sed_read_observation(segment) {
            segment_observations.push(observation);
        }
        if let Some(observation) = rg_search_observation(segment) {
            segment_observations.push(observation);
        }
        if segment_observations.is_empty()
            && let Some(observation) = plain_read_observation(segment)
        {
            segment_observations.push(observation);
        }
        observations.extend(segment_observations);
    }
    observations
}

/// Splits shell-like tokens into simple command segments.
fn shell_command_segments(tokens: &[String]) -> Vec<&[String]> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token.as_str(), "&&" | "||" | "|" | ";") {
            if start < index {
                segments.push(&tokens[start..index]);
            }
            start = index.saturating_add(1);
        }
    }
    if start < tokens.len() {
        segments.push(&tokens[start..]);
    }
    segments
}

/// Returns a structured observation for a bounded `sed -n` read.
fn sed_read_observation(tokens: &[String]) -> Option<ShellReadObservation> {
    if !tokens
        .windows(2)
        .any(|pair| pair[0] == "sed" && pair[1] == "-n")
    {
        return None;
    }
    let target = read_target_hint(tokens)?;
    let ranges = tokens
        .iter()
        .filter_map(|token| sed_range_hint(token))
        .collect();
    Some(ShellReadObservation {
        kind: ShellReadObservationKind::Read,
        target,
        ranges,
        query: None,
    })
}

/// Returns a structured observation for a ripgrep search.
fn rg_search_observation(tokens: &[String]) -> Option<ShellReadObservation> {
    let rg_index = tokens
        .iter()
        .position(|token| token == "rg" || token.starts_with("rg"))?;
    let query = tokens
        .iter()
        .skip(rg_index + 1)
        .find(|token| !token.starts_with('-') && !looks_like_read_target(token))
        .cloned()
        .unwrap_or_else(|| "(unknown)".to_string());
    let target = tokens
        .iter()
        .skip(rg_index + 1)
        .rfind(|token| looks_like_read_target(token))
        .cloned()
        .unwrap_or_else(|| "(cwd)".to_string());
    Some(ShellReadObservation {
        kind: ShellReadObservationKind::Search,
        target,
        ranges: Vec::new(),
        query: Some(query),
    })
}

/// Returns a best-effort plain file-read observation.
fn plain_read_observation(tokens: &[String]) -> Option<ShellReadObservation> {
    if !tokens
        .iter()
        .any(|token| token == "cat" || token == "read" || token == "python" || token == "python3")
    {
        return None;
    }
    let target = read_target_hint(tokens)?;
    Some(ShellReadObservation {
        kind: ShellReadObservationKind::Read,
        target,
        ranges: Vec::new(),
        query: None,
    })
}

/// Extracts a best-effort line range from one token.
fn sed_range_hint(token: &str) -> Option<ShellReadRange> {
    let trimmed = token.trim_matches('\'').trim_matches('"');
    let trimmed = trimmed.strip_suffix('p').unwrap_or(trimmed);
    let (start, end) = trimmed.split_once(',')?;
    let start_line = start.parse::<usize>().ok()?;
    let end_line = end.parse::<usize>().ok()?;
    Some(ShellReadRange {
        start_line,
        end_line,
    })
}

/// Extracts a best-effort file target from shell-like tokens.
fn read_target_hint(tokens: &[String]) -> Option<String> {
    tokens
        .iter()
        .rev()
        .find(|token| looks_like_read_target(token))
        .cloned()
}

/// Returns whether one token looks like a file path or source target.
fn looks_like_read_target(token: &str) -> bool {
    if token.starts_with('-') || token.contains('=') {
        return false;
    }
    token.contains('/')
        || token.ends_with(".rs")
        || token.ends_with(".md")
        || token.ends_with(".toml")
        || token.ends_with(".json")
        || token.ends_with(".yaml")
        || token.ends_with(".yml")
        || token.ends_with(".txt")
}

/// Splits shell-like command text into coarse tokens for heuristic analysis.
fn shell_like_tokens(command: &str) -> Vec<String> {
    shlex::split(command).unwrap_or_else(|| {
        command
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    })
}
