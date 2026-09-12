//! Runtime evidence gathering for the effective sandbox resolver.
//!
//! Status, audit, and shell action results all consume the same resolved state
//! through this module so a compiled plan, capability proof, host access,
//! approved fallback, or foreign pane can never be reported differently by one
//! surface than another. Evidence comes from existing owners only: the
//! configured sandbox, the pane-effective permission policy, the exact
//! capability cache, retained fallback approvals, and pane shell attestation.

use mez_agent::ApprovalPolicy;
use mez_agent::permissions::{EffectCompleteness, PermissionEvaluation};

use crate::runtime::{
    ActionResult, AgentAction, AgentActionPayload, AgentTurnRecord, RuntimeSessionService,
    SandboxConfig,
};
use crate::security::sandbox::{
    EffectiveSandboxState, SandboxAuditSummary, SandboxCapabilityCacheKey,
    SandboxEffectiveEvidence, SandboxExecutionHost, resolve_effective_sandbox,
};

/// Owned resolver evidence gathered outside long runtime borrows.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeSandboxEvidence {
    /// Configured sandbox selected for the pane.
    sandbox: SandboxConfig,
    /// Effective approval policy for the turn or pane.
    approval_policy: ApprovalPolicy,
    /// Attested execution host for the pane.
    host: SandboxExecutionHost,
    /// Exact capability proof for the live environment, when cached.
    capability: Option<SandboxCapabilityCacheKey>,
    /// Redacted facts from a compiled launch plan, when one exists.
    plan: Option<SandboxAuditSummary>,
    /// Whether the retained structured effects were complete.
    effects_complete: bool,
    /// Whether an approved one-shot unsandboxed retry is active.
    sandbox_bypass_approved: bool,
}

impl RuntimeSandboxEvidence {
    /// Resolves the closed effective sandbox state from owned evidence.
    pub(crate) fn resolve(&self) -> EffectiveSandboxState {
        resolve_effective_sandbox(
            &self.sandbox,
            self.approval_policy,
            self.host,
            SandboxEffectiveEvidence {
                capability: self.capability.as_ref(),
                plan: self.plan.as_ref(),
                effects_complete: self.effects_complete,
                sandbox_bypass_approved: self.sandbox_bypass_approved,
            },
        )
    }
}

impl RuntimeSessionService {
    /// Returns the effective sandbox state for one pane from live evidence.
    pub(crate) fn effective_sandbox_state_for_pane(&self, pane_id: &str) -> EffectiveSandboxState {
        let sandbox = self.sandbox_config_for_pane(pane_id);
        let approval_policy = self.permission_policy_for_pane(pane_id).approval_policy;
        let capability = sandbox
            .backend()
            .and_then(|backend| self.sandbox_capability_proof_for_pane(pane_id, backend));
        resolve_effective_sandbox(
            &sandbox,
            approval_policy,
            self.sandbox_execution_host(pane_id),
            SandboxEffectiveEvidence {
                capability: capability.as_ref(),
                plan: None,
                effects_complete: false,
                sandbox_bypass_approved: false,
            },
        )
    }

    /// Gathers owned resolver evidence for one shell action identity.
    ///
    /// The returned value owns every input so callers may resolve it while
    /// holding an unrelated mutable runtime borrow.
    pub(crate) fn sandbox_evidence_for_action_id(
        &self,
        turn: &AgentTurnRecord,
        action_id: &str,
        plan: Option<&SandboxAuditSummary>,
        evaluation: Option<&PermissionEvaluation>,
    ) -> RuntimeSandboxEvidence {
        let sandbox = self.sandbox_config_for_pane(&turn.pane_id);
        let approval_policy = self.permission_policy_for_turn(turn).approval_policy;
        let fallback_identity = (turn.turn_id.clone(), action_id.to_string());
        let sandbox_bypass_approved = self
            .agent
            .sandbox_fallback_audits
            .contains_key(&fallback_identity)
            && self.sandbox_bypass_active_for_action(&turn.turn_id, action_id);
        let capability = sandbox
            .backend()
            .and_then(|backend| self.sandbox_capability_proof_for_pane(&turn.pane_id, backend));
        RuntimeSandboxEvidence {
            sandbox,
            approval_policy,
            host: self.sandbox_execution_host(&turn.pane_id),
            capability,
            plan: plan.cloned(),
            effects_complete: evaluation
                .is_some_and(|evaluation| evaluation.completeness == EffectCompleteness::Complete),
            sandbox_bypass_approved,
        }
    }

    /// Returns the effective sandbox state for one shell action.
    pub(crate) fn effective_sandbox_state_for_action(
        &self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        plan: Option<&SandboxAuditSummary>,
        evaluation: Option<&PermissionEvaluation>,
    ) -> EffectiveSandboxState {
        self.sandbox_evidence_for_action_id(turn, &action.id, plan, evaluation)
            .resolve()
    }

    /// Returns the execution host used for one pane's boundary projection.
    pub(crate) fn sandbox_execution_host(&self, pane_id: &str) -> SandboxExecutionHost {
        if self.pane_has_uncertified_foreign_shell_boundary(pane_id) {
            SandboxExecutionHost::Unattested
        } else {
            SandboxExecutionHost::Pane
        }
    }

    /// Attaches the bounded effective-sandbox projection to one shell result.
    ///
    /// Only the fixed boundary, enforcement, network-mode, and reason keys are
    /// added; the projection never includes argv, paths, or probe output.
    pub(crate) fn attach_effective_sandbox_to_shell_result(
        &self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        plan: Option<&SandboxAuditSummary>,
        evaluation: Option<&PermissionEvaluation>,
        result: &mut ActionResult,
    ) {
        if !matches!(
            action.payload,
            AgentActionPayload::ShellCommand { .. } | AgentActionPayload::ApplyPatch { .. }
        ) {
            return;
        }
        let Some(structured) = result.structured_content_json.as_deref() else {
            return;
        };
        let state = self.effective_sandbox_state_for_action(turn, action, plan, evaluation);
        result.structured_content_json = Some(
            mez_agent::shell_structured_content_with_sandbox_effective_json(
                structured,
                state.structured_json(),
            ),
        );
    }
}
