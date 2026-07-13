//! Permissions Policy implementation.
//!
//! This module owns the permissions policy boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    ApprovalDecision, ApprovalPolicy, BTreeMap, BlockedApprovalQueue, BlockedApprovalRequest,
    BlockedApprovalState, CommandRule, CommandRuleScope, CommandRuleStore,
    DEFAULT_COMMAND_SHELL_CLASSIFICATION, MezError, PathScopes, PermissionAuthorityChange,
    PermissionPolicy, PermissionPreset, Result, RuleDecision, RuleMatch, SessionApprovalStore,
    analyze_shell, classify_tokens, decode_rule_record, encode_rule_record, exact_command_sha256,
    normalize_exact_command_text, tokenize_shell_words, tokenize_single_candidate,
    writes_escape_scopes,
};
use std::time::{SystemTime, UNIX_EPOCH};

// Permission policy evaluation and authority comparisons.

/// Runs the approval prefix for shell command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn approval_prefix_for_shell_command(command: &str) -> Result<Vec<String>> {
    tokenize_single_candidate(command).ok_or_else(|| {
        MezError::invalid_args("unable to derive approval prefix from shell command")
    })
}

impl Default for BlockedApprovalQueue {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            next_id: 1,
            requests: BTreeMap::new(),
        }
    }
}

impl BlockedApprovalQueue {
    /// Runs the create operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn create(&mut self, mut request: BlockedApprovalRequest) -> Result<String> {
        if request.requesting_agent_id.is_empty()
            || request.pane_id.is_empty()
            || request.action_kind.is_empty()
            || request.action_summary.is_empty()
        {
            return Err(MezError::invalid_args(
                "blocked approval requests require agent, pane, action kind, and summary",
            ));
        }
        let id = format!("ba{}", self.next_id);
        self.next_id += 1;
        request.id = id.clone();
        request.created_at_unix_seconds = Some(current_unix_seconds());
        request.decided_at_unix_seconds = None;
        request.decided_by_client_id = None;
        request.state = BlockedApprovalState::Pending;
        request.decision = None;
        request.redirect_instruction = None;
        self.requests.insert(id.clone(), request);
        Ok(id)
    }

    /// Runs the decide operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn decide(
        &mut self,
        request_id: &str,
        decision: ApprovalDecision,
        redirect_instruction: Option<String>,
    ) -> Result<&BlockedApprovalRequest> {
        self.decide_with_client(request_id, decision, redirect_instruction, None)
    }

    /// Runs the decide with client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn decide_with_client(
        &mut self,
        request_id: &str,
        decision: ApprovalDecision,
        redirect_instruction: Option<String>,
        decided_by_client_id: Option<String>,
    ) -> Result<&BlockedApprovalRequest> {
        let request = self.requests.get_mut(request_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "approval request not found",
            )
        })?;
        if request.state != BlockedApprovalState::Pending {
            return Err(MezError::conflict(
                "blocked approval request has already been decided",
            ));
        }
        if decision == ApprovalDecision::Redirect
            && redirect_instruction
                .as_deref()
                .is_none_or(|instruction| instruction.trim().is_empty())
        {
            return Err(MezError::invalid_args(
                "redirect decisions require a redirect instruction",
            ));
        }
        request.state = match decision {
            ApprovalDecision::Approve => BlockedApprovalState::Approved,
            ApprovalDecision::Disapprove => BlockedApprovalState::Disapproved,
            ApprovalDecision::Redirect => BlockedApprovalState::Redirected,
        };
        request.decision = Some(decision);
        request.redirect_instruction = redirect_instruction;
        request.decided_at_unix_seconds = Some(current_unix_seconds());
        request.decided_by_client_id = decided_by_client_id;
        Ok(request)
    }

    /// Runs the get operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn get(&self, request_id: &str) -> Option<&BlockedApprovalRequest> {
        self.requests.get(request_id)
    }

    /// Runs the pending operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn pending(&self) -> Vec<&BlockedApprovalRequest> {
        self.requests
            .values()
            .filter(|request| request.state == BlockedApprovalState::Pending)
            .collect()
    }

    /// Runs the requests operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn requests(&self) -> impl Iterator<Item = &BlockedApprovalRequest> {
        self.requests.values()
    }
}

/// Runs the current unix seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

impl CommandRuleStore {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Runs the add operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn add(&mut self, rule: CommandRule) -> Result<()> {
        if rule.scope == CommandRuleScope::BuiltIn {
            return Err(MezError::invalid_args(
                "persisted command rule store must not contain built-in rules",
            ));
        }
        self.rules.push(rule);
        Ok(())
    }

    /// Runs the remove operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn remove(&mut self, rule_id: &str) -> Result<CommandRule> {
        let index = parse_rule_id(rule_id)?;
        if index >= self.rules.len() {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotFound,
                "command rule not found",
            ));
        }
        Ok(self.rules.remove(index))
    }

    /// Runs the rules operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn rules(&self) -> &[CommandRule] {
        &self.rules
    }

    /// Runs the encode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn encode(&self) -> String {
        self.rules
            .iter()
            .map(encode_rule_record)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Runs the decode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn decode(data: &str) -> Result<Self> {
        let mut store = Self::new();
        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            store.add(decode_rule_record(line)?)?;
        }
        Ok(store)
    }
}

/// Runs the parse rule id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_rule_id(rule_id: &str) -> Result<usize> {
    let value = rule_id
        .strip_prefix("rule")
        .ok_or_else(|| MezError::invalid_args("command rule id must use the rule<N> format"))?;
    let number = value
        .parse::<usize>()
        .map_err(|_| MezError::invalid_args("command rule id number is invalid"))?;
    if number == 0 {
        return Err(MezError::invalid_args(
            "command rule id numbers are one-based",
        ));
    }
    Ok(number - 1)
}

impl PermissionPolicy {
    /// Runs the rules operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn rules(&self) -> &[CommandRule] {
        &self.rules
    }

    /// Runs the add rule operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn add_rule(&mut self, rule: CommandRule) {
        self.rules.push(rule);
    }

    /// Runs the remove rule operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn remove_rule(&mut self, rule_id: &str) -> Result<CommandRule> {
        let index = parse_rule_id(rule_id)?;
        if index >= self.rules.len() {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotFound,
                "command rule not found",
            ));
        }
        if self.rules[index].scope == CommandRuleScope::BuiltIn {
            return Err(MezError::invalid_args(
                "built-in command rules cannot be removed from the live policy",
            ));
        }
        Ok(self.rules.remove(index))
    }

    /// Runs the set approval bypass operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_approval_bypass(&mut self, active: bool) {
        self.approval_bypass = active;
    }

    /// Runs the with approval policy operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn with_approval_policy(mut self, approval_policy: ApprovalPolicy) -> Self {
        self.approval_policy = approval_policy;
        self
    }

    /// Runs the approval bypass operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn approval_bypass(&self) -> bool {
        self.approval_bypass
    }

    /// Projects the live product policy into bounded agent-shell display state.
    ///
    /// Command rules, path scopes, approval persistence, and enforcement stay
    /// owned by Mezzanine; the agent shell receives only user-visible scalars.
    pub fn agent_shell_summary(&self) -> mez_agent::AgentShellPermissionSummary {
        mez_agent::AgentShellPermissionSummary {
            preset: self.preset,
            approval_policy: self.approval_policy,
            approval_bypass: self.approval_bypass(),
            command_rule_count: self.rules().len(),
        }
    }

    /// Runs the evaluate shell command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn evaluate_shell_command(&self, command: &str) -> RuleDecision {
        self.evaluate_shell_command_scoped(command, None)
    }

    /// Runs the evaluate shell command for shell classification operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn evaluate_shell_command_for_shell_classification(
        &self,
        command: &str,
        shell_classification: &str,
    ) -> RuleDecision {
        self.evaluate_shell_command_scoped_with_classification(command, None, shell_classification)
    }

    /// Runs the evaluate shell command in scope operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn evaluate_shell_command_in_scope(
        &self,
        command: &str,
        scopes: &PathScopes,
    ) -> RuleDecision {
        self.evaluate_shell_command_scoped_with_classification(
            command,
            Some(scopes),
            DEFAULT_COMMAND_SHELL_CLASSIFICATION,
        )
    }

    /// Runs the evaluate shell command scoped operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn evaluate_shell_command_scoped(
        &self,
        command: &str,
        scopes: Option<&PathScopes>,
    ) -> RuleDecision {
        self.evaluate_shell_command_scoped_with_classification(
            command,
            scopes,
            DEFAULT_COMMAND_SHELL_CLASSIFICATION,
        )
    }

    /// Runs the evaluate shell command scoped with classification operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn evaluate_shell_command_scoped_with_classification(
        &self,
        command: &str,
        scopes: Option<&PathScopes>,
        shell_classification: &str,
    ) -> RuleDecision {
        if self.approval_bypass {
            return RuleDecision::Allow;
        }

        let cwd_trusted = scopes.is_some_and(|s| {
            self.trusted_directories
                .iter()
                .any(|trusted| cwd_starts_with(&s.current_directory, trusted))
        });

        let analysis = analyze_shell(command);
        if analysis.candidates.is_empty() {
            return self.apply_approval_policy(RuleDecision::Prompt, cwd_trusted);
        };

        let mut decision = RuleDecision::Allow;
        for candidate in analysis.candidates {
            if let Some(exact_decision) =
                self.evaluate_exact_sha256_candidate(&candidate, shell_classification)
            {
                decision = decision.min(exact_decision);
                if decision == RuleDecision::Forbid {
                    return RuleDecision::Forbid;
                }
                continue;
            }
            if analysis.unsafe_syntax {
                if let Some(exact_decision) = self.evaluate_exact_token_candidate(&candidate) {
                    decision = decision.min(exact_decision);
                    if decision == RuleDecision::Forbid {
                        return RuleDecision::Forbid;
                    }
                    continue;
                }
                if let Some(tokens) = tokenize_shell_words(&candidate)
                    && self.evaluate_tokens(&tokens, scopes) == RuleDecision::Forbid
                {
                    return RuleDecision::Forbid;
                }
                decision = decision.min(RuleDecision::Prompt);
                continue;
            }
            let Some(tokens) = tokenize_shell_words(&candidate) else {
                return self.apply_approval_policy(RuleDecision::Prompt, cwd_trusted);
            };
            let candidate_decision = self.evaluate_tokens(&tokens, scopes);
            decision = decision.min(candidate_decision);
            if decision == RuleDecision::Forbid {
                return RuleDecision::Forbid;
            }
        }
        self.apply_approval_policy(decision, cwd_trusted)
    }

    /// Runs the evaluate exact sha256 candidate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn evaluate_exact_sha256_candidate(
        &self,
        candidate: &str,
        shell_classification: &str,
    ) -> Option<RuleDecision> {
        let normalized = normalize_exact_command_text(candidate, false);
        let digest_hex = exact_command_sha256(shell_classification, &normalized);
        self.rules
            .iter()
            .filter(|rule| rule.exact_sha256_matches(&digest_hex, shell_classification))
            .map(|rule| rule.decision)
            .min()
    }

    /// Runs the evaluate exact token candidate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn evaluate_exact_token_candidate(&self, candidate: &str) -> Option<RuleDecision> {
        let tokens = tokenize_shell_words(candidate)?;
        self.rules
            .iter()
            .filter(|rule| matches!(rule.rule_match, RuleMatch::Exact) && rule.pattern == tokens)
            .map(|rule| rule.decision)
            .min()
    }

    /// Runs the evaluate shell command with approvals operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn evaluate_shell_command_with_approvals(
        &self,
        command: &str,
        approvals: &SessionApprovalStore,
    ) -> RuleDecision {
        self.evaluate_shell_command_with_approvals_scoped(command, approvals, None)
    }

    /// Evaluates a shell command against command policy, session approvals, and
    /// optional shell-resolved path scopes.
    pub fn evaluate_shell_command_with_approvals_scoped(
        &self,
        command: &str,
        approvals: &SessionApprovalStore,
        scopes: Option<&PathScopes>,
    ) -> RuleDecision {
        let base_decision = self.evaluate_shell_command_scoped(command, scopes);
        let scope_requires_fresh_approval = scopes.is_some()
            && base_decision == RuleDecision::Prompt
            && self.evaluate_shell_command(command) == RuleDecision::Allow;
        match approvals.evaluate(command) {
            Some(ApprovalDecision::Approve)
                if base_decision == RuleDecision::Prompt && !scope_requires_fresh_approval =>
            {
                RuleDecision::Allow
            }
            Some(ApprovalDecision::Approve) => base_decision,
            Some(ApprovalDecision::Disapprove | ApprovalDecision::Redirect) => RuleDecision::Forbid,
            None => base_decision,
        }
    }

    /// Runs the apply approval policy operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_approval_policy(
        &self,
        decision: RuleDecision,
        _cwd_trusted: bool,
    ) -> RuleDecision {
        match (self.approval_policy, decision) {
            (ApprovalPolicy::FullAccess, RuleDecision::Prompt) => RuleDecision::Allow,
            _ => decision,
        }
    }

    /// Runs the evaluate tokens operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn evaluate_tokens(
        &self,
        tokens: &[String],
        scopes: Option<&PathScopes>,
    ) -> RuleDecision {
        let matching_rules = self
            .rules
            .iter()
            .filter(|rule| rule.matches(tokens, scopes))
            .collect::<Vec<_>>();
        let rule_decision = matching_rules
            .iter()
            .map(|rule| rule.decision)
            .min()
            .unwrap_or(RuleDecision::Prompt);

        if rule_decision != RuleDecision::Allow {
            return rule_decision;
        }
        if matching_rules.iter().any(|rule| {
            rule.decision == RuleDecision::Allow && rule.scope != CommandRuleScope::BuiltIn
        }) {
            return RuleDecision::Allow;
        }

        let effects = classify_tokens(tokens, scopes);
        if effects.unknown
            || effects.destructive
            || effects.privilege_change
            || effects.credentials
            || effects.network
            || writes_escape_scopes(&effects, scopes)
        {
            RuleDecision::Prompt
        } else {
            RuleDecision::Allow
        }
    }
}

/// Runs the compare permission preset authority operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compare_permission_preset_authority(
    current: PermissionPreset,
    requested: PermissionPreset,
) -> PermissionAuthorityChange {
    compare_authority_rank(
        permission_preset_authority_rank(current),
        permission_preset_authority_rank(requested),
    )
}

/// Runs the compare approval policy authority operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn compare_approval_policy_authority(
    current: ApprovalPolicy,
    requested: ApprovalPolicy,
) -> PermissionAuthorityChange {
    compare_authority_rank(
        approval_policy_authority_rank(current),
        approval_policy_authority_rank(requested),
    )
}

/// Runs the compare authority rank operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn compare_authority_rank(current: u8, requested: u8) -> PermissionAuthorityChange {
    match requested.cmp(&current) {
        std::cmp::Ordering::Less => PermissionAuthorityChange::Narrowing,
        std::cmp::Ordering::Equal => PermissionAuthorityChange::NoChange,
        std::cmp::Ordering::Greater => PermissionAuthorityChange::Broadening,
    }
}

/// Runs the permission preset authority rank operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn permission_preset_authority_rank(preset: PermissionPreset) -> u8 {
    match preset {
        PermissionPreset::ReadOnly => 0,
        PermissionPreset::Auto => 1,
    }
}

/// Runs the cwd starts with operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cwd_starts_with(cwd: &str, trusted: &str) -> bool {
    if cwd == trusted {
        return true;
    }
    let trusted = if trusted.ends_with('/') {
        trusted.to_string()
    } else {
        format!("{trusted}/")
    };
    cwd.starts_with(&trusted) && cwd.len() > trusted.len()
}

/// Runs the approval policy authority rank operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn approval_policy_authority_rank(policy: ApprovalPolicy) -> u8 {
    match policy {
        ApprovalPolicy::Ask => 0,
        ApprovalPolicy::AutoAllow => 1,
        ApprovalPolicy::FullAccess => 2,
    }
}
