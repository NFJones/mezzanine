//! Regression coverage for the permissions tests subsystem.
//!
//! These tests describe the behavior protected by the repository
//! specification and workflow guidance. Keeping the scenarios documented
//! makes failures easier to map back to the user-visible contract.

// Permission module tests.

use super::rules::sha256_hex;
use super::{
    ApprovalDecision, ApprovalPolicy, ApprovalScope, ArgumentPolicy, BlockedApprovalQueue,
    BlockedApprovalRequest, BlockedApprovalState, CommandRule, CommandRuleScope, CommandRuleStore,
    DEFAULT_COMMAND_SHELL_CLASSIFICATION, DeclaredCommandEffects, EffectCompleteness, PathScopes,
    PermissionAuthorityChange, PermissionErrorKind, PermissionPolicy, PermissionPreset,
    RuleDecision, RuleMatch, SessionApprovalStore, classify_shell_command,
    compare_approval_policy_authority, compare_permission_preset_authority,
    normalize_exact_command_text,
};

/// Runs the shell resolved scopes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn shell_resolved_scopes(
    current_directory: &str,
    read_scopes: &[&str],
    write_scopes: &[&str],
    canonical_paths: &[(&str, &str)],
) -> PathScopes {
    let canonical_paths = canonical_paths
        .iter()
        .map(|(requested, canonical)| ((*requested).to_string(), (*canonical).to_string()))
        .collect();
    PathScopes::try_shell_resolved(
        current_directory,
        read_scopes
            .iter()
            .map(|scope| (*scope).to_string())
            .collect(),
        write_scopes
            .iter()
            .map(|scope| (*scope).to_string())
            .collect(),
        canonical_paths,
    )
    .unwrap()
}

/// Verifies trusted scope construction rejects lexical or relative authority.
/// A caller must not be able to label unchecked path text as shell-resolved and
/// later use it for security-sensitive containment or mount compilation.
#[test]
fn shell_resolved_scopes_require_canonical_absolute_authority() {
    let relative_cwd = PathScopes::try_shell_resolved(
        "repo",
        vec!["/repo".to_string()],
        Vec::new(),
        Default::default(),
    )
    .unwrap_err();
    assert_eq!(relative_cwd.kind(), PermissionErrorKind::InvalidArgs);

    let lexical_scope = PathScopes::try_shell_resolved(
        "/repo",
        vec!["/repo/../outside".to_string()],
        Vec::new(),
        Default::default(),
    )
    .unwrap_err();
    assert_eq!(lexical_scope.kind(), PermissionErrorKind::InvalidArgs);
}

/// Verifies normalized authority is deterministic, write access implies read
/// access, and child authority is the canonical intersection with its parent.
#[test]
fn shell_resolved_scopes_normalize_and_intersect_authority() {
    let parent = PathScopes::try_shell_resolved(
        "/repo",
        vec!["/repo".to_string(), "/repo/src".to_string()],
        vec!["/repo/target".to_string()],
        Default::default(),
    )
    .unwrap();
    assert_eq!(parent.read_scopes, vec!["/repo".to_string()]);
    assert_eq!(parent.write_scopes, vec!["/repo/target".to_string()]);

    let requested = PathScopes::try_shell_resolved(
        "/repo",
        vec!["/repo/src".to_string(), "/outside".to_string()],
        vec![
            "/repo/target/generated".to_string(),
            "/outside/write".to_string(),
        ],
        Default::default(),
    )
    .unwrap();
    let effective = parent.intersection(&requested).unwrap();

    assert_eq!(
        effective.read_scopes,
        vec![
            "/repo/src".to_string(),
            "/repo/target/generated".to_string(),
        ]
    );
    assert_eq!(
        effective.write_scopes,
        vec!["/repo/target/generated".to_string()]
    );
}

/// Verifies read only policy allows safe exact command.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn read_only_policy_allows_safe_exact_command() {
    let policy = PermissionPolicy::default();

    assert_eq!(policy.evaluate_shell_command("pwd"), RuleDecision::Allow);
}

/// Verifies that a basic directory listing is treated as read-only inspection.
/// This covers the common agent action shape for "list the current directory",
/// where the command has no path arguments but still declares a read of `.`.
#[test]
fn ls_policy_allows_common_read_only_listing_forms() {
    let policy = PermissionPolicy::default();

    assert_eq!(policy.evaluate_shell_command("ls"), RuleDecision::Allow);
    assert_eq!(
        policy.evaluate_shell_command("ls -la --color=auto"),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command("ls --sort=size src"),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command("ls --hyperlink=always"),
        RuleDecision::Prompt
    );

    let effects = classify_shell_command("ls -la", None).unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].reads, vec![".".to_string()]);
    assert!(!effects[0].unknown);
}

/// Verifies that listing explicit paths still goes through the scoped path
/// preflight instead of allowing arbitrary reads because `ls` itself is safe.
#[test]
fn ls_path_arguments_must_stay_inside_read_scopes() {
    let policy = PermissionPolicy::default();
    let scopes = shell_resolved_scopes(
        "/repo",
        &["/repo/src"],
        &[],
        &[("src", "/repo/src"), ("../secret", "/secret")],
    );

    assert_eq!(
        policy.evaluate_shell_command_in_scope("ls src", &scopes),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command_in_scope("ls ../secret", &scopes),
        RuleDecision::Prompt
    );
}

/// Verifies permission authority comparison classifies broadening and narrowing.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn permission_authority_comparison_classifies_broadening_and_narrowing() {
    assert_eq!(
        compare_permission_preset_authority(PermissionPreset::ReadOnly, PermissionPreset::Auto),
        PermissionAuthorityChange::Broadening
    );
    assert_eq!(
        compare_permission_preset_authority(PermissionPreset::Auto, PermissionPreset::ReadOnly),
        PermissionAuthorityChange::Narrowing
    );
    assert_eq!(
        compare_approval_policy_authority(ApprovalPolicy::Ask, ApprovalPolicy::FullAccess),
        PermissionAuthorityChange::Broadening
    );
    assert_eq!(
        compare_approval_policy_authority(ApprovalPolicy::AutoAllow, ApprovalPolicy::Ask),
        PermissionAuthorityChange::Narrowing
    );
    assert_eq!(
        compare_approval_policy_authority(ApprovalPolicy::Ask, ApprovalPolicy::AutoAllow),
        PermissionAuthorityChange::Broadening
    );
    assert_eq!(
        compare_approval_policy_authority(ApprovalPolicy::FullAccess, ApprovalPolicy::FullAccess),
        PermissionAuthorityChange::NoChange
    );
}

/// Verifies destructive command prompts without configured deny.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn destructive_command_prompts_without_configured_deny() {
    let policy = PermissionPolicy::default();

    assert_eq!(
        policy.evaluate_shell_command("rm -rf target"),
        RuleDecision::Prompt
    );
}

/// Verifies unclassified shell syntax prompts.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn unclassified_shell_syntax_prompts() {
    let policy = PermissionPolicy::default();

    assert_eq!(
        policy.evaluate_shell_command("echo $(pwd)"),
        RuleDecision::Prompt
    );
}

/// Verifies bypass allows mezzanine gated actions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn bypass_allows_mezzanine_gated_actions() {
    let mut policy = PermissionPolicy::default();
    policy.set_approval_bypass(true);

    assert_eq!(
        policy.evaluate_shell_command("rm -rf target"),
        RuleDecision::Allow
    );
}

/// Verifies session approval store persists prefix decisions in memory.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn session_approval_store_persists_prefix_decisions_in_memory() {
    let mut approvals = SessionApprovalStore::default();
    let id = approvals
        .decide_prefix(
            ["cargo", "test"],
            ApprovalScope::Session,
            ApprovalDecision::Approve,
        )
        .unwrap();

    assert_eq!(id, "ap1");
    assert_eq!(
        approvals.evaluate("cargo test --all-targets"),
        Some(ApprovalDecision::Approve)
    );
    assert_eq!(approvals.grants().count(), 1);
}

/// Verifies session approval store uses most specific prefix.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn session_approval_store_uses_most_specific_prefix() {
    let mut approvals = SessionApprovalStore::default();
    approvals
        .decide_prefix(["cargo"], ApprovalScope::Session, ApprovalDecision::Approve)
        .unwrap();
    approvals
        .decide_prefix(
            ["cargo", "publish"],
            ApprovalScope::Session,
            ApprovalDecision::Disapprove,
        )
        .unwrap();

    assert_eq!(
        approvals.evaluate("cargo publish"),
        Some(ApprovalDecision::Disapprove)
    );
}

/// Verifies policy can evaluate with session approvals.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn policy_can_evaluate_with_session_approvals() {
    let policy = PermissionPolicy::default();
    let mut approvals = SessionApprovalStore::default();
    approvals
        .decide_prefix(["grep"], ApprovalScope::Session, ApprovalDecision::Approve)
        .unwrap();

    assert_eq!(
        policy.evaluate_shell_command_with_approvals("grep needle file", &approvals),
        RuleDecision::Allow
    );
}

/// Verifies approval changes only the command-wide authorization decision and
/// preserves the structured candidates and effects computed by policy.
#[test]
fn structured_approval_preserves_resource_effects() {
    let policy = PermissionPolicy::default();
    let command = "env";
    let before = policy.evaluate_shell_command_structured(command);
    let mut approvals = SessionApprovalStore::default();
    approvals
        .decide_prefix(["env"], ApprovalScope::Session, ApprovalDecision::Approve)
        .unwrap();

    let after =
        policy.evaluate_shell_command_structured_with_approvals_scoped(command, &approvals, None);

    assert_eq!(before.decision, RuleDecision::Prompt);
    assert_eq!(after.decision, RuleDecision::Allow);
    assert_eq!(after.candidates, before.candidates);
    assert_eq!(after.matched_rule_ids, before.matched_rule_ids);
    assert_eq!(after.effects, before.effects);
    assert_eq!(after.completeness, before.completeness);
}

/// Verifies session approvals do not override configured denies.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn session_approvals_do_not_override_configured_denies() {
    let mut policy = PermissionPolicy::default();
    policy.add_rule(
        CommandRule::new(["rm"], RuleDecision::Forbid, RuleMatch::Prefix)
            .unwrap()
            .with_scope(CommandRuleScope::Project),
    );
    let mut approvals = SessionApprovalStore::default();
    approvals
        .decide_prefix(["rm"], ApprovalScope::Session, ApprovalDecision::Approve)
        .unwrap();

    assert_eq!(
        policy.evaluate_shell_command_with_approvals("rm -rf target", &approvals),
        RuleDecision::Forbid
    );
}

/// Verifies that auto-allow leaves generic prompt decisions in place until the
/// agent planner receives a structured model assertion, while explicit user
/// approvals and already-safe commands still resolve normally.
#[test]
fn approval_policy_auto_allow_keeps_prompt_decisions_for_agent_assessment() {
    let policy = PermissionPolicy::default().with_approval_policy(ApprovalPolicy::AutoAllow);
    let mut approvals = SessionApprovalStore::default();
    approvals
        .decide_prefix(["env"], ApprovalScope::Session, ApprovalDecision::Approve)
        .unwrap();

    assert_eq!(policy.evaluate_shell_command("env"), RuleDecision::Prompt);
    assert_eq!(
        policy.evaluate_shell_command_with_approvals("env", &approvals),
        RuleDecision::Allow
    );
    assert_eq!(policy.evaluate_shell_command("pwd"), RuleDecision::Allow);
}

/// Verifies that auto-allow does not convert commands with unproven side
/// effects into generic allows at the permission layer; those commands must
/// stay promptable for agent-side assessment.
#[test]
fn auto_allow_preserves_unproven_effects_for_model_self_assessment() {
    let mut policy = PermissionPolicy::default().with_approval_policy(ApprovalPolicy::AutoAllow);
    policy.trusted_directories.push("/repo".to_string());
    let scopes = shell_resolved_scopes(
        "/repo/project",
        &["/repo"],
        &["/repo"],
        &[("README.md", "/repo/project/README.md")],
    );

    assert_eq!(
        policy.evaluate_shell_command_in_scope("cat README.md", &scopes),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command_in_scope("env", &scopes),
        RuleDecision::Prompt
    );
    assert_eq!(
        policy.evaluate_shell_command_in_scope("cargo test", &scopes),
        RuleDecision::Prompt
    );
    assert_eq!(
        policy.evaluate_shell_command_in_scope("rm -rf target", &scopes),
        RuleDecision::Prompt
    );
}

/// Verifies blocked approval queue tracks pending and decisions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn blocked_approval_queue_tracks_pending_and_decisions() {
    let mut queue = BlockedApprovalQueue::default();
    let id = queue
        .create_at(
            BlockedApprovalRequest {
                id: String::new(),
                requesting_agent_id: "a1".to_string(),
                pane_id: "%1".to_string(),
                parent_agent_chain: vec!["a0".to_string()],
                action_kind: "shell_command".to_string(),
                action_summary: "cargo test".to_string(),
                declared_effects: vec!["read".to_string()],
                matched_rules: vec!["prompt cargo".to_string()],
                read_scopes: vec!["/repo".to_string()],
                write_scopes: vec!["/repo".to_string()],
                cooperation_mode: None,
                created_at_unix_seconds: None,
                decided_at_unix_seconds: None,
                decided_by_client_id: None,
                state: BlockedApprovalState::Approved,
                decision: Some(ApprovalDecision::Approve),
                redirect_instruction: Some("stale".to_string()),
            },
            10,
        )
        .unwrap();

    assert_eq!(id, "ba1");
    assert_eq!(queue.pending().len(), 1);
    assert_eq!(
        queue.get(&id).unwrap().parent_agent_chain,
        vec!["a0".to_string()]
    );
    assert_eq!(queue.get(&id).unwrap().created_at_unix_seconds, Some(10));

    let decided = queue
        .decide_at(&id, ApprovalDecision::Approve, None, 20)
        .unwrap();
    assert_eq!(decided.state, BlockedApprovalState::Approved);
    assert_eq!(decided.decided_at_unix_seconds, Some(20));
    assert!(queue.pending().is_empty());
}

/// Verifies blocked approval redirect requires instruction.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn blocked_approval_redirect_requires_instruction() {
    let mut queue = BlockedApprovalQueue::default();
    let id = queue
        .create_at(
            BlockedApprovalRequest {
                id: String::new(),
                requesting_agent_id: "a1".to_string(),
                pane_id: "%1".to_string(),
                parent_agent_chain: Vec::new(),
                action_kind: "shell_command".to_string(),
                action_summary: "rm target".to_string(),
                declared_effects: vec!["delete".to_string()],
                matched_rules: vec!["deny rm".to_string()],
                read_scopes: Vec::new(),
                write_scopes: vec!["/repo".to_string()],
                cooperation_mode: None,
                created_at_unix_seconds: None,
                decided_at_unix_seconds: None,
                decided_by_client_id: None,
                state: BlockedApprovalState::Pending,
                decision: None,
                redirect_instruction: None,
            },
            10,
        )
        .unwrap();

    let error = queue
        .decide_at(&id, ApprovalDecision::Redirect, None, 20)
        .unwrap_err();
    assert_eq!(error.kind(), PermissionErrorKind::InvalidArgs);

    let redirected = queue
        .decide_at(
            &id,
            ApprovalDecision::Redirect,
            Some("explain why deletion is needed".to_string()),
            30,
        )
        .unwrap();
    assert_eq!(redirected.state, BlockedApprovalState::Redirected);
    assert_eq!(
        redirected.redirect_instruction.as_deref(),
        Some("explain why deletion is needed")
    );
}

/// Verifies command rule store round trips scoped rules.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn command_rule_store_round_trips_scoped_rules() {
    let mut store = CommandRuleStore::new();
    store
        .add(
            CommandRule::new(["git", "status"], RuleDecision::Allow, RuleMatch::Prefix)
                .unwrap()
                .with_scope(CommandRuleScope::User)
                .with_argument_policy(ArgumentPolicy::ReadPaths {
                    allowed_options: vec!["--short".to_string()],
                })
                .with_justification("status inspection")
                .with_id("git-status")
                .unwrap()
                .with_declared_effects(DeclaredCommandEffects {
                    completeness: EffectCompleteness::Complete,
                    read_scopes: vec![".".to_string()],
                    write_scopes: Vec::new(),
                    network: Some(false),
                    credentials: Some(false),
                    process_control: Some(false),
                })
                .unwrap(),
        )
        .unwrap();

    let encoded = store.encode();
    let decoded = CommandRuleStore::decode(&encoded).unwrap();

    assert_eq!(decoded.rules(), store.rules());
    assert!(encoded.contains("user"));
    assert!(encoded.contains("read_paths:--short"));
}

/// Verifies command rule store round trips exact sha256 rules.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn command_rule_store_round_trips_exact_sha256_rules() {
    let normalized = normalize_exact_command_text("printf '%s\\n' \"$USER\"", false);
    let rule = CommandRule::new_exact_sha256(
        &normalized,
        DEFAULT_COMMAND_SHELL_CLASSIFICATION,
        RuleDecision::Allow,
    )
    .unwrap()
    .with_scope(CommandRuleScope::Session);
    let mut store = CommandRuleStore::new();
    store.add(rule).unwrap();

    let decoded = CommandRuleStore::decode(&store.encode()).unwrap();

    assert_eq!(decoded.rules(), store.rules());
}

/// Verifies command rule store removes rules by stable display id.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn command_rule_store_removes_rules_by_stable_display_id() {
    let mut store = CommandRuleStore::new();
    store
        .add(
            CommandRule::new(["git", "status"], RuleDecision::Allow, RuleMatch::Prefix)
                .unwrap()
                .with_scope(CommandRuleScope::User),
        )
        .unwrap();
    store
        .add(
            CommandRule::new(["cargo", "test"], RuleDecision::Prompt, RuleMatch::Prefix)
                .unwrap()
                .with_scope(CommandRuleScope::Session),
        )
        .unwrap();

    let removed = store.remove("rule1").unwrap();

    assert_eq!(removed.pattern, vec!["git", "status"]);
    assert_eq!(store.rules()[0].pattern, vec!["cargo", "test"]);
    assert_eq!(
        store.remove("rule9").unwrap_err().kind(),
        PermissionErrorKind::NotFound
    );
}

/// Verifies exact sha256 rule allows matching unclassified command only.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn exact_sha256_rule_allows_matching_unclassified_command_only() {
    let command = "printf '%s\\n' \"$USER\"";
    let normalized = normalize_exact_command_text(command, false);
    let mut policy = PermissionPolicy::default();
    policy.add_rule(
        CommandRule::new_exact_sha256(
            &normalized,
            DEFAULT_COMMAND_SHELL_CLASSIFICATION,
            RuleDecision::Allow,
        )
        .unwrap(),
    );

    assert_eq!(policy.evaluate_shell_command(command), RuleDecision::Allow);
    assert_eq!(
        policy.evaluate_shell_command("printf '%s\\n' \"$HOME\""),
        RuleDecision::Prompt
    );
    assert_eq!(
        policy.evaluate_shell_command_for_shell_classification(command, "bash-5"),
        RuleDecision::Prompt
    );
}

/// Verifies exact rule allows matching unclassified command only.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn exact_rule_allows_matching_unclassified_command_only() {
    let mut policy = PermissionPolicy::default();
    policy.add_rule(
        CommandRule::new(
            ["cat", "src/lib.rs", ">", "/tmp/copy"],
            RuleDecision::Allow,
            RuleMatch::Exact,
        )
        .unwrap()
        .with_scope(CommandRuleScope::Session),
    );

    assert_eq!(
        policy.evaluate_shell_command("cat src/lib.rs > /tmp/copy"),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command("cat src/lib.rs > /tmp/other"),
        RuleDecision::Prompt
    );
    assert_eq!(
        policy.evaluate_shell_command("cat src/lib.rs > /tmp/copy; rm -rf target"),
        RuleDecision::Prompt
    );
}

/// Verifies user prefix allow rule whitelists matching command tree.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn user_prefix_allow_rule_whitelists_matching_command_tree() {
    let mut policy = PermissionPolicy::default();
    policy.add_rule(
        CommandRule::new(["cargo", "test"], RuleDecision::Allow, RuleMatch::Prefix)
            .unwrap()
            .with_scope(CommandRuleScope::Session),
    );

    assert_eq!(
        policy.evaluate_shell_command("cargo test --all-targets"),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command("cargo build"),
        RuleDecision::Prompt
    );
}

/// Verifies exact sha256 normalization preserves contents except line endings.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn exact_sha256_normalization_preserves_contents_except_line_endings() {
    assert_eq!(
        normalize_exact_command_text("echo one\r\necho two\r", true),
        "echo one\necho two"
    );
    assert_eq!(
        normalize_exact_command_text("echo one\n\n", true),
        "echo one\n"
    );
}

/// Verifies sha256 implementation matches known digest.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn sha256_implementation_matches_known_digest() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// Verifies command rule store rejects built in persistence.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn command_rule_store_rejects_built_in_persistence() {
    let mut store = CommandRuleStore::new();
    let error = store
        .add(
            CommandRule::new(["pwd"], RuleDecision::Allow, RuleMatch::Exact)
                .unwrap()
                .with_scope(CommandRuleScope::BuiltIn),
        )
        .unwrap_err();

    assert_eq!(error.kind(), PermissionErrorKind::InvalidArgs);
}

/// Verifies git status allows only safe status arguments.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn git_status_allows_only_safe_status_arguments() {
    let policy = PermissionPolicy::default();

    assert_eq!(
        policy.evaluate_shell_command("git status --short --branch"),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command("git status --output=report"),
        RuleDecision::Prompt
    );
}

/// Verifies git read only rules allow common inspection and reject writers.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn git_read_only_rules_allow_common_inspection_and_reject_writers() {
    let policy = PermissionPolicy::default();
    let scopes = shell_resolved_scopes(
        "/repo",
        &["/repo/src"],
        &[],
        &[("src/lib.rs", "/repo/src/lib.rs")],
    );

    assert_eq!(
        policy.evaluate_shell_command_in_scope("git diff -- src/lib.rs", &scopes),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command_in_scope("git log --oneline -- src/lib.rs", &scopes),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command_in_scope("git show HEAD:src/lib.rs", &scopes),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command("git rev-parse --show-toplevel"),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command_in_scope("git diff -- ../secret.txt", &scopes),
        RuleDecision::Prompt
    );
    assert_eq!(
        policy.evaluate_shell_command("git diff --output=patch.txt"),
        RuleDecision::Prompt
    );
    assert_eq!(
        policy.evaluate_shell_command("git show --ext-diff HEAD"),
        RuleDecision::Prompt
    );
    assert_eq!(
        policy.evaluate_shell_command("git log --paginate"),
        RuleDecision::Prompt
    );
    assert_eq!(
        policy.evaluate_shell_command("git log -p"),
        RuleDecision::Prompt
    );
}

/// Verifies printf literal output is allowed without expansion.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn printf_literal_output_is_allowed_without_expansion() {
    let policy = PermissionPolicy::default();

    assert_eq!(
        policy.evaluate_shell_command("printf '%s\\n' ready"),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command("printf '%s\\n' \"$USER\""),
        RuleDecision::Prompt
    );
    assert_eq!(
        policy.evaluate_shell_command("printf"),
        RuleDecision::Prompt
    );
}

/// Verifies text processing rules allow only read only forms.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn text_processing_rules_allow_only_read_only_forms() {
    let policy = PermissionPolicy::default();
    let scopes = shell_resolved_scopes(
        "/repo",
        &["/repo/src"],
        &["/repo/src"],
        &[("src/lib.rs", "/repo/src/lib.rs")],
    );

    assert_eq!(
        policy.evaluate_shell_command_in_scope("sed -n '1,3p' src/lib.rs", &scopes),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command_in_scope("awk '{print $1}' src/lib.rs", &scopes),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command_in_scope("sed -i 's/a/b/' src/lib.rs", &scopes),
        RuleDecision::Prompt
    );
}

/// Verifies find rule rejects exec and delete actions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn find_rule_rejects_exec_and_delete_actions() {
    let policy = PermissionPolicy::default();
    let scopes = shell_resolved_scopes("/repo", &["/repo/src"], &[], &[("src", "/repo/src")]);

    assert_eq!(
        policy.evaluate_shell_command_in_scope(
            "find src -maxdepth 2 -type f -name '*.rs' -print",
            &scopes,
        ),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command_in_scope("find src -delete", &scopes),
        RuleDecision::Prompt
    );
    assert_eq!(
        policy.evaluate_shell_command_in_scope("find src -exec rm {} ';'", &scopes),
        RuleDecision::Prompt
    );
}

/// Verifies env and xargs require approval by default.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn env_and_xargs_require_approval_by_default() {
    let policy = PermissionPolicy::default();

    assert_eq!(policy.evaluate_shell_command("env"), RuleDecision::Prompt);
    assert_eq!(
        policy.evaluate_shell_command("xargs rm"),
        RuleDecision::Prompt
    );
}

/// Verifies builtin discovery rules allow single executable probe.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn builtin_discovery_rules_allow_single_executable_probe() {
    let policy = PermissionPolicy::default();

    assert_eq!(
        policy.evaluate_shell_command("command -v rg"),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command("which python3"),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command("type -a cargo-clippy"),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command("command -v 'rg;rm'"),
        RuleDecision::Prompt
    );
}

/// Verifies builtin uname allows safe system probe options.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn builtin_uname_allows_safe_system_probe_options() {
    let policy = PermissionPolicy::default();

    assert_eq!(policy.evaluate_shell_command("uname"), RuleDecision::Allow);
    assert_eq!(
        policy.evaluate_shell_command("uname -s -m"),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command("uname -sm"),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command("uname --operating-system"),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command("uname --help"),
        RuleDecision::Prompt
    );
}

/// Verifies read path arguments must stay inside read scopes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn read_path_arguments_must_stay_inside_read_scopes() {
    let policy = PermissionPolicy::default();
    let scopes = shell_resolved_scopes(
        "/repo",
        &["/repo/src"],
        &["/repo/src"],
        &[("src/lib.rs", "/repo/src/lib.rs")],
    );

    assert_eq!(
        policy.evaluate_shell_command_in_scope("cat src/lib.rs", &scopes),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command_in_scope("cat ../secrets.txt", &scopes),
        RuleDecision::Prompt
    );
}

/// Verifies that scoped read decisions use shell-canonical path evidence rather
/// than lexical path prefixes. A symlink-like path that lexically sits under the
/// read scope must still prompt when the shell-resolved target escapes.
#[test]
fn shell_resolved_canonical_escape_prompts_for_scoped_reads() {
    let policy = PermissionPolicy::default();
    let scopes = shell_resolved_scopes(
        "/repo",
        &["/repo"],
        &[],
        &[("link/secret.txt", "/outside/secret.txt")],
    );

    assert_eq!(
        policy.evaluate_shell_command_in_scope("cat link/secret.txt", &scopes),
        RuleDecision::Prompt
    );
}

/// Verifies that a previous command-prefix approval cannot suppress a fresh
/// scoped path preflight. This keeps a changed cwd or canonical path from
/// reusing stale approval state to escape active scopes.
#[test]
fn session_approval_does_not_override_scoped_path_prompt() {
    let policy = PermissionPolicy::default();
    let mut approvals = SessionApprovalStore::default();
    approvals
        .decide_prefix(["cat"], ApprovalScope::Session, ApprovalDecision::Approve)
        .unwrap();
    let scopes = shell_resolved_scopes(
        "/repo",
        &["/repo"],
        &[],
        &[("link/secret.txt", "/outside/secret.txt")],
    );

    assert_eq!(
        policy.evaluate_shell_command_with_approvals_scoped(
            "cat link/secret.txt",
            &approvals,
            Some(&scopes),
        ),
        RuleDecision::Prompt
    );
}

/// Verifies that scoped path checks fail closed when the pane shell has not
/// provided canonical path evidence. Auto-allow must leave that prompt intact
/// until the agent planner receives a structured model-side reasonableness
/// assertion for the active request.
#[test]
fn unresolved_path_scopes_fail_closed_for_auto_allow() {
    let policy = PermissionPolicy::default();
    let scopes = PathScopes::unresolved(
        "/repo",
        vec!["/repo".to_string()],
        vec!["/repo".to_string()],
    );

    assert_eq!(
        policy.evaluate_shell_command_in_scope("cat src/lib.rs", &scopes),
        RuleDecision::Prompt
    );
    assert_eq!(
        policy
            .with_approval_policy(ApprovalPolicy::AutoAllow)
            .evaluate_shell_command_in_scope("cat src/lib.rs", &scopes),
        RuleDecision::Prompt
    );
}

/// Verifies command candidates split at control operators.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn command_candidates_split_at_control_operators() {
    let policy = PermissionPolicy::default();

    assert_eq!(
        policy.evaluate_shell_command("pwd; hostname"),
        RuleDecision::Allow
    );
    assert_eq!(
        policy.evaluate_shell_command("pwd; rm -rf target"),
        RuleDecision::Prompt
    );
}

/// Verifies unsafe shell syntax requires prompt.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn unsafe_shell_syntax_requires_prompt() {
    let policy = PermissionPolicy::default();

    assert_eq!(
        policy.evaluate_shell_command("cat src/lib.rs > copy"),
        RuleDecision::Prompt
    );
    assert_eq!(
        policy.evaluate_shell_command("echo $(pwd)"),
        RuleDecision::Prompt
    );
}

/// Verifies quoted path arguments are classified.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn quoted_path_arguments_are_classified() {
    let policy = PermissionPolicy::default();
    let scopes = shell_resolved_scopes(
        "/repo",
        &["/repo"],
        &[],
        &[("src/main file.rs", "/repo/src/main file.rs")],
    );

    assert_eq!(
        policy.evaluate_shell_command_in_scope("cat 'src/main file.rs'", &scopes),
        RuleDecision::Allow
    );
}

/// Verifies broad interpreters classify as unknown effects.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn broad_interpreters_classify_as_unknown_effects() {
    let effects = classify_shell_command("python3 -c 'print(1)'", None).unwrap();

    assert_eq!(effects.len(), 1);
    assert!(effects[0].unknown);
}

/// Verifies structured evaluation retains the stable rule identity and the
/// complete resource requirements declared for an otherwise broad command.
#[test]
fn structured_evaluation_retains_complete_declared_effects() {
    let mut policy = PermissionPolicy::default();
    policy.add_rule(
        CommandRule::new(["cargo", "test"], RuleDecision::Allow, RuleMatch::Prefix)
            .unwrap()
            .with_scope(CommandRuleScope::Session)
            .with_id("cargo-test")
            .unwrap()
            .with_declared_effects(DeclaredCommandEffects {
                completeness: EffectCompleteness::Complete,
                read_scopes: vec!["src".to_string()],
                write_scopes: vec!["target".to_string()],
                network: Some(false),
                credentials: Some(false),
                process_control: Some(false),
            })
            .unwrap(),
    );

    let evaluation = policy.evaluate_shell_command_structured("cargo test --all-targets");

    assert_eq!(evaluation.decision, RuleDecision::Allow);
    assert_eq!(evaluation.candidates.len(), 1);
    assert_eq!(evaluation.candidates[0].matched_rule_ids, ["cargo-test"]);
    assert_eq!(
        evaluation.candidates[0].completeness,
        EffectCompleteness::Complete
    );
    assert_eq!(evaluation.effects.reads, ["src"]);
    assert_eq!(evaluation.effects.writes, ["target"]);
    assert!(!evaluation.effects.unknown);
}

/// Verifies one incomplete candidate makes filesystem narrowing unknown while
/// known security-relevant facts from another candidate remain available.
#[test]
fn structured_evaluation_preserves_known_facts_across_unknown_candidates() {
    let mut policy = PermissionPolicy::default();
    policy.add_rule(
        CommandRule::new(["curl"], RuleDecision::Allow, RuleMatch::Prefix)
            .unwrap()
            .with_scope(CommandRuleScope::Session)
            .with_id("network-client")
            .unwrap()
            .with_declared_effects(DeclaredCommandEffects {
                completeness: EffectCompleteness::Complete,
                read_scopes: Vec::new(),
                write_scopes: Vec::new(),
                network: Some(true),
                credentials: Some(false),
                process_control: Some(false),
            })
            .unwrap(),
    );
    policy.add_rule(
        CommandRule::new(["python3"], RuleDecision::Allow, RuleMatch::Prefix)
            .unwrap()
            .with_scope(CommandRuleScope::Session)
            .with_id("legacy-python")
            .unwrap(),
    );

    let evaluation = policy.evaluate_shell_command_structured("curl example.test | python3");

    assert_eq!(evaluation.decision, RuleDecision::Allow);
    assert_eq!(evaluation.candidates.len(), 2);
    assert_eq!(evaluation.completeness, EffectCompleteness::Unknown);
    assert!(evaluation.effects.unknown);
    assert!(evaluation.effects.network);
    assert_eq!(
        evaluation.matched_rule_ids,
        ["legacy-python", "network-client"]
    );
}

/// Verifies one structured evaluation retains stable rule identity and uses a
/// complete declaration to replace the classifier's conservative unknown for
/// a broad command family.
#[test]
fn structured_permission_evaluation_uses_complete_declared_effects() {
    let mut policy = PermissionPolicy::default();
    policy.add_rule(
        CommandRule::new(["cargo", "test"], RuleDecision::Allow, RuleMatch::Prefix)
            .unwrap()
            .with_scope(CommandRuleScope::User)
            .with_id("cargo-test")
            .unwrap()
            .with_declared_effects(DeclaredCommandEffects {
                completeness: EffectCompleteness::Complete,
                read_scopes: vec![".".to_string()],
                write_scopes: vec!["target".to_string()],
                network: Some(false),
                credentials: Some(false),
                process_control: Some(false),
            })
            .unwrap(),
    );

    let evaluation = policy.evaluate_shell_command_structured("cargo test --all-targets");

    assert_eq!(evaluation.decision, RuleDecision::Allow);
    assert_eq!(evaluation.completeness, EffectCompleteness::Complete);
    assert_eq!(evaluation.candidates.len(), 1);
    assert_eq!(evaluation.candidates[0].command, "cargo test --all-targets");
    assert_eq!(evaluation.candidates[0].matched_rule_ids, ["cargo-test"]);
    assert_eq!(evaluation.effects.reads, ["."]);
    assert_eq!(evaluation.effects.writes, ["target"]);
    assert!(!evaluation.effects.unknown);
}

/// Verifies an undeclared allow rule preserves its authorization decision but
/// leaves filesystem narrowing unknown rather than trusting the command name.
#[test]
fn structured_permission_evaluation_keeps_undeclared_rule_effects_unknown() {
    let mut policy = PermissionPolicy::default();
    policy.add_rule(
        CommandRule::new(["cargo", "test"], RuleDecision::Allow, RuleMatch::Prefix)
            .unwrap()
            .with_scope(CommandRuleScope::User)
            .with_id("cargo-test")
            .unwrap(),
    );

    let evaluation = policy.evaluate_shell_command_structured("cargo test");

    assert_eq!(evaluation.decision, RuleDecision::Allow);
    assert_eq!(evaluation.completeness, EffectCompleteness::Unknown);
    assert_eq!(evaluation.candidates[0].matched_rule_ids, ["cargo-test"]);
    assert!(evaluation.effects.unknown);
}
