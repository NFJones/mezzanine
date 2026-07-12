//! Runtime tests for config hooks audit behavior.

use super::*;

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

    let matching = crate::hooks::plan_event(
        &service.hook_definitions,
        HookEvent::UserPromptSubmit,
        r#"{"pane_id":"pane-2"}"#,
    )
    .unwrap();
    let fallback = crate::hooks::plan_event(
        &service.hook_definitions,
        HookEvent::UserPromptSubmit,
        r#"{"agent_id":"agent-1"}"#,
    )
    .unwrap();
    let filtered = crate::hooks::plan_event(
        &service.hook_definitions,
        HookEvent::UserPromptSubmit,
        r#"{"pane_id":"other","agent_id":"agent-2"}"#,
    )
    .unwrap();

    assert_eq!(report.hooks_configured, 1);
    assert_eq!(service.hook_definitions[0].matcher_groups.len(), 2);
    assert_eq!(matching.plans.len(), 1);
    assert_eq!(fallback.plans.len(), 1);
    assert!(filtered.plans.is_empty());
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
        r#"{"jsonrpc":"2.0","id":"audit-approval","method":"agent/shell/command","params":{"idempotency_key":"audit-approval","input":"/approval full-access"}}"#,
        &primary,
    );

    assert!(output.contains("changed=true"), "{output}");
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"permission""#), "{audit}");
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
