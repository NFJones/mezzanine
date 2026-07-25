//! Live typed sandbox toolchain command handling.
//!
//! This module owns the `/toolchain` grammar and its runtime effects. It reads
//! discovery only from active-pane bootstrap evidence, persists only allowlisted
//! kind names, delegates live config changes to the transactional mutation
//! helper, and delegates reload to the existing full control-plane operation.
//! Discovered host roots are shown only in direct pane-local status output and
//! are never written to config or durable audit metadata.

use super::{
    AgentShellCommandOutcome, ConfigMutation, ConfigMutationOperation, ConfigMutationValue,
    MezError, Result, RuntimeSessionService, current_unix_seconds, json_escape,
    parse_slash_command, runtime_apply_persisted_config_mutation_batch,
    runtime_effective_config_value, runtime_primary_config_path,
};
use crate::runtime::{SandboxConfig, SandboxToolchainKind};
use crate::security::audit::{AuditActor, AuditRecord};
use crate::security::sandbox::{
    SANDBOX_RUST_PATH, SANDBOX_ZIG_PATH, SUPPORTED_SANDBOX_TOOLCHAIN_KINDS,
    discover_rust_from_environment_managers, parse_sandbox_toolchain_kind,
    resolve_toolchain_projection,
};

/// Strict operation accepted by `/toolchain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolchainCommand {
    /// Reports configured, discoverable, and effective state.
    Status,
    /// Lists stable supported kinds.
    List,
    /// Validates active-pane discovery evidence without mutation.
    Detect(SandboxToolchainKind),
    /// Enables one typed projection after explicit confirmation.
    Enable(SandboxToolchainKind),
    /// Disables one typed projection after explicit confirmation.
    Disable(SandboxToolchainKind),
    /// Runs the existing full disk-backed configuration reload.
    Reload,
}

impl ToolchainCommand {
    /// Returns the stable operation spelling used in output and audit records.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::List => "list",
            Self::Detect(_) => "detect",
            Self::Enable(_) => "enable",
            Self::Disable(_) => "disable",
            Self::Reload => "reload",
        }
    }

    /// Returns the typed kind associated with a kind-specific operation.
    const fn kind(self) -> Option<SandboxToolchainKind> {
        match self {
            Self::Detect(kind) | Self::Enable(kind) | Self::Disable(kind) => Some(kind),
            Self::Status | Self::List | Self::Reload => None,
        }
    }
}

/// Pane-local projection of one toolchain state inspection.
#[derive(Debug, Clone)]
struct ToolchainStatus {
    backend: &'static str,
    configured: Vec<String>,
    discoverable: Vec<String>,
    discovery_state: &'static str,
    effective_state: &'static str,
    cargo_bin: Option<String>,
    rustup_home: Option<String>,
    zig_root: Option<String>,
    discovery_error: Option<String>,
    generation: u64,
}

impl RuntimeSessionService {
    /// Executes `/toolchain` through the primary-client runtime boundary.
    ///
    /// # Errors
    /// Returns an argument error for invalid grammar, a forbidden error for a
    /// non-primary caller, and a fail-closed state/config error when discovery,
    /// persistence, live application, or full reload cannot complete.
    pub(super) fn execute_agent_shell_toolchain_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden(
                "toolchain commands require the primary client",
            ));
        }
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("toolchain command must be a slash command"))?;
        if invocation.name != "toolchain" {
            return Err(MezError::invalid_args(
                "toolchain executor received another slash command",
            ));
        }
        let operation = parse_toolchain_command(&invocation.args)?;
        match operation {
            ToolchainCommand::Status => {
                let status = self.toolchain_status_for_pane(pane_id)?;
                self.append_toolchain_audit(
                    primary_client_id,
                    pane_id,
                    operation,
                    false,
                    "inspected",
                )?;
                Ok(AgentShellCommandOutcome::Display {
                    command: "toolchain".to_string(),
                    body: render_toolchain_status(pane_id, &status),
                })
            }
            ToolchainCommand::List => {
                self.append_toolchain_audit(
                    primary_client_id,
                    pane_id,
                    operation,
                    false,
                    "listed",
                )?;
                Ok(AgentShellCommandOutcome::Display {
                    command: "toolchain".to_string(),
                    body: format!(
                        "supported_kinds={} count={} typed_allowlist=true source=runtime-toolchain",
                        supported_toolchain_names().join(","),
                        SUPPORTED_SANDBOX_TOOLCHAIN_KINDS.len()
                    ),
                })
            }
            ToolchainCommand::Detect(kind) => {
                let status = self.toolchain_status_for_pane(pane_id)?;
                let signature = self.pane_environment_signature(pane_id).ok_or_else(|| {
                    MezError::invalid_state(
                        "toolchain detection requires active-pane bootstrap evidence",
                    )
                })?;
                let detail =
                    detect_toolchain_detail(kind, &signature.environment_managers, &signature.os)?;
                self.append_toolchain_audit(
                    primary_client_id,
                    pane_id,
                    operation,
                    false,
                    "detected",
                )?;
                Ok(AgentShellCommandOutcome::Display {
                    command: "toolchain".to_string(),
                    body: format!(
                        "pane={} operation=detect kind={} available=true {} configured={} generation={} changed=false source=active-pane-bootstrap",
                        json_escape(pane_id),
                        kind.as_str(),
                        detail,
                        status.configured.iter().any(|name| name == kind.as_str()),
                        status.generation,
                    ),
                })
            }
            ToolchainCommand::Enable(kind) => {
                let signature = self.pane_environment_signature(pane_id).ok_or_else(|| {
                    MezError::invalid_state(
                        "toolchain enable requires active-pane bootstrap evidence",
                    )
                })?;
                resolve_toolchain_projection(
                    &[kind],
                    &signature.environment_managers,
                    &signature.os,
                )
                .map_err(|error| MezError::invalid_state(error.message()))?;
                self.mutate_toolchain(primary_client_id, pane_id, operation, kind, true)
            }
            ToolchainCommand::Disable(kind) => {
                self.mutate_toolchain(primary_client_id, pane_id, operation, kind, false)
            }
            ToolchainCommand::Reload => {
                let before = self.toolchain_status_for_pane(pane_id)?;
                let request = format!(
                    r#"{{"jsonrpc":"2.0","id":"agent-toolchain-reload","method":"config/reload","params":{{"idempotency_key":"agent-toolchain-reload-{}"}}}}"#,
                    current_unix_seconds()
                );
                let response = self.dispatch_runtime_control_body(&request, primary_client_id);
                reject_control_error(&response)?;
                let after = self.toolchain_status_for_pane(pane_id)?;
                let changed = before.configured != after.configured
                    || before.backend != after.backend
                    || before.generation != after.generation;
                self.append_toolchain_audit(
                    primary_client_id,
                    pane_id,
                    operation,
                    changed,
                    "reloaded",
                )?;
                let visibility = self.agent_shell_visibility_for_pane(pane_id)?;
                Ok(AgentShellCommandOutcome::Mutated {
                    command: "toolchain".to_string(),
                    body: format!(
                        "pane={} operation=reload full_config_reload=true before_configured={} after_configured={} before_state={} after_state={} generation_before={} generation_after={} changed={} subsequent_actions=true existing_shells_unchanged=true running_actions_unchanged=true",
                        json_escape(pane_id),
                        render_names(&before.configured),
                        render_names(&after.configured),
                        before.effective_state,
                        after.effective_state,
                        before.generation,
                        after.generation,
                        changed,
                    ),
                    visibility,
                })
            }
        }
    }

    /// Applies one confirmed typed selection mutation transactionally.
    fn mutate_toolchain(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        operation: ToolchainCommand,
        kind: SandboxToolchainKind,
        enable: bool,
    ) -> Result<AgentShellCommandOutcome> {
        let before = self.configured_toolchain_names()?;
        let mut after = before.clone();
        if enable {
            if !after.iter().any(|name| name == kind.as_str()) {
                after.push(kind.as_str().to_string());
            }
        } else {
            after.retain(|name| name != kind.as_str());
        }
        let changed = before != after;
        let path = runtime_primary_config_path(self)?.ok_or_else(|| {
            MezError::invalid_state("toolchain mutation requires a primary config path")
        })?;
        let generation_before = self.session.config_generation;
        let report = runtime_apply_persisted_config_mutation_batch(
            self,
            path,
            &[ConfigMutation {
                path: "permissions.bubblewrap.toolchains".to_string(),
                operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(
                    after.clone(),
                )),
            }],
            if enable {
                "agent/shell/toolchain-enable"
            } else {
                "agent/shell/toolchain-disable"
            },
        )?;
        let generation_after = self.session.config_generation;
        if changed != report.changed {
            return Err(MezError::invalid_state(
                "toolchain mutation change accounting diverged from config persistence",
            ));
        }
        self.append_toolchain_audit(
            primary_client_id,
            pane_id,
            operation,
            report.changed,
            if report.changed { "applied" } else { "no_op" },
        )?;
        let visibility = self.agent_shell_visibility_for_pane(pane_id)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "toolchain".to_string(),
            body: format!(
                "pane={} operation={} kind={} configured={} configured_kinds={} changed={} generation_before={} generation_after={} persisted_kind_only=true subsequent_actions=true existing_shells_unchanged=true running_actions_unchanged=true",
                json_escape(pane_id),
                operation.as_str(),
                kind.as_str(),
                enable,
                render_names(&after),
                report.changed,
                generation_before,
                generation_after,
            ),
            visibility,
        })
    }

    /// Builds status from effective config and active-pane bootstrap evidence.
    fn toolchain_status_for_pane(&self, pane_id: &str) -> Result<ToolchainStatus> {
        let configured = self.configured_toolchain_names()?;
        let backend = self.configured_permissions().sandbox.as_str();
        let (discovery_state, discoverable, cargo_bin, rustup_home, zig_root, discovery_error) =
            match self.pane_environment_signature(pane_id) {
                None if self.pane_bootstrap_is_pending(pane_id) => {
                    ("bootstrap-pending", Vec::new(), None, None, None, None)
                }
                None => (
                    "environment-unavailable",
                    Vec::new(),
                    None,
                    None,
                    None,
                    None,
                ),
                Some(signature) => {
                    let mut discoverable = Vec::new();
                    let mut errors = Vec::new();
                    let (cargo_bin, rustup_home) = match discover_rust_from_environment_managers(
                        &signature.environment_managers,
                    ) {
                        Ok(discovery) => {
                            discoverable.push("rust".to_string());
                            (
                                Some(discovery.cargo_bin.display().to_string()),
                                Some(discovery.rustup_home.display().to_string()),
                            )
                        }
                        Err(error) => {
                            errors.push(format!("rust:{}", error.message()));
                            (None, None)
                        }
                    };
                    let zig_root = match resolve_toolchain_projection(
                        &[SandboxToolchainKind::Zig],
                        &signature.environment_managers,
                        &signature.os,
                    ) {
                        Ok(Some(projection)) => {
                            discoverable.push("zig".to_string());
                            projection
                                .roots
                                .first()
                                .map(|root| root.host_path.display().to_string())
                        }
                        Ok(None) => None,
                        Err(error) => {
                            errors.push(format!("zig:{}", error.message()));
                            None
                        }
                    };
                    let state = if discoverable.is_empty() {
                        "unavailable"
                    } else {
                        "available"
                    };
                    (
                        state,
                        discoverable,
                        cargo_bin,
                        rustup_home,
                        zig_root,
                        (!errors.is_empty()).then(|| errors.join(";")),
                    )
                }
            };
        let selected = !configured.is_empty();
        let selected_discoverable = configured
            .iter()
            .all(|kind| discoverable.iter().any(|available| available == kind));
        let any_discoverable = !discoverable.is_empty();
        let sandbox_applies = matches!(
            self.configured_permissions().sandbox,
            SandboxConfig::Bubblewrap(_)
        ) && !self.permission_policy().approval_policy.bypasses_sandbox();
        let effective_state = match (
            selected,
            selected_discoverable,
            any_discoverable,
            sandbox_applies,
            backend,
        ) {
            (true, true, _, true, "bubblewrap") => "active",
            (true, false, _, _, "bubblewrap") => "selected-unavailable",
            (true, _, _, false, _) => "selected-inactive",
            (false, _, true, _, _) => "available-disabled",
            (false, _, false, _, _) => "disabled-unavailable",
            _ => "selected-inactive",
        };
        Ok(ToolchainStatus {
            backend,
            configured,
            discoverable,
            discovery_state,
            effective_state,
            cargo_bin,
            rustup_home,
            zig_root,
            discovery_error,
            generation: self.session.config_generation,
        })
    }

    /// Reads persisted kind names from the effective config without host paths.
    fn configured_toolchain_names(&self) -> Result<Vec<String>> {
        let structured = runtime_effective_config_value(self.integration.config_layers())?;
        let values = structured
            .get("permissions")
            .and_then(|value| value.get("bubblewrap"))
            .and_then(|value| value.get("toolchains"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut names = Vec::new();
        for value in values {
            let name = value.as_str().ok_or_else(|| {
                MezError::config("permissions.bubblewrap.toolchains must contain strings")
            })?;
            let kind = parse_sandbox_toolchain_kind(name).ok_or_else(|| {
                MezError::config("permissions.bubblewrap.toolchains contains an unsupported kind")
            })?;
            if names.iter().any(|existing| existing == kind.as_str()) {
                return Err(MezError::config(
                    "permissions.bubblewrap.toolchains contains duplicate kinds",
                ));
            }
            names.push(kind.as_str().to_string());
        }
        Ok(names)
    }

    /// Appends redacted toolchain command metadata without discovered roots.
    fn append_toolchain_audit(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        operation: ToolchainCommand,
        changed: bool,
        outcome: &str,
    ) -> Result<()> {
        let generation = self.session.config_generation;
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let mut record = AuditRecord::new(
            self.session.id.to_string(),
            AuditActor {
                kind: "client".to_string(),
                id: primary_client_id.as_str().to_string(),
            },
            "toolchain",
            operation.as_str(),
        )
        .with_pane_id(pane_id.to_string())
        .with_metadata(
            "kind",
            operation.kind().map_or("all", SandboxToolchainKind::as_str),
        )
        .with_metadata("changed", changed.to_string())
        .with_metadata("config_generation", generation.to_string());
        record.outcome = outcome.to_string();
        audit_log.append(record.sanitized())?;
        Ok(())
    }
}

/// Parses the exact public grammar, including mandatory mutation confirmation.
fn parse_toolchain_command(args: &str) -> Result<ToolchainCommand> {
    let words = args.split_ascii_whitespace().collect::<Vec<_>>();
    match words.as_slice() {
        [] | ["status"] => Ok(ToolchainCommand::Status),
        ["list"] => Ok(ToolchainCommand::List),
        ["detect"] => Ok(ToolchainCommand::Detect(SandboxToolchainKind::Rust)),
        ["detect", name] => parse_sandbox_toolchain_kind(name)
            .map(ToolchainCommand::Detect)
            .ok_or_else(|| MezError::invalid_args("toolchain detect received an unsupported kind")),
        ["enable", name, "--yes"] => parse_sandbox_toolchain_kind(name)
            .map(ToolchainCommand::Enable)
            .ok_or_else(|| MezError::invalid_args("toolchain enable received an unsupported kind")),
        ["disable", name, "--yes"] => parse_sandbox_toolchain_kind(name)
            .map(ToolchainCommand::Disable)
            .ok_or_else(|| {
                MezError::invalid_args("toolchain disable received an unsupported kind")
            }),
        ["reload"] => Ok(ToolchainCommand::Reload),
        _ => Err(MezError::invalid_args(
            "toolchain expects status, list, detect [kind], enable kind --yes, disable kind --yes, or reload",
        )),
    }
}

/// Renders one successful pane-bootstrap detection without persisting roots.
fn detect_toolchain_detail(
    kind: SandboxToolchainKind,
    environment_managers: &[String],
    host_os: &str,
) -> Result<String> {
    match kind {
        SandboxToolchainKind::Rust => {
            let discovery = discover_rust_from_environment_managers(environment_managers)
                .map_err(|error| MezError::invalid_state(error.message()))?;
            Ok(format!(
                "cargo_bin={} rustup_home={} sandbox_path={}",
                json_escape(&discovery.cargo_bin.display().to_string()),
                json_escape(&discovery.rustup_home.display().to_string()),
                SANDBOX_RUST_PATH,
            ))
        }
        SandboxToolchainKind::Zig => {
            let projection = resolve_toolchain_projection(&[kind], environment_managers, host_os)
                .map_err(|error| MezError::invalid_state(error.message()))?
                .ok_or_else(|| {
                    MezError::invalid_state("Zig projection unexpectedly resolved empty")
                })?;
            let root = projection.roots.first().ok_or_else(|| {
                MezError::invalid_state("Zig projection is missing its distribution root")
            })?;
            Ok(format!(
                "zig_root={} sandbox_path={}",
                json_escape(&root.host_path.display().to_string()),
                SANDBOX_ZIG_PATH,
            ))
        }
    }
}

/// Renders the complete pane-local status without ambient environment data.
fn render_toolchain_status(pane_id: &str, status: &ToolchainStatus) -> String {
    format!(
        "pane={} backend={} supported={} configured={} discoverable={} discovery={} effective={} cargo_bin={} rustup_home={} zig_root={} discovery_error={} rust_sandbox_path={} zig_sandbox_path={} generation={} source=active-pane-bootstrap",
        json_escape(pane_id),
        status.backend,
        supported_toolchain_names().join(","),
        render_names(&status.configured),
        render_names(&status.discoverable),
        status.discovery_state,
        status.effective_state,
        status
            .cargo_bin
            .as_deref()
            .map(json_escape)
            .unwrap_or_else(|| "none".to_string()),
        status
            .rustup_home
            .as_deref()
            .map(json_escape)
            .unwrap_or_else(|| "none".to_string()),
        status
            .zig_root
            .as_deref()
            .map(json_escape)
            .unwrap_or_else(|| "none".to_string()),
        status
            .discovery_error
            .as_deref()
            .map(json_escape)
            .unwrap_or_else(|| "none".to_string()),
        SANDBOX_RUST_PATH,
        SANDBOX_ZIG_PATH,
        status.generation,
    )
}

/// Returns stable supported kind names from the shared metadata owner.
fn supported_toolchain_names() -> Vec<&'static str> {
    SUPPORTED_SANDBOX_TOOLCHAIN_KINDS
        .iter()
        .copied()
        .map(SandboxToolchainKind::as_str)
        .collect()
}

/// Renders an empty kind selection explicitly rather than as an empty field.
fn render_names(names: &[String]) -> String {
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(",")
    }
}

/// Converts a nested JSON-RPC error from the full reload path into a typed error.
fn reject_control_error(response: &str) -> Result<()> {
    let value: serde_json::Value = serde_json::from_str(response).map_err(|error| {
        MezError::invalid_state(format!("invalid config reload response: {error}"))
    })?;
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("config reload failed");
        return Err(MezError::invalid_state(message));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ToolchainCommand, parse_toolchain_command};
    use crate::runtime::SandboxToolchainKind;

    /// Verifies the command grammar accepts only documented operations and
    /// requires explicit confirmation for every persisted mutation.
    #[test]
    fn toolchain_parser_is_strict_and_requires_confirmation() {
        assert_eq!(
            parse_toolchain_command("").unwrap(),
            ToolchainCommand::Status
        );
        assert_eq!(
            parse_toolchain_command("detect rust").unwrap(),
            ToolchainCommand::Detect(SandboxToolchainKind::Rust)
        );
        assert_eq!(
            parse_toolchain_command("enable rust --yes").unwrap(),
            ToolchainCommand::Enable(SandboxToolchainKind::Rust)
        );
        assert_eq!(
            parse_toolchain_command("enable zig --yes").unwrap(),
            ToolchainCommand::Enable(SandboxToolchainKind::Zig)
        );
        for invalid in [
            "enable rust",
            "disable rust",
            "enable rust --yes --yes",
            "detect python",
            "status extra",
            "reload extra",
        ] {
            assert!(parse_toolchain_command(invalid).is_err(), "{invalid}");
        }
    }
}
