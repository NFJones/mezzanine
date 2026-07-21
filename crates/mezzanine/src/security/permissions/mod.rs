//! Product adapter for canonical agent permission policy.
//!
//! Command policy, approval records, scopes, and deterministic evaluation live
//! in `mez_agent::permissions`. This module binds one live policy to product
//! approval and path-scope state for the agent turn planner.

use mez_agent::permissions::{
    ApprovalPolicy, PathScopes, PermissionEvaluation, PermissionPlanning, PermissionPolicy,
    SessionApprovalStore,
};

/// Borrowed planning view over active product permission state.
pub struct ProductPermissionPlanning<'a> {
    policy: &'a PermissionPolicy,
    approvals: &'a SessionApprovalStore,
    path_scopes: Option<&'a PathScopes>,
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
            sandbox_first_local_prompts: false,
        }
    }

    /// Enables sandbox-first dispatch for local actions that would otherwise
    /// require a fresh approval.
    pub fn with_sandbox_first_local_prompts(mut self, enabled: bool) -> Self {
        self.sandbox_first_local_prompts = enabled;
        self
    }
}

impl PermissionPlanning for ProductPermissionPlanning<'_> {
    fn evaluate_command_structured(&self, command: &str) -> PermissionEvaluation {
        self.policy
            .evaluate_shell_command_structured_with_approvals_scoped(
                command,
                self.approvals,
                self.path_scopes,
            )
    }

    fn approval_policy(&self) -> ApprovalPolicy {
        self.policy.approval_policy
    }

    fn approval_bypass(&self) -> bool {
        self.policy.approval_bypass()
    }

    fn sandbox_first_local_prompts(&self) -> bool {
        self.sandbox_first_local_prompts
    }
}
