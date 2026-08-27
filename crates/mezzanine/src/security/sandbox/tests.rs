//! Pure regression coverage for Bubblewrap policy compilation.

#[cfg(target_os = "linux")]
mod real_bubblewrap;

use std::collections::BTreeMap;
use std::os::unix::net::UnixListener;
use std::path::Path;

use mez_agent::permissions::{
    CandidateEvaluation, EffectCompleteness, EffectiveCommandEffects, PathScopes,
    PermissionEvaluation, ResolvedPathEvidence, ResolvedPathKind, ResolvedPathObjectKind,
    RuleDecision,
};

use crate::runtime::{ConfiguredSandboxEnvironment, ConfiguredSandboxGroups};

use super::*;

fn config() -> BubblewrapConfig {
    BubblewrapConfig {
        executable: "/usr/bin/bwrap".to_string(),
        unavailable: SandboxUnavailablePolicy::Fail,
        network: SandboxNetworkMode::Isolated,
        environment: SandboxEnvironmentPolicy::Minimal,
        group_whitelist: ConfiguredSandboxGroups::default(),
        env_whitelist: ConfiguredSandboxEnvironment::default(),
        git_user_name: None,
        git_user_email: None,
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
                object_kind: ResolvedPathObjectKind::Directory,
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
                object_kind: ResolvedPathObjectKind::Directory,
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
        identity: resolve_sandbox_identity(
            &config.group_whitelist,
            &identity::current_process_environment_signature().unwrap(),
        )
        .unwrap(),
        capability: capability(config),
        pane_environment_signature: "pane-env-sha256",
        environment_evidence: Box::leak(Box::new(
            mez_agent::shell::PaneEnvironmentEvidence::restrictive(
                &mez_agent::shell::PaneEnvironmentRequest::new(
                    config.env_whitelist.requested_names.clone(),
                )
                .unwrap(),
                "test_default",
            ),
        )),
        network_policy: NetworkPolicy::Prompt,
        maximum_authority: authority,
        permission_evaluation: evaluation,
        preserve_maximum_authority: false,
        child_shell_path: "/bin/sh",
        command_file_host_path: BUBBLEWRAP_COMMAND_FILE_HOST_PLACEHOLDER,
        managed_home: None,
        pane_home_directory: None,
        stateful: false,
        interactive: false,
    }
}

fn capability(config: &BubblewrapConfig) -> BubblewrapCapability {
    let plan = bubblewrap_capability_probe_plan(config, "/bin/sh").unwrap();
    parse_bubblewrap_capability_probe("%1", "pane-env-sha256", 0, &plan, 0, plan.expected_stdout)
        .unwrap()
}

/// Builds a capability whose digest is bound to the supplied pane environment
/// evidence, matching the production probe and launch sequence.
fn capability_with_environment(
    config: &BubblewrapConfig,
    environment_evidence: &mez_agent::shell::PaneEnvironmentEvidence,
) -> BubblewrapCapability {
    let identity = resolve_sandbox_identity(
        &config.group_whitelist,
        &identity::current_process_environment_signature().unwrap(),
    )
    .unwrap();
    let plan = bubblewrap_capability_probe_plan_for_identity(
        config,
        "/bin/sh",
        &identity,
        environment_evidence,
    )
    .unwrap();
    parse_bubblewrap_capability_probe("%1", "pane-env-sha256", 0, &plan, 0, plan.expected_stdout)
        .unwrap()
}

/// Verifies a verified PATH named by the environment whitelist reaches both
/// the capability profile and the ordinary sandbox workload unchanged.
#[test]
fn sandbox_compiler_forwards_whitelisted_path() {
    let mut config = config();
    config.env_whitelist = ConfiguredSandboxEnvironment {
        requested_names: vec!["PATH".to_string()],
    };
    let environment_request =
        mez_agent::shell::PaneEnvironmentRequest::new(config.env_whitelist.requested_names.clone())
            .unwrap();
    let environment_evidence = mez_agent::shell::PaneEnvironmentEvidence::from_parts(
        &environment_request,
        BTreeMap::from([(
            "PATH".to_string(),
            "/home/mez/.cargo/bin:/usr/bin:/bin".to_string(),
        )]),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = authority();
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let mut compile_request = request(&config, &authority, &evaluation);
    compile_request.capability = capability_with_environment(&config, &environment_evidence);
    compile_request.environment_evidence = &environment_evidence;

    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();
    assert!(plan.arguments.windows(3).any(|arguments| {
        arguments == ["--setenv", "PATH", "/home/mez/.cargo/bin:/usr/bin:/bin"]
    }));
}

/// Verifies the compiler accepts canonical host-resolved authority used by
/// native shell mode while retaining the same permission and mount checks as
/// pane-shell-resolved authority.
#[test]
fn sandbox_compiler_accepts_host_resolved_authority() {
    let config = config();
    let shell_authority = authority();
    let host_authority = PathScopes::try_host_resolved_with_evidence(
        shell_authority.current_directory,
        shell_authority.read_scopes,
        shell_authority.write_scopes,
        shell_authority.path_evidence,
    )
    .unwrap();
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());

    let plan =
        compile_bubblewrap_launch_plan(request(&config, &host_authority, &evaluation)).unwrap();

    assert_eq!(
        plan.audit_summary.runtime_profile_version,
        BUBBLEWRAP_RUNTIME_PROFILE_VERSION
    );
}

/// Only explicitly allowed evaluations may produce a Bubblewrap launch plan;
/// pending prompts and hard forbids both remain non-dispatchable.
#[test]
fn sandbox_compiler_requires_explicit_allow() {
    let config = config();
    let authority = authority();
    let mut prompt = evaluation(EffectCompleteness::Unknown, effects());
    prompt.decision = RuleDecision::Prompt;

    let error = compile_bubblewrap_launch_plan(request(&config, &authority, &prompt)).unwrap_err();
    assert_eq!(error.kind(), SandboxCompileErrorKind::Unauthorized);

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

/// The backend-tagged lifecycle parser preserves Bubblewrap ordering and
/// exposes only typed child and payload-establishment evidence.
#[test]
fn sandbox_lifecycle_parser_dispatches_to_bubblewrap_contract() {
    let status = parse_sandbox_lifecycle_status(
        SandboxBackend::Bubblewrap,
        "{\"child-pid\":42}\n{\"exit-code\":0}\n",
    )
    .unwrap();

    assert_eq!(status.child_pid(), Some(42));
    assert_eq!(status.exit_code(), Some(0));
    assert!(
        parse_sandbox_lifecycle_status(
            SandboxBackend::Bubblewrap,
            "{\"exit-code\":0}\n{\"child-pid\":42}\n",
        )
        .is_err()
    );
}

/// The Seatbelt lifecycle parser accepts only the ordered code-owned launcher
/// sequence and distinguishes sandbox entry from payload establishment and
/// completion. Duplicate, reordered, and unknown records remain untrusted.
#[test]
fn sandbox_lifecycle_parser_validates_seatbelt_launcher_sequence() {
    let status = parse_sandbox_lifecycle_status(
        SandboxBackend::Seatbelt,
        "{\"version\":1,\"event\":\"sandbox-entered\"}\n{\"version\":1,\"event\":\"child-established\",\"child-pid\":42}\n{\"version\":1,\"event\":\"exit\",\"exit-code\":7}\n",
    )
    .unwrap();

    assert!(status.sandbox_entered());
    assert!(status.payload_established());
    assert_eq!(status.child_pid(), Some(42));
    assert_eq!(status.exit_code(), Some(7));
    for invalid in [
        "{\"version\":1,\"event\":\"child-established\",\"child-pid\":42}\n",
        "{\"version\":1,\"event\":\"sandbox-entered\"}\n{\"version\":1,\"event\":\"sandbox-entered\"}\n",
        "{\"version\":1,\"event\":\"sandbox-entered\"}\n{\"version\":1,\"event\":\"exit\",\"exit-code\":0}\n",
        "{\"version\":2,\"event\":\"sandbox-entered\"}\n",
    ] {
        assert!(parse_sandbox_lifecycle_status(SandboxBackend::Seatbelt, invalid).is_err());
    }
}

/// Live Bubblewrap failures provide one concise, authority-preserving command
/// that expands into the existing structured sandbox diagnostics and remedies.
#[test]
fn bubblewrap_failure_remediation_points_to_verbose_status() {
    let remediated = bubblewrap_failure_remediation("Bubblewrap probe failed.");
    assert_eq!(
        remediated,
        "Bubblewrap probe failed. Run `mez sandbox status --verbose` to inspect the executable, authority, and configuration remedies."
    );
    assert_eq!(bubblewrap_failure_remediation(&remediated), remediated);
}

/// Builds resolver-backed authority for one protected IPC path so tests prove
/// that the shared compiler consumes trusted object-kind evidence rather than
/// rediscovering filesystem metadata.
fn protected_ipc_authority(
    path: &Path,
    object_kind: ResolvedPathObjectKind,
    write: bool,
) -> PathScopes {
    let canonical_path = path.to_string_lossy().into_owned();
    let evidence = BTreeMap::from([(
        canonical_path.clone(),
        ResolvedPathEvidence {
            canonical_path: canonical_path.clone(),
            kind: ResolvedPathKind::Existing,
            nearest_existing_parent: canonical_path.clone(),
            object_kind,
        },
    )]);
    let read_scopes = vec!["/workspace".to_string(), canonical_path.clone()];
    let write_scopes = write.then_some(canonical_path).into_iter().collect();
    PathScopes::try_shell_resolved_with_evidence("/workspace", read_scopes, write_scopes, evidence)
        .unwrap()
}

/// Unix sockets are the sole IPC endpoint type that may receive the narrow
/// read-only exception; regular files and directories must remain forbidden.
#[test]
fn ipc_read_scope_requires_an_existing_unix_socket() {
    let root = std::env::temp_dir().join(format!(
        "mez-ipc-read-scope-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let socket = root.join("service.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    let file = root.join("regular");
    std::fs::write(&file, "not a socket").unwrap();

    let socket_authority =
        protected_ipc_authority(&socket, ResolvedPathObjectKind::UnixSocket, false);
    assert!(validate_ipc_read_scope(&socket_authority, socket.to_str().unwrap()).is_ok());
    let file_authority = protected_ipc_authority(&file, ResolvedPathObjectKind::File, false);
    assert_eq!(
        validate_ipc_read_scope(&file_authority, file.to_str().unwrap())
            .unwrap_err()
            .kind(),
        SandboxCompileErrorKind::ForbiddenHostPath
    );
    let directory_authority =
        protected_ipc_authority(&root, ResolvedPathObjectKind::Directory, false);
    assert_eq!(
        validate_ipc_read_scope(&directory_authority, Path::new(&root).to_str().unwrap())
            .unwrap_err()
            .kind(),
        SandboxCompileErrorKind::ForbiddenHostPath
    );

    drop(_listener);
    std::fs::remove_dir_all(&root).unwrap();
}

/// An exact Unix socket below the protected runtime root may be projected
/// read-only, while the protected directory, a regular file, and write access
/// remain forbidden by the production authority compiler.
#[test]
fn protected_ipc_socket_read_scope_is_compiled_read_only() {
    let root = Path::new("/run/user").join(format!(
        "mez-ipc-authority-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    if let Err(error) = std::fs::create_dir_all(&root) {
        if matches!(
            error.kind(),
            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
        ) {
            eprintln!(
                "skipping IPC read-scope test: cannot create {}",
                root.display()
            );
            return;
        }
        panic!("create IPC socket fixture {}: {error}", root.display());
    }
    let socket = root.join("service.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    let file = root.join("regular");
    std::fs::write(&file, "not a socket").unwrap();
    let config = config();
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let socket_scope = socket.to_string_lossy().into_owned();

    let socket_authority =
        protected_ipc_authority(&socket, ResolvedPathObjectKind::UnixSocket, false);
    let plan =
        compile_bubblewrap_launch_plan(request(&config, &socket_authority, &evaluation)).unwrap();
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", socket_scope.as_str(), socket_scope.as_str()])
    );
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--symlink", "/run", "/var/run"])
    );

    for (read_scope, object_kind) in [
        (root.as_path(), ResolvedPathObjectKind::Directory),
        (file.as_path(), ResolvedPathObjectKind::File),
    ] {
        let authority = protected_ipc_authority(read_scope, object_kind, false);
        assert_eq!(
            compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation))
                .unwrap_err()
                .kind(),
            SandboxCompileErrorKind::ForbiddenHostPath
        );
    }

    let write_authority =
        protected_ipc_authority(&socket, ResolvedPathObjectKind::UnixSocket, true);
    assert_eq!(
        compile_bubblewrap_launch_plan(request(&config, &write_authority, &evaluation))
            .unwrap_err()
            .kind(),
        SandboxCompileErrorKind::ForbiddenHostPath
    );

    drop(_listener);
    std::fs::remove_dir_all(&root).unwrap();
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
    assert_eq!(plan.audit_summary.read_only_grant_count, 1);
    assert_eq!(plan.audit_summary.read_write_grant_count, 1);
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

/// Broad deterministic user-home authority projects its configured scope
/// without inspecting or masking credential-named descendants.
#[test]
fn user_home_authority_does_not_emit_credential_masks() {
    let config = config();
    let authority = home_authority("/home/alice");
    let mut unknown = effects();
    unknown.unknown = true;
    let evaluation = evaluation(EffectCompleteness::Unknown, unknown);

    let plan = compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap();
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", "/home/alice", "/home/alice"])
    );
    for protected in [
        "/home/alice/.ssh",
        "/home/alice/.gnupg",
        "/home/alice/.aws",
        "/home/alice/.azure",
        "/home/alice/.kube",
        "/home/alice/.docker",
        "/home/alice/.config/mezzanine",
    ] {
        assert!(
            !plan
                .arguments
                .windows(2)
                .any(|args| args == ["--tmpfs", protected]),
            "credential path {protected} must not be implicitly masked"
        );
    }
}

/// Complete effects may narrow command authority to an explicitly configured
/// credential-named descendant without implicit sandbox policy rejection.
#[test]
fn narrowed_credential_directory_authority_is_allowed() {
    let config = config();
    let authority = home_authority("/home/alice");
    let mut complete = effects();
    complete.reads.push(".ssh".to_string());
    let evaluation = evaluation(EffectCompleteness::Complete, complete);

    let plan = compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap();

    assert_eq!(
        plan.audit_summary.authority_source,
        SandboxAuthoritySource::Narrowed
    );
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", "/home/alice/.ssh", "/home/alice/.ssh"])
    );
}

/// Multi-user home roots remain forbidden because they exceed a bounded user
/// authority boundary.
#[test]
fn multi_user_home_authority_fails_closed() {
    let config = config();
    let authority = home_authority("/home");
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());

    let error =
        compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap_err();

    assert_eq!(error.kind(), SandboxCompileErrorKind::ForbiddenHostPath);
}

/// Direct credential-directory authority is accepted when explicitly
/// configured by the user.
#[test]
fn direct_credential_directory_authority_is_allowed() {
    let config = config();
    let authority = home_authority("/home/alice/.ssh");
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());

    let plan = compile_bubblewrap_launch_plan(request(&config, &authority, &evaluation)).unwrap();
    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", "/home/alice/.ssh", "/home/alice/.ssh"])
    );
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
    assert_eq!(plan.expected_stdout, "mez-bubblewrap-capability-v6");
    assert!(plan.arguments.contains(&"--unshare-net".to_string()));
    assert!(plan.arguments.contains(&"--uid".to_string()));
    assert!(plan.arguments.contains(&"--gid".to_string()));
    assert!(plan.arguments.contains(&"--disable-userns".to_string()));
    assert!(plan.arguments.contains(&"--clearenv".to_string()));
    assert!(
        plan.arguments
            .last()
            .is_some_and(|script| script.contains("/proc/self/status"))
    );
    assert!(
        plan.arguments
            .last()
            .is_some_and(|script| script.contains("while read -r key"))
    );
    assert!(
        plan.arguments
            .last()
            .is_some_and(|script| !script.contains("id -u") && !script.contains("id -g"))
    );
    assert!(
        plan.arguments
            .iter()
            .any(|argument| argument.contains("/etc/passwd"))
    );
    assert!(
        plan.arguments
            .last()
            .is_some_and(|script| script.contains("printf '%s' 'mez-bubblewrap-capability-v6'"))
    );
    let capability = parse_bubblewrap_capability_probe(
        "%1",
        "pane-env-sha256",
        0,
        &plan,
        0,
        "mez-bubblewrap-capability-v6",
    )
    .unwrap();
    assert_eq!(
        capability.cache_key.runtime_profile_version,
        BUBBLEWRAP_RUNTIME_PROFILE_VERSION
    );
    assert_eq!(capability.cache_key.executable, "/usr/bin/bwrap");
    assert_eq!(capability.cache_key.bubblewrap_executable, "/usr/bin/bwrap");
    assert_eq!(capability.cache_key.pane_id, "%1");
    assert_eq!(capability.cache_key.config_generation, 0);
    assert_eq!(
        capability.cache_key.pane_environment_signature,
        "pane-env-sha256"
    );

    for contaminated_output in [
        "mez-bubblewrap-capability-v6\n",
        "mez-bubblewrap-capability-v6\r\n",
        "leading-mez-bubblewrap-capability-v6",
        "mez-bubblewrap-capability-v6trailing",
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

/// Capability probes use a fixed POSIX interpreter even when the pane's
/// workload shell is Fish, while retaining that workload shell in probe
/// identity so capabilities cannot alias across child interpreters.
#[test]
fn capability_probe_uses_posix_sh_for_fish_workload_shell() {
    let config = config();
    let fish_plan = bubblewrap_capability_probe_plan(&config, "/usr/bin/fish").unwrap();
    let posix_plan = bubblewrap_capability_probe_plan(&config, "/bin/sh").unwrap();

    assert_eq!(
        fish_plan.arguments[fish_plan.arguments.len() - 3],
        "/bin/sh"
    );
    assert_eq!(fish_plan.arguments[fish_plan.arguments.len() - 2], "-c");
    assert!(
        fish_plan
            .arguments
            .last()
            .is_some_and(|script| script.contains("mez-bubblewrap-capability-v6"))
    );
    assert_ne!(fish_plan.probe_sha256, posix_plan.probe_sha256);
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
            object_kind: ResolvedPathObjectKind::File,
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
            object_kind: ResolvedPathObjectKind::Directory,
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

/// Unknown effects retain maximum write authority while enforcing a
/// configured create target through its trusted nearest existing parent.
#[test]
fn maximum_authority_create_target_uses_nearest_existing_parent() {
    let config = config();
    let mut authority = authority();
    authority.write_scopes = vec!["/workspace/target/new/output.txt".to_string()];
    authority.path_evidence.insert(
        "target/new/output.txt".to_string(),
        ResolvedPathEvidence {
            canonical_path: "/workspace/target/new/output.txt".to_string(),
            kind: ResolvedPathKind::CreateTarget,
            nearest_existing_parent: "/workspace/target".to_string(),
            object_kind: ResolvedPathObjectKind::Directory,
        },
    );
    let mut unknown = effects();
    unknown.unknown = true;
    let evaluation = evaluation(EffectCompleteness::Unknown, unknown);

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
    assert_eq!(plan.audit_summary.network, SandboxNetworkMode::Connected);
    assert!(!plan.arguments.contains(&"--unshare-net".to_string()));

    let mut network = effects();
    network.network = true;
    let network = evaluation(EffectCompleteness::Complete, network);
    let plan = compile_bubblewrap_launch_plan(request(&config, &authority, &network)).unwrap();
    assert_eq!(plan.audit_summary.network, SandboxNetworkMode::Connected);
    assert!(!plan.arguments.contains(&"--unshare-net".to_string()));

    let mut denied_network = request(&config, &authority, &network);
    denied_network.network_policy = NetworkPolicy::Deny;
    let plan = compile_bubblewrap_launch_plan(denied_network).unwrap();
    assert_eq!(plan.audit_summary.network, SandboxNetworkMode::Isolated);
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
    let identity = resolve_sandbox_identity(
        &config.group_whitelist,
        &identity::current_process_environment_signature().unwrap(),
    )
    .unwrap();
    let managed_home = BubblewrapManagedHome {
        host_path: Path::new("/private/mez/cache-home").to_path_buf(),
        passwd_path: Path::new("/private/mez/passwd").to_path_buf(),
        group_path: Path::new("/private/mez/group").to_path_buf(),
        user_id: identity.user_id,
        group_id: identity.primary_group_id,
        supplementary_group_ids: Vec::new(),
        project_key: "0".repeat(64),
    };
    let mut compile_request = request(&config, &authority, &evaluation);
    compile_request.managed_home = Some(&managed_home);

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
    for arguments in [
        &["--ro-bind", "/private/mez/passwd", "/etc/passwd"][..],
        &["--ro-bind", "/private/mez/group", "/etc/group"][..],
    ] {
        assert!(
            plan.arguments
                .windows(arguments.len())
                .any(|window| window == arguments),
            "missing synthetic identity arguments {arguments:?}"
        );
    }
}

/// Verifies a pane username distinct from the former fixed alias determines
/// every launch-time home and account environment path without altering the
/// fixed sandbox compilation templates.
#[test]
fn sandbox_home_uses_the_resolved_pane_username() {
    let config = config();
    let authority = home_authority("/home/alice");
    let evaluation = evaluation(EffectCompleteness::Unknown, effects());
    let environment = mez_agent::EnvironmentSignature::new(
        "linux",
        "x86_64",
        None,
        "pane-host",
        "alice",
        Some("/home/alice".to_string()),
        "/bin/sh",
        mez_agent::ShellClassification::PosixSh,
        None,
        None,
        "/home/alice",
        None,
        false,
        None,
        Vec::new(),
    )
    .unwrap()
    .with_process_identity(
        1000,
        1000,
        vec![mez_agent::EnvironmentGroup {
            id: 1000,
            name: "alice".to_string(),
        }],
    )
    .unwrap();
    let identity = resolve_sandbox_identity(&config.group_whitelist, &environment).unwrap();
    let mut compile_request = request(&config, &authority, &evaluation);
    let probe = bubblewrap_capability_probe_plan_for_identity(
        &config,
        "/bin/sh",
        &identity,
        compile_request.environment_evidence,
    )
    .unwrap();
    compile_request.identity = identity;
    compile_request.capability = parse_bubblewrap_capability_probe(
        "%1",
        "pane-env-sha256",
        0,
        &probe,
        0,
        probe.expected_stdout,
    )
    .unwrap();
    compile_request.pane_home_directory = Some(Path::new("/home/alice"));

    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();

    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", "/home/alice", "/home/alice"])
    );
    for (name, value) in [
        ("HOME", "/home/alice"),
        ("XDG_CACHE_HOME", "/home/alice/.cache"),
        ("XDG_CONFIG_HOME", "/home/alice/.config"),
        ("XDG_DATA_HOME", "/home/alice/.local/share"),
        ("XDG_STATE_HOME", "/home/alice/.local/state"),
        ("USER", "alice"),
        ("LOGNAME", "alice"),
    ] {
        assert!(
            plan.arguments
                .windows(3)
                .any(|args| args == ["--setenv", name, value]),
            "missing {name}={value}"
        );
    }
    assert_eq!(plan.sandbox_working_directory, "/home/alice");
}

/// Authorized pane-home paths are rehomed below the synthetic user home while
/// paths outside that home retain their canonical sandbox destinations.
#[test]
fn pane_home_authority_is_projected_below_synthetic_home() {
    let config = config();
    let authority = home_authority("/home/alice");
    let mut unknown = effects();
    unknown.unknown = true;
    let evaluation = evaluation(EffectCompleteness::Unknown, unknown);
    let mut compile_request = request(&config, &authority, &evaluation);
    compile_request.pane_home_directory = Some(Path::new("/home/alice"));

    let plan = compile_bubblewrap_launch_plan(compile_request).unwrap();

    assert!(
        plan.arguments
            .windows(3)
            .any(|args| args == ["--ro-bind", "/home/alice", "/home/mez"]),
    );
    assert_eq!(plan.sandbox_working_directory, "/home/mez");
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

/// Verified default PATH evidence reaches the Bubblewrap command-search path,
/// while values outside the configured whitelist remain absent from its
/// environment.
#[test]
fn default_pane_environment_forwards_whitelisted_path() {
    let config = config();
    let environment_request =
        mez_agent::shell::PaneEnvironmentRequest::new(config.env_whitelist.requested_names.clone())
            .unwrap();
    let environment_evidence = mez_agent::shell::PaneEnvironmentEvidence::from_parts(
        &environment_request,
        BTreeMap::from([("PATH".to_string(), "/opt/tools:/usr/bin".to_string())]),
        BTreeMap::new(),
    )
    .unwrap();
    let identity = resolve_sandbox_identity(
        &config.group_whitelist,
        &identity::current_process_environment_signature().unwrap(),
    )
    .unwrap();
    let probe = bubblewrap_capability_probe_plan_for_identity(
        &config,
        "/bin/sh",
        &identity,
        &environment_evidence,
    )
    .unwrap();
    let capability = parse_bubblewrap_capability_probe(
        "%1",
        "pane-env-sha256",
        0,
        &probe,
        0,
        probe.expected_stdout,
    )
    .unwrap();
    let authority = authority();
    let evaluation = evaluation(EffectCompleteness::Complete, effects());
    let mut request = request(&config, &authority, &evaluation);
    request.identity = identity;
    request.capability = capability;
    request.environment_evidence = &environment_evidence;

    let plan = compile_bubblewrap_launch_plan(request).unwrap();
    assert!(
        plan.arguments
            .windows(3)
            .any(|arguments| arguments == ["--setenv", "PATH", "/opt/tools:/usr/bin"])
    );
    assert!(
        !plan
            .arguments
            .iter()
            .any(|argument| argument == "UNSET_VALUE")
    );
    assert!(!plan.arguments.iter().any(|argument| argument == "pane-ci"));
}
