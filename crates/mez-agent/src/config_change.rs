//! Model-facing live configuration mutation contracts.
//!
//! This module owns the stable operation names and value-shape guidance that
//! providers expose for `config_change` actions. The product crate remains
//! responsible for enumerating supported setting paths, validating values,
//! persisting changes, and applying them to the running process.

/// Provider-visible operation names for model-authored live config changes.
pub const CONFIG_CHANGE_OPERATION_NAMES: &[&str] = &["set", "unset", "reset"];

/// Provider-visible value guidance for model-authored live config changes.
pub const CONFIG_CHANGE_VALUE_DESCRIPTION: &str = "For operation=set, provide a string containing one JSON scalar or string array accepted by config/set: JSON string, integer, boolean, or string array. Plain text is accepted as a JSON string. For operation=unset or reset, use null. reset removes the explicit override so the lower-precedence or default value becomes effective. Objects and null set-values are rejected.";

#[cfg(test)]
mod tests {
    use super::{CONFIG_CHANGE_OPERATION_NAMES, CONFIG_CHANGE_VALUE_DESCRIPTION};

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
}
