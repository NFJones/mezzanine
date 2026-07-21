//! Pure regression coverage for Bubblewrap policy compilation.

#[cfg(target_os = "linux")]
mod real_bubblewrap;

use std::collections::BTreeMap;

use mez_agent::permissions::{
    CandidateEvaluation, EffectCompleteness, EffectiveCommandEffects, PathScopes,
    PermissionEvaluation, ResolvedPathEvidence, ResolvedPathKind, RuleDecision,
};

use super::*;

fn config() -> BubblewrapConfig {
    BubblewrapConfig {
        executable: "/usr/bin/bwrap".to_string(),
        unavailable: SandboxUnavailablePolicy::Fail,
        network: BubblewrapNetworkMode::Isolated,
        environment: SandboxEnvironmentPolicy::Minimal,
    }
}

fn effects() -> EffectiveCommandEffects {
    EffectiveCommandEffects {
        reads: Vec::new(),
        writes: Vec::new(),
        creates: Vec::new(),
        deletes: Vec::new(),
        touches: Vec::new(),
        network: false,
        credentials: false,
        process_control: false,
        destructive: false,
        privilege_change: false,
        unknown: false,
    }
}

fn evaluation(
    completeness: EffectCompleteness,
    effects: EffectiveCommandEffects,
) -> PermissionEvaluation {
    PermissionEvaluation {
        decision: RuleDecision::Allow,
        candidates: vec![CandidateEvaluation {
            command: "cargo test".to_string(),
            decision: RuleDecision::Allow,
            matched_rule_ids: vec!["cargo-test".to_string()],
            effects: effects.clone(),
            completeness,
        }],
        matched_rule_ids: vec!["cargo-test".to_string()],
        effects,
        completeness,
    }
}

fn authority() -> PathScopes {
    let mut evidence = BTreeMap::new();
    for path in [".", "src", "target"] {
        let canonical = match path {
            "." => "/workspace",
            "src" => "/workspace/src",
            "target" => "/workspace/target",
            _ => unreachable!(),
        };
        evidence.insert(
            path.to_string(),
            ResolvedPathEvidence {
                canonical_path: canonical.to_string(),
                kind: ResolvedPathKind::Existing,
                nearest_existing_parent: canonical.to_string(),
            },
        );
    }
    PathScopes::try_shell_resolved_with_evidence(
        "/workspace",
        vec!["/workspace".to_string()],
        vec!["/workspace/target".to_string()],
        evidence,
    )
    .unwrap()
}

/// Builds pane-resolved authority rooted at one synthetic user home.
fn home_authority(home: &str) -> PathScopes {
    let mut evidence = BTreeMap::new();
    for protected in [".ssh", ".gnupg", ".aws", ".azure", ".kube", ".docker"] {
        let canonical = format!("{home}/{protected}");
        evidence.insert(
            canonical.clone(),
            ResolvedPathEvidence {
                canonical_path: canonical.clone(),
                kind: ResolvedPathKind::Existing,
                nearest_existing_parent: canonical,
            },
        );
    }
    PathScopes::try_shell_resolved_with_evidence(home, vec![home.to_string()], Vec::new(), evidence)
        .unwrap()
}

fn request<'a>(
    config: &'a BubblewrapConfig,
    authority: &'a PathScopes,
    evaluation: &'a PermissionEvaluation,
) -> BubblewrapCompileRequest<'a> {
    BubblewrapCompileRequest {
        config,
        capability: capability(config),
        pane_environment_signature: "pane-env-sha256",
        network_policy: NetworkPolicy::Prompt,
        maximum_authority: authority,
        permission_evaluation: evaluation,
        child_shell_path: "/bin/sh",
        command_file_host_path: BUBBLEWRAP_COMMAND_FILE_HOST_PLACEHOLDER,
        stateful: false,
        interactive: false,
    }
}

fn capability(config: &BubblewrapConfig) -> BubblewrapCapability {
    let plan = bubblewrap_capability_probe_plan(config, "/bin/sh").unwrap();
    parse_bubblewrap_capability_probe("pane-env-sha256", &plan, 0, plan.expected_stdout).unwrap()
}

/// Prompt evaluations may compile for sandbox-first execution, while hard
/// forbids remain terminal and cannot produce a Bubblewrap launch plan.
#[test]
fn sandbox_compiler_accepts_prompts_and_rejects_forbids() {
    let config = config();
    let authority = authority();
    let mut prompt = evaluation(EffectCompleteness::Unknown, effects());
    prompt.decision = RuleDecision::Prompt;

    compile_bubblewrap_launch_plan(request(&config, &authority, &prompt)).unwrap();

    let mut forbid = prompt;
    forbid.decision = RuleDecision::Forbid;
    let error = compile_bubblewrap_launch_plan(request(&config, &authority, &forbid)).unwrap_err();
    assert_eq!(error.kind(), SandboxCompileErrorKind::Unauthorized);
}

/// Unknown effects retain configured maximum authority without exposing host
/// root, host networking, IPC sockets, or inherited environment variables.
#[test]
fn unknown_effects_compile_to_bounded_maximum_authority() {
    let config = config();
    let authority = authority();
    let mut unknown = effects();
    unknown.unknown = true;
    let evaluation = evaluation(EffectCompleteness::Unknown, unknown);

    let plan = compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap();

    assert_eq!(
        plan.audit_summary.authority_source,
        SandboxAuthoritySource::Maximum
    );
    assert_eq!(plan.audit_summary.read_only_mount_count, 1);
    assert_eq!(plan.audit_summary.read_write_mount_count, 1);
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", "/workspace", "/workspace"])
    );
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--bind", "/workspace/target", "/workspace/target"])
    );
    assert!(plan.arguments.contains(&"--unshare-net".to_string()));
    assert!(plan.arguments.contains(&"--disable-userns".to_string()));
    assert!(plan.arguments.contains(&"--clearenv".to_string()));
    assert!(
        !plan
            .arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", "/", "/"])
    );
    assert!(
        !plan
            .arguments
            .iter()
            .any(|argument| argument.starts_with("/run/user"))
    );
}

/// Broad deterministic user-home authority keeps ordinary files available but
/// masks every direct credential directory after the parent host bind.
#[test]
fn user_home_authority_emits_credential_masks_after_host_mounts() {
    let config = config();
    let authority = home_authority("/home/alice");
    let mut unknown = effects();
    unknown.unknown = true;
    let evaluation = evaluation(EffectCompleteness::Unknown, unknown);

    let plan = compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap();
    assert_eq!(plan.audit_summary.protected_mask_count, 6);
    let parent_mount = plan
        .arguments
        .windows(3)
        .position(|args| args == ["--ro-bind", "/home/alice", "/home/alice"])
        .unwrap();
    for protected in [".ssh", ".gnupg", ".aws", ".azure", ".kube", ".docker"] {
        let destination = format!("/home/alice/{protected}");
        let mask = plan
            .arguments
            .windows(2)
            .position(|args| args == ["--tmpfs", destination.as_str()])
            .unwrap();
        assert!(parent_mount < mask, "mask must follow its parent host bind");
    }
}

/// Complete effects that narrow to a deterministic user home retain the same
/// credential masks as maximum-authority compilation.
#[test]
fn narrowed_user_home_authority_retains_credential_masks() {
    let config = config();
    let authority = home_authority("/home/alice");
    let mut complete = effects();
    complete.reads.push(".".to_string());
    let evaluation = evaluation(EffectCompleteness::Complete, complete);

    let plan = compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap();

    assert_eq!(
        plan.audit_summary.authority_source,
        SandboxAuthoritySource::Narrowed
    );
    assert!(
        plan.arguments
            .windows(2)
            .any(|args| { args == ["--tmpfs", "/home/alice/.ssh"] })
    );
}

/// Complete effects cannot bypass protected descendant masking by narrowing
/// command authority directly to a credential directory.
#[test]
fn narrowed_credential_directory_authority_fails_closed() {
    let config = config();
    let authority = home_authority("/home/alice");
    let mut complete = effects();
    complete.reads.push(".ssh".to_string());
    let evaluation = evaluation(EffectCompleteness::Complete, complete);

    let error =
        compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap_err();

    assert_eq!(error.kind(), SandboxCompileErrorKind::ForbiddenHostPath);
}

/// Multi-user home roots cannot be protected by deterministic direct-child
/// masks and therefore fail closed before a launch plan is produced.
#[test]
fn multi_user_home_authority_fails_closed() {
    let config = config();
    let authority = home_authority("/home");
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());

    let error =
        compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap_err();

    assert_eq!(error.kind(), SandboxCompileErrorKind::ForbiddenHostPath);
}

/// Direct credential-directory authority remains forbidden even though broad
/// deterministic parents are projected with protected descendant masks.
#[test]
fn direct_credential_directory_authority_fails_closed() {
    let config = config();
    let authority = home_authority("/home/alice/.ssh");
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());

    let error =
        compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap_err();

    assert_eq!(error.kind(), SandboxCompileErrorKind::ForbiddenHostPath);
}

/// Complete effects narrow mounts to resolved paths and produce deterministic
/// argv and hashes for identical typed inputs.
#[test]
fn complete_effects_narrow_and_hash_deterministically() {
    let config = config();
    let authority = authority();
    let mut complete = effects();
    complete.reads.push("src".to_string());
    complete.writes.push("target".to_string());
    let evaluation = evaluation(EffectCompleteness::Complete, complete);

    let first = compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap();
    let second = compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap();

    assert_eq!(first, second);
    assert_eq!(first.audit_summary.plan_sha256.len(), 64);
    assert_eq!(
        first.audit_summary.authority_source,
        SandboxAuthoritySource::Narrowed
    );
    assert!(
        first
            .arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", "/workspace/src", "/workspace/src"])
    );
    assert!(
        first
            .arguments
            .windows(3)
            .any(|args| args == ["--bind", "/workspace/target", "/workspace/target"])
    );
    assert!(
        !first
            .arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", "/workspace", "/workspace"])
    );
}

/// A nested read-only effect remains mounted after a writable parent so the
/// more-specific mount can narrow access instead of being discarded.
#[test]
fn nested_read_only_effect_survives_writable_parent() {
    let config = config();
    let mut authority = authority();
    authority.write_scopes = vec!["/workspace".to_string()];
    let mut complete = effects();
    complete.reads.push("src".to_string());
    complete.writes.push(".".to_string());
    let evaluation = evaluation(EffectCompleteness::Complete, complete);

    let plan = compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap();
    let writable_parent = plan
        .arguments
        .windows(3)
        .position(|args| args == ["--bind", "/workspace", "/workspace"])
        .unwrap();
    let read_only_child = plan
        .arguments
        .windows(3)
        .position(|args| args == ["--ro-bind", "/workspace/src", "/workspace/src"])
        .unwrap();

    assert!(writable_parent < read_only_child);
}

/// Capability probes exercise the same fixed runtime profile and are accepted
/// only for exact success output in a named pane environment.
#[test]
fn capability_probe_is_deterministic_and_environment_bound() {
    let config = config();
    let plan = bubblewrap_capability_probe_plan(&config, "/bin/sh").unwrap();

    assert_eq!(plan.executable, "/usr/bin/bwrap");
    assert!(plan.arguments.contains(&"--unshare-net".to_string()));
    assert!(plan.arguments.contains(&"--disable-userns".to_string()));
    assert!(plan.arguments.contains(&"--clearenv".to_string()));
    assert!(
        plan.arguments
            .iter()
            .any(|argument| argument.contains("/etc/passwd"))
    );
    let capability = parse_bubblewrap_capability_probe(
        "pane-env-sha256",
        &plan,
        0,
        "mez-bubblewrap-capability-v1\n",
    )
    .unwrap();
    assert_eq!(
        capability.cache_key.runtime_profile_version,
        BUBBLEWRAP_RUNTIME_PROFILE_VERSION
    );
    assert_eq!(capability.cache_key.executable, "/usr/bin/bwrap");
    assert_eq!(
        capability.cache_key.pane_environment_signature,
        "pane-env-sha256"
    );

    assert_eq!(
        parse_bubblewrap_capability_probe("pane-env-sha256", &plan, 1, "")
            .unwrap_err()
            .kind(),
        SandboxCompileErrorKind::CapabilityProbeFailed
    );

    let authority = authority();
    let evaluation = evaluation(EffectCompleteness::Complete, effects());
    let mut mismatched = request(&config, &authority, &evaluation);
    mismatched.pane_environment_signature = "different-pane-environment";
    assert_eq!(
        compile_bubblewrap_launch_plan(mismatched)
            .unwrap_err()
            .kind(),
        SandboxCompileErrorKind::CapabilityProbeFailed
    );
}

/// Complete effects never widen maximum authority, even when path evidence is
/// otherwise trusted and canonical.
#[test]
fn complete_effects_outside_authority_fail_closed() {
    let config = config();
    let mut authority = authority();
    authority.path_evidence.insert(
        "../sibling".to_string(),
        ResolvedPathEvidence {
            canonical_path: "/sibling".to_string(),
            kind: ResolvedPathKind::Existing,
            nearest_existing_parent: "/sibling".to_string(),
        },
    );
    let mut complete = effects();
    complete.reads.push("../sibling".to_string());
    let evaluation = evaluation(EffectCompleteness::Complete, complete);

    let error =
        compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap_err();

    assert_eq!(
        error.kind(),
        SandboxCompileErrorKind::EffectOutsideAuthority
    );
}

/// Create targets mount only their nearest existing writable parent, retaining
/// fail-closed canonical containment.
#[test]
fn create_targets_mount_nearest_existing_parent() {
    let config = config();
    let mut authority = authority();
    authority.path_evidence.insert(
        "target/new/output.txt".to_string(),
        ResolvedPathEvidence {
            canonical_path: "/workspace/target/new/output.txt".to_string(),
            kind: ResolvedPathKind::CreateTarget,
            nearest_existing_parent: "/workspace/target".to_string(),
        },
    );
    let mut complete = effects();
    complete.creates.push("target/new/output.txt".to_string());
    let evaluation = evaluation(EffectCompleteness::Complete, complete);

    let plan = compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap();

    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--bind", "/workspace/target", "/workspace/target"])
    );
    assert!(
        !plan
            .arguments
            .iter()
            .any(|argument| argument == "/workspace/target/new/output.txt")
    );
}

/// Network, credential, process-control, stateful, and interactive
/// requirements fail before launch rather than weakening confinement.
#[test]
fn unsupported_requirements_fail_before_launch() {
    let config = config();
    let authority = authority();
    let mut network = effects();
    network.network = true;
    let network = evaluation(EffectCompleteness::Complete, network);
    let error = compile_bubblewrap_launch_plan(request(&config, &authority, &network)).unwrap_err();
    assert_eq!(
        error.kind(),
        SandboxCompileErrorKind::MediatedNetworkUnavailable
    );

    let mut credentials = effects();
    credentials.credentials = true;
    let credentials = evaluation(EffectCompleteness::Complete, credentials);
    let error =
        compile_bubblewrap_launch_plan(request(&config, &authority, &credentials)).unwrap_err();
    assert_eq!(
        error.kind(),
        SandboxCompileErrorKind::UnsupportedRequirement
    );

    let safe = evaluation(EffectCompleteness::Complete, effects());
    let mut stateful = request(&config, &authority, &safe);
    stateful.stateful = true;
    assert_eq!(
        compile_bubblewrap_launch_plan(stateful).unwrap_err().kind(),
        SandboxCompileErrorKind::UnsupportedRequirement
    );
    let mut interactive = request(&config, &authority, &safe);
    interactive.interactive = true;
    assert_eq!(
        compile_bubblewrap_launch_plan(interactive)
            .unwrap_err()
            .kind(),
        SandboxCompileErrorKind::UnsupportedRequirement
    );
}

/// Unresolved authority and forbidden host projections fail without producing
/// any launch plan or policy-only fallback.
#[test]
fn unresolved_and_forbidden_authority_fail_closed() {
    let config = config();
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let unresolved = PathScopes::unresolved(
        "/workspace",
        vec!["/workspace".to_string()],
        vec!["/workspace".to_string()],
    );
    assert_eq!(
        compile_bubblewrap_launch_plan(request(&config, &unresolved, &evaluation))
            .unwrap_err()
            .kind(),
        SandboxCompileErrorKind::UnresolvedAuthority
    );

    let root =
        PathScopes::try_shell_resolved("/", vec!["/".to_string()], Vec::new(), BTreeMap::new())
            .unwrap();
    assert_eq!(
        compile_bubblewrap_launch_plan(request(&config, &root, &evaluation))
            .unwrap_err()
            .kind(),
        SandboxCompileErrorKind::ForbiddenHostPath
    );
}
