//! Theme definitions for Mezzanine-owned terminal UI surfaces.
//!
//! Pane application output keeps using the SGR colors emitted by the pane
//! process. This module only describes colors that Mezzanine itself owns:
//! frames, pane dividers, transcript presentation, command and display overlays,
//! and copy selection.

use super::{
    BTreeMap, GraphicRendition, MezError, Result, TerminalColor,
    render::{
        shifted_channel, terminal_color_contrast_ratio, terminal_color_relative_luminance,
        terminal_color_rgb,
    },
};

/// User-configurable color slots for Mezzanine-owned UI components.
pub const UI_COLOR_SLOT_NAMES: &[&str] = &[
    "window_frame_fg",
    "window_frame_bg",
    "window_active_fg",
    "window_active_bg",
    "window_inactive_fg",
    "window_inactive_bg",
    "pane_frame_active_fg",
    "pane_frame_active_bg",
    "pane_frame_inactive_fg",
    "pane_frame_inactive_bg",
    "pane_border_active_fg",
    "pane_border_active_bg",
    "pane_border_inactive_fg",
    "pane_border_inactive_bg",
    "pane_divider_fg",
    "pane_divider_bg",
    "frame_fill_fg",
    "frame_fill_bg",
    "scroll_indicator_fg",
    "scroll_indicator_bg",
    "pane_pwd_fg",
    "pane_pwd_bg",
    "window_status_uptime_fg",
    "window_status_uptime_bg",
    "window_status_datetime_fg",
    "window_status_datetime_bg",
    "prompt_fg",
    "prompt_bg",
    "agent_prompt_fg",
    "agent_prompt_bg",
    "agent_transcript_user_fg",
    "agent_transcript_user_bg",
    "agent_transcript_assistant_fg",
    "agent_transcript_assistant_bg",
    "agent_transcript_status_fg",
    "agent_transcript_status_bg",
    "agent_transcript_error_fg",
    "agent_transcript_error_bg",
    "agent_transcript_command_fg",
    "agent_transcript_command_bg",
    "agent_model_fg",
    "agent_model_bg",
    "agent_reasoning_fg",
    "agent_reasoning_bg",
    "agent_status_idle_fg",
    "agent_status_idle_bg",
    "agent_status_running_fg",
    "agent_status_running_bg",
    "agent_status_blocked_fg",
    "agent_status_blocked_bg",
    "agent_status_failed_fg",
    "agent_status_failed_bg",
    "display_overlay_fg",
    "display_overlay_bg",
    "copy_selection_fg",
    "copy_selection_bg",
    "syntax_plain_fg",
    "syntax_plain_bg",
    "syntax_keyword_fg",
    "syntax_keyword_bg",
    "syntax_string_fg",
    "syntax_string_bg",
    "syntax_comment_fg",
    "syntax_comment_bg",
    "syntax_type_fg",
    "syntax_type_bg",
    "syntax_function_fg",
    "syntax_function_bg",
    "syntax_number_fg",
    "syntax_number_bg",
    "syntax_operator_fg",
    "syntax_operator_bg",
];

/// Built-in theme names accepted by `theme.active`.
pub const BUILTIN_UI_THEME_NAMES: &[&str] = &[
    "deepforest",
    "gruvbox_dark",
    "gruvbox_light",
    "solarized_dark",
    "solarized_light",
    "monokai",
    "dracula",
    "nord",
    "tokyo_night",
    "catppuccin_latte",
    "catppuccin_frappe",
    "catppuccin_macchiato",
    "catppuccin_mocha",
    "one_half_dark",
    "one_half_light",
    "onedark",
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
];

/// Built-in theme selected when no explicit `theme.active` setting is present.
pub const DEFAULT_UI_THEME_NAME: &str = "kanagawa";

const MIN_BUILTIN_LOW_EMPHASIS_CONTRAST_RATIO: f64 = 4.5;

/// Foreground/background color pair for a UI element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiColorPair {
    /// Text or glyph foreground color.
    pub foreground: TerminalColor,
    /// Full-row or component background color.
    pub background: TerminalColor,
}

impl UiColorPair {
    /// Converts the pair into terminal rendition attributes.
    pub fn rendition(self) -> GraphicRendition {
        GraphicRendition {
            foreground: Some(self.foreground),
            background: Some(self.background),
            ..GraphicRendition::default()
        }
    }
}

/// Resolved color assignments for every colored Mezzanine-owned UI component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiThemeColors {
    /// Non-pillbox custom window frame rows.
    pub window_frame: UiColorPair,
    /// Active window entry in the default window pillbox.
    pub window_active: UiColorPair,
    /// Inactive window entry in the default window pillbox.
    pub window_inactive: UiColorPair,
    /// Active pane frame/title row.
    pub pane_frame_active: UiColorPair,
    /// Inactive pane frame/title row.
    pub pane_frame_inactive: UiColorPair,
    /// Box-drawing border glyphs that enclose the active pane.
    pub pane_border_active: UiColorPair,
    /// Box-drawing border glyphs that do not enclose the active pane.
    pub pane_border_inactive: UiColorPair,
    /// Pane divider glyphs between split regions.
    pub pane_divider: UiColorPair,
    /// Subtle full-row fill behind frame surfaces.
    pub frame_fill: UiColorPair,
    /// Right-aligned scrollback position indicator on active pane frames.
    pub scroll_indicator: UiColorPair,
    /// Home-relative working-directory pill in pane frame status.
    pub pane_pwd: UiColorPair,
    /// System uptime item in the right side of the window status line.
    pub window_status_uptime: UiColorPair,
    /// Local datetime item in the right side of the window status line.
    pub window_status_datetime: UiColorPair,
    /// Command prompt rows.
    pub prompt: UiColorPair,
    /// Pane-local agent input prompt rows.
    pub agent_prompt: UiColorPair,
    /// Agent transcript gutter and user prompt label.
    pub agent_transcript_user: UiColorPair,
    /// Agent transcript gutter and assistant response label.
    pub agent_transcript_assistant: UiColorPair,
    /// Agent transcript status and thinking lines.
    pub agent_transcript_status: UiColorPair,
    /// Agent transcript error lines.
    pub agent_transcript_error: UiColorPair,
    /// Agent transcript command preview lines.
    pub agent_transcript_command: UiColorPair,
    /// Agent model pill in pane frame status.
    pub agent_model: UiColorPair,
    /// Agent reasoning pill in pane frame status.
    pub agent_reasoning: UiColorPair,
    /// Idle or completed agent status pill in pane frame status.
    pub agent_status_idle: UiColorPair,
    /// Running or queued agent status pill in pane frame status.
    pub agent_status_running: UiColorPair,
    /// Blocked agent status pill in pane frame status.
    pub agent_status_blocked: UiColorPair,
    /// Failed or interrupted agent status pill in pane frame status.
    pub agent_status_failed: UiColorPair,
    /// Command output, diagnostic, and prompt error overlays.
    pub display_overlay: UiColorPair,
    /// Copy-mode selection highlight.
    pub copy_selection: UiColorPair,
    /// Default syntax-highlighted source text.
    pub syntax_plain: UiColorPair,
    /// Syntax-highlighted language keywords and storage modifiers.
    pub syntax_keyword: UiColorPair,
    /// Syntax-highlighted string and character literals.
    pub syntax_string: UiColorPair,
    /// Syntax-highlighted comments and documentation comments.
    pub syntax_comment: UiColorPair,
    /// Syntax-highlighted type, class, and module names.
    pub syntax_type: UiColorPair,
    /// Syntax-highlighted function and method names.
    pub syntax_function: UiColorPair,
    /// Syntax-highlighted numeric and constant literals.
    pub syntax_number: UiColorPair,
    /// Syntax-highlighted operators and punctuation.
    pub syntax_operator: UiColorPair,
}

/// Resolved active UI theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiTheme {
    /// Active theme name from config.
    pub name: String,
    /// Resolved aliases available to theme color references.
    pub aliases: BTreeMap<String, TerminalColor>,
    /// Resolved per-component colors.
    pub colors: UiThemeColors,
}

impl Default for UiTheme {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        default_ui_theme()
    }
}

/// String-based theme definition before alias/color resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiThemeDefinition {
    /// Alias to hex-code mappings.
    pub aliases: BTreeMap<String, String>,
    /// Flat color slot to alias-or-hex mappings.
    pub colors: BTreeMap<String, String>,
}

impl UiThemeDefinition {
    /// Overlay another theme definition, replacing only supplied aliases/slots.
    pub fn merge(&mut self, other: UiThemeDefinition) {
        self.aliases.extend(other.aliases);
        self.colors.extend(other.colors);
    }
}

/// Returns the named built-in theme definition.
pub fn builtin_ui_theme_definition(name: &str) -> Option<UiThemeDefinition> {
    match name {
        "deepforest" => Some(definition_from_palette(UiThemePalette {
            primary: "#57c785",
            secondary: "#3f8f68",
            tertiary: "#d7c46a",
            surface: "#0b1f17",
            foreground: "#e4efe8",
            muted: "#8fa99a",
            thinking: "#9aa69e",
            danger: "#c05f5f",
            agent_prompt_background: "#252827",
        })),
        "gruvbox_dark" => Some(definition_from_palette(UiThemePalette {
            primary: "#fabd2f",
            secondary: "#83a598",
            tertiary: "#b8bb26",
            surface: "#282828",
            foreground: "#ebdbb2",
            muted: "#a89984",
            thinking: "#bdae93",
            danger: "#fb4934",
            agent_prompt_background: "#32302f",
        })),
        "gruvbox_light" => Some(definition_from_palette(UiThemePalette {
            primary: "#b57614",
            secondary: "#076678",
            tertiary: "#79740e",
            surface: "#fbf1c7",
            foreground: "#3c3836",
            muted: "#7c6f64",
            thinking: "#928374",
            danger: "#cc241d",
            agent_prompt_background: "#eeeeee",
        })),
        "solarized_dark" => Some(definition_from_palette(UiThemePalette {
            primary: "#268bd2",
            secondary: "#2aa198",
            tertiary: "#b58900",
            surface: "#002b36",
            foreground: "#eee8d5",
            muted: "#839496",
            thinking: "#93a1a1",
            danger: "#dc322f",
            agent_prompt_background: "#073642",
        })),
        "solarized_light" => Some(definition_from_palette(UiThemePalette {
            primary: "#268bd2",
            secondary: "#2aa198",
            tertiary: "#b58900",
            surface: "#fdf6e3",
            foreground: "#073642",
            muted: "#657b83",
            thinking: "#839496",
            danger: "#dc322f",
            agent_prompt_background: "#eee8d5",
        })),
        "monokai" => Some(definition_from_palette(UiThemePalette {
            primary: "#a6e22e",
            secondary: "#66d9ef",
            tertiary: "#fd971f",
            surface: "#272822",
            foreground: "#f8f8f2",
            muted: "#a59f85",
            thinking: "#bcbcaf",
            danger: "#f92672",
            agent_prompt_background: "#34352f",
        })),
        "dracula" => Some(definition_from_palette(UiThemePalette {
            primary: "#bd93f9",
            secondary: "#8be9fd",
            tertiary: "#ffb86c",
            surface: "#282a36",
            foreground: "#f8f8f2",
            muted: "#6272a4",
            thinking: "#b8bfd9",
            danger: "#ff5555",
            agent_prompt_background: "#343746",
        })),
        "nord" => Some(definition_from_palette(UiThemePalette {
            primary: "#88c0d0",
            secondary: "#81a1c1",
            tertiary: "#a3be8c",
            surface: "#2e3440",
            foreground: "#eceff4",
            muted: "#4c566a",
            thinking: "#aeb6c4",
            danger: "#bf616a",
            agent_prompt_background: "#3b4252",
        })),
        "tokyo_night" => Some(definition_from_palette(UiThemePalette {
            primary: "#7aa2f7",
            secondary: "#bb9af7",
            tertiary: "#e0af68",
            surface: "#1a1b26",
            foreground: "#c0caf5",
            muted: "#565f89",
            thinking: "#a9b1d6",
            danger: "#f7768e",
            agent_prompt_background: "#24283b",
        })),
        "catppuccin_latte" => Some(definition_from_palette(UiThemePalette {
            primary: "#1e66f5",
            secondary: "#179299",
            tertiary: "#df8e1d",
            surface: "#eff1f5",
            foreground: "#4c4f69",
            muted: "#7c7f93",
            thinking: "#8c8fa1",
            danger: "#d20f39",
            agent_prompt_background: "#e6e9ef",
        })),
        "catppuccin_frappe" => Some(definition_from_palette(UiThemePalette {
            primary: "#8caaee",
            secondary: "#81c8be",
            tertiary: "#e5c890",
            surface: "#303446",
            foreground: "#c6d0f5",
            muted: "#838ba7",
            thinking: "#a5adce",
            danger: "#e78284",
            agent_prompt_background: "#414559",
        })),
        "catppuccin_macchiato" => Some(definition_from_palette(UiThemePalette {
            primary: "#8aadf4",
            secondary: "#8bd5ca",
            tertiary: "#eed49f",
            surface: "#24273a",
            foreground: "#cad3f5",
            muted: "#8087a2",
            thinking: "#a5adcb",
            danger: "#ed8796",
            agent_prompt_background: "#363a4f",
        })),
        "catppuccin_mocha" => Some(definition_from_palette(UiThemePalette {
            primary: "#cba6f7",
            secondary: "#89b4fa",
            tertiary: "#f9e2af",
            surface: "#1e1e2e",
            foreground: "#cdd6f4",
            muted: "#6c7086",
            thinking: "#a6adc8",
            danger: "#f38ba8",
            agent_prompt_background: "#313244",
        })),
        "one_half_dark" => Some(definition_from_palette(UiThemePalette {
            primary: "#61afef",
            secondary: "#56b6c2",
            tertiary: "#e5c07b",
            surface: "#282c34",
            foreground: "#dcdfe4",
            muted: "#5c6370",
            thinking: "#abb2bf",
            danger: "#e06c75",
            agent_prompt_background: "#313640",
        })),
        "one_half_light" => Some(definition_from_palette(UiThemePalette {
            primary: "#0184bc",
            secondary: "#0997b3",
            tertiary: "#c18401",
            surface: "#fafafa",
            foreground: "#383a42",
            muted: "#696c77",
            thinking: "#7f848e",
            danger: "#e45649",
            agent_prompt_background: "#f0f0f0",
        })),
        "onedark" => Some(definition_from_palette(UiThemePalette {
            primary: "#61afef",
            secondary: "#98c379",
            tertiary: "#e5c07b",
            surface: "#282c34",
            foreground: "#abb2bf",
            muted: "#5c6370",
            thinking: "#9da5b4",
            danger: "#e06c75",
            agent_prompt_background: "#2c313a",
        })),
        "rose_pine" => Some(definition_from_palette(UiThemePalette {
            primary: "#c4a7e7",
            secondary: "#9ccfd8",
            tertiary: "#f6c177",
            surface: "#191724",
            foreground: "#e0def4",
            muted: "#6e6a86",
            thinking: "#908caa",
            danger: "#eb6f92",
            agent_prompt_background: "#1f1d2e",
        })),
        "rose_pine_moon" => Some(definition_from_palette(UiThemePalette {
            primary: "#c4a7e7",
            secondary: "#9ccfd8",
            tertiary: "#f6c177",
            surface: "#232136",
            foreground: "#e0def4",
            muted: "#6e6a86",
            thinking: "#908caa",
            danger: "#eb6f92",
            agent_prompt_background: "#2a273f",
        })),
        "rose_pine_dawn" => Some(definition_from_palette(UiThemePalette {
            primary: "#907aa9",
            secondary: "#56949f",
            tertiary: "#ea9d34",
            surface: "#faf4ed",
            foreground: "#575279",
            muted: "#9893a5",
            thinking: "#797593",
            danger: "#b4637a",
            agent_prompt_background: "#fffaf3",
        })),
        "kanagawa" => Some(definition_from_palette(UiThemePalette {
            primary: "#7e9cd8",
            secondary: "#7aa89f",
            tertiary: "#e6c384",
            surface: "#1f1f28",
            foreground: "#dcd7ba",
            muted: "#727169",
            thinking: "#938aa9",
            danger: "#e82424",
            agent_prompt_background: "#2a2a37",
        })),
        "everforest_dark" => Some(definition_from_palette(UiThemePalette {
            primary: "#a7c080",
            secondary: "#7fbbb3",
            tertiary: "#dbbc7f",
            surface: "#2d353b",
            foreground: "#d3c6aa",
            muted: "#7a8478",
            thinking: "#9da9a0",
            danger: "#e67e80",
            agent_prompt_background: "#343f44",
        })),
        "everforest_light" => Some(definition_from_palette(UiThemePalette {
            primary: "#8da101",
            secondary: "#35a77c",
            tertiary: "#dfa000",
            surface: "#fff9e8",
            foreground: "#5c6a72",
            muted: "#939f91",
            thinking: "#7a8478",
            danger: "#f85552",
            agent_prompt_background: "#f7f2e0",
        })),
        "ayu" | "ayu_dark" => Some(definition_from_palette(UiThemePalette {
            primary: "#59c2ff",
            secondary: "#95e6cb",
            tertiary: "#e6b450",
            surface: "#0a0e14",
            foreground: "#b3b1ad",
            muted: "#626a73",
            thinking: "#9aa0a6",
            danger: "#f07178",
            agent_prompt_background: "#151a21",
        })),
        "ayu_light" => Some(definition_from_palette(UiThemePalette {
            primary: "#55b4d4",
            secondary: "#4cbf99",
            tertiary: "#f2ae49",
            surface: "#fafafa",
            foreground: "#5c6773",
            muted: "#abb0b6",
            thinking: "#7f8790",
            danger: "#f07178",
            agent_prompt_background: "#f0f0f0",
        })),
        "ayu_mirage" => Some(definition_from_palette(UiThemePalette {
            primary: "#73d0ff",
            secondary: "#95e6cb",
            tertiary: "#ffd580",
            surface: "#1f2430",
            foreground: "#cbccc6",
            muted: "#5c6773",
            thinking: "#a0a7b0",
            danger: "#f28779",
            agent_prompt_background: "#252b38",
        })),
        "high_contrast_dark" => Some(definition_from_palette(UiThemePalette {
            primary: "#00ffff",
            secondary: "#00ff00",
            tertiary: "#ffff00",
            surface: "#000000",
            foreground: "#ffffff",
            muted: "#bdbdbd",
            thinking: "#c6c6c6",
            danger: "#ff0000",
            agent_prompt_background: "#1a1a1a",
        })),
        "high_contrast_light" => Some(definition_from_palette(UiThemePalette {
            primary: "#0000ff",
            secondary: "#007a3d",
            tertiary: "#795e26",
            surface: "#ffffff",
            foreground: "#000000",
            muted: "#666666",
            thinking: "#6e6e6e",
            danger: "#b00020",
            agent_prompt_background: "#f2f2f2",
        })),
        _ => None,
    }
}

/// Returns whether `name` is a built-in UI theme.
pub fn is_builtin_ui_theme(name: &str) -> bool {
    BUILTIN_UI_THEME_NAMES.contains(&name)
}

/// Resolves a string-based theme definition into terminal colors.
pub fn resolve_ui_theme(name: &str, definition: UiThemeDefinition) -> Result<UiTheme> {
    let aliases = resolve_aliases(&definition.aliases)?;
    let colors = UiThemeColors {
        window_frame: pair_from_slots(&definition.colors, &aliases, "window_frame")?,
        window_active: pair_from_slots(&definition.colors, &aliases, "window_active")?,
        window_inactive: pair_from_slots(&definition.colors, &aliases, "window_inactive")?,
        pane_frame_active: pair_from_slots(&definition.colors, &aliases, "pane_frame_active")?,
        pane_frame_inactive: pair_from_slots(&definition.colors, &aliases, "pane_frame_inactive")?,
        pane_border_active: pair_from_slots(&definition.colors, &aliases, "pane_border_active")?,
        pane_border_inactive: pair_from_slots(
            &definition.colors,
            &aliases,
            "pane_border_inactive",
        )?,
        pane_divider: pair_from_slots(&definition.colors, &aliases, "pane_divider")?,
        frame_fill: pair_from_slots(&definition.colors, &aliases, "frame_fill")?,
        scroll_indicator: pair_from_slots(&definition.colors, &aliases, "scroll_indicator")?,
        pane_pwd: pair_from_slots(&definition.colors, &aliases, "pane_pwd")?,
        window_status_uptime: pair_from_slots(
            &definition.colors,
            &aliases,
            "window_status_uptime",
        )?,
        window_status_datetime: pair_from_slots(
            &definition.colors,
            &aliases,
            "window_status_datetime",
        )?,
        prompt: pair_from_slots(&definition.colors, &aliases, "prompt")?,
        agent_prompt: pair_from_slots(&definition.colors, &aliases, "agent_prompt")?,
        agent_transcript_user: pair_from_slots(
            &definition.colors,
            &aliases,
            "agent_transcript_user",
        )?,
        agent_transcript_assistant: pair_from_slots(
            &definition.colors,
            &aliases,
            "agent_transcript_assistant",
        )?,
        agent_transcript_status: pair_from_slots(
            &definition.colors,
            &aliases,
            "agent_transcript_status",
        )?,
        agent_transcript_error: pair_from_slots(
            &definition.colors,
            &aliases,
            "agent_transcript_error",
        )?,
        agent_transcript_command: pair_from_slots(
            &definition.colors,
            &aliases,
            "agent_transcript_command",
        )?,
        agent_model: pair_from_slots(&definition.colors, &aliases, "agent_model")?,
        agent_reasoning: pair_from_slots(&definition.colors, &aliases, "agent_reasoning")?,
        agent_status_idle: pair_from_slots(&definition.colors, &aliases, "agent_status_idle")?,
        agent_status_running: pair_from_slots(
            &definition.colors,
            &aliases,
            "agent_status_running",
        )?,
        agent_status_blocked: pair_from_slots(
            &definition.colors,
            &aliases,
            "agent_status_blocked",
        )?,
        agent_status_failed: pair_from_slots(&definition.colors, &aliases, "agent_status_failed")?,
        display_overlay: pair_from_slots(&definition.colors, &aliases, "display_overlay")?,
        copy_selection: pair_from_slots(&definition.colors, &aliases, "copy_selection")?,
        syntax_plain: pair_from_slots(&definition.colors, &aliases, "syntax_plain")?,
        syntax_keyword: pair_from_slots(&definition.colors, &aliases, "syntax_keyword")?,
        syntax_string: pair_from_slots(&definition.colors, &aliases, "syntax_string")?,
        syntax_comment: pair_from_slots(&definition.colors, &aliases, "syntax_comment")?,
        syntax_type: pair_from_slots(&definition.colors, &aliases, "syntax_type")?,
        syntax_function: pair_from_slots(&definition.colors, &aliases, "syntax_function")?,
        syntax_number: pair_from_slots(&definition.colors, &aliases, "syntax_number")?,
        syntax_operator: pair_from_slots(&definition.colors, &aliases, "syntax_operator")?,
    };
    Ok(UiTheme {
        name: name.to_string(),
        aliases,
        colors,
    })
}

/// Parses `#rgb` or `#rrggbb` into a true-color terminal color.
pub fn parse_hex_color(value: &str) -> Option<TerminalColor> {
    let hex = value.strip_prefix('#')?;
    let bytes = hex.as_bytes();
    match bytes.len() {
        3 => {
            let red = duplicate_hex_nibble(bytes[0])?;
            let green = duplicate_hex_nibble(bytes[1])?;
            let blue = duplicate_hex_nibble(bytes[2])?;
            Some(TerminalColor::Rgb(red, green, blue))
        }
        6 => {
            let red = parse_hex_byte(bytes[0], bytes[1])?;
            let green = parse_hex_byte(bytes[2], bytes[3])?;
            let blue = parse_hex_byte(bytes[4], bytes[5])?;
            Some(TerminalColor::Rgb(red, green, blue))
        }
        _ => None,
    }
}

/// Returns true if `value` is a valid color alias identifier.
pub fn valid_color_alias_name(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

/// Returns the resolved default built-in theme.
pub fn default_ui_theme() -> UiTheme {
    if let Some(definition) = builtin_ui_theme_definition(DEFAULT_UI_THEME_NAME)
        && let Ok(theme) = resolve_ui_theme(DEFAULT_UI_THEME_NAME, definition)
    {
        return theme;
    }
    deepforest_ui_theme()
}

/// Returns the resolved `deepforest` theme.
pub fn deepforest_ui_theme() -> UiTheme {
    UiTheme {
        name: "deepforest".to_string(),
        aliases: [
            ("primary".to_string(), TerminalColor::Rgb(0x57, 0xc7, 0x85)),
            (
                "secondary".to_string(),
                TerminalColor::Rgb(0x3f, 0x8f, 0x68),
            ),
            ("tertiary".to_string(), TerminalColor::Rgb(0xd7, 0xc4, 0x6a)),
            ("surface".to_string(), TerminalColor::Rgb(0x0b, 0x1f, 0x17)),
            ("danger".to_string(), TerminalColor::Rgb(0xc0, 0x5f, 0x5f)),
            (
                "foreground".to_string(),
                TerminalColor::Rgb(0xe4, 0xef, 0xe8),
            ),
            ("muted".to_string(), TerminalColor::Rgb(0x8f, 0xa9, 0x9a)),
            ("thinking".to_string(), TerminalColor::Rgb(0x9a, 0xa6, 0x9e)),
        ]
        .into_iter()
        .collect(),
        colors: UiThemeColors {
            window_frame: pair("#57c785", "#0b1f17"),
            window_active: pair("#0b1f17", "#57c785"),
            window_inactive: pair("#e4efe8", "#3f8f68"),
            pane_frame_active: pair("#e4efe8", "#3f8f68"),
            pane_frame_inactive: pair("#8fa99a", "#0b1f17"),
            pane_border_active: pair("#57c785", "#0b1f17"),
            pane_border_inactive: pair("#8fa99a", "#0b1f17"),
            pane_divider: pair("#d7c46a", "#0b1f17"),
            frame_fill: pair("#e4efe8", "#0b1f17"),
            scroll_indicator: pair("#0b1f17", "#d7c46a"),
            pane_pwd: pair("#0b1f17", "#8fa99a"),
            window_status_uptime: pair("#0b1f17", "#3f8f68"),
            window_status_datetime: pair("#0b1f17", "#d7c46a"),
            prompt: pair("#57c785", "#0b1f17"),
            agent_prompt: pair("#57c785", "#252827"),
            agent_transcript_user: pair("#57c785", "#0b1f17"),
            agent_transcript_assistant: pair("#3f8f68", "#0b1f17"),
            agent_transcript_status: pair("#9aa69e", "#0b1f17"),
            agent_transcript_error: pair("#c05f5f", "#0b1f17"),
            agent_transcript_command: pair("#d7c46a", "#0b1f17"),
            agent_model: pair("#0b1f17", "#3f8f68"),
            agent_reasoning: pair("#0b1f17", "#d7c46a"),
            agent_status_idle: pair("#0b1f17", "#8fa99a"),
            agent_status_running: pair("#0b1f17", "#57c785"),
            agent_status_blocked: pair("#0b1f17", "#d7c46a"),
            agent_status_failed: pair("#0b1f17", "#c05f5f"),
            display_overlay: pair("#3f8f68", "#0b1f17"),
            copy_selection: pair("#0b1f17", "#d7c46a"),
            syntax_plain: pair("#e4efe8", "#0b1f17"),
            syntax_keyword: pair("#57c785", "#0b1f17"),
            syntax_string: pair("#d7c46a", "#0b1f17"),
            syntax_comment: pair("#9aa69e", "#0b1f17"),
            syntax_type: pair("#3f8f68", "#0b1f17"),
            syntax_function: pair("#57c785", "#0b1f17"),
            syntax_number: pair("#d7c46a", "#0b1f17"),
            syntax_operator: pair("#8fa99a", "#0b1f17"),
        },
    }
}

/// Named palette inputs used to derive a complete built-in UI theme.
struct UiThemePalette<'a> {
    /// High-impact accent used for active surfaces and user transcript labels.
    primary: &'a str,
    /// Secondary accent used for inactive surfaces and assistant labels.
    secondary: &'a str,
    /// Tertiary accent used for warning, command, and selection surfaces.
    tertiary: &'a str,
    /// Base surface color used for frame and transcript backgrounds.
    surface: &'a str,
    /// Default high-contrast foreground color for frame text.
    foreground: &'a str,
    /// Muted foreground color used for inactive and idle status text.
    muted: &'a str,
    /// Theme-relative grey equivalent used for agent thinking text.
    thinking: &'a str,
    /// Error accent used for failures and transcript error lines.
    danger: &'a str,
    /// Slightly lifted surface used by the pane-local agent prompt.
    agent_prompt_background: &'a str,
}

/// Derives a complete theme definition from one named built-in palette.
fn definition_from_palette(palette: UiThemePalette<'_>) -> UiThemeDefinition {
    let muted = contrast_managed_palette_hex(palette.muted, palette.surface);
    let thinking = visible_thinking_palette_hex(palette.thinking, palette.surface);
    let aliases = [
        ("primary", palette.primary.to_string()),
        ("secondary", palette.secondary.to_string()),
        ("tertiary", palette.tertiary.to_string()),
        ("surface", palette.surface.to_string()),
        ("foreground", palette.foreground.to_string()),
        ("muted", muted),
        ("thinking", thinking),
        ("danger", palette.danger.to_string()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect();
    let agent_prompt_foreground =
        contrasting_binary_hex_for_background(palette.agent_prompt_background);
    let mut colors: BTreeMap<String, String> = [
        ("window_frame_fg", "primary"),
        ("window_frame_bg", "surface"),
        ("window_active_fg", "surface"),
        ("window_active_bg", "primary"),
        ("window_inactive_fg", "foreground"),
        ("window_inactive_bg", "secondary"),
        ("pane_frame_active_fg", "foreground"),
        ("pane_frame_active_bg", "secondary"),
        ("pane_frame_inactive_fg", "muted"),
        ("pane_frame_inactive_bg", "surface"),
        ("pane_border_active_fg", "primary"),
        ("pane_border_active_bg", "surface"),
        ("pane_border_inactive_fg", "muted"),
        ("pane_border_inactive_bg", "surface"),
        ("pane_divider_fg", "tertiary"),
        ("pane_divider_bg", "surface"),
        ("frame_fill_fg", "foreground"),
        ("frame_fill_bg", "surface"),
        ("scroll_indicator_fg", "surface"),
        ("scroll_indicator_bg", "tertiary"),
        ("pane_pwd_fg", "surface"),
        ("pane_pwd_bg", "muted"),
        ("window_status_uptime_fg", "surface"),
        ("window_status_uptime_bg", "secondary"),
        ("window_status_datetime_fg", "surface"),
        ("window_status_datetime_bg", "tertiary"),
        ("prompt_fg", "primary"),
        ("prompt_bg", "surface"),
        ("agent_prompt_bg", palette.agent_prompt_background),
        ("agent_transcript_user_fg", "primary"),
        ("agent_transcript_user_bg", "surface"),
        ("agent_transcript_assistant_fg", "secondary"),
        ("agent_transcript_assistant_bg", "surface"),
        ("agent_transcript_status_fg", "thinking"),
        ("agent_transcript_status_bg", "surface"),
        ("agent_transcript_error_fg", "danger"),
        ("agent_transcript_error_bg", "surface"),
        ("agent_transcript_command_fg", "tertiary"),
        ("agent_transcript_command_bg", "surface"),
        ("agent_model_fg", "surface"),
        ("agent_model_bg", "secondary"),
        ("agent_reasoning_fg", "surface"),
        ("agent_reasoning_bg", "tertiary"),
        ("agent_status_idle_fg", "surface"),
        ("agent_status_idle_bg", "muted"),
        ("agent_status_running_fg", "surface"),
        ("agent_status_running_bg", "primary"),
        ("agent_status_blocked_fg", "surface"),
        ("agent_status_blocked_bg", "tertiary"),
        ("agent_status_failed_fg", "surface"),
        ("agent_status_failed_bg", "danger"),
        ("display_overlay_fg", "secondary"),
        ("display_overlay_bg", "surface"),
        ("copy_selection_fg", "surface"),
        ("copy_selection_bg", "tertiary"),
        ("syntax_plain_fg", "foreground"),
        ("syntax_plain_bg", "surface"),
        ("syntax_keyword_fg", "primary"),
        ("syntax_keyword_bg", "surface"),
        ("syntax_string_fg", "tertiary"),
        ("syntax_string_bg", "surface"),
        ("syntax_comment_fg", "thinking"),
        ("syntax_comment_bg", "surface"),
        ("syntax_type_fg", "secondary"),
        ("syntax_type_bg", "surface"),
        ("syntax_function_fg", "primary"),
        ("syntax_function_bg", "surface"),
        ("syntax_number_fg", "tertiary"),
        ("syntax_number_bg", "surface"),
        ("syntax_operator_fg", "muted"),
        ("syntax_operator_bg", "surface"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect();
    colors.insert(
        "agent_prompt_fg".to_string(),
        agent_prompt_foreground.to_string(),
    );
    UiThemeDefinition { aliases, colors }
}

/// Returns black or white for one true-color background string.
fn contrasting_binary_hex_for_background(background: &str) -> &'static str {
    let Some(TerminalColor::Rgb(red, green, blue)) = parse_hex_color(background) else {
        return "#ffffff";
    };
    let luminance = (u32::from(red) * 299 + u32::from(green) * 587 + u32::from(blue) * 114) / 1000;
    if luminance >= 140 {
        "#000000"
    } else {
        "#ffffff"
    }
}

/// Returns a palette color adjusted only when needed to be readable.
///
/// Built-in themes intentionally carry lower-emphasis text, but those colors
/// still need normal text contrast against the theme surface. The adjustment
/// shifts all channels together so the source palette's hue remains recognizable.
fn contrast_managed_palette_hex(foreground: &str, background: &str) -> String {
    let Some(foreground_color) = parse_hex_color(foreground) else {
        return foreground.to_string();
    };
    let Some(background_color) = parse_hex_color(background) else {
        return foreground.to_string();
    };
    if terminal_color_contrast_ratio(foreground_color, background_color)
        .is_some_and(|ratio| ratio >= MIN_BUILTIN_LOW_EMPHASIS_CONTRAST_RATIO)
    {
        return foreground.to_string();
    }
    let Some((red, green, blue)) = terminal_color_rgb(foreground_color) else {
        return foreground.to_string();
    };
    let Some(background_luminance) = terminal_color_relative_luminance(background_color) else {
        return foreground.to_string();
    };
    let shift_direction = if background_luminance >= 0.5 { -1 } else { 1 };
    for shift in 1..=255 {
        let shifted = TerminalColor::Rgb(
            shifted_channel(red, shift * shift_direction),
            shifted_channel(green, shift * shift_direction),
            shifted_channel(blue, shift * shift_direction),
        );
        if terminal_color_contrast_ratio(shifted, background_color)
            .is_some_and(|ratio| ratio >= MIN_BUILTIN_LOW_EMPHASIS_CONTRAST_RATIO)
        {
            return terminal_color_to_hex(shifted);
        }
    }
    if background_luminance >= 0.5 {
        "#000000".to_string()
    } else {
        "#ffffff".to_string()
    }
}

/// Returns a readable thinking/status color with a brighter floor on dark UI
/// surfaces.
///
/// Thinking lines are rendered with a lower-emphasis presentation on top of
/// this color, so dark themes need a slightly brighter neutral before runtime
/// dimming is applied.
fn visible_thinking_palette_hex(foreground: &str, background: &str) -> String {
    const DARK_SURFACE_MIN_THINKING_AVERAGE: u16 = 165;

    let managed = contrast_managed_palette_hex(foreground, background);
    let Some(managed_color) = parse_hex_color(&managed) else {
        return managed;
    };
    let Some(background_color) = parse_hex_color(background) else {
        return managed;
    };
    let Some(background_luminance) = terminal_color_relative_luminance(background_color) else {
        return managed;
    };
    if background_luminance >= 0.5 {
        return managed;
    }
    let Some((red, green, blue)) = terminal_color_rgb(managed_color) else {
        return managed;
    };
    let average = (u16::from(red) + u16::from(green) + u16::from(blue)) / 3;
    if average >= DARK_SURFACE_MIN_THINKING_AVERAGE {
        return managed;
    }
    for shift in 1..=255 {
        let shifted = TerminalColor::Rgb(
            shifted_channel(red, shift),
            shifted_channel(green, shift),
            shifted_channel(blue, shift),
        );
        let Some((shifted_red, shifted_green, shifted_blue)) = terminal_color_rgb(shifted) else {
            continue;
        };
        let shifted_average =
            (u16::from(shifted_red) + u16::from(shifted_green) + u16::from(shifted_blue)) / 3;
        if shifted_average >= DARK_SURFACE_MIN_THINKING_AVERAGE
            && terminal_color_contrast_ratio(shifted, background_color)
                .is_some_and(|ratio| ratio >= MIN_BUILTIN_LOW_EMPHASIS_CONTRAST_RATIO)
        {
            return terminal_color_to_hex(shifted);
        }
    }
    managed
}

/// Renders one true-color value as a six-digit lowercase hex string.
fn terminal_color_to_hex(color: TerminalColor) -> String {
    match color {
        TerminalColor::Rgb(red, green, blue) => {
            format!("#{red:02x}{green:02x}{blue:02x}")
        }
        TerminalColor::Indexed(index) => format!("#{index:02x}{index:02x}{index:02x}"),
    }
}

/// Runs the resolve aliases operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn resolve_aliases(aliases: &BTreeMap<String, String>) -> Result<BTreeMap<String, TerminalColor>> {
    let mut resolved = BTreeMap::new();
    for (alias, value) in aliases {
        if !valid_color_alias_name(alias) {
            return Err(MezError::config(format!(
                "theme alias `{alias}` must be an identifier"
            )));
        }
        let Some(color) = parse_hex_color(value) else {
            return Err(MezError::config(format!(
                "theme.aliases.{alias} must be a #rgb or #rrggbb hex color"
            )));
        };
        resolved.insert(alias.clone(), color);
    }
    Ok(resolved)
}

/// Runs the pair from slots operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pair_from_slots(
    colors: &BTreeMap<String, String>,
    aliases: &BTreeMap<String, TerminalColor>,
    component: &str,
) -> Result<UiColorPair> {
    let foreground = color_from_slot(colors, aliases, &format!("{component}_fg"))?;
    let background = color_from_slot(colors, aliases, &format!("{component}_bg"))?;
    Ok(UiColorPair {
        foreground,
        background,
    })
}

/// Runs the color from slot operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn color_from_slot(
    colors: &BTreeMap<String, String>,
    aliases: &BTreeMap<String, TerminalColor>,
    slot: &str,
) -> Result<TerminalColor> {
    let value = colors
        .get(slot)
        .ok_or_else(|| MezError::config(format!("theme.colors.{slot} is required")))?;
    resolve_color_reference(value, aliases).ok_or_else(|| {
        MezError::config(format!("theme.colors.{slot} must be a hex color or alias"))
    })
}

/// Runs the resolve color reference operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn resolve_color_reference(
    value: &str,
    aliases: &BTreeMap<String, TerminalColor>,
) -> Option<TerminalColor> {
    parse_hex_color(value).or_else(|| aliases.get(value).copied())
}

/// Runs the duplicate hex nibble operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn duplicate_hex_nibble(value: u8) -> Option<u8> {
    let digit = hex_digit(value)?;
    u8::try_from(digit * 17).ok()
}

/// Runs the parse hex byte operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_hex_byte(high: u8, low: u8) -> Option<u8> {
    let high = hex_digit(high)?;
    let low = hex_digit(low)?;
    u8::try_from((high << 4) | low).ok()
}

/// Runs the hex digit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn hex_digit(value: u8) -> Option<u32> {
    match value {
        b'0'..=b'9' => Some(u32::from(value - b'0')),
        b'a'..=b'f' => Some(u32::from(value - b'a' + 10)),
        b'A'..=b'F' => Some(u32::from(value - b'A' + 10)),
        _ => None,
    }
}

/// Runs the pair operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pair(foreground: &str, background: &str) -> UiColorPair {
    UiColorPair {
        foreground: parse_hex_color(foreground).unwrap_or(TerminalColor::Rgb(255, 255, 255)),
        background: parse_hex_color(background).unwrap_or(TerminalColor::Rgb(0, 0, 0)),
    }
}
