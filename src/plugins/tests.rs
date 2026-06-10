use super::{PluginManifest, install_local_plugin, load_enabled_plugins};
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
