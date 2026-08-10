//! Product adapter for canonical agent permission policy.
//!
//! Command policy, approval records, scopes, and deterministic evaluation live
//! in `mez_agent::permissions`. This module binds one live policy to product
//! approval and path-scope state for the agent turn planner.

use mez_agent::permissions::{
    ApprovalPolicy, DEFAULT_COMMAND_SHELL_CLASSIFICATION, PathScopes, PermissionEvaluation,
    PermissionPlanning, PermissionPolicy, SessionApprovalStore,
};

/// Borrowed planning view over active product permission state.
pub struct ProductPermissionPlanning<'a> {
    policy: &'a PermissionPolicy,
    approvals: &'a SessionApprovalStore,
    path_scopes: Option<&'a PathScopes>,
    shell_classification: &'a str,
    sandbox_first_local_prompts: bool,
}

impl<'a> ProductPermissionPlanning<'a> {
    /// Creates a planning adapter over active policy, approvals, and path facts.
    pub fn new(
        policy: &'a PermissionPolicy,
        approvals: &'a SessionApprovalStore,
        path_scopes: Option<&'a PathScopes>,
    ) -> Self {
        Self {
            policy,
            approvals,
            path_scopes,
            shell_classification: DEFAULT_COMMAND_SHELL_CLASSIFICATION,
            sandbox_first_local_prompts: false,
        }
    }

    /// Selects the grammar from the same live pane shell identity that will
    /// render and execute authorized command source.
    pub fn with_shell_classification(mut self, shell_classification: &'a str) -> Self {
        self.shell_classification = shell_classification;
        self
    }

    /// Enables sandbox-first dispatch for local actions after applying the
    /// active approval policy's interaction requirements.
    pub fn with_sandbox_first_local_prompts(mut self, enabled: bool) -> Self {
        self.sandbox_first_local_prompts = enabled;
        self
    }
}

impl PermissionPlanning for ProductPermissionPlanning<'_> {
    fn evaluate_command_structured(&self, command: &str) -> PermissionEvaluation {
        self.policy
            .evaluate_shell_command_structured_with_approvals_scoped_for_shell_classification(
                command,
                self.approvals,
                self.path_scopes,
                self.shell_classification,
            )
    }

    fn shell_classification(&self) -> &str {
        self.shell_classification
    }

    fn approval_policy(&self) -> ApprovalPolicy {
        self.policy.approval_policy
    }

    fn approval_bypass(&self) -> bool {
        self.policy.approval_bypass()
    }

    fn sandbox_first_local_prompts(&self) -> bool {
        self.sandbox_first_local_prompts && self.policy.approval_policy != ApprovalPolicy::Ask
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies Bubblewrap cannot turn an ask-mode prompt into execution before
    /// the user has approved the action.
    #[test]
    fn ask_policy_retains_fresh_approval_before_sandbox_dispatch() {
        let policy = PermissionPolicy::default().with_approval_policy(ApprovalPolicy::Ask);
        let approvals = SessionApprovalStore::default();
        let planning = ProductPermissionPlanning::new(&policy, &approvals, None)
            .with_sandbox_first_local_prompts(true);

        assert!(!planning.sandbox_first_local_prompts());
    }

    /// Verifies auto-allow may proceed to Bubblewrap only after the planner's
    /// model-rationale gate has accepted a prompting action.
    #[test]
    fn auto_allow_policy_retains_sandbox_dispatch_after_model_gate() {
        let policy = PermissionPolicy::default().with_approval_policy(ApprovalPolicy::AutoAllow);
        let approvals = SessionApprovalStore::default();
        let planning = ProductPermissionPlanning::new(&policy, &approvals, None)
            .with_sandbox_first_local_prompts(true);

        assert!(planning.sandbox_first_local_prompts());
    }

    /// Verifies product planning analyzes source with the same Fish identity
    /// that will render and execute it, rather than the Unix-like default.
    /// A Fish command substitution hidden in double quotes must require fresh
    /// approval even though those parentheses are literal POSIX text.
    #[test]
    fn fish_planning_uses_pane_shell_classification() {
        let policy = PermissionPolicy::default();
        let approvals = SessionApprovalStore::default();
        let command = "printf '%s\\n' \"(curl https://example.test)\"";
        let planning = ProductPermissionPlanning::new(&policy, &approvals, None)
            .with_shell_classification("fish");

        assert_eq!(planning.shell_classification(), "fish");
        let evaluation = planning.evaluate_command_structured(command);
        assert_eq!(
            evaluation.decision,
            mez_agent::permissions::RuleDecision::Prompt
        );
        assert!(evaluation.effects.unknown);
    }
}
