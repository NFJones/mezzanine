//! Pure regression coverage for Bubblewrap policy compilation.

#[cfg(target_os = "linux")]
mod real_bubblewrap;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

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
        git_user_name: None,
        git_user_email: None,
        toolchains: Vec::new(),
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

/// Authorized network requirements use the connected profile while credential,
/// process-control, stateful, and interactive requirements still fail closed.
#[test]
fn unsupported_requirements_fail_before_launch() {
    let config = config();
    let authority = authority();
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
        projection.environment.get("ZIG_GLOBAL_CACHE_DIR"),
        Some(&"/home/mez/.cache/zig")
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
    assert_eq!(
        compile_bubblewrap_launch_plan(outside_request)
            .unwrap_err()
            .kind(),
        SandboxCompileErrorKind::ToolchainOutsideAuthority
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
        assert_eq!(projection.environment.get(name), Some(&value));
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
    assert_eq!(
        compile_bubblewrap_launch_plan(outside_request)
            .unwrap_err()
            .kind(),
        SandboxCompileErrorKind::ToolchainOutsideAuthority
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
        projection.environment.get("DENO_DIR"),
        Some(&"/home/mez/.cache/deno")
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
    assert_eq!(
        compile_bubblewrap_launch_plan(outside_request)
            .unwrap_err()
            .kind(),
        SandboxCompileErrorKind::ToolchainOutsideAuthority
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
    invalid_path.path_entries.push(SANDBOX_RUST_CARGO_BIN);
    assert_eq!(
        invalid_path.validate().unwrap_err().kind(),
        SandboxCompileErrorKind::InvalidInput
    );

    let mut invalid_environment = projection.clone();
    invalid_environment
        .environment
        .insert("RUSTUP_HOME", "/unexpected");
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

/// Toolchain convenience must not project either bootstrap-derived root from
/// outside the pane-resolved maximum read authority.
#[test]
fn rust_toolchain_projection_rejects_roots_outside_maximum_authority() {
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

        let error = compile_bubblewrap_launch_plan(compile_request).unwrap_err();
        assert_eq!(
            error.kind(),
            SandboxCompileErrorKind::ToolchainOutsideAuthority
        );
        assert!(!error.kind().approval_fallback_eligible());
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
        vec!["rust", "zig", "go", "deno"]
    );
    assert_eq!(
        parse_sandbox_toolchain_kind("rust"),
        Some(SandboxToolchainKind::Rust)
    );
    assert_eq!(parse_sandbox_toolchain_kind("python"), None);
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
