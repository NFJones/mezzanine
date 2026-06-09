//! Runtime audit option readers.
//!
//! This module owns audit-log materialization from effective runtime config.
//! Keeping audit readers separate from permission, provider, MCP, hook, and
//! control-payload helpers makes the runtime config facade narrower while
//! preserving the same validation and defaulting behavior.

use super::*;

/// Runs the runtime audit log from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_audit_log_from_config(
    root: &Value,
    config_root: Option<&Path>,
) -> Result<Option<AuditLog>> {
    let Some(audit) = runtime_json_object(root, "audit") else {
        return Ok(None);
    };
    if let Some(format) = runtime_json_string(audit.get("format"))
        && format != "jsonl"
    {
        return Err(MezError::config("audit.format must be jsonl"));
    }
    let enabled = runtime_json_bool(audit.get("enabled")).unwrap_or(false);
    let required = runtime_json_bool(audit.get("required")).unwrap_or(false);
    if !enabled && !required {
        return Ok(None);
    }
    let path_text = runtime_json_string(audit.get("path")).unwrap_or("audit.jsonl");
    if path_text.trim().is_empty() {
        return Err(MezError::config("audit.path must not be empty"));
    }
    let path = PathBuf::from(path_text);
    let path = if path.is_absolute() {
        path
    } else if let Some(config_root) = config_root {
        config_root.join(path)
    } else {
        path
    };
    let retention = runtime_audit_retention_policy(audit)?;
    Ok(Some(
        AuditLog::new(AuditConfig {
            enabled,
            path,
            hash_chain: runtime_json_bool(audit.get("hash_chain")).unwrap_or(false),
            required,
        })
        .with_retention(retention),
    ))
}

/// Runs the runtime audit config present operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_audit_config_present(root: &Value) -> bool {
    runtime_json_object(root, "audit").is_some()
}

/// Runs the runtime audit retention policy operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_audit_retention_policy(
    audit: &serde_json::Map<String, Value>,
) -> Result<AuditRetentionPolicy> {
    let Some(value) = audit.get("retention_days") else {
        return Ok(AuditRetentionPolicy::disabled());
    };
    let Some(days) = value.as_u64() else {
        return Err(MezError::config(
            "audit.retention_days must be a positive integer",
        ));
    };
    if days == 0 {
        return Err(MezError::config(
            "audit.retention_days must be greater than zero",
        ));
    }
    Ok(AuditRetentionPolicy::retain_days(days))
}
