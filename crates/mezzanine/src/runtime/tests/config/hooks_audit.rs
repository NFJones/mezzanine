//! Runtime tests for config hooks audit behavior.

use super::*;
use crate::runtime::run_external_shell_hook_command;

/// Verifies the external-shell compatibility runner drains large stdout and
/// stderr streams concurrently, retains bounded prefixes, and reports the
/// complete observed byte counts instead of blocking on full child pipes.
#[test]
fn external_shell_hook_drains_and_bounds_both_output_streams() {
    let hook = crate::integrations::hooks::HookDefinition {
        id: "external-large-output".to_string(),
        event: HookEvent::SessionDetach,
        invocation: crate::integrations::hooks::HookInvocation::FocusedShell {
            command: "i=0; while [ $i -lt 20000 ]; do printf '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'; printf 'fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210' >&2; i=$((i + 1)); done".to_string(),
        },
        enabled: true,
        required: false,
        agent_hook: true,
        matcher_groups: Vec::new(),
        timeout_ms: Some(5_000),
        on_failure: None,
    };
    let plan = crate::integrations::hooks::plan_hook(&hook)
        .unwrap()
        .unwrap();

    let output = run_external_shell_hook_command(Path::new("/bin/sh"), &plan).unwrap();

    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout.len(), 1024 * 1024);
    assert_eq!(output.stderr.len(), 1024 * 1024);
    assert_eq!(output.stdout_bytes, 1_280_000);
    assert_eq!(output.stderr_bytes, 1_280_000);
    assert!(output.stdout_truncated);
    assert!(output.stderr_truncated);
}

/// Verifies runtime applies configured lifecycle hooks.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_applies_configured_lifecycle_hooks() {
    let root = temp_root("configured-hooks");
    let payload_path = root.join("attach-payload.json");
    let mut service = test_runtime_service();
    let report = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[hooks.attach]\nevent = \"client_attach\"\nprogram = \"/bin/sh\"\nargs = [\"-c\", \"cat > \\\"$1\\\"\", \"hook\", \"{}\"]\n\n[hooks.focused]\nevent = \"client_attach\"\ncommand = \"printf hook-from-config\"\nagent_hook = true\non_failure = \"warn\"\n",
                payload_path.display()
            ),
        }])
        .unwrap();

    assert_eq!(report.hooks_configured, 2);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();

    let payload = fs::read_to_string(&payload_path).unwrap();
    assert!(payload.contains(r#""client_id":"#), "{payload}");
    assert!(payload.contains(primary.as_str()), "{payload}");
    assert_eq!(service.focused_shell_hook_queue_len(), 1);
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime config parses hook matcher groups.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_config_parses_hook_matcher_groups() {
    let mut service = test_runtime_service();
    let report = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[hooks.prompt]\nevent = \"user_prompt_submit\"\nprogram = \"/bin/echo\"\n[hooks.prompt.match.pane_id]\nprefix = \"pane-\"\n[[hooks.prompt.matches]]\npath = \"agent_id\"\nequals = \"agent-1\"\n".to_string(),
        }])
        .unwrap();

    let matching = crate::integrations::hooks::plan_event(
        service.integration.hook_definitions(),
        HookEvent::UserPromptSubmit,
        r#"{"pane_id":"pane-2"}"#,
    )
    .unwrap();
    let fallback = crate::integrations::hooks::plan_event(
        service.integration.hook_definitions(),
        HookEvent::UserPromptSubmit,
        r#"{"agent_id":"agent-1"}"#,
    )
    .unwrap();
    let filtered = crate::integrations::hooks::plan_event(
        service.integration.hook_definitions(),
        HookEvent::UserPromptSubmit,
        r#"{"pane_id":"other","agent_id":"agent-2"}"#,
    )
    .unwrap();

    assert_eq!(report.hooks_configured, 1);
    assert_eq!(
        service.integration.hook_definitions()[0]
            .matcher_groups
            .len(),
        2
    );
    assert_eq!(matching.plans.len(), 1);
    assert_eq!(fallback.plans.len(), 1);
    assert!(filtered.plans.is_empty());
}

/// Verifies an adapter-owned blocking pre-shell program hook is queued for
/// async execution and its successful completion authorizes the guarded phase.
///
/// The serialized runtime must return a pending decision without spawning the
/// child itself. Applying the typed worker completion clears pending ownership
/// and prevents the same hook from being dispatched a second time.
#[test]
fn runtime_async_program_hook_completion_resumes_pre_shell_pipeline() {
    let mut service = test_runtime_service();
    service.use_hook_effect_adapter();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[hooks.guard]\nevent = \"pre_shell_command\"\nprogram = \"/bin/sh\"\nargs = [\"-c\", \"true\"]\non_failure = \"block\"\n"
                .to_string(),
        }])
        .unwrap();
    let continuation = crate::runtime::PendingFocusedShellHookContinuation {
        turn_id: "turn-hook".to_string(),
        action_id: "action-hook".to_string(),
        phase_command_sha256: "phase-digest".to_string(),
    };

    let decision = service
        .run_configured_pre_action_hooks_with_continuation(
            HookEvent::PreShellCommand,
            r#"{"command":"printf guarded"}"#,
            Some(continuation.clone()),
        )
        .unwrap();

    assert_eq!(
        decision,
        crate::runtime::RuntimeHookPipelineDecision::Pending
    );
    assert_eq!(
        service
            .integration
            .pending_program_hook_continuations()
            .len(),
        1
    );
    let mut effects = service.drain_program_hook_transition().side_effects;
    assert_eq!(effects.len(), 1);
    let RuntimeSideEffect::RunProgramHook {
        plan,
        triggering_event_completed,
        continuation: pending,
    } = effects.pop().unwrap()
    else {
        panic!("blocking program hook should produce a hook-worker side effect");
    };
    assert!(!triggering_event_completed);
    let pending = pending.expect("blocking program hook should retain its continuation");
    let result = crate::integrations::hooks::HookExecutionResult {
        hook_id: plan.hook_id.clone(),
        event: plan.event,
        status: crate::integrations::hooks::HookExecutionStatus::Succeeded,
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
        stdout_bytes: 0,
        stderr_bytes: 0,
        stdout_truncated: false,
        stderr_truncated: false,
        failure: None,
    };

    service
        .apply_hook_transition(crate::runtime::AsyncHookEvent::ProgramCompleted {
            plan,
            result: Box::new(result),
            triggering_event_completed: false,
            continuation: Some(pending),
        })
        .unwrap();

    assert!(
        service
            .integration
            .pending_program_hook_continuations()
            .is_empty()
    );
    assert_eq!(
        service
            .run_configured_pre_action_hooks_with_continuation(
                HookEvent::PreShellCommand,
                r#"{"command":"printf guarded"}"#,
                Some(continuation),
            )
            .unwrap(),
        crate::runtime::RuntimeHookPipelineDecision::Continue
    );
    assert!(
        service
            .drain_program_hook_transition()
            .side_effects
            .is_empty()
    );
}

/// Verifies an adapter-owned blocking program hook never falls back to the
/// synchronous compatibility executor when its event lacks a continuation.
///
/// Unsupported continuation ownership must fail closed immediately rather
/// than spawn or wait for a subprocess while the serialized actor is held.
#[test]
fn runtime_adapter_rejects_blocking_program_hook_without_continuation() {
    let mut service = test_runtime_service();
    service.use_hook_effect_adapter();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[hooks.guard]\nevent = \"layout_load\"\nprogram = \"/bin/sh\"\nargs = [\"-c\", \"sleep 30\"]\non_failure = \"block\"\n"
                .to_string(),
        }])
        .unwrap();

    let started = std::time::Instant::now();
    let error = service
        .run_configured_pre_action_hooks(HookEvent::LayoutLoad, r#"{"session_id":"test"}"#)
        .unwrap_err();

    assert!(started.elapsed() < std::time::Duration::from_millis(100));
    assert!(
        error
            .message()
            .contains("has no async continuation for event layout_load"),
        "{error}"
    );
    assert!(
        service
            .drain_program_hook_transition()
            .side_effects
            .is_empty()
    );
}

/// Verifies that runtime configuration can initialize the audit writer from
/// `[audit]` settings. The path is resolved under the configured Mezzanine
/// config root when relative, and subsequent auditable runtime actions write
/// JSONL records through the configured hash-chain and retention modes.
#[test]
fn runtime_applies_audit_log_from_config_layers() {
    let mut service = test_runtime_service();
    let root = temp_root("runtime-audit-config");
    let config_root = root.join("config");
    service.set_config_root(config_root.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[audit]\nenabled = true\npath = \"security/audit.jsonl\"\nformat = \"jsonl\"\nretention_days = 1\nhash_chain = true\nrequired = true\n".to_string(),
        }])
        .unwrap();
    let audit_path = config_root.join("security/audit.jsonl");
    assert_eq!(service.audit_log().unwrap().path(), audit_path.as_path());
    fs::create_dir_all(audit_path.parent().unwrap()).unwrap();
    fs::write(
        &audit_path,
        "{\"timestamp\":\"unix:1\",\"action\":\"old\"}\n",
    )
    .unwrap();

    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let output = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"audit-approval","method":"agent/shell/command","params":{"idempotency_key":"audit-approval","input":"/approval host-access"}}"#,
        &primary,
    );

    assert!(output.contains("changed=true"), "{output}");
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"permission""#), "{audit}");
    assert!(audit.contains(r#""decision":"host-access""#), "{audit}");
    assert!(
        audit.contains(r#""authority_source":"primary-user""#),
        "{audit}"
    );
    assert!(audit.contains(r#""hash":"#), "{audit}");
    assert!(!audit.contains(r#""action":"old""#), "{audit}");
    let _ = fs::remove_dir_all(root);
}

/// Verifies that an adapter-owned runtime keeps audit persistence deferred
/// when a live configuration reload installs a replacement audit writer. The
/// ownership decision belongs to the actor boundary rather than the global
/// external-effect compatibility mode.
#[test]
fn runtime_preserves_audit_adapter_ownership_across_config_reload() {
    let mut service = test_runtime_service();
    let root = temp_root("runtime-audit-adapter-reload");
    let audit_path = root.join("audit.jsonl");
    service.set_config_root(root.clone());
    service.use_audit_effect_adapter();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[audit]\nenabled = true\npath = \"audit.jsonl\"\nrequired = true\n".to_string(),
        }])
        .unwrap();

    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let output = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"audit-adapter-reload","method":"agent/shell/command","params":{"idempotency_key":"audit-adapter-reload","input":"/approval full-access"}}"#,
        &primary,
    );

    assert!(output.contains("changed=true"), "{output}");
    assert!(!audit_path.exists());
    let transition = service.drain_audit_persistence_transition();
    assert_eq!(transition.side_effects.len(), 1);
    assert!(matches!(
        &transition.side_effects[0],
        RuntimeSideEffect::PersistAuditLog { path, .. } if path == &audit_path
    ));
    let _ = fs::remove_dir_all(root);
}

/// Verifies that invalid audit retention configuration fails before replacing
/// the runtime audit writer. A zero-day retention window would immediately
/// discard useful audit history, so the config layer is rejected instead of
/// silently enabling destructive pruning.
#[test]
fn runtime_rejects_invalid_audit_retention_days() {
    let mut service = test_runtime_service();
    let error = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[audit]\nenabled = true\nretention_days = 0\n".to_string(),
        }])
        .unwrap_err();

    assert!(error.message().contains("audit.retention_days"), "{error}");
    assert!(service.audit_log().is_none());
}
