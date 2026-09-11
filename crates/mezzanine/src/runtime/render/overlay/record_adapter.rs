//! Runtime display-record parsing and executable-choice adaptation.

use super::action_registry::{OverlayActionTarget, overlay_terminal_command_target};
use super::display_content::*;
use crate::runtime::render::*;
use mez_mux::theme::parse_hex_color;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Structured representation of one compact display row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeDisplayRecord {
    /// Leading non-key fields, such as an index or key-binding notation.
    prefix: Vec<String>,
    /// Parsed key/value fields from the display row.
    pub(crate) fields: Vec<(String, String)>,
}

impl RuntimeDisplayRecord {
    /// Parses a colon-delimited `key=value:key=value` style display row.
    pub(super) fn parse_colon_delimited(line: &str) -> Option<Self> {
        if !line.contains('=') || !line.contains(':') {
            return None;
        }
        let mut record = Self {
            prefix: Vec::new(),
            fields: Vec::new(),
        };
        for segment in line.split(':') {
            record.push_segment(segment.trim());
        }
        record.has_fields().then_some(record)
    }

    /// Parses a whitespace-delimited `key=value key=value` style display row.
    pub(super) fn parse_space_delimited(line: &str) -> Option<Self> {
        if !line.contains('=') {
            return None;
        }
        let fields = line
            .split_whitespace()
            .map(runtime_parse_display_field)
            .collect::<Option<Vec<_>>>()?;
        (!fields.is_empty()).then_some(Self {
            prefix: Vec::new(),
            fields,
        })
    }

    /// Adds one colon-delimited segment to this record.
    fn push_segment(&mut self, segment: &str) {
        if segment.is_empty() {
            return;
        }
        if let Some(field) = runtime_parse_display_field(segment) {
            self.fields.push(field);
        } else if let Some((_, value)) = self.fields.last_mut() {
            value.push(':');
            value.push_str(segment);
        } else {
            self.prefix.push(segment.to_string());
        }
    }

    /// Returns true when this record has at least one key/value field.
    fn has_fields(&self) -> bool {
        !self.fields.is_empty()
    }

    /// Formats this record as a readable display line with action chips.
    pub(super) fn into_display_line(self) -> RuntimeDisplayLine {
        let choices = self.choices();
        let has_choices = !choices.is_empty();
        let mut text = String::new();
        let mut placements = Vec::new();
        let mut style_spans = Vec::new();
        if has_choices {
            text.push_str("actions: ");
            for choice in choices {
                if !placements.is_empty() {
                    text.push(' ');
                }
                let chip = format!("[{}]", choice.label);
                let start_column = UnicodeWidthStr::width(text.as_str());
                let width = UnicodeWidthStr::width(chip.as_str());
                text.push_str(&chip);
                placements.push(RuntimeDisplayChoicePlacement {
                    start_column,
                    width,
                    label: choice.label,
                    target: choice.target,
                    kind: choice.kind,
                });
            }
        }
        let mut append_part = |part: String| {
            if !text.is_empty() {
                text.push_str(" | ");
            }
            let start = UnicodeWidthStr::width(text.as_str());
            text.push_str(&part);
            start
        };
        if !self.prefix.is_empty() {
            append_part(self.prefix.join(" "));
        }
        for (key, value) in &self.fields {
            if self.choice_field_is_consumed(key, value, has_choices) {
                continue;
            }
            let label = format!("{}: ", runtime_display_field_label(key));
            let display_value = runtime_display_field_value(value);
            let part_start = append_part(format!("{label}{display_value}"));
            if key == "preview" {
                style_spans.extend(runtime_theme_preview_style_spans(
                    part_start.saturating_add(UnicodeWidthStr::width(label.as_str())),
                    display_value.as_str(),
                    self.field_value("preview_colors"),
                ));
            }
        }
        RuntimeDisplayLine {
            text,
            choices: placements,
            style_spans,
        }
    }

    /// Returns executable choices encoded by this row.
    fn choices(&self) -> Vec<RuntimeDisplayChoice> {
        let mut choices = Vec::new();
        for command in self
            .field_values("commands")
            .flat_map(|value| runtime_split_display_commands(value, '|'))
        {
            runtime_push_unique_display_choice(&mut choices, command);
        }
        for key in ["select_command", "command_action", "action"] {
            for value in self.field_values(key) {
                runtime_push_unique_display_choice(&mut choices, value);
            }
        }
        for value in self.field_values("actions") {
            for command in runtime_split_display_commands(value, ',') {
                runtime_push_unique_display_choice(&mut choices, command);
            }
        }
        choices
    }

    /// Returns all values for one field key.
    fn field_values(&self, key: &str) -> impl Iterator<Item = &str> {
        self.fields
            .iter()
            .filter(move |(field_key, _)| field_key == key)
            .map(|(_, value)| value.as_str())
    }

    /// Returns the first value for one display field key.
    pub(super) fn field_value(&self, key: &str) -> Option<&str> {
        self.field_values(key).next()
    }

    /// Returns whether a field was used as selector metadata.
    fn choice_field_is_consumed(&self, key: &str, value: &str, has_choices: bool) -> bool {
        match key {
            "commands" | "select_command" | "command_action" => has_choices,
            "actions" => has_choices,
            "action" => runtime_display_executable_choice(value).is_some(),
            "preview_colors" => self.field_value("preview").is_some(),
            _ => false,
        }
    }
}

/// Splits a compact display choice field into command candidates.
pub(crate) fn runtime_split_display_commands(
    value: &str,
    separator: char,
) -> impl Iterator<Item = &str> {
    value
        .split(separator)
        .map(str::trim)
        .filter(|command| !command.is_empty() && *command != "none")
}

/// Pushes one executable choice if an equivalent target is not already present.
pub(crate) fn runtime_push_unique_display_choice(
    choices: &mut Vec<RuntimeDisplayChoice>,
    command: &str,
) {
    let Some(choice) = runtime_display_executable_choice(command) else {
        return;
    };
    if choices
        .iter()
        .any(|existing| existing.target == choice.target)
    {
        return;
    }
    choices.push(choice);
}

/// Converts a command string into a selectable display choice when valid.
///
/// The choice carries one typed, validated target instead of the raw command
/// text, so the range is only executable through the action registry.
pub(crate) fn runtime_display_executable_choice(command: &str) -> Option<RuntimeDisplayChoice> {
    let target = overlay_terminal_command_target(command)?;
    let OverlayActionTarget::TerminalCommand { name, .. } = &target else {
        return None;
    };
    Some(RuntimeDisplayChoice {
        label: runtime_display_choice_label(name),
        kind: runtime_display_choice_kind(name),
        target,
    })
}

/// Returns whether a command name is part of the Mez terminal command set.
pub(crate) fn runtime_display_is_known_command(command_name: &str) -> bool {
    baseline_commands()
        .iter()
        .any(|command| command.name == command_name)
}

/// Returns a concise action label for one command name.
pub(crate) fn runtime_display_choice_label(command_name: &str) -> String {
    match command_name {
        "select-window" | "select-group" | "select-pane" | "select-layout" => "select",
        "detach-client" => "detach",
        "paste-buffer" | "paste-clipboard" => "paste",
        "delete-buffer" => "delete",
        "copy-selection" => "copy",
        other => other.split('-').next().unwrap_or(other),
    }
    .to_string()
}

/// Returns the themed visual category for one command name.
pub(crate) fn runtime_display_choice_kind(command_name: &str) -> OverlaySelectionKind {
    match command_name {
        "delete-buffer" | "detach-client" | "kill-pane" | "kill-window" | "kill-group"
        | "kill-session" => OverlaySelectionKind::Danger,
        "paste-buffer" | "paste-clipboard" | "copy-selection" => OverlaySelectionKind::Secondary,
        _ => OverlaySelectionKind::Primary,
    }
}

/// Parses one `key=value` display field.
pub(crate) fn runtime_parse_display_field(segment: &str) -> Option<(String, String)> {
    let (key, value) = segment.split_once('=')?;
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return None;
    }
    Some((key.to_string(), value.trim().to_string()))
}

/// Returns a lowercase human-readable label for a compact display field name.
pub(crate) fn runtime_display_field_label(key: &str) -> String {
    key.split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| part.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Renders untrusted text as one compact display-record field value.
///
/// Compact records separate fields with `:` and keys from values with `=`, and
/// the record parser turns any `key=value` segment into a field. A
/// program-settable value such as a pane title, client name, window name, or
/// buffer preview could therefore inject an executable `action=` field into a
/// chooser row, which no product-authored row intended. Untrusted values pass
/// through here and lose exactly those structural characters plus any control
/// character, while keeping their readable text.
pub(crate) fn runtime_display_field_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, ':' | '=') {
                ' '
            } else {
                ch
            }
        })
        .collect()
}

/// Returns a readable value for common compact display values.
pub(crate) fn runtime_display_field_value(value: &str) -> String {
    match value {
        "true" => "yes".to_string(),
        "false" => "no".to_string(),
        "none" => "none".to_string(),
        _ => value.to_string(),
    }
}

/// Returns per-block color spans for one theme preview field.
pub(super) fn runtime_theme_preview_style_spans(
    start_column: usize,
    preview: &str,
    preview_colors: Option<&str>,
) -> Vec<TerminalStyleSpan> {
    let colors = preview_colors
        .into_iter()
        .flat_map(|value| value.split(','))
        .filter_map(|value| parse_hex_color(value.trim()))
        .collect::<Vec<_>>();
    let mut spans = Vec::new();
    let mut column = start_column;
    for (index, ch) in preview.chars().enumerate() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0).max(1);
        if ch.is_whitespace() {
            column = column.saturating_add(width);
            continue;
        }
        let Some(color) = colors.get(index) else {
            column = column.saturating_add(width);
            continue;
        };
        push_or_extend_style_span(
            &mut spans,
            TerminalStyleSpan {
                start: column,
                length: width,
                rendition: GraphicRendition {
                    foreground: Some(*color),
                    ..GraphicRendition::default()
                },
            },
        );
        column = column.saturating_add(width);
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::{RuntimeDisplayRecord, runtime_display_field_text, runtime_parse_display_field};

    /// Verifies untrusted field text cannot inject a compact-record field.
    ///
    /// A pane title or buffer preview is program-settable, so a `:` or `=` in it
    /// must never start a new `key=value` segment that the parser could accept as
    /// an executable choice.
    #[test]
    fn untrusted_field_text_cannot_inject_a_record_field() {
        let hostile = "title:action=kill-session";
        let sanitized = runtime_display_field_text(hostile);
        assert!(!sanitized.contains(':'), "{sanitized}");
        assert!(!sanitized.contains('='), "{sanitized}");
        assert!(sanitized.contains("kill-session"), "{sanitized}");

        let row = format!(
            "pane=%1:index=0:active=true:title={}:action=select-pane -t %1",
            sanitized
        );
        let record = RuntimeDisplayRecord::parse_colon_delimited(&row)
            .expect("the product row must stay a compact record");
        assert_eq!(
            record.field_value("title"),
            Some("title action kill-session")
        );
        assert_eq!(
            record
                .fields
                .iter()
                .filter(|(key, _)| key == "action")
                .count(),
            1
        );
    }

    /// Verifies control characters in untrusted text cannot break a single row.
    #[test]
    fn untrusted_field_text_cannot_break_a_record_row() {
        let sanitized = runtime_display_field_text("first\nsecond\tthird");
        assert!(!sanitized.contains('\n'), "{sanitized}");
        assert!(!sanitized.contains('\t'), "{sanitized}");
        assert_eq!(sanitized, "first second third");
        assert!(runtime_parse_display_field("title=a b").is_some());
    }
}
