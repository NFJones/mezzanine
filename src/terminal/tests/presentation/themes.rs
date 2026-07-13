//! Regression tests for terminal presentation themes behavior.

use crate::terminal::render::agent_prompt_input_rendition;
use crate::terminal::tests::fixtures::{
    test_contrast_ratio, test_relative_luminance, test_rgb_channels,
};
use crate::terminal::{BTreeMap, UiColorPair, UiTheme};
use mez_mux::theme::{
    BUILTIN_UI_THEME_NAMES, builtin_ui_theme_definition, parse_hex_color, resolve_ui_theme,
};
use mez_terminal::TerminalColor;
use std::collections::BTreeSet;

fn builtin_theme_preserves_exact_snapshot(name: &str) -> bool {
    matches!(
        name,
        "acid_grapefruit" | "acid_lemon" | "acid_tangerine" | "acid_lime"
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeFidelityCategory {
    UpstreamFamily,
    InterpretiveFamily,
    Native,
}

struct ThemeFidelityTarget {
    name: &'static str,
    category: ThemeFidelityCategory,
    anchors: &'static [(&'static str, &'static str)],
}

fn builtin_theme_fidelity_targets() -> &'static [ThemeFidelityTarget] {
    &[
        ThemeFidelityTarget {
            name: "deepforest",
            category: ThemeFidelityCategory::Native,
            anchors: &[],
        },
        ThemeFidelityTarget {
            name: "apprentice",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#5f875f"),
                ("surface", "#262626"),
                ("danger", "#af5f5f"),
            ],
        },
        ThemeFidelityTarget {
            name: "gruvbox_dark",
            category: ThemeFidelityCategory::InterpretiveFamily,
            anchors: &[
                ("primary", "#fabd2f"),
                ("surface", "#282828"),
                ("danger", "#fb4934"),
            ],
        },
        ThemeFidelityTarget {
            name: "gruvbox_light",
            category: ThemeFidelityCategory::InterpretiveFamily,
            anchors: &[
                ("primary", "#b57614"),
                ("surface", "#fbf1c7"),
                ("danger", "#cc241d"),
            ],
        },
        ThemeFidelityTarget {
            name: "solarized_dark",
            category: ThemeFidelityCategory::InterpretiveFamily,
            anchors: &[
                ("primary", "#268bd2"),
                ("surface", "#002b36"),
                ("danger", "#dc322f"),
            ],
        },
        ThemeFidelityTarget {
            name: "solarized_light",
            category: ThemeFidelityCategory::InterpretiveFamily,
            anchors: &[
                ("primary", "#268bd2"),
                ("surface", "#fdf6e3"),
                ("danger", "#dc322f"),
            ],
        },
        ThemeFidelityTarget {
            name: "monokai",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#a6e22e"),
                ("surface", "#272822"),
                ("danger", "#f92672"),
            ],
        },
        ThemeFidelityTarget {
            name: "dracula",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#bd93f9"),
                ("surface", "#282a36"),
                ("danger", "#ff5555"),
            ],
        },
        ThemeFidelityTarget {
            name: "nord",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#88c0d0"),
                ("surface", "#2e3440"),
                ("danger", "#bf616a"),
            ],
        },
        ThemeFidelityTarget {
            name: "tokyo_night",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#7aa2f7"),
                ("surface", "#1a1b26"),
                ("danger", "#f7768e"),
            ],
        },
        ThemeFidelityTarget {
            name: "catppuccin_latte",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#1e66f5"),
                ("surface", "#eff1f5"),
                ("danger", "#d20f39"),
            ],
        },
        ThemeFidelityTarget {
            name: "catppuccin_frappe",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#8caaee"),
                ("surface", "#303446"),
                ("danger", "#e78284"),
            ],
        },
        ThemeFidelityTarget {
            name: "catppuccin_macchiato",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#8aadf4"),
                ("surface", "#24273a"),
                ("danger", "#ed8796"),
            ],
        },
        ThemeFidelityTarget {
            name: "catppuccin_mocha",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#cba6f7"),
                ("surface", "#1e1e2e"),
                ("danger", "#f38ba8"),
            ],
        },
        ThemeFidelityTarget {
            name: "one_half_dark",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#61afef"),
                ("surface", "#282c34"),
                ("danger", "#e06c75"),
            ],
        },
        ThemeFidelityTarget {
            name: "one_half_light",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#0184bc"),
                ("surface", "#fafafa"),
                ("danger", "#e45649"),
            ],
        },
        ThemeFidelityTarget {
            name: "onedark",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#61afef"),
                ("surface", "#282c34"),
                ("danger", "#e06c75"),
            ],
        },
        ThemeFidelityTarget {
            name: "rose_pine",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#c4a7e7"),
                ("surface", "#191724"),
                ("danger", "#eb6f92"),
            ],
        },
        ThemeFidelityTarget {
            name: "rose_pine_moon",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#c4a7e7"),
                ("surface", "#232136"),
                ("danger", "#eb6f92"),
            ],
        },
        ThemeFidelityTarget {
            name: "rose_pine_dawn",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#907aa9"),
                ("surface", "#faf4ed"),
                ("danger", "#b4637a"),
            ],
        },
        ThemeFidelityTarget {
            name: "kanagawa",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#7e9cd8"),
                ("surface", "#1f1f28"),
                ("danger", "#e82424"),
            ],
        },
        ThemeFidelityTarget {
            name: "everforest_dark",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#a7c080"),
                ("surface", "#2d353b"),
                ("danger", "#e67e80"),
            ],
        },
        ThemeFidelityTarget {
            name: "everforest_light",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#8da101"),
                ("surface", "#fff9e8"),
                ("danger", "#f85552"),
            ],
        },
        ThemeFidelityTarget {
            name: "ayu",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#59c2ff"),
                ("surface", "#0a0e14"),
                ("danger", "#f07178"),
            ],
        },
        ThemeFidelityTarget {
            name: "ayu_dark",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#59c2ff"),
                ("surface", "#0a0e14"),
                ("danger", "#f07178"),
            ],
        },
        ThemeFidelityTarget {
            name: "ayu_light",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#55b4d4"),
                ("surface", "#fafafa"),
                ("danger", "#f07178"),
            ],
        },
        ThemeFidelityTarget {
            name: "ayu_mirage",
            category: ThemeFidelityCategory::UpstreamFamily,
            anchors: &[
                ("primary", "#73d0ff"),
                ("surface", "#1f2430"),
                ("danger", "#f28779"),
            ],
        },
        ThemeFidelityTarget {
            name: "acid_grapefruit",
            category: ThemeFidelityCategory::Native,
            anchors: &[],
        },
        ThemeFidelityTarget {
            name: "acid_lemon",
            category: ThemeFidelityCategory::Native,
            anchors: &[],
        },
        ThemeFidelityTarget {
            name: "acid_tangerine",
            category: ThemeFidelityCategory::Native,
            anchors: &[],
        },
        ThemeFidelityTarget {
            name: "acid_lime",
            category: ThemeFidelityCategory::Native,
            anchors: &[],
        },
        ThemeFidelityTarget {
            name: "high_contrast_dark",
            category: ThemeFidelityCategory::Native,
            anchors: &[],
        },
        ThemeFidelityTarget {
            name: "high_contrast_light",
            category: ThemeFidelityCategory::Native,
            anchors: &[],
        },
    ]
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
        if builtin_theme_preserves_exact_snapshot(name) {
            continue;
        }
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
        if builtin_theme_preserves_exact_snapshot(name) {
            continue;
        }
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

/// Verifies built-in themes keep every text-bearing derived UI surface readable.
///
/// Accent-backed frame, status, command, and selection surfaces are derived from
/// compact built-in palettes rather than hand-authored per-theme slots. This
/// guards that derivation against pairing bright or saturated accents with a
/// foreground that preserves theme identity but cannot be read reliably.
#[test]
fn builtin_themes_keep_text_bearing_pairs_readable() {
    for name in BUILTIN_UI_THEME_NAMES {
        if builtin_theme_preserves_exact_snapshot(name) {
            continue;
        }
        let definition =
            builtin_ui_theme_definition(name).unwrap_or_else(|| panic!("missing theme {name}"));
        let theme = resolve_ui_theme(name, definition).expect("built-in theme must resolve");
        let pairs = [
            ("window_active", theme.colors.window_active),
            ("window_inactive", theme.colors.window_inactive),
            ("pane_frame_active", theme.colors.pane_frame_active),
            ("frame_fill", theme.colors.frame_fill),
            ("scroll_indicator", theme.colors.scroll_indicator),
            ("pane_pwd", theme.colors.pane_pwd),
            ("window_status_uptime", theme.colors.window_status_uptime),
            (
                "window_status_datetime",
                theme.colors.window_status_datetime,
            ),
            ("prompt", theme.colors.prompt),
            ("agent_prompt", theme.colors.agent_prompt),
            ("agent_transcript_user", theme.colors.agent_transcript_user),
            (
                "agent_transcript_assistant",
                theme.colors.agent_transcript_assistant,
            ),
            (
                "agent_transcript_status",
                theme.colors.agent_transcript_status,
            ),
            (
                "agent_transcript_error",
                theme.colors.agent_transcript_error,
            ),
            (
                "agent_transcript_command",
                theme.colors.agent_transcript_command,
            ),
            ("agent_model", theme.colors.agent_model),
            ("agent_reasoning", theme.colors.agent_reasoning),
            ("agent_status_idle", theme.colors.agent_status_idle),
            ("agent_status_running", theme.colors.agent_status_running),
            ("agent_status_blocked", theme.colors.agent_status_blocked),
            ("agent_status_failed", theme.colors.agent_status_failed),
            ("display_overlay", theme.colors.display_overlay),
            ("copy_selection", theme.colors.copy_selection),
            ("syntax_plain", theme.colors.syntax_plain),
            ("syntax_keyword", theme.colors.syntax_keyword),
            ("syntax_string", theme.colors.syntax_string),
            ("syntax_comment", theme.colors.syntax_comment),
            ("syntax_type", theme.colors.syntax_type),
            ("syntax_function", theme.colors.syntax_function),
            ("syntax_number", theme.colors.syntax_number),
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
        "acid_grapefruit",
        "acid_lemon",
        "acid_tangerine",
        "acid_lime",
        "apprentice",
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

/// Verifies the built-in acid_lime theme preserves the recorded reference
/// alias palette and full color-slot mapping.
///
/// The remaining acid-family built-in theme tasks derive from this exact
/// reference, so the built-in definition should match the captured user-config
/// snapshot rather than a contrast-managed approximation.
#[test]
fn acid_lime_builtin_theme_matches_documented_reference_palette() {
    let definition = builtin_ui_theme_definition("acid_lime").expect("missing acid_lime theme");
    let expected_aliases = [
        ("primary", "#bfff00"),
        ("secondary", "#7fbf3f"),
        ("tertiary", "#d7ff5f"),
        ("thinking", "#c9d89a"),
        ("danger", "#ff5c57"),
        ("foreground", "#eef7d0"),
        ("muted", "#6f7f3c"),
        ("surface", "#1b1f0a"),
        ("danger_foreground", "#ff7b74"),
        ("danger_text", "#140200"),
        ("muted_text", "#0f1206"),
        ("primary_foreground", "#d8ff5a"),
        ("primary_text", "#111400"),
        ("secondary_foreground", "#a8e85a"),
        ("secondary_text", "#111400"),
        ("tertiary_foreground", "#e6ff8a"),
        ("tertiary_text", "#111400"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect::<BTreeMap<_, _>>();
    let expected_colors = [
        ("window_frame_fg", "primary_foreground"),
        ("window_frame_bg", "surface"),
        ("window_active_fg", "primary_text"),
        ("window_active_bg", "primary"),
        ("window_inactive_fg", "secondary_text"),
        ("window_inactive_bg", "secondary"),
        ("pane_frame_active_fg", "secondary_text"),
        ("pane_frame_active_bg", "secondary"),
        ("pane_frame_inactive_fg", "muted"),
        ("pane_frame_inactive_bg", "surface"),
        ("pane_border_active_fg", "primary_foreground"),
        ("pane_border_active_bg", "surface"),
        ("pane_border_inactive_fg", "muted"),
        ("pane_border_inactive_bg", "surface"),
        ("pane_divider_fg", "tertiary_foreground"),
        ("pane_divider_bg", "surface"),
        ("frame_fill_fg", "foreground"),
        ("frame_fill_bg", "surface"),
        ("scroll_indicator_fg", "tertiary_text"),
        ("scroll_indicator_bg", "tertiary"),
        ("pane_pwd_fg", "muted_text"),
        ("pane_pwd_bg", "muted"),
        ("window_status_uptime_fg", "secondary_text"),
        ("window_status_uptime_bg", "secondary"),
        ("window_status_datetime_fg", "tertiary_text"),
        ("window_status_datetime_bg", "tertiary"),
        ("prompt_fg", "primary_foreground"),
        ("prompt_bg", "surface"),
        ("agent_prompt_fg", "#f8ffe0"),
        ("agent_prompt_bg", "#20250c"),
        ("agent_transcript_user_fg", "primary_foreground"),
        ("agent_transcript_user_bg", "surface"),
        ("agent_transcript_assistant_fg", "secondary_foreground"),
        ("agent_transcript_assistant_bg", "surface"),
        ("agent_transcript_status_fg", "thinking"),
        ("agent_transcript_status_bg", "surface"),
        ("agent_transcript_error_fg", "danger_foreground"),
        ("agent_transcript_error_bg", "surface"),
        ("agent_transcript_command_fg", "tertiary_foreground"),
        ("agent_transcript_command_bg", "surface"),
        ("agent_model_fg", "secondary_text"),
        ("agent_model_bg", "secondary"),
        ("agent_reasoning_fg", "tertiary_text"),
        ("agent_reasoning_bg", "tertiary"),
        ("agent_status_idle_fg", "muted_text"),
        ("agent_status_idle_bg", "muted"),
        ("agent_status_running_fg", "primary_text"),
        ("agent_status_running_bg", "primary"),
        ("agent_status_blocked_fg", "tertiary_text"),
        ("agent_status_blocked_bg", "tertiary"),
        ("agent_status_failed_fg", "danger_text"),
        ("agent_status_failed_bg", "danger"),
        ("display_overlay_fg", "secondary_foreground"),
        ("display_overlay_bg", "surface"),
        ("copy_selection_fg", "tertiary_text"),
        ("copy_selection_bg", "tertiary"),
        ("syntax_plain_fg", "foreground"),
        ("syntax_plain_bg", "surface"),
        ("syntax_keyword_fg", "primary_foreground"),
        ("syntax_keyword_bg", "surface"),
        ("syntax_string_fg", "tertiary_foreground"),
        ("syntax_string_bg", "surface"),
        ("syntax_comment_fg", "thinking"),
        ("syntax_comment_bg", "surface"),
        ("syntax_type_fg", "secondary_foreground"),
        ("syntax_type_bg", "surface"),
        ("syntax_function_fg", "primary_foreground"),
        ("syntax_function_bg", "surface"),
        ("syntax_number_fg", "tertiary_foreground"),
        ("syntax_number_bg", "surface"),
        ("syntax_operator_fg", "muted"),
        ("syntax_operator_bg", "surface"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect::<BTreeMap<_, _>>();

    assert_eq!(definition.aliases, expected_aliases);
    assert_eq!(definition.colors, expected_colors);
}

/// Verifies the built-in acid_grapefruit theme preserves the derived
/// grapefruit reference alias palette and full color-slot mapping.
///
/// The acid-family themes are exact snapshot-style built-ins. Keeping this full
/// mapping in test coverage makes the red hue-shift reproducible for users and
/// for later acid-family siblings.
#[test]
fn acid_grapefruit_builtin_theme_matches_documented_reference_palette() {
    let definition =
        builtin_ui_theme_definition("acid_grapefruit").expect("missing acid_grapefruit theme");
    let expected_aliases = [
        ("primary", "#ff5f73"),
        ("secondary", "#d74f71"),
        ("tertiary", "#ff9a7a"),
        ("thinking", "#e6b3b3"),
        ("danger", "#ff3350"),
        ("foreground", "#fff0ea"),
        ("muted", "#96606a"),
        ("surface", "#2a1116"),
        ("danger_foreground", "#ff9aa6"),
        ("danger_text", "#140002"),
        ("muted_text", "#171012"),
        ("primary_foreground", "#ffb0bb"),
        ("primary_text", "#140002"),
        ("secondary_foreground", "#f07a94"),
        ("secondary_text", "#140002"),
        ("tertiary_foreground", "#ffb39d"),
        ("tertiary_text", "#140402"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect::<BTreeMap<_, _>>();
    let expected_colors = [
        ("window_frame_fg", "primary_foreground"),
        ("window_frame_bg", "surface"),
        ("window_active_fg", "primary_text"),
        ("window_active_bg", "primary"),
        ("window_inactive_fg", "secondary_text"),
        ("window_inactive_bg", "secondary"),
        ("pane_frame_active_fg", "secondary_text"),
        ("pane_frame_active_bg", "secondary"),
        ("pane_frame_inactive_fg", "muted"),
        ("pane_frame_inactive_bg", "surface"),
        ("pane_border_active_fg", "primary_foreground"),
        ("pane_border_active_bg", "surface"),
        ("pane_border_inactive_fg", "muted"),
        ("pane_border_inactive_bg", "surface"),
        ("pane_divider_fg", "tertiary_foreground"),
        ("pane_divider_bg", "surface"),
        ("frame_fill_fg", "foreground"),
        ("frame_fill_bg", "surface"),
        ("scroll_indicator_fg", "tertiary_text"),
        ("scroll_indicator_bg", "tertiary"),
        ("pane_pwd_fg", "muted_text"),
        ("pane_pwd_bg", "muted"),
        ("window_status_uptime_fg", "secondary_text"),
        ("window_status_uptime_bg", "secondary"),
        ("window_status_datetime_fg", "tertiary_text"),
        ("window_status_datetime_bg", "tertiary"),
        ("prompt_fg", "primary_foreground"),
        ("prompt_bg", "surface"),
        ("agent_prompt_fg", "#fff2ee"),
        ("agent_prompt_bg", "#301219"),
        ("agent_transcript_user_fg", "primary_foreground"),
        ("agent_transcript_user_bg", "surface"),
        ("agent_transcript_assistant_fg", "secondary_foreground"),
        ("agent_transcript_assistant_bg", "surface"),
        ("agent_transcript_status_fg", "thinking"),
        ("agent_transcript_status_bg", "surface"),
        ("agent_transcript_error_fg", "danger_foreground"),
        ("agent_transcript_error_bg", "surface"),
        ("agent_transcript_command_fg", "tertiary_foreground"),
        ("agent_transcript_command_bg", "surface"),
        ("agent_model_fg", "secondary_text"),
        ("agent_model_bg", "secondary"),
        ("agent_reasoning_fg", "tertiary_text"),
        ("agent_reasoning_bg", "tertiary"),
        ("agent_status_idle_fg", "muted_text"),
        ("agent_status_idle_bg", "muted"),
        ("agent_status_running_fg", "primary_text"),
        ("agent_status_running_bg", "primary"),
        ("agent_status_blocked_fg", "tertiary_text"),
        ("agent_status_blocked_bg", "tertiary"),
        ("agent_status_failed_fg", "danger_text"),
        ("agent_status_failed_bg", "danger"),
        ("display_overlay_fg", "secondary_foreground"),
        ("display_overlay_bg", "surface"),
        ("copy_selection_fg", "tertiary_text"),
        ("copy_selection_bg", "tertiary"),
        ("syntax_plain_fg", "foreground"),
        ("syntax_plain_bg", "surface"),
        ("syntax_keyword_fg", "primary_foreground"),
        ("syntax_keyword_bg", "surface"),
        ("syntax_string_fg", "tertiary_foreground"),
        ("syntax_string_bg", "surface"),
        ("syntax_comment_fg", "thinking"),
        ("syntax_comment_bg", "surface"),
        ("syntax_type_fg", "secondary_foreground"),
        ("syntax_type_bg", "surface"),
        ("syntax_function_fg", "primary_foreground"),
        ("syntax_function_bg", "surface"),
        ("syntax_number_fg", "tertiary_foreground"),
        ("syntax_number_bg", "surface"),
        ("syntax_operator_fg", "muted"),
        ("syntax_operator_bg", "surface"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect::<BTreeMap<_, _>>();

    assert_eq!(definition.aliases, expected_aliases);
    assert_eq!(definition.colors, expected_colors);
}

/// Verifies the built-in acid_lemon theme preserves the derived lemon
/// reference alias palette and full color-slot mapping.
///
/// The acid-family themes are exact snapshot-style built-ins. Keeping this full
/// mapping in test coverage makes the yellow hue-shift reproducible for users
/// and for later acid-family siblings.
#[test]
fn acid_lemon_builtin_theme_matches_documented_reference_palette() {
    let definition = builtin_ui_theme_definition("acid_lemon").expect("missing acid_lemon theme");
    let expected_aliases = [
        ("primary", "#fff066"),
        ("secondary", "#d8bf52"),
        ("tertiary", "#fff799"),
        ("thinking", "#e6ddb0"),
        ("danger", "#ff5c57"),
        ("foreground", "#fffced"),
        ("muted", "#9a8f52"),
        ("surface", "#2a250f"),
        ("danger_foreground", "#ff7b74"),
        ("danger_text", "#140200"),
        ("muted_text", "#171407"),
        ("primary_foreground", "#fff7a8"),
        ("primary_text", "#141200"),
        ("secondary_foreground", "#f3dd7a"),
        ("secondary_text", "#141200"),
        ("tertiary_foreground", "#fffabc"),
        ("tertiary_text", "#141200"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect::<BTreeMap<_, _>>();
    let expected_colors = [
        ("window_frame_fg", "primary_foreground"),
        ("window_frame_bg", "surface"),
        ("window_active_fg", "primary_text"),
        ("window_active_bg", "primary"),
        ("window_inactive_fg", "secondary_text"),
        ("window_inactive_bg", "secondary"),
        ("pane_frame_active_fg", "secondary_text"),
        ("pane_frame_active_bg", "secondary"),
        ("pane_frame_inactive_fg", "muted"),
        ("pane_frame_inactive_bg", "surface"),
        ("pane_border_active_fg", "primary_foreground"),
        ("pane_border_active_bg", "surface"),
        ("pane_border_inactive_fg", "muted"),
        ("pane_border_inactive_bg", "surface"),
        ("pane_divider_fg", "tertiary_foreground"),
        ("pane_divider_bg", "surface"),
        ("frame_fill_fg", "foreground"),
        ("frame_fill_bg", "surface"),
        ("scroll_indicator_fg", "tertiary_text"),
        ("scroll_indicator_bg", "tertiary"),
        ("pane_pwd_fg", "muted_text"),
        ("pane_pwd_bg", "muted"),
        ("window_status_uptime_fg", "secondary_text"),
        ("window_status_uptime_bg", "secondary"),
        ("window_status_datetime_fg", "tertiary_text"),
        ("window_status_datetime_bg", "tertiary"),
        ("prompt_fg", "primary_foreground"),
        ("prompt_bg", "surface"),
        ("agent_prompt_fg", "#fffef2"),
        ("agent_prompt_bg", "#302b12"),
        ("agent_transcript_user_fg", "primary_foreground"),
        ("agent_transcript_user_bg", "surface"),
        ("agent_transcript_assistant_fg", "secondary_foreground"),
        ("agent_transcript_assistant_bg", "surface"),
        ("agent_transcript_status_fg", "thinking"),
        ("agent_transcript_status_bg", "surface"),
        ("agent_transcript_error_fg", "danger_foreground"),
        ("agent_transcript_error_bg", "surface"),
        ("agent_transcript_command_fg", "tertiary_foreground"),
        ("agent_transcript_command_bg", "surface"),
        ("agent_model_fg", "secondary_text"),
        ("agent_model_bg", "secondary"),
        ("agent_reasoning_fg", "tertiary_text"),
        ("agent_reasoning_bg", "tertiary"),
        ("agent_status_idle_fg", "muted_text"),
        ("agent_status_idle_bg", "muted"),
        ("agent_status_running_fg", "primary_text"),
        ("agent_status_running_bg", "primary"),
        ("agent_status_blocked_fg", "tertiary_text"),
        ("agent_status_blocked_bg", "tertiary"),
        ("agent_status_failed_fg", "danger_text"),
        ("agent_status_failed_bg", "danger"),
        ("display_overlay_fg", "secondary_foreground"),
        ("display_overlay_bg", "surface"),
        ("copy_selection_fg", "tertiary_text"),
        ("copy_selection_bg", "tertiary"),
        ("syntax_plain_fg", "foreground"),
        ("syntax_plain_bg", "surface"),
        ("syntax_keyword_fg", "primary_foreground"),
        ("syntax_keyword_bg", "surface"),
        ("syntax_string_fg", "tertiary_foreground"),
        ("syntax_string_bg", "surface"),
        ("syntax_comment_fg", "thinking"),
        ("syntax_comment_bg", "surface"),
        ("syntax_type_fg", "secondary_foreground"),
        ("syntax_type_bg", "surface"),
        ("syntax_function_fg", "primary_foreground"),
        ("syntax_function_bg", "surface"),
        ("syntax_number_fg", "tertiary_foreground"),
        ("syntax_number_bg", "surface"),
        ("syntax_operator_fg", "muted"),
        ("syntax_operator_bg", "surface"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect::<BTreeMap<_, _>>();

    assert_eq!(definition.aliases, expected_aliases);
    assert_eq!(definition.colors, expected_colors);
}

/// Verifies the built-in acid_tangerine theme preserves the derived tangerine
/// reference alias palette and full color-slot mapping.
///
/// The acid-family themes are exact snapshot-style built-ins. Keeping this full
/// mapping in test coverage makes the orange hue-shift reproducible for users
/// and for later acid-family siblings.
#[test]
fn acid_tangerine_builtin_theme_matches_documented_reference_palette() {
    let definition =
        builtin_ui_theme_definition("acid_tangerine").expect("missing acid_tangerine theme");
    let expected_aliases = [
        ("primary", "#ffab3d"),
        ("secondary", "#d88a52"),
        ("tertiary", "#ffca8e"),
        ("thinking", "#e6c5b0"),
        ("danger", "#ff5c57"),
        ("foreground", "#fff2ea"),
        ("muted", "#966c52"),
        ("surface", "#2a180f"),
        ("danger_foreground", "#ff9f88"),
        ("danger_text", "#140200"),
        ("muted_text", "#17100b"),
        ("primary_foreground", "#ffd08a"),
        ("primary_text", "#140800"),
        ("secondary_foreground", "#f2b27e"),
        ("secondary_text", "#140800"),
        ("tertiary_foreground", "#ffd9ad"),
        ("tertiary_text", "#140900"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect::<BTreeMap<_, _>>();
    let expected_colors = [
        ("window_frame_fg", "primary_foreground"),
        ("window_frame_bg", "surface"),
        ("window_active_fg", "primary_text"),
        ("window_active_bg", "primary"),
        ("window_inactive_fg", "secondary_text"),
        ("window_inactive_bg", "secondary"),
        ("pane_frame_active_fg", "secondary_text"),
        ("pane_frame_active_bg", "secondary"),
        ("pane_frame_inactive_fg", "muted"),
        ("pane_frame_inactive_bg", "surface"),
        ("pane_border_active_fg", "primary_foreground"),
        ("pane_border_active_bg", "surface"),
        ("pane_border_inactive_fg", "muted"),
        ("pane_border_inactive_bg", "surface"),
        ("pane_divider_fg", "tertiary_foreground"),
        ("pane_divider_bg", "surface"),
        ("frame_fill_fg", "foreground"),
        ("frame_fill_bg", "surface"),
        ("scroll_indicator_fg", "tertiary_text"),
        ("scroll_indicator_bg", "tertiary"),
        ("pane_pwd_fg", "muted_text"),
        ("pane_pwd_bg", "muted"),
        ("window_status_uptime_fg", "secondary_text"),
        ("window_status_uptime_bg", "secondary"),
        ("window_status_datetime_fg", "tertiary_text"),
        ("window_status_datetime_bg", "tertiary"),
        ("prompt_fg", "primary_foreground"),
        ("prompt_bg", "surface"),
        ("agent_prompt_fg", "#fff5f0"),
        ("agent_prompt_bg", "#301b12"),
        ("agent_transcript_user_fg", "primary_foreground"),
        ("agent_transcript_user_bg", "surface"),
        ("agent_transcript_assistant_fg", "secondary_foreground"),
        ("agent_transcript_assistant_bg", "surface"),
        ("agent_transcript_status_fg", "thinking"),
        ("agent_transcript_status_bg", "surface"),
        ("agent_transcript_error_fg", "danger_foreground"),
        ("agent_transcript_error_bg", "surface"),
        ("agent_transcript_command_fg", "tertiary_foreground"),
        ("agent_transcript_command_bg", "surface"),
        ("agent_model_fg", "secondary_text"),
        ("agent_model_bg", "secondary"),
        ("agent_reasoning_fg", "tertiary_text"),
        ("agent_reasoning_bg", "tertiary"),
        ("agent_status_idle_fg", "muted_text"),
        ("agent_status_idle_bg", "muted"),
        ("agent_status_running_fg", "primary_text"),
        ("agent_status_running_bg", "primary"),
        ("agent_status_blocked_fg", "tertiary_text"),
        ("agent_status_blocked_bg", "tertiary"),
        ("agent_status_failed_fg", "danger_text"),
        ("agent_status_failed_bg", "danger"),
        ("display_overlay_fg", "secondary_foreground"),
        ("display_overlay_bg", "surface"),
        ("copy_selection_fg", "tertiary_text"),
        ("copy_selection_bg", "tertiary"),
        ("syntax_plain_fg", "foreground"),
        ("syntax_plain_bg", "surface"),
        ("syntax_keyword_fg", "primary_foreground"),
        ("syntax_keyword_bg", "surface"),
        ("syntax_string_fg", "tertiary_foreground"),
        ("syntax_string_bg", "surface"),
        ("syntax_comment_fg", "thinking"),
        ("syntax_comment_bg", "surface"),
        ("syntax_type_fg", "secondary_foreground"),
        ("syntax_type_bg", "surface"),
        ("syntax_function_fg", "primary_foreground"),
        ("syntax_function_bg", "surface"),
        ("syntax_number_fg", "tertiary_foreground"),
        ("syntax_number_bg", "surface"),
        ("syntax_operator_fg", "muted"),
        ("syntax_operator_bg", "surface"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect::<BTreeMap<_, _>>();

    assert_eq!(definition.aliases, expected_aliases);
    assert_eq!(definition.colors, expected_colors);
}

/// Verifies each built-in theme has an explicit fidelity target.
///
/// Theme names can be exact upstream-family adaptations, interpretive family
/// adaptations, or Mezzanine-native palettes. Keeping the target table complete
/// makes future palette additions document whether they are intended to track an
/// external product family or define an original Mezzanine accessibility style.
#[test]
fn builtin_theme_fidelity_targets_cover_registry() {
    let targets = builtin_theme_fidelity_targets();
    let target_names = targets
        .iter()
        .map(|target| target.name)
        .collect::<BTreeSet<_>>();
    let registry_names = BUILTIN_UI_THEME_NAMES
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();

    assert_eq!(
        target_names, registry_names,
        "every built-in theme should have a documented fidelity target"
    );
}

/// Verifies upstream-family and interpretive built-ins retain expected anchors.
///
/// The derivation layer may contrast-manage secondary foreground aliases, but
/// the raw named anchors should stay pinned to the family colors documented for
/// each built-in. This guards future readability work from silently replacing a
/// familiar external palette with unrelated Mezzanine colors.
#[test]
fn builtin_theme_family_targets_keep_expected_palette_anchors() {
    for target in builtin_theme_fidelity_targets() {
        if target.category == ThemeFidelityCategory::Native {
            continue;
        }

        let definition = builtin_ui_theme_definition(target.name)
            .unwrap_or_else(|| panic!("missing theme {}", target.name));
        for &(alias, expected) in target.anchors {
            assert_eq!(
                definition.aliases.get(alias).map(String::as_str),
                Some(expected),
                "{} should keep {alias} anchored to its documented family color",
                target.name
            );
        }
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
        if builtin_theme_preserves_exact_snapshot(name) {
            continue;
        }
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

/// Verifies agent prompt input styling uses the configured prompt foreground
/// instead of recomputing a separate black-or-white contrast color.
#[test]
fn agent_prompt_input_rendition_uses_configured_prompt_foreground() {
    let mut theme = UiTheme::default();
    theme.colors.agent_prompt = UiColorPair {
        foreground: TerminalColor::Rgb(0xff, 0x00, 0x00),
        background: TerminalColor::Rgb(0x20, 0x20, 0x20),
    };

    let rendition = agent_prompt_input_rendition(&theme);

    assert_eq!(
        rendition.foreground,
        Some(theme.colors.agent_prompt.foreground)
    );
    assert_eq!(
        rendition.background,
        Some(theme.colors.agent_prompt.background)
    );
}
