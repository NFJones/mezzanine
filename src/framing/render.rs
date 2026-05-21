//! Visible frame template rendering helpers.
//!
//! Rendering here is limited to deterministic text substitution and overflow
//! handling. Terminal drawing remains outside this module.

use super::types::{FrameContext, FrameOverflow};

/// Renders a visible frame template with named context fields.
///
/// Placeholders use `#{field.name}` syntax. Missing values render as empty
/// strings, control characters are sanitized, and the configured overflow
/// policy is applied to the final text.
pub fn render_frame_template(
    template: &str,
    context: &FrameContext,
    width: usize,
    overflow: FrameOverflow,
) -> String {
    let mut rendered = String::new();
    let mut rest = template;

    while let Some(start) = rest.find("#{") {
        rendered.push_str(&sanitize_frame_text(&rest[..start]));
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find('}') else {
            rendered.push_str(&sanitize_frame_text(&rest[start..]));
            break;
        };
        let key = &after_start[..end];
        rendered.push_str(&sanitize_frame_text(context.field(key)));
        rest = &after_start[end + 1..];
    }
    rendered.push_str(&sanitize_frame_text(rest));

    apply_overflow(rendered, width, overflow)
}

/// Renders a compact status line for observers waiting on task completion.
pub fn render_pending_observer_status(observers: &[(String, String)], width: usize) -> String {
    if observers.is_empty() {
        return String::new();
    }
    let mut status = String::from("pending observers: ");
    for (index, (id, name)) in observers.iter().enumerate() {
        if index > 0 {
            status.push_str(", ");
        }
        status.push_str(&sanitize_frame_text(id));
        status.push('(');
        status.push_str(&sanitize_frame_text(name));
        status.push(')');
    }
    apply_overflow(status, width, FrameOverflow::Elide)
}

/// Replaces control characters with spaces for visible frame text.
pub fn sanitize_frame_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

/// Runs the apply overflow operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn apply_overflow(value: String, width: usize, overflow: FrameOverflow) -> String {
    if width == 0 || value.chars().count() <= width {
        return value;
    }

    match overflow {
        FrameOverflow::Truncate => value.chars().take(width).collect(),
        FrameOverflow::Elide => elide(value, width),
        FrameOverflow::Wrap => wrap(value, width),
    }
}

/// Runs the elide operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn elide(value: String, width: usize) -> String {
    if width <= 3 {
        return ".".repeat(width);
    }
    let keep = width - 3;
    let mut output = value.chars().take(keep).collect::<String>();
    output.push_str("...");
    output
}

/// Runs the wrap operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn wrap(value: String, width: usize) -> String {
    let mut output = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index > 0 && index % width == 0 {
            output.push('\n');
        }
        output.push(ch);
    }
    output
}
