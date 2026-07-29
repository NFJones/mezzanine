//! Pure regression coverage for Bubblewrap policy compilation.

#[cfg(target_os = "linux")]
mod real_bubblewrap;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

use mez_agent::permissions::{
    CandidateEvaluation, EffectCompleteness, EffectiveCommandEffects, PathScopes,
    PermissionEvaluation, ResolvedPathEvidence, ResolvedPathKind, RuleDecision,
};

use crate::runtime::{CustomToolchainDefinition, CustomToolchainReference, ToolchainSelection};

use super::*;

fn config() -> BubblewrapConfig {
    BubblewrapConfig {
        executable: "/usr/bin/bwrap".to_string(),
        unavailable: SandboxUnavailablePolicy::Fail,
        network: BubblewrapNetworkMode::Isolated,
        environment: SandboxEnvironmentPolicy::Minimal,
        git_user_name: None,
        git_user_email: None,
        toolchains: Vec::new(),
        toolchain_selections: Vec::new(),
        custom_toolchains: BTreeMap::new(),
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
    let confinement_effects =
        (completeness == EffectCompleteness::Complete).then_some(effects.clone());
    PermissionEvaluation {
        decision: RuleDecision::Allow,
        candidates: vec![CandidateEvaluation {
            command: "cargo test".to_string(),
            decision: RuleDecision::Allow,
            matched_rule_ids: vec!["cargo-test".to_string()],
            effects: effects.clone(),
            confinement_effects: confinement_effects.clone(),
            completeness,
        }],
        matched_rule_ids: vec!["cargo-test".to_string()],
        effects,
        confinement_effects,
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
    for protected in [
        ".ssh",
        ".gnupg",
        ".aws",
        ".azure",
        ".kube",
        ".docker",
        ".config/mezzanine",
    ] {
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
        preserve_maximum_authority: false,
        child_shell_path: "/bin/sh",
        command_file_host_path: BUBBLEWRAP_COMMAND_FILE_HOST_PLACEHOLDER,
        managed_home_host_path: None,
        toolchain_projection: None,
        stateful: false,
        interactive: false,
    }
}

fn capability(config: &BubblewrapConfig) -> BubblewrapCapability {
    let plan = bubblewrap_capability_probe_plan(config, "/bin/sh").unwrap();
    parse_bubblewrap_capability_probe("%1", "pane-env-sha256", 0, &plan, 0, plan.expected_stdout)
        .unwrap()
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

/// Trusted Bubblewrap status requires ordered typed lifecycle documents and
/// treats absence of an exit-code event as no proof of payload execution.
#[test]
fn bubblewrap_status_parser_validates_payload_execution_evidence() {
    let complete =
        parse_bubblewrap_status("{\"child-pid\":42,\"mnt-namespace\":7}\n{\"exit-code\":9}\n")
            .unwrap();
    assert_eq!(complete.child_pid, Some(42));
    assert_eq!(complete.exit_code, Some(9));

    let pre_exec = parse_bubblewrap_status("{\"child-pid\":42}\n").unwrap();
    assert_eq!(pre_exec.exit_code, None);
    assert!(parse_bubblewrap_status("{\"exit-code\":0}\n").is_err());
    assert!(parse_bubblewrap_status("{\"child-pid\":42}\n{\"child-pid\":43}\n").is_err());
    assert!(parse_bubblewrap_status("not-json\n").is_err());
}

/// Live Bubblewrap failures provide one concise, authority-preserving command
/// that expands into the existing structured sandbox diagnostics and remedies.
#[test]
fn bubblewrap_failure_remediation_points_to_verbose_status() {
    assert_eq!(
        bubblewrap_failure_remediation("Bubblewrap probe failed."),
        "Bubblewrap probe failed. Run `mez sandbox status --verbose` to inspect the executable, authority, and configuration remedies."
    );
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

/// Verifies every sandbox launch projects the fixed host runtime inputs,
/// including Debian-style executable alternatives, independent of whether the
/// workload receives a private network namespace.
#[test]
fn network_support_files_are_projected_for_every_network_policy() {
    let config = config();
    let authority = authority();
    let evaluation = evaluation(EffectCompleteness::Complete, effects());

    for network_policy in [
        NetworkPolicy::Deny,
        NetworkPolicy::Prompt,
        NetworkPolicy::Allow,
    ] {
        let mut compile_request = request(&config, &authority, &evaluation);
        compile_request.network_policy = network_policy;
        let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();

        assert!(
            plan.arguments
                .windows(2)
                .any(|args| args == ["--dir", "/etc/ssl"]),
            "missing TLS certificate parent directory with {network_policy:?} policy"
        );
        for path in [
            "/etc/alternatives",
            "/etc/ssl/certs",
            "/etc/resolv.conf",
            "/etc/nsswitch.conf",
            "/etc/hosts",
        ] {
            assert!(
                plan.arguments
                    .windows(3)
                    .any(|args| { args == ["--ro-bind-try", path, path] }),
                "missing network support projection for {path} with {network_policy:?} policy"
            );
        }
    }
}

/// Verifies classifier-observed filesystem operands remain advisory even when
/// they are lexically concrete. Outside, missing, expanded, and heuristic
/// operands must not require path evidence or narrow the maximum mount graph.
#[test]
fn advisory_filesystem_operands_compile_to_maximum_authority() {
    let config = config();
    let authority = authority();
    let mut advisory = effects();
    advisory.reads = vec![
        "/home/alice".to_string(),
        "missing.txt".to_string(),
        "*.rs".to_string(),
        "~/secret.txt".to_string(),
        "escape-link".to_string(),
        "5".to_string(),
    ];
    let evaluation = evaluation(EffectCompleteness::Unknown, advisory);

    let plan = compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap();

    assert_eq!(
        plan.audit_summary.authority_source,
        SandboxAuthoritySource::Maximum
    );
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", "/workspace", "/workspace"])
    );
    for advisory_operand in [
        "/home/alice",
        "missing.txt",
        "*.rs",
        "~/secret.txt",
        "escape-link",
        "5",
    ] {
        assert!(
            !plan
                .arguments
                .iter()
                .any(|argument| argument == advisory_operand)
        );
    }
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
    assert_eq!(plan.audit_summary.protected_mask_count, 7);
    let parent_mount = plan
        .arguments
        .windows(3)
        .position(|args| args == ["--ro-bind", "/home/alice", "/home/alice"])
        .unwrap();
    for protected in [
        ".ssh",
        ".gnupg",
        ".aws",
        ".azure",
        ".kube",
        ".docker",
        ".config/mezzanine",
    ] {
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

/// Complete semantic-patch effects retain every configured writable mount.
///
/// Patch target authorization happens in the generated read and write phases,
/// so Bubblewrap must expose the complete effective write authority rather than
/// narrowing the namespace to classifier evidence for the synthetic command.
#[test]
fn semantic_patch_preserves_maximum_write_authority() {
    let config = config();
    let mut authority = authority();
    authority.write_scopes = vec![
        "/workspace/target".to_string(),
        "/workspace/generated".to_string(),
    ];
    let mut complete = effects();
    complete.writes.push("target/one.txt".to_string());
    let evaluation = evaluation(EffectCompleteness::Complete, complete);
    let mut request = request(&config, &authority, &evaluation);
    request.preserve_maximum_authority = true;

    let plan = compile_bubblewrap_launch_plan(request).unwrap();

    assert_eq!(
        plan.audit_summary.authority_source,
        SandboxAuthoritySource::Maximum
    );
    for path in ["/workspace/target", "/workspace/generated"] {
        assert!(
            plan.arguments
                .windows(3)
                .any(|args| args == ["--bind", path, path]),
            "missing writable mount for {path}: {:?}",
            plan.arguments
        );
    }
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
    assert_eq!(plan.expected_stdout, "mez-bubblewrap-capability-v1");
    assert!(plan.arguments.contains(&"--unshare-net".to_string()));
    assert!(plan.arguments.contains(&"--disable-userns".to_string()));
    assert!(plan.arguments.contains(&"--clearenv".to_string()));
    assert!(
        plan.arguments
            .iter()
            .any(|argument| argument.contains("/etc/passwd"))
    );
    assert!(
        plan.arguments
            .last()
            .is_some_and(|script| script.contains("printf '%s' 'mez-bubblewrap-capability-v1'"))
    );
    let capability = parse_bubblewrap_capability_probe(
        "%1",
        "pane-env-sha256",
        0,
        &plan,
        0,
        "mez-bubblewrap-capability-v1",
    )
    .unwrap();
    assert_eq!(
        capability.cache_key.runtime_profile_version,
        BUBBLEWRAP_RUNTIME_PROFILE_VERSION
    );
    assert_eq!(capability.cache_key.executable, "/usr/bin/bwrap");
    assert_eq!(capability.cache_key.pane_id, "%1");
    assert_eq!(capability.cache_key.config_generation, 0);
    assert_eq!(
        capability.cache_key.pane_environment_signature,
        "pane-env-sha256"
    );

    for contaminated_output in [
        "mez-bubblewrap-capability-v1\n",
        "mez-bubblewrap-capability-v1\r\n",
        "leading-mez-bubblewrap-capability-v1",
        "mez-bubblewrap-capability-v1trailing",
        "",
    ] {
        assert_eq!(
            parse_bubblewrap_capability_probe(
                "%1",
                "pane-env-sha256",
                0,
                &plan,
                0,
                contaminated_output,
            )
            .unwrap_err()
            .kind(),
            SandboxCompileErrorKind::CapabilityProbeFailed
        );
    }

    assert_eq!(
        parse_bubblewrap_capability_probe("%1", "pane-env-sha256", 0, &plan, 1, "")
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

/// Verifies an allow network policy selects the connected profile even when a
/// command has no inferred network effect, while unsupported requirements still
/// fail closed before launch.
#[test]
fn unsupported_requirements_fail_before_launch() {
    let config = config();
    let authority = authority();
    let no_network = evaluation(EffectCompleteness::Complete, effects());
    let mut allowed_no_network = request(&config, &authority, &no_network);
    allowed_no_network.network_policy = NetworkPolicy::Allow;
    let plan = compile_bubblewrap_launch_plan(allowed_no_network).unwrap();
    assert_eq!(plan.audit_summary.network, BubblewrapNetworkMode::Connected);
    assert!(!plan.arguments.contains(&"--unshare-net".to_string()));

    let mut network = effects();
    network.network = true;
    let network = evaluation(EffectCompleteness::Complete, network);
    let plan = compile_bubblewrap_launch_plan(request(&config, &authority, &network)).unwrap();
    assert_eq!(plan.audit_summary.network, BubblewrapNetworkMode::Connected);
    assert!(!plan.arguments.contains(&"--unshare-net".to_string()));

    let mut denied_network = request(&config, &authority, &network);
    denied_network.network_policy = NetworkPolicy::Deny;
    let plan = compile_bubblewrap_launch_plan(denied_network).unwrap();
    assert_eq!(plan.audit_summary.network, BubblewrapNetworkMode::Isolated);
    assert!(plan.arguments.contains(&"--unshare-net".to_string()));

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

/// A managed project home replaces the ephemeral home with one writable bind
/// and publishes deterministic XDG directories inside that synthetic home.
#[test]
fn managed_home_is_bound_with_expected_xdg_environment() {
    let config = config();
    let authority = authority();
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let managed_home = Path::new("/private/mez/cache-home");
    let mut compile_request = request(&config, &authority, &evaluation);
    compile_request.managed_home_host_path = Some(managed_home);

    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();

    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--bind", "/private/mez/cache-home", "/home/mez"])
    );
    assert!(
        !plan
            .arguments
            .windows(2)
            .any(|args| args == ["--tmpfs", "/home/mez"])
    );
    for (name, value) in [
        ("HOME", "/home/mez"),
        ("XDG_CACHE_HOME", "/home/mez/.cache"),
        ("XDG_CONFIG_HOME", "/home/mez/.config"),
        ("XDG_DATA_HOME", "/home/mez/.local/share"),
        ("XDG_STATE_HOME", "/home/mez/.local/state"),
    ] {
        assert!(
            plan.arguments
                .windows(3)
                .any(|args| args == ["--setenv", name, value]),
            "missing {name}={value}"
        );
    }
}

/// A configured sanitized Git identity uses command-scope Git configuration
/// while disabling all host system and global configuration discovery.
#[test]
fn git_identity_projection_is_sanitized_and_explicit() {
    let mut config = config();
    config.git_user_name = Some("Mez Test".to_string());
    config.git_user_email = Some("mez@example.invalid".to_string());
    let authority = authority();
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());

    let plan = compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap();

    for (name, value) in [
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_COUNT", "2"),
        ("GIT_CONFIG_KEY_0", "user.name"),
        ("GIT_CONFIG_VALUE_0", "Mez Test"),
        ("GIT_CONFIG_KEY_1", "user.email"),
        ("GIT_CONFIG_VALUE_1", "mez@example.invalid"),
    ] {
        assert!(
            plan.arguments
                .windows(3)
                .any(|args| args == ["--setenv", name, value]),
            "missing sanitized Git setting {name}={value}"
        );
    }
    assert!(!plan.arguments.iter().any(|argument| {
        argument.contains("credential")
            || argument.contains("signing")
            || argument.contains("include")
            || argument.contains("insteadOf")
    }));
}

/// Omitted Git identity still disables host configuration and does not invent
/// author values, leaving repository-local identity available to Git.
#[test]
fn omitted_git_identity_does_not_invent_author_values() {
    let config = config();
    let authority = authority();
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());

    let plan = compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap();

    assert!(
        plan.arguments
            .windows(3)
            .any(|args| { args == ["--setenv", "GIT_CONFIG_GLOBAL", "/dev/null"] })
    );
    assert!(!plan.arguments.iter().any(|argument| {
        argument == "GIT_CONFIG_COUNT"
            || argument == "GIT_CONFIG_KEY_0"
            || argument == "GIT_CONFIG_VALUE_0"
    }));
}

/// A selected Rust toolchain projects only the two canonical allowlisted roots
/// read-only and gives Cargo binaries deterministic precedence over system PATH.
#[test]
fn rust_toolchain_projection_is_read_only_and_deterministic() {
    let mut config = config();
    config.toolchains = vec![SandboxToolchainKind::Rust];
    let authority = home_authority("/home/alice");
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let projection = resolve_toolchain_projection(
        &config.toolchains,
        &[
            "cargo-bin:/home/alice/.cargo/bin".to_string(),
            "rustup:/home/alice/.rustup".to_string(),
        ],
        "linux",
    )
    .unwrap()
    .unwrap();
    let mut compile_request = request(&config, &authority, &evaluation);
    compile_request.toolchain_projection = Some(&projection);

    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();

    for (source, destination) in [
        (
            "/home/alice/.cargo/bin",
            "/opt/mez/toolchains/rust/cargo-bin",
        ),
        ("/home/alice/.rustup", "/opt/mez/toolchains/rust/rustup"),
    ] {
        assert!(
            plan.arguments
                .windows(3)
                .any(|args| args == ["--ro-bind", source, destination])
        );
        assert!(
            !plan
                .arguments
                .windows(3)
                .any(|args| { args == ["--bind", source, destination] })
        );
    }
    for (name, value) in [
        ("CARGO_HOME", "/home/mez/.cargo"),
        ("RUSTUP_HOME", "/opt/mez/toolchains/rust/rustup"),
        ("PATH", "/opt/mez/toolchains/rust/cargo-bin:/usr/bin:/bin"),
    ] {
        assert!(
            plan.arguments
                .windows(3)
                .any(|args| args == ["--setenv", name, value]),
            "missing {name}={value}"
        );
    }
}

/// The Rust descriptor preserves the established fixed projection contract
/// while explicitly classifying host roots and supported pane platforms.
#[test]
fn rust_toolchain_descriptor_matches_existing_projection_metadata() {
    let descriptor = toolchain_descriptor(SandboxToolchainKind::Rust);

    assert_eq!(descriptor.aliases, ["rust"]);
    assert_eq!(descriptor.roots.len(), 2);
    assert_eq!(descriptor.roots[0].evidence_kind, "cargo-bin");
    assert_eq!(
        descriptor.roots[0].sandbox_destination,
        SANDBOX_RUST_CARGO_BIN
    );
    assert_eq!(
        descriptor.roots[0].authority_class,
        ToolchainAuthorityClass::UserTools
    );
    assert_eq!(descriptor.roots[1].evidence_kind, "rustup");
    assert_eq!(descriptor.roots[1].sandbox_destination, SANDBOX_RUSTUP_HOME);
    assert_eq!(
        descriptor.roots[1].authority_class,
        ToolchainAuthorityClass::Runtime
    );
    assert_eq!(descriptor.path_entries, [SANDBOX_RUST_CARGO_BIN]);
    assert!(descriptor.coupling.required.is_empty());
    assert!(descriptor.coupling.optional.is_empty());
    assert!(descriptor.platform.supports("linux"));
    assert!(ToolchainPlatform::Linux.supports("linux"));
    assert!(ToolchainPlatform::MacOs.supports("darwin"));
    assert!(ToolchainPlatform::Windows.supports("windows"));
    assert!(!ToolchainPlatform::Linux.supports("windows"));
}

/// A validated self-contained Zig distribution is projected read-only with
/// deterministic PATH precedence and managed global-cache redirection.
#[test]
fn zig_toolchain_projection_is_read_only_and_cache_isolated() {
    let base = std::env::temp_dir().join(format!(
        "mez-zig-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("zig-0.14.0");
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("zig"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(root.join("zig"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let root = root.canonicalize().unwrap();

    let descriptor = toolchain_descriptor(SandboxToolchainKind::Zig);
    assert_eq!(descriptor.aliases, ["zig"]);
    assert_eq!(descriptor.roots[0].evidence_kind, "zig");
    assert_eq!(descriptor.roots[0].sandbox_destination, SANDBOX_ZIG_ROOT);
    assert_eq!(descriptor.roots[0].required_executables, ["zig"]);
    assert_eq!(descriptor.roots[0].required_directories, ["lib"]);

    let managers = [format!("zig:{}", root.display())];
    let projection = resolve_toolchain_projection(&[SandboxToolchainKind::Zig], &managers, "linux")
        .unwrap()
        .unwrap();
    assert_eq!(projection.executable_path(), SANDBOX_ZIG_PATH);
    assert_eq!(
        projection
            .environment
            .get("ZIG_GLOBAL_CACHE_DIR")
            .map(String::as_str),
        Some("/home/mez/.cache/zig")
    );

    let mut config = config();
    config.toolchains = vec![SandboxToolchainKind::Zig];
    let home_scope = home_authority(&base.canonicalize().unwrap().display().to_string());
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let mut compile_request = request(&config, &home_scope, &evaluation);
    compile_request.toolchain_projection = Some(&projection);
    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();

    let source = root.display().to_string();
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", source.as_str(), SANDBOX_ZIG_ROOT])
    );
    assert!(
        !plan
            .arguments
            .windows(3)
            .any(|args| args == ["--bind", source.as_str(), SANDBOX_ZIG_ROOT])
    );
    for (name, value) in [
        ("ZIG_GLOBAL_CACHE_DIR", "/home/mez/.cache/zig"),
        ("PATH", SANDBOX_ZIG_PATH),
    ] {
        assert!(
            plan.arguments
                .windows(3)
                .any(|args| args == ["--setenv", name, value]),
            "missing {name}={value}"
        );
    }

    let outside = authority();
    let mut outside_request = request(&config, &outside, &evaluation);
    outside_request.toolchain_projection = Some(&projection);
    let outside_plan = compile_bubblewrap_launch_plan(outside_request).unwrap();
    assert!(
        outside_plan
            .arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", source.as_str(), SANDBOX_ZIG_ROOT])
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// Zig discovery rejects missing distribution layout, non-executable files,
/// and symlink shims instead of widening a selected executable directory.
#[test]
fn zig_toolchain_discovery_rejects_malformed_and_symlinked_distributions() {
    let base = std::env::temp_dir().join(format!(
        "mez-zig-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("zig-invalid");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("zig"), "not executable").unwrap();
    let root = root.canonicalize().unwrap();
    let managers = [format!("zig:{}", root.display())];

    let missing_layout =
        resolve_toolchain_projection(&[SandboxToolchainKind::Zig], &managers, "linux").unwrap_err();
    assert!(matches!(
        missing_layout.kind(),
        SandboxCompileErrorKind::InvalidInput | SandboxCompileErrorKind::ForbiddenHostPath
    ));

    std::fs::create_dir_all(root.join("lib")).unwrap();
    let non_executable =
        resolve_toolchain_projection(&[SandboxToolchainKind::Zig], &managers, "linux").unwrap_err();
    assert_eq!(
        non_executable.kind(),
        SandboxCompileErrorKind::ForbiddenHostPath
    );

    let external = base.join("external-zig");
    std::fs::write(&external, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(root.join("zig")).unwrap();
    std::os::unix::fs::symlink(&external, root.join("zig")).unwrap();
    let search_path = std::env::join_paths([root.as_path()]).unwrap();
    let symlink = discover_zig_from_search_path(Some(&search_path)).unwrap_err();
    assert_eq!(symlink.kind(), SandboxCompileErrorKind::ForbiddenHostPath);

    let _ = std::fs::remove_dir_all(&base);
}

/// A validated Go SDK is mounted read-only while all writable workspace,
/// module, and build caches are redirected beneath the managed home.
#[test]
fn go_toolchain_projection_is_read_only_and_cache_isolated() {
    let base = std::env::temp_dir().join(format!(
        "mez-go-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("go-sdk");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("bin/go"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(root.join("bin/go"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let root = root.canonicalize().unwrap();

    let descriptor = toolchain_descriptor(SandboxToolchainKind::Go);
    assert_eq!(descriptor.aliases, ["go", "golang"]);
    assert_eq!(descriptor.roots[0].evidence_kind, "go");
    assert_eq!(descriptor.roots[0].sandbox_destination, SANDBOX_GO_ROOT);
    assert_eq!(descriptor.roots[0].required_executables, ["bin/go"]);
    assert_eq!(descriptor.roots[0].required_directories, ["src"]);

    let managers = [format!("go:{}", root.display())];
    let projection = resolve_toolchain_projection(&[SandboxToolchainKind::Go], &managers, "linux")
        .unwrap()
        .unwrap();
    assert_eq!(projection.executable_path(), SANDBOX_GO_PATH);
    for (name, value) in [
        ("GOROOT", SANDBOX_GO_ROOT),
        ("GOPATH", "/home/mez/go"),
        ("GOMODCACHE", "/home/mez/go/pkg/mod"),
        ("GOCACHE", "/home/mez/.cache/go-build"),
    ] {
        assert_eq!(
            projection.environment.get(name).map(String::as_str),
            Some(value)
        );
    }
    assert!(!projection.environment.contains_key("GOBIN"));

    let mut config = config();
    config.toolchains = vec![SandboxToolchainKind::Go];
    let home_scope = home_authority(&base.canonicalize().unwrap().display().to_string());
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let mut compile_request = request(&config, &home_scope, &evaluation);
    compile_request.toolchain_projection = Some(&projection);
    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();
    let source = root.display().to_string();
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", source.as_str(), SANDBOX_GO_ROOT])
    );
    assert!(
        !plan
            .arguments
            .windows(3)
            .any(|args| args == ["--bind", source.as_str(), SANDBOX_GO_ROOT])
    );
    for (name, value) in [
        ("GOROOT", SANDBOX_GO_ROOT),
        ("GOPATH", "/home/mez/go"),
        ("GOMODCACHE", "/home/mez/go/pkg/mod"),
        ("GOCACHE", "/home/mez/.cache/go-build"),
        ("PATH", SANDBOX_GO_PATH),
    ] {
        assert!(
            plan.arguments
                .windows(3)
                .any(|args| args == ["--setenv", name, value]),
            "missing {name}={value}"
        );
    }

    let outside = authority();
    let mut outside_request = request(&config, &outside, &evaluation);
    outside_request.toolchain_projection = Some(&projection);
    let outside_plan = compile_bubblewrap_launch_plan(outside_request).unwrap();
    assert!(
        outside_plan
            .arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", source.as_str(), SANDBOX_GO_ROOT])
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// Go discovery rejects incomplete SDK layouts, non-executable binaries, and
/// symlink shims rather than treating GOPATH or an arbitrary bin as an SDK.
#[test]
fn go_toolchain_discovery_rejects_malformed_and_symlinked_sdks() {
    let base = std::env::temp_dir().join(format!(
        "mez-go-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("go-sdk");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/go"), "not executable").unwrap();
    let root = root.canonicalize().unwrap();
    let managers = [format!("go:{}", root.display())];

    let missing_layout =
        resolve_toolchain_projection(&[SandboxToolchainKind::Go], &managers, "linux").unwrap_err();
    assert!(matches!(
        missing_layout.kind(),
        SandboxCompileErrorKind::InvalidInput | SandboxCompileErrorKind::ForbiddenHostPath
    ));

    std::fs::create_dir_all(root.join("src")).unwrap();
    let non_executable =
        resolve_toolchain_projection(&[SandboxToolchainKind::Go], &managers, "linux").unwrap_err();
    assert_eq!(
        non_executable.kind(),
        SandboxCompileErrorKind::ForbiddenHostPath
    );

    let external = base.join("external-go");
    std::fs::write(&external, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(root.join("bin/go")).unwrap();
    std::os::unix::fs::symlink(&external, root.join("bin/go")).unwrap();
    let search_path = std::env::join_paths([root.join("bin")]).unwrap();
    let symlink = discover_go_from_search_path(Some(&search_path)).unwrap_err();
    assert_eq!(symlink.kind(), SandboxCompileErrorKind::ForbiddenHostPath);

    let _ = std::fs::remove_dir_all(&base);
}

/// A validated Deno runtime is projected read-only with deterministic PATH
/// precedence and a managed cache that imports no host authentication state.
#[test]
fn deno_toolchain_projection_is_read_only_and_cache_isolated() {
    let base = std::env::temp_dir().join(format!(
        "mez-deno-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("deno-runtime");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("deno"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(root.join("deno"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let root = root.canonicalize().unwrap();

    let descriptor = toolchain_descriptor(SandboxToolchainKind::Deno);
    assert_eq!(descriptor.aliases, ["deno"]);
    assert_eq!(descriptor.roots[0].evidence_kind, "deno");
    assert_eq!(descriptor.roots[0].sandbox_destination, SANDBOX_DENO_ROOT);
    assert_eq!(descriptor.roots[0].required_executables, ["deno"]);
    assert!(descriptor.roots[0].required_directories.is_empty());

    let managers = [format!("deno:{}", root.display())];
    let projection =
        resolve_toolchain_projection(&[SandboxToolchainKind::Deno], &managers, "linux")
            .unwrap()
            .unwrap();
    assert_eq!(projection.executable_path(), SANDBOX_DENO_PATH);
    assert_eq!(
        projection.environment.get("DENO_DIR").map(String::as_str),
        Some("/home/mez/.cache/deno")
    );
    for omitted in ["DENO_AUTH_TOKENS", "DENO_CERT", "NPM_CONFIG_USERCONFIG"] {
        assert!(!projection.environment.contains_key(omitted));
    }

    let mut config = config();
    config.toolchains = vec![SandboxToolchainKind::Deno];
    let home_scope = home_authority(&base.canonicalize().unwrap().display().to_string());
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let mut compile_request = request(&config, &home_scope, &evaluation);
    compile_request.toolchain_projection = Some(&projection);
    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();
    let source = root.display().to_string();
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", source.as_str(), SANDBOX_DENO_ROOT])
    );
    assert!(
        !plan
            .arguments
            .windows(3)
            .any(|args| args == ["--bind", source.as_str(), SANDBOX_DENO_ROOT])
    );
    for (name, value) in [
        ("DENO_DIR", "/home/mez/.cache/deno"),
        ("PATH", SANDBOX_DENO_PATH),
    ] {
        assert!(
            plan.arguments
                .windows(3)
                .any(|args| args == ["--setenv", name, value]),
            "missing {name}={value}"
        );
    }

    let outside = authority();
    let mut outside_request = request(&config, &outside, &evaluation);
    outside_request.toolchain_projection = Some(&projection);
    let outside_plan = compile_bubblewrap_launch_plan(outside_request).unwrap();
    assert!(
        outside_plan
            .arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", source.as_str(), SANDBOX_DENO_ROOT])
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// Deno discovery rejects non-executable runtime files and symlink shims
/// instead of importing a manager bin directory or host DENO_DIR state.
#[test]
fn deno_toolchain_discovery_rejects_non_executable_and_symlinked_runtimes() {
    let base = std::env::temp_dir().join(format!(
        "mez-deno-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("deno-runtime");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("deno"), "not executable").unwrap();
    let root = root.canonicalize().unwrap();
    let managers = [format!("deno:{}", root.display())];

    let non_executable =
        resolve_toolchain_projection(&[SandboxToolchainKind::Deno], &managers, "linux")
            .unwrap_err();
    assert_eq!(
        non_executable.kind(),
        SandboxCompileErrorKind::ForbiddenHostPath
    );

    let external = base.join("external-deno");
    std::fs::write(&external, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(root.join("deno")).unwrap();
    std::os::unix::fs::symlink(&external, root.join("deno")).unwrap();
    let search_path = std::env::join_paths([root.as_path()]).unwrap();
    let symlink = discover_deno_from_search_path(Some(&search_path)).unwrap_err();
    assert_eq!(symlink.kind(), SandboxCompileErrorKind::ForbiddenHostPath);

    let _ = std::fs::remove_dir_all(&base);
}

/// A validated Bun distribution is projected read-only with deterministic
/// PATH and BUN_INSTALL while package cache state remains under managed home.
#[test]
fn bun_toolchain_projection_is_read_only_and_cache_isolated() {
    let base = std::env::temp_dir().join(format!(
        "mez-bun-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("bun-runtime");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/bun"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(root.join("bin/bun"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let root = root.canonicalize().unwrap();

    let descriptor = toolchain_descriptor(SandboxToolchainKind::Bun);
    assert_eq!(descriptor.aliases, ["bun"]);
    assert_eq!(descriptor.roots[0].evidence_kind, "bun");
    assert_eq!(descriptor.roots[0].sandbox_destination, SANDBOX_BUN_ROOT);
    assert_eq!(descriptor.roots[0].required_executables, ["bin/bun"]);
    assert!(descriptor.roots[0].required_directories.is_empty());

    let managers = [format!("bun:{}", root.display())];
    let projection = resolve_toolchain_projection(&[SandboxToolchainKind::Bun], &managers, "linux")
        .unwrap()
        .unwrap();
    assert_eq!(projection.executable_path(), SANDBOX_BUN_PATH);
    assert_eq!(
        projection
            .environment
            .get("BUN_INSTALL")
            .map(String::as_str),
        Some(SANDBOX_BUN_ROOT)
    );
    assert_eq!(
        projection
            .environment
            .get("BUN_INSTALL_CACHE_DIR")
            .map(String::as_str),
        Some("/home/mez/.cache/bun")
    );
    for omitted in ["BUN_AUTH_TOKEN", "NPM_CONFIG_USERCONFIG", "NODE_PATH"] {
        assert!(!projection.environment.contains_key(omitted));
    }

    let mut config = config();
    config.toolchains = vec![SandboxToolchainKind::Bun];
    let home_scope = home_authority(&base.canonicalize().unwrap().display().to_string());
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let mut compile_request = request(&config, &home_scope, &evaluation);
    compile_request.toolchain_projection = Some(&projection);
    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();
    let source = root.display().to_string();
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", source.as_str(), SANDBOX_BUN_ROOT])
    );
    assert!(
        !plan
            .arguments
            .windows(3)
            .any(|args| args == ["--bind", source.as_str(), SANDBOX_BUN_ROOT])
    );
    for (name, value) in [
        ("BUN_INSTALL", SANDBOX_BUN_ROOT),
        ("BUN_INSTALL_CACHE_DIR", "/home/mez/.cache/bun"),
        ("PATH", SANDBOX_BUN_PATH),
    ] {
        assert!(
            plan.arguments
                .windows(3)
                .any(|args| args == ["--setenv", name, value]),
            "missing {name}={value}"
        );
    }

    let outside = authority();
    let mut outside_request = request(&config, &outside, &evaluation);
    outside_request.toolchain_projection = Some(&projection);
    let outside_plan = compile_bubblewrap_launch_plan(outside_request).unwrap();
    assert!(
        outside_plan
            .arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", source.as_str(), SANDBOX_BUN_ROOT])
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// Bun discovery rejects non-executable distribution files and symlink shims
/// instead of importing a manager root, global tools, or ambient BUN_INSTALL.
#[test]
fn bun_toolchain_discovery_rejects_non_executable_and_symlinked_distributions() {
    let base = std::env::temp_dir().join(format!(
        "mez-bun-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("bun-runtime");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/bun"), "not executable").unwrap();
    let root = root.canonicalize().unwrap();
    let managers = [format!("bun:{}", root.display())];

    let non_executable =
        resolve_toolchain_projection(&[SandboxToolchainKind::Bun], &managers, "linux").unwrap_err();
    assert_eq!(
        non_executable.kind(),
        SandboxCompileErrorKind::ForbiddenHostPath
    );

    let external = base.join("external-bun");
    std::fs::write(&external, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(root.join("bin/bun")).unwrap();
    std::os::unix::fs::symlink(&external, root.join("bin/bun")).unwrap();
    let search_path = std::env::join_paths([root.join("bin")]).unwrap();
    let symlink = discover_bun_from_search_path(Some(&search_path)).unwrap_err();
    assert_eq!(symlink.kind(), SandboxCompileErrorKind::ForbiddenHostPath);

    let _ = std::fs::remove_dir_all(&base);
}

/// A validated JDK is projected read-only with a fixed JAVA_HOME and requires
/// compiler, runtime, and archive executables from one canonical SDK root.
#[test]
fn jdk_toolchain_projection_is_read_only_and_complete() {
    let base = std::env::temp_dir().join(format!(
        "mez-jdk-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("jdk-runtime");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("lib")).unwrap();
    for executable in ["java", "javac", "jar"] {
        std::fs::write(root.join("bin").join(executable), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(
            root.join("bin").join(executable),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let root = root.canonicalize().unwrap();

    let descriptor = toolchain_descriptor(SandboxToolchainKind::Jdk);
    assert_eq!(descriptor.aliases, ["jdk", "java"]);
    assert_eq!(descriptor.roots[0].evidence_kind, "jdk-runtime");
    assert_eq!(descriptor.roots[0].sandbox_destination, SANDBOX_JDK_ROOT);
    assert_eq!(
        descriptor.roots[0].required_executables,
        ["bin/java", "bin/javac", "bin/jar"]
    );

    let managers = [format!("jdk-runtime:{}", root.display())];
    let projection = resolve_toolchain_projection(&[SandboxToolchainKind::Jdk], &managers, "linux")
        .unwrap()
        .unwrap();
    assert_eq!(projection.executable_path(), SANDBOX_JDK_PATH);
    assert_eq!(
        projection.environment.get("JAVA_HOME").map(String::as_str),
        Some(SANDBOX_JDK_ROOT)
    );

    let mut config = config();
    config.toolchains = vec![SandboxToolchainKind::Jdk];
    let home_scope = home_authority(&base.canonicalize().unwrap().display().to_string());
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let mut compile_request = request(&config, &home_scope, &evaluation);
    compile_request.toolchain_projection = Some(&projection);
    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();
    let source = root.display().to_string();
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", source.as_str(), SANDBOX_JDK_ROOT])
    );
    for (name, value) in [("JAVA_HOME", SANDBOX_JDK_ROOT), ("PATH", SANDBOX_JDK_PATH)] {
        assert!(
            plan.arguments
                .windows(3)
                .any(|args| args == ["--setenv", name, value]),
            "missing {name}={value}"
        );
    }

    let _ = std::fs::remove_dir_all(&base);
}

/// JDK discovery rejects JRE-only and shimmed installations rather than
/// broadening to a manager home or accepting an incomplete runtime.
#[test]
fn jdk_toolchain_discovery_rejects_incomplete_and_symlinked_sdks() {
    let base = std::env::temp_dir().join(format!(
        "mez-jdk-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("jdk-runtime");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("bin/java"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        root.join("bin/java"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let root = root.canonicalize().unwrap();
    let managers = [format!("jdk-runtime:{}", root.display())];

    assert!(
        resolve_toolchain_projection(&[SandboxToolchainKind::Jdk], &managers, "linux").is_err()
    );

    for executable in ["javac", "jar"] {
        std::fs::write(root.join("bin").join(executable), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(
            root.join("bin").join(executable),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let external = base.join("external-javac");
    std::fs::write(&external, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(root.join("bin/javac")).unwrap();
    std::os::unix::fs::symlink(&external, root.join("bin/javac")).unwrap();
    let search_path = std::env::join_paths([root.join("bin")]).unwrap();
    let error = discover_jdk_from_search_path(Some(&search_path)).unwrap_err();
    assert_eq!(error.kind(), SandboxCompileErrorKind::ForbiddenHostPath);

    let _ = std::fs::remove_dir_all(&base);
}

/// Repository Maven and Gradle wrappers take precedence over standalone
/// distributions, retain the selected JDK, and isolate all mutable build-tool
/// state without adding the repository root or host configuration to PATH.
#[test]
fn jvm_build_tool_wrappers_prefer_trusted_project_and_isolate_state() {
    let base = std::env::temp_dir().join(format!(
        "mez-jvm-wrapper-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let jdk = base.join("jdk");
    std::fs::create_dir_all(jdk.join("bin")).unwrap();
    std::fs::create_dir_all(jdk.join("lib")).unwrap();
    for executable in ["java", "javac", "jar"] {
        let path = jdk.join("bin").join(executable);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let project = base.join("project");
    std::fs::create_dir_all(project.join(".mvn/wrapper")).unwrap();
    std::fs::create_dir_all(project.join("gradle/wrapper")).unwrap();
    for wrapper in ["mvnw", "gradlew"] {
        let path = project.join(wrapper);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(
        project.join(".mvn/wrapper/maven-wrapper.properties"),
        "distributionUrl=https://repo.maven.apache.org/maven2/org/apache/maven/apache-maven/3.9.9/apache-maven-3.9.9-bin.zip\n",
    )
    .unwrap();
    std::fs::write(
        project.join("gradle/wrapper/gradle-wrapper.properties"),
        "distributionUrl=https\\://services.gradle.org/distributions/gradle-8.12-bin.zip\n",
    )
    .unwrap();
    let jdk = jdk.canonicalize().unwrap();
    let project = project.canonicalize().unwrap();
    let managers = [format!("jdk-runtime:{}", jdk.display())];

    let projection = resolve_toolchain_projection_for_project(
        &[
            SandboxToolchainKind::Jdk,
            SandboxToolchainKind::Maven,
            SandboxToolchainKind::Gradle,
        ],
        &managers,
        "linux",
        Some(&project),
    )
    .unwrap()
    .unwrap();

    assert_eq!(projection.roots.len(), 1);
    assert_eq!(projection.roots[0].sandbox_destination, SANDBOX_JDK_ROOT);
    assert_eq!(projection.project_environments.len(), 2);
    assert_eq!(projection.executable_path(), SANDBOX_JDK_PATH);
    assert_eq!(
        projection
            .environment
            .get("MAVEN_USER_HOME")
            .map(String::as_str),
        Some("/home/mez/.m2")
    );
    assert_eq!(
        projection
            .environment
            .get("GRADLE_USER_HOME")
            .map(String::as_str),
        Some("/home/mez/.gradle")
    );
    assert_eq!(
        projection
            .environment
            .get("GRADLE_OPTS")
            .map(String::as_str),
        Some("-Dorg.gradle.daemon=false")
    );
    assert_eq!(projection.managed_state.len(), 2);
    for variable in ["MAVEN_OPTS", "MAVEN_CONFIG", "GRADLE_HOME"] {
        assert!(!projection.environment.contains_key(variable));
    }

    let missing_jdk = resolve_toolchain_projection_for_project(
        &[SandboxToolchainKind::Maven],
        &managers,
        "linux",
        Some(&project),
    )
    .unwrap_err();
    assert_eq!(
        missing_jdk.kind(),
        SandboxCompileErrorKind::UnsupportedRequirement
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// Maven and Gradle fall back to exact standalone pane evidence when wrappers
/// are absent, while malformed, credential-bearing, or symlinked repository
/// wrapper metadata fails closed instead of importing host build-tool state.
#[test]
fn jvm_build_tools_validate_standalone_fallback_and_wrapper_metadata() {
    let base = std::env::temp_dir().join(format!(
        "mez-jvm-wrapper-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let mut managers = Vec::new();
    for (evidence, name, executable, directories) in [
        ("jdk-runtime", "jdk", "java", ["lib", "bin"]),
        ("maven-runtime", "maven", "mvn", ["lib", "boot"]),
        ("gradle-runtime", "gradle", "gradle", ["lib", "bin"]),
    ] {
        let root = base.join(name);
        for directory in directories {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let mut executables = vec![executable];
        if name == "jdk" {
            executables.extend(["javac", "jar"]);
        }
        for executable in executables {
            let path = root.join("bin").join(executable);
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        managers.push(format!(
            "{evidence}:{}",
            root.canonicalize().unwrap().display()
        ));
    }
    let project = base.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project = project.canonicalize().unwrap();
    let projection = resolve_toolchain_projection_for_project(
        &[
            SandboxToolchainKind::Jdk,
            SandboxToolchainKind::Maven,
            SandboxToolchainKind::Gradle,
        ],
        &managers,
        "linux",
        Some(&project),
    )
    .unwrap()
    .unwrap();
    assert_eq!(projection.roots.len(), 3);
    assert_eq!(projection.roots[1].sandbox_destination, SANDBOX_MAVEN_ROOT);
    assert_eq!(projection.roots[2].sandbox_destination, SANDBOX_GRADLE_ROOT);
    assert_eq!(
        projection.executable_path(),
        "/opt/mez/toolchains/jdk/root/bin:/opt/mez/toolchains/maven/root/bin:/opt/mez/toolchains/gradle/root/bin:/usr/bin:/bin"
    );

    std::fs::create_dir_all(project.join("gradle/wrapper")).unwrap();
    std::fs::write(project.join("gradlew"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(
        project.join("gradlew"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(
        project.join("gradle/wrapper/gradle-wrapper.properties"),
        "distributionUrl=https://user:secret@example.invalid/gradle.zip\n",
    )
    .unwrap();
    let credential_url =
        discover_jvm_project_wrapper(&project, SandboxToolchainKind::Gradle).unwrap_err();
    assert_eq!(
        credential_url.kind(),
        SandboxCompileErrorKind::ForbiddenHostPath
    );

    std::fs::remove_file(project.join("gradlew")).unwrap();
    let external = base.join("external-gradlew");
    std::fs::write(&external, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(&external, project.join("gradlew")).unwrap();
    let symlink = discover_jvm_project_wrapper(&project, SandboxToolchainKind::Gradle).unwrap_err();
    assert_eq!(symlink.kind(), SandboxCompileErrorKind::ForbiddenHostPath);
    let _ = std::fs::remove_dir_all(&base);
}

/// A standalone Kotlin/JVM compiler distribution composes read-only with an
/// explicitly selected JDK and preserves the JDK-owned JAVA_HOME contract.
#[test]
fn kotlin_jvm_toolchain_requires_and_composes_with_jdk() {
    let base = std::env::temp_dir().join(format!(
        "mez-kotlin-jvm-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let jdk_root = base.join("jdk-runtime");
    std::fs::create_dir_all(jdk_root.join("bin")).unwrap();
    std::fs::create_dir_all(jdk_root.join("lib")).unwrap();
    for executable in ["java", "javac", "jar"] {
        std::fs::write(jdk_root.join("bin").join(executable), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(
            jdk_root.join("bin").join(executable),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let kotlin_root = base.join("kotlin-compiler");
    std::fs::create_dir_all(kotlin_root.join("bin")).unwrap();
    std::fs::create_dir_all(kotlin_root.join("lib")).unwrap();
    for executable in ["kotlinc", "kotlin"] {
        std::fs::write(
            kotlin_root.join("bin").join(executable),
            "#!/bin/sh\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(
            kotlin_root.join("bin").join(executable),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let jdk_root = jdk_root.canonicalize().unwrap();
    let kotlin_root = kotlin_root.canonicalize().unwrap();
    let managers = [
        format!("jdk-runtime:{}", jdk_root.display()),
        format!("kotlin-jvm:{}", kotlin_root.display()),
    ];

    let missing = resolve_toolchain_projection(&[SandboxToolchainKind::Kotlin], &managers, "linux")
        .unwrap_err();
    assert_eq!(
        missing.kind(),
        SandboxCompileErrorKind::UnsupportedRequirement
    );

    let descriptor = toolchain_descriptor(SandboxToolchainKind::Kotlin);
    assert_eq!(descriptor.aliases, ["kotlin", "kotlin-jvm"]);
    assert_eq!(descriptor.roots[0].evidence_kind, "kotlin-jvm");
    assert_eq!(descriptor.roots[0].sandbox_destination, SANDBOX_KOTLIN_ROOT);
    assert_eq!(descriptor.coupling.required, [SandboxToolchainKind::Jdk]);

    let projection = resolve_toolchain_projection(
        &[SandboxToolchainKind::Jdk, SandboxToolchainKind::Kotlin],
        &managers,
        "linux",
    )
    .unwrap()
    .unwrap();
    assert_eq!(projection.executable_path(), SANDBOX_KOTLIN_JDK_PATH);
    assert_eq!(
        projection.environment.get("JAVA_HOME").map(String::as_str),
        Some(SANDBOX_JDK_ROOT)
    );
    assert_eq!(projection.roots.len(), 2);

    let _ = std::fs::remove_dir_all(&base);
}

/// Kotlin discovery rejects incomplete distributions and manager shims rather
/// than broadening to SDKMAN, asdf, mise, or unrelated compiler versions.
#[test]
fn kotlin_jvm_discovery_rejects_incomplete_and_symlinked_distributions() {
    let base = std::env::temp_dir().join(format!(
        "mez-kotlin-jvm-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("kotlin-compiler");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/kotlinc"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        root.join("bin/kotlinc"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let root = root.canonicalize().unwrap();
    let managers = [format!("kotlin-jvm:{}", root.display())];

    assert!(
        resolve_toolchain_projection(
            &[SandboxToolchainKind::Jdk, SandboxToolchainKind::Kotlin],
            &managers,
            "linux",
        )
        .is_err()
    );

    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("bin/kotlin"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        root.join("bin/kotlin"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let external = base.join("external-kotlinc");
    std::fs::write(&external, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(root.join("bin/kotlinc")).unwrap();
    std::os::unix::fs::symlink(&external, root.join("bin/kotlinc")).unwrap();
    let search_path = std::env::join_paths([root.join("bin")]).unwrap();
    let error = discover_kotlin_from_search_path(Some(&search_path)).unwrap_err();
    assert_eq!(error.kind(), SandboxCompileErrorKind::ForbiddenHostPath);

    let _ = std::fs::remove_dir_all(&base);
}

/// A complete Ruby prefix is projected read-only with only its matching
/// package executables and project-isolated RubyGems and Bundler state.
#[test]
fn ruby_toolchain_projection_is_read_only_and_package_state_isolated() {
    let base = std::env::temp_dir().join(format!(
        "mez-ruby-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("ruby-runtime");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("lib/ruby")).unwrap();
    for executable in ["ruby", "gem", "bundle"] {
        let executable_path = root.join("bin").join(executable);
        std::fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(executable_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let root = root.canonicalize().unwrap();

    let descriptor = toolchain_descriptor(SandboxToolchainKind::Ruby);
    assert_eq!(descriptor.aliases, ["ruby"]);
    assert_eq!(descriptor.roots[0].evidence_kind, "ruby-runtime");
    assert_eq!(descriptor.roots[0].sandbox_destination, SANDBOX_RUBY_ROOT);
    assert_eq!(
        descriptor.roots[0].required_executables,
        ["bin/ruby", "bin/gem", "bin/bundle"]
    );

    let managers = [format!("ruby-runtime:{}", root.display())];
    let projection =
        resolve_toolchain_projection(&[SandboxToolchainKind::Ruby], &managers, "linux")
            .unwrap()
            .unwrap();
    assert_eq!(projection.executable_path(), SANDBOX_RUBY_PATH);
    for (name, value) in [
        ("GEM_HOME", "/home/mez/.local/share/ruby/gems"),
        ("GEM_PATH", "/home/mez/.local/share/ruby/gems"),
        ("BUNDLE_USER_HOME", "/home/mez/.local/share/bundle"),
        ("BUNDLE_USER_CACHE", "/home/mez/.cache/bundle"),
        ("BUNDLE_USER_CONFIG", "/home/mez/.config/bundle/config"),
        ("BUNDLE_USER_PLUGIN", "/home/mez/.local/share/bundle/plugin"),
    ] {
        assert_eq!(
            projection.environment.get(name).map(String::as_str),
            Some(value),
            "missing {name}"
        );
    }

    let mut config = config();
    config.toolchains = vec![SandboxToolchainKind::Ruby];
    let home_scope = home_authority(&base.canonicalize().unwrap().display().to_string());
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let mut compile_request = request(&config, &home_scope, &evaluation);
    compile_request.toolchain_projection = Some(&projection);
    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();
    let source = root.display().to_string();
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", source.as_str(), SANDBOX_RUBY_ROOT])
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// Ruby discovery rejects incomplete prefixes and manager shims rather than
/// broadening to rbenv, RVM, asdf, mise, gemsets, or user executable trees.
#[test]
fn ruby_toolchain_discovery_rejects_incomplete_and_symlinked_runtimes() {
    let base = std::env::temp_dir().join(format!(
        "mez-ruby-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("ruby-runtime");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("lib/ruby")).unwrap();
    std::fs::write(root.join("bin/ruby"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        root.join("bin/ruby"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let root = root.canonicalize().unwrap();
    let managers = [format!("ruby-runtime:{}", root.display())];

    assert!(
        resolve_toolchain_projection(&[SandboxToolchainKind::Ruby], &managers, "linux").is_err()
    );

    for executable in ["gem", "bundle"] {
        let executable_path = root.join("bin").join(executable);
        std::fs::write(&executable_path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(executable_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let external = base.join("external-ruby");
    std::fs::write(&external, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(root.join("bin/ruby")).unwrap();
    std::os::unix::fs::symlink(&external, root.join("bin/ruby")).unwrap();
    let search_path = std::env::join_paths([root.join("bin")]).unwrap();
    let error = discover_ruby_from_search_path(Some(&search_path)).unwrap_err();
    assert_eq!(error.kind(), SandboxCompileErrorKind::ForbiddenHostPath);

    let _ = std::fs::remove_dir_all(&base);
}

/// A complete PHP runtime composes read-only with an optional standalone
/// Composer companion whose mutable home and cache remain project-isolated.
#[test]
fn php_and_composer_toolchains_compose_with_managed_package_state() {
    let base = std::env::temp_dir().join(format!(
        "mez-php-composer-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let php_root = base.join("php-runtime");
    std::fs::create_dir_all(php_root.join("bin")).unwrap();
    std::fs::create_dir_all(php_root.join("lib/php")).unwrap();
    std::fs::write(php_root.join("bin/php"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        php_root.join("bin/php"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let composer_root = base.join("composer-runtime");
    std::fs::create_dir_all(composer_root.join("bin")).unwrap();
    std::fs::write(composer_root.join("bin/composer"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        composer_root.join("bin/composer"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let php_root = php_root.canonicalize().unwrap();
    let composer_root = composer_root.canonicalize().unwrap();
    let managers = [
        format!("php-runtime:{}", php_root.display()),
        format!("composer-runtime:{}", composer_root.display()),
    ];

    let missing =
        resolve_toolchain_projection(&[SandboxToolchainKind::Composer], &managers, "linux")
            .unwrap_err();
    assert_eq!(
        missing.kind(),
        SandboxCompileErrorKind::UnsupportedRequirement
    );

    let php = toolchain_descriptor(SandboxToolchainKind::Php);
    assert_eq!(php.aliases, ["php"]);
    assert_eq!(php.roots[0].evidence_kind, "php-runtime");
    assert_eq!(php.roots[0].sandbox_destination, SANDBOX_PHP_ROOT);
    let composer = toolchain_descriptor(SandboxToolchainKind::Composer);
    assert_eq!(composer.aliases, ["composer"]);
    assert_eq!(composer.roots[0].evidence_kind, "composer-runtime");
    assert_eq!(composer.roots[0].sandbox_destination, SANDBOX_COMPOSER_ROOT);
    assert_eq!(composer.coupling.required, [SandboxToolchainKind::Php]);

    let projection = resolve_toolchain_projection(
        &[SandboxToolchainKind::Php, SandboxToolchainKind::Composer],
        &managers,
        "linux",
    )
    .unwrap()
    .unwrap();
    assert_eq!(projection.executable_path(), SANDBOX_PHP_COMPOSER_PATH);
    for (name, value) in [
        ("COMPOSER_HOME", "/home/mez/.config/composer"),
        ("COMPOSER_CACHE_DIR", "/home/mez/.cache/composer"),
        (
            "COMPOSER_VENDOR_DIR",
            "/home/mez/.local/share/composer/vendor",
        ),
    ] {
        assert_eq!(
            projection.environment.get(name).map(String::as_str),
            Some(value),
            "missing {name}"
        );
    }
    assert_eq!(projection.roots.len(), 2);

    let _ = std::fs::remove_dir_all(&base);
}

/// PHP and Composer discovery reject incomplete roots and manager shims rather
/// than broadening to asdf, mise, host configuration, or global package trees.
#[test]
fn php_and_composer_discovery_rejects_incomplete_and_symlinked_roots() {
    let base = std::env::temp_dir().join(format!(
        "mez-php-composer-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let php_root = base.join("php-runtime");
    std::fs::create_dir_all(php_root.join("bin")).unwrap();
    std::fs::write(php_root.join("bin/php"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        php_root.join("bin/php"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let php_root = php_root.canonicalize().unwrap();
    let managers = [format!("php-runtime:{}", php_root.display())];
    assert!(
        resolve_toolchain_projection(&[SandboxToolchainKind::Php], &managers, "linux").is_err()
    );

    std::fs::create_dir_all(php_root.join("lib/php")).unwrap();
    let external_php = base.join("external-php");
    std::fs::write(&external_php, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external_php, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(php_root.join("bin/php")).unwrap();
    std::os::unix::fs::symlink(&external_php, php_root.join("bin/php")).unwrap();
    let php_path = std::env::join_paths([php_root.join("bin")]).unwrap();
    let php_error = discover_php_from_search_path(Some(&php_path)).unwrap_err();
    assert_eq!(php_error.kind(), SandboxCompileErrorKind::ForbiddenHostPath);

    let composer_root = base.join("composer-runtime");
    std::fs::create_dir_all(composer_root.join("bin")).unwrap();
    let external_composer = base.join("external-composer");
    std::fs::write(&external_composer, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external_composer, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(&external_composer, composer_root.join("bin/composer")).unwrap();
    let composer_path = std::env::join_paths([composer_root.join("bin")]).unwrap();
    let composer_error = discover_composer_from_search_path(Some(&composer_path)).unwrap_err();
    assert_eq!(
        composer_error.kind(),
        SandboxCompileErrorKind::ForbiddenHostPath
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// A complete Erlang/OTP runtime composes read-only with its required Elixir
/// companion while Mix, Hex, and Rebar state remains project-isolated.
#[test]
fn erlang_and_elixir_toolchains_compose_with_managed_package_state() {
    let base = std::env::temp_dir().join(format!(
        "mez-erlang-elixir-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let erlang_root = base.join("erlang-runtime");
    std::fs::create_dir_all(erlang_root.join("bin")).unwrap();
    std::fs::create_dir_all(erlang_root.join("lib/erlang")).unwrap();
    for executable in ["erl", "erlc", "escript"] {
        let path = erlang_root.join("bin").join(executable);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let elixir_root = base.join("elixir-runtime");
    std::fs::create_dir_all(elixir_root.join("bin")).unwrap();
    std::fs::create_dir_all(elixir_root.join("lib/elixir")).unwrap();
    for executable in ["elixir", "elixirc", "mix"] {
        let path = elixir_root.join("bin").join(executable);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let erlang_root = erlang_root.canonicalize().unwrap();
    let elixir_root = elixir_root.canonicalize().unwrap();
    let managers = [
        format!("erlang-otp:{}", erlang_root.display()),
        format!("elixir-runtime:{}", elixir_root.display()),
    ];

    let missing = resolve_toolchain_projection(&[SandboxToolchainKind::Elixir], &managers, "linux")
        .unwrap_err();
    assert_eq!(
        missing.kind(),
        SandboxCompileErrorKind::UnsupportedRequirement
    );

    let erlang = toolchain_descriptor(SandboxToolchainKind::Erlang);
    assert_eq!(erlang.aliases, ["erlang"]);
    assert_eq!(erlang.roots[0].evidence_kind, "erlang-otp");
    assert_eq!(erlang.roots[0].sandbox_destination, SANDBOX_ERLANG_ROOT);
    let elixir = toolchain_descriptor(SandboxToolchainKind::Elixir);
    assert_eq!(elixir.aliases, ["elixir"]);
    assert_eq!(elixir.roots[0].evidence_kind, "elixir-runtime");
    assert_eq!(elixir.roots[0].sandbox_destination, SANDBOX_ELIXIR_ROOT);
    assert_eq!(elixir.coupling.required, [SandboxToolchainKind::Erlang]);

    let projection = resolve_toolchain_projection(
        &[SandboxToolchainKind::Erlang, SandboxToolchainKind::Elixir],
        &managers,
        "linux",
    )
    .unwrap()
    .unwrap();
    assert_eq!(projection.executable_path(), SANDBOX_ERLANG_ELIXIR_PATH);
    for (name, value) in [
        ("MIX_HOME", "/home/mez/.local/share/mix"),
        ("HEX_HOME", "/home/mez/.local/share/hex"),
        ("REBAR_CACHE_DIR", "/home/mez/.cache/rebar3"),
    ] {
        assert_eq!(
            projection.environment.get(name).map(String::as_str),
            Some(value),
            "missing {name}"
        );
    }
    assert_eq!(projection.roots.len(), 2);

    let _ = std::fs::remove_dir_all(&base);
}

/// Erlang and Elixir discovery reject incomplete roots and manager shims
/// rather than broadening to asdf, mise, host archives, or credential state.
#[test]
fn erlang_and_elixir_discovery_rejects_incomplete_and_symlinked_roots() {
    let base = std::env::temp_dir().join(format!(
        "mez-erlang-elixir-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let erlang_root = base.join("erlang-runtime");
    std::fs::create_dir_all(erlang_root.join("bin")).unwrap();
    for executable in ["erl", "erlc", "escript"] {
        let path = erlang_root.join("bin").join(executable);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let erlang_root = erlang_root.canonicalize().unwrap();
    let managers = [format!("erlang-otp:{}", erlang_root.display())];
    assert!(
        resolve_toolchain_projection(&[SandboxToolchainKind::Erlang], &managers, "linux").is_err()
    );

    std::fs::create_dir_all(erlang_root.join("lib/erlang")).unwrap();
    let external_erlang = base.join("external-erlang");
    std::fs::write(&external_erlang, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external_erlang, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(erlang_root.join("bin/erl")).unwrap();
    std::os::unix::fs::symlink(&external_erlang, erlang_root.join("bin/erl")).unwrap();
    let erlang_path = std::env::join_paths([erlang_root.join("bin")]).unwrap();
    let erlang_error = discover_erlang_from_search_path(Some(&erlang_path)).unwrap_err();
    assert_eq!(
        erlang_error.kind(),
        SandboxCompileErrorKind::ForbiddenHostPath
    );

    let elixir_root = base.join("elixir-runtime");
    std::fs::create_dir_all(elixir_root.join("bin")).unwrap();
    std::fs::create_dir_all(elixir_root.join("lib/elixir")).unwrap();
    for executable in ["elixirc", "mix"] {
        let path = elixir_root.join("bin").join(executable);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let external_elixir = base.join("external-elixir");
    std::fs::write(&external_elixir, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external_elixir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(&external_elixir, elixir_root.join("bin/elixir")).unwrap();
    let elixir_path = std::env::join_paths([elixir_root.join("bin")]).unwrap();
    let elixir_error = discover_elixir_from_search_path(Some(&elixir_path)).unwrap_err();
    assert_eq!(
        elixir_error.kind(),
        SandboxCompileErrorKind::ForbiddenHostPath
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// A complete GHC compiler composes read-only with Cabal and Stack while all
/// package-manager state remains beneath the project-isolated managed home.
#[test]
fn ghc_cabal_and_stack_compose_with_managed_package_state() {
    let base = std::env::temp_dir().join(format!(
        "mez-haskell-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let ghc_root = base.join("ghc-compiler");
    std::fs::create_dir_all(ghc_root.join("bin")).unwrap();
    std::fs::create_dir_all(ghc_root.join("lib/ghc")).unwrap();
    for executable in ["ghc", "ghci", "runghc", "ghc-pkg"] {
        let path = ghc_root.join("bin").join(executable);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let cabal_root = base.join("cabal-companion");
    std::fs::create_dir_all(cabal_root.join("bin")).unwrap();
    std::fs::write(cabal_root.join("bin/cabal"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        cabal_root.join("bin/cabal"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let stack_root = base.join("stack-companion");
    std::fs::create_dir_all(stack_root.join("bin")).unwrap();
    std::fs::write(stack_root.join("bin/stack"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        stack_root.join("bin/stack"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let ghc_root = ghc_root.canonicalize().unwrap();
    let cabal_root = cabal_root.canonicalize().unwrap();
    let stack_root = stack_root.canonicalize().unwrap();
    let managers = [
        format!("ghc-compiler:{}", ghc_root.display()),
        format!("cabal-companion:{}", cabal_root.display()),
        format!("stack-companion:{}", stack_root.display()),
    ];

    for companion in [SandboxToolchainKind::Cabal, SandboxToolchainKind::Stack] {
        let missing = resolve_toolchain_projection(&[companion], &managers, "linux").unwrap_err();
        assert_eq!(
            missing.kind(),
            SandboxCompileErrorKind::UnsupportedRequirement
        );
    }

    let ghc = toolchain_descriptor(SandboxToolchainKind::Ghc);
    assert_eq!(ghc.roots[0].evidence_kind, "ghc-compiler");
    assert_eq!(ghc.roots[0].sandbox_destination, SANDBOX_GHC_ROOT);
    let cabal = toolchain_descriptor(SandboxToolchainKind::Cabal);
    assert_eq!(cabal.roots[0].sandbox_destination, SANDBOX_CABAL_ROOT);
    assert_eq!(cabal.coupling.required, [SandboxToolchainKind::Ghc]);
    let stack = toolchain_descriptor(SandboxToolchainKind::Stack);
    assert_eq!(stack.roots[0].sandbox_destination, SANDBOX_STACK_ROOT);
    assert_eq!(stack.coupling.required, [SandboxToolchainKind::Ghc]);

    let projection = resolve_toolchain_projection(
        &[
            SandboxToolchainKind::Ghc,
            SandboxToolchainKind::Cabal,
            SandboxToolchainKind::Stack,
        ],
        &managers,
        "linux",
    )
    .unwrap()
    .unwrap();
    assert_eq!(projection.executable_path(), SANDBOX_GHC_CABAL_STACK_PATH);
    for (name, value) in [
        ("GHC_ENVIRONMENT", "-"),
        ("CABAL_DIR", "/home/mez/.local/share/cabal"),
        ("STACK_ROOT", "/home/mez/.local/share/stack"),
    ] {
        assert_eq!(
            projection.environment.get(name).map(String::as_str),
            Some(value),
            "missing {name}"
        );
    }
    assert_eq!(projection.roots.len(), 3);

    let _ = std::fs::remove_dir_all(&base);
}

/// Haskell discovery rejects incomplete compiler roots and manager shims
/// instead of broadening to GHCup, package stores, or user executable trees.
#[test]
fn ghc_cabal_and_stack_discovery_rejects_incomplete_and_symlinked_roots() {
    let base = std::env::temp_dir().join(format!(
        "mez-haskell-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let ghc_root = base.join("ghc-compiler");
    std::fs::create_dir_all(ghc_root.join("bin")).unwrap();
    for executable in ["ghc", "ghci", "runghc", "ghc-pkg"] {
        let path = ghc_root.join("bin").join(executable);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let ghc_root = ghc_root.canonicalize().unwrap();
    let managers = [format!("ghc-compiler:{}", ghc_root.display())];
    assert!(
        resolve_toolchain_projection(&[SandboxToolchainKind::Ghc], &managers, "linux").is_err()
    );

    std::fs::create_dir_all(ghc_root.join("lib/ghc")).unwrap();
    let external = base.join("external-tool");
    std::fs::write(&external, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(ghc_root.join("bin/ghc")).unwrap();
    std::os::unix::fs::symlink(&external, ghc_root.join("bin/ghc")).unwrap();
    let ghc_path = std::env::join_paths([ghc_root.join("bin")]).unwrap();
    assert_eq!(
        discover_ghc_from_search_path(Some(&ghc_path))
            .unwrap_err()
            .kind(),
        SandboxCompileErrorKind::ForbiddenHostPath
    );

    for (name, discover) in [
        (
            "cabal",
            discover_cabal_from_search_path
                as fn(Option<&std::ffi::OsStr>) -> Result<Option<PathBuf>, SandboxCompileError>,
        ),
        ("stack", discover_stack_from_search_path),
    ] {
        let root = base.join(format!("{name}-companion"));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::os::unix::fs::symlink(&external, root.join("bin").join(name)).unwrap();
        let search_path = std::env::join_paths([root.join("bin")]).unwrap();
        assert_eq!(
            discover(Some(&search_path)).unwrap_err().kind(),
            SandboxCompileErrorKind::ForbiddenHostPath
        );
    }

    let _ = std::fs::remove_dir_all(&base);
}

/// A validated .NET SDK is projected read-only with fixed runtime and managed
/// state variables while telemetry and first-time setup remain deterministic.
#[test]
fn dotnet_toolchain_projection_is_read_only_and_state_isolated() {
    let base = std::env::temp_dir().join(format!(
        "mez-dotnet-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("dotnet-sdk");
    for directory in ["sdk", "shared", "packs"] {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    std::fs::write(root.join("dotnet"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(root.join("dotnet"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let root = root.canonicalize().unwrap();

    let descriptor = toolchain_descriptor(SandboxToolchainKind::Dotnet);
    assert_eq!(descriptor.aliases, ["dotnet", ".net"]);
    assert_eq!(descriptor.roots[0].evidence_kind, "dotnet-sdk");
    assert_eq!(descriptor.roots[0].sandbox_destination, SANDBOX_DOTNET_ROOT);
    assert_eq!(descriptor.roots[0].required_executables, ["dotnet"]);
    assert_eq!(
        descriptor.roots[0].required_directories,
        ["sdk", "shared", "packs"]
    );

    let managers = [format!("dotnet-sdk:{}", root.display())];
    let projection =
        resolve_toolchain_projection(&[SandboxToolchainKind::Dotnet], &managers, "linux")
            .unwrap()
            .unwrap();
    assert_eq!(projection.executable_path(), SANDBOX_DOTNET_PATH);
    for (name, value) in [
        ("DOTNET_ROOT", SANDBOX_DOTNET_ROOT),
        ("DOTNET_CLI_HOME", "/home/mez/.dotnet"),
        ("NUGET_PACKAGES", "/home/mez/.cache/nuget/packages"),
        ("DOTNET_CLI_TELEMETRY_OPTOUT", "1"),
        ("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1"),
        ("DOTNET_NOLOGO", "1"),
    ] {
        assert_eq!(
            projection.environment.get(name).map(String::as_str),
            Some(value)
        );
    }

    let mut config = config();
    config.toolchains = vec![SandboxToolchainKind::Dotnet];
    let home_scope = home_authority(&base.canonicalize().unwrap().display().to_string());
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let mut compile_request = request(&config, &home_scope, &evaluation);
    compile_request.toolchain_projection = Some(&projection);
    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();
    let source = root.display().to_string();
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", source.as_str(), SANDBOX_DOTNET_ROOT])
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// .NET discovery rejects runtime-only and shimmed installations instead of
/// broadening to a manager prefix or accepting an incomplete SDK.
#[test]
fn dotnet_toolchain_discovery_rejects_incomplete_and_symlinked_sdks() {
    let base = std::env::temp_dir().join(format!(
        "mez-dotnet-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("dotnet-sdk");
    std::fs::create_dir_all(root.join("shared")).unwrap();
    std::fs::write(root.join("dotnet"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(root.join("dotnet"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let root = root.canonicalize().unwrap();
    let managers = [format!("dotnet-sdk:{}", root.display())];

    assert!(
        resolve_toolchain_projection(&[SandboxToolchainKind::Dotnet], &managers, "linux").is_err()
    );

    for directory in ["sdk", "packs"] {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    let external = base.join("external-dotnet");
    std::fs::write(&external, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(root.join("dotnet")).unwrap();
    std::os::unix::fs::symlink(&external, root.join("dotnet")).unwrap();
    let search_path = std::env::join_paths([root.clone()]).unwrap();
    let error = discover_dotnet_from_search_path(Some(&search_path)).unwrap_err();
    assert_eq!(error.kind(), SandboxCompileErrorKind::ForbiddenHostPath);

    let _ = std::fs::remove_dir_all(&base);
}

/// A validated Dart SDK is projected read-only with a deterministic executable
/// path and project-isolated Pub package state.
#[test]
fn dart_toolchain_projection_is_read_only_and_pub_state_isolated() {
    let base = std::env::temp_dir().join(format!(
        "mez-dart-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("dart-sdk");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("bin/dart"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        root.join("bin/dart"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let root = root.canonicalize().unwrap();

    let descriptor = toolchain_descriptor(SandboxToolchainKind::Dart);
    assert_eq!(descriptor.aliases, ["dart"]);
    assert_eq!(descriptor.roots[0].evidence_kind, "dart-sdk");
    assert_eq!(descriptor.roots[0].sandbox_destination, SANDBOX_DART_ROOT);
    assert_eq!(descriptor.roots[0].required_executables, ["bin/dart"]);
    assert_eq!(descriptor.roots[0].required_directories, ["lib"]);

    let managers = [format!("dart-sdk:{}", root.display())];
    let projection =
        resolve_toolchain_projection(&[SandboxToolchainKind::Dart], &managers, "linux")
            .unwrap()
            .unwrap();
    assert_eq!(projection.executable_path(), SANDBOX_DART_PATH);
    assert_eq!(
        projection.environment.get("PUB_CACHE").map(String::as_str),
        Some("/home/mez/.cache/dart-pub")
    );

    let mut config = config();
    config.toolchains = vec![SandboxToolchainKind::Dart];
    let home_scope = home_authority(&base.canonicalize().unwrap().display().to_string());
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let mut compile_request = request(&config, &home_scope, &evaluation);
    compile_request.toolchain_projection = Some(&projection);
    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();
    let source = root.display().to_string();
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", source.as_str(), SANDBOX_DART_ROOT])
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// Dart discovery rejects incomplete and shimmed SDKs instead of accepting a
/// Flutter installation, manager prefix, or arbitrary executable tree.
#[test]
fn dart_toolchain_discovery_rejects_incomplete_and_symlinked_sdks() {
    let base = std::env::temp_dir().join(format!(
        "mez-dart-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("dart-sdk");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/dart"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        root.join("bin/dart"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let root = root.canonicalize().unwrap();
    let managers = [format!("dart-sdk:{}", root.display())];

    assert!(
        resolve_toolchain_projection(&[SandboxToolchainKind::Dart], &managers, "linux").is_err()
    );

    std::fs::create_dir_all(root.join("lib")).unwrap();
    let external = base.join("external-dart");
    std::fs::write(&external, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(root.join("bin/dart")).unwrap();
    std::os::unix::fs::symlink(&external, root.join("bin/dart")).unwrap();
    let search_path = std::env::join_paths([root.join("bin")]).unwrap();
    let error = discover_dart_from_search_path(Some(&search_path)).unwrap_err();
    assert_eq!(error.kind(), SandboxCompileErrorKind::ForbiddenHostPath);

    let _ = std::fs::remove_dir_all(&base);
}

/// A validated Node.js distribution is projected read-only with bundled
/// executables contained in its runtime root and mutable package state managed.
#[test]
fn node_toolchain_projection_is_read_only_and_package_state_isolated() {
    let base = std::env::temp_dir().join(format!(
        "mez-node-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("node-runtime");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("lib")).unwrap();
    for executable in ["node", "npm", "npx", "corepack"] {
        std::fs::write(root.join("bin").join(executable), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(
            root.join("bin").join(executable),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let root = root.canonicalize().unwrap();

    let descriptor = toolchain_descriptor(SandboxToolchainKind::Node);
    assert_eq!(descriptor.aliases, ["node", "nodejs"]);
    assert_eq!(descriptor.roots[0].evidence_kind, "node-runtime");
    assert_eq!(descriptor.roots[0].sandbox_destination, SANDBOX_NODE_ROOT);
    assert_eq!(descriptor.roots[0].required_executables, ["bin/node"]);
    assert_eq!(descriptor.roots[0].required_directories, ["lib"]);

    let managers = [format!("node-runtime:{}", root.display())];
    let projection =
        resolve_toolchain_projection(&[SandboxToolchainKind::Node], &managers, "linux")
            .unwrap()
            .unwrap();
    assert_eq!(projection.executable_path(), SANDBOX_NODE_PATH);
    assert_eq!(
        projection
            .environment
            .get("NPM_CONFIG_CACHE")
            .map(String::as_str),
        Some("/home/mez/.cache/npm")
    );
    assert_eq!(
        projection
            .environment
            .get("COREPACK_HOME")
            .map(String::as_str),
        Some("/home/mez/.cache/node/corepack")
    );
    for omitted in [
        "NPM_CONFIG_USERCONFIG",
        "NPM_TOKEN",
        "NODE_PATH",
        "npm_config_prefix",
    ] {
        assert!(!projection.environment.contains_key(omitted));
    }
    assert!(!projection.executable_path().contains("node_modules/.bin"));

    let mut config = config();
    config.toolchains = vec![SandboxToolchainKind::Node];
    let home_scope = home_authority(&base.canonicalize().unwrap().display().to_string());
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let mut compile_request = request(&config, &home_scope, &evaluation);
    compile_request.toolchain_projection = Some(&projection);
    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();
    let source = root.display().to_string();
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", source.as_str(), SANDBOX_NODE_ROOT])
    );
    assert!(
        !plan
            .arguments
            .windows(3)
            .any(|args| args == ["--bind", source.as_str(), SANDBOX_NODE_ROOT])
    );
    for (name, value) in [
        ("NPM_CONFIG_CACHE", "/home/mez/.cache/npm"),
        ("COREPACK_HOME", "/home/mez/.cache/node/corepack"),
        ("PATH", SANDBOX_NODE_PATH),
    ] {
        assert!(
            plan.arguments
                .windows(3)
                .any(|args| args == ["--setenv", name, value]),
            "missing {name}={value}"
        );
    }

    let outside = authority();
    let mut outside_request = request(&config, &outside, &evaluation);
    outside_request.toolchain_projection = Some(&projection);
    let outside_plan = compile_bubblewrap_launch_plan(outside_request).unwrap();
    assert!(
        outside_plan
            .arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", source.as_str(), SANDBOX_NODE_ROOT])
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// Node.js discovery rejects incomplete distributions, non-executable runtime
/// files, and manager shims without consulting npm or version-manager state.
#[test]
fn node_toolchain_discovery_rejects_malformed_and_symlinked_distributions() {
    let base = std::env::temp_dir().join(format!(
        "mez-node-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("node-runtime");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/node"), "not executable").unwrap();
    let root = root.canonicalize().unwrap();
    let managers = [format!("node-runtime:{}", root.display())];

    let missing_layout =
        resolve_toolchain_projection(&[SandboxToolchainKind::Node], &managers, "linux")
            .unwrap_err();
    assert!(matches!(
        missing_layout.kind(),
        SandboxCompileErrorKind::InvalidInput | SandboxCompileErrorKind::ForbiddenHostPath
    ));

    std::fs::create_dir_all(root.join("lib")).unwrap();
    let non_executable =
        resolve_toolchain_projection(&[SandboxToolchainKind::Node], &managers, "linux")
            .unwrap_err();
    assert_eq!(
        non_executable.kind(),
        SandboxCompileErrorKind::ForbiddenHostPath
    );

    let external = base.join("external-node");
    std::fs::write(&external, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(root.join("bin/node")).unwrap();
    std::os::unix::fs::symlink(&external, root.join("bin/node")).unwrap();
    let search_path = std::env::join_paths([root.join("bin")]).unwrap();
    let symlink = discover_node_from_search_path(Some(&search_path)).unwrap_err();
    assert_eq!(symlink.kind(), SandboxCompileErrorKind::ForbiddenHostPath);

    let _ = std::fs::remove_dir_all(&base);
}

/// A selected Python base runtime remains read-only while one valid trusted
/// project `.venv` reuses project authority and takes deterministic PATH precedence.
#[test]
fn python_toolchain_composes_contained_project_environment() {
    let base = std::env::temp_dir().join(format!(
        "mez-python-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let runtime = base.join("python-runtime");
    std::fs::create_dir_all(runtime.join("bin")).unwrap();
    std::fs::create_dir_all(runtime.join("lib")).unwrap();
    std::fs::write(runtime.join("bin/python3"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        runtime.join("bin/python3"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let project = base.join("project");
    let environment = project.join(".venv");
    std::fs::create_dir_all(environment.join("bin")).unwrap();
    std::fs::write(environment.join("pyvenv.cfg"), "home = /python\n").unwrap();
    std::fs::write(environment.join("bin/python"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        environment.join("bin/python"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let runtime = runtime.canonicalize().unwrap();
    let project = project.canonicalize().unwrap();
    let environment = environment.canonicalize().unwrap();
    let managers = [format!("python-runtime:{}", runtime.display())];

    let projection = resolve_toolchain_projection_for_project(
        &[SandboxToolchainKind::Python],
        &managers,
        "linux",
        Some(&project),
    )
    .unwrap()
    .unwrap();
    assert_eq!(projection.roots[0].sandbox_destination, SANDBOX_PYTHON_ROOT);
    assert_eq!(projection.project_environments[0].host_path, environment);
    assert!(projection.executable_path().starts_with(&format!(
        "{}/bin:{}",
        environment.display(),
        SANDBOX_PYTHON_ROOT
    )));
    assert_eq!(
        projection
            .environment
            .get("PIP_CACHE_DIR")
            .map(String::as_str),
        Some("/home/mez/.cache/pip")
    );
    assert_eq!(
        projection
            .environment
            .get("UV_CACHE_DIR")
            .map(String::as_str),
        Some("/home/mez/.cache/uv")
    );
    assert_eq!(
        projection
            .environment
            .get("PYTHONNOUSERSITE")
            .map(String::as_str),
        Some("1")
    );
    for omitted in [
        "PYTHONHOME",
        "PYTHONPATH",
        "PIP_CONFIG_FILE",
        "UV_CONFIG_FILE",
    ] {
        assert!(!projection.environment.contains_key(omitted));
    }

    let maximum_authority = home_authority(&base.canonicalize().unwrap().display().to_string());
    projection.validate_authority(&maximum_authority).unwrap();
    let outside = authority();
    assert_eq!(
        projection.validate_authority(&outside).unwrap_err().kind(),
        SandboxCompileErrorKind::ToolchainOutsideAuthority
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// A selected custom toolchain resolves canonical multi-root state into fixed
/// read-only mounts, ordered PATH entries, and sandbox-path environment values.
#[test]
fn custom_toolchain_projection_is_fixed_read_only_and_authority_bounded() {
    let base = std::env::temp_dir().join(format!(
        "mez-custom-toolchain-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let runtime = base.join("runtime");
    let tools = base.join("tools");
    std::fs::create_dir_all(runtime.join("bin")).unwrap();
    std::fs::create_dir_all(tools.join("tools/bin")).unwrap();
    std::fs::write(runtime.join("bin/acme"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        runtime.join("bin/acme"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let runtime = runtime.canonicalize().unwrap();
    let tools = tools.canonicalize().unwrap();
    let base = base.canonicalize().unwrap();

    let mut config = config();
    config.toolchain_selections = vec![ToolchainSelection::custom_for_test("acme").unwrap()];
    config.custom_toolchains.insert(
        "acme".to_string(),
        CustomToolchainDefinition {
            description: Some("Acme SDK".to_string()),
            roots: vec![runtime.display().to_string(), tools.display().to_string()],
            path_entries: vec![
                CustomToolchainReference {
                    root_index: 0,
                    relative_path: "bin".to_string(),
                },
                CustomToolchainReference {
                    root_index: 1,
                    relative_path: "tools/bin".to_string(),
                },
            ],
            required_executables: vec![CustomToolchainReference {
                root_index: 0,
                relative_path: "bin/acme".to_string(),
            }],
            environment: BTreeMap::from([(
                "ACME_HOME".to_string(),
                CustomToolchainReference {
                    root_index: 0,
                    relative_path: ".".to_string(),
                },
            )]),
        },
    );
    let projection =
        resolve_configured_toolchain_projection_for_project(&config, &[], "linux", None, &[])
            .unwrap()
            .unwrap();
    assert_eq!(projection.custom_names, ["acme"]);
    assert_eq!(
        projection.executable_path(),
        "/opt/mez/toolchains/custom/acme/roots/0/bin:/opt/mez/toolchains/custom/acme/roots/1/tools/bin:/usr/bin:/bin"
    );
    assert_eq!(
        projection.environment.get("ACME_HOME").map(String::as_str),
        Some("/opt/mez/toolchains/custom/acme/roots/0")
    );

    let maximum_authority = home_authority(&base.display().to_string());
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let mut compile_request = request(&config, &maximum_authority, &evaluation);
    compile_request.toolchain_projection = Some(&projection);
    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();
    for (source, destination) in [
        (
            runtime.display().to_string(),
            "/opt/mez/toolchains/custom/acme/roots/0",
        ),
        (
            tools.display().to_string(),
            "/opt/mez/toolchains/custom/acme/roots/1",
        ),
    ] {
        assert!(
            plan.arguments
                .windows(3)
                .any(|args| args == ["--ro-bind", source.as_str(), destination])
        );
        assert!(
            !plan
                .arguments
                .windows(3)
                .any(|args| args == ["--bind", source.as_str(), destination])
        );
    }
    assert_eq!(
        projection
            .validate_authority(&authority())
            .unwrap_err()
            .kind(),
        SandboxCompileErrorKind::ToolchainOutsideAuthority
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// Custom resolution rejects escaping internal symlinks and non-executable
/// declared requirements before a Bubblewrap workload can be compiled.
#[test]
fn custom_toolchain_projection_rejects_escaping_and_non_executable_references() {
    let base = std::env::temp_dir().join(format!(
        "mez-custom-toolchain-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("runtime");
    let outside = base.join("outside");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("bin/acme"), "not executable").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
    let root = root.canonicalize().unwrap();

    let mut config = config();
    config.toolchain_selections = vec![ToolchainSelection::custom_for_test("acme").unwrap()];
    config.custom_toolchains.insert(
        "acme".to_string(),
        CustomToolchainDefinition {
            description: None,
            roots: vec![root.display().to_string()],
            path_entries: vec![CustomToolchainReference {
                root_index: 0,
                relative_path: "escape".to_string(),
            }],
            required_executables: vec![CustomToolchainReference {
                root_index: 0,
                relative_path: "bin/acme".to_string(),
            }],
            environment: BTreeMap::new(),
        },
    );
    let escaping =
        resolve_configured_toolchain_projection_for_project(&config, &[], "linux", None, &[])
            .unwrap_err();
    assert_eq!(escaping.kind(), SandboxCompileErrorKind::ForbiddenHostPath);

    config
        .custom_toolchains
        .get_mut("acme")
        .unwrap()
        .path_entries = vec![CustomToolchainReference {
        root_index: 0,
        relative_path: "bin".to_string(),
    }];
    let non_executable =
        resolve_configured_toolchain_projection_for_project(&config, &[], "linux", None, &[])
            .unwrap_err();
    assert_eq!(
        non_executable.kind(),
        SandboxCompileErrorKind::ForbiddenHostPath
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// Built-in and custom selections preserve configured PATH precedence while
/// retaining fixed descriptor metadata and deterministic system fallbacks.
#[test]
fn custom_toolchain_projection_composes_in_configured_selection_order() {
    let base = std::env::temp_dir().join(format!(
        "mez-custom-toolchain-order-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let custom = base.join("acme");
    let cargo_bin = base.join(".cargo/bin");
    let rustup = base.join(".rustup");
    std::fs::create_dir_all(custom.join("bin")).unwrap();
    std::fs::create_dir_all(&cargo_bin).unwrap();
    std::fs::create_dir_all(&rustup).unwrap();
    std::fs::write(custom.join("bin/acme"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        custom.join("bin/acme"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let custom = custom.canonicalize().unwrap();
    let cargo_bin = cargo_bin.canonicalize().unwrap();
    let rustup = rustup.canonicalize().unwrap();

    let mut config = config();
    config.toolchain_selections = vec![
        ToolchainSelection::custom_for_test("acme").unwrap(),
        ToolchainSelection::BuiltIn(SandboxToolchainKind::Rust),
    ];
    config.custom_toolchains.insert(
        "acme".to_string(),
        CustomToolchainDefinition {
            description: None,
            roots: vec![custom.display().to_string()],
            path_entries: vec![CustomToolchainReference {
                root_index: 0,
                relative_path: "bin".to_string(),
            }],
            required_executables: vec![CustomToolchainReference {
                root_index: 0,
                relative_path: "bin/acme".to_string(),
            }],
            environment: BTreeMap::new(),
        },
    );
    let managers = [
        format!("cargo-bin:{}", cargo_bin.display()),
        format!("rustup:{}", rustup.display()),
    ];

    let projection =
        resolve_configured_toolchain_projection_for_project(&config, &managers, "linux", None, &[])
            .unwrap()
            .unwrap();

    assert_eq!(projection.custom_names, ["acme"]);
    assert_eq!(projection.kinds, [SandboxToolchainKind::Rust]);
    assert_eq!(
        projection.executable_path(),
        "/opt/mez/toolchains/custom/acme/roots/0/bin:/opt/mez/toolchains/rust/cargo-bin:/usr/bin:/bin"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// Custom roots cannot overlap Mezzanine configuration or control-runtime
/// storage even when those paths otherwise satisfy structural validation.
#[test]
fn custom_toolchain_projection_rejects_mezzanine_owned_roots() {
    let base = std::env::temp_dir().join(format!(
        "mez-custom-toolchain-protected-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let protected = base.join("config");
    std::fs::create_dir_all(protected.join("bin")).unwrap();
    let protected = protected.canonicalize().unwrap();

    let mut config = config();
    config.toolchain_selections = vec![ToolchainSelection::custom_for_test("acme").unwrap()];
    config.custom_toolchains.insert(
        "acme".to_string(),
        CustomToolchainDefinition {
            description: None,
            roots: vec![protected.display().to_string()],
            path_entries: vec![CustomToolchainReference {
                root_index: 0,
                relative_path: "bin".to_string(),
            }],
            required_executables: Vec::new(),
            environment: BTreeMap::new(),
        },
    );

    let error = resolve_configured_toolchain_projection_for_project(
        &config,
        &[],
        "linux",
        None,
        std::slice::from_ref(&protected),
    )
    .unwrap_err();

    assert_eq!(error.kind(), SandboxCompileErrorKind::ForbiddenHostPath);
    let _ = std::fs::remove_dir_all(&base);
}

/// Python project-environment discovery fails closed for malformed or
/// symlinked `.venv` state instead of importing an external environment.
#[test]
fn python_toolchain_rejects_malformed_and_symlinked_project_environments() {
    let base = std::env::temp_dir().join(format!(
        "mez-python-project-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let runtime = base.join("python-runtime");
    std::fs::create_dir_all(runtime.join("bin")).unwrap();
    std::fs::create_dir_all(runtime.join("lib")).unwrap();
    std::fs::write(runtime.join("bin/python3"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(
        runtime.join("bin/python3"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let project = base.join("project");
    let environment = project.join(".venv");
    std::fs::create_dir_all(environment.join("bin")).unwrap();
    let runtime = runtime.canonicalize().unwrap();
    let project = project.canonicalize().unwrap();
    let managers = [format!("python-runtime:{}", runtime.display())];

    let malformed = resolve_toolchain_projection_for_project(
        &[SandboxToolchainKind::Python],
        &managers,
        "linux",
        Some(&project),
    )
    .unwrap_err();
    assert_eq!(malformed.kind(), SandboxCompileErrorKind::InvalidInput);

    std::fs::remove_dir_all(&environment).unwrap();
    let external = base.join("external-venv");
    std::fs::create_dir_all(&external).unwrap();
    std::os::unix::fs::symlink(&external, &environment).unwrap();
    let symlink = resolve_toolchain_projection_for_project(
        &[SandboxToolchainKind::Python],
        &managers,
        "linux",
        Some(&project),
    )
    .unwrap_err();
    assert_eq!(symlink.kind(), SandboxCompileErrorKind::ForbiddenHostPath);
    let _ = std::fs::remove_dir_all(&base);
}

/// A selected OCaml toolchain accepts only the direct trusted-project `_opam`
/// switch, gives its `bin` directory deterministic PATH precedence, and sets
/// `OPAM_SWITCH_PREFIX` without consulting or projecting global opam state.
#[test]
fn ocaml_toolchain_composes_contained_local_switch() {
    let base = std::env::temp_dir().join(format!(
        "mez-ocaml-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let project = base.join("project");
    let environment = project.join("_opam");
    for directory in ["bin", "lib", "share"] {
        std::fs::create_dir_all(environment.join(directory)).unwrap();
    }
    for executable in ["ocaml", "ocamlc", "ocamlopt", "dune"] {
        let path = environment.join("bin").join(executable);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let project = project.canonicalize().unwrap();
    let environment = environment.canonicalize().unwrap();

    let projection = resolve_toolchain_projection_for_project(
        &[SandboxToolchainKind::Ocaml],
        &[],
        "linux",
        Some(&project),
    )
    .unwrap()
    .unwrap();
    assert!(projection.roots.is_empty());
    assert_eq!(projection.project_environments.len(), 1);
    assert_eq!(projection.project_environments[0].host_path, environment);
    assert_eq!(
        projection.executable_path(),
        format!("{}/bin:/usr/bin:/bin", environment.display())
    );

    let maximum_authority = home_authority(&base.canonicalize().unwrap().display().to_string());
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let mut config = config();
    config.toolchains = vec![SandboxToolchainKind::Ocaml];
    let mut compile_request = request(&config, &maximum_authority, &evaluation);
    compile_request.toolchain_projection = Some(&projection);
    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();
    let environment_text = environment.display().to_string();
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| { args == ["--setenv", "OPAM_SWITCH_PREFIX", environment_text.as_str()] })
    );
    assert!(plan.arguments.windows(3).any(|args| {
        args == [
            "--setenv",
            "PATH",
            format!("{}/bin:/usr/bin:/bin", environment.display()).as_str(),
        ]
    }));
    assert!(
        !plan
            .arguments
            .iter()
            .any(|argument| argument.contains("/.opam"))
    );

    let outside = authority();
    assert_eq!(
        projection.validate_authority(&outside).unwrap_err().kind(),
        SandboxCompileErrorKind::ToolchainOutsideAuthority
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// OCaml local-switch discovery fails closed for absent, incomplete,
/// non-executable, or symlinked `_opam` state and never falls back to a global
/// manager switch elsewhere in the user's home.
#[test]
fn ocaml_toolchain_rejects_absent_malformed_and_symlinked_local_switches() {
    let base = std::env::temp_dir().join(format!(
        "mez-ocaml-project-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let project = base.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project = project.canonicalize().unwrap();

    assert!(
        discover_ocaml_project_environment(&project)
            .unwrap()
            .is_none()
    );
    let missing = resolve_toolchain_projection_for_project(
        &[SandboxToolchainKind::Ocaml],
        &[],
        "linux",
        Some(&project),
    )
    .unwrap_err();
    assert_eq!(
        missing.kind(),
        SandboxCompileErrorKind::UnsupportedRequirement
    );

    let environment = project.join("_opam");
    for directory in ["bin", "lib", "share"] {
        std::fs::create_dir_all(environment.join(directory)).unwrap();
    }
    let malformed = discover_ocaml_project_environment(&project).unwrap_err();
    assert_eq!(malformed.kind(), SandboxCompileErrorKind::InvalidInput);

    std::fs::remove_dir_all(&environment).unwrap();
    let external = base.join("global-opam-switch");
    std::fs::create_dir_all(&external).unwrap();
    std::os::unix::fs::symlink(&external, &environment).unwrap();
    let symlink = discover_ocaml_project_environment(&project).unwrap_err();
    assert_eq!(symlink.kind(), SandboxCompileErrorKind::ForbiddenHostPath);

    let _ = std::fs::remove_dir_all(&base);
}

/// Explicit LLVM, GCC, CMake, Ninja, and Meson selections compose only their
/// validated standalone roots in stable descriptor order without importing
/// ambient compiler flags, package-manager prefixes, or unrelated user tools.
#[test]
fn native_toolchains_compose_explicit_standalone_roots() {
    let base = std::env::temp_dir().join(format!(
        "mez-native-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let specifications = [
        (
            "llvm-toolchain",
            "llvm",
            ["clang", "clang++", "llvm-ar", "llvm-config"].as_slice(),
            ["lib/clang"].as_slice(),
        ),
        (
            "gcc-toolchain",
            "gcc",
            ["gcc", "g++", "gcc-ar"].as_slice(),
            ["lib/gcc"].as_slice(),
        ),
        (
            "cmake-toolchain",
            "cmake",
            ["cmake", "ctest"].as_slice(),
            ["share/cmake"].as_slice(),
        ),
        (
            "ninja-toolchain",
            "ninja",
            ["ninja"].as_slice(),
            [].as_slice(),
        ),
        (
            "meson-toolchain",
            "meson",
            ["meson"].as_slice(),
            [].as_slice(),
        ),
    ];
    let mut managers = Vec::new();
    let mut roots = Vec::new();
    for (evidence, name, executables, directories) in specifications {
        let root = base.join(name);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        for directory in directories {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        for executable in executables {
            let path = root.join("bin").join(executable);
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let root = root.canonicalize().unwrap();
        managers.push(format!("{evidence}:{}", root.display()));
        roots.push(root);
    }

    let projection = resolve_toolchain_projection(
        &[
            SandboxToolchainKind::Llvm,
            SandboxToolchainKind::Gcc,
            SandboxToolchainKind::Cmake,
            SandboxToolchainKind::Ninja,
            SandboxToolchainKind::Meson,
        ],
        &managers,
        "linux",
    )
    .unwrap()
    .unwrap();
    assert_eq!(projection.roots.len(), 5);
    assert_eq!(
        projection.executable_path(),
        "/opt/mez/toolchains/llvm/root/bin:/opt/mez/toolchains/gcc/root/bin:/opt/mez/toolchains/cmake/root/bin:/opt/mez/toolchains/ninja/root/bin:/opt/mez/toolchains/meson/root/bin:/usr/bin:/bin"
    );
    for variable in ["CC", "CXX", "CFLAGS", "CPPFLAGS", "LDFLAGS"] {
        assert!(!projection.environment.contains_key(variable));
    }
    assert_eq!(
        projection
            .roots
            .iter()
            .map(|root| root.host_path.clone())
            .collect::<Vec<_>>(),
        roots
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// Native tooling discovery rejects an incomplete compiler root and a selected
/// executable symlink instead of broadening projection to its package prefix.
#[test]
fn native_toolchains_reject_incomplete_and_symlinked_roots() {
    let base = std::env::temp_dir().join(format!(
        "mez-native-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let llvm = base.join("llvm");
    std::fs::create_dir_all(llvm.join("bin")).unwrap();
    std::fs::write(llvm.join("bin/clang"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(
        llvm.join("bin/clang"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let llvm = llvm.canonicalize().unwrap();
    let incomplete = resolve_toolchain_projection(
        &[SandboxToolchainKind::Llvm],
        &[format!("llvm-toolchain:{}", llvm.display())],
        "linux",
    )
    .unwrap_err();
    assert_eq!(incomplete.kind(), SandboxCompileErrorKind::InvalidInput);

    let external = base.join("external-ninja");
    std::fs::write(&external, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
    let ninja = base.join("ninja/bin");
    std::fs::create_dir_all(&ninja).unwrap();
    std::os::unix::fs::symlink(&external, ninja.join("ninja")).unwrap();
    let search_path = std::env::join_paths([ninja]).unwrap();
    let symlink = discover_ninja_from_search_path(Some(&search_path)).unwrap_err();
    assert_eq!(symlink.kind(), SandboxCompileErrorKind::ForbiddenHostPath);
    let _ = std::fs::remove_dir_all(&base);
}

/// A complete standalone Swift distribution is accepted only on Linux, mounts
/// read-only at its fixed root, and redirects SwiftPM mutable state beneath the
/// managed home without inheriting Apple SDK or compiler/linker environment.
#[test]
fn swift_toolchain_projection_is_linux_only_and_state_isolated() {
    let base = std::env::temp_dir().join(format!(
        "mez-swift-projection-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("swift");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("lib/swift/linux")).unwrap();
    for executable in ["swift", "swiftc", "swift-package", "sourcekit-lsp"] {
        let path = root.join("bin").join(executable);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let root = root.canonicalize().unwrap();
    let managers = [format!("swift-toolchain:{}", root.display())];

    let projection =
        resolve_toolchain_projection(&[SandboxToolchainKind::Swift], &managers, "linux")
            .unwrap()
            .unwrap();
    assert_eq!(projection.roots[0].host_path, root);
    assert_eq!(projection.roots[0].sandbox_destination, SANDBOX_SWIFT_ROOT);
    assert_eq!(projection.executable_path(), SANDBOX_SWIFT_PATH);
    assert_eq!(
        projection.environment.get("SWIFTPM_CACHE_PATH"),
        Some(&"/home/mez/.cache/swiftpm".to_string())
    );
    assert_eq!(
        projection.environment.get("SWIFTPM_CONFIG_PATH"),
        Some(&"/home/mez/.config/swiftpm".to_string())
    );
    assert_eq!(projection.managed_state.len(), 3);
    for variable in [
        "SDKROOT",
        "DEVELOPER_DIR",
        "TOOLCHAINS",
        "CC",
        "CXX",
        "CFLAGS",
        "LDFLAGS",
    ] {
        assert!(!projection.environment.contains_key(variable));
    }

    let unsupported =
        resolve_toolchain_projection(&[SandboxToolchainKind::Swift], &managers, "macos")
            .unwrap_err();
    assert_eq!(
        unsupported.kind(),
        SandboxCompileErrorKind::UnsupportedRequirement
    );
    assert!(unsupported.message().contains("unsupported on macos"));
    let _ = std::fs::remove_dir_all(&base);
}

/// Swift discovery fails closed for incomplete distributions and manager shims
/// instead of projecting swiftenv, asdf, mise, or an unrelated host prefix.
#[test]
fn swift_toolchain_rejects_incomplete_and_symlinked_distributions() {
    let base = std::env::temp_dir().join(format!(
        "mez-swift-invalid-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let incomplete = base.join("incomplete");
    std::fs::create_dir_all(incomplete.join("bin")).unwrap();
    let swiftc = incomplete.join("bin/swiftc");
    std::fs::write(&swiftc, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&swiftc, std::fs::Permissions::from_mode(0o755)).unwrap();
    let search_path = std::env::join_paths([incomplete.join("bin")]).unwrap();
    let incomplete = discover_swift_from_search_path(Some(&search_path)).unwrap_err();
    assert_eq!(incomplete.kind(), SandboxCompileErrorKind::InvalidInput);

    let external = base.join("external-swiftc");
    std::fs::write(&external, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
    let shim = base.join("shim/bin");
    std::fs::create_dir_all(&shim).unwrap();
    std::os::unix::fs::symlink(&external, shim.join("swiftc")).unwrap();
    let search_path = std::env::join_paths([shim]).unwrap();
    let symlink = discover_swift_from_search_path(Some(&search_path)).unwrap_err();
    assert_eq!(symlink.kind(), SandboxCompileErrorKind::ForbiddenHostPath);
    let _ = std::fs::remove_dir_all(&base);
}

/// Descriptor resolution and final launch validation reject ambiguous
/// selection and any mutation of code-owned projection metadata or classes.
#[test]
fn toolchain_projection_rejects_duplicates_and_tampered_metadata() {
    let managers = [
        "cargo-bin:/home/alice/.cargo/bin".to_string(),
        "rustup:/home/alice/.rustup".to_string(),
    ];
    let duplicate = resolve_toolchain_projection(
        &[SandboxToolchainKind::Rust, SandboxToolchainKind::Rust],
        &managers,
        "linux",
    )
    .unwrap_err();
    assert_eq!(duplicate.kind(), SandboxCompileErrorKind::InvalidInput);

    let projection =
        resolve_toolchain_projection(&[SandboxToolchainKind::Rust], &managers, "linux")
            .unwrap()
            .unwrap();

    let mut invalid_class = projection.clone();
    invalid_class.roots[0].authority_class = ToolchainAuthorityClass::Credential;
    assert_eq!(
        invalid_class.validate().unwrap_err().kind(),
        SandboxCompileErrorKind::InvalidInput
    );

    let mut invalid_path = projection.clone();
    invalid_path
        .path_entries
        .push(SANDBOX_RUST_CARGO_BIN.to_string());
    assert_eq!(
        invalid_path.validate().unwrap_err().kind(),
        SandboxCompileErrorKind::InvalidInput
    );

    let mut invalid_environment = projection.clone();
    invalid_environment
        .environment
        .insert("RUSTUP_HOME".to_string(), "/unexpected".to_string());
    assert_eq!(
        invalid_environment.validate().unwrap_err().kind(),
        SandboxCompileErrorKind::InvalidInput
    );

    let mut invalid_state = projection.clone();
    invalid_state.managed_state[0].sandbox_path = "/tmp/cargo";
    assert_eq!(
        invalid_state.validate().unwrap_err().kind(),
        SandboxCompileErrorKind::InvalidInput
    );

    let mut colliding_mount = projection;
    colliding_mount.roots.push(colliding_mount.roots[0].clone());
    assert_eq!(
        colliding_mount.validate().unwrap_err().kind(),
        SandboxCompileErrorKind::InvalidInput
    );
}

/// Enabled toolchains add their validated roots to effective read authority
/// while exposing them only at fixed read-only sandbox destinations.
#[test]
fn rust_toolchain_projection_adds_read_authority_without_generic_mounts() {
    let mut config = config();
    config.toolchains = vec![SandboxToolchainKind::Rust];
    let authority = authority();
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());

    for managers in [
        [
            "cargo-bin:/outside/.cargo/bin".to_string(),
            "rustup:/outside/.rustup".to_string(),
        ],
        [
            "cargo-bin:/workspace/.cargo/bin".to_string(),
            "rustup:/outside/.rustup".to_string(),
        ],
        [
            "cargo-bin:/workspace2/.cargo/bin".to_string(),
            "rustup:/workspace2/.rustup".to_string(),
        ],
    ] {
        let projection = resolve_toolchain_projection(&config.toolchains, &managers, "linux")
            .unwrap()
            .unwrap();
        let mut compile_request = request(&config, &authority, &evaluation);
        compile_request.toolchain_projection = Some(&projection);

        let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();
        for root in &projection.roots {
            let source = root.host_path.to_string_lossy();
            assert!(plan.arguments.windows(3).any(|args| {
                args[0] == "--ro-bind" && args[1] == source && args[2] == root.sandbox_destination
            }));
            assert!(
                !plan.arguments.windows(3).any(|args| {
                    args[0] == "--ro-bind" && args[1] == source && args[2] == source
                })
            );
        }
    }
}

/// Rust selection fails closed unless bootstrap evidence supplies both
/// canonical allowlisted roots and never accepts an arbitrary host directory.
#[test]
fn rust_toolchain_resolution_rejects_missing_and_arbitrary_roots() {
    let mut config = config();
    config.toolchains = vec![SandboxToolchainKind::Rust];

    let missing = resolve_toolchain_projection(
        &config.toolchains,
        &["rustup:/home/alice/.rustup".into()],
        "linux",
    )
    .unwrap_err();
    assert_eq!(
        missing.kind(),
        SandboxCompileErrorKind::UnsupportedRequirement
    );

    let arbitrary = resolve_toolchain_projection(
        &config.toolchains,
        &[
            "cargo-bin:/home/alice/tools/bin".into(),
            "rustup:/home/alice/.rustup".into(),
        ],
        "linux",
    )
    .unwrap_err();
    assert_eq!(arbitrary.kind(), SandboxCompileErrorKind::ForbiddenHostPath);
}

/// Strict bootstrap discovery accepts exactly one canonical record for each
/// Rust root while ignoring unrelated environment-manager evidence.
///
/// This protects all runtime callers from reimplementing manager parsing and
/// proves the discovered roots use the same fixed projection metadata as the
/// launch compiler and direct-user CLI.
#[test]
fn rust_toolchain_discovery_accepts_strict_records_and_shared_metadata() {
    let discovery = discover_rust_from_environment_managers(&[
        "node:/home/alice/.local/node".to_string(),
        "cargo-bin:/home/alice/.cargo/bin".to_string(),
        "rustup:/home/alice/.rustup".to_string(),
    ])
    .unwrap();

    assert_eq!(discovery.cargo_bin, PathBuf::from("/home/alice/.cargo/bin"));
    assert_eq!(discovery.rustup_home, PathBuf::from("/home/alice/.rustup"));
    assert_eq!(
        SUPPORTED_SANDBOX_TOOLCHAIN_KINDS
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>(),
        vec![
            "rust", "zig", "go", "deno", "bun", "node", "python", "jdk", "maven", "gradle",
            "dotnet", "dart", "kotlin", "ruby", "php", "composer", "erlang", "elixir", "ghc",
            "cabal", "stack", "ocaml", "llvm", "gcc", "cmake", "ninja", "meson", "swift"
        ]
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("rust"),
        Some(SandboxToolchainKind::Rust)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("python"),
        Some(SandboxToolchainKind::Python)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("jdk"),
        Some(SandboxToolchainKind::Jdk)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("maven"),
        Some(SandboxToolchainKind::Maven)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("gradle"),
        Some(SandboxToolchainKind::Gradle)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("swift"),
        Some(SandboxToolchainKind::Swift)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("dotnet"),
        Some(SandboxToolchainKind::Dotnet)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("dart"),
        Some(SandboxToolchainKind::Dart)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("kotlin"),
        Some(SandboxToolchainKind::Kotlin)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("ruby"),
        Some(SandboxToolchainKind::Ruby)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("php"),
        Some(SandboxToolchainKind::Php)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("composer"),
        Some(SandboxToolchainKind::Composer)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("erlang"),
        Some(SandboxToolchainKind::Erlang)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("elixir"),
        Some(SandboxToolchainKind::Elixir)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("ghc"),
        Some(SandboxToolchainKind::Ghc)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("cabal"),
        Some(SandboxToolchainKind::Cabal)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("stack"),
        Some(SandboxToolchainKind::Stack)
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("ocaml"),
        Some(SandboxToolchainKind::Ocaml)
    );
    assert_eq!(
        SANDBOX_RUST_PATH,
        format!("{SANDBOX_RUST_CARGO_BIN}:/usr/bin:/bin")
    );
    assert_eq!(SANDBOX_RUSTUP_HOME, "/opt/mez/toolchains/rust/rustup");
}

/// Strict bootstrap discovery rejects empty and duplicate Rust manager
/// records instead of silently selecting the first ambiguous host path.
#[test]
fn rust_toolchain_discovery_rejects_malformed_and_duplicate_records() {
    for managers in [
        vec![
            "cargo-bin".to_string(),
            "rustup:/home/alice/.rustup".to_string(),
        ],
        vec![
            "cargo-bin:/home/alice/.cargo/bin".to_string(),
            "cargo-bin:/home/bob/.cargo/bin".to_string(),
            "rustup:/home/alice/.rustup".to_string(),
        ],
        vec![
            "cargo-bin:/home/alice/.cargo/bin".to_string(),
            "rustup:".to_string(),
        ],
    ] {
        let error = discover_rust_from_environment_managers(&managers).unwrap_err();
        assert_eq!(error.kind(), SandboxCompileErrorKind::InvalidInput);
    }
}

/// Shared Rust-root validation rejects overlap, runtime directories, and
/// unexpected allowlist names before roots can reach launch compilation.
#[test]
fn rust_toolchain_discovery_rejects_overlapping_and_forbidden_roots() {
    for managers in [
        vec![
            "cargo-bin:/home/alice/.rustup/.cargo/bin".to_string(),
            "rustup:/home/alice/.rustup".to_string(),
        ],
        vec![
            "cargo-bin:/home/alice/.cargo/bin".to_string(),
            "rustup:/run/user/1000/.rustup".to_string(),
        ],
        vec![
            "cargo-bin:/home/alice/tools/bin".to_string(),
            "rustup:/home/alice/.rustup".to_string(),
        ],
    ] {
        let error = discover_rust_from_environment_managers(&managers).unwrap_err();
        assert_eq!(error.kind(), SandboxCompileErrorKind::ForbiddenHostPath);
    }
}

/// Direct-user home discovery preserves partial availability while rejecting
/// symlinked conventional roots without creating or modifying filesystem state.
#[test]
fn rust_toolchain_home_discovery_preserves_partial_state_and_rejects_symlinks() {
    let root = std::env::temp_dir().join(format!(
        "mez-toolchain-home-discovery-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".cargo/bin")).unwrap();

    let partial = discover_rust_from_home(Some(&root)).unwrap();
    assert!(partial.cargo_bin.is_some());
    assert!(partial.rustup_home.is_none());
    assert!(!partial.available());

    let external = root.with_extension("external-rustup");
    let _ = std::fs::remove_dir_all(&external);
    std::fs::create_dir_all(&external).unwrap();
    std::os::unix::fs::symlink(&external, root.join(".rustup")).unwrap();
    let error = discover_rust_from_home(Some(&root)).unwrap_err();
    assert_eq!(error.kind(), SandboxCompileErrorKind::ForbiddenHostPath);

    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(external);
}
