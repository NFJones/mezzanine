//! Pane-facing terminal mouse protocol parsing.
//!
//! This module owns SGR mouse packet decoding and the protocol-level event
//! values produced by that decoder. Multiplexer hit testing, policy, actions,
//! and host routing remain outside this crate.

/// A button encoded by a terminal mouse packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// Left mouse button.
    Left,
    /// Middle mouse button.
    Middle,
    /// Right mouse button.
    Right,
    /// Upward wheel movement.
    WheelUp,
    /// Downward wheel movement.
    WheelDown,
    /// Another protocol button code.
    Other(u16),
}

/// The action encoded by a terminal mouse packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    /// Button press.
    Press,
    /// Button release.
    Release,
    /// Mouse drag.
    Drag,
    /// Mouse-wheel movement.
    Scroll,
}

/// Keyboard modifiers encoded by a terminal mouse packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseModifiers {
    /// Whether Shift was held.
    pub shift: bool,
    /// Whether Alt was held.
    pub alt: bool,
    /// Whether Control was held.
    pub ctrl: bool,
}

/// One decoded terminal mouse event using zero-based cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    /// Mouse action kind.
    pub kind: MouseEventKind,
    /// Mouse button.
    pub button: MouseButton,
    /// Zero-based terminal column.
    pub column: u16,
    /// Zero-based terminal row.
    pub row: u16,
    /// Active keyboard modifiers.
    pub modifiers: MouseModifiers,
}

/// Parses one complete SGR mouse packet.
///
/// Returns `None` for non-SGR input, malformed packets, and one-based
/// coordinates that cannot be represented as a terminal cell.
pub fn parse_sgr_mouse(input: &[u8]) -> Option<MouseEvent> {
    let text = std::str::from_utf8(input).ok()?;
    let rest = text.strip_prefix("\u{1b}[<")?;
    let final_byte = match rest.chars().last() {
        Some(byte @ ('M' | 'm')) => byte,
        _ => return None,
    };
    let body = &rest[..rest.len().saturating_sub(final_byte.len_utf8())];
    let fields = body.split(';').collect::<Vec<_>>();
    if fields.len() != 3 {
        return None;
    }
    let code = fields[0].parse::<u16>().ok()?;
    let column = fields[1].parse::<u16>().ok()?;
    let row = fields[2].parse::<u16>().ok()?;
    if column == 0 || row == 0 {
        return None;
    }

    let modifiers = MouseModifiers {
        shift: code & 4 != 0,
        alt: code & 8 != 0,
        ctrl: code & 16 != 0,
    };
    let drag = code & 32 != 0;
    let wheel = code & 64 != 0;
    let base = code & 0b11;
    let button = if wheel {
        match base {
            0 => MouseButton::WheelUp,
            1 => MouseButton::WheelDown,
            other => MouseButton::Other(64 + other),
        }
    } else {
        match base {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            2 => MouseButton::Right,
            other => MouseButton::Other(other),
        }
    };
    let kind = if wheel {
        MouseEventKind::Scroll
    } else if final_byte == 'm' {
        MouseEventKind::Release
    } else if drag {
        MouseEventKind::Drag
    } else {
        MouseEventKind::Press
    };

    Some(MouseEvent {
        kind,
        button,
        column: column.saturating_sub(1),
        row: row.saturating_sub(1),
        modifiers,
    })
}
