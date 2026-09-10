//! Product adapter for canonical agent permission policy.
//!
//! Command policy, approval records, scopes, and deterministic evaluation live
//! in `mez_agent::permissions`. This module binds one live policy to product
//! approval and path-scope state for the agent turn planner.

use crate::runtime::runtime_message_recipient_decision;
use mez_agent::permissions::{
    ApprovalPolicy, DEFAULT_COMMAND_SHELL_CLASSIFICATION, PathScopes, PermissionEvaluation,
    PermissionPlanning, PermissionPolicy, RuleDecision, SessionApprovalStore,
};

/// Borrowed planning view over active product permission state.
pub struct ProductPermissionPlanning<'a> {
    policy: &'a PermissionPolicy,
    approvals: &'a SessionApprovalStore,
    path_scopes: Option<&'a PathScopes>,
    shell_classification: &'a str,
    sandbox_first_local_prompts: bool,
    macro_bridge_recipients: Vec<String>,
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
            macro_bridge_recipients: Vec::new(),
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

    /// Binds the macro and bridge child agent ids owned by one planned turn.
    ///
    /// Runtime macro and bridge sends are runtime-owned orchestration steps,
    /// so their recipients never prompt regardless of the active approval mode.
    pub fn with_macro_bridge_recipients(mut self, recipients: Vec<String>) -> Self {
        self.macro_bridge_recipients = recipients;
        self
    }

    /// Returns the product decision for one model-planned message recipient.
    ///
    /// Explicit deny rules win in every mode, explicit allow rules admit the
    /// recipient, runtime macro and bridge targets stay ungated, and otherwise
    /// the effective approval policy decides whether delivery must prompt.
    pub fn message_recipient_decision(&self, recipient: &str) -> RuleDecision {
        if self.message_recipient_is_macro_bridge(recipient) {
            return RuleDecision::Allow;
        }
        runtime_message_recipient_decision(self.policy, recipient)
    }

    /// Reports whether one recipient names a runtime macro or bridge child.
    fn message_recipient_is_macro_bridge(&self, recipient: &str) -> bool {
        if self.macro_bridge_recipients.is_empty() {
            return false;
        }
        let candidate = recipient.strip_prefix("agent:").unwrap_or(recipient);
        self.macro_bridge_recipients
            .iter()
            .any(|agent_id| agent_id == candidate)
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

    fn evaluate_message_recipient(&self, recipient: &str) -> RuleDecision {
        self.message_recipient_decision(recipient)
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

    /// Builds one product policy carrying a message pseudo-command rule.
    fn message_rule_policy(
        decision: RuleDecision,
        approval_policy: ApprovalPolicy,
    ) -> PermissionPolicy {
        let mut policy = PermissionPolicy::default().with_approval_policy(approval_policy);
        policy.add_rule(mez_agent::permissions::CommandRule {
            id: Some("test-message-rule".to_string()),
            pattern: vec!["send_message".to_string()],
            decision,
            rule_match: mez_agent::permissions::RuleMatch::Prefix,
            argument_policy: mez_agent::permissions::ArgumentPolicy::None,
            scope: mez_agent::permissions::CommandRuleScope::User,
            justification: None,
            declared_effects: None,
        });
        policy
    }

    /// Verifies message-recipient decisions follow the four approval modes for
    /// direct recipients and every fan-out scope, keep explicit deny precedence
    /// in every mode, and leave runtime macro and bridge recipients ungated.
    #[test]
    fn message_recipient_decisions_follow_approval_modes() {
        let approvals = SessionApprovalStore::default();
        let recipients = [
            "session",
            "agent-9",
            "agent:agent-9",
            "role:reviewer",
            "capability:search",
            "group:reviewers",
        ];
        for recipient in recipients {
            for (approval_policy, expected) in [
                (ApprovalPolicy::Ask, RuleDecision::Prompt),
                (ApprovalPolicy::AutoAllow, RuleDecision::Prompt),
                (ApprovalPolicy::FullAccess, RuleDecision::Allow),
                (ApprovalPolicy::HostAccess, RuleDecision::Allow),
            ] {
                let policy = PermissionPolicy::default().with_approval_policy(approval_policy);
                let planning = ProductPermissionPlanning::new(&policy, &approvals, None);
                assert_eq!(
                    planning.message_recipient_decision(recipient),
                    expected,
                    "{recipient} under {approval_policy:?}"
                );
            }

            for approval_policy in [
                ApprovalPolicy::Ask,
                ApprovalPolicy::AutoAllow,
                ApprovalPolicy::FullAccess,
                ApprovalPolicy::HostAccess,
            ] {
                let denied = message_rule_policy(RuleDecision::Forbid, approval_policy);
                let planning = ProductPermissionPlanning::new(&denied, &approvals, None);
                assert_eq!(
                    planning.message_recipient_decision(recipient),
                    RuleDecision::Forbid,
                    "{recipient} deny precedence under {approval_policy:?}"
                );
            }

            let allowed = message_rule_policy(RuleDecision::Allow, ApprovalPolicy::Ask);
            let planning = ProductPermissionPlanning::new(&allowed, &approvals, None);
            assert_eq!(
                planning.message_recipient_decision(recipient),
                RuleDecision::Allow,
                "{recipient} explicit allow"
            );
        }

        let ask = PermissionPolicy::default().with_approval_policy(ApprovalPolicy::Ask);
        let planning = ProductPermissionPlanning::new(&ask, &approvals, None)
            .with_macro_bridge_recipients(vec!["agent-macro".to_string()]);
        assert_eq!(
            planning.message_recipient_decision("agent:agent-macro"),
            RuleDecision::Allow
        );
        assert_eq!(
            planning.message_recipient_decision("agent-macro"),
            RuleDecision::Allow
        );
        assert_eq!(
            planning.message_recipient_decision("agent-other"),
            RuleDecision::Prompt
        );
        assert_eq!(
            planning.message_recipient_decision("not a recipient"),
            RuleDecision::Allow
        );
    }
}
