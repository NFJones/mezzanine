//! Provider-independent volatile activity state for one agent turn.
//!
//! This module owns shell and network action history, advisory implementation
//! pressure, validation-command recognition, and mid-turn user-steering
//! context. Product runtime code retains the maps that scope these values to
//! live turns and owns pane dispatch, clocks, context insertion, and tracing.

use crate::{AgentAction, AgentActionPayload};

/// User-authored steering input accepted while a turn is already running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTurnSteering {
    /// Original user prompt text.
    pub input: String,
    /// Product-supplied Unix timestamp when the prompt was accepted.
    pub submitted_at_unix_seconds: u64,
}

/// Builds canonical model-facing context for one mid-turn steering input.
pub fn agent_turn_steering_context_content(steering: &AgentTurnSteering) -> String {
    format!(
        "[user steering input during active turn]\n\
submitted_at_unix_seconds={}\n\
The user added this instruction while the current turn was already in progress.\n\
Incorporate it into the current task from this point forward. Do not restart\n\
completed work unless necessary. If this conflicts with earlier instructions,\n\
the newer user instruction takes precedence.\n\n\
User input:\n{}",
        steering.submitted_at_unix_seconds, steering.input
    )
}

/// Shell dispatch history for one active agent turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentShellDispatchHistory {
    /// Commands dispatched during the turn.
    pub commands: Vec<String>,
    /// Commands that reached a successful transaction boundary.
    pub succeeded_commands: Vec<String>,
    /// Consecutive model-authored shell dispatches in the current phase.
    pub consecutive_shell_dispatches: usize,
    /// Consecutive successful model-authored shell commands.
    pub consecutive_successful_shell_commands: usize,
    /// Whether a file mutation succeeded during this turn.
    pub successful_file_mutation_this_turn: bool,
    /// Whether validation succeeded after the latest file mutation.
    pub successful_validation_after_file_mutation: bool,
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
        self.consecutive_shell_dispatches = self.consecutive_shell_dispatches.saturating_add(1);
    }

    /// Records a shell-backed action that completed successfully.
    pub fn record_success(
        &mut self,
        command: impl Into<String>,
        action: &AgentAction,
        command_is_validation: bool,
    ) {
        self.succeeded_commands.push(command.into());
        match action.payload {
            AgentActionPayload::ShellCommand { .. } => {
                if command_is_validation && self.successful_file_mutation_this_turn {
                    self.consecutive_shell_dispatches = 0;
                    self.consecutive_successful_shell_commands = 0;
                    self.successful_validation_after_file_mutation = true;
                } else {
                    self.consecutive_successful_shell_commands =
                        self.consecutive_successful_shell_commands.saturating_add(1);
                }
            }
            AgentActionPayload::ApplyPatch { .. } => {
                self.consecutive_shell_dispatches = 0;
                self.consecutive_successful_shell_commands = 0;
                self.successful_file_mutation_this_turn = true;
                self.successful_validation_after_file_mutation = false;
            }
            _ => {}
        }
    }

    /// Resets inspection streaks after a non-shell runtime effect.
    pub fn reset_successive_shell_commands(&mut self) {
        self.consecutive_shell_dispatches = 0;
        self.consecutive_successful_shell_commands = 0;
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

/// Current advisory action-pressure phase for an active turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionPressurePhase {
    /// Repeated shell inspection crossed the configured threshold.
    InspectionStreak {
        /// Consecutive shell dispatch count.
        consecutive_shell_dispatches: usize,
        /// Current staged pressure severity.
        severity: ActionPressureSeverity,
    },
    /// A file mutation succeeded but validation has not.
    MutationAwaitingValidation,
    /// A file mutation and later validation both succeeded.
    MutationValidated,
}

/// Current inspection-streak pressure severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionPressureSeverity {
    /// Early nudge after a short inspection streak.
    Gentle,
    /// Stronger nudge after a longer inspection streak.
    Medium,
    /// Highest pressure after prolonged shell inspection.
    Strong,
}

/// Returns the current action-pressure phase for one turn history.
pub fn action_pressure_phase(
    history: &AgentShellDispatchHistory,
    threshold: usize,
) -> Option<ActionPressurePhase> {
    if history.successful_file_mutation_this_turn
        && history.successful_validation_after_file_mutation
    {
        return Some(ActionPressurePhase::MutationValidated);
    }
    if history.successful_file_mutation_this_turn {
        return Some(ActionPressurePhase::MutationAwaitingValidation);
    }
    let consecutive_shell_dispatches = history.consecutive_shell_dispatches;
    if consecutive_shell_dispatches >= threshold {
        return Some(ActionPressurePhase::InspectionStreak {
            consecutive_shell_dispatches,
            severity: action_pressure_severity(consecutive_shell_dispatches, threshold),
        });
    }
    None
}

/// Returns the staged severity for a shell-command inspection streak.
pub fn action_pressure_severity(
    consecutive_shell_dispatches: usize,
    threshold: usize,
) -> ActionPressureSeverity {
    let medium_threshold = threshold.saturating_mul(2).max(6);
    let strong_threshold = threshold.saturating_mul(3).max(10);
    if consecutive_shell_dispatches >= strong_threshold {
        ActionPressureSeverity::Strong
    } else if consecutive_shell_dispatches >= medium_threshold {
        ActionPressureSeverity::Medium
    } else {
        ActionPressureSeverity::Gentle
    }
}

/// Builds canonical model-facing advisory context for one pressure phase.
pub fn action_pressure_context_content(phase: ActionPressurePhase) -> String {
    let phase_message = match phase {
        ActionPressurePhase::InspectionStreak {
            consecutive_shell_dispatches,
            severity,
        } => {
            let severity_message = match severity {
                ActionPressureSeverity::Gentle => {
                    "Apply gentle pressure now: stop broadening discovery unless one named missing fact still blocks the next implementation, validation, or report action."
                }
                ActionPressureSeverity::Medium => {
                    "Apply medium pressure now: prefer the next implementation, focused regression test, execution-based validation, or final-report action instead of further shell discovery."
                }
                ActionPressureSeverity::Strong => {
                    "Apply strong pressure now: do not continue shell discovery without a concrete justification from recent evidence for why another shell_command is required before acting."
                }
            };
            format!(
                "This turn has already dispatched {consecutive_shell_dispatches} consecutive shell_command actions. {severity_message}"
            )
        }
        ActionPressurePhase::MutationAwaitingValidation => {
            "A file mutation has already succeeded this turn. Prefer execution-based validation, required format/build/lint/test commands, focused diff/status review, or final report now.".to_string()
        }
        ActionPressurePhase::MutationValidated => {
            "A file mutation and at least one validation command have already succeeded this turn. Run any remaining repository-required validation, commit or handoff step, or final report now.".to_string()
        }
    };
    format!(
        "{phase_message} \
         Continue following active repository guidance, validation, documentation, and handoff requirements. \
         Do not edit repository instruction or guidance files merely to satisfy this acceleration hint; change them only when the user explicitly requested guidance changes or they are part of the task. \
         Use another shell_command only for one named missing fact that would make the next edit, execution-based validation, repair, commit, or report wrong. \
         This is advisory context, not a failed action result, and it does not relax repository rules or permission/capability requirements."
    )
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

    /// Builds one shell-command action for history-transition tests.
    fn shell_action() -> AgentAction {
        AgentAction {
            id: "shell-1".to_string(),
            rationale: "validate".to_string(),
            payload: AgentActionPayload::ShellCommand {
                summary: "check".to_string(),
                command: "cargo check".to_string(),
                interactive: false,
                stateful: false,
                timeout_ms: None,
            },
        }
    }

    /// Verifies pressure escalates deterministically with inspection history.
    #[test]
    fn action_pressure_escalates_with_shell_dispatches() {
        let mut history = AgentShellDispatchHistory::default();
        for _ in 0..10 {
            history.record("rg TODO");
        }
        assert_eq!(
            action_pressure_phase(&history, 3),
            Some(ActionPressurePhase::InspectionStreak {
                consecutive_shell_dispatches: 10,
                severity: ActionPressureSeverity::Strong,
            })
        );
        assert!(
            action_pressure_context_content(action_pressure_phase(&history, 3).unwrap())
                .contains("Apply strong pressure")
        );
    }

    /// Verifies a successful mutation followed by validation advances the
    /// canonical pressure phase and recognizes common validation commands.
    #[test]
    fn action_pressure_tracks_mutation_validation_progress() {
        let mut history = AgentShellDispatchHistory::default();
        let patch = AgentAction {
            id: "patch-1".to_string(),
            rationale: "edit".to_string(),
            payload: AgentActionPayload::ApplyPatch {
                patch: "*** Begin Patch\n*** End Patch".to_string(),
                strip: None,
            },
        };
        history.record_success("apply patch", &patch, false);
        assert_eq!(
            action_pressure_phase(&history, 3),
            Some(ActionPressurePhase::MutationAwaitingValidation)
        );
        assert!(shell_command_looks_like_validation(
            "timeout 60s cargo test"
        ));
        history.record_success("cargo test", &shell_action(), true);
        assert_eq!(
            action_pressure_phase(&history, 3),
            Some(ActionPressurePhase::MutationValidated)
        );
    }

    /// Verifies steering context preserves user text and precedence guidance.
    #[test]
    fn steering_context_preserves_input_and_timestamp() {
        let content = agent_turn_steering_context_content(&AgentTurnSteering {
            input: "Focus on the parser".to_string(),
            submitted_at_unix_seconds: 42,
        });
        assert!(content.contains("submitted_at_unix_seconds=42"));
        assert!(content.contains("newer user instruction takes precedence"));
        assert!(content.ends_with("Focus on the parser"));
    }
}
