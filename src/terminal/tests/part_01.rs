// Regression coverage for the terminal tests subsystem.
//
// These tests describe the behavior protected by the repository
// specification and workflow guidance. Keeping the scenarios documented
// makes failures easier to map back to the user-visible contract.

// Terminal module tests.

use super::client_loop::{
    AttachedTerminalOutputFrameState, AttachedTerminalOutputModes,
    encode_attached_terminal_output_frame_with_keypad_transition,
    encode_attached_terminal_output_frame_with_styles,
    encode_attached_terminal_output_update_frame_with_styles,
    route_client_input_actions_with_host_paste_buffer,
    route_client_input_actions_with_host_paste_state,
};
use super::fd::duration_to_timespec;
use super::screen::{GraphicRendition, TerminalColor, TerminalStyleSpan};
use super::{
    AlternateScreenState, AttachedTerminalClientLoopConfig, AttachedTerminalClientLoopIo,
    AttachedTerminalFd, AttachedTerminalFdLoopIo, AttachedTerminalFdReadiness,
    AttachedTerminalFdRole, BTreeMap, BUILTIN_UI_THEME_NAMES, CapabilitySupport, ClientStatusKind,
    ClientStatusLine, ClientViewRole, CopyMode, CopyModeKeyAction, CopyPosition,
    DEFAULT_HISTORY_LIMIT, DEFAULT_HISTORY_ROTATE_LINES, DEFAULT_PANE_FRAME_TEMPLATE,
    DEFAULT_TERMINAL_PROFILE_NAME, DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE,
    DEFAULT_WINDOW_FRAME_TEMPLATE, Duration, GroupFocusTarget, HistoryBuffer, KeyBindings,
    KeyChord, KeyCode, MouseAction, MouseBorderCell, MouseButton, MouseEvent, MouseEventKind,
    MouseModifiers, MousePaneAgentSelectorCell, MousePaneAgentStatusCell, MousePaneRegion,
    MousePolicy, MouseWindowActionFrameCell, MouseWindowFrameCell, MouseWindowGroupFrameCell,
    MuxAction, PaneAgentStatusField, PaneFocusDirection, PaneRenderInput, PasteBufferTarget,
    PasteBuffers, ReadlinePromptRegion, RenderedClientView, Result, SearchDirection, Size,
    TerminalClientLoopAction, TerminalClientLoopConfig, TerminalCursorState, TerminalCursorStyle,
    TerminalDiagnosticSeverity, TerminalFdInterest, TerminalFrameContext, TerminalFramePosition,
    TerminalFrameRenderOptions, TerminalFrameStyle, TerminalInputClassification, TerminalModeState,
    TerminalOscEvent, TerminalPaneFrameContext, TerminalProfile, TerminalRawModeGuard,
    TerminalScreen, TerminalStyledLine, TerminalWindowFrameContext,
    TerminalWindowGroupFrameContext, TerminalWindowStatusContext, TerminfoSource, UiTheme, Window,
    WindowFocusTarget, WindowFrameAction, apply_client_view_offset, builtin_ui_theme_definition,
    classify_mouse_event, classify_terminal_input, compose_client_presentation,
    compose_display_overlay_line_style_spans, compose_display_overlay_lines,
    compose_display_region_overlay_line_style_spans, compose_display_region_overlay_lines,
    compose_modal_display_overlay_line_style_spans, compose_modal_display_overlay_lines,
    compose_prompt_overlay_presentation, compose_prompt_overlay_presentation_with_styles,
    compose_prompt_region_presentation_with_styles, compose_readline_prompt_client_presentation,
    draw_window_from_screens, modal_display_overlay_max_scroll, pane_divider_glyph_for_test,
    pane_frame_agent_status_pillbox_cells, pane_render_region_size_for_geometry, parse_hex_color,
    parse_sgr_mouse, plan_attached_terminal_client_step, poll_attached_terminal_fd_readiness,
    render_attached_client_view, render_readline_prompt_status_row, render_window,
    render_window_with_pane_frame_template, rendered_pane_geometries, resolve_ui_theme,
    route_client_input, route_client_input_actions, run_attached_terminal_client_loop,
    select_installed_terminfo, select_terminfo, terminal_char_width, terminal_profile_named,
    terminal_text_width, window_frame_action_pillbox_cells,
};
use crate::ids::IdFactory;
use crate::layout::{PaneGeometry, SplitDirection};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use unicode_width::UnicodeWidthStr;

/// Returns the display column at which one text fragment starts.
///
/// # Parameters
/// - `line`: The rendered terminal line to inspect.
/// - `needle`: The text fragment whose starting column is needed.
fn display_column_for_fragment(line: &str, needle: &str) -> usize {
    let byte_index = line
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} missing from {line:?}"));
    UnicodeWidthStr::width(&line[..byte_index])
}

/// Returns RGB components for true-color test values.
fn test_rgb_channels(color: TerminalColor) -> (u8, u8, u8) {
    match color {
        TerminalColor::Rgb(red, green, blue) => (red, green, blue),
        TerminalColor::Indexed(index) => panic!("expected true-color value: {index}"),
    }
}

/// Returns true when a test color is a neutral grey.
fn test_color_is_grayscale(color: TerminalColor) -> bool {
    let (red, green, blue) = test_rgb_channels(color);
    red == green && green == blue
}

/// Returns WCAG-style contrast ratio for two true-color test values.
fn test_contrast_ratio(foreground: TerminalColor, background: TerminalColor) -> f64 {
    let foreground_luminance = test_relative_luminance(foreground);
    let background_luminance = test_relative_luminance(background);
    let lighter = foreground_luminance.max(background_luminance);
    let darker = foreground_luminance.min(background_luminance);
    (lighter + 0.05) / (darker + 0.05)
}

/// Returns the relative luminance of a true-color test value.
fn test_relative_luminance(color: TerminalColor) -> f64 {
    let (red, green, blue) = test_rgb_channels(color);
    0.2126 * test_srgb_channel_to_linear(red)
        + 0.7152 * test_srgb_channel_to_linear(green)
        + 0.0722 * test_srgb_channel_to_linear(blue)
}

/// Converts one sRGB channel to linear-light space for contrast assertions.
fn test_srgb_channel_to_linear(channel: u8) -> f64 {
    let normalized = f64::from(channel) / 255.0;
    if normalized <= 0.03928 {
        normalized / 12.92
    } else {
        ((normalized + 0.055) / 1.055).powf(2.4)
    }
}

/// Runs the pipe pair operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pipe_pair() -> std::io::Result<(File, File)> {
    let (read_end, write_end) = rustix::pipe::pipe().map_err(std::io::Error::from)?;
    Ok((File::from(read_end), File::from(write_end)))
}

/// Verifies history buffer evicts oldest lines first.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn history_buffer_evicts_oldest_lines_first() {
    let mut history = HistoryBuffer::new(2).unwrap();

    history.push_line("one");
    history.push_line("two");
    history.push_line("three");

    assert_eq!(history.lines().collect::<Vec<_>>(), vec!["two", "three"]);
}

/// Verifies history buffer relimits and evicts oldest lines.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn history_buffer_relimits_and_evicts_oldest_lines() {
    let mut history = HistoryBuffer::new(4).unwrap();

    history.push_line("one");
    history.push_line("two");
    history.push_line("three");
    history.push_line("four");
    history.set_limit(2).unwrap();

    assert_eq!(history.limit(), 2);
    assert_eq!(history.lines().collect::<Vec<_>>(), vec!["three", "four"]);
    assert!(HistoryBuffer::new(1).unwrap().set_limit(0).is_err());
}

/// Verifies history buffer rotates oldest lines in configurable batches.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn history_buffer_rotates_oldest_lines_in_configured_batches() {
    let mut history = HistoryBuffer::new_with_rotation(5, 2).unwrap();

    for line in ["one", "two", "three", "four", "five", "six"] {
        history.push_line(line);
    }

    assert_eq!(history.limit(), 5);
    assert_eq!(history.rotate_lines(), 2);
    assert_eq!(
        history.lines().collect::<Vec<_>>(),
        vec!["three", "four", "five", "six"]
    );
    history.push_line("seven");
    assert_eq!(
        history.lines().collect::<Vec<_>>(),
        vec!["three", "four", "five", "six", "seven"]
    );
    assert!(HistoryBuffer::new_with_rotation(2, 0).is_err());
}

/// Verifies terminal screen relimits history buffer.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_relimits_history_buffer() {
    let mut screen = TerminalScreen::new(Size::new(8, 2).unwrap(), 4).unwrap();
    screen.restore_normal_content(
        &["one".to_string(), "two".to_string(), "three".to_string()],
        &[],
    );

    screen.set_history_limit(2).unwrap();

    assert_eq!(screen.history_limit(), 2);
    assert_eq!(
        screen.history().lines().collect::<Vec<_>>(),
        vec!["two", "three"]
    );
}

/// Verifies that snapshot resume can rebuild a visible terminal row with SGR
/// spans even though the original PTY byte stream is no longer available.
#[test]
fn terminal_screen_restores_styled_visible_snapshot_content() {
    let mut screen = TerminalScreen::new(Size::new(12, 2).unwrap(), 4).unwrap();
    let rendition = GraphicRendition {
        bold: true,
        dim: false,
        italic: false,
        strikethrough: false,
        double_underline: false,
        hidden: false,
        underline: false,
        inverse: false,
        foreground: Some(TerminalColor::Rgb(1, 2, 3)),
        background: Some(TerminalColor::Indexed(4)),
    };

    screen.restore_normal_styled_content(
        &["history".to_string()],
        &[TerminalStyledLine {
            text: "styled".to_string(),
            style_spans: vec![TerminalStyleSpan {
                start: 0,
                length: 6,
                rendition,
            }],
            copy_text: None,
        }],
    );

    assert_eq!(
        screen.history().lines().collect::<Vec<_>>(),
        vec!["history"]
    );
    assert_eq!(screen.visible_lines()[0], "styled");
    assert_eq!(screen.cell_rendition(0, 0), Some(rendition));
    assert_eq!(
        screen.visible_styled_lines()[0].style_spans,
        vec![TerminalStyleSpan {
            start: 0,
            length: 6,
            rendition
        }]
    );
}

/// Verifies that theme hex parsing accepts the documented shorthand and full
/// true-color forms while rejecting malformed UTF-8 input without panicking.
/// User config validation and runtime theme resolution both rely on this
/// parser before assigning colors to Mezzanine-owned UI surfaces.
#[test]
fn theme_hex_color_parser_accepts_documented_forms() {
    assert_eq!(
        parse_hex_color("#abc"),
        Some(TerminalColor::Rgb(0xaa, 0xbb, 0xcc))
    );
    assert_eq!(
        parse_hex_color("#123456"),
        Some(TerminalColor::Rgb(0x12, 0x34, 0x56))
    );
    assert_eq!(parse_hex_color("#ééé"), None);
    assert_eq!(parse_hex_color("123456"), None);
}

/// Verifies built-in themes render agent thinking/status transcript text as a
/// visible theme-relative grey rather than reusing a high-emphasis user or
/// assistant accent. Thinking lines are additionally rendered dim by the
/// runtime, so this test protects the color side of the lower-emphasis
/// presentation contract across every built-in palette.
#[test]
fn builtin_themes_use_visible_muted_grey_for_agent_thinking() {
    fn channel_spread(color: TerminalColor) -> i32 {
        let (red, green, blue) = test_rgb_channels(color);
        let red = i32::from(red);
        let green = i32::from(green);
        let blue = i32::from(blue);
        let min = red.min(green).min(blue);
        let max = red.max(green).max(blue);
        max - min
    }

    fn channel_average(color: TerminalColor) -> u16 {
        let (red, green, blue) = test_rgb_channels(color);
        (u16::from(red) + u16::from(green) + u16::from(blue)) / 3
    }

    for name in BUILTIN_UI_THEME_NAMES {
        let definition =
            builtin_ui_theme_definition(name).unwrap_or_else(|| panic!("missing theme {name}"));
        let theme = resolve_ui_theme(name, definition).expect("built-in theme must resolve");
        let thinking = theme.colors.agent_transcript_status.foreground;
        let background = theme.colors.agent_transcript_status.background;

        assert_ne!(
            thinking, theme.colors.agent_transcript_user.foreground,
            "{name} should not reuse the user transcript accent for thinking"
        );
        assert_ne!(
            thinking, theme.colors.agent_transcript_assistant.foreground,
            "{name} should not reuse the assistant transcript accent for thinking"
        );
        assert!(
            channel_spread(thinking) <= 55,
            "{name} thinking color should stay grey-equivalent: {:?}",
            thinking
        );
        assert!(
            test_contrast_ratio(thinking, background) >= 4.5,
            "{name} thinking color should remain readable against its background"
        );
        if test_relative_luminance(background) < 0.5 {
            assert!(
                channel_average(thinking) >= 165,
                "{name} thinking color should stay bright enough after dim rendering: {:?}",
                thinking
            );
        }
    }
}

/// Verifies built-in themes keep every muted/low-emphasis color pair readable.
///
/// These pairs carry inactive frames, thinking/status transcript text, and syntax
/// comments/operators. They should look quiet, but they still need normal text
/// contrast against the theme surface on both dark and light built-in palettes.
#[test]
fn builtin_themes_keep_low_emphasis_text_pairs_readable() {
    for name in BUILTIN_UI_THEME_NAMES {
        let definition =
            builtin_ui_theme_definition(name).unwrap_or_else(|| panic!("missing theme {name}"));
        let theme = resolve_ui_theme(name, definition).expect("built-in theme must resolve");
        let pairs = [
            ("pane_frame_inactive", theme.colors.pane_frame_inactive),
            ("pane_border_inactive", theme.colors.pane_border_inactive),
            ("pane_pwd", theme.colors.pane_pwd),
            ("agent_status_idle", theme.colors.agent_status_idle),
            (
                "agent_transcript_status",
                theme.colors.agent_transcript_status,
            ),
            ("syntax_comment", theme.colors.syntax_comment),
            ("syntax_operator", theme.colors.syntax_operator),
        ];

        for (slot, pair) in pairs {
            assert!(
                test_contrast_ratio(pair.foreground, pair.background) >= 4.5,
                "{name} {slot} should have readable contrast: {:?} on {:?}",
                pair.foreground,
                pair.background
            );
        }
    }
}

/// Verifies the built-in registry contains the common theme families that the
/// command selector, `list-themes`, and `set-theme` all expose by name. This
/// guards against adding a palette implementation without making it discoverable
/// or accidentally leaving duplicate names in the public registry.
#[test]
fn builtin_theme_registry_includes_common_variants_without_duplicates() {
    let unique = BUILTIN_UI_THEME_NAMES
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        unique.len(),
        BUILTIN_UI_THEME_NAMES.len(),
        "built-in theme names must be unique"
    );

    for expected in [
        "catppuccin_latte",
        "catppuccin_frappe",
        "catppuccin_macchiato",
        "catppuccin_mocha",
        "one_half_dark",
        "one_half_light",
        "rose_pine",
        "rose_pine_moon",
        "rose_pine_dawn",
        "kanagawa",
        "everforest_dark",
        "everforest_light",
        "ayu",
        "ayu_dark",
        "ayu_light",
        "ayu_mirage",
        "high_contrast_dark",
        "high_contrast_light",
    ] {
        assert!(
            unique.contains(expected),
            "built-in theme registry should include {expected}"
        );
    }

    for name in BUILTIN_UI_THEME_NAMES {
        let definition =
            builtin_ui_theme_definition(name).unwrap_or_else(|| panic!("missing theme {name}"));
        resolve_ui_theme(name, definition).expect("built-in theme must resolve");
    }
}

/// Verifies built-in themes choose a black-or-white agent prompt foreground
/// from each prompt background. Prompt input must stay readable even when the
/// active theme uses a light agent prompt surface.
#[test]
fn builtin_themes_use_binary_agent_prompt_foreground() {
    fn luminance(color: TerminalColor) -> u32 {
        match color {
            TerminalColor::Rgb(red, green, blue) => {
                (u32::from(red) * 299 + u32::from(green) * 587 + u32::from(blue) * 114) / 1000
            }
            TerminalColor::Indexed(index) => panic!("expected true-color prompt color: {index}"),
        }
    }

    for name in BUILTIN_UI_THEME_NAMES {
        let definition =
            builtin_ui_theme_definition(name).unwrap_or_else(|| panic!("missing theme {name}"));
        let theme = resolve_ui_theme(name, definition).expect("built-in theme must resolve");
        let expected = if luminance(theme.colors.agent_prompt.background) >= 140 {
            TerminalColor::Rgb(0x00, 0x00, 0x00)
        } else {
            TerminalColor::Rgb(0xff, 0xff, 0xff)
        };

        assert_eq!(
            theme.colors.agent_prompt.foreground, expected,
            "{name} agent prompt foreground should be binary contrast"
        );
    }
}

/// Verifies default history limit matches spec.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn default_history_limit_matches_spec() {
    let history = HistoryBuffer::default_limit();

    assert_eq!(history.limit(), DEFAULT_HISTORY_LIMIT);
    assert_eq!(history.rotate_lines(), DEFAULT_HISTORY_ROTATE_LINES);
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
        CapabilitySupport::Supported
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
        CapabilitySupport::Supported
    );
    assert_eq!(
        profile.capabilities.dec_private_modes.sgr_mouse,
        CapabilitySupport::Supported
    );
    assert_eq!(
        profile.capabilities.title_setting,
        CapabilitySupport::Supported
    );
    assert_eq!(profile.capabilities.clipboard, CapabilitySupport::Supported);
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
        terminal_profile_named("ansi").unwrap_err().kind(),
        crate::error::MezErrorKind::InvalidArgs
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

/// Verifies parses key binding notation for default surface.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn parses_key_binding_notation_for_default_surface() {
    assert_eq!(
        KeyChord::parse("C-a").unwrap(),
        KeyBindings::default().escape
    );
    assert_eq!(
        KeyChord::parse("Alt+\\").unwrap(),
        KeyChord::alt(KeyCode::Char('\\'))
    );
    assert_eq!(KeyBindings::default().split_vertical, None);
    assert_eq!(KeyBindings::default().new_window, None);
    assert_eq!(
        KeyChord::parse("A--").unwrap(),
        KeyChord::alt(KeyCode::Char('-'))
    );
    assert_eq!(
        KeyChord::parse("C-A-PageDown").unwrap(),
        KeyChord::ctrl_alt(KeyCode::PageDown)
    );
    assert_eq!(KeyBindings::default().new_group, None);
    assert_eq!(KeyBindings::default().agent_shell, None);
    assert_eq!(KeyBindings::default().focus_previous_group, None);
    assert_eq!(KeyBindings::default().focus_next_group, None);
    assert_eq!(
        KeyChord::parse("Ctrl+Alt+Up").unwrap(),
        KeyChord::ctrl_alt(KeyCode::Up)
    );
    assert_eq!(
        super::key_chord_input_bytes(KeyChord::parse("C-a").unwrap()).unwrap(),
        b"\x01"
    );
    assert_eq!(
        super::key_chord_input_bytes(KeyChord::parse("A--").unwrap()).unwrap(),
        b"\x1b-"
    );
    assert_eq!(
        super::key_chord_input_bytes(KeyChord::parse("C-A-PageDown").unwrap()).unwrap(),
        b"\x1b[6;7~"
    );
    assert_eq!(
        super::key_chord_input_bytes(KeyChord::parse("A-S-=").unwrap()).unwrap(),
        b"\x1b+"
    );
    assert_eq!(
        super::key_chord_input_bytes(KeyChord::parse("C-A-S-PageUp").unwrap()).unwrap(),
        b"\x1b[5;8~"
    );
    assert_eq!(
        KeyChord::parse("Home").unwrap(),
        KeyChord::new(KeyCode::Home)
    );
    assert_eq!(KeyChord::parse("End").unwrap(), KeyChord::new(KeyCode::End));
    assert_eq!(
        super::key_chord_input_bytes(KeyChord::parse("Home").unwrap()).unwrap(),
        b"\x1b[H"
    );
    assert_eq!(
        super::key_chord_input_bytes(KeyChord::parse("C-End").unwrap()).unwrap(),
        b"\x1b[1;5F"
    );
    assert_eq!(
        KeyChord::parse("C-C-a").unwrap_err().kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
    assert_eq!(
        KeyChord::parse("DefinitelyNotAKey").unwrap_err().kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
}

/// Verifies classifies default direct mux key bindings.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn classifies_default_direct_mux_key_bindings() {
    let bindings = KeyBindings::default();

    assert_eq!(
        classify_terminal_input(b"\x1b\\", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b-", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b=", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b+", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b]", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[1;7A", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[1;7B", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[1;7D", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[1;7C", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[5;7~", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[6;7~", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[5;8~", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b[6;8~", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"ordinary input", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
    assert_eq!(
        classify_terminal_input(b"\x1b]0;title\x07", &bindings).unwrap(),
        TerminalInputClassification::ForwardToPane
    );
}

/// Verifies classifies established mux-compatible default prefix key bindings.
///
/// The default prefix table is the primary keyboard contract for users who
/// expect default mux navigation and pane/window commands. This test keeps the
/// broad table pinned so direct convenience bindings cannot accidentally
/// replace the prefix surface.
#[test]
fn classifies_default_prefix_key_bindings() {
    let bindings = KeyBindings::default();
    let assert_prefix = |input: &[u8], action: MuxAction| {
        assert_eq!(
            classify_terminal_input(input, &bindings).unwrap(),
            TerminalInputClassification::Mux(action)
        );
    };

    assert_eq!(
        classify_terminal_input(b"\x01", &bindings).unwrap(),
        TerminalInputClassification::PrefixKeyMode
    );
    assert_prefix(b"\x01\x01", MuxAction::SendPrefixToPane);
    assert_prefix(b"\x01:", MuxAction::EnterCommandPrompt);
    assert_prefix(b"\x01?", MuxAction::ListKeyBindings);
    assert_prefix(b"\x01d", MuxAction::DetachPrimaryClient);
    assert_prefix(b"\x01D", MuxAction::ChooseClientOrObserverToDetach);
    assert_prefix(b"\x01c", MuxAction::NewWindow);
    assert_prefix(b"\x01,", MuxAction::RenameWindow);
    assert_prefix(b"\x01&", MuxAction::KillWindowAfterConfirmation);
    assert_prefix(
        b"\x01w",
        MuxAction::FocusWindow(WindowFocusTarget::ChooseInteractively),
    );
    assert_prefix(
        b"\x01G",
        MuxAction::FocusGroup(GroupFocusTarget::ChooseInteractively),
    );
    assert_prefix(b"\x01C", MuxAction::NewGroup);
    assert_prefix(b"\x01(", MuxAction::FocusGroup(GroupFocusTarget::Previous));
    assert_prefix(b"\x01)", MuxAction::FocusGroup(GroupFocusTarget::Next));
    assert_prefix(b"\x01a", MuxAction::ToggleAgentShell);
    assert_prefix(b"\x01n", MuxAction::FocusWindow(WindowFocusTarget::Next));
    assert_prefix(
        b"\x01p",
        MuxAction::FocusWindow(WindowFocusTarget::Previous),
    );
    assert_prefix(
        b"\x01l",
        MuxAction::FocusWindow(WindowFocusTarget::LastActive),
    );
    assert_prefix(
        b"\x014",
        MuxAction::FocusWindow(WindowFocusTarget::Index(4)),
    );
    assert_prefix(
        b"\x01'",
        MuxAction::FocusWindow(WindowFocusTarget::PromptForIndex),
    );
    assert_prefix(
        b"\x01.",
        MuxAction::FocusWindow(WindowFocusTarget::PromptForNewIndex),
    );
    assert_prefix(b"\x01%", MuxAction::SplitPaneVertical);
    assert_prefix(b"\x01\"", MuxAction::SplitPaneHorizontal);
    assert_prefix(b"\x01\x1bOA", MuxAction::FocusPane(PaneFocusDirection::Up));
    assert_prefix(
        b"\x01\x1bOB",
        MuxAction::FocusPane(PaneFocusDirection::Down),
    );
    assert_prefix(
        b"\x01\x1bOD",
        MuxAction::FocusPane(PaneFocusDirection::Left),
    );
    assert_prefix(
        b"\x01\x1bOC",
        MuxAction::FocusPane(PaneFocusDirection::Right),
    );
    assert_prefix(b"\x01o", MuxAction::CyclePane);
    assert_prefix(b"\x01;", MuxAction::FocusLastPane);
    assert_prefix(b"\x01q", MuxAction::ShowPaneIndexes);
    assert_prefix(b"\x01z", MuxAction::TogglePaneZoom);
    assert_prefix(b"\x01 ", MuxAction::CycleLayouts);
    assert_prefix(b"\x01x", MuxAction::KillPaneAfterConfirmation);
    assert_prefix(b"\x01!", MuxAction::BreakPaneToNewWindow);
    assert_prefix(b"\x01{", MuxAction::SwapPanePrevious);
    assert_prefix(b"\x01}", MuxAction::SwapPaneNext);
    assert_prefix(b"\x01\x1b[5~", MuxAction::EnterCopyModeAndPageUp);
    assert_prefix(b"\x01[", MuxAction::EnterCopyMode);
    assert_prefix(
        b"\x01]",
        MuxAction::PasteBuffer(PasteBufferTarget::MostRecent),
    );
    assert_prefix(b"\x01#", MuxAction::ListPasteBuffers);
    assert_prefix(
        b"\x01=",
        MuxAction::PasteBuffer(PasteBufferTarget::ChooseInteractively),
    );
    assert_prefix(b"\x01-", MuxAction::DeleteMostRecentPasteBuffer);
    assert_prefix(b"\x01O", MuxAction::ChoosePendingObservers);
    assert_prefix(b"\x01~", MuxAction::ShowMessages);
    assert_eq!(
        classify_terminal_input(b"\x01e", &bindings).unwrap(),
        TerminalInputClassification::UnboundPrefix(KeyChord::new(KeyCode::Char('e')))
    );
}

/// Verifies classifies mouse sequences as terminal input.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn classifies_mouse_sequences_as_terminal_input() {
    assert_eq!(
        classify_terminal_input(b"\x1b[<0;12;5M", &KeyBindings::default()).unwrap(),
        TerminalInputClassification::Mouse(MouseEvent {
            kind: MouseEventKind::Press,
            button: MouseButton::Left,
            column: 11,
            row: 4,
            modifiers: MouseModifiers {
                shift: false,
                alt: false,
                ctrl: false,
            },
        })
    );
}

/// Verifies parses sgr mouse press drag release and scroll.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn parses_sgr_mouse_press_drag_release_and_scroll() {
    let press = parse_sgr_mouse(b"\x1b[<0;12;5M").unwrap().unwrap();
    assert_eq!(press.kind, MouseEventKind::Press);
    assert_eq!(press.button, MouseButton::Left);
    assert_eq!(press.column, 11);
    assert_eq!(press.row, 4);

    let drag = parse_sgr_mouse(b"\x1b[<32;12;6M").unwrap().unwrap();
    assert_eq!(drag.kind, MouseEventKind::Drag);

    let release = parse_sgr_mouse(b"\x1b[<0;12;6m").unwrap().unwrap();
    assert_eq!(release.kind, MouseEventKind::Release);

    let scroll = parse_sgr_mouse(b"\x1b[<65;12;6M").unwrap().unwrap();
    assert_eq!(scroll.kind, MouseEventKind::Scroll);
    assert_eq!(scroll.button, MouseButton::WheelDown);
}

/// Verifies classifies mouse actions for resize selection scroll and forwarding.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn classifies_mouse_actions_for_resize_selection_scroll_and_forwarding() {
    let event = MouseEvent {
        kind: MouseEventKind::Drag,
        button: MouseButton::Left,
        column: 4,
        row: 2,
        modifiers: MouseModifiers {
            shift: false,
            alt: false,
            ctrl: false,
        },
    };

    assert_eq!(
        classify_mouse_event(
            event,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: false,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: true,
                over_window_frame: false,
                copy_mode_active: false,
            },
        ),
        MouseAction::ResizePane { column: 4, row: 2 }
    );
    assert_eq!(
        classify_mouse_event(
            event,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: true,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: true,
                over_window_frame: false,
                copy_mode_active: true,
            },
        ),
        MouseAction::CopySelectionUpdate(CopyPosition { line: 2, column: 4 })
    );
    assert_eq!(
        classify_mouse_event(
            event,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: true,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: true,
                over_window_frame: false,
                copy_mode_active: false,
            },
        ),
        MouseAction::ResizePane { column: 4, row: 2 }
    );

    let pane_drag = MouseEvent {
        column: 8,
        row: 3,
        ..event
    };
    assert_eq!(
        classify_mouse_event(
            pane_drag,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: true,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: false,
                over_window_frame: false,
                copy_mode_active: false,
            },
        ),
        MouseAction::ForwardToPane
    );

    let scroll = MouseEvent {
        kind: MouseEventKind::Scroll,
        button: MouseButton::WheelUp,
        ..event
    };
    assert_eq!(
        classify_mouse_event(
            scroll,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: false,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: false,
                over_window_frame: false,
                copy_mode_active: false,
            },
        ),
        MouseAction::ScrollHistory {
            lines: -3,
            position: CopyPosition { line: 2, column: 4 },
        }
    );
    assert_eq!(
        classify_mouse_event(
            scroll,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: true,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: false,
                over_window_frame: false,
                copy_mode_active: false,
            },
        ),
        MouseAction::ForwardToPane
    );
    assert_eq!(
        classify_mouse_event(
            scroll,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: true,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: false,
                over_window_frame: false,
                copy_mode_active: true,
            },
        ),
        MouseAction::ScrollHistory {
            lines: -3,
            position: CopyPosition { line: 2, column: 4 },
        }
    );

    let right_click = MouseEvent {
        kind: MouseEventKind::Press,
        button: MouseButton::Right,
        ..event
    };
    assert_eq!(
        classify_mouse_event(
            right_click,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: true,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: false,
                over_window_frame: false,
                copy_mode_active: false,
            },
        ),
        MouseAction::ForwardToPane
    );

    let release = MouseEvent {
        kind: MouseEventKind::Release,
        button: MouseButton::Left,
        ..event
    };
    assert_eq!(
        classify_mouse_event(
            release,
            MousePolicy {
                enabled: true,
                pane_application_mouse_mode: false,
                pane_sgr_mouse_mode: false,
                pane_application_cursor_mode: false,
                pane_application_keypad_mode: false,
                pane_resize_active: false,
                over_pane_border: false,
                over_window_frame: false,
                copy_mode_active: true,
            },
        ),
        MouseAction::CopySelectionFinish(CopyPosition { line: 2, column: 4 })
    );
}

/// Verifies client loop routes input to pane mux and mouse actions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn client_loop_routes_input_to_pane_mux_and_mouse_actions() {
    let mut config = TerminalClientLoopConfig::default();

    assert_eq!(
        route_client_input(b"echo hi", &config).unwrap(),
        TerminalClientLoopAction::ForwardToPane(b"echo hi".to_vec())
    );
    assert_eq!(
        route_client_input(b"\x01%", &config).unwrap(),
        TerminalClientLoopAction::ExecuteMux(MuxAction::SplitPaneVertical)
    );
    assert_eq!(
        route_client_input(b"\x01", &config).unwrap(),
        TerminalClientLoopAction::EnterPrefixKeyMode
    );
    config.command_bindings.insert(
        KeyChord::new(KeyCode::Char('x')),
        "split-window -h".to_string(),
    );
    assert_eq!(
        route_client_input(b"\x01x", &config).unwrap(),
        TerminalClientLoopAction::ExecuteCommand("split-window -h".to_string())
    );

    let mut mouse_config = config.clone();
    mouse_config.mouse_policy.over_pane_border = true;
    assert_eq!(
        route_client_input(b"\x1b[<32;12;5M", &mouse_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane { column: 11, row: 4 })
    );

    let mut border_config = config.clone();
    border_config.mouse_border_cells = vec![MouseBorderCell { column: 11, row: 4 }];
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5M", &border_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane { column: 11, row: 4 })
    );

    let mut frame_config = config.clone();
    frame_config.mouse_window_frame_cells = vec![MouseWindowFrameCell {
        column: 11,
        row: 4,
        window_index: 2,
    }];
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5M", &frame_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::FocusWindow { index: 2 })
    );

    let mut group_frame_config = frame_config.clone();
    group_frame_config.mouse_window_group_frame_cells = vec![MouseWindowGroupFrameCell {
        column: 11,
        row: 4,
        group_index: 1,
    }];
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5M", &group_frame_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::FocusGroup { index: 1 })
    );

    let mut action_frame_config = frame_config.clone();
    action_frame_config.mouse_window_action_frame_cells = vec![MouseWindowActionFrameCell {
        column: 11,
        row: 4,
        action: WindowFrameAction::NewWindow,
    }];
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5M", &action_frame_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::PressWindowAction {
            action: WindowFrameAction::NewWindow,
        })
    );
    action_frame_config.frame_context.pressed_window_action = Some(WindowFrameAction::NewWindow);
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5m", &action_frame_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ReleaseWindowAction {
            action: WindowFrameAction::NewWindow,
        })
    );
    assert_eq!(
        route_client_input(b"\x1b[<0;13;5m", &action_frame_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::CancelWindowAction)
    );

    let mut pane_status_config = frame_config.clone();
    pane_status_config.mouse_pane_agent_status_cells = vec![MousePaneAgentStatusCell {
        column: 11,
        row: 4,
        pane_index: 0,
        field: PaneAgentStatusField::Model,
    }];
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5M", &pane_status_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::Ignore)
    );
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5m", &pane_status_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::OpenPaneAgentStatusSelector {
            pane_index: 0,
            field: PaneAgentStatusField::Model,
        })
    );

    let mut pane_selector_config = frame_config.clone();
    pane_selector_config.mouse_pane_agent_selector_cells = vec![MousePaneAgentSelectorCell {
        column: 11,
        row: 4,
        pane_index: 0,
        field: PaneAgentStatusField::Reasoning,
        item_index: 2,
    }];
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5M", &pane_selector_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::HoverPaneAgentStatusSelector {
            pane_index: 0,
            field: PaneAgentStatusField::Reasoning,
            item_index: 2,
        })
    );
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5m", &pane_selector_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::SelectPaneAgentStatusSelector {
            pane_index: 0,
            field: PaneAgentStatusField::Reasoning,
            item_index: 2,
        })
    );
    assert_eq!(
        route_client_input(b"\x1b[<0;13;5M", &pane_selector_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ClosePaneAgentStatusSelector)
    );
    assert_eq!(
        route_client_input(b"\x1b[<0;13;5m", &pane_selector_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::Ignore)
    );

    let mut display_overlay_config = frame_config.clone();
    display_overlay_config.primary_display_overlay_active = true;
    assert_eq!(
        route_client_input(b"\x1b[<0;4;3M", &display_overlay_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::SelectDisplayOverlay {
            position: CopyPosition { line: 2, column: 3 },
        })
    );
    assert_eq!(
        route_client_input(b"\x1b[<64;4;3M", &display_overlay_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ScrollDisplayOverlay { lines: -3 })
    );

    mouse_config.mouse_policy.over_pane_border = false;
    mouse_config.mouse_pane_regions = vec![MousePaneRegion {
        pane_id: "%1".to_string(),
        column: 0,
        row: 0,
        columns: 40,
        rows: 20,
        application_sgr_mouse_mode: true,
        application_mouse_mode: true,
        copy_mode_active: false,
        active: true,
    }];
    assert_eq!(
        route_client_input(b"\x1b[<0;12;5M", &mouse_config).unwrap(),
        TerminalClientLoopAction::ForwardMouseToPane {
            pane_id: "%1".to_string(),
            input: b"\x1b[<0;12;5M".to_vec(),
        }
    );
    assert_eq!(
        route_client_input(b"\x1b[<2;12;5M", &mouse_config).unwrap(),
        TerminalClientLoopAction::ForwardMouseToPane {
            pane_id: "%1".to_string(),
            input: b"\x1b[<2;12;5M".to_vec(),
        }
    );
    assert_eq!(
        route_client_input(b"\x1b[<65;12;5M", &mouse_config).unwrap(),
        TerminalClientLoopAction::ForwardMouseToPane {
            pane_id: "%1".to_string(),
            input: b"\x1b[<65;12;5M".to_vec(),
        }
    );
    mouse_config.mouse_policy.pane_resize_active = true;
    assert_eq!(
        route_client_input(b"\x1b[<32;20;5M", &mouse_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane { column: 19, row: 4 })
    );
    assert_eq!(
        route_client_input(b"\x1b[<0;20;5m", &mouse_config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::FinishResizePane)
    );
}

/// Verifies that a previously entered prefix key state routes the next key
/// through the prefix table instead of opening the command prompt immediately.
///
/// This protects the split between the escape key and the command-prompt
/// binding so callers can keep the prefix state across terminal read frames.
#[test]
fn client_loop_routes_pending_prefix_key_to_prefix_table() {
    let config = TerminalClientLoopConfig {
        prefix_key_pending: true,
        ..TerminalClientLoopConfig::default()
    };

    assert_eq!(
        route_client_input(b":", &config).unwrap(),
        TerminalClientLoopAction::ExecuteMux(MuxAction::EnterCommandPrompt)
    );
}

/// Verifies that pending prefix state is consumed once and remaining bytes keep
/// their normal pane-forwarding behavior.
///
/// This regression scenario covers attached terminals that deliver the key
/// after the escape and pane text in the same read buffer.
#[test]
fn client_loop_consumes_pending_prefix_before_forwarding_remainder() {
    let config = TerminalClientLoopConfig {
        prefix_key_pending: true,
        ..TerminalClientLoopConfig::default()
    };

    assert_eq!(
        route_client_input_actions(b"cabc", &config).unwrap(),
        vec![
            TerminalClientLoopAction::ExecuteMux(MuxAction::NewWindow),
            TerminalClientLoopAction::ForwardToPane(b"abc".to_vec()),
        ]
    );
}

/// Verifies that pane applications receive mouse input only inside their own
/// rendered content region. A mouse-aware program in one pane must not suppress
/// Mezzanine history scrolling or selection in neighboring panes.
#[test]
fn client_loop_scopes_application_mouse_forwarding_to_pane_regions() {
    let mut config = TerminalClientLoopConfig {
        mouse_pane_regions: vec![
            MousePaneRegion {
                pane_id: "%1".to_string(),
                column: 0,
                row: 1,
                columns: 39,
                rows: 20,
                application_sgr_mouse_mode: true,
                application_mouse_mode: true,
                copy_mode_active: false,
                active: true,
            },
            MousePaneRegion {
                pane_id: "%2".to_string(),
                column: 40,
                row: 1,
                columns: 40,
                rows: 20,
                application_sgr_mouse_mode: false,
                application_mouse_mode: false,
                copy_mode_active: false,
                active: false,
            },
        ],
        ..TerminalClientLoopConfig::default()
    };

    assert_eq!(
        route_client_input(b"\x1b[<65;12;5M", &config).unwrap(),
        TerminalClientLoopAction::ForwardMouseToPane {
            pane_id: "%1".to_string(),
            input: b"\x1b[<65;12;4M".to_vec(),
        }
    );
    assert_eq!(
        route_client_input(b"\x1b[<65;50;5M", &config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ScrollHistory {
            lines: 3,
            position: CopyPosition {
                line: 4,
                column: 49,
            },
        })
    );

    config.mouse_border_cells = vec![MouseBorderCell { column: 39, row: 5 }];
    assert_eq!(
        route_client_input(b"\x1b[<0;40;6M", &config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane { column: 39, row: 5 })
    );
}

/// Verifies that the first button press in an unfocused mouse-aware pane is a
/// Mezzanine focus action instead of being forwarded to the previously focused
/// pane. After that focus update, later events in the same pane may be forwarded
/// to the pane application.
#[test]
fn client_loop_focuses_unfocused_mouse_region_before_forwarding() {
    let mut config = TerminalClientLoopConfig {
        mouse_pane_regions: vec![
            MousePaneRegion {
                pane_id: "%1".to_string(),
                column: 0,
                row: 1,
                columns: 39,
                rows: 20,
                application_sgr_mouse_mode: false,
                application_mouse_mode: false,
                copy_mode_active: false,
                active: true,
            },
            MousePaneRegion {
                pane_id: "%2".to_string(),
                column: 40,
                row: 1,
                columns: 40,
                rows: 20,
                application_sgr_mouse_mode: true,
                application_mouse_mode: true,
                copy_mode_active: false,
                active: false,
            },
        ],
        ..TerminalClientLoopConfig::default()
    };

    assert_eq!(
        route_client_input(b"\x1b[<0;50;5M", &config).unwrap(),
        TerminalClientLoopAction::HandleMouse(MouseAction::FocusPaneOnly(CopyPosition {
            line: 4,
            column: 49,
        }))
    );

    config.mouse_pane_regions[0].active = false;
    config.mouse_pane_regions[1].active = true;
    assert_eq!(
        route_client_input(b"\x1b[<0;50;5M", &config).unwrap(),
        TerminalClientLoopAction::ForwardMouseToPane {
            pane_id: "%2".to_string(),
            input: b"\x1b[<0;10;4M".to_vec(),
        }
    );
}

/// Verifies that pane applications using legacy xterm mouse tracking without
/// SGR mode still receive mouse input. Ncurses programs under `screen-256color`,
/// including htop, commonly request DECSET 1000 without DECSET 1006 and expect
/// `ESC [ M` encoded coordinates local to the pane.
#[test]
fn client_loop_translates_sgr_host_mouse_to_legacy_xterm_pane_mouse() {
    let config = TerminalClientLoopConfig {
        mouse_pane_regions: vec![MousePaneRegion {
            pane_id: "%2".to_string(),
            column: 40,
            row: 1,
            columns: 40,
            rows: 20,
            application_sgr_mouse_mode: false,
            application_mouse_mode: true,
            copy_mode_active: false,
            active: true,
        }],
        ..TerminalClientLoopConfig::default()
    };

    assert_eq!(
        route_client_input(b"\x1b[<65;50;5M", &config).unwrap(),
        TerminalClientLoopAction::ForwardMouseToPane {
            pane_id: "%2".to_string(),
            input: vec![b'\x1b', b'[', b'M', b'a', b'*', b'$'],
        }
    );
}

/// Verifies host bracketed paste payloads are forwarded without interpreting
/// bytes that look like Mezzanine prefix commands or mouse reports. Clipboard
/// paste data belongs to the pane application, and routing it as mux input can
/// turn a large paste into accidental commands.
#[test]
fn client_loop_forwards_host_bracketed_paste_as_opaque_input() {
    let config = TerminalClientLoopConfig::default();
    let mut paste_active = false;
    let input = b"\x1b[200~alpha\x01=beta\x1b[<0;12;5M\x1b[201~";

    let actions =
        route_client_input_actions_with_host_paste_state(input, &config, &mut paste_active)
            .unwrap();

    assert_eq!(
        actions,
        vec![TerminalClientLoopAction::ForwardToPane(input.to_vec())]
    );
    assert!(!paste_active);
}

/// Verifies host bracketed paste state survives across terminal read chunks.
/// Large clipboard pastes are read in bounded chunks; the chunks between the
/// start and end delimiters must remain opaque, while input after the closing
/// delimiter resumes normal mux-prefix parsing.
#[test]
fn client_loop_keeps_host_bracketed_paste_opaque_across_chunks() {
    let config = TerminalClientLoopConfig::default();
    let mut paste_active = false;
    let first = b"\x1b[200~alpha\x01";
    let second = b"=beta\x1b[201~\x01=";

    let first_actions =
        route_client_input_actions_with_host_paste_state(first, &config, &mut paste_active)
            .unwrap();
    assert_eq!(
        first_actions,
        vec![TerminalClientLoopAction::ForwardToPane(first.to_vec())]
    );
    assert!(paste_active);

    let second_actions =
        route_client_input_actions_with_host_paste_state(second, &config, &mut paste_active)
            .unwrap();

    assert_eq!(
        second_actions,
        vec![
            TerminalClientLoopAction::ForwardToPane(b"=beta\x1b[201~".to_vec()),
            TerminalClientLoopAction::ExecuteMux(MuxAction::PasteBuffer(
                PasteBufferTarget::ChooseInteractively,
            )),
        ]
    );
    assert!(!paste_active);
}

/// Verifies a large host bracketed paste stays opaque over many bounded
/// terminal-read chunks. This protects full-screen editor pastes where
/// transcript-sized clipboard contents may contain text that resembles
/// Mezzanine prefix commands or SGR mouse packets.
#[test]
fn client_loop_keeps_large_host_bracketed_paste_opaque_across_many_chunks() {
    let config = TerminalClientLoopConfig::default();
    let mut paste_active = false;
    let mut input = b"\x1b[200~".to_vec();
    input.extend(
        "prompt \u{e0b0} agent trace line\n"
            .repeat(18_000)
            .as_bytes(),
    );
    input.extend_from_slice(b"\x01=not-a-mux-command\n\x1b[<0;12;5Mnot-mouse\n");
    input.extend_from_slice(b"\x1b[201~\x01=");

    let mut forwarded = Vec::new();
    let mut mux_actions = Vec::new();
    for chunk in input.chunks(4096) {
        for action in
            route_client_input_actions_with_host_paste_state(chunk, &config, &mut paste_active)
                .unwrap()
        {
            match action {
                TerminalClientLoopAction::ForwardToPane(bytes) => forwarded.extend(bytes),
                other => mux_actions.push(other),
            }
        }
    }

    assert_eq!(forwarded, input[..input.len().saturating_sub(2)]);
    assert_eq!(
        mux_actions,
        vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::PasteBuffer(PasteBufferTarget::ChooseInteractively,)
        )]
    );
    assert!(!paste_active);
}

/// Verifies buffered host bracketed paste routing waits for the closing
/// delimiter before forwarding large paste content. This prevents typed input
/// after a slow terminal paste from overtaking an incomplete shell heredoc body.
#[test]
fn client_loop_buffers_incomplete_host_bracketed_paste_until_close() {
    let config = TerminalClientLoopConfig::default();
    let mut paste_active = false;
    let mut paste_buffer = Vec::new();
    let first = b"\x1b[200~cat <<'EOF'\nbody";
    let second = b"\nEOF\n\x1b[201~\x01=";

    let first_actions = route_client_input_actions_with_host_paste_buffer(
        first,
        &config,
        &mut paste_active,
        &mut paste_buffer,
    )
    .unwrap();
    assert!(first_actions.is_empty());
    assert!(paste_active);
    assert_eq!(paste_buffer, first);

    let second_actions = route_client_input_actions_with_host_paste_buffer(
        second,
        &config,
        &mut paste_active,
        &mut paste_buffer,
    )
    .unwrap();
    let mut expected_paste = first.to_vec();
    expected_paste.extend_from_slice(b"\nEOF\n\x1b[201~");
    assert_eq!(
        second_actions,
        vec![
            TerminalClientLoopAction::ForwardToPane(expected_paste),
            TerminalClientLoopAction::ExecuteMux(MuxAction::PasteBuffer(
                PasteBufferTarget::ChooseInteractively,
            )),
        ]
    );
    assert!(!paste_active);
    assert!(paste_buffer.is_empty());
}

/// Verifies that a single terminal read containing multiple SGR mouse packets is
/// split into separate mux actions instead of being forwarded as pane input. Drag
/// reporting commonly arrives batched, and forwarding a malformed aggregate
/// sequence would print mouse escape bytes into the active shell.
#[test]
fn attached_terminal_client_step_splits_batched_mouse_sequences() {
    let config = TerminalClientLoopConfig {
        mouse_border_cells: vec![MouseBorderCell { column: 11, row: 4 }],
        ..TerminalClientLoopConfig::default()
    };
    let readiness = vec![AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Input,
        fd: 0,
        interest: TerminalFdInterest::read(),
        readable: true,
        writable: false,
        hangup: false,
        error: false,
    }];

    let step = plan_attached_terminal_client_step(
        &readiness,
        Some(b"\x1b[<0;12;5M\x1b[<32;20;5M\x1b[<0;20;5m"),
        None,
        None,
        &config,
    )
    .unwrap();

    assert_eq!(
        step.actions,
        vec![
            TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane { column: 11, row: 4 }),
            TerminalClientLoopAction::HandleMouse(MouseAction::ResizePane { column: 19, row: 4 }),
            TerminalClientLoopAction::HandleMouse(MouseAction::FinishResizePane),
        ]
    );
}

/// Verifies that a drag selection keeps ownership after it crosses a rendered
/// pane border. Batched mouse reads must classify the border cell as a copy
/// update rather than starting a resize once the initial pane click has armed a
/// selection gesture.
#[test]
fn attached_terminal_client_step_keeps_selection_active_across_borders() {
    let config = TerminalClientLoopConfig {
        mouse_border_cells: vec![MouseBorderCell { column: 11, row: 4 }],
        ..TerminalClientLoopConfig::default()
    };
    let readiness = vec![AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Input,
        fd: 0,
        interest: TerminalFdInterest::read(),
        readable: true,
        writable: false,
        hangup: false,
        error: false,
    }];

    let step = plan_attached_terminal_client_step(
        &readiness,
        Some(b"\x1b[<0;2;3M\x1b[<32;12;5M\x1b[<0;12;5m"),
        None,
        None,
        &config,
    )
    .unwrap();

    assert_eq!(
        step.actions,
        vec![
            TerminalClientLoopAction::HandleMouse(MouseAction::FocusPane(CopyPosition {
                line: 2,
                column: 1,
            })),
            TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionUpdate(CopyPosition {
                line: 4,
                column: 11,
            },)),
            TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionFinish(CopyPosition {
                line: 4,
                column: 11,
            },)),
        ]
    );
}

/// Verifies that holding a drag selection beyond a pane edge keeps producing
/// selection-update actions even when the host terminal has no new mouse packet.
/// Runtime uses this synthetic update to keep pane history autoscrolling until
/// the pointer returns inside the pane or the button is released.
#[test]
fn attached_terminal_client_step_continues_selection_autoscroll_without_input() {
    let config = TerminalClientLoopConfig {
        mouse_selection_autoscroll_position: Some(CopyPosition { line: 0, column: 3 }),
        ..TerminalClientLoopConfig::default()
    };
    let readiness = vec![AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Output,
        fd: 1,
        interest: TerminalFdInterest::write(),
        readable: false,
        writable: true,
        hangup: false,
        error: false,
    }];

    let step = plan_attached_terminal_client_step(&readiness, None, None, None, &config).unwrap();

    assert_eq!(
        step.actions,
        vec![TerminalClientLoopAction::HandleMouse(
            MouseAction::CopySelectionUpdate(CopyPosition { line: 0, column: 3 })
        )]
    );
}

/// Verifies attached terminal client step routes input and composes output.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_client_step_routes_input_and_composes_output() {
    let config = TerminalClientLoopConfig::default();
    let view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(12, 3).unwrap(),
        client_size: Size::new(12, 3).unwrap(),
        lines: vec![
            "one         ".to_string(),
            "two         ".to_string(),
            "three       ".to_string(),
        ],
        line_style_spans: vec![Vec::new(), Vec::new(), Vec::new()],
        requires_client_scroll: false,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 1,
        cursor_column: 2,
        cursor_visible: true,
        cursor_style: TerminalCursorStyle::Block,
        cursor_blink: true,
        cursor_blink_interval_ms: 500,
        application_keypad: false,
        bracketed_paste: false,
        animation_refresh_interval_ms: 0,
        ui_theme: UiTheme::default(),
        agent_prompt_region: None,
        primary_prompt_active: false,
    };
    let readiness = vec![
        AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: true,
            writable: false,
            hangup: false,
            error: false,
        },
        AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Output,
            fd: 1,
            interest: TerminalFdInterest::write(),
            readable: false,
            writable: true,
            hangup: false,
            error: false,
        },
    ];
    let status = ClientStatusLine {
        kind: ClientStatusKind::Plain,
        text: "ready".to_string(),
    };

    let plan = plan_attached_terminal_client_step(
        &readiness,
        Some(b"\x01%"),
        Some(&view),
        Some(&status),
        &config,
    )
    .unwrap();

    assert_eq!(
        plan.actions,
        vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::SplitPaneVertical
        )]
    );
    assert_eq!(plan.output_lines.len(), 3);
    assert_eq!(plan.output_lines[2], "ready       ");
    assert!(!plan.input_hangup);
    assert!(plan.error_roles.is_empty());
}

/// Verifies that actor-owned prompt overlays receive raw key bytes before
/// normal mux key classification. This preserves readline semantics for keys
/// such as the configured prefix while the command prompt is active.
#[test]
fn attached_terminal_client_step_forwards_raw_input_when_primary_prompt_is_active() {
    let config = TerminalClientLoopConfig::default();
    let view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(12, 3).unwrap(),
        client_size: Size::new(12, 3).unwrap(),
        lines: vec![
            "one         ".to_string(),
            "two         ".to_string(),
            "▐ :         ".to_string(),
        ],
        line_style_spans: vec![Vec::new(), Vec::new(), Vec::new()],
        requires_client_scroll: false,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 2,
        cursor_column: 3,
        cursor_visible: true,
        cursor_style: TerminalCursorStyle::Block,
        cursor_blink: true,
        cursor_blink_interval_ms: 500,
        application_keypad: false,
        bracketed_paste: false,
        animation_refresh_interval_ms: 0,
        ui_theme: UiTheme::default(),
        agent_prompt_region: None,
        primary_prompt_active: true,
    };
    let readiness = vec![AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Input,
        fd: 0,
        interest: TerminalFdInterest::read(),
        readable: true,
        writable: false,
        hangup: false,
        error: false,
    }];

    let plan =
        plan_attached_terminal_client_step(&readiness, Some(b"\x01:"), Some(&view), None, &config)
            .unwrap();

    assert_eq!(
        plan.actions,
        vec![TerminalClientLoopAction::ForwardToPane(b"\x01:".to_vec())]
    );
}

/// Verifies attached terminal client step routes batched prefix command prompt.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
/// Verifies that batched prefix-command bytes still open the command prompt.
/// Detached control-socket attach can read `Ctrl+A : Enter` in one buffer, and
/// the prefix command trigger must not be reported as an unbound prefix just
/// because a following byte arrived before the prompt loop starts.
fn attached_terminal_client_step_routes_batched_prefix_command_prompt() {
    let config = TerminalClientLoopConfig::default();
    let readiness = vec![AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Input,
        fd: 0,
        interest: TerminalFdInterest::read(),
        readable: true,
        writable: false,
        hangup: false,
        error: false,
    }];

    let plan =
        plan_attached_terminal_client_step(&readiness, Some(b"\x01:\r"), None, None, &config)
            .unwrap();

    assert_eq!(
        plan.actions,
        vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::EnterCommandPrompt
        )]
    );
}

/// Verifies attached terminal client step reports hangups and errors without output.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_client_step_reports_hangups_and_errors_without_output() {
    let config = TerminalClientLoopConfig::default();
    let readiness = vec![
        AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: false,
            writable: false,
            hangup: true,
            error: false,
        },
        AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Output,
            fd: 1,
            interest: TerminalFdInterest::write(),
            readable: false,
            writable: false,
            hangup: false,
            error: true,
        },
    ];

    let plan =
        plan_attached_terminal_client_step(&readiness, Some(b"ignored"), None, None, &config)
            .unwrap();

    assert!(plan.actions.is_empty());
    assert!(plan.output_lines.is_empty());
    assert!(plan.input_hangup);
    assert!(!plan.output_hangup);
    assert_eq!(plan.error_roles, vec![AttachedTerminalFdRole::Output]);
}

/// Carries Fake Attached Terminal Loop Io state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default)]
struct FakeAttachedTerminalLoopIo {
    /// Stores the readiness batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    readiness_batches: Vec<Vec<AttachedTerminalFdReadiness>>,
    /// Stores the input batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    input_batches: Vec<Vec<u8>>,
    /// Stores the written batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    written_batches: Vec<Vec<String>>,
}

impl AttachedTerminalClientLoopIo for FakeAttachedTerminalLoopIo {
    /// Runs the poll readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_readiness(&mut self) -> Result<Vec<AttachedTerminalFdReadiness>> {
        if self.readiness_batches.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.readiness_batches.remove(0))
    }

    /// Runs the read input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_input(&mut self, max_bytes: usize) -> Result<Vec<u8>> {
        if self.input_batches.is_empty() {
            return Ok(Vec::new());
        }
        let mut input = self.input_batches.remove(0);
        input.truncate(max_bytes);
        Ok(input)
    }

    /// Runs the write output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_output(&mut self, lines: &[String]) -> Result<usize> {
        self.written_batches.push(lines.to_vec());
        Ok(lines.iter().map(String::len).sum())
    }
}

/// Verifies attached terminal client loop pumps input output and stops on hangup.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_client_loop_pumps_input_output_and_stops_on_hangup() {
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![
            vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: true,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ],
            vec![AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: false,
                writable: false,
                hangup: true,
                error: false,
            }],
        ],
        input_batches: vec![b"\x01c".to_vec()],
        written_batches: Vec::new(),
    };
    let view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(10, 2).unwrap(),
        client_size: Size::new(10, 2).unwrap(),
        lines: vec!["pane      ".to_string(), "old       ".to_string()],
        line_style_spans: vec![Vec::new(), Vec::new()],
        requires_client_scroll: false,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 0,
        cursor_column: 0,
        cursor_visible: true,
        cursor_style: TerminalCursorStyle::Block,
        cursor_blink: true,
        cursor_blink_interval_ms: 500,
        application_keypad: false,
        bracketed_paste: false,
        animation_refresh_interval_ms: 0,
        ui_theme: UiTheme::default(),
        agent_prompt_region: None,
        primary_prompt_active: false,
    };
    let status = ClientStatusLine {
        kind: ClientStatusKind::Plain,
        text: "ready".to_string(),
    };

    let report = run_attached_terminal_client_loop(
        &mut io,
        || Ok(Some((view.clone(), Some(status.clone())))),
        &TerminalClientLoopConfig::default(),
        AttachedTerminalClientLoopConfig {
            max_iterations: 4,
            max_input_bytes: 64,
        },
    )
    .unwrap();

    assert_eq!(report.iterations, 2);
    assert_eq!(
        report.actions,
        vec![TerminalClientLoopAction::ExecuteMux(MuxAction::NewWindow)]
    );
    assert_eq!(report.output_frames, 1);
    assert_eq!(io.written_batches.len(), 1);
    assert_eq!(io.written_batches[0][1], "ready     ");
    assert_eq!(report.input_hangups, 1);
    assert!(report.error_roles.is_empty());
}

/// Verifies attached terminal client loop rejects zero limits.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_client_loop_rejects_zero_limits() {
    let mut io = FakeAttachedTerminalLoopIo::default();
    let error = run_attached_terminal_client_loop(
        &mut io,
        || Ok(None),
        &TerminalClientLoopConfig::default(),
        AttachedTerminalClientLoopConfig {
            max_iterations: 0,
            max_input_bytes: 1,
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies the default attached-client loop can read a large foreground paste
/// as one logical terminal input event. This keeps clipboard paste throughput
/// high enough that ordinary shell/editor pastes are not truncated by a small
/// harness read ceiling.
#[test]
fn attached_terminal_client_loop_default_limits_allow_large_paste_reads() {
    let config = AttachedTerminalClientLoopConfig::default();

    assert!(config.max_iterations >= 128);
    assert!(config.max_input_bytes >= 1024 * 1024);
}

/// Verifies attached terminal fd loop io reads and writes unix fds.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_fd_loop_io_reads_and_writes_unix_fds() {
    let (mut input_writer, input_reader) = UnixStream::pair().unwrap();
    let (output_writer, mut output_reader) = UnixStream::pair().unwrap();
    input_writer.write_all(b"\x01c").unwrap();
    output_reader
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let mut io = AttachedTerminalFdLoopIo::new(
        input_reader.as_raw_fd(),
        output_writer.as_raw_fd(),
        None,
        Some(Duration::ZERO),
    )
    .unwrap();
    let view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(8, 2).unwrap(),
        client_size: Size::new(8, 2).unwrap(),
        lines: vec!["pane    ".to_string(), "        ".to_string()],
        line_style_spans: vec![Vec::new(), Vec::new()],
        requires_client_scroll: false,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 0,
        cursor_column: 0,
        cursor_visible: true,
        cursor_style: TerminalCursorStyle::Block,
        cursor_blink: true,
        cursor_blink_interval_ms: 500,
        application_keypad: false,
        bracketed_paste: false,
        animation_refresh_interval_ms: 0,
        ui_theme: UiTheme::default(),
        agent_prompt_region: None,
        primary_prompt_active: false,
    };

    let report = run_attached_terminal_client_loop(
        &mut io,
        || Ok(Some((view.clone(), None))),
        &TerminalClientLoopConfig::default(),
        AttachedTerminalClientLoopConfig {
            max_iterations: 1,
            max_input_bytes: 64,
        },
    )
    .unwrap();
    let mut output = [0u8; 128];
    let output_len = output_reader.read(&mut output).unwrap();
    let rendered = String::from_utf8_lossy(&output[..output_len]);

    assert_eq!(
        report.actions,
        vec![TerminalClientLoopAction::ExecuteMux(MuxAction::NewWindow)]
    );
    assert_eq!(report.output_frames, 1);
    assert!(report.bytes_written > 0);
    assert!(rendered.starts_with(
        "\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?2004l\x1b[2J\x1b[H"
    ));
    assert!(rendered.contains("pane"));
    assert!(rendered.ends_with("\x1b[?25l\x1b[2 q\x1b[1;1H\x1b[?25h"));
}

/// Verifies that attached TTY output writability is sampled after the blocking
/// input poll rather than making the client loop wake immediately while idle.
/// Terminal output fds are usually writable, so including them in the blocking
/// poll turns the renderer into a fixed-rate busy loop.
#[test]
fn attached_terminal_fd_loop_io_blocks_until_input_poll_timeout_when_output_is_writable() {
    let (_input_writer, input_reader) = UnixStream::pair().unwrap();
    let (output_writer, _output_reader) = UnixStream::pair().unwrap();
    let mut io = AttachedTerminalFdLoopIo::new(
        input_reader.as_raw_fd(),
        output_writer.as_raw_fd(),
        None,
        Some(Duration::from_millis(25)),
    )
    .unwrap();

    let started = std::time::Instant::now();
    let readiness = io.poll_readiness().unwrap();

    assert!(started.elapsed() >= Duration::from_millis(10));
    assert!(readiness.iter().any(|ready| {
        ready.role == AttachedTerminalFdRole::Output && ready.writable && ready.is_ready()
    }));
    assert!(
        readiness
            .iter()
            .all(|ready| ready.role != AttachedTerminalFdRole::Input || !ready.readable)
    );
}

/// Verifies that attached-terminal frames suppress the host cursor, reset
/// coordinate-affecting terminal modes, enable host mouse reporting, clear
/// stale viewport cells, and restore a configured Mezzanine cursor at the
/// requested active-surface position.
#[test]
fn attached_terminal_output_frame_controls_cursor_presentation() {
    let frame = encode_attached_terminal_output_frame_with_styles(
        &[
            "pane    ".to_string(),
            "body    ".to_string(),
            "status  ".to_string(),
        ],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_style: TerminalCursorStyle::Underline,
            cursor_blink: false,
            cursor_visible: true,
            cursor_row: 2,
            cursor_column: 3,
            ..AttachedTerminalOutputModes::default()
        },
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(rendered.starts_with(
        "\x1b[?25l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[?1000;1002;1006h\x1b[?2004l\x1b[2J\x1b[H"
    ));
    assert!(rendered.contains("pane"));
    assert!(rendered.ends_with("\x1b[?25l\x1b[4 q\x1b[3;4H\x1b[?25h"));
}

/// Verifies attached-terminal cursor presentation clamps to the rendered frame
/// bounds before emitting one-based terminal coordinates. A visible cursor at the
/// internal end-of-row insertion point must not become column `width + 1`, since
/// terminals can wrap or clamp that coordinate differently.
#[test]
fn attached_terminal_output_frame_clamps_visible_cursor_to_rendered_bounds() {
    let frame = encode_attached_terminal_output_frame_with_styles(
        &["abcde".to_string(), "vwxyz".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: true,
            cursor_blink: false,
            cursor_row: 9,
            cursor_column: 5,
            ..AttachedTerminalOutputModes::default()
        },
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(
        rendered.ends_with("\x1b[?25l\x1b[2 q\x1b[2;5H\x1b[?25h"),
        "{rendered:?}"
    );
    assert!(!rendered.contains("\x1b[10;6H"), "{rendered:?}");
}

/// Verifies attached-terminal redraws place the cursor at the screen-model
/// insertion point even after high Private Use prompt glyphs. Font-specific
/// width guesses can put the visible cursor one column away from the next
/// echoed character, so presentation frames must not add a separate glyph-width
/// correction over the terminal screen cursor.
#[test]
fn attached_terminal_output_frame_uses_screen_cursor_after_patched_font_prompt_glyph() {
    let frame = encode_attached_terminal_output_frame_with_styles(
        &["\u{f432}       ".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: true,
            cursor_blink: false,
            cursor_row: 0,
            cursor_column: 1,
            ..AttachedTerminalOutputModes::default()
        },
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(
        rendered.ends_with("\x1b[?25l\x1b[2 q\x1b[1;2H\x1b[?25h"),
        "{rendered:?}"
    );
}

/// Verifies that Mezzanine-owned cursor blink timing hides the cursor during
/// the off phase instead of relying on terminal-emulator blink rates.
#[test]
fn attached_terminal_output_frame_honors_cursor_blink_interval_phase() {
    let frame = encode_attached_terminal_output_frame_with_styles(
        &["pane".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: true,
            cursor_blink: true,
            cursor_blink_interval_ms: 500,
            cursor_blink_elapsed_ms: 250,
            ..AttachedTerminalOutputModes::default()
        },
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(rendered.ends_with("\x1b[?25l"), "{rendered:?}");
}

/// Verifies that stable-size attached-terminal redraws are encoded as row
/// updates instead of clearing the full viewport. This reduces foreground TTY
/// flicker while still allowing the first draw and resizes to invalidate the
/// whole surface. Changed rows are already full-width, so the update must not
/// append erase-to-end-of-line after the row text because that can clear a
/// freshly drawn final-column cell while host autowrap is pending.
#[test]
fn attached_terminal_output_update_redraws_only_changed_rows() {
    let previous_lines = vec!["one    ".to_string(), "two    ".to_string()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &["one    ".to_string(), "changed".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: true,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.starts_with("\x1b[?25l"), "{rendered:?}");
    assert!(rendered.contains("\x1b[2;1Hchanged"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[K"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[1;1Hone"), "{rendered:?}");
}

/// Verifies that same-width printable ASCII row changes can update only the
/// changed span instead of rewriting the whole row. This keeps frequent status
/// or prompt edits small on slower terminal links while preserving the existing
/// row-diff contract for unsafe text.
#[test]
fn attached_terminal_output_update_uses_changed_ascii_span_when_safe() {
    let previous_lines = vec!["aaaaaaaaaa".to_string()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &["aaaabaaaaa".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.contains("\x1b[1;5Hb"), "{rendered:?}");
    assert!(!rendered.contains("aaaabaaaaa"), "{rendered:?}");
}

/// Verifies multi-row viewport changes rewrite whole changed rows instead of
/// issuing one tiny span update per row. Scrollback and copy-mode paging often
/// change most visible rows at once; full-row diff updates keep each rendered
/// row self-contained without falling back to a full-screen clear.
#[test]
fn attached_terminal_output_update_rewrites_full_rows_for_multi_row_changes() {
    let previous_lines = vec!["row 001".to_string(), "row 002".to_string()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &["row 101".to_string(), "row 102".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.contains("\x1b[1;1Hrow 101"), "{rendered:?}");
    assert!(rendered.contains("\x1b[2;1Hrow 102"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[1;5H1"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[2;5H1"), "{rendered:?}");
}

/// Verifies that non-ASCII row changes keep the full-row rewrite path. Wide
/// glyph byte offsets and display columns do not have a one-to-one mapping, so
/// bounded span updates must avoid those rows rather than risking cursor
/// placement inside a multi-byte character.
#[test]
fn attached_terminal_output_update_rewrites_wide_glyph_rows() {
    let previous_lines = vec!["aa✔aa".to_string()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &["aa✘aa".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: false,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.contains("\x1b[1;1H"), "{rendered:?}");
    assert!(rendered.contains("aa✘aa"), "{rendered:?}");
}

/// Verifies stable-row attached-terminal updates clear only rows that shrink
/// instead of falling back to a full-screen redraw. This avoids stale trailing
/// cells over remote terminal links while keeping the update bounded to the
/// changed row.
#[test]
fn attached_terminal_output_update_clears_shrinking_rows_without_full_redraw() {
    let previous_lines = vec!["wide text".to_string(), "steady".to_string()];
    let previous = AttachedTerminalOutputFrameState::new(&previous_lines, &[]);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &["short".to_string(), "steady".to_string()],
        &[],
        None,
        AttachedTerminalOutputModes {
            cursor_visible: true,
            cursor_blink: false,
            ..AttachedTerminalOutputModes::default()
        },
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(rendered.contains("\x1b[1;1H\x1b[2Kshort"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[2;1Hsteady"), "{rendered:?}");
}

/// Verifies stable-size attached-terminal updates avoid sending any bytes when
/// the rendered rows, style spans, bracketed-paste mode, and cursor
/// presentation are unchanged. This keeps idle status refreshes cheap over
/// higher-latency terminal links.
#[test]
fn attached_terminal_output_update_omits_unchanged_frame_bytes() {
    let lines = vec!["one    ".to_string(), "two    ".to_string()];
    let modes = AttachedTerminalOutputModes {
        cursor_visible: true,
        cursor_blink: false,
        cursor_row: 0,
        cursor_column: 0,
        ..AttachedTerminalOutputModes::default()
    };
    let previous = AttachedTerminalOutputFrameState::new_with_modes(&lines, &[], modes);

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &lines,
        &[],
        None,
        modes,
        Some(&previous),
    );

    assert!(frame.is_empty(), "{:?}", String::from_utf8_lossy(&frame));
}

/// Verifies stable-size attached-terminal updates emit only cursor bytes when
/// the visible content is unchanged and the cursor moves. Row-differential
/// updates should not resend static presentation setup or host bracketed-paste
/// mode just to reposition the cursor.
#[test]
fn attached_terminal_output_update_uses_cursor_only_frame_for_cursor_moves() {
    let lines = vec!["one    ".to_string(), "two    ".to_string()];
    let previous_modes = AttachedTerminalOutputModes {
        cursor_visible: true,
        cursor_blink: false,
        cursor_row: 0,
        cursor_column: 0,
        ..AttachedTerminalOutputModes::default()
    };
    let previous = AttachedTerminalOutputFrameState::new_with_modes(&lines, &[], previous_modes);
    let next_modes = AttachedTerminalOutputModes {
        cursor_column: 1,
        ..previous_modes
    };

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &lines,
        &[],
        None,
        next_modes,
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert!(!rendered.contains("\x1b[2J"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[?1000;1002;1006h"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[?2004"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[1;1Hone"), "{rendered:?}");
    assert_eq!(rendered, "\x1b[?25l\x1b[2 q\x1b[1;2H\x1b[?25h");
}

/// Verifies stable-size attached-terminal updates emit bracketed-paste mode
/// changes without resending the rest of the static presentation prologue.
#[test]
fn attached_terminal_output_update_emits_only_changed_bracketed_paste_mode() {
    let lines = vec!["one    ".to_string(), "two    ".to_string()];
    let previous = AttachedTerminalOutputFrameState::new_with_modes(
        &lines,
        &[],
        AttachedTerminalOutputModes::default(),
    );
    let next_modes = AttachedTerminalOutputModes {
        bracketed_paste: true,
        ..AttachedTerminalOutputModes::default()
    };

    let frame = encode_attached_terminal_output_update_frame_with_styles(
        &lines,
        &[],
        None,
        next_modes,
        Some(&previous),
    );
    let rendered = String::from_utf8(frame).unwrap();

    assert_eq!(rendered, "\x1b[?2004h");
}

/// Verifies that the presentation restore sequence disables Mezzanine-owned
/// mouse capture, resets coordinate-affecting terminal modes, clears
/// Mezzanine's drawn viewport, makes the host cursor visible again, and resets
/// cursor style after foreground detachment.
#[test]
fn attached_terminal_restore_frame_restores_cursor_visibility() {
    let restore = String::from_utf8(
        super::client_loop::attached_terminal_restore_presentation_frame().to_vec(),
    )
    .unwrap();

    assert_eq!(
        restore,
        "\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[2J\x1b[H\x1b[?25h\x1b[0 q"
    );
}

/// Verifies attached terminal fd rejects negative fd and empty interest.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_fd_rejects_negative_fd_and_empty_interest() {
    assert_eq!(
        AttachedTerminalFd::input(-1, TerminalFdInterest::read())
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
    assert_eq!(
        AttachedTerminalFd::output(1, TerminalFdInterest::default())
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
}

/// Verifies terminal raw mode rejects invalid fd before termios calls.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_raw_mode_rejects_invalid_fd_before_termios_calls() {
    let error = TerminalRawModeGuard::enable(-1).unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies attached terminal readiness reports readable input.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_reports_readable_input() {
    let (mut writer, reader) = UnixStream::pair().unwrap();
    writer.write_all(b"x").unwrap();
    let descriptor =
        AttachedTerminalFd::input(reader.as_raw_fd(), TerminalFdInterest::read()).unwrap();

    let readiness =
        poll_attached_terminal_fd_readiness(&[descriptor], Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 1);
    assert_eq!(readiness[0].role, AttachedTerminalFdRole::Input);
    assert_eq!(readiness[0].fd, reader.as_raw_fd());
    assert!(readiness[0].readable);
    assert!(!readiness[0].writable);
    assert!(!readiness[0].hangup);
    assert!(!readiness[0].error);
    assert!(readiness[0].is_ready());
}

/// Verifies attached terminal readiness reports writable output.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_reports_writable_output() {
    let (stream, _peer) = UnixStream::pair().unwrap();
    let descriptor =
        AttachedTerminalFd::output(stream.as_raw_fd(), TerminalFdInterest::write()).unwrap();

    let readiness =
        poll_attached_terminal_fd_readiness(&[descriptor], Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 1);
    assert_eq!(readiness[0].role, AttachedTerminalFdRole::Output);
    assert!(readiness[0].writable);
    assert!(!readiness[0].readable);
    assert!(!readiness[0].hangup);
    assert!(!readiness[0].error);
}

/// Verifies attached terminal readiness preserves control fd order.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_preserves_control_fd_order() {
    let (mut writer, input) = UnixStream::pair().unwrap();
    let (control, _control_peer) = UnixStream::pair().unwrap();
    writer.write_all(b"x").unwrap();
    let descriptors = [
        AttachedTerminalFd::control(control.as_raw_fd(), TerminalFdInterest::write()).unwrap(),
        AttachedTerminalFd::input(input.as_raw_fd(), TerminalFdInterest::read()).unwrap(),
    ];

    let readiness =
        poll_attached_terminal_fd_readiness(&descriptors, Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 2);
    assert_eq!(readiness[0].role, AttachedTerminalFdRole::Control);
    assert_eq!(readiness[0].interest, TerminalFdInterest::write());
    assert!(readiness[0].writable);
    assert_eq!(readiness[1].role, AttachedTerminalFdRole::Input);
    assert_eq!(readiness[1].interest, TerminalFdInterest::read());
    assert!(readiness[1].readable);
}

/// Verifies attached terminal readiness timeout returns not ready.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_timeout_returns_not_ready() {
    let (stream, _peer) = UnixStream::pair().unwrap();
    let descriptor =
        AttachedTerminalFd::input(stream.as_raw_fd(), TerminalFdInterest::read()).unwrap();

    let readiness =
        poll_attached_terminal_fd_readiness(&[descriptor], Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 1);
    assert!(!readiness[0].is_ready());
}

/// Verifies attached terminal readiness reports hangup.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_reports_hangup() {
    let (stream, peer) = UnixStream::pair().unwrap();
    drop(peer);
    let descriptor =
        AttachedTerminalFd::input(stream.as_raw_fd(), TerminalFdInterest::read()).unwrap();

    let readiness =
        poll_attached_terminal_fd_readiness(&[descriptor], Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 1);
    assert!(readiness[0].hangup);
    assert!(!readiness[0].error);
}

/// Verifies attached terminal readiness reports pipe error.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_reports_pipe_error() {
    let (read_end, write_end) = pipe_pair().unwrap();
    drop(read_end);
    let descriptor =
        AttachedTerminalFd::output(write_end.as_raw_fd(), TerminalFdInterest::write()).unwrap();

    let readiness =
        poll_attached_terminal_fd_readiness(&[descriptor], Some(Duration::ZERO)).unwrap();

    assert_eq!(readiness.len(), 1);
    assert!(readiness[0].error);
}

/// Verifies attached terminal readiness rejects invalid fd.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_rejects_invalid_fd() {
    let error = AttachedTerminalFd::control(-1, TerminalFdInterest::read()).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies attached terminal readiness timeout conversion preserves precision.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attached_terminal_readiness_timeout_conversion_preserves_precision() {
    let zero = duration_to_timespec(Duration::ZERO).unwrap();
    assert_eq!(zero.tv_sec, 0);
    assert_eq!(zero.tv_nsec, 0);

    let one_nano = duration_to_timespec(Duration::from_nanos(1)).unwrap();
    assert_eq!(one_nano.tv_sec, 0);
    assert_eq!(one_nano.tv_nsec, 1);

    let two_millis = duration_to_timespec(Duration::from_millis(2)).unwrap();
    assert_eq!(two_millis.tv_sec, 0);
    assert_eq!(two_millis.tv_nsec, 2_000_000);
}

/// Verifies client loop draws window from live pane screens.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn client_loop_draws_window_from_live_pane_screens() {
    let mut ids = crate::ids::IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(20, 4).unwrap());
    window
        .split_active(&mut ids, crate::layout::SplitDirection::Vertical)
        .unwrap();
    let mut screens = BTreeMap::new();
    let body_size = |size: Size| Size::new(size.columns, size.rows - 1).unwrap();
    let mut left = TerminalScreen::new(body_size(window.panes()[0].size), 10).unwrap();
    left.feed(b"left");
    let mut right = TerminalScreen::new(body_size(window.panes()[1].size), 10).unwrap();
    right.feed(b"right");
    screens.insert(window.panes()[0].id.to_string(), left);
    screens.insert(window.panes()[1].id.to_string(), right);

    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };
    let rendered = draw_window_from_screens(&window, &screens, &config).unwrap();
    let joined = rendered.join("\n");

    assert_eq!(rendered.len(), 4);
    assert!(joined.contains("left"));
    assert!(joined.contains("right"));
}

/// Verifies left panes reserve the shared divider column when an even vertical
/// split creates a right divider neighbor.
///
/// This regression covers the selected-agent prompt bug directly at the
/// render-region sizing boundary so later render changes cannot let content
/// overwrite the right-side divider.
#[test]
fn pane_render_region_reserves_right_divider_for_even_vertical_split() {
    let geometries = vec![
        PaneGeometry {
            index: 0,
            column: 0,
            row: 0,
            columns: 5,
            rows: 3,
        },
        PaneGeometry {
            index: 1,
            column: 5,
            row: 0,
            columns: 5,
            rows: 3,
        },
    ];

    assert_eq!(
        pane_render_region_size_for_geometry(&geometries[0], &geometries).unwrap(),
        Size::new(4, 3).unwrap()
    );
}

/// Verifies left panes reserve the shared divider column when an odd vertical
/// split leaves the left pane one column wider than its neighbor.
///
/// This regression protects the off-by-one case called out in the fix plan so
/// uneven split math cannot let agent-prompt text overwrite the divider.
#[test]
fn pane_render_region_reserves_right_divider_for_odd_vertical_split() {
    let geometries = vec![
        PaneGeometry {
            index: 0,
            column: 0,
            row: 0,
            columns: 6,
            rows: 3,
        },
        PaneGeometry {
            index: 1,
            column: 6,
            row: 0,
            columns: 5,
            rows: 3,
        },
    ];

    assert_eq!(
        pane_render_region_size_for_geometry(&geometries[0], &geometries).unwrap(),
        Size::new(5, 3).unwrap()
    );
}

/// Verifies client loop draws zoomed pane across window body.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn client_loop_draws_zoomed_pane_across_window_body() {
    let mut ids = crate::ids::IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(20, 4).unwrap());
    window
        .split_active(&mut ids, crate::layout::SplitDirection::Vertical)
        .unwrap();
    window.toggle_zoom_active();
    let mut screens = BTreeMap::new();
    let mut left = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();
    left.feed(b"left");
    let mut right = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();
    right.feed(b"right");
    screens.insert(window.panes()[0].id.to_string(), left);
    screens.insert(window.panes()[1].id.to_string(), right);

    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };
    let rendered = draw_window_from_screens(&window, &screens, &config).unwrap();
    let joined = rendered.join("\n");

    assert_eq!(rendered.len(), 4);
    assert!(joined.contains("right"));
    assert!(!joined.contains("left"));
    assert_eq!(UnicodeWidthStr::width(rendered[0].as_str()), 20);
}

/// Verifies that rendered client views carry visible screen SGR spans beside
/// their plain text lines. This keeps terminal/view consumers from needing
/// private screen access to observe colors and attributes.
#[test]
fn client_view_preserves_terminal_style_spans() {
    let mut ids = crate::ids::IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(8, 2).unwrap());
    let mut screen = TerminalScreen::new(Size::new(8, 2).unwrap(), 10).unwrap();
    screen.feed(b"\x1b[1;38;5;120mAB\x1b[0mC");
    let mut screens = BTreeMap::new();
    screens.insert(window.active_pane().id.to_string(), screen);
    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        Size::new(8, 2).unwrap(),
    )
    .unwrap()
    .unwrap();

    assert_eq!(view.lines[0], "ABC     ");
    assert_eq!(
        view.line_style_spans[0],
        vec![TerminalStyleSpan {
            start: 0,
            length: 2,
            rendition: GraphicRendition {
                bold: true,
                dim: false,
                italic: false,
                strikethrough: false,
                double_underline: false,
                hidden: false,
                underline: false,
                inverse: false,
                foreground: Some(TerminalColor::Indexed(120)),
                background: None,
            },
        }]
    );
}

/// Verifies that side-by-side rendering offsets style spans by each pane's
/// rendered width, so styled content from a later pane points at the correct
/// terminal-cell columns in the composed client view.
#[test]
fn client_view_offsets_style_spans_across_side_by_side_panes() {
    let mut ids = crate::ids::IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(8, 2).unwrap());
    window
        .split_active(&mut ids, crate::layout::SplitDirection::Vertical)
        .unwrap();
    let mut screens = BTreeMap::new();
    let mut left = TerminalScreen::new(window.panes()[0].size, 10).unwrap();
    left.feed(b"L");
    let mut right = TerminalScreen::new(window.panes()[1].size, 10).unwrap();
    right.feed(b"\x1b[7mR\x1b[0m");
    screens.insert(window.panes()[0].id.to_string(), left);
    screens.insert(window.panes()[1].id.to_string(), right);
    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        Size::new(8, 2).unwrap(),
    )
    .unwrap()
    .unwrap();

    assert_eq!(view.lines[0], "L  \u{2502}R   ");
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.start == 4
            && span.length == 1
            && span.rendition
                == GraphicRendition {
                    bold: false,
                    dim: false,
                    italic: false,
                    strikethrough: false,
                    double_underline: false,
                    hidden: false,
                    underline: false,
                    inverse: true,
                    foreground: None,
                    background: None,
                }
    }));
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.start == 3
            && span.length == 1
            && span.rendition.foreground == Some(TerminalColor::Rgb(0x7e, 0x9c, 0xd8))
            && span.rendition.background.is_none()
    }));
}

/// Verifies client view hides pending observers and keeps primary dimensions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn client_view_hides_pending_observers_and_keeps_primary_dimensions() {
    let mut ids = crate::ids::IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(20, 4).unwrap());
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();
    screen.feed(b"live\nviewport");
    let mut screens = BTreeMap::new();
    screens.insert(window.active_pane().id.to_string(), screen);
    let config = TerminalClientLoopConfig::default();

    let pending = render_attached_client_view(
        ClientViewRole::PendingObserver,
        &window,
        &screens,
        &config,
        Size::new(10, 2).unwrap(),
    )
    .unwrap();
    let observer = render_attached_client_view(
        ClientViewRole::Observer,
        &window,
        &screens,
        &config,
        Size::new(10, 2).unwrap(),
    )
    .unwrap()
    .unwrap();
    let primary = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        Size::new(20, 4).unwrap(),
    )
    .unwrap()
    .unwrap();

    assert!(pending.is_none());
    assert_eq!(observer.authoritative_size, Size::new(20, 4).unwrap());
    assert_eq!(observer.client_size, Size::new(10, 2).unwrap());
    assert!(observer.requires_client_scroll);
    assert_eq!(observer.lines.len(), 4);
    assert!(observer.lines.join("\n").contains("live"));
    assert!(!primary.requires_client_scroll);
}

/// Verifies observer client presentation uses local viewport offset.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn observer_client_presentation_uses_local_viewport_offset() {
    let mut view = RenderedClientView {
        role: ClientViewRole::Observer,
        authoritative_size: Size::new(8, 4).unwrap(),
        client_size: Size::new(4, 2).unwrap(),
        lines: vec![
            "abcd1234".to_string(),
            "efgh5678".to_string(),
            "ijkl9012".to_string(),
            "mnop3456".to_string(),
        ],
        line_style_spans: vec![Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        requires_client_scroll: true,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 0,
        cursor_column: 0,
        cursor_visible: false,
        cursor_style: TerminalCursorStyle::Block,
        cursor_blink: false,
        cursor_blink_interval_ms: 500,
        application_keypad: false,
        bracketed_paste: false,
        animation_refresh_interval_ms: 0,
        ui_theme: UiTheme::default(),
        agent_prompt_region: None,
        primary_prompt_active: false,
    };

    apply_client_view_offset(&mut view, 2, 4);
    assert_eq!(
        compose_client_presentation(&view, None),
        vec!["9012".to_string(), "3456".to_string()]
    );
    apply_client_view_offset(&mut view, 99, 99);
    assert_eq!(view.viewport_row, 2);
    assert_eq!(view.viewport_column, 4);
}

/// Verifies that the built-in attached-terminal render configuration presents
/// visible window and pane state by default instead of launching into an
/// unframed, state-free viewport.
#[test]
fn default_client_loop_config_renders_window_and_pane_state_rows() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(24, 4).unwrap());
    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &TerminalClientLoopConfig::default(),
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(view.lines[0].contains("0 shell"), "{:?}", view.lines);
    assert!(view.lines[3].contains("main"), "{:?}", view.lines);
    assert!(view.line_style_spans[3].iter().any(|span| {
        span.start == 0
            && span.length == usize::from(window.size.columns)
            && span.rendition.background == Some(TerminalColor::Rgb(0x1f, 0x1f, 0x28))
    }));
    assert!(
        view.line_style_spans[3]
            .iter()
            .any(|span| span.rendition.background == Some(TerminalColor::Rgb(0x7e, 0x9c, 0xd8)))
    );
    assert!(view.line_style_spans[0].iter().any(|span| {
        span.start == 0
            && span.length == usize::from(window.size.columns)
            && span.rendition.background == Some(TerminalColor::Rgb(0x1f, 0x1f, 0x28))
    }));
    assert!(
        view.line_style_spans[0]
            .iter()
            .any(|span| span.rendition.background == Some(TerminalColor::Rgb(0x7a, 0xa8, 0x9f)))
    );
    assert!(view.cursor_visible);
    assert_eq!(view.cursor_row, 1);
    assert_eq!(view.cursor_column, 0);
}

/// Verifies that attached-client rendering honors pane applications that hide
/// the terminal cursor, including alternate-screen full-screen TUIs.
#[test]
fn attached_client_view_hides_cursor_when_pane_screen_hides_cursor() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(24, 4).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut screen = TerminalScreen::new(Size::new(24, 2).unwrap(), 10).unwrap();
    screen.feed(b"\x1b[?1049h\x1b[?25lhtop");
    let screens = BTreeMap::from([(pane_id, screen)]);

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &TerminalClientLoopConfig::default(),
        window.size,
    )
    .unwrap()
    .unwrap();

    assert!(!view.cursor_visible);
}

/// Verifies that attached-terminal cursor composition treats pane titles merged
/// into horizontal dividers as divider content rather than as an extra row in
/// the bottom pane. This protects over/under splits from reporting the active
/// bottom pane cursor one terminal row below the PTY cursor position.
#[test]
fn attached_client_view_places_bottom_split_cursor_below_merged_divider_title() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(24, 5).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &TerminalClientLoopConfig::default(),
        window.size,
    )
    .unwrap()
    .unwrap();

    assert_eq!(window.active_pane_index(), 1);
    let merged_title_row = view
        .lines
        .iter()
        .position(|line| line.starts_with(" 1 shell"))
        .unwrap();
    assert_eq!(view.cursor_row, merged_title_row + 1);
    assert_eq!(view.cursor_column, 0);
}

/// Verifies attached-client cursor clamping stops before a shared right divider.
///
/// A pane's rightmost shared divider cell belongs to the mux frame, not the
/// pane content region. Cursor placement must therefore clamp before that cell
/// so pane-local UI cannot overwrite the divider.
#[test]
fn attached_client_view_clamps_cursor_before_right_divider() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(10, 3).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window.select_pane("0").unwrap();

    let mut left = TerminalScreen::new(window.panes()[0].size, 10).unwrap();
    left.feed(b"abcde");
    let right = TerminalScreen::new(window.panes()[1].size, 10).unwrap();
    let screens = BTreeMap::from([
        (window.panes()[0].id.to_string(), left),
        (window.panes()[1].id.to_string(), right),
    ]);
    let config = TerminalClientLoopConfig {
        window_frames_enabled: false,
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &screens,
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    assert_eq!(view.cursor_row, 0);
    assert_eq!(view.cursor_column, 3);
}

/// Verifies pane-local agent prompt rendering preserves the right divider when
/// the selected agent pane is on the left side of a vertical split.
///
/// This protects the selected agent shell prompt from drawing its text or
/// prompt background into the mux-managed border cell.
#[test]
fn render_attached_client_view_keeps_agent_prompt_before_right_divider() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(30, 4).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window.select_pane("0").unwrap();
    let left_id = window.panes()[0].id.to_string();
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("go");
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        left_id,
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(prompt),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        pane_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    let divider_column = window.panes()[0].size.columns.saturating_sub(1) as usize;
    let prompt_row = view
        .lines
        .iter()
        .position(|line| line.contains("agent>"))
        .expect("left agent prompt should be visible");
    assert_eq!(
        view.lines[prompt_row].chars().nth(divider_column),
        Some('│'),
        "{}",
        view.lines[prompt_row]
    );
    assert!(
        view.line_style_spans[prompt_row].iter().all(|span| {
            span.start >= divider_column || span.start.saturating_add(span.length) <= divider_column
        }),
        "{:?}",
        view.line_style_spans[prompt_row]
    );
}

/// Verifies client presentation renders status line inside authoritative size.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn client_presentation_renders_status_line_inside_authoritative_size() {
    let view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(12, 3).unwrap(),
        client_size: Size::new(12, 3).unwrap(),
        lines: vec!["one".to_string(), "two".to_string(), "three".to_string()],
        line_style_spans: vec![Vec::new(), Vec::new(), Vec::new()],
        requires_client_scroll: false,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 0,
        cursor_column: 0,
        cursor_visible: false,
        cursor_style: TerminalCursorStyle::Block,
        cursor_blink: true,
        cursor_blink_interval_ms: 500,
        application_keypad: false,
        bracketed_paste: false,
        animation_refresh_interval_ms: 0,
        ui_theme: UiTheme::default(),
        agent_prompt_region: None,
        primary_prompt_active: false,
    };

    let lines = compose_client_presentation(
        &view,
        Some(&ClientStatusLine {
            kind: ClientStatusKind::CopyMode,
            text: "select".to_string(),
        }),
    );

    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "one");
    assert_eq!(lines[2], "copy: select");
}

/// Verifies readline prompt status row renders prompt and cursor column.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_prompt_status_row_renders_prompt_and_cursor_column() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("run");
    assert!(prompt.buffer.move_left());

    let row = render_readline_prompt_status_row(&prompt, 12);

    assert_eq!(
        row.status,
        ClientStatusLine {
            kind: ClientStatusKind::Plain,
            text: "▐ agent> run".to_string(),
        }
    );
    assert_eq!(row.cursor_column, 11);
    assert!(row.cursor_visible);
}

/// Verifies readline prompt status row reports truncated cursor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_prompt_status_row_reports_truncated_cursor() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Command);
    prompt.buffer.insert_text("very-long-command");

    let row = render_readline_prompt_status_row(&prompt, 8);

    assert_eq!(row.status.text, "▐ :very-");
    assert_eq!(row.cursor_column, 7);
    assert!(!row.cursor_visible);
}

/// Verifies readline prompt client presentation places prompt on status row.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn readline_prompt_client_presentation_places_prompt_on_status_row() {
    let view = RenderedClientView {
        role: ClientViewRole::Primary,
        authoritative_size: Size::new(14, 3).unwrap(),
        client_size: Size::new(14, 3).unwrap(),
        lines: vec!["pane".to_string(), "body".to_string(), "old".to_string()],
        line_style_spans: vec![Vec::new(), Vec::new(), Vec::new()],
        requires_client_scroll: false,
        viewport_row: 0,
        viewport_column: 0,
        cursor_row: 0,
        cursor_column: 0,
        cursor_visible: false,
        cursor_style: TerminalCursorStyle::Block,
        cursor_blink: true,
        cursor_blink_interval_ms: 500,
        application_keypad: false,
        bracketed_paste: false,
        animation_refresh_interval_ms: 0,
        ui_theme: UiTheme::default(),
        agent_prompt_region: None,
        primary_prompt_active: false,
    };
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Command);
    prompt.buffer.insert_text("rename");

    let presentation = compose_readline_prompt_client_presentation(&view, &prompt);

    assert_eq!(presentation.lines.len(), 3);
    assert_eq!(presentation.lines[0], "pane");
    assert_eq!(presentation.lines[2], "▐ :rename-wind");
    assert_eq!(presentation.cursor_row, 2);
    assert_eq!(presentation.cursor_column, 9);
    assert!(presentation.cursor_visible);
}

/// Verifies that prompt overlays composed from plain line batches still carry
/// cursor placement for attached-terminal output. Control-socket and async
/// prompt paths use this helper when they do not have a full `RenderedClientView`
/// but still need to present an interactive prompt cursor.
#[test]
fn prompt_overlay_presentation_places_cursor_on_prompt_row() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Command);
    prompt.buffer.insert_text("auth-login");

    let presentation = compose_prompt_overlay_presentation(
        &["pane".to_string(), "old".to_string()],
        &prompt,
        Size::new(24, 3).unwrap(),
    );

    assert_eq!(presentation.lines.len(), 3);
    assert_eq!(presentation.lines[0], "pane                    ");
    assert!(
        presentation
            .lines
            .iter()
            .all(|line| line.chars().count() == 24)
    );
    assert_eq!(presentation.lines[2], "▐ :auth-login           ");
    assert_eq!(presentation.cursor_row, 2);
    assert_eq!(presentation.cursor_column, 13);
    assert!(presentation.cursor_visible);
}

/// Verifies that command-prompt shadow hints are rendered as dim spans on top
/// of the normal prompt-row styling rather than becoming editable prompt text.
#[test]
fn prompt_overlay_presentation_styles_command_shadow_hint() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Command);
    prompt.buffer.insert_text("mcp-");

    let presentation = compose_prompt_overlay_presentation_with_styles(
        &[
            "pane                    ".to_string(),
            "old                     ".to_string(),
        ],
        &[Vec::new(), Vec::new()],
        &prompt,
        Size::new(24, 2).unwrap(),
        &UiTheme::default(),
    );

    assert_eq!(presentation.lines[1], "▐ :mcp-add              ");
    assert!(
        presentation.line_style_spans[1]
            .iter()
            .any(|span| span.start == 7 && span.length == 3 && span.rendition.dim)
    );
    assert!(
        presentation.line_style_spans[1]
            .iter()
            .any(|span| span.start == 7
                && span.length == 3
                && span.rendition.foreground.is_some_and(|foreground| {
                    test_color_is_grayscale(foreground)
                        && test_contrast_ratio(
                            foreground,
                            UiTheme::default().colors.prompt.background,
                        ) >= 4.5
                }))
    );
}

/// Verifies that pane-local agent prompt overlays are drawn inside the owning
/// pane region and keep cursor placement relative to that pane rather than the
/// full terminal footer.
#[test]
fn prompt_region_presentation_places_agent_prompt_inside_pane() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("go");
    let presentation = compose_prompt_region_presentation_with_styles(
        &[
            "top line            ".to_string(),
            "left pane           ".to_string(),
            "old prompt          ".to_string(),
            "footer              ".to_string(),
        ],
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &prompt,
        Size::new(20, 4).unwrap(),
        ReadlinePromptRegion {
            row: 1,
            column: 2,
            columns: 12,
            rows: 2,
        },
        &UiTheme::default(),
    );

    assert_eq!(presentation.lines[0], "top line            ");
    assert_eq!(presentation.lines[2], "ol▐ agent> go       ");
    assert_eq!(presentation.cursor_row, 2);
    assert_eq!(presentation.cursor_column, 13);
    assert!(presentation.cursor_visible);
    assert_eq!(
        presentation.line_style_spans[2]
            .iter()
            .find(|span| span.start == 2)
            .unwrap()
            .rendition
            .background,
        Some(UiTheme::default().colors.agent_prompt.background)
    );
}

/// Verifies pane-local prompts wrap at words before using a hard boundary.
///
/// Agent prompt input can be long, and wrapping at a prior space keeps adjacent
/// words readable while still fitting the reserved prompt region.
#[test]
fn prompt_region_presentation_wraps_prompt_at_word_boundary() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("alpha beta gamma");
    let presentation = compose_prompt_region_presentation_with_styles(
        &[
            "top line                ".to_string(),
            "left pane               ".to_string(),
            "old prompt              ".to_string(),
            "footer                  ".to_string(),
        ],
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &prompt,
        Size::new(24, 4).unwrap(),
        ReadlinePromptRegion {
            row: 1,
            column: 0,
            columns: 16,
            rows: 3,
        },
        &UiTheme::default(),
    );

    assert_eq!(presentation.lines[1], "▐ agent> alpha          ");
    assert_eq!(presentation.lines[2], "         beta           ");
    assert_eq!(presentation.lines[3], "         gamma          ");
}

/// Verifies hard-wrapped unbroken agent prompt input starts at the top of the
/// prompt region instead of bottom-aligning the first wrapped row.
#[test]
fn prompt_region_presentation_hard_wrap_keeps_first_row_stable() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("abcdefghijkl");
    let presentation = compose_prompt_region_presentation_with_styles(
        &[
            "top line                ".to_string(),
            "left pane               ".to_string(),
            "old prompt              ".to_string(),
            "footer                  ".to_string(),
        ],
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &prompt,
        Size::new(24, 4).unwrap(),
        ReadlinePromptRegion {
            row: 1,
            column: 0,
            columns: 16,
            rows: 3,
        },
        &UiTheme::default(),
    );

    assert_eq!(presentation.lines[1], "▐ agent> abcdefg        ");
    assert_eq!(presentation.lines[2], "         hijkl          ");
    assert_eq!(presentation.lines[3], "footer                  ");
}

/// Verifies that pane-local agent prompts render slash-command hints inside the
/// pane region with the same dim styling as footer command prompts.
#[test]
fn prompt_region_presentation_styles_agent_shadow_hint() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("/mod");
    let presentation = compose_prompt_region_presentation_with_styles(
        &[
            "top line            ".to_string(),
            "left pane           ".to_string(),
            "old prompt          ".to_string(),
            "footer              ".to_string(),
        ],
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &prompt,
        Size::new(20, 4).unwrap(),
        ReadlinePromptRegion {
            row: 1,
            column: 1,
            columns: 18,
            rows: 2,
        },
        &UiTheme::default(),
    );

    assert_eq!(presentation.lines[2], "o▐ agent> /model    ");
    assert!(
        presentation.line_style_spans[2]
            .iter()
            .any(|span| span.start == 14 && span.length == 2 && span.rendition.dim)
    );
    assert!(
        presentation.line_style_spans[2]
            .iter()
            .any(|span| span.start == 14
                && span.length == 2
                && span.rendition.foreground.is_some_and(|foreground| {
                    test_color_is_grayscale(foreground)
                        && test_contrast_ratio(
                            foreground,
                            UiTheme::default().colors.agent_prompt.background,
                        ) >= 4.5
                }))
    );
}

/// Verifies pane-local agent prompt input and completion shadows choose
/// contrast-aware black/white foregrounds against light prompt themes.
#[test]
fn prompt_region_presentation_uses_contrast_prompt_foreground_on_light_theme() {
    let definition = builtin_ui_theme_definition("catppuccin_latte").unwrap();
    let theme = resolve_ui_theme("catppuccin_latte", definition).unwrap();
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("/mod");
    let presentation = compose_prompt_region_presentation_with_styles(
        &[
            "top line            ".to_string(),
            "left pane           ".to_string(),
            "old prompt          ".to_string(),
            "footer              ".to_string(),
        ],
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &prompt,
        Size::new(20, 4).unwrap(),
        ReadlinePromptRegion {
            row: 1,
            column: 1,
            columns: 18,
            rows: 2,
        },
        &theme,
    );

    let prompt_span = presentation.line_style_spans[2]
        .iter()
        .find(|span| span.start == 1 && span.length == 18)
        .unwrap();
    assert_eq!(
        prompt_span.rendition.foreground,
        Some(TerminalColor::Rgb(0x00, 0x00, 0x00))
    );
    assert_eq!(
        prompt_span.rendition.background,
        Some(theme.colors.agent_prompt.background)
    );
    assert!(
        presentation.line_style_spans[2]
            .iter()
            .any(|span| span.start == 14
                && span.length == 2
                && span.rendition.dim
                && span.rendition.foreground.is_some_and(|foreground| {
                    test_color_is_grayscale(foreground)
                        && test_contrast_ratio(foreground, theme.colors.agent_prompt.background)
                            >= 4.5
                        && foreground != prompt_span.rendition.foreground.unwrap()
                }))
    );
}

/// Verifies pane-local `$skill` completion hints receive a readable muted style
/// instead of inheriting the editable prompt foreground.
#[test]
fn prompt_region_presentation_styles_agent_skill_shadow_hint() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("$rev");
    prompt.set_selector_extra_candidates([crate::selector::SelectorExtraCandidate::new(
        crate::selector::SelectorSurface::AgentCommand,
        "$",
        crate::selector::SelectorCandidate::new(
            "$review",
            crate::selector::SelectorCandidateKind::Value,
            true,
        )
        .with_detail("Review workflow"),
    )]);
    let theme = UiTheme::default();
    let presentation = compose_prompt_region_presentation_with_styles(
        &[
            "top line            ".to_string(),
            "left pane           ".to_string(),
            "old prompt          ".to_string(),
            "footer              ".to_string(),
        ],
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &prompt,
        Size::new(20, 4).unwrap(),
        ReadlinePromptRegion {
            row: 1,
            column: 1,
            columns: 18,
            rows: 2,
        },
        &theme,
    );

    assert_eq!(presentation.lines[2], "o▐ agent> $review   ");
    let prompt_span = presentation.line_style_spans[2]
        .iter()
        .find(|span| span.start == 1 && span.length == 18)
        .unwrap();
    assert!(
        presentation.line_style_spans[2]
            .iter()
            .any(|span| span.rendition.dim
                && span.rendition.foreground.is_some_and(|foreground| {
                    test_color_is_grayscale(foreground)
                        && foreground != prompt_span.rendition.foreground.unwrap()
                        && test_contrast_ratio(foreground, theme.colors.agent_prompt.background)
                            >= 4.5
                }))
    );
}

/// Verifies attached pane rendering preserves agent prompt shadow hint styling.
///
/// The standalone prompt-region renderer already styles completion shadows, but
/// pane-local agent mode uses a separate `AgentPromptBlock` path. This protects
/// that path so slash and skill completions stay visually muted in real panes.
#[test]
fn render_attached_client_view_styles_agent_prompt_shadow_hint() {
    let mut ids = IdFactory::default();
    let window = Window::new(&mut ids, 0, "main", Size::new(24, 4).unwrap());
    let pane_id = window.panes()[0].id.to_string();
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text("/mod");
    let mut frame_context = TerminalFrameContext::default();
    frame_context.panes.insert(
        pane_id.clone(),
        TerminalPaneFrameContext {
            mode: Some("agent".to_string()),
            agent_prompt: Some(prompt),
            ..TerminalPaneFrameContext::default()
        },
    );
    let config = TerminalClientLoopConfig {
        frame_context,
        window_frames_enabled: false,
        ..TerminalClientLoopConfig::default()
    };

    let view = render_attached_client_view(
        ClientViewRole::Primary,
        &window,
        &BTreeMap::new(),
        &config,
        window.size,
    )
    .unwrap()
    .unwrap();

    let row = view
        .lines
        .iter()
        .position(|line| line.contains("/model"))
        .expect("agent prompt should include completion shadow");
    let hint_start = display_column_for_fragment(&view.lines[row], "el");
    assert!(
        view.line_style_spans[row].iter().any(|span| {
            span.start == hint_start
                && span.length == 2
                && span.rendition.dim
                && span.rendition.background == Some(config.ui_theme.colors.agent_prompt.background)
        }),
        "{:?}",
        view.line_style_spans[row]
    );
}

/// Verifies that a long pasted agent prompt expands upward within the pane and
/// exposes a length note instead of silently hiding that the prompt is large.
#[test]
fn prompt_region_presentation_expands_agent_prompt_for_long_input() {
    let mut prompt =
        crate::readline::ReadlinePrompt::new(crate::readline::ReadlinePromptKind::Agent);
    prompt.buffer.insert_text(&"x".repeat(200));
    let presentation = compose_prompt_region_presentation_with_styles(
        &[
            "one                 ".to_string(),
            "two                 ".to_string(),
            "three               ".to_string(),
            "four                ".to_string(),
        ],
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &prompt,
        Size::new(20, 4).unwrap(),
        ReadlinePromptRegion {
            row: 0,
            column: 0,
            columns: 20,
            rows: 4,
        },
        &UiTheme::default(),
    );

    assert_eq!(presentation.lines[0], "▐ agent> [200 chars ");
    assert_eq!(presentation.cursor_row, 3);
    assert_eq!(presentation.cursor_column, 19);
    assert!(presentation.cursor_visible);
}

/// Verifies display overlay refits base lines to current size.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
/// Verifies that overlays normalize retained base rows to the current terminal
/// size. This prevents stale pre-resize frames from leaving long rows behind
/// when the attached terminal shrinks during a prompt or command display.
fn display_overlay_refits_base_lines_to_current_size() {
    let lines = compose_display_overlay_lines(
        &["abcdefghijklmnopqrstuvwxyz".to_string()],
        &["ok".to_string()],
        Size::new(10, 3).unwrap(),
    );

    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "abcdefghij");
    assert_eq!(lines[1], "          ");
    assert_eq!(lines[2], "ok        ");
    assert!(lines.iter().all(|line| line.chars().count() == 10));
}

/// Verifies that display overlays keep style spans for retained base rows,
/// clip them to the current terminal width, and clear styles on rows replaced
/// by Mezzanine-owned display text.
#[test]
fn display_overlay_preserves_and_refits_retained_base_styles() {
    let spans = compose_display_overlay_line_style_spans(
        &[vec![TerminalStyleSpan {
            start: 0,
            length: 26,
            rendition: GraphicRendition {
                foreground: Some(TerminalColor::Indexed(2)),
                ..GraphicRendition::default()
            },
        }]],
        &["ok".to_string()],
        Size::new(10, 3).unwrap(),
        &UiTheme::default(),
    );

    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0].len(), 1);
    assert_eq!(spans[0][0].length, 10);
    assert_eq!(
        spans[0][0].rendition.foreground,
        Some(TerminalColor::Indexed(2))
    );
    assert!(spans[1].is_empty());
    assert_eq!(spans[2].len(), 1);
    assert_eq!(spans[2][0].start, 0);
    assert_eq!(spans[2][0].length, 2);
    assert_eq!(
        spans[2][0].rendition.foreground,
        Some(TerminalColor::Rgb(0x7a, 0xa8, 0x9f))
    );
    assert_eq!(spans[2][0].rendition.background, None);
}

/// Verifies that command display overlays can target the active pane's client
/// region instead of replacing the whole terminal frame. This keeps `help` and
/// other command output visually local to the pane that invoked the command.
#[test]
fn display_region_overlay_renders_output_inside_requested_pane_region() {
    let region = ReadlinePromptRegion {
        row: 0,
        column: 2,
        columns: 8,
        rows: 3,
    };
    let base = vec![
        "............".to_string(),
        "............".to_string(),
        "............".to_string(),
        "............".to_string(),
    ];
    let display = vec![
        "first".to_string(),
        "second".to_string(),
        "third".to_string(),
    ];

    let lines =
        compose_display_region_overlay_lines(&base, &display, Size::new(12, 4).unwrap(), region);
    let spans = compose_display_region_overlay_line_style_spans(
        &[Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        &display,
        Size::new(12, 4).unwrap(),
        region,
        &UiTheme::default(),
    );

    assert_eq!(lines[0], "..second  ..");
    assert_eq!(lines[1], "..third   ..");
    assert_eq!(lines[2], "............");
    assert_eq!(spans[0][0].start, 2);
    assert_eq!(spans[0][0].length, 6);
    assert_eq!(spans[1][0].start, 2);
    assert_eq!(spans[1][0].length, 5);
    assert_eq!(spans[0][0].rendition.background, None);
    assert_eq!(spans[1][0].rendition.background, None);
    assert!(spans[2].is_empty());
}

/// Verifies that modal command display overlays fill the entire terminal
/// window and expose an explicit Escape affordance. Long output is pageable by
/// scroll offset instead of disappearing on the next terminal redraw.
#[test]
fn modal_display_overlay_covers_terminal_and_pages_output() {
    let display = vec![
        "line one".to_string(),
        "line two".to_string(),
        "line three".to_string(),
        "line four".to_string(),
    ];

    let lines = compose_modal_display_overlay_lines(&display, Size::new(24, 4).unwrap(), 1);
    let spans = compose_modal_display_overlay_line_style_spans(
        &display,
        Size::new(24, 4).unwrap(),
        1,
        &UiTheme::default(),
    );

    assert_eq!(
        modal_display_overlay_max_scroll(&display, Size::new(24, 4).unwrap()),
        2
    );
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0], "mezzanine command output");
    assert_eq!(lines[1], "line two                ");
    assert_eq!(lines[2], "line three              ");
    assert!(lines[3].contains("esc: return"));
    assert_eq!(spans.len(), 4);
    assert_eq!(spans[0][0].start, 0);
    assert_eq!(spans[0][0].length, "mezzanine command output".len());
    assert_eq!(spans[1][0].length, "line two".len());
    assert_eq!(spans[2][0].length, "line three".len());
    assert_eq!(spans[3][0].start, 0);
    assert_eq!(spans[3][0].rendition.background, None);
}

/// Verifies alternate screen is not history recordable.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn alternate_screen_is_not_history_recordable() {
    let mut state = AlternateScreenState::new();
    assert!(state.should_record_to_history());

    state.enter();

    assert!(!state.should_record_to_history());
}

/// Verifies terminal screen prints line oriented output.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_prints_line_oriented_output() {
    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();

    screen.feed(b"hello\r\nworld");

    assert_eq!(screen.visible_lines()[0], "hello");
    assert_eq!(screen.visible_lines()[1], "world");
}

/// Verifies that terminal autowrap is deferred after writing the last column.
/// Real terminals keep the cursor visually on the bottom-right cell until the
/// next printable character arrives; this keeps echoed prompt input visible on
/// the bottom row and only scrolls when more output actually needs space.
#[test]
fn terminal_screen_defers_autowrap_until_next_printable_cell() {
    let mut screen = TerminalScreen::new(Size::new(4, 2).unwrap(), 10).unwrap();

    screen.feed(b"abcd");
    assert_eq!(screen.visible_lines(), vec!["abcd", ""]);
    assert_eq!(
        screen.history().lines().collect::<Vec<_>>(),
        Vec::<&str>::new()
    );

    screen.feed(b"e");
    assert_eq!(screen.visible_lines(), vec!["abcd", "e"]);

    screen.feed(b"fghijk");
    assert_eq!(screen.history().lines().collect::<Vec<_>>(), vec!["abcd"]);
    assert_eq!(screen.visible_lines(), vec!["efgh", "ijk"]);
}

/// Verifies terminal screen tracks activity and bell events.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_tracks_activity_and_bell_events() {
    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();

    screen.feed(b"hello\x07");
    screen.feed(b"world\x07\x07");

    assert_eq!(screen.activity_events(), 2);
    assert_eq!(screen.bell_events(), 3);
    assert_eq!(screen.visible_lines()[0], "helloworld");
}

/// Verifies terminal screen scrolls normal output into history.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_scrolls_normal_output_into_history() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[4;38;5;42mone\x1b[0m\ntwo\nthree");

    assert_eq!(screen.history().lines().collect::<Vec<_>>(), vec!["one"]);
    let styled_history = screen.history().styled_lines().collect::<Vec<_>>();
    assert_eq!(styled_history[0].text, "one");
    assert_eq!(
        styled_history[0].style_spans,
        vec![TerminalStyleSpan {
            start: 0,
            length: 3,
            rendition: GraphicRendition {
                bold: false,
                dim: false,
                italic: false,
                strikethrough: false,
                double_underline: false,
                hidden: false,
                underline: true,
                inverse: false,
                foreground: Some(TerminalColor::Indexed(42)),
                background: None,
            }
        }]
    );
    assert_eq!(screen.visible_lines()[1], "three");
}

/// Verifies terminal screen excludes alternate screen from history.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_excludes_alternate_screen_from_history() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[?1049halt\ninside\nmore\x1b[?1049lback");

    assert!(screen.history().is_empty());
    assert_eq!(screen.visible_lines()[0], "back");
    assert!(!screen.alternate_screen_active());
}

/// Verifies terminal screen handles cursor address and clear line.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_handles_cursor_address_and_clear_line() {
    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();

    screen.feed(b"abcdef\x1b[1;3H\x1b[Kxy");

    assert_eq!(screen.visible_lines()[0], "abxy");
}

/// Verifies terminal screen handles relative cursor movement and c0 controls.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_handles_relative_cursor_movement_and_c0_controls() {
    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();

    screen.feed(b"top\nmid\nbot\x1b[A\x1b[2DZZ\x1b[B\x1b[CQ\r!\tT\x08?");

    assert_eq!(screen.visible_lines()[0], "top");
    assert_eq!(screen.visible_lines()[1], "mZZ");
    assert_eq!(screen.visible_lines()[2], "!ot Q   ?");
}

/// Verifies terminal screen handles erase display variants.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn terminal_screen_handles_erase_display_variants() {
    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();

    screen.feed(b"abc\n123\nxyz\x1b[2;2H\x1b[J");
    assert_eq!(screen.visible_lines(), vec!["abc", "1", ""]);

    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();
    screen.feed(b"\x1b[2Jabc\n123\nxyz\x1b[2;2H\x1b[1J");
    assert_eq!(screen.visible_lines(), vec!["", "  3", "xyz"]);

    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();
    screen.feed(b"\x1b[2Jdone");
    assert_eq!(screen.visible_lines()[0], "done");

    let mut screen = TerminalScreen::new(Size::new(10, 2).unwrap(), 10).unwrap();
    screen.feed(b"one\ntwo\nthree\x1b[3J");
    assert!(screen.history().is_empty());
    assert_eq!(screen.visible_lines()[1], "three");
}

/// Verifies UI clears preserve pane logs by scrolling visible rows into history.
///
/// Agent-mode entry, exit, and prompt `Ctrl+L` use this path so the pane can
/// look freshly cleared without deleting content from copyable scrollback.
#[test]
fn terminal_screen_clear_visible_into_history_preserves_log_rows() {
    let mut screen = TerminalScreen::new(Size::new(10, 3).unwrap(), 10).unwrap();

    screen.feed(b"\x1b[31mred\x1b[0m\nmiddle\nbottom");
    screen.clear_visible_into_history();

    assert_eq!(screen.visible_lines(), vec!["", "", ""]);
    assert_eq!(
        screen.history().lines().collect::<Vec<_>>(),
        vec!["red", "middle", "bottom"]
    );
    let styled_history = screen.history().styled_lines().collect::<Vec<_>>();
    assert_eq!(
        styled_history[0].style_spans[0].rendition.foreground,
        Some(TerminalColor::Indexed(1))
    );
}
