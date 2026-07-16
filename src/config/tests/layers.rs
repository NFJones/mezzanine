//! Config layers tests.

use super::*;

/// Verifies effective config applies layers in order with source tracking.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn effective_config_applies_layers_in_order_with_source_tracking() {
    let effective = compose_effective_config(&[
        ConfigLayer {
            name: "defaults".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 10000\n[frames.pane]\nenabled = false\n".to_string(),
        },
        ConfigLayer {
            name: "primary".to_string(),
            path: Some(PathBuf::from("/home/user/.config/mezzanine/config.toml")),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 2000\n".to_string(),
        },
        ConfigLayer {
            name: "live".to_string(),
            path: None,
            format: ConfigFormat::Json,
            scope: ConfigScope::LiveOverride,
            trusted: true,
            text: r#"{"frames":{"pane":{"enabled":true}}}"#.to_string(),
        },
    ])
    .unwrap();

    assert_eq!(effective.get("history.lines"), Some("2000"));
    assert_eq!(effective.source_for("history.lines"), Some("primary"));
    assert_eq!(effective.get("frames.pane.enabled"), Some("true"));
    assert_eq!(effective.source_for("frames.pane.enabled"), Some("live"));
    assert_eq!(
        effective.applied_layers(),
        &[
            "defaults".to_string(),
            "primary".to_string(),
            "live".to_string()
        ]
    );
}

/// Verifies untrusted project overlay is skipped with diagnostic.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn untrusted_project_overlay_is_skipped_with_diagnostic() {
    let effective = compose_effective_config(&[
        ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 10000\n".to_string(),
        },
        ConfigLayer {
            name: "project".to_string(),
            path: Some(PathBuf::from("/repo/.mezzanine/config.toml")),
            format: ConfigFormat::Toml,
            scope: ConfigScope::ProjectOverlay,
            trusted: false,
            text: "[history]\nlines = 50\n".to_string(),
        },
    ])
    .unwrap();

    assert_eq!(effective.get("history.lines"), Some("10000"));
    assert_eq!(effective.skipped_layers(), &["project".to_string()]);
    assert!(effective.diagnostics()[0].message.contains("pending trust"));
}

/// Verifies invalid layer prevents effective config.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn invalid_layer_prevents_effective_config() {
    let error = compose_effective_config(&[ConfigLayer {
        name: "bad".to_string(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: "[session]\ndefault_command = \"vim\"\n".to_string(),
    }])
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Config);
}
