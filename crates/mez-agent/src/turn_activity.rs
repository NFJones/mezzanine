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
}

impl AgentNetworkActionHistory {
    /// Records one executed network request.
    pub fn record(&mut self, request: impl Into<String>) {
        self.requests.push(request.into());
    }
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
