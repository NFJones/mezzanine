//! Model-facing live configuration mutation contracts.
//!
//! This module owns the stable operation names and value-shape guidance that
//! providers expose for `config_change` actions. The product crate remains
//! responsible for enumerating supported setting paths, validating values,
//! persisting changes, and applying them to the running process.

use std::error::Error;
use std::fmt;

/// Provider-visible operation names for model-authored live config changes.
pub const CONFIG_CHANGE_OPERATION_NAMES: &[&str] = &["set", "unset", "reset"];

/// Provider-visible fallback guidance for the live configuration setting path.
///
/// Product adapters may replace this text with a more specific description of
/// their supported live paths when they grant the config-change capability.
pub const CONFIG_CHANGE_SETTING_PATH_DESCRIPTION: &str = "Dotted live configuration path. Use only paths advertised by the product adapter, and inspect current configuration before changing dynamic names.";

/// Provider-visible value guidance for model-authored live config changes.
pub const CONFIG_CHANGE_VALUE_DESCRIPTION: &str = "For operation=set, provide a string containing one JSON scalar or string array accepted by config/set: JSON string, integer, boolean, or string array. Plain text is accepted as a JSON string. For operation=unset or reset, use null. reset removes the explicit override so the lower-precedence or default value becomes effective. Objects and null set-values are rejected.";

/// Canonical execution kind for a model-authored configuration mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigChangeOperation {
    /// Assigns one scalar or string-array value.
    Set,
    /// Removes the explicitly configured value.
    Unset,
}

impl ConfigChangeOperation {
    /// Reports whether this operation requires an accompanying value.
    pub fn sets_value(self) -> bool {
        matches!(self, Self::Set)
    }
}

/// Canonical scalar value accepted by the configuration-change contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigChangeValue {
    /// One UTF-8 string value.
    String(String),
    /// One signed integer value.
    Integer(i64),
    /// One boolean value.
    Boolean(bool),
    /// One ordered array containing only UTF-8 strings.
    StringArray(Vec<String>),
}

impl ConfigChangeValue {
    /// Serializes this value into its canonical JSON representation.
    pub fn canonical_json(&self) -> String {
        match self {
            Self::String(value) => serde_json::Value::String(value.clone()).to_string(),
            Self::Integer(value) => value.to_string(),
            Self::Boolean(value) => value.to_string(),
            Self::StringArray(values) => serde_json::json!(values).to_string(),
        }
    }
}

/// Error returned when a model-authored configuration mutation is malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigChangeError {
    message: String,
}

impl ConfigChangeError {
    /// Creates a configuration-change contract error with one diagnostic.
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the stable diagnostic for product error projection.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ConfigChangeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ConfigChangeError {}

/// Normalizes one model-authored configuration operation.
///
/// Compatibility spellings remain accepted at the execution boundary because
/// retained turns may have been authored against an older action surface. New
/// provider schemas advertise only [`CONFIG_CHANGE_OPERATION_NAMES`].
pub fn normalize_config_change_operation(
    operation: &str,
) -> Result<ConfigChangeOperation, ConfigChangeError> {
    match operation.trim().to_ascii_lowercase().as_str() {
        "set" | "replace" | "update" => Ok(ConfigChangeOperation::Set),
        "unset" | "remove" | "delete" | "reset" => Ok(ConfigChangeOperation::Unset),
        _ => Err(ConfigChangeError::new(
            "config_change operation must be set, replace, update, unset, remove, delete, or reset",
        )),
    }
}

/// Parses one model-authored value into the canonical supported scalar shape.
///
/// Raw non-JSON text is treated as a string. JSON objects, null, floating-point
/// numbers, and arrays containing non-string values are rejected.
pub fn parse_config_change_value(
    value: Option<&str>,
) -> Result<ConfigChangeValue, ConfigChangeError> {
    let Some(value) = value else {
        return Err(ConfigChangeError::new(
            "approved config_change set operation requires a value",
        ));
    };
    let parsed = match serde_json::from_str::<serde_json::Value>(value) {
        Ok(parsed) => parsed,
        Err(_) => return Ok(ConfigChangeValue::String(value.to_string())),
    };
    match parsed {
        serde_json::Value::String(value) => Ok(ConfigChangeValue::String(value)),
        serde_json::Value::Bool(value) => Ok(ConfigChangeValue::Boolean(value)),
        serde_json::Value::Number(value) => value
            .as_i64()
            .map(ConfigChangeValue::Integer)
            .ok_or_else(|| ConfigChangeError::new("config_change integer value is invalid")),
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(|value| match value {
                serde_json::Value::String(value) => Ok(value),
                _ => Err(ConfigChangeError::new(
                    "config_change string arrays must contain only strings",
                )),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(ConfigChangeValue::StringArray),
        serde_json::Value::Object(_) | serde_json::Value::Null => Err(ConfigChangeError::new(
            "config_change supports only string, integer, boolean, or string-array values",
        )),
    }
}

/// Parses a configuration-change value required to name a product resource.
///
/// Raw text and JSON string literals are accepted. Every other JSON value is
/// rejected so product command adapters never infer a resource name from a
/// number, boolean, array, object, or null.
pub fn config_change_string_value(
    setting_path: &str,
    value: Option<&str>,
) -> Result<String, ConfigChangeError> {
    let Some(value) = value else {
        return Err(ConfigChangeError::new(format!(
            "approved config_change set operation for {setting_path} requires a value"
        )));
    };
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(serde_json::Value::String(value)) => Ok(value),
        Ok(_) => Err(ConfigChangeError::new(format!(
            "config_change {setting_path} requires a string value"
        ))),
        Err(_) => Ok(value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Verifies providers expose only the stable operations implemented by the
    /// product live-configuration mutation planner.
    fn config_change_operations_match_the_live_mutation_contract() {
        assert_eq!(CONFIG_CHANGE_OPERATION_NAMES, ["set", "unset", "reset"]);
    }

    #[test]
    /// Verifies provider guidance preserves the null and scalar restrictions
    /// that prevent models from submitting unsupported container values.
    fn config_change_value_guidance_describes_supported_shapes() {
        assert!(CONFIG_CHANGE_VALUE_DESCRIPTION.contains("JSON scalar or string array"));
        assert!(CONFIG_CHANGE_VALUE_DESCRIPTION.contains("unset or reset, use null"));
        assert!(
            CONFIG_CHANGE_VALUE_DESCRIPTION.contains("Objects and null set-values are rejected")
        );
    }

    #[test]
    /// Verifies execution normalization preserves stable and retained operation
    /// spellings without exposing the compatibility names in provider schemas.
    fn config_change_operations_normalize_to_two_execution_kinds() {
        for operation in ["set", "replace", "update"] {
            assert_eq!(
                normalize_config_change_operation(operation).unwrap(),
                ConfigChangeOperation::Set
            );
        }
        for operation in ["unset", "remove", "delete", "reset"] {
            assert_eq!(
                normalize_config_change_operation(operation).unwrap(),
                ConfigChangeOperation::Unset
            );
        }
        assert!(normalize_config_change_operation("merge").is_err());
    }

    #[test]
    /// Verifies model values normalize to the supported scalar contract and
    /// reject container shapes that product config mutation cannot apply.
    fn config_change_values_parse_without_product_schema_dependencies() {
        assert_eq!(
            parse_config_change_value(Some("blue")).unwrap(),
            ConfigChangeValue::String("blue".to_string())
        );
        assert_eq!(
            parse_config_change_value(Some(r#"["red","blue"]"#)).unwrap(),
            ConfigChangeValue::StringArray(vec!["red".to_string(), "blue".to_string()])
        );
        assert_eq!(
            parse_config_change_value(Some("42")).unwrap(),
            ConfigChangeValue::Integer(42)
        );
        assert!(parse_config_change_value(Some(r#"{"nested":true}"#)).is_err());
        assert!(parse_config_change_value(Some("1.5")).is_err());
        assert!(parse_config_change_value(None).is_err());
    }

    #[test]
    /// Verifies command resource names accept raw and JSON strings while
    /// rejecting non-string JSON values.
    fn config_change_command_string_values_are_strict() {
        assert_eq!(
            config_change_string_value("theme.active", Some(r#""solarized""#)).unwrap(),
            "solarized"
        );
        assert_eq!(
            config_change_string_value("theme.active", Some("solarized")).unwrap(),
            "solarized"
        );
        assert!(config_change_string_value("theme.active", Some("true")).is_err());
    }
}
