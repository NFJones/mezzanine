//! Agent action permission primitives.
//!
//! This module implements the first layer of command-prefix policy and approval
//! bypass state. It intentionally fails closed for shell syntax that cannot be
//! classified by the current lightweight tokenizer.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use crate::error::{MezError, Result};

/// Exposes the classification module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod classification;
/// Exposes the paths module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod paths;
/// Exposes the policy module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod policy;
/// Exposes the rules module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod rules;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

pub use policy::{
    approval_prefix_for_shell_command, compare_approval_policy_authority,
    compare_permission_preset_authority,
};
pub use rules::{
    builtin_rules, classify_shell_command, exact_command_sha256, normalize_exact_command_text,
};
pub use types::{
    ApprovalDecision, ApprovalGrant, ApprovalPolicy, ApprovalScope, ArgumentPolicy,
    BlockedApprovalQueue, BlockedApprovalRequest, BlockedApprovalState, CommandRule,
    CommandRuleScope, CommandRuleStore, DEFAULT_COMMAND_SHELL_CLASSIFICATION,
    EffectiveCommandEffects, PathResolutionStatus, PathScopes, PermissionAuthorityChange,
    PermissionPolicy, PermissionPreset, RuleDecision, RuleMatch, SessionApprovalStore,
};

use classification::{
    analyze_shell, classify_tokens, tokenize_shell_words, tokenize_single_candidate,
    validate_git_read_only_subcommand,
};
use paths::writes_escape_scopes;
use rules::{decode_rule_record, encode_rule_record};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
