//! Permissions Types implementation.
//!
//! This module owns the permissions types boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use std::collections::BTreeMap;

use mez_agent::{ApprovalPolicy, PermissionPreset, RuleDecision};

use super::{
    MezError, Result,
    classification::{
        find_args_are_read_only, git_read_only_args_are_safe, literal_output_args_are_safe,
        remaining_args_are_executable_probes, remaining_args_are_read_paths,
        remaining_args_are_script_then_read_paths, tokenize_single_candidate, uname_args_are_safe,
    },
    rules::{
        builtin_rules, exact_command_sha256, validate_sha256_hex, validate_shell_classification,
    },
};

// Permission data types, approval stores, and queues.

/// Defines the DEFAULT COMMAND SHELL CLASSIFICATION const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_COMMAND_SHELL_CLASSIFICATION: &str = "unix-like";

/// Carries Permission Authority Change state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionAuthorityChange {
    /// Represents the Narrowing case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Narrowing,
    /// Represents the No Change case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    NoChange,
    /// Represents the Broadening case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Broadening,
}

/// Carries Rule Match state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleMatch {
    /// Represents the Prefix case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Prefix,
    /// Represents the Exact case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Exact,
    /// Represents the Exact Sha256 case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ExactSha256 {
        /// Stores the digest hex value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        digest_hex: String,
        /// Stores the shell classification value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        shell_classification: String,
    },
}

/// Carries Command Rule Scope state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRuleScope {
    /// Represents the Built In case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    BuiltIn,
    /// Represents the Session case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Session,
    /// Represents the Project case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Project,
    /// Represents the User case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    User,
    /// Represents the Managed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Managed,
}

/// Carries Argument Policy state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgumentPolicy {
    /// Represents the None case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    None,
    /// Represents the Executable Probe case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ExecutableProbe {
        /// Stores the allowed options value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        allowed_options: Vec<String>,
    },
    /// Represents the Uname Probe case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    UnameProbe,
    /// Represents the Literal Output case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    LiteralOutput,
    /// Represents the Read Paths case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ReadPaths {
        /// Stores the allowed options value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        allowed_options: Vec<String>,
    },
    /// Represents the Script Then Read Paths case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ScriptThenReadPaths {
        /// Stores the allowed options value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        allowed_options: Vec<String>,
    },
    /// Represents the Find Read Only case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    FindReadOnly,
    /// Represents the Git Read Only case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    GitReadOnly {
        /// Stores the subcommand value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        subcommand: String,
    },
}

/// Carries Command Rule state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRule {
    /// Stores the pattern value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pattern: Vec<String>,
    /// Stores the decision value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub decision: RuleDecision,
    /// Stores the rule match value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub rule_match: RuleMatch,
    /// Stores the argument policy value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub argument_policy: ArgumentPolicy,
    /// Stores the scope value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub scope: CommandRuleScope,
    /// Stores the justification value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub justification: Option<String>,
}

impl CommandRule {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(
        pattern: impl IntoIterator<Item = impl Into<String>>,
        decision: RuleDecision,
        rule_match: RuleMatch,
    ) -> Result<Self> {
        let pattern = pattern.into_iter().map(Into::into).collect::<Vec<_>>();
        if pattern.is_empty() {
            return Err(MezError::invalid_args(
                "command prefix rule pattern must not be empty",
            ));
        }
        Ok(Self {
            pattern,
            decision,
            rule_match,
            argument_policy: ArgumentPolicy::None,
            scope: CommandRuleScope::Managed,
            justification: None,
        })
    }

    /// Runs the new exact sha256 operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new_exact_sha256(
        normalized_command_text: &str,
        shell_classification: impl Into<String>,
        decision: RuleDecision,
    ) -> Result<Self> {
        let shell_classification = shell_classification.into();
        validate_shell_classification(&shell_classification)?;
        let digest_hex = exact_command_sha256(&shell_classification, normalized_command_text);
        Self::from_exact_sha256_digest(digest_hex, shell_classification, decision)
    }

    /// Runs the from exact sha256 digest operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_exact_sha256_digest(
        digest_hex: impl Into<String>,
        shell_classification: impl Into<String>,
        decision: RuleDecision,
    ) -> Result<Self> {
        let digest_hex = digest_hex.into();
        let shell_classification = shell_classification.into();
        validate_sha256_hex(&digest_hex)?;
        validate_shell_classification(&shell_classification)?;
        Ok(Self {
            pattern: vec![digest_hex.clone()],
            decision,
            rule_match: RuleMatch::ExactSha256 {
                digest_hex,
                shell_classification,
            },
            argument_policy: ArgumentPolicy::None,
            scope: CommandRuleScope::Managed,
            justification: None,
        })
    }

    /// Runs the with argument policy operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn with_argument_policy(mut self, argument_policy: ArgumentPolicy) -> Self {
        self.argument_policy = argument_policy;
        self
    }

    /// Runs the with scope operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn with_scope(mut self, scope: CommandRuleScope) -> Self {
        self.scope = scope;
        self
    }

    /// Runs the with justification operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn with_justification(mut self, justification: impl Into<String>) -> Self {
        self.justification = Some(justification.into());
        self
    }

    /// Runs the matches operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn matches(&self, tokens: &[String], scopes: Option<&PathScopes>) -> bool {
        match self.rule_match {
            RuleMatch::Prefix => {
                tokens.starts_with(&self.pattern)
                    && (self.decision != RuleDecision::Allow
                        || self.scope != CommandRuleScope::BuiltIn
                        || self.arguments_match(&tokens[self.pattern.len()..], scopes))
            }
            RuleMatch::Exact => tokens == self.pattern,
            RuleMatch::ExactSha256 { .. } => false,
        }
    }

    /// Runs the exact sha256 matches operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn exact_sha256_matches(
        &self,
        digest_hex: &str,
        shell_classification: &str,
    ) -> bool {
        match &self.rule_match {
            RuleMatch::ExactSha256 {
                digest_hex: expected_digest,
                shell_classification: expected_classification,
            } => expected_digest == digest_hex && expected_classification == shell_classification,
            RuleMatch::Prefix | RuleMatch::Exact => false,
        }
    }

    /// Runs the arguments match operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn arguments_match(
        &self,
        remaining: &[String],
        scopes: Option<&PathScopes>,
    ) -> bool {
        match &self.argument_policy {
            ArgumentPolicy::None => remaining.is_empty(),
            ArgumentPolicy::ExecutableProbe { allowed_options } => {
                remaining_args_are_executable_probes(remaining, allowed_options)
            }
            ArgumentPolicy::UnameProbe => uname_args_are_safe(remaining),
            ArgumentPolicy::LiteralOutput => literal_output_args_are_safe(remaining),
            ArgumentPolicy::ReadPaths { allowed_options } => {
                remaining_args_are_read_paths(remaining, allowed_options, scopes)
            }
            ArgumentPolicy::ScriptThenReadPaths { allowed_options } => {
                remaining_args_are_script_then_read_paths(remaining, allowed_options, scopes)
            }
            ArgumentPolicy::FindReadOnly => find_args_are_read_only(remaining, scopes),
            ArgumentPolicy::GitReadOnly { subcommand } => {
                git_read_only_args_are_safe(subcommand, remaining, scopes)
            }
        }
    }
}

/// Carries Permission Policy state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct PermissionPolicy {
    /// Stores the preset value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub preset: PermissionPreset,
    /// Stores the approval policy value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub approval_policy: ApprovalPolicy,
    /// Stores the rules value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) rules: Vec<CommandRule>,
    /// Stores the approval bypass value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) approval_bypass: bool,
    /// Stores the trusted directories value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub trusted_directories: Vec<String>,
}

impl Default for PermissionPolicy {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            preset: PermissionPreset::ReadOnly,
            approval_policy: ApprovalPolicy::Ask,
            rules: builtin_rules(),
            approval_bypass: false,
            trusted_directories: Vec::new(),
        }
    }
}

/// Carries Approval Scope state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalScope {
    /// Represents the Session case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Session,
    /// Represents the Global case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Global,
}

/// Carries Approval Decision state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Represents the Approve case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Approve,
    /// Represents the Disapprove case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Disapprove,
    /// Represents the Redirect case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Redirect,
}

/// Carries Approval Grant state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalGrant {
    /// Stores the id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: String,
    /// Stores the command prefix value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub command_prefix: Vec<String>,
    /// Stores the scope value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub scope: ApprovalScope,
    /// Stores the decision value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub decision: ApprovalDecision,
}

/// Carries Session Approval Store state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct SessionApprovalStore {
    /// Stores the next id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) next_id: u64,
    /// Stores the grants value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) grants: BTreeMap<String, ApprovalGrant>,
}

/// Carries Blocked Approval State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedApprovalState {
    /// Represents the Pending case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Pending,
    /// Represents the Approved case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Approved,
    /// Represents the Disapproved case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Disapproved,
    /// Represents the Redirected case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Redirected,
}

/// Carries Blocked Approval Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedApprovalRequest {
    /// Stores the id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: String,
    /// Stores the requesting agent id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub requesting_agent_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the parent agent chain value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub parent_agent_chain: Vec<String>,
    /// Stores the action kind value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub action_kind: String,
    /// Stores the action summary value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub action_summary: String,
    /// Stores the declared effects value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub declared_effects: Vec<String>,
    /// Stores the matched rules value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub matched_rules: Vec<String>,
    /// Stores the read scopes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub read_scopes: Vec<String>,
    /// Stores the write scopes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub write_scopes: Vec<String>,
    /// Stores the cooperation mode value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cooperation_mode: Option<String>,
    /// Stores the created at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub created_at_unix_seconds: Option<u64>,
    /// Stores the decided at unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub decided_at_unix_seconds: Option<u64>,
    /// Stores the decided by client id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub decided_by_client_id: Option<String>,
    /// Stores the state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub state: BlockedApprovalState,
    /// Stores the decision value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub decision: Option<ApprovalDecision>,
    /// Stores the redirect instruction value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub redirect_instruction: Option<String>,
}

/// Carries Blocked Approval Queue state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct BlockedApprovalQueue {
    /// Stores the next id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) next_id: u64,
    /// Stores the requests value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) requests: BTreeMap<String, BlockedApprovalRequest>,
}

/// Carries Command Rule Store state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandRuleStore {
    /// Stores the rules value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) rules: Vec<CommandRule>,
}

/// Carries Path Resolution Status state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathResolutionStatus {
    /// Paths were observed and canonicalized through the pane shell.
    ShellResolved,
    /// Paths are lexical or otherwise not trusted for security decisions.
    Unresolved,
}

/// Carries Path Scopes state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathScopes {
    /// Pane-shell current directory used to resolve relative paths.
    pub current_directory: String,
    /// Canonical read scopes active for the agent or subagent.
    pub read_scopes: Vec<String>,
    /// Canonical write scopes active for the agent or subagent.
    pub write_scopes: Vec<String>,
    /// Canonical pane-shell paths keyed by requested path or normalized path.
    pub canonical_paths: BTreeMap<String, String>,
    /// Whether this scope set came from trusted shell-mediated resolution.
    pub resolution_status: PathResolutionStatus,
}

impl PathScopes {
    /// Builds a scope set whose current directory, scopes, and path mappings are
    /// trusted shell-observed values.
    pub fn shell_resolved(
        current_directory: impl Into<String>,
        read_scopes: Vec<String>,
        write_scopes: Vec<String>,
    ) -> Self {
        Self {
            current_directory: current_directory.into(),
            read_scopes,
            write_scopes,
            canonical_paths: BTreeMap::new(),
            resolution_status: PathResolutionStatus::ShellResolved,
        }
    }

    /// Builds a scope set that must fail closed for scoped security checks.
    pub fn unresolved(
        current_directory: impl Into<String>,
        read_scopes: Vec<String>,
        write_scopes: Vec<String>,
    ) -> Self {
        Self {
            current_directory: current_directory.into(),
            read_scopes,
            write_scopes,
            canonical_paths: BTreeMap::new(),
            resolution_status: PathResolutionStatus::Unresolved,
        }
    }

    /// Adds a shell-canonical path for one requested command path.
    pub fn with_canonical_path(
        mut self,
        requested_path: impl Into<String>,
        canonical_path: impl Into<String>,
    ) -> Self {
        self.canonical_paths
            .insert(requested_path.into(), canonical_path.into());
        self
    }
}

/// Carries Effective Command Effects state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveCommandEffects {
    /// Stores the reads value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub reads: Vec<String>,
    /// Stores the writes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub writes: Vec<String>,
    /// Stores the creates value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub creates: Vec<String>,
    /// Stores the deletes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub deletes: Vec<String>,
    /// Stores the touches value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub touches: Vec<String>,
    /// Stores the network value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub network: bool,
    /// Stores the credentials value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub credentials: bool,
    /// Stores the process control value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub process_control: bool,
    /// Stores the destructive value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub destructive: bool,
    /// Stores the privilege change value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub privilege_change: bool,
    /// Stores the unknown value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub unknown: bool,
}

impl Default for SessionApprovalStore {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            next_id: 1,
            grants: BTreeMap::new(),
        }
    }
}

impl SessionApprovalStore {
    /// Runs the decide prefix operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn decide_prefix(
        &mut self,
        prefix: impl IntoIterator<Item = impl Into<String>>,
        scope: ApprovalScope,
        decision: ApprovalDecision,
    ) -> Result<String> {
        let command_prefix = prefix.into_iter().map(Into::into).collect::<Vec<_>>();
        if command_prefix.is_empty() {
            return Err(MezError::invalid_args(
                "approval command prefix must not be empty",
            ));
        }
        let id = format!("ap{}", self.next_id);
        self.next_id += 1;
        self.grants.insert(
            id.clone(),
            ApprovalGrant {
                id: id.clone(),
                command_prefix,
                scope,
                decision,
            },
        );
        Ok(id)
    }

    /// Runs the evaluate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn evaluate(&self, command: &str) -> Option<ApprovalDecision> {
        let tokens = tokenize_single_candidate(command)?;
        self.grants
            .values()
            .filter(|grant| tokens.starts_with(&grant.command_prefix))
            .max_by_key(|grant| grant.command_prefix.len())
            .map(|grant| grant.decision)
    }

    /// Runs the grants operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn grants(&self) -> impl Iterator<Item = &ApprovalGrant> {
        self.grants.values()
    }
}
