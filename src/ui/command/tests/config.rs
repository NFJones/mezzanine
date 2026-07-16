//! Command config tests.

use super::*;

/// Verifies config commands report live config requirements without store.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_commands_report_live_config_requirements_without_store() {
    let (mut session, primary) = test_session();

    let set = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("set-option history.lines 2048").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        set,
        "path=history.lines:value=2048:changed=false:reason=live-config-control-unavailable"
    );

    let theme = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("set-theme nord").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        theme,
        "theme=nord:changed=false:reason=live-config-control-unavailable"
    );

    let bind = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("bind-key C-a split-window -h").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        bind,
        "key=C-a:command=split-window -h:changed=false:reason=live-config-control-unavailable"
    );

    let unbind = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("unbind-key C-a").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        unbind,
        "key=C-a:removed=false:reason=live-config-control-unavailable"
    );
}

/// Verifies config store commands mutate primary config and apply source files.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_store_commands_mutate_primary_config_and_apply_source_file() {
    let root =
        std::env::temp_dir().join(format!("mez-command-config-store-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let paths = ConfigPaths::from_root(root.clone());
    let config_path = paths.ensure_default_config().unwrap();

    let set = display_body(
        execute_config_store_command(
            &paths,
            &parse_command_sequence("set-option history.lines 2048")
                .unwrap()
                .remove(0),
        )
        .unwrap(),
    );
    assert_eq!(
        set,
        "path=history.lines:changed=true:reload_required=true:source=config-store"
    );
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("lines = 2048"));

    let set_theme = display_body(
        execute_config_store_command(
            &paths,
            &parse_command_sequence("set-theme nord").unwrap().remove(0),
        )
        .unwrap(),
    );
    assert!(set_theme.contains("theme=nord"), "{set_theme}");
    assert!(set_theme.contains("changed=true"), "{set_theme}");
    assert!(set_theme.contains("reload_required=true"), "{set_theme}");
    assert!(set_theme.contains("source=config-store"), "{set_theme}");
    assert!(set_theme.contains("aliases=17"), "{set_theme}");
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("active = \"nord\""));
    assert!(text.contains("primary = \"#88c0d0\""));
    assert!(text.contains("window_active_bg = \"primary\""));
    assert!(!text.contains("#7e9cd8"));

    let bind = display_body(
        execute_config_store_command(
            &paths,
            &parse_command_sequence("bind-key C-a split-window -h")
                .unwrap()
                .remove(0),
        )
        .unwrap(),
    );
    assert!(bind.contains("key=C-a:config_key=key_43_2d_61"));
    assert!(bind.contains("command=split-window -h"));
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("[keys.command_bindings]"));
    assert!(text.contains("key_43_2d_61 = \"split-window -h\""));

    let unbind = display_body(
        execute_config_store_command(
            &paths,
            &parse_command_sequence("unbind-key C-a").unwrap().remove(0),
        )
        .unwrap(),
    );
    assert!(unbind.contains("removed=true"));
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(!text.contains("key_43_2d_61 = \"split-window -h\""));

    let mcp = display_body(
        execute_config_store_command(
            &paths,
            &parse_command_sequence("mcp add fs --command mcp-fs --arg --root --arg . --disabled")
                .unwrap()
                .remove(0),
        )
        .unwrap(),
    );
    assert!(mcp.contains("server=fs:action=add"), "{mcp}");
    assert!(mcp.contains("changed=true"), "{mcp}");
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("[mcp_servers.fs]"));
    assert!(text.contains("enabled = false"));
    assert!(text.contains("command = \"mcp-fs\""));
    assert!(text.contains("args"));

    let mcp_tools = display_body(
        execute_config_store_command(
            &paths,
            &parse_command_sequence("mcp tools disable fs write_file")
                .unwrap()
                .remove(0),
        )
        .unwrap(),
    );
    assert!(mcp_tools.contains("action=tools-disable"), "{mcp_tools}");
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("disabled_tools"));
    assert!(text.contains("write_file"));

    let source_path = root.join("sourced-config.toml");
    let source_text = fs::read_to_string(&config_path)
        .unwrap()
        .replace("lines = 2048", "lines = 4096");
    fs::write(&source_path, source_text).unwrap();

    let source = display_body(
        execute_config_store_command(
            &paths,
            &parse_command_sequence(&format!("source-file {}", source_path.display()))
                .unwrap()
                .remove(0),
        )
        .unwrap(),
    );
    assert!(source.contains("valid=true"));
    assert!(source.contains("diagnostics=0"));
    assert!(source.contains("applied=true"));
    assert!(source.contains("changed=true"));
    assert!(source.contains("reload_required=true"));
    assert!(source.contains(&format!("target={}", config_path.display())));
    assert!(source.contains("source=config-store"));
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("lines = 4096"));

    let _ = fs::remove_dir_all(root);
}
