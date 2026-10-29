//! Instruction context budget projected from the existing configuration schema.
//!
//! Discovery owns filesystem reads. This reader only validates the total source
//! byte limit used by prompt snapshots, so live instruction-only changes reach
//! the same actor-owned projection as initial configuration and rollback.

use crate::error::{MezError, Result};

/// Reads the existing positive `instructions.max_bytes`, defaulting to the
/// shared 32 KiB limit. Non-integer, zero or unrepresentable values fail before
/// the agent settings projection mutates runtime state.
pub(crate) fn instruction_context_max_bytes_from_config(root: &serde_json::Value) -> Result<usize> {
    let Some(value) = root.pointer("/instructions/max_bytes") else {
        return Ok(mez_agent::instructions::DEFAULT_INSTRUCTION_CONTEXT_MAX_BYTES);
    };
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            MezError::config("instructions.max_bytes must be a positive representable integer")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Missing limits use the documented shared default, while explicit limits
    /// remain exact. Invalid values cannot silently disable the source-byte cap.
    #[test]
    fn instruction_context_budget_validates_config_values() {
        assert_eq!(
            instruction_context_max_bytes_from_config(&serde_json::json!({})).unwrap(),
            32768
        );
        assert_eq!(
            instruction_context_max_bytes_from_config(
                &serde_json::json!({"instructions":{"max_bytes":7}})
            )
            .unwrap(),
            7
        );
        for value in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("7"),
            serde_json::Value::Null,
        ] {
            assert!(
                instruction_context_max_bytes_from_config(
                    &serde_json::json!({"instructions":{"max_bytes":value}})
                )
                .is_err()
            );
        }
    }
}
