//! Config schema validation tests.

use super::*;

/// Verifies rejects unknown top level keys.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_unknown_top_level_keys() {
    let validation =
        validate_config_text(ConfigFormat::Toml, "unknown = true\n", ConfigScope::Primary);

    assert!(!validation.valid);
    assert_eq!(validation.diagnostics[0].path, "unknown");
}

/// Verifies rejects unknown nested schema keys.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_unknown_nested_schema_keys() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[terminal]\nextra = true\n[frames.status]\nenabled = true\n[frames.pane]\nright_status = \"pane\"\n[providers.openai]\nunknown = true\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "terminal.extra"
            && diagnostic.message == "unknown terminal configuration key"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "frames.status.enabled"
            && diagnostic.message == "unknown frames configuration target"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "frames.pane.right_status"
            && diagnostic.message == "unknown frame configuration key"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "providers.openai.unknown"
            && diagnostic.message == "unknown provider configuration key"
    }));
}

/// Verifies active and configured key-preset tables accept the same fixed and
/// command-binding fields as the materialized key table.
#[test]
fn accepts_key_preset_schema_paths() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        &format!(
            "version = {CURRENT_CONFIG_SCHEMA_VERSION}\n[key_preset]\nactive = \"custom\"\n[key_presets.custom]\nescape = \"C-a\"\nnew_window = \"A-n\"\n[key_presets.custom.command_bindings]\nx = \"new-window\"\n"
        ),
        ConfigScope::Primary,
    );

    assert!(validation.valid, "{:?}", validation.diagnostics);
}

/// Verifies misspelled active-selector and configured-preset fields are
/// rejected rather than silently ignored.
#[test]
fn rejects_unknown_key_preset_schema_paths() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        &format!(
            "version = {CURRENT_CONFIG_SCHEMA_VERSION}\n[key_preset]\nselected = \"simple\"\n[key_presets.custom]\nunknown = \"A-x\"\n"
        ),
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "key_preset.selected"
            && diagnostic.message == "unknown key_preset configuration key"
    }));
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "key_presets.custom.unknown"
            && diagnostic.message == "unknown key preset configuration key"
    }));
}
