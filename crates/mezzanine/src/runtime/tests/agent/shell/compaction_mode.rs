//! Public command authority, pane lifetime and inheritance regressions.

use super::*;

/// Text equality after a different primary writer's A/B/A sequence cannot
/// acknowledge a new queued revision with the original A's durable receipt.
#[test]
fn runtime_compaction_mode_untracked_primary_aba_cannot_reuse_durable_receipt() {
    let (mut service, primary) = fixture();
    let root = temp_root("mode-untracked-aba");
    let path = root.join("config.toml");
    let text = "[agents]\ncompaction_mode = \"summary\"\n";
    fs::write(&path, text).unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".into(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: text.into(),
        }])
        .unwrap();
    let first = service
        .execute_agent_shell_command(&primary, "/compaction-mode raw_tail --global")
        .unwrap();
    assert!(first.contains("Persistence | durable"), "{first}");
    for (index, mode) in ["summary", "raw_tail"].into_iter().enumerate() {
        if index == 1 {
            service.use_config_effect_adapter();
        }
        let request = serde_json::json!({"jsonrpc":"2.0","id":index,"method":"config/set","params":{
            "path":"agents.compaction_mode", "value":mode, "persist":{"scope":"user","path":path}, "idempotency_key":format!("mode-aba-{index}")}}).to_string();
        let response = service.dispatch_runtime_control_body(&request, &primary);
        assert!(!response.contains("\"error\""), "{response}");
    }
    assert_eq!(service.agent_compaction_mode().name(), "raw_tail");
    assert!(fs::read_to_string(&path).unwrap().contains("summary"));
    let status = service
        .execute_agent_shell_command(&primary, "/compaction-mode")
        .unwrap();
    assert!(
        !status.contains("Persistence | durable"),
        "old A acknowledged queued C: {status}"
    );
    assert_eq!(
        service
            .drain_config_persistence_transition()
            .side_effects
            .len(),
        1
    );
    fs::remove_dir_all(root).unwrap();
}

/// A global reset removes only the primary key. Existing higher-precedence
/// layers and explicit overrides, including equal-default choices, still win.
#[test]
fn runtime_compaction_mode_global_preserves_overrides_and_layer_precedence() {
    let (mut service, primary) = fixture();
    let root = temp_root("mode-layer-precedence");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    let text = "# unrelated author comment\n[agents]\ncompaction_mode=\"summary\"\n[history]\nlines=4321\n";
    fs::write(&path, text).unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".into(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: text.into(),
        }])
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "/compaction-mode summary")
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%sibling")
        .unwrap();
    service.set_agent_compaction_mode_override(
        "%sibling",
        Some(crate::runtime::config::CompactionMode::RawTail),
    );
    let change = service
        .execute_agent_shell_command(&primary, "/compaction-mode raw_tail --global")
        .unwrap();
    assert!(change.contains("changed=true"), "{change}");
    assert!(change.contains("override_masks_global=true"), "{change}");
    assert!(change.contains("Persistence | durable"), "{change}");
    assert_eq!(
        service.effective_agent_compaction_mode("%1").name(),
        "summary"
    );
    assert_eq!(
        service.effective_agent_compaction_mode("%sibling").name(),
        "raw_tail"
    );
    assert_eq!(
        service.effective_agent_compaction_mode("%new").name(),
        "raw_tail"
    );
    let mut layers = service.config_layers().to_vec();
    layers.push(ConfigLayer {
        name: "live".into(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::LiveOverride,
        trusted: true,
        text: "[agents]\ncompaction_mode=\"raw_tail\"\n".into(),
    });
    service.replace_config_layers(layers).unwrap();
    let reset = service
        .execute_agent_shell_command(&primary, "/compaction-mode reset --global")
        .unwrap();
    assert!(reset.contains("Primary-config value | unset"), "{reset}");
    assert!(reset.contains("Configured default | `raw_tail`"), "{reset}");
    assert!(reset.contains("Effective mode | `summary`"), "{reset}");
    let noop = service
        .execute_agent_shell_command(&primary, "/compaction-mode reset --global")
        .unwrap();
    assert!(noop.contains("changed=false"), "{noop}");
    service
        .execute_agent_shell_command(&primary, "/compaction-mode reset")
        .unwrap();
    assert_eq!(
        service.effective_agent_compaction_mode("%1").name(),
        "raw_tail"
    );
    assert_eq!(
        service
            .agent_compaction_mode_override("%sibling")
            .unwrap()
            .name(),
        "raw_tail"
    );
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(persisted.contains("# unrelated author comment"));
    assert!(persisted.contains("4321"));
    assert!(!persisted.contains("compaction_mode"));
    fs::remove_dir_all(root).unwrap();
}

fn fixture() -> (RuntimeSessionService, mez_core::ids::ClientId) {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("mode-primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    (service, primary)
}

#[test]
fn runtime_compaction_mode_pane_lifetime_and_default_inheritance() {
    let (mut service, primary) = fixture();
    service
        .execute_agent_shell_command(&primary, "/compaction-mode summary")
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%sibling")
        .unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "mode".into(),
            path: None,
            scope: ConfigScope::Primary,
            format: ConfigFormat::Toml,
            trusted: true,
            text: "[agents]\ncompaction_mode=\"raw_tail\"\n".into(),
        }])
        .unwrap();
    assert_eq!(
        service.effective_agent_compaction_mode("%1").name(),
        "summary"
    );
    assert_eq!(
        service.effective_agent_compaction_mode("%sibling").name(),
        "raw_tail"
    );
    for command in ["/new", "/clear", "/exit"] {
        service
            .execute_agent_shell_command(&primary, command)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        assert_eq!(
            service.agent_compaction_mode_override("%1").unwrap().name(),
            "summary"
        );
    }
    // Binding a saved conversation is not selection authority.
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "resumed-mode-conversation", 0)
        .unwrap();
    assert_eq!(
        service.effective_agent_compaction_mode("%1").name(),
        "summary"
    );
    service.cleanup_removed_pane_runtime_state("%1").unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    assert!(service.agent_compaction_mode_override("%1").is_none());
    assert_eq!(
        service.effective_agent_compaction_mode("%1").name(),
        "raw_tail"
    );
    assert!(
        test_runtime_service()
            .agent_compaction_mode_override("%1")
            .is_none()
    );
}

#[test]
fn runtime_compaction_mode_control_and_status_actions_cannot_mutate() {
    let (mut service, primary) = fixture();
    let generation = service.session.config_generation;
    for input in [
        "/compaction-mode raw_tail",
        "/compaction-mode reset",
        "/compaction-mode raw_tail --global",
    ] {
        let body = serde_json::json!({"jsonrpc":"2.0","id":input,"method":"agent/shell/command","params":{"idempotency_key":input,"input":input}}).to_string();
        let response = service.dispatch_runtime_control_body(&body, &primary);
        assert!(
            response.contains("authenticated primary-client input"),
            "{response}"
        );
        assert!(
            service
                .execute_agent_shell_command_for_pane(&primary, "%1", input)
                .is_err()
        );
        assert!(service.agent_compaction_mode_override("%1").is_none());
        assert_eq!(service.session.config_generation, generation);
    }
    let read = service.dispatch_runtime_control_body(r#"{"jsonrpc":"2.0","id":"read","method":"agent/shell/command","params":{"idempotency_key":"read","input":"/compaction-mode status"}}"#, &primary);
    assert!(read.contains("# Compaction Mode Status"), "{read}");
}

#[test]
fn runtime_compaction_mode_busy_guards_are_scoped_and_status_is_narrow() {
    let (mut service, primary) = fixture();
    service.mark_agent_compacting_for_tests("%sibling", 1);
    let local = service
        .execute_agent_shell_command(&primary, "/compaction-mode raw_tail")
        .unwrap();
    assert!(local.contains("changed=true"), "{local}");
    let global = service
        .execute_agent_shell_command(&primary, "/compaction-mode summary --global")
        .unwrap();
    assert!(global.contains("compaction is active"), "{global}");
    service.mark_agent_compacting_for_tests("%1", 1);
    for input in ["/compaction-mode", "/compaction-mode status"] {
        let read = service
            .execute_agent_shell_command(&primary, input)
            .unwrap();
        assert!(read.contains("# Compaction Mode Status"), "{read}");
    }
    for input in [
        "/compaction-mode reset",
        "/compaction-mode summary",
        "/compaction-mode status --global",
        "/compaction-mode status extra",
    ] {
        let reply = service
            .execute_agent_shell_command(&primary, input)
            .unwrap();
        assert!(reply.contains("cannot mutate pane state"), "{reply}");
    }
    assert_eq!(
        service.effective_agent_compaction_mode("%1").name(),
        "raw_tail"
    );
}

#[test]
fn runtime_compaction_mode_invalid_grammar_and_missing_target_are_local() {
    let (mut service, primary) = fixture();
    let before = service.config_layers().to_vec();
    let generation = service.session.config_generation;
    for args in [
        "toggle",
        "raw-tail",
        "status --global",
        "summary --global --global",
        "summary --yes",
        "'raw_tail",
        "--global reset",
    ] {
        let reply = service
            .execute_agent_shell_command(&primary, &format!("/compaction-mode {args}"))
            .unwrap();
        assert!(reply.contains("Compaction mode error"), "{reply}");
        assert_eq!(service.config_layers(), before);
        assert!(service.agent_compaction_mode_override("%1").is_none());
    }
    let missing = service
        .execute_agent_shell_command(&primary, "/compaction-mode raw_tail --global")
        .unwrap();
    assert!(missing.contains("configuration target"), "{missing}");
    assert_eq!(service.session.config_generation, generation);
    let read = service
        .execute_agent_shell_command(&primary, "/status")
        .unwrap();
    assert!(read.contains("Compaction mode"), "{read}");
}
