//! Permissions Policy implementation.
//!
//! This module owns the permissions policy boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    ApprovalDecision, ApprovalPolicy, BTreeMap, BlockedApprovalQueue, BlockedApprovalRequest,
    BlockedApprovalState, CandidateEvaluation, CommandRule, CommandRuleScope, CommandRuleStore,
    DEFAULT_COMMAND_SHELL_CLASSIFICATION, DeclaredCommandEffects, EffectCompleteness,
    EffectiveCommandEffects, MezError, PathScopes, PermissionAuthorityChange, PermissionEvaluation,
    PermissionPolicy, PermissionPreset, Result, RuleDecision, RuleMatch, SessionApprovalStore,
    analyze_shell, classify_tokens, decode_rule_record, encode_rule_record, exact_command_sha256,
    normalize_exact_command_text, tokenize_shell_words, tokenize_single_candidate,
    writes_escape_scopes,
};
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
    pub fn create_at(
        &mut self,
        mut request: BlockedApprovalRequest,
        now_unix_seconds: u64,
    ) -> Result<String> {
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
        request.created_at_unix_seconds = Some(now_unix_seconds);
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
    pub fn decide_at(
        &mut self,
        request_id: &str,
        decision: ApprovalDecision,
        redirect_instruction: Option<String>,
        now_unix_seconds: u64,
    ) -> Result<&BlockedApprovalRequest> {
        self.decide_with_client_at(
            request_id,
            decision,
            redirect_instruction,
            None,
            now_unix_seconds,
        )
    }

    /// Runs the decide with client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn decide_with_client_at(
        &mut self,
        request_id: &str,
        decision: ApprovalDecision,
        redirect_instruction: Option<String>,
        decided_by_client_id: Option<String>,
        now_unix_seconds: u64,
    ) -> Result<&BlockedApprovalRequest> {
        let request = self
            .requests
            .get_mut(request_id)
            .ok_or_else(|| MezError::not_found("approval request not found"))?;
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
        request.decided_at_unix_seconds = Some(now_unix_seconds);
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
            return Err(MezError::not_found("command rule not found"));
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
            return Err(MezError::not_found("command rule not found"));
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
    pub fn agent_shell_summary(&self) -> super::AgentShellPermissionSummary {
        super::AgentShellPermissionSummary {
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

    /// Evaluates one shell policy command into authorization and resource
    /// effects without requiring later consumers to rematch command text.
    pub fn evaluate_shell_command_structured(&self, command: &str) -> PermissionEvaluation {
        self.evaluate_shell_command_structured_scoped(command, None)
    }

    /// Evaluates one shell policy command with shell-resolved path evidence.
    pub fn evaluate_shell_command_structured_in_scope(
        &self,
        command: &str,
        scopes: &PathScopes,
    ) -> PermissionEvaluation {
        self.evaluate_shell_command_structured_scoped(command, Some(scopes))
    }

    fn evaluate_shell_command_structured_scoped(
        &self,
        command: &str,
        scopes: Option<&PathScopes>,
    ) -> PermissionEvaluation {
        let decision = self.evaluate_shell_command_scoped(command, scopes);
        let analysis = analyze_shell(command);
        let mut candidates = Vec::with_capacity(analysis.candidates.len());
        for candidate in analysis.candidates {
            candidates.push(self.evaluate_candidate_structured(
                candidate,
                scopes,
                analysis.unsafe_syntax,
                DEFAULT_COMMAND_SHELL_CLASSIFICATION,
            ));
        }
        let completeness = if !candidates.is_empty()
            && candidates
                .iter()
                .all(|candidate| candidate.completeness == EffectCompleteness::Complete)
        {
            EffectCompleteness::Complete
        } else {
            EffectCompleteness::Unknown
        };
        let effects = aggregate_candidate_effects(&candidates, completeness);
        let mut matched_rule_ids = candidates
            .iter()
            .flat_map(|candidate| candidate.matched_rule_ids.iter().cloned())
            .collect::<Vec<_>>();
        matched_rule_ids.sort();
        matched_rule_ids.dedup();
        PermissionEvaluation {
            decision,
            candidates,
            matched_rule_ids,
            effects,
            completeness,
        }
    }

    fn evaluate_candidate_structured(
        &self,
        command: String,
        scopes: Option<&PathScopes>,
        unsafe_syntax: bool,
        shell_classification: &str,
    ) -> CandidateEvaluation {
        let exact_sha_rules = self.matching_exact_sha256_rules(&command, shell_classification);
        if !exact_sha_rules.is_empty() {
            return candidate_evaluation_from_rules(
                command,
                exact_sha_rules,
                EffectiveCommandEffects::unknown(),
            );
        }

        let tokens = tokenize_shell_words(&command);
        if unsafe_syntax {
            let exact_rules = tokens
                .as_ref()
                .map(|tokens| self.matching_exact_token_rules(tokens))
                .unwrap_or_default();
            if !exact_rules.is_empty() {
                return candidate_evaluation_from_rules(
                    command,
                    exact_rules,
                    EffectiveCommandEffects::unknown(),
                );
            }
            let matching_rules = tokens
                .as_ref()
                .map(|tokens| self.matching_token_rules(tokens, scopes))
                .unwrap_or_default();
            let decision = if matching_rules
                .iter()
                .any(|rule| rule.decision == RuleDecision::Forbid)
            {
                RuleDecision::Forbid
            } else {
                RuleDecision::Prompt
            };
            return CandidateEvaluation {
                command,
                decision,
                matched_rule_ids: stable_rule_ids(&matching_rules),
                effects: EffectiveCommandEffects::unknown(),
                completeness: EffectCompleteness::Unknown,
            };
        }

        let Some(tokens) = tokens else {
            return CandidateEvaluation {
                command,
                decision: RuleDecision::Prompt,
                matched_rule_ids: Vec::new(),
                effects: EffectiveCommandEffects::unknown(),
                completeness: EffectCompleteness::Unknown,
            };
        };
        let matching_rules = self.matching_token_rules(&tokens, scopes);
        let decision = self.evaluate_tokens(&tokens, scopes);
        let classified = classify_tokens(&tokens, scopes);
        candidate_evaluation_from_rules_with_decision(command, matching_rules, classified, decision)
    }

    fn matching_exact_sha256_rules(
        &self,
        candidate: &str,
        shell_classification: &str,
    ) -> Vec<&CommandRule> {
        let normalized = normalize_exact_command_text(candidate, false);
        let digest_hex = exact_command_sha256(shell_classification, &normalized);
        self.rules
            .iter()
            .filter(|rule| rule.exact_sha256_matches(&digest_hex, shell_classification))
            .collect()
    }

    fn matching_exact_token_rules(&self, tokens: &[String]) -> Vec<&CommandRule> {
        self.rules
            .iter()
            .filter(|rule| matches!(rule.rule_match, RuleMatch::Exact) && rule.pattern == tokens)
            .collect()
    }

    fn matching_token_rules(
        &self,
        tokens: &[String],
        scopes: Option<&PathScopes>,
    ) -> Vec<&CommandRule> {
        self.rules
            .iter()
            .filter(|rule| rule.matches(tokens, scopes))
            .collect()
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
        self.evaluate_shell_command_structured_with_approvals_scoped(command, approvals, scopes)
            .decision
    }

    /// Applies session approval state to one structured permission evaluation
    /// without changing its candidates, matched rules, effects, or
    /// completeness.
    pub fn evaluate_shell_command_structured_with_approvals_scoped(
        &self,
        command: &str,
        approvals: &SessionApprovalStore,
        scopes: Option<&PathScopes>,
    ) -> PermissionEvaluation {
        let mut evaluation = self.evaluate_shell_command_structured_scoped(command, scopes);
        let base_decision = evaluation.decision;
        let scope_requires_fresh_approval = scopes.is_some()
            && base_decision == RuleDecision::Prompt
            && self.evaluate_shell_command(command) == RuleDecision::Allow;
        evaluation.decision = match approvals.evaluate(command) {
            Some(ApprovalDecision::Approve)
                if base_decision == RuleDecision::Prompt && !scope_requires_fresh_approval =>
            {
                RuleDecision::Allow
            }
            Some(ApprovalDecision::Approve) => base_decision,
            Some(ApprovalDecision::Disapprove | ApprovalDecision::Redirect) => RuleDecision::Forbid,
            None => base_decision,
        };
        evaluation
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

/// Builds one candidate evaluation from matching rules and classifier facts.
fn candidate_evaluation_from_rules(
    command: String,
    rules: Vec<&CommandRule>,
    classified: EffectiveCommandEffects,
) -> CandidateEvaluation {
    let decision = rules
        .iter()
        .map(|rule| rule.decision)
        .min()
        .unwrap_or(RuleDecision::Prompt);
    candidate_evaluation_from_rules_with_decision(command, rules, classified, decision)
}

/// Builds one candidate evaluation while preserving the already-computed
/// authorization decision.
fn candidate_evaluation_from_rules_with_decision(
    command: String,
    rules: Vec<&CommandRule>,
    classified: EffectiveCommandEffects,
    decision: RuleDecision,
) -> CandidateEvaluation {
    let matched_rule_ids = stable_rule_ids(&rules);
    let (effects, completeness) = merge_candidate_effects(classified, &rules, decision);
    CandidateEvaluation {
        command,
        decision,
        matched_rule_ids,
        effects,
        completeness,
    }
}

/// Returns sorted, deduplicated configured rule identities.
fn stable_rule_ids(rules: &[&CommandRule]) -> Vec<String> {
    let mut ids = rules
        .iter()
        .filter_map(|rule| rule.id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

/// Merges classifier facts with declared effects without allowing declarations
/// to erase known security-sensitive requirements.
fn merge_candidate_effects(
    mut effects: EffectiveCommandEffects,
    rules: &[&CommandRule],
    decision: RuleDecision,
) -> (EffectiveCommandEffects, EffectCompleteness) {
    let configured_allow_rules = rules
        .iter()
        .copied()
        .filter(|rule| {
            rule.decision == RuleDecision::Allow && rule.scope != CommandRuleScope::BuiltIn
        })
        .collect::<Vec<_>>();
    if decision != RuleDecision::Allow || configured_allow_rules.is_empty() {
        let completeness = if effects.unknown {
            EffectCompleteness::Unknown
        } else {
            EffectCompleteness::Complete
        };
        return (effects, completeness);
    }

    let complete = configured_allow_rules.iter().all(|rule| {
        rule.declared_effects
            .as_ref()
            .is_some_and(|declared| declared.completeness == EffectCompleteness::Complete)
    });
    for declared in configured_allow_rules
        .iter()
        .filter_map(|rule| rule.declared_effects.as_ref())
    {
        merge_declared_effects(&mut effects, declared);
    }
    effects.unknown = !complete;
    normalize_effect_paths(&mut effects);
    (
        effects,
        if complete {
            EffectCompleteness::Complete
        } else {
            EffectCompleteness::Unknown
        },
    )
}

/// Adds one declaration to accumulated classifier facts.
fn merge_declared_effects(
    effects: &mut EffectiveCommandEffects,
    declared: &DeclaredCommandEffects,
) {
    effects.reads.extend(declared.read_scopes.iter().cloned());
    effects.writes.extend(declared.write_scopes.iter().cloned());
    effects.network |= declared.network.unwrap_or(false);
    effects.credentials |= declared.credentials.unwrap_or(false);
    effects.process_control |= declared.process_control.unwrap_or(false);
}

/// Unions candidate effects while retaining known facts from incomplete
/// candidates and marking command-wide narrowing unknown when required.
fn aggregate_candidate_effects(
    candidates: &[CandidateEvaluation],
    completeness: EffectCompleteness,
) -> EffectiveCommandEffects {
    let mut aggregate = EffectiveCommandEffects::empty();
    for candidate in candidates {
        aggregate
            .reads
            .extend(candidate.effects.reads.iter().cloned());
        aggregate
            .writes
            .extend(candidate.effects.writes.iter().cloned());
        aggregate
            .creates
            .extend(candidate.effects.creates.iter().cloned());
        aggregate
            .deletes
            .extend(candidate.effects.deletes.iter().cloned());
        aggregate
            .touches
            .extend(candidate.effects.touches.iter().cloned());
        aggregate.network |= candidate.effects.network;
        aggregate.credentials |= candidate.effects.credentials;
        aggregate.process_control |= candidate.effects.process_control;
        aggregate.destructive |= candidate.effects.destructive;
        aggregate.privilege_change |= candidate.effects.privilege_change;
    }
    aggregate.unknown = completeness == EffectCompleteness::Unknown;
    normalize_effect_paths(&mut aggregate);
    aggregate
}

/// Sorts and deduplicates path-like effect collections for deterministic
/// evaluation and audit output.
fn normalize_effect_paths(effects: &mut EffectiveCommandEffects) {
    for paths in [
        &mut effects.reads,
        &mut effects.writes,
        &mut effects.creates,
        &mut effects.deletes,
        &mut effects.touches,
    ] {
        paths.sort();
        paths.dedup();
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
