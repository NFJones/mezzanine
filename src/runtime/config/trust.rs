//! Runtime project-trust configuration helpers.
//!
//! This module owns project-root request parsing, trust-decision formatting, and
//! project-overlay metadata serialization for the runtime config boundary. It
//! deliberately keeps file-system canonicalization and overlay capability
//! summarization separate from the broader live-config application code.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config::{
    ConfigDiagnostic, ConfigFormat, ConfigLayer, ConfigScope, validate_config_text,
};
use crate::control::unix_seconds_to_rfc3339;
use crate::error::{MezError, Result};
use crate::project::{ProjectTrustRecord, TrustDecision};

use super::{
    ensure_absolute, json_escape, optional_path_json, optional_string_json,
    runtime_json_string_field, runtime_json_value, runtime_string_array_json,
};

/// Extracts and canonicalizes the project root from a project-trust params object.
///
/// Returns an invalid-argument error when params are not an object, when the
/// `project_root` field is missing, or when the supplied path is not absolute.
pub(in crate::runtime) fn runtime_project_root_param(
    params: &str,
    method: &str,
) -> Result<PathBuf> {
    let value = runtime_json_value(params)?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args(format!("{method} requires a params object")))?;
    let root = object
        .get("project_root")
        .and_then(Value::as_str)
        .ok_or_else(|| MezError::invalid_args(format!("{method} requires project_root")))?;
    let root = PathBuf::from(root);
    ensure_absolute(&root)?;
    Ok(fs::canonicalize(&root).unwrap_or(root))
}

/// Parses a project-trust decision from a control request payload.
///
/// Accepted decision names are `trust`/`trusted`, `reject`/`rejected`, and
/// `revoke`/`revoked`; other values return an invalid-argument error.
pub(in crate::runtime) fn runtime_trust_decision_param(params: &str) -> Result<TrustDecision> {
    match runtime_json_string_field(params, "decision").as_deref() {
        Some("trust" | "trusted") => Ok(TrustDecision::Trusted),
        Some("reject" | "rejected") => Ok(TrustDecision::Rejected),
        Some("revoke" | "revoked") => Ok(TrustDecision::Revoked),
        _ => Err(MezError::invalid_args(
            "project/trust/decide requires decision trust or reject",
        )),
    }
}

/// Reports whether a path is located under a project root after best-effort canonicalization.
///
/// Nonexistent paths fall back to their original path values so pending config
/// overlays can still be matched before all files exist on disk.
pub(in crate::runtime) fn runtime_path_under_project_root(
    path: &Path,
    project_root: &Path,
) -> bool {
    let canonical_root =
        fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let canonical_path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical_path.starts_with(canonical_root)
}

/// Serializes a project-trust record and its matching project-overlay metadata.
///
/// The JSON shape is consumed by the control protocol, so this helper preserves
/// field names, null handling, and diagnostic formatting exactly.
pub(in crate::runtime) fn runtime_project_trust_record_json(
    record: &ProjectTrustRecord,
    layers: &[ConfigLayer],
) -> String {
    let overlay_layers = runtime_project_overlay_layers(record, layers);
    let overlays = runtime_project_overlay_files_json(&overlay_layers);
    let capability_summary = runtime_project_overlay_capability_summary_json(&overlay_layers);
    let trusted_at = if record.trusted_at_unix_seconds == 0 {
        "null".to_string()
    } else {
        format!(
            r#""{}""#,
            unix_seconds_to_rfc3339(record.trusted_at_unix_seconds)
        )
    };
    let rejected_at = if matches!(record.state, TrustDecision::Rejected) {
        trusted_at.clone()
    } else {
        "null".to_string()
    };
    let revoked_at = if matches!(record.state, TrustDecision::Revoked) {
        trusted_at.clone()
    } else {
        "null".to_string()
    };
    format!(
        r#"{{"id":"{}","version":1,"project_root":"{}","state":"{}","git_marker_path":{},"trusted_at":{},"rejected_at":{},"revoked_at":{},"decided_by_client_id":{},"trust_policy_version":{},"configuration_schema_version":{},"overlay_files":{},"capability_expansion_summary":{},"diagnostics":[]}}"#,
        json_escape(&record.project_root.to_string_lossy()),
        json_escape(&record.project_root.to_string_lossy()),
        runtime_trust_decision_name(record.state),
        optional_path_json(record.git_marker_path.as_deref()),
        if matches!(record.state, TrustDecision::Trusted) {
            trusted_at.as_str()
        } else {
            "null"
        },
        rejected_at,
        revoked_at,
        optional_string_json(record.decided_by_client_id.as_deref()),
        record.trust_policy_version,
        record.configuration_schema_version,
        overlays,
        capability_summary
    )
}

/// Collects project-overlay config layers that belong to a trust record's root.
fn runtime_project_overlay_layers<'a>(
    record: &ProjectTrustRecord,
    layers: &'a [ConfigLayer],
) -> Vec<&'a ConfigLayer> {
    layers
        .iter()
        .filter(|layer| layer.scope == ConfigScope::ProjectOverlay)
        .filter(|layer| {
            layer
                .path
                .as_ref()
                .is_some_and(|path| runtime_path_under_project_root(path, &record.project_root))
        })
        .collect()
}

/// Serializes project-overlay file application state and diagnostics.
fn runtime_project_overlay_files_json(layers: &[&ConfigLayer]) -> String {
    let files = layers
        .iter()
        .map(|layer| {
            let diagnostics =
                validate_config_text(layer.format, &layer.text, layer.scope).diagnostics;
            let applied = layer.trusted && diagnostics.is_empty();
            format!(
                r#"{{"path":"{}","format":"{}","applied":{},"diagnostics":{}}}"#,
                json_escape(
                    &layer
                        .path
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string())
                        .unwrap_or_else(|| layer.name.clone())
                ),
                runtime_config_format_name(layer.format),
                applied,
                runtime_config_diagnostics_json(&diagnostics)
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", files.join(","))
}

/// Summarizes elevated capabilities requested by project-overlay config text.
fn runtime_project_overlay_capability_summary_json(layers: &[&ConfigLayer]) -> String {
    let mut capabilities = Vec::new();
    for layer in layers {
        let lower = layer.text.to_ascii_lowercase();
        push_capability_if(
            &mut capabilities,
            lower.contains("[hooks") || lower.contains("hooks:") || lower.contains("\"hooks\""),
            "hooks",
        );
        push_capability_if(
            &mut capabilities,
            lower.contains("[mcp_servers")
                || lower.contains("mcp_servers:")
                || lower.contains("\"mcp_servers\""),
            "mcp_servers",
        );
        push_capability_if(
            &mut capabilities,
            lower.contains("command_rules")
                || lower.contains("global_command_rules")
                || lower.contains("\"command_rules\""),
            "command_rules",
        );
        push_capability_if(
            &mut capabilities,
            lower.contains("[providers")
                || lower.contains("providers:")
                || lower.contains("\"providers\""),
            "providers",
        );
        push_capability_if(
            &mut capabilities,
            lower.contains("[permissions")
                || lower.contains("permissions:")
                || lower.contains("\"permissions\""),
            "permissions",
        );
    }
    capabilities.sort();
    capabilities.dedup();
    runtime_string_array_json(&capabilities)
}

/// Adds a named capability to the summary set when the predicate matched.
fn push_capability_if(capabilities: &mut Vec<String>, enabled: bool, capability: &str) {
    if enabled {
        capabilities.push(capability.to_string());
    }
}

/// Serializes project-overlay validation diagnostics for trust inspection.
fn runtime_config_diagnostics_json(diagnostics: &[ConfigDiagnostic]) -> String {
    let diagnostics = diagnostics
        .iter()
        .map(|diagnostic| {
            format!(
                r#"{{"severity":"error","code":"config_invalid","message":"{}","path":"{}"}}"#,
                json_escape(&diagnostic.message),
                json_escape(&diagnostic.path),
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", diagnostics.join(","))
}

/// Returns the control-protocol name for a config source format.
fn runtime_config_format_name(format: ConfigFormat) -> &'static str {
    match format {
        ConfigFormat::Toml => "toml",
        ConfigFormat::Yaml => "yaml",
        ConfigFormat::Json => "json",
    }
}

/// Returns the control-protocol name for a project-trust decision.
pub(in crate::runtime) fn runtime_trust_decision_name(decision: TrustDecision) -> &'static str {
    match decision {
        TrustDecision::Pending => "pending",
        TrustDecision::Trusted => "trusted",
        TrustDecision::Rejected => "rejected",
        TrustDecision::Revoked => "revoked",
    }
}
