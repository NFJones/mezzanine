//! Config parse tests.

use super::*;

/// Verifies validates default toml config.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn validates_default_toml_config() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        DEFAULT_CONFIG_TOML,
        ConfigScope::Primary,
    );

    assert!(validation.valid, "{:?}", validation.diagnostics);
}

/// Verifies that user-facing theme configuration accepts alias-based color
/// assignments while still rejecting malformed hex values and unknown UI slots.
/// Theme values are applied at runtime, but static config validation needs to
/// catch spelling mistakes before a user reloads a broken interactive theme.
#[test]
fn validates_theme_aliases_and_color_slots() {
    let valid = validate_config_text(
        ConfigFormat::Toml,
        r##"
[theme]
active = "gruvbox_dark"

[theme.aliases]
primary = "#123456"

[theme.colors]
window_active_bg = "primary"
prompt_fg = "#abc"
syntax_keyword_fg = "primary"

[themes.deepforest_alt.aliases]
tertiary = "#fed"

[themes.deepforest_alt.colors]
pane_divider_fg = "tertiary"
syntax_string_fg = "tertiary"
"##,
        ConfigScope::Primary,
    );

    assert!(valid.valid, "{:?}", valid.diagnostics);

    let invalid = validate_config_text(
        ConfigFormat::Toml,
        r##"
[theme.aliases]
primary = "green"

[theme.colors]
not_a_slot = "primary"
prompt_fg = "$bad"
"##,
        ConfigScope::Primary,
    );

    assert!(!invalid.valid);
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "theme.aliases.primary" && diagnostic.message.contains("hex colors")
    }));
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "theme.colors.not_a_slot"
            && diagnostic.message.contains("unknown theme color slot")
    }));
    assert!(invalid.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "theme.colors.prompt_fg"
            && diagnostic.message.contains("hex colors or alias names")
    }));
}

/// Verifies validate config text rejects malformed supported formats.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn validate_config_text_rejects_malformed_supported_formats() {
    let json = validate_config_text(ConfigFormat::Json, r#"{ "session": "#, ConfigScope::Primary);
    let toml = validate_config_text(ConfigFormat::Toml, "[session", ConfigScope::Primary);
    let yaml = validate_config_text(
        ConfigFormat::Yaml,
        "session:\n  - [unterminated\n",
        ConfigScope::Primary,
    );

    assert!(!json.valid);
    assert_eq!(json.diagnostics[0].path, "$");
    assert!(json.diagnostics[0].message.contains("invalid JSON"));
    assert!(!toml.valid);
    assert!(toml.diagnostics[0].message.contains("invalid TOML"));
    assert!(!yaml.valid);
    assert!(yaml.diagnostics[0].message.contains("invalid YAML"));
}
/// Verifies YAML config parsing preserves mapping and root-shape behavior.
///
/// This regression scenario covers the maintained YAML parser replacement so
/// empty documents, mapping roots, and scalar roots keep the same user-visible
/// validation contract.
#[test]
fn yaml_config_parser_preserves_mapping_and_root_shape_behavior() {
    let empty = validate_config_text(ConfigFormat::Yaml, "  \n", ConfigScope::Primary);
    assert!(empty.valid, "{:?}", empty.diagnostics);

    let mapping = validate_config_text(
        ConfigFormat::Yaml,
        "history:\n  lines: 200\n  persist: true\n",
        ConfigScope::Primary,
    );
    assert!(mapping.valid, "{:?}", mapping.diagnostics);
    let values = extract_config_values(
        ConfigFormat::Yaml,
        "history:\n  lines: 200\n  persist: true\n",
    );
    assert_eq!(values.get("history.lines").map(String::as_str), Some("200"));
    assert_eq!(
        values.get("history.persist").map(String::as_str),
        Some("true")
    );

    let scalar = validate_config_text(ConfigFormat::Yaml, "42\n", ConfigScope::Primary);
    assert!(!scalar.valid);
    assert_eq!(scalar.diagnostics[0].path, "$".to_string());
    assert!(
        scalar.diagnostics[0]
            .message
            .contains("YAML configuration root must be a mapping")
    );
}

/// Verifies validate config file reports syntax errors with file context.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn validate_config_file_reports_syntax_errors_with_file_context() {
    let root = temp_root("parse-context");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("config.json");
    fs::write(&path, r#"{ "history": "#).unwrap();

    let validation = validate_config_file(&path, ConfigScope::Primary).unwrap();

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, path.display().to_string());

    let _ = fs::remove_dir_all(root);
}
