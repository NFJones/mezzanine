//! Provider-independent volatile activity state for one agent turn.
//!
//! This module owns shell and network action history, validation-command
//! recognition, and mid-turn user-steering state.
//! Product runtime code retains the maps that scope these values to live turns
//! and owns pane dispatch, clocks, context insertion, and tracing.

/// User-authored steering input accepted while a turn is already running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTurnSteering {
    /// Original user prompt text.
    pub input: String,
    /// Product-supplied Unix timestamp when the prompt was accepted.
    pub submitted_at_unix_seconds: u64,
}

/// Returns the exact user-authored text for one mid-turn steering event.
///
/// Timestamps remain controller metadata. Stable prompt authority already
/// defines instruction precedence, so chronology does not need synthetic
/// coaching wrapped around the user's words.
pub fn agent_turn_steering_context_content(steering: &AgentTurnSteering) -> String {
    steering.input.clone()
}

/// Shell dispatch history for one active agent turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentShellDispatchHistory {
    /// Commands dispatched during the turn.
    pub commands: Vec<String>,
    /// Commands that reached a successful transaction boundary.
    pub succeeded_commands: Vec<String>,
}

impl AgentShellDispatchHistory {
    /// Returns the number of model-selected shell commands dispatched.
    pub fn dispatched_count(&self) -> usize {
        self.commands.len()
    }

    /// Returns how many times the exact command text succeeded.
    pub fn exact_success_count(&self, command: &str) -> usize {
        self.succeeded_commands
            .iter()
            .filter(|existing| existing.as_str() == command)
            .count()
    }

    /// Records one dispatched shell command.
    pub fn record(&mut self, command: impl Into<String>) {
        self.commands.push(command.into());
    }

    /// Records a shell-backed command that completed successfully.
    pub fn record_success(&mut self, command: impl Into<String>) {
        self.succeeded_commands.push(command.into());
    }
}

/// Network action history for one active agent turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentNetworkActionHistory {
    /// Network requests executed during the turn.
    pub requests: Vec<String>,
    /// Typed network actions retained for conservative no-progress detection.
    pub actions: Vec<AgentNetworkActionRecord>,
    progress_epoch: u64,
}

impl AgentNetworkActionHistory {
    /// Records one dispatched web search and its normalized topic signature.
    pub fn record_web_search(
        &mut self,
        request: impl Into<String>,
        query: &str,
        domains: &[String],
    ) {
        let request = request.into();
        self.requests.push(request.clone());
        self.actions.push(AgentNetworkActionRecord {
            kind: AgentNetworkActionKind::WebSearch,
            request,
            signature: canonical_web_search_signature(query, domains),
            progress_epoch: self.progress_epoch,
        });
    }

    /// Records a URL fetch, which is concrete progress beyond search-result discovery.
    pub fn record_fetch_url(&mut self, request: impl Into<String>) {
        let request = request.into();
        self.requests.push(request.clone());
        self.actions.push(AgentNetworkActionRecord {
            kind: AgentNetworkActionKind::FetchUrl,
            request,
            signature: Vec::new(),
            progress_epoch: self.progress_epoch,
        });
        self.mark_progress();
    }

    /// Returns the current consecutive streak equivalent to a proposed search.
    pub fn equivalent_search_streak(&self, query: &str, domains: &[String]) -> usize {
        let candidate = canonical_web_search_signature(query, domains);
        self.actions
            .iter()
            .rev()
            .take_while(|record| {
                record.progress_epoch == self.progress_epoch
                    && record.kind == AgentNetworkActionKind::WebSearch
                    && web_search_signatures_are_equivalent(&record.signature, &candidate)
            })
            .count()
    }

    /// Starts a fresh no-progress epoch after a concrete strategy or task action.
    pub fn mark_progress(&mut self) {
        self.progress_epoch = self.progress_epoch.saturating_add(1);
    }
}

/// Typed kind of runtime-network action retained in per-turn activity history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentNetworkActionKind {
    /// A search-engine query whose returned results still need to be processed.
    WebSearch,
    /// A direct URL retrieval selected from known evidence.
    FetchUrl,
}

/// One network action retained for no-progress diagnostics and loop detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentNetworkActionRecord {
    /// Network action family.
    pub kind: AgentNetworkActionKind,
    /// Exact permission-facing request string dispatched by the runtime.
    pub request: String,
    /// Conservative normalized topic tokens for web searches.
    pub signature: Vec<String>,
    progress_epoch: u64,
}

/// Produces stable topic tokens while discarding presentation-only search words.
fn canonical_web_search_signature(query: &str, domains: &[String]) -> Vec<String> {
    let normalized = query
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '.' {
                character
            } else {
                ' '
            }
        })
        .collect::<String>();
    let mut signature = normalized
        .split_whitespace()
        .filter_map(|token| match token {
            "a" | "an" | "and" | "for" | "find" | "guide" | "latest" | "official" | "page"
            | "search" | "the" | "to" => None,
            "aws" => Some("amazon".to_string()),
            "docs" => Some("documentation".to_string()),
            token => Some(token.to_string()),
        })
        .collect::<Vec<_>>();
    signature.extend(
        domains
            .iter()
            .map(|domain| format!("site:{}", domain.trim().to_ascii_lowercase())),
    );
    signature.sort();
    signature.dedup();
    signature
}

/// Conservatively groups trivial query reformulations without equating broad topics.
fn web_search_signatures_are_equivalent(left: &[String], right: &[String]) -> bool {
    if left.is_empty() || right.is_empty() {
        return false;
    }
    if left == right {
        return true;
    }
    let intersection = left.iter().filter(|token| right.contains(token)).count();
    let union = left
        .len()
        .saturating_add(right.len())
        .saturating_sub(intersection);
    intersection >= 2 && intersection.saturating_mul(100) >= union.saturating_mul(60)
}

/// Returns whether a shell command appears to run execution-based validation.
pub fn shell_command_looks_like_validation(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    [
        "cargo test",
        "cargo check",
        "cargo clippy",
        "cargo fmt",
        "just test",
        "just check",
        "just clippy",
        "just fmt",
        "npm test",
        "pnpm test",
        "yarn test",
        "pytest",
        "go test",
        "git diff --check",
    ]
    .iter()
    .any(|needle| command.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies shell history retains only data used by deterministic loop
    /// detection after model-facing implementation pressure is removed.
    #[test]
    fn shell_history_tracks_dispatches_and_exact_successes() {
        let mut history = AgentShellDispatchHistory::default();
        history.record("rg TODO");
        history.record("rg TODO");
        history.record_success("rg TODO");

        assert_eq!(history.dispatched_count(), 2);
        assert_eq!(history.exact_success_count("rg TODO"), 1);
    }

    /// Verifies trivial AWS documentation query reformulations form one
    /// search-only streak while a direct fetch starts a fresh progress epoch.
    #[test]
    fn network_history_detects_equivalent_search_streaks_and_fetch_progress() {
        let mut history = AgentNetworkActionHistory::default();
        history.record_web_search(
            "request-1",
            "AWS Bedrock model access official documentation",
            &[],
        );
        history.record_web_search("request-2", "Amazon Bedrock model access docs", &[]);

        assert_eq!(
            history.equivalent_search_streak("bedrock model access documentation", &[]),
            2
        );

        history.record_fetch_url("request-3");
        assert_eq!(
            history.equivalent_search_streak("bedrock model access documentation", &[]),
            0
        );
        assert_eq!(history.requests.len(), 3);
        assert_eq!(history.actions.len(), 3);
    }

    /// Verifies a materially different topic is not rejected merely because
    /// several web searches have already occurred in the same turn.
    #[test]
    fn network_history_keeps_materially_different_searches_independent() {
        let mut history = AgentNetworkActionHistory::default();
        history.record_web_search("request-1", "AWS Bedrock model access", &[]);
        history.record_web_search("request-2", "AWS Bedrock model access docs", &[]);

        assert_eq!(
            history.equivalent_search_streak("AWS Bedrock OAuth callback security", &[]),
            0
        );
    }

    /// Verifies common validation commands remain recognizable for controller
    /// diagnostics without producing model-facing pressure text.
    #[test]
    fn recognizes_validation_commands_without_pressure_state() {
        assert!(shell_command_looks_like_validation(
            "timeout 60s cargo test"
        ));
        assert!(!shell_command_looks_like_validation("rg TODO"));
    }

    /// Verifies steering context preserves only the exact user text while the
    /// timestamp remains available in typed controller state.
    #[test]
    fn steering_context_preserves_exact_input_without_metadata() {
        let steering = AgentTurnSteering {
            input: "Focus on the parser".to_string(),
            submitted_at_unix_seconds: 42,
        };
        let content = agent_turn_steering_context_content(&steering);
        assert_eq!(content, "Focus on the parser");
        assert_eq!(steering.submitted_at_unix_seconds, 42);
    }
}
