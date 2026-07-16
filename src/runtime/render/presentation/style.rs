//! Terminal presentation styles, palettes, and SGR projection.

use super::*;

/// Carries Agent Terminal Presentation Style state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentTerminalPresentationStyle {
    /// Represents the User Prompt case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    UserPrompt,
    /// Represents the Assistant case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Assistant,
    /// Represents the Status case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Status,
    /// Represents the Error case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Error,
    /// Represents the Command case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Command,
    /// Represents markdown or rich informational output produced by an agent shell command.
    ///
    /// Command display blocks are not model-authored assistant messages, so they
    /// render without an `agent>` label and use a neutral gutter.
    CommandDisplay,
    /// Represents diff metadata such as file labels and hunk headers.
    ///
    /// Mutating semantic actions use this style for bounded change previews
    /// rendered after hidden shell execution has completed.
    DiffHeader,
    /// Represents added lines in a mutating semantic action preview.
    ///
    /// The style intentionally reuses the user/accent color family so additions
    /// are visible without introducing another required theme slot.
    DiffAddition,
    /// Represents removed lines in a mutating semantic action preview.
    ///
    /// The style intentionally reuses the error color family so removals read as
    /// destructive or subtractive change without adding a new theme slot.
    DiffDeletion,
    /// Represents unchanged context lines in a mutating semantic action preview.
    ///
    /// Context stays visually quieter than additions and deletions while
    /// retaining enough contrast to make line numbers and nearby text readable.
    DiffContext,
}

impl AgentTerminalPresentationStyle {
    /// Returns the theme color pair for this agent transcript presentation.
    pub(super) fn color_pair(self, ui_theme: &UiTheme) -> UiColorPair {
        match self {
            Self::UserPrompt => ui_theme.colors.agent_transcript_user,
            Self::Assistant => ui_theme.colors.agent_transcript_assistant,
            Self::Status => ui_theme.colors.agent_transcript_status,
            Self::Error => ui_theme.colors.agent_transcript_error,
            Self::Command => ui_theme.colors.agent_transcript_command,
            Self::CommandDisplay => ui_theme.colors.frame_fill,
            Self::DiffHeader => UiColorPair {
                foreground: ui_theme.colors.agent_transcript_command.foreground,
                background: ui_theme.colors.frame_fill.background,
            },
            Self::DiffAddition => UiColorPair {
                foreground: ui_theme.colors.agent_transcript_user.foreground,
                background: ui_theme.colors.frame_fill.background,
            },
            Self::DiffDeletion => UiColorPair {
                foreground: ui_theme.colors.agent_transcript_error.foreground,
                background: ui_theme.colors.frame_fill.background,
            },
            Self::DiffContext => UiColorPair {
                foreground: ui_theme.colors.agent_transcript_status.foreground,
                background: ui_theme.colors.frame_fill.background,
            },
        }
    }

    /// Returns the SGR prefix used before rendering a transcript gutter.
    pub(super) fn sgr_prefix(self, ui_theme: &UiTheme) -> String {
        let mut rendition = agent_text_foreground_rendition(self.color_pair(ui_theme));
        match self {
            Self::Status | Self::DiffContext => rendition.dim = true,
            Self::UserPrompt
            | Self::Assistant
            | Self::Error
            | Self::Command
            | Self::DiffHeader
            | Self::DiffAddition
            | Self::DiffDeletion => {
                rendition.bold = true;
            }
            Self::CommandDisplay => {}
        }
        agent_terminal_sgr_sequence(rendition)
    }

    /// Returns the colored speaker label for transcript styles that should
    /// reset to default text after the name indicator.
    pub(super) fn speaker_indicator(self) -> Option<&'static str> {
        match self {
            Self::UserPrompt => Some("user> "),
            Self::Assistant => Some("mez> "),
            Self::Status
            | Self::Error
            | Self::Command
            | Self::CommandDisplay
            | Self::DiffHeader
            | Self::DiffAddition
            | Self::DiffDeletion
            | Self::DiffContext => None,
        }
    }

    /// Returns the stable persistence name for this presentation style.
    pub(in crate::runtime::render) fn persistence_name(self) -> &'static str {
        match self {
            Self::UserPrompt => "user-prompt",
            Self::Assistant => "assistant",
            Self::Status => "status",
            Self::Error => "error",
            Self::Command => "command",
            Self::CommandDisplay => "command-display",
            Self::DiffHeader => "diff-header",
            Self::DiffAddition => "diff-addition",
            Self::DiffDeletion => "diff-deletion",
            Self::DiffContext => "diff-context",
        }
    }

    /// Restores one persisted presentation style name.
    pub(in crate::runtime::render) fn from_persistence_name(name: &str) -> Option<Self> {
        match name {
            "user-prompt" => Some(Self::UserPrompt),
            "assistant" => Some(Self::Assistant),
            "status" => Some(Self::Status),
            "error" => Some(Self::Error),
            "command" => Some(Self::Command),
            "command-display" => Some(Self::CommandDisplay),
            "diff-header" => Some(Self::DiffHeader),
            "diff-addition" => Some(Self::DiffAddition),
            "diff-deletion" => Some(Self::DiffDeletion),
            "diff-context" => Some(Self::DiffContext),
            _ => None,
        }
    }
}

/// Returns a foreground-only rendition for agent-authored transcript text.
///
/// Agent transcript content is injected into pane buffers as ordinary terminal
/// text. It should not paint a background over the user's terminal theme.
pub(in crate::runtime::render) fn agent_text_foreground_rendition(
    pair: UiColorPair,
) -> GraphicRendition {
    GraphicRendition {
        foreground: Some(pair.foreground),
        ..GraphicRendition::default()
    }
}

/// Converts a graphic rendition to an SGR sequence for pane-buffer injection.
pub(in crate::runtime::render) fn agent_terminal_sgr_sequence(
    rendition: GraphicRendition,
) -> String {
    if rendition == GraphicRendition::default() {
        return "\x1b[0m".to_string();
    }
    let mut codes = vec!["0".to_string()];
    if rendition.bold {
        codes.push("1".to_string());
    }
    if rendition.dim {
        codes.push("2".to_string());
    }
    if rendition.italic {
        codes.push("3".to_string());
    }
    if rendition.underline {
        if rendition.double_underline {
            codes.push("21".to_string());
        } else {
            codes.push("4".to_string());
        }
    }
    if rendition.strikethrough {
        codes.push("9".to_string());
    }
    if rendition.inverse {
        codes.push("7".to_string());
    }
    if rendition.hidden {
        codes.push("8".to_string());
    }
    if let Some(color) = rendition.foreground {
        push_agent_terminal_sgr_color_codes(&mut codes, color, false);
    }
    if let Some(color) = rendition.background {
        push_agent_terminal_sgr_color_codes(&mut codes, color, true);
    }
    format!("\x1b[{}m", codes.join(";"))
}

/// Appends SGR foreground or background color parameters.
pub(in crate::runtime::render) fn push_agent_terminal_sgr_color_codes(
    codes: &mut Vec<String>,
    color: TerminalColor,
    background: bool,
) {
    match color {
        TerminalColor::Indexed(index) if index < 8 => {
            codes.push((u16::from(index) + if background { 40 } else { 30 }).to_string());
        }
        TerminalColor::Indexed(index) if index < 16 => {
            codes.push((u16::from(index - 8) + if background { 100 } else { 90 }).to_string());
        }
        TerminalColor::Indexed(index) => {
            codes.push(if background { "48" } else { "38" }.to_string());
            codes.push("5".to_string());
            codes.push(index.to_string());
        }
        TerminalColor::Rgb(red, green, blue) => {
            codes.push(if background { "48" } else { "38" }.to_string());
            codes.push("2".to_string());
            codes.push(red.to_string());
            codes.push(green.to_string());
            codes.push(blue.to_string());
        }
    }
}

/// Defines the AGENT TERMINAL MESSAGE PREFIX const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(in crate::runtime::render) const AGENT_TERMINAL_MESSAGE_PREFIX: &str = "▐ ";
/// Editable prompt marker rendered after the agent terminal gutter.
pub(in crate::runtime::render) const AGENT_PROMPT_TEXT_PREFIX: &str = "mez> ";
/// Maximum action-result lines rendered directly into the pane buffer.
pub(in crate::runtime::render) const AGENT_ACTION_RESULT_DISPLAY_MAX_LINES: usize = 200;
/// Maximum action-result bytes rendered directly into the pane buffer.
pub(in crate::runtime::render) const AGENT_ACTION_RESULT_DISPLAY_MAX_BYTES: usize = 64 * 1024;
