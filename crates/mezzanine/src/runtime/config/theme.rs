//! Runtime UI theme materialization from effective configuration.
//!
//! This module owns conversion from layered runtime configuration values into
//! resolved terminal UI themes. Keeping theme parsing here separates visual
//! palette materialization from the broader runtime config application flow.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::{MezError, Result};
use mez_mux::theme::{
    DEFAULT_UI_THEME_NAME, UiTheme, UiThemeDefinition, builtin_ui_theme_definition,
    resolve_ui_theme, valid_color_alias_name,
};

/// Runs the runtime ui theme from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn runtime_ui_theme_from_config(root: &Value) -> Result<UiTheme> {
    let theme = root.get("theme").and_then(Value::as_object);
    let active = theme
        .and_then(|object| object.get("active"))
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_UI_THEME_NAME);
    if active.trim().is_empty() || !valid_color_alias_name(active) {
        return Err(MezError::config(
            "theme.active must be a non-empty theme identifier",
        ));
    }

    let mut definition = if let Some(definition) = builtin_ui_theme_definition(active) {
        definition
    } else {
        let Some(custom_theme) = root
            .get("themes")
            .and_then(Value::as_object)
            .and_then(|themes| themes.get(active))
        else {
            return Err(MezError::config(format!(
                "theme.active `{active}` does not name a built-in or configured theme"
            )));
        };
        let mut definition = builtin_ui_theme_definition("deepforest")
            .ok_or_else(|| MezError::config("built-in deepforest theme is unavailable"))?;
        definition.merge(runtime_theme_definition_from_value(
            custom_theme,
            &format!("themes.{active}"),
        )?);
        definition
    };

    if let Some(theme) = root.get("theme") {
        definition.merge(runtime_theme_definition_from_value(theme, "theme")?);
    }

    Ok(resolve_ui_theme(active, definition)?)
}

/// Runs the runtime theme definition from value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_theme_definition_from_value(value: &Value, path: &str) -> Result<UiThemeDefinition> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config(format!("{path} must be a table")))?;
    Ok(UiThemeDefinition {
        aliases: runtime_string_map_from_value(object.get("aliases"), &format!("{path}.aliases"))?,
        colors: runtime_string_map_from_value(object.get("colors"), &format!("{path}.colors"))?,
    })
}

/// Runs the runtime string map from value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_string_map_from_value(
    value: Option<&Value>,
    path: &str,
) -> Result<BTreeMap<String, String>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config(format!("{path} must be a table")))?;
    object
        .iter()
        .map(|(key, value)| {
            let value = value
                .as_str()
                .ok_or_else(|| MezError::config(format!("{path}.{key} must be a string value")))?;
            Ok((key.clone(), value.to_string()))
        })
        .collect()
}
