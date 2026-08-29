//! Reusable key-assignment preset definitions.
//!
//! This module owns built-in key presets and the merge semantics shared by
//! configuration and command surfaces. Preset definitions distinguish an
//! omitted direct binding from an explicitly disabled binding so configured
//! presets can inherit defaults without losing `null` declarations.

use std::collections::BTreeMap;

use crate::input::{KeyBindings, KeyChord, KeyCode, KeyModifiers};

/// Built-in key-preset names accepted by `key_preset.active`.
pub const BUILTIN_KEY_PRESET_NAMES: &[&str] = &["default", "simple"];

/// Built-in key preset selected when no explicit preset is configured.
pub const DEFAULT_KEY_PRESET_NAME: &str = "default";

/// A partial key-assignment preset definition.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyPresetDefinition {
    /// Optional prefix chord override.
    pub escape: Option<KeyChord>,
    /// Optional direct vertical-split declaration.
    pub split_vertical: Option<Option<KeyChord>>,
    /// Optional direct horizontal-split declaration.
    pub split_horizontal: Option<Option<KeyChord>>,
    /// Optional direct new-window declaration.
    pub new_window: Option<Option<KeyChord>>,
    /// Optional direct new-group declaration.
    pub new_group: Option<Option<KeyChord>>,
    /// Optional direct agent-shell declaration.
    pub agent_shell: Option<Option<KeyChord>>,
    /// Optional prompt-edit prefix-suffix declaration.
    pub edit_prompt: Option<Option<KeyChord>>,
    /// Optional direct upward pane-focus declaration.
    pub focus_up: Option<Option<KeyChord>>,
    /// Optional direct downward pane-focus declaration.
    pub focus_down: Option<Option<KeyChord>>,
    /// Optional direct leftward pane-focus declaration.
    pub focus_left: Option<Option<KeyChord>>,
    /// Optional direct rightward pane-focus declaration.
    pub focus_right: Option<Option<KeyChord>>,
    /// Optional direct previous-window declaration.
    pub focus_previous_window: Option<Option<KeyChord>>,
    /// Optional direct next-window declaration.
    pub focus_next_window: Option<Option<KeyChord>>,
    /// Optional direct previous-group declaration.
    pub focus_previous_group: Option<Option<KeyChord>>,
    /// Optional direct next-group declaration.
    pub focus_next_group: Option<Option<KeyChord>>,
    /// Prefix command bindings keyed by key-chord notation.
    pub command_bindings: BTreeMap<String, String>,
}

impl KeyPresetDefinition {
    /// Applies this partial definition over the supplied base bindings.
    pub fn materialize(&self, mut bindings: KeyBindings) -> KeyBindings {
        if let Some(chord) = self.escape {
            bindings.escape = chord;
        }
        apply_optional(&mut bindings.split_vertical, self.split_vertical);
        apply_optional(&mut bindings.split_horizontal, self.split_horizontal);
        apply_optional(&mut bindings.new_window, self.new_window);
        apply_optional(&mut bindings.new_group, self.new_group);
        apply_optional(&mut bindings.agent_shell, self.agent_shell);
        apply_optional(&mut bindings.edit_prompt, self.edit_prompt);
        apply_optional(&mut bindings.focus_up, self.focus_up);
        apply_optional(&mut bindings.focus_down, self.focus_down);
        apply_optional(&mut bindings.focus_left, self.focus_left);
        apply_optional(&mut bindings.focus_right, self.focus_right);
        apply_optional(
            &mut bindings.focus_previous_window,
            self.focus_previous_window,
        );
        apply_optional(&mut bindings.focus_next_window, self.focus_next_window);
        apply_optional(
            &mut bindings.focus_previous_group,
            self.focus_previous_group,
        );
        apply_optional(&mut bindings.focus_next_group, self.focus_next_group);
        bindings
    }

    /// Returns the number of enabled direct bindings in the materialized preset.
    pub fn direct_binding_count(&self) -> usize {
        let bindings = self.materialize(KeyBindings::default());
        [
            bindings.split_vertical,
            bindings.split_horizontal,
            bindings.new_window,
            bindings.new_group,
            bindings.agent_shell,
            bindings.focus_up,
            bindings.focus_down,
            bindings.focus_left,
            bindings.focus_right,
            bindings.focus_previous_window,
            bindings.focus_next_window,
            bindings.focus_previous_group,
            bindings.focus_next_group,
        ]
        .into_iter()
        .flatten()
        .count()
    }
}

/// Returns one built-in key-preset definition by name.
pub fn builtin_key_preset_definition(name: &str) -> Option<KeyPresetDefinition> {
    match name {
        "default" => Some(KeyPresetDefinition::default()),
        "simple" => Some(simple_key_preset_definition()),
        _ => None,
    }
}

/// Returns the fully materialized bindings for one built-in key preset.
pub fn builtin_key_preset_bindings(name: &str) -> Option<KeyBindings> {
    builtin_key_preset_definition(name)
        .map(|definition| definition.materialize(KeyBindings::default()))
}

/// Returns the Markdown header used by key-preset listing commands.
pub fn key_preset_list_table_header() -> String {
    [
        "| active | preset | source | prefix | bindings | action |".to_string(),
        "| --- | --- | --- | --- | --- | --- |".to_string(),
    ]
    .join("\n")
}

/// Returns one Markdown row for a key-preset listing command.
pub fn key_preset_list_table_row(
    name: &str,
    source: &str,
    active: bool,
    definition: &KeyPresetDefinition,
) -> String {
    let bindings = definition.materialize(KeyBindings::default());
    let active_marker = if active { "★ active" } else { "—" };
    let summary = format!(
        "{} direct, {} command",
        definition.direct_binding_count(),
        definition.command_bindings.len()
    );
    let action = format!("set-key-preset {name}");
    format!(
        "| {} | {} | {} | {} | {} | [`{}`]({}) |",
        markdown_cell(active_marker),
        markdown_cell(name),
        markdown_cell(source),
        markdown_cell(&key_chord_notation(bindings.escape)),
        markdown_cell(&summary),
        markdown_cell(&action),
        action_destination(&action),
    )
}

fn simple_key_preset_definition() -> KeyPresetDefinition {
    KeyPresetDefinition {
        escape: Some(KeyChord::ctrl(KeyCode::Char('a'))),
        split_vertical: Some(Some(KeyChord::alt(KeyCode::Char('\\')))),
        split_horizontal: Some(Some(KeyChord::alt(KeyCode::Char('-')))),
        new_window: Some(Some(KeyChord::alt(KeyCode::Char('=')))),
        new_group: Some(Some(KeyChord::alt(KeyCode::Char('+')))),
        agent_shell: Some(Some(KeyChord::alt(KeyCode::Char(']')))),
        edit_prompt: None,
        focus_up: Some(Some(KeyChord::ctrl_alt(KeyCode::Up))),
        focus_down: Some(Some(KeyChord::ctrl_alt(KeyCode::Down))),
        focus_left: Some(Some(KeyChord::ctrl_alt(KeyCode::Left))),
        focus_right: Some(Some(KeyChord::ctrl_alt(KeyCode::Right))),
        focus_previous_window: Some(Some(KeyChord::ctrl_alt(KeyCode::PageUp))),
        focus_next_window: Some(Some(KeyChord::ctrl_alt(KeyCode::PageDown))),
        focus_previous_group: Some(Some(chord(KeyCode::PageUp, true, true, true))),
        focus_next_group: Some(Some(chord(KeyCode::PageDown, true, true, true))),
        command_bindings: BTreeMap::new(),
    }
}

fn apply_optional(target: &mut Option<KeyChord>, declaration: Option<Option<KeyChord>>) {
    if let Some(value) = declaration {
        *target = value;
    }
}

fn chord(code: KeyCode, ctrl: bool, alt: bool, shift: bool) -> KeyChord {
    KeyChord {
        code,
        modifiers: KeyModifiers { ctrl, alt, shift },
    }
}

fn key_chord_notation(chord: KeyChord) -> String {
    let mut components = Vec::new();
    if chord.modifiers.ctrl {
        components.push("C".to_string());
    }
    if chord.modifiers.alt {
        components.push("A".to_string());
    }
    if chord.modifiers.shift {
        components.push("S".to_string());
    }
    components.push(match chord.code {
        KeyCode::Char(character) => character.to_string(),
        KeyCode::Up => "Up".to_string(),
        KeyCode::Down => "Down".to_string(),
        KeyCode::Left => "Left".to_string(),
        KeyCode::Right => "Right".to_string(),
        KeyCode::PageUp => "PageUp".to_string(),
        KeyCode::PageDown => "PageDown".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
    });
    components.join("-")
}

fn markdown_cell(value: &str) -> String {
    value.replace('|', r"\|").replace('\n', "<br>")
}

fn action_destination(command: &str) -> String {
    let mut encoded = String::from("mez-agent:");
    for byte in command.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the default preset preserves the established prefix-only key
    /// behavior so adopting presets does not alter clean-install input routing.
    #[test]
    fn default_preset_matches_default_key_bindings() {
        assert_eq!(
            builtin_key_preset_bindings("default"),
            Some(KeyBindings::default())
        );
    }

    /// Verifies the simple preset captures representative direct actions and
    /// all thirteen requested direct bindings without changing the prefix.
    #[test]
    fn simple_preset_materializes_requested_direct_bindings() {
        let bindings = builtin_key_preset_bindings("simple").unwrap();
        assert_eq!(bindings.escape, KeyChord::ctrl(KeyCode::Char('a')));
        assert_eq!(
            bindings.split_vertical,
            Some(KeyChord::alt(KeyCode::Char('\\')))
        );
        assert_eq!(bindings.new_group, KeyChord::parse("A-S-=").ok());
        assert_eq!(
            bindings.focus_next_group,
            Some(chord(KeyCode::PageDown, true, true, true))
        );
        assert_eq!(
            builtin_key_preset_definition("simple")
                .unwrap()
                .direct_binding_count(),
            13
        );
    }

    /// Verifies omitted fields inherit from the base while an explicit nested
    /// `None` disables a direct binding, matching configured-preset semantics.
    #[test]
    fn partial_preset_distinguishes_inheritance_from_disabled_binding() {
        let base = KeyBindings {
            new_window: Some(KeyChord::alt(KeyCode::Char('n'))),
            new_group: Some(KeyChord::alt(KeyCode::Char('g'))),
            ..KeyBindings::default()
        };
        let definition = KeyPresetDefinition {
            new_window: Some(None),
            ..KeyPresetDefinition::default()
        };
        let bindings = definition.materialize(base);
        assert_eq!(bindings.new_window, None);
        assert_eq!(bindings.new_group, Some(KeyChord::alt(KeyCode::Char('g'))));
    }

    /// Verifies prompt editing is a preset-owned prefix action whose suffix can
    /// be remapped or disabled without creating an ordinary direct binding.
    #[test]
    fn prompt_edit_prefix_binding_supports_remap_and_disable() {
        let remapped = KeyPresetDefinition {
            edit_prompt: Some(Some(KeyChord::new(KeyCode::Char('v')))),
            ..KeyPresetDefinition::default()
        }
        .materialize(KeyBindings::default());
        assert_eq!(
            remapped.edit_prompt,
            Some(KeyChord::new(KeyCode::Char('v')))
        );

        let disabled = KeyPresetDefinition {
            edit_prompt: Some(None),
            ..KeyPresetDefinition::default()
        }
        .materialize(KeyBindings::default());
        assert_eq!(disabled.edit_prompt, None);
    }

    /// Verifies key-preset rows expose selectable internal command links using
    /// the same Markdown action convention as theme rows.
    #[test]
    fn key_preset_row_contains_selectable_set_action() {
        let row = key_preset_list_table_row(
            "simple",
            "builtin",
            true,
            &builtin_key_preset_definition("simple").unwrap(),
        );
        assert!(row.contains("★ active"));
        assert!(row.contains("13 direct, 0 command"));
        assert!(row.contains("[`set-key-preset simple`](mez-agent:set-key-preset%20simple)"));
    }
}
