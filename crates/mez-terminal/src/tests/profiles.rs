//! Regression tests for terminal profiles behavior.

use crate::{
    CapabilitySupport, DEFAULT_TERMINAL_PROFILE_NAME, GraphicRendition, PaneRenditionCompatibility,
    TerminalDiagnosticSeverity, TerminalProfile, TerminfoSource, select_installed_terminfo,
    select_terminfo, terminal_profile_named,
};

/// Verifies only Screen-family TERM values select standout compatibility.
///
/// The boundary must match the TERM grammar rather than arbitrary substrings,
/// so profiles such as tmux and custom terminals retain real italic text.
#[test]
fn pane_rendition_compatibility_classifies_screen_terms() {
    assert_eq!(
        PaneRenditionCompatibility::for_term("screen"),
        PaneRenditionCompatibility::ScreenStandout
    );
    assert_eq!(
        PaneRenditionCompatibility::for_term("screen-256color"),
        PaneRenditionCompatibility::ScreenStandout
    );
    assert_eq!(
        PaneRenditionCompatibility::for_term("screen-direct"),
        PaneRenditionCompatibility::ScreenStandout
    );
    for term in ["tmux-256color", "mez-256color", "xterm-screen"] {
        assert_eq!(
            PaneRenditionCompatibility::for_term(term),
            PaneRenditionCompatibility::Normal
        );
    }
}

/// Verifies Screen standout translation changes only italic presentation.
///
/// Canonical parser state remains italic while attached-client presentation
/// receives reverse video and preserves all unrelated style attributes.
#[test]
fn screen_standout_compatibility_translates_only_italic() {
    let source = GraphicRendition {
        bold: true,
        italic: true,
        underline: true,
        ..GraphicRendition::default()
    };
    let presented = PaneRenditionCompatibility::ScreenStandout.present_rendition(source);

    assert!(presented.bold);
    assert!(presented.underline);
    assert!(!presented.italic);
    assert!(presented.inverse);
    assert_eq!(
        PaneRenditionCompatibility::Normal.present_rendition(source),
        source
    );
}

/// Verifies xterm compatible profile declares required capabilities.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn xterm_compatible_profile_declares_required_capabilities() {
    let profile = TerminalProfile::XtermCompatible.compatibility_profile();

    assert_eq!(profile.name, "xterm-compatible");
    assert_eq!(
        profile.capabilities.c0_controls,
        CapabilitySupport::Supported
    );
    assert_eq!(
        profile.capabilities.esc_sequences,
        CapabilitySupport::Supported
    );
    assert_eq!(
        profile.capabilities.csi_sequences,
        CapabilitySupport::Supported
    );
    assert_eq!(
        profile.capabilities.osc_string_controls,
        CapabilitySupport::Supported
    );
    assert_eq!(
        profile.capabilities.dcs_string_controls,
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        profile.capabilities.sgr.indexed_256_colors,
        CapabilitySupport::Supported
    );
    assert_eq!(
        profile.capabilities.sgr.true_color,
        CapabilitySupport::Supported
    );
    assert_eq!(
        profile.capabilities.dec_private_modes.alternate_screen,
        CapabilitySupport::Supported
    );
    assert_eq!(
        profile.capabilities.dec_private_modes.application_cursor,
        CapabilitySupport::Supported
    );
    assert_eq!(
        profile.capabilities.dec_private_modes.application_keypad,
        CapabilitySupport::Supported
    );
    assert_eq!(
        profile.capabilities.dec_private_modes.bracketed_paste,
        CapabilitySupport::Supported
    );
    assert_eq!(
        profile.capabilities.dec_private_modes.focus_events,
        CapabilitySupport::HostDependent
    );
    assert_eq!(
        profile.capabilities.dec_private_modes.sgr_mouse,
        CapabilitySupport::Supported
    );
    assert_eq!(
        profile.capabilities.title_setting,
        CapabilitySupport::Supported
    );
    assert_eq!(
        profile.capabilities.clipboard,
        CapabilitySupport::PolicyGated
    );
    assert_eq!(
        profile.capabilities.save_restore.cursor,
        CapabilitySupport::Supported
    );
    assert_eq!(
        profile.capabilities.save_restore.modes,
        CapabilitySupport::Supported
    );
}

/// Verifies terminal profile lookup uses stable names.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_profile_lookup_uses_stable_names() {
    let profile = terminal_profile_named("xterm-compatible").unwrap();

    assert_eq!(profile.profile, TerminalProfile::XtermCompatible);
    assert_eq!(profile.name, DEFAULT_TERMINAL_PROFILE_NAME);
    assert_eq!(
        terminal_profile_named("ansi").unwrap_err().message(),
        "unknown terminal compatibility profile ansi"
    );
}

/// Verifies terminfo prefers mezzanine entry.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminfo_prefers_mezzanine_entry() {
    let selection = select_terminfo(true, ["screen-256color"]);

    assert_eq!(selection.term, "mez-256color");
    assert_eq!(selection.profile_name(), "xterm-compatible");
    assert_eq!(selection.source, TerminfoSource::Mezzanine);
    assert!(!selection.degraded);
}

/// Verifies terminfo accepts mezzanine alias from installed terms.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminfo_accepts_mezzanine_alias_from_installed_terms() {
    let selection = select_installed_terminfo(["mezzanine-256color", "screen-256color"]);

    assert_eq!(selection.term, "mezzanine-256color");
    assert_eq!(selection.profile, TerminalProfile::XtermCompatible);
    assert_eq!(selection.source, TerminfoSource::Mezzanine);
    assert!(!selection.degraded);
}

/// Verifies the xterm-compatible profile does not advertise DCS string controls
/// before the emulator grows a matching handler.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn xterm_compatible_profile_does_not_advertise_unimplemented_dcs_support() {
    let profile = terminal_profile_named("xterm-compatible").unwrap();

    assert_eq!(
        profile.capabilities.dcs_string_controls,
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        profile.capabilities.osc_string_controls,
        CapabilitySupport::Supported
    );
}

/// Verifies terminfo fallbacks have capability safe degradation.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminfo_fallbacks_have_capability_safe_degradation() {
    let selection = select_installed_terminfo(["screen-256color"]);

    assert_eq!(selection.term, "screen-256color");
    assert_eq!(selection.profile, TerminalProfile::XtermCompatible);
    assert_eq!(selection.source, TerminfoSource::InstalledFallback);
    assert!(selection.degraded);
    assert_eq!(
        selection.advertised_capabilities.sgr.indexed_256_colors,
        CapabilitySupport::Supported
    );
    assert_eq!(
        selection.advertised_capabilities.osc_string_controls,
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        selection
            .advertised_capabilities
            .dec_private_modes
            .bracketed_paste,
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        selection
            .advertised_capabilities
            .dec_private_modes
            .sgr_mouse,
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        selection.advertised_capabilities.clipboard,
        CapabilitySupport::Unsupported
    );
}

/// Verifies terminfo diagnostics expose profile term and degradation.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminfo_diagnostics_expose_profile_term_and_degradation() {
    let selection = select_installed_terminfo(["vt100"]);

    assert!(selection.degraded);
    assert!(selection.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "terminal.profile_selected"
            && diagnostic.message.contains("profile=xterm-compatible")
            && diagnostic.message.contains("TERM=vt100")
    }));
    assert!(selection.diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == TerminalDiagnosticSeverity::Warning
            && diagnostic.code == "terminal.terminfo_fallback"
    }));
}

/// Verifies terminfo uses dumb when no fallback is installed.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminfo_uses_dumb_when_no_fallback_is_installed() {
    let selection = select_terminfo(false, [] as [&str; 0]);

    assert_eq!(selection.term, "dumb");
    assert_eq!(selection.profile, TerminalProfile::Dumb);
    assert_eq!(selection.source, TerminfoSource::BuiltInDumb);
    assert!(selection.degraded);
    assert_eq!(
        selection.advertised_capabilities.csi_sequences,
        CapabilitySupport::Unsupported
    );
    assert!(selection.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "terminal.terminfo_builtin_dumb"
            && diagnostic.message.contains("TERM=dumb")
            && diagnostic.message.contains("mez-256color")
    }));
}

/// Verifies terminfo does not use host xterm identity by default.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminfo_does_not_use_host_xterm_identity_by_default() {
    let selection = select_installed_terminfo(["xterm-256color", "xterm"]);

    assert_eq!(selection.term, "dumb");
    assert_eq!(selection.profile, TerminalProfile::Dumb);
    assert_eq!(selection.source, TerminfoSource::BuiltInDumb);
    assert!(selection.degraded);
}
