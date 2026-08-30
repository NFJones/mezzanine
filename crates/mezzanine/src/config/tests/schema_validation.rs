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

/// Verifies external editors use structured argv candidates and prompt editing
/// is configurable through both the materialized and preset key surfaces.
#[test]
fn accepts_external_editor_and_edit_prompt_schema_paths() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        &format!(
            "version = {CURRENT_CONFIG_SCHEMA_VERSION}\n[external_editor]\ncommand = [\"editor\", \"{{file}}\"]\nfallback = [[\"vim\", \"{{file}}\"], [\"vi\"]]\n[keys]\nedit_prompt = \"v\"\n[key_presets.custom]\nedit_prompt = \"e\"\n"
        ),
        ConfigScope::Primary,
    );

    assert!(validation.valid, "{:?}", validation.diagnostics);
}

/// Verifies editor candidates reject shell strings, empty executables,
/// duplicate file placeholders, unsupported interpolation, and ASCII controls.
#[test]
fn rejects_invalid_external_editor_argv() {
    for (body, expected) in [
        (
            "command = \"vim {file}\"",
            "external_editor.command must be a non-empty argv string array",
        ),
        (
            "command = [\"\", \"{file}\"]",
            "external_editor.command executable must not be empty",
        ),
        (
            "command = [\"vim\", \"{file}\", \"{file}\"]",
            "external_editor.command must contain at most one {file} placeholder",
        ),
        (
            "command = [\"vim\", \"{draft}\"]",
            "external_editor.command contains unsupported interpolation",
        ),
        (
            "command = [\"vim\\u0000\", \"{file}\"]",
            "external_editor.command arguments must not contain ASCII control bytes",
        ),
        (
            "command = [\"vim\", \"label\\tvalue\", \"{file}\"]",
            "external_editor.command arguments must not contain ASCII control bytes",
        ),
        (
            "command = [\"vim\", \"{file}\"]\nfallback = [[\"nano\"], []]",
            "external_editor.fallback[1] must be a non-empty argv string array",
        ),
    ] {
        let validation = validate_config_text(
            ConfigFormat::Json,
            &format!(
                "{{\"version\":{CURRENT_CONFIG_SCHEMA_VERSION},\"external_editor\":{{{}}}}}",
                body.replace("command = ", "\"command\":")
                    .replace("\nfallback = ", ",\"fallback\":")
            ),
            ConfigScope::Primary,
        );
        assert!(
            validation
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message == expected),
            "missing {expected:?} for {body:?}: {:?}",
            validation.diagnostics
        );
    }
}

/// Verifies configuration rejects more candidates than the hidden runner can
/// execute so invocation cannot fail later with its internal contract status.
#[test]
fn rejects_external_editor_candidate_count_above_runner_limit() {
    let fallback = (0..16)
        .map(|index| vec![format!("editor-{index}")])
        .collect::<Vec<_>>();
    let text = serde_json::json!({
        "version": CURRENT_CONFIG_SCHEMA_VERSION,
        "external_editor": {
            "command": ["editor", "{file}"],
            "fallback": fallback,
        }
    })
    .to_string();

    let validation = validate_config_text(ConfigFormat::Json, &text, ConfigScope::Primary);

    assert!(
        validation.diagnostics.iter().any(|diagnostic| {
            diagnostic.path == "external_editor.fallback"
                && diagnostic.message
                    == "external_editor supports at most 16 command and fallback candidates"
        }),
        "{:?}",
        validation.diagnostics
    );
}
