use super::{
    InstalledPlugin, PluginManifest, PluginRegistry, install_local_plugin, load_enabled_plugins,
    plugin_command_from_args, uninstall_plugin,
};
use std::fs;
use std::path::{Path, PathBuf};

/// Creates a unique temporary plugin test root.
fn test_temp_root(label: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "mez-plugin-{label}-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

/// Writes a minimal plugin package with one skill payload.
fn write_skill_plugin(root: &Path, id: &str) {
    fs::create_dir_all(root.join("skills/demo")).unwrap();
    fs::write(
        root.join("mez-plugin.toml"),
        format!(
            "schema_version = 1\nid = \"{id}\"\nname = \"Demo Plugin\"\ndescription = \"Adds a demo skill.\"\nversion = \"0.1.0\"\n\n[payloads]\nskills = \"skills\"\n"
        ),
    )
    .unwrap();
    fs::write(
        root.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: Demo skill\n---\n\nUse demo.\n",
    )
    .unwrap();
}

/// Verifies manifest validation rejects payload paths that escape the plugin root.
#[test]
fn plugin_manifest_rejects_path_traversal_payloads() {
    let error = PluginManifest::parse(
        "schema_version = 1\nid = \"bad\"\nname = \"Bad\"\ndescription = \"Bad plugin\"\nversion = \"0\"\n\n[payloads]\nskills = \"../skills\"\n",
    )
    .unwrap_err();

    assert!(error.message().contains("must not escape"), "{error:?}");
}

/// Verifies local CLI-owned install and enablement produce runtime plugin skill roots.
#[test]
fn plugin_install_and_load_exposes_enabled_skill_root() {
    let root = test_temp_root("install-load");
    let config_root = root.join("config");
    let package = root.join("package");
    write_skill_plugin(&package, "demo-plugin");

    let install = install_local_plugin(&config_root, &package, true).unwrap();
    assert!(
        install.contains("installed plugin demo-plugin"),
        "{install}"
    );

    let outcome = load_enabled_plugins(&config_root);
    assert_eq!(outcome.skill_roots.len(), 1);
    assert_eq!(outcome.skill_roots[0].plugin_id, "demo-plugin");
    assert!(outcome.skill_roots[0].path.ends_with("skills"));
}

/// Verifies uninstall refuses a tampered registry path instead of deleting an
/// arbitrary directory outside the local plugin store.
#[test]
fn plugin_uninstall_refuses_registry_path_outside_installed_root() {
    let root = test_temp_root("uninstall-path-boundary");
    let config_root = root.join("config");
    let outside = root.join("outside-data");
    fs::create_dir_all(&outside).unwrap();

    let mut registry = PluginRegistry::default();
    registry.plugins.insert(
        "demo-plugin".to_string(),
        InstalledPlugin {
            id: "demo-plugin".to_string(),
            name: "Demo Plugin".to_string(),
            description: "Adds a demo skill.".to_string(),
            version: "0.1.0".to_string(),
            path: outside.clone(),
            enabled: false,
        },
    );
    registry.write(&config_root).unwrap();

    let error = uninstall_plugin(&config_root, "demo-plugin").unwrap_err();

    assert!(outside.exists());
    assert!(
        error.message().contains("does not match expected"),
        "{error:?}"
    );
}

/// Verifies install rejects configurations where the plugin store would be
/// copied into the package being installed.
#[test]
fn plugin_install_refuses_destination_inside_source_package() {
    let root = test_temp_root("install-self-copy");
    let package = root.join("package");
    let config_root = package.join(".mez");
    write_skill_plugin(&package, "demo-plugin");

    let error = install_local_plugin(&config_root, &package, false).unwrap_err();

    assert!(
        error.message().contains("must not contain each other"),
        "{error:?}"
    );
}

/// Verifies reserved plugin payload categories all surface diagnostics while
/// remaining inactive.
#[test]
fn plugin_loader_reports_all_reserved_payload_categories() {
    let root = test_temp_root("reserved-diagnostics");
    let config_root = root.join("config");
    let package = root.join("package");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("mez-plugin.toml"),
        "schema_version = 1\nid = \"demo-plugin\"\nname = \"Demo Plugin\"\ndescription = \"Adds declarations.\"\nversion = \"0.1.0\"\n\n[payloads]\nmcp_servers = \"mcp.toml\"\nhooks = \"hooks.toml\"\nsubagents = \"subagents.toml\"\npersonalities = \"personalities.toml\"\n",
    )
    .unwrap();

    install_local_plugin(&config_root, &package, true).unwrap();
    let outcome = load_enabled_plugins(&config_root);

    assert!(outcome.skill_roots.is_empty());
    assert!(
        outcome
            .diagnostics
            .iter()
            .any(|message| message.contains("MCP servers")),
        "{:?}",
        outcome.diagnostics
    );
    assert!(
        outcome
            .diagnostics
            .iter()
            .any(|message| message.contains("hooks")),
        "{:?}",
        outcome.diagnostics
    );
    assert!(
        outcome
            .diagnostics
            .iter()
            .any(|message| message.contains("subagents")),
        "{:?}",
        outcome.diagnostics
    );
    assert!(
        outcome
            .diagnostics
            .iter()
            .any(|message| message.contains("personalities")),
        "{:?}",
        outcome.diagnostics
    );
}

/// Verifies `/plugin` parser rejects accidental trailing arguments rather than
/// silently executing a different read-only command.
#[test]
fn plugin_slash_parser_rejects_extra_arguments() {
    let list_error = plugin_command_from_args("list demo").unwrap_err();
    let inspect_error = plugin_command_from_args("inspect demo extra").unwrap_err();

    assert!(list_error.message().contains("extra arguments"));
    assert!(inspect_error.message().contains("exactly one plugin id"));
}
