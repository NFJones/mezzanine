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
