//! Live typed sandbox toolchain command handling.
//!
//! This module owns the `/toolchain` grammar and its runtime effects. It reads
//! discovery only from active-pane bootstrap evidence, persists only allowlisted
//! kind names, delegates live config changes to the transactional mutation
//! helper, and delegates reload to the existing full control-plane operation.
//! Discovered host roots are shown only in direct pane-local status output and
//! are never written to config or durable audit metadata.

use std::collections::BTreeMap;

use super::shell::AgentShellCommandOrigin;
use super::{
    AgentShellCommandOutcome, ConfigMutation, ConfigMutationOperation, ConfigMutationValue,
    MezError, Result, RuntimeSessionService, current_unix_seconds, json_escape,
    parse_slash_command, runtime_apply_persisted_config_mutation_batch,
    runtime_apply_persisted_config_mutation_batch_atomically, runtime_effective_config_value,
    runtime_primary_config_path,
};
use crate::integrations::agent::slash::AgentShellPresentation;
use crate::runtime::control::{
    normalized_custom_toolchain_mutation_digest, normalized_toolchain_selectors_digest,
};
use crate::runtime::{
    CustomToolchainDefinition, CustomToolchainName, SandboxConfig, SandboxToolchainKind,
    ToolchainSelection,
};
use crate::security::audit::{AuditActor, AuditRecord};
use crate::security::sandbox::{
    SANDBOX_BUN_PATH, SANDBOX_DENO_PATH, SANDBOX_GO_PATH, SANDBOX_NODE_PATH, SANDBOX_PYTHON_PATH,
    SANDBOX_RUST_PATH, SANDBOX_ZIG_PATH, SUPPORTED_SANDBOX_TOOLCHAIN_KINDS, ToolchainDescriptor,
    ToolchainPlatform, discover_rust_from_environment_managers,
    resolve_configured_toolchain_projection_for_project, resolve_toolchain_projection,
    toolchain_descriptor,
};

/// Strict operation accepted by `/toolchain`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolchainCommand {
    /// Reports configured, discoverable, and effective state.
    Status(Option<ToolchainSelection>),
    /// Lists stable supported kinds.
    List,
    /// Validates active-pane discovery evidence without mutation.
    Detect(ToolchainSelection),
    /// Enables ordered typed projections after explicit confirmation.
    Enable(Vec<ToolchainSelection>),
    /// Disables ordered typed projections after explicit confirmation.
    Disable(Vec<ToolchainSelection>),
    /// Creates or atomically replaces one constrained custom definition.
    Define {
        name: CustomToolchainName,
        definition: CustomToolchainDefinition,
    },
    /// Removes one custom definition, optionally disabling it atomically.
    Remove {
        name: CustomToolchainName,
        disable: bool,
    },
    /// Runs the existing full disk-backed configuration reload.
    Reload,
}

impl ToolchainCommand {
    /// Returns the stable operation spelling used in output and audit records.
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Status(_) => "status",
            Self::List => "list",
            Self::Detect(_) => "detect",
            Self::Enable(_) => "enable",
            Self::Disable(_) => "disable",
            Self::Define { .. } => "define",
            Self::Remove { .. } => "remove",
            Self::Reload => "reload",
        }
    }

    /// Returns the typed kind associated with a kind-specific operation.
    const fn kind(&self) -> Option<SandboxToolchainKind> {
        match self {
            Self::Detect(ToolchainSelection::BuiltIn(kind)) => Some(*kind),
            Self::Detect(ToolchainSelection::Custom(_)) => None,
            Self::Enable(_) | Self::Disable(_) | Self::Define { .. } | Self::Remove { .. } => None,
            Self::Status(_) | Self::List | Self::Reload => None,
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
    go_root: Option<String>,
    deno_root: Option<String>,
    bun_root: Option<String>,
    node_root: Option<String>,
    python_root: Option<String>,
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
        origin: AgentShellCommandOrigin,
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
        if let Some(settlement) = parse_toolchain_settlement(&invocation.args)? {
            if !origin.is_authenticated_primary_input() {
                return Err(MezError::forbidden(
                    "toolchain mutation settlement requires authenticated primary-client input",
                ));
            }
            return self.settle_pending_toolchain_mutation(primary_client_id, pane_id, settlement);
        }
        let operation = parse_toolchain_command(&invocation.args)?;
        if matches!(
            operation,
            ToolchainCommand::Enable(_)
                | ToolchainCommand::Disable(_)
                | ToolchainCommand::Define { .. }
                | ToolchainCommand::Remove { .. }
                | ToolchainCommand::Reload
        ) && !origin.is_authenticated_primary_input()
        {
            return Err(MezError::forbidden(
                "toolchain mutations require authenticated primary-client input",
            ));
        }
        match &operation {
            ToolchainCommand::Status(requested) => {
                let status = self.toolchain_status_for_pane(pane_id)?;
                self.append_toolchain_audit(
                    primary_client_id,
                    pane_id,
                    &operation,
                    false,
                    "inspected",
                )?;
                Ok(AgentShellCommandOutcome::Presented {
                    command: "toolchain".to_string(),
                    body: match requested {
                        Some(ToolchainSelection::Custom(name)) => {
                            self.render_custom_toolchain_inspection(pane_id, name, "Status")?
                        }
                        Some(ToolchainSelection::BuiltIn(kind)) => {
                            render_toolchain_status(pane_id, &status, Some(*kind))
                        }
                        None => render_toolchain_status(pane_id, &status, None),
                    },
                    presentation: AgentShellPresentation::Pager,
                })
            }
            ToolchainCommand::List => {
                self.append_toolchain_audit(
                    primary_client_id,
                    pane_id,
                    &operation,
                    false,
                    "listed",
                )?;
                Ok(AgentShellCommandOutcome::Presented {
                    command: "toolchain".to_string(),
                    body: render_toolchain_list(&self.configured_permissions().sandbox),
                    presentation: AgentShellPresentation::Pager,
                })
            }
            ToolchainCommand::Detect(selection) => {
                let status = self.toolchain_status_for_pane(pane_id)?;
                let body = match selection {
                    ToolchainSelection::BuiltIn(kind) => {
                        let signature =
                            self.pane_environment_signature(pane_id).ok_or_else(|| {
                                MezError::invalid_state(
                                    "toolchain detection requires active-pane bootstrap evidence",
                                )
                            })?;
                        let detail = detect_toolchain_detail(
                            *kind,
                            &signature.environment_managers,
                            &signature.os,
                        )?;
                        render_toolchain_detection(pane_id, *kind, &detail, &status)
                    }
                    ToolchainSelection::Custom(name) => {
                        self.render_custom_toolchain_inspection(pane_id, name, "Detection")?
                    }
                };
                self.append_toolchain_audit(
                    primary_client_id,
                    pane_id,
                    &operation,
                    false,
                    "detected",
                )?;
                Ok(AgentShellCommandOutcome::Presented {
                    command: "toolchain".to_string(),
                    body,
                    presentation: AgentShellPresentation::Pager,
                })
            }
            ToolchainCommand::Enable(selectors) => {
                self.preflight_toolchain_enable(pane_id, selectors)?;
                self.mutate_toolchains(primary_client_id, pane_id, &operation, selectors, true)
            }
            ToolchainCommand::Disable(selectors) => {
                self.mutate_toolchains(primary_client_id, pane_id, &operation, selectors, false)
            }
            ToolchainCommand::Define { name, definition } => self.define_custom_toolchain(
                primary_client_id,
                pane_id,
                &operation,
                name,
                definition,
            ),
            ToolchainCommand::Remove { name, disable } => {
                self.remove_custom_toolchain(primary_client_id, pane_id, &operation, name, *disable)
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
                    &operation,
                    changed,
                    "reloaded",
                )?;
                Ok(AgentShellCommandOutcome::Presented {
                    command: "toolchain".to_string(),
                    body: format!(
                        "Reloaded the full configuration; changed={changed}; generation {} → {}; changes apply to subsequent actions, while existing shells and running actions are unchanged.",
                        before.generation, after.generation,
                    ),
                    presentation: AgentShellPresentation::Notice,
                })
            }
        }
    }

    /// Settles one exact external mutation request through direct primary
    /// input. Successful confirmation is one-shot; stale, changed-generation,
    /// wrong-pane, mismatched-digest, and replay attempts fail closed.
    fn settle_pending_toolchain_mutation(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        settlement: ToolchainMutationSettlement,
    ) -> Result<AgentShellCommandOutcome> {
        let pending = self
            .control
            .pending_toolchain_mutation(&settlement.request_id)
            .cloned()
            .ok_or_else(|| {
                MezError::invalid_state("toolchain mutation request is missing or already settled")
            })?;
        if pending.pane_id != pane_id {
            return Err(MezError::forbidden(
                "toolchain mutation request belongs to another pane",
            ));
        }
        if current_unix_seconds() > pending.expires_at_unix_seconds {
            self.control
                .remove_pending_toolchain_mutation(&settlement.request_id);
            return Err(MezError::invalid_state(
                "toolchain mutation request has expired",
            ));
        }
        if self.session.config_generation != pending.config_generation {
            self.control
                .remove_pending_toolchain_mutation(&settlement.request_id);
            return Err(MezError::conflict(
                "toolchain mutation request is stale because configuration changed",
            ));
        }
        let normalized_digest = if let Some(name) = &pending.custom_name {
            normalized_custom_toolchain_mutation_digest(
                &pending.operation,
                name,
                pending.definition.as_ref(),
                pending.disable,
            )
        } else {
            normalized_toolchain_selectors_digest(&pending.operation, &pending.selectors)
        };
        if settlement.digest != pending.digest || pending.digest != normalized_digest {
            return Err(MezError::forbidden(
                "toolchain mutation confirmation digest does not match the pending request",
            ));
        }
        if !settlement.approve {
            self.append_pending_toolchain_settlement_audit(
                primary_client_id,
                &pending,
                "rejected",
            )?;
            self.control
                .remove_pending_toolchain_mutation(&settlement.request_id);
            return Ok(AgentShellCommandOutcome::Presented {
                command: "toolchain".to_string(),
                body: format!(
                    "Rejected pending toolchain {} request for {}; no configuration changed.",
                    pending.operation,
                    pending.custom_name.as_ref().map_or_else(
                        || pending
                            .selectors
                            .iter()
                            .map(ToolchainSelection::as_str)
                            .collect::<Vec<_>>()
                            .join(", "),
                        |name| name.selector().to_string(),
                    ),
                ),
                presentation: AgentShellPresentation::Notice,
            });
        }

        let operation = match pending.operation.as_str() {
            "enable" => ToolchainCommand::Enable(pending.selectors.clone()),
            "disable" => ToolchainCommand::Disable(pending.selectors.clone()),
            "define" => ToolchainCommand::Define {
                name: pending.custom_name.clone().ok_or_else(|| {
                    MezError::invalid_state("pending custom definition is missing its name")
                })?,
                definition: pending.definition.clone().ok_or_else(|| {
                    MezError::invalid_state("pending custom definition is missing its payload")
                })?,
            },
            "remove" => ToolchainCommand::Remove {
                name: pending.custom_name.clone().ok_or_else(|| {
                    MezError::invalid_state("pending custom removal is missing its name")
                })?,
                disable: pending.disable,
            },
            _ => {
                return Err(MezError::invalid_state(
                    "pending toolchain mutation contains an unsupported operation",
                ));
            }
        };
        let outcome = match &operation {
            ToolchainCommand::Enable(selectors) => {
                self.preflight_toolchain_enable(pane_id, selectors)?;
                self.mutate_toolchains(primary_client_id, pane_id, &operation, selectors, true)?
            }
            ToolchainCommand::Disable(selectors) => {
                self.mutate_toolchains(primary_client_id, pane_id, &operation, selectors, false)?
            }
            ToolchainCommand::Define { name, definition } => self.define_custom_toolchain(
                primary_client_id,
                pane_id,
                &operation,
                name,
                definition,
            )?,
            ToolchainCommand::Remove { name, disable } => self.remove_custom_toolchain(
                primary_client_id,
                pane_id,
                &operation,
                name,
                *disable,
            )?,
            _ => return Err(MezError::invalid_state("pending mutation is not mutating")),
        };
        self.append_pending_toolchain_settlement_audit(primary_client_id, &pending, "applied")?;
        self.control
            .remove_pending_toolchain_mutation(&settlement.request_id);
        Ok(outcome)
    }

    /// Appends redacted provenance for one primary-input settlement without
    /// retaining host roots or other discovery evidence.
    fn append_pending_toolchain_settlement_audit(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pending: &crate::runtime::control::PendingToolchainMutation,
        outcome: &str,
    ) -> Result<()> {
        let Some(audit_log) = self.persistence.audit_log_mut() else {
            return Ok(());
        };
        let mut record = AuditRecord::new(
            self.session.id.to_string(),
            AuditActor {
                kind: "client".to_string(),
                id: primary_client_id.to_string(),
            },
            "toolchain_mutation",
            "settle",
        )
        .with_pane_id(pending.pane_id.clone())
        .with_metadata("origin", "authenticated_primary_input")
        .with_metadata(
            "submitted_by_client_id",
            pending.submitted_by_client_id.clone(),
        )
        .with_metadata("operation", pending.operation.clone())
        .with_metadata(
            "selectors",
            pending.custom_name.as_ref().map_or_else(
                || {
                    pending
                        .selectors
                        .iter()
                        .map(ToolchainSelection::as_str)
                        .collect::<Vec<_>>()
                        .join(",")
                },
                |name| name.selector().to_string(),
            ),
        )
        .with_metadata("request_digest_prefix", pending.digest[..24].to_string())
        .with_metadata(
            "config_generation",
            self.session.config_generation.to_string(),
        );
        record.outcome = outcome.to_string();
        audit_log.append(record.sanitized())?;
        Ok(())
    }

    /// Applies one confirmed ordered selection mutation transactionally.
    fn mutate_toolchains(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        operation: &ToolchainCommand,
        selectors: &[ToolchainSelection],
        enable: bool,
    ) -> Result<AgentShellCommandOutcome> {
        let before = self.configured_toolchain_names()?;
        let mut after = before.clone();
        if enable {
            for selector in selectors {
                if !after.iter().any(|name| name == selector.as_str()) {
                    after.push(selector.as_str().to_string());
                }
            }
        } else {
            after.retain(|name| !selectors.iter().any(|selector| selector.as_str() == name));
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
        let action = if enable { "Enabled" } else { "Disabled" };
        let result = if report.changed { "updated" } else { "no-op" };
        let selector_names = selectors
            .iter()
            .map(ToolchainSelection::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        Ok(AgentShellCommandOutcome::Presented {
            command: "toolchain".to_string(),
            body: format!(
                "{action} {}; {result}; changed={}; generation {generation_before} → {generation_after}; applies to subsequent actions.",
                selector_names, report.changed,
            ),
            presentation: AgentShellPresentation::Notice,
        })
    }

    /// Creates or replaces one custom definition through one final-document
    /// validation, persistence, and live-application transaction.
    fn define_custom_toolchain(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        operation: &ToolchainCommand,
        name: &CustomToolchainName,
        definition: &CustomToolchainDefinition,
    ) -> Result<AgentShellCommandOutcome> {
        let SandboxConfig::Bubblewrap(current) = &self.configured_permissions().sandbox else {
            return Err(MezError::invalid_state(
                "custom toolchain definition requires the Bubblewrap sandbox backend",
            ));
        };
        if current.custom_toolchains.get(name.name()) == Some(definition) {
            let generation = self.session.config_generation;
            self.append_toolchain_audit(primary_client_id, pane_id, operation, false, "no_op")?;
            return Ok(AgentShellCommandOutcome::Presented {
                command: "toolchain".to_string(),
                body: format!(
                    "Defined custom:{}; no-op; changed=false; generation {generation} → {generation}.",
                    name.name(),
                ),
                presentation: AgentShellPresentation::Notice,
            });
        }
        let mut candidate = current.clone();
        candidate
            .custom_toolchains
            .insert(name.name().to_string(), definition.clone());
        self.preflight_toolchain_candidate(pane_id, &candidate, Some(name))?;

        let base = format!("permissions.bubblewrap.custom_toolchains.{}", name.name());
        let mut mutations = vec![ConfigMutation {
            path: base.clone(),
            operation: ConfigMutationOperation::Unset,
        }];
        if let Some(description) = &definition.description {
            mutations.push(ConfigMutation {
                path: format!("{base}.description"),
                operation: ConfigMutationOperation::Set(ConfigMutationValue::String(
                    description.clone(),
                )),
            });
        }
        mutations.extend([
            ConfigMutation {
                path: format!("{base}.roots"),
                operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(
                    definition.roots.clone(),
                )),
            },
            ConfigMutation {
                path: format!("{base}.path_entries"),
                operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(
                    definition
                        .path_entries
                        .iter()
                        .map(|reference| reference.as_str())
                        .collect(),
                )),
            },
            ConfigMutation {
                path: format!("{base}.required_executables"),
                operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(
                    definition
                        .required_executables
                        .iter()
                        .map(|reference| reference.as_str())
                        .collect(),
                )),
            },
        ]);
        for (variable, reference) in &definition.environment {
            mutations.push(ConfigMutation {
                path: format!("{base}.environment.{variable}"),
                operation: ConfigMutationOperation::Set(ConfigMutationValue::String(
                    reference.as_str(),
                )),
            });
        }

        let path = runtime_primary_config_path(self)?.ok_or_else(|| {
            MezError::invalid_state("custom toolchain definition requires a primary config path")
        })?;
        let generation_before = self.session.config_generation;
        let report = runtime_apply_persisted_config_mutation_batch_atomically(
            self,
            path,
            &mutations,
            "agent/shell/toolchain-define",
        )?;
        let generation_after = self.session.config_generation;
        self.append_toolchain_audit(
            primary_client_id,
            pane_id,
            operation,
            report.changed,
            if report.changed { "applied" } else { "no_op" },
        )?;
        Ok(AgentShellCommandOutcome::Presented {
            command: "toolchain".to_string(),
            body: format!(
                "Defined custom:{}; changed={}; generation {generation_before} → {generation_after}; enabled replacements apply to subsequent actions.",
                name.name(),
                report.changed,
            ),
            presentation: AgentShellPresentation::Notice,
        })
    }

    /// Removes one custom definition, optionally disabling its selector in the
    /// same atomic configuration transaction.
    fn remove_custom_toolchain(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        operation: &ToolchainCommand,
        name: &CustomToolchainName,
        disable: bool,
    ) -> Result<AgentShellCommandOutcome> {
        let selector = name.selector();
        let before = self.configured_toolchain_names()?;
        let enabled = before.iter().any(|configured| configured == selector);
        if enabled && !disable {
            return Err(MezError::conflict(
                "enabled custom toolchain removal requires --disable",
            ));
        }
        let definition_exists = matches!(
            &self.configured_permissions().sandbox,
            SandboxConfig::Bubblewrap(config) if config.custom_toolchains.contains_key(name.name())
        );
        if !enabled && !definition_exists {
            let generation = self.session.config_generation;
            self.append_toolchain_audit(primary_client_id, pane_id, operation, false, "no_op")?;
            return Ok(AgentShellCommandOutcome::Presented {
                command: "toolchain".to_string(),
                body: format!(
                    "Removed custom:{}; no-op; changed=false; generation {generation} → {generation}.",
                    name.name(),
                ),
                presentation: AgentShellPresentation::Notice,
            });
        }
        let after = before
            .into_iter()
            .filter(|configured| configured != selector)
            .collect::<Vec<_>>();
        let mut mutations = Vec::new();
        if enabled {
            mutations.push(ConfigMutation {
                path: "permissions.bubblewrap.toolchains".to_string(),
                operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(after)),
            });
        }
        mutations.push(ConfigMutation {
            path: format!("permissions.bubblewrap.custom_toolchains.{}", name.name()),
            operation: ConfigMutationOperation::Unset,
        });
        let path = runtime_primary_config_path(self)?.ok_or_else(|| {
            MezError::invalid_state("custom toolchain removal requires a primary config path")
        })?;
        let generation_before = self.session.config_generation;
        let report = runtime_apply_persisted_config_mutation_batch_atomically(
            self,
            path,
            &mutations,
            "agent/shell/toolchain-remove",
        )?;
        let generation_after = self.session.config_generation;
        self.append_toolchain_audit(
            primary_client_id,
            pane_id,
            operation,
            report.changed,
            if report.changed { "applied" } else { "no_op" },
        )?;
        Ok(AgentShellCommandOutcome::Presented {
            command: "toolchain".to_string(),
            body: format!(
                "Removed custom:{}; changed={}; generation {generation_before} → {generation_after}; applies to subsequent actions.",
                name.name(),
                report.changed,
            ),
            presentation: AgentShellPresentation::Notice,
        })
    }

    /// Resolves the complete candidate selection through the production
    /// descriptor/custom projection compiler before persistence.
    fn preflight_toolchain_enable(
        &self,
        pane_id: &str,
        selectors: &[ToolchainSelection],
    ) -> Result<()> {
        let SandboxConfig::Bubblewrap(current) = &self.configured_permissions().sandbox else {
            return Err(MezError::invalid_state(
                "toolchain enable requires the Bubblewrap sandbox backend",
            ));
        };
        let mut candidate = current.clone();
        for selector in selectors {
            if !candidate.toolchain_selections.contains(selector) {
                candidate.toolchain_selections.push(selector.clone());
            }
        }
        candidate.toolchains = candidate
            .toolchain_selections
            .iter()
            .filter_map(|selector| match selector {
                ToolchainSelection::BuiltIn(kind) => Some(*kind),
                ToolchainSelection::Custom(_) => None,
            })
            .collect();
        self.preflight_toolchain_candidate(pane_id, &candidate, None)
    }

    /// Resolves and authority-checks a complete candidate Bubblewrap
    /// toolchain configuration without mutating disk or live state.
    fn preflight_toolchain_candidate(
        &self,
        pane_id: &str,
        candidate: &crate::runtime::BubblewrapConfig,
        ensure_custom: Option<&CustomToolchainName>,
    ) -> Result<()> {
        let signature = self.pane_environment_signature(pane_id).ok_or_else(|| {
            MezError::invalid_state("toolchain preflight requires active-pane bootstrap evidence")
        })?;
        let mut candidate = candidate.clone();
        if let Some(name) = ensure_custom {
            let selector = ToolchainSelection::Custom(name.clone());
            if !candidate.toolchain_selections.contains(&selector) {
                candidate.toolchain_selections.push(selector);
            }
        }
        let protected_roots = self
            .integration
            .config_root()
            .into_iter()
            .chain(self.session.socket_path().parent())
            .map(std::path::PathBuf::from)
            .collect::<Vec<_>>();
        let projection = resolve_configured_toolchain_projection_for_project(
            &candidate,
            &signature.environment_managers,
            &signature.os,
            self.trusted_project_root_for_pane(pane_id).as_deref(),
            &protected_roots,
        )
        .map_err(|error| MezError::invalid_state(error.message()))?;
        if let Some(projection) = projection
            && !projection.custom_names.is_empty()
        {
            let authority = self.path_scopes_for_pane(pane_id).ok_or_else(|| {
                MezError::invalid_state(
                    "custom toolchain preflight requires resolved pane read authority",
                )
            })?;
            projection
                .validate_authority(&authority)
                .map_err(|error| MezError::invalid_state(error.message()))?;
        }
        Ok(())
    }

    /// Renders one configured custom definition after resolving its exact
    /// fixed projection and validating it against pane read authority.
    fn render_custom_toolchain_inspection(
        &self,
        pane_id: &str,
        name: &CustomToolchainName,
        title: &str,
    ) -> Result<String> {
        let SandboxConfig::Bubblewrap(config) = &self.configured_permissions().sandbox else {
            return Err(MezError::invalid_state(
                "custom toolchain inspection requires the Bubblewrap sandbox backend",
            ));
        };
        let definition = config.custom_toolchains.get(name.name()).ok_or_else(|| {
            MezError::invalid_args(format!(
                "custom toolchain definition `{}` is not configured",
                name.selector()
            ))
        })?;
        let mut candidate = config.clone();
        candidate.toolchain_selections = vec![ToolchainSelection::Custom(name.clone())];
        let signature = self.pane_environment_signature(pane_id).ok_or_else(|| {
            MezError::invalid_state(
                "custom toolchain inspection requires active-pane bootstrap evidence",
            )
        })?;
        let protected_roots = self
            .integration
            .config_root()
            .into_iter()
            .chain(self.session.socket_path().parent())
            .map(std::path::PathBuf::from)
            .collect::<Vec<_>>();
        let projection = resolve_configured_toolchain_projection_for_project(
            &candidate,
            &signature.environment_managers,
            &signature.os,
            self.trusted_project_root_for_pane(pane_id).as_deref(),
            &protected_roots,
        )
        .map_err(|error| MezError::invalid_state(error.message()))?
        .ok_or_else(|| MezError::invalid_state("custom toolchain projection resolved empty"))?;
        let authority = self.path_scopes_for_pane(pane_id).ok_or_else(|| {
            MezError::invalid_state(
                "custom toolchain inspection requires resolved pane read authority",
            )
        })?;
        projection
            .validate_authority(&authority)
            .map_err(|error| MezError::invalid_state(error.message()))?;
        let configured = config
            .toolchain_selections
            .contains(&ToolchainSelection::Custom(name.clone()));
        let roots = projection
            .roots
            .iter()
            .map(|root| {
                format!(
                    "`{}` → `{}`",
                    markdown_cell(&root.host_path.display().to_string()),
                    markdown_cell(&root.sandbox_destination),
                )
            })
            .collect::<Vec<_>>()
            .join("<br>");
        let path = projection
            .path_entries
            .iter()
            .map(|entry| format!("`{}`", markdown_cell(entry)))
            .collect::<Vec<_>>()
            .join("<br>");
        let environment = projection
            .environment
            .iter()
            .map(|(variable, value)| {
                format!("`{}={}`", markdown_cell(variable), markdown_cell(value),)
            })
            .collect::<Vec<_>>()
            .join("<br>");
        Ok(format!(
            "# Custom Toolchain {title}\n\n| Field | Value |\n| --- | --- |\n| Identity | `{}` |\n| Description | {} |\n| Configured | {} |\n| Pane | `{}` |\n| Generation | {} |\n| Authority | allowed |\n| Read-only roots | {} |\n| PATH entries | {} |\n| Environment | {} |\n| Required executables | {} |\n",
            name.selector(),
            definition
                .description
                .as_deref()
                .map(markdown_cell)
                .unwrap_or_else(|| "—".to_string()),
            yes_no(configured),
            markdown_cell(pane_id),
            self.session.config_generation,
            if roots.is_empty() { "—" } else { &roots },
            if path.is_empty() { "—" } else { &path },
            if environment.is_empty() {
                "—"
            } else {
                &environment
            },
            definition
                .required_executables
                .iter()
                .map(|reference| format!("`{}`", reference.as_str()))
                .collect::<Vec<_>>()
                .join("<br>"),
        ))
    }

    /// Builds status from effective config and active-pane bootstrap evidence.
    fn toolchain_status_for_pane(&self, pane_id: &str) -> Result<ToolchainStatus> {
        let configured = self.configured_toolchain_names()?;
        let backend = self.configured_permissions().sandbox.as_str();
        let (
            discovery_state,
            discoverable,
            cargo_bin,
            rustup_home,
            zig_root,
            go_root,
            deno_root,
            bun_root,
            node_root,
            python_root,
            discovery_error,
        ) = match self.pane_environment_signature(pane_id) {
            None if self.pane_bootstrap_is_pending(pane_id) => (
                "bootstrap-pending",
                Vec::new(),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ),
            None => (
                "environment-unavailable",
                Vec::new(),
                None,
                None,
                None,
                None,
                None,
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
                let go_root = match resolve_toolchain_projection(
                    &[SandboxToolchainKind::Go],
                    &signature.environment_managers,
                    &signature.os,
                ) {
                    Ok(Some(projection)) => {
                        discoverable.push("go".to_string());
                        projection
                            .roots
                            .first()
                            .map(|root| root.host_path.display().to_string())
                    }
                    Ok(None) => None,
                    Err(error) => {
                        errors.push(format!("go:{}", error.message()));
                        None
                    }
                };
                let deno_root = match resolve_toolchain_projection(
                    &[SandboxToolchainKind::Deno],
                    &signature.environment_managers,
                    &signature.os,
                ) {
                    Ok(Some(projection)) => {
                        discoverable.push("deno".to_string());
                        projection
                            .roots
                            .first()
                            .map(|root| root.host_path.display().to_string())
                    }
                    Ok(None) => None,
                    Err(error) => {
                        errors.push(format!("deno:{}", error.message()));
                        None
                    }
                };
                let bun_root = match resolve_toolchain_projection(
                    &[SandboxToolchainKind::Bun],
                    &signature.environment_managers,
                    &signature.os,
                ) {
                    Ok(Some(projection)) => {
                        discoverable.push("bun".to_string());
                        projection
                            .roots
                            .first()
                            .map(|root| root.host_path.display().to_string())
                    }
                    Ok(None) => None,
                    Err(error) => {
                        errors.push(format!("bun:{}", error.message()));
                        None
                    }
                };
                let node_root = match resolve_toolchain_projection(
                    &[SandboxToolchainKind::Node],
                    &signature.environment_managers,
                    &signature.os,
                ) {
                    Ok(Some(projection)) => {
                        discoverable.push("node".to_string());
                        projection
                            .roots
                            .first()
                            .map(|root| root.host_path.display().to_string())
                    }
                    Ok(None) => None,
                    Err(error) => {
                        errors.push(format!("node:{}", error.message()));
                        None
                    }
                };
                let python_root = match resolve_toolchain_projection(
                    &[SandboxToolchainKind::Python],
                    &signature.environment_managers,
                    &signature.os,
                ) {
                    Ok(Some(projection)) => {
                        discoverable.push("python".to_string());
                        projection
                            .roots
                            .first()
                            .map(|root| root.host_path.display().to_string())
                    }
                    Ok(None) => None,
                    Err(error) => {
                        errors.push(format!("python:{}", error.message()));
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
                    go_root,
                    deno_root,
                    bun_root,
                    node_root,
                    python_root,
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
            go_root,
            deno_root,
            bun_root,
            node_root,
            python_root,
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
            let selector = ToolchainSelection::parse(name)?;
            if names.iter().any(|existing| existing == selector.as_str()) {
                return Err(MezError::config(
                    "permissions.bubblewrap.toolchains contains duplicate selectors",
                ));
            }
            names.push(selector.as_str().to_string());
        }
        Ok(names)
    }

    /// Appends redacted toolchain command metadata without discovered roots.
    fn append_toolchain_audit(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        operation: &ToolchainCommand,
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

/// Exact primary-input decision for one externally submitted mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolchainMutationSettlement {
    request_id: String,
    digest: String,
    approve: bool,
}

/// Parses confirmation and rejection separately from ordinary toolchain
/// operations so the established typed operation enum remains copyable.
fn parse_toolchain_settlement(args: &str) -> Result<Option<ToolchainMutationSettlement>> {
    let words = args.split_ascii_whitespace().collect::<Vec<_>>();
    match words.as_slice() {
        ["confirm", request_id, digest, "--yes"] => Ok(Some(ToolchainMutationSettlement {
            request_id: (*request_id).to_string(),
            digest: (*digest).to_string(),
            approve: true,
        })),
        ["reject", request_id, digest] => Ok(Some(ToolchainMutationSettlement {
            request_id: (*request_id).to_string(),
            digest: (*digest).to_string(),
            approve: false,
        })),
        ["confirm", ..] | ["reject", ..] => Err(MezError::invalid_args(
            "toolchain settlement expects confirm request-id digest --yes or reject request-id digest",
        )),
        _ => Ok(None),
    }
}

/// Parses the exact public grammar, including mandatory mutation confirmation.
fn parse_toolchain_command(args: &str) -> Result<ToolchainCommand> {
    let words = shlex::split(args)
        .ok_or_else(|| MezError::invalid_args("toolchain arguments contain invalid quoting"))?;
    let words = words.iter().map(String::as_str).collect::<Vec<_>>();
    match words.as_slice() {
        [] | ["status"] => Ok(ToolchainCommand::Status(None)),
        ["status", selector] => ToolchainSelection::parse(selector)
            .map(|selection| ToolchainCommand::Status(Some(selection)))
            .map_err(|error| MezError::invalid_args(error.message())),
        ["list"] => Ok(ToolchainCommand::List),
        ["detect"] => Ok(ToolchainCommand::Detect(ToolchainSelection::BuiltIn(
            SandboxToolchainKind::Rust,
        ))),
        ["detect", selector] => ToolchainSelection::parse(selector)
            .map(ToolchainCommand::Detect)
            .map_err(|error| MezError::invalid_args(error.message())),
        ["enable", selectors @ .., "--yes"] => {
            parse_toolchain_selectors(selectors, "enable").map(ToolchainCommand::Enable)
        }
        ["disable", selectors @ .., "--yes"] => {
            parse_toolchain_selectors(selectors, "disable").map(ToolchainCommand::Disable)
        }
        ["define", name, options @ ..] => parse_custom_toolchain_definition(name, options),
        ["remove", selector, options @ ..] => parse_custom_toolchain_removal(selector, options),
        ["reload"] => Ok(ToolchainCommand::Reload),
        _ => Err(MezError::invalid_args(
            "toolchain expects status, list, detect [kind], define, enable, disable, remove, or reload",
        )),
    }
}

/// Parses one strict custom definition with repeatable root/reference flags.
fn parse_custom_toolchain_definition(name: &str, words: &[&str]) -> Result<ToolchainCommand> {
    let name = CustomToolchainName::parse(name)
        .map_err(|error| MezError::invalid_args(error.message()))?;
    let mut roots = Vec::new();
    let mut path_entries = Vec::new();
    let mut required_executables = Vec::new();
    let mut environment = BTreeMap::new();
    let mut description = None;
    let mut confirmed = false;
    let mut index = 0;
    while index < words.len() {
        let flag = words[index];
        if flag == "--yes" {
            if confirmed {
                return Err(MezError::invalid_args(
                    "toolchain define received duplicate --yes",
                ));
            }
            confirmed = true;
            index += 1;
            continue;
        }
        let value = words.get(index + 1).ok_or_else(|| {
            MezError::invalid_args(format!("toolchain define flag `{flag}` requires a value"))
        })?;
        match flag {
            "--root" => roots.push((*value).to_string()),
            "--path" => path_entries.push((*value).to_string()),
            "--require" => required_executables.push((*value).to_string()),
            "--description" => {
                if description.replace((*value).to_string()).is_some() {
                    return Err(MezError::invalid_args(
                        "toolchain define received duplicate --description",
                    ));
                }
            }
            "--env-root" => {
                let (variable, reference) = value.split_once('=').ok_or_else(|| {
                    MezError::invalid_args("toolchain define --env-root expects NAME=REFERENCE")
                })?;
                if environment
                    .insert(variable.to_string(), reference.to_string())
                    .is_some()
                {
                    return Err(MezError::invalid_args(
                        "toolchain define received a duplicate environment variable",
                    ));
                }
            }
            _ => {
                return Err(MezError::invalid_args(format!(
                    "toolchain define received unknown flag `{flag}`"
                )));
            }
        }
        index += 2;
    }
    if !confirmed {
        return Err(MezError::invalid_args(
            "toolchain define requires explicit --yes confirmation",
        ));
    }
    let definition = CustomToolchainDefinition::new(
        description,
        roots,
        path_entries,
        required_executables,
        environment,
    )
    .map_err(|error| MezError::invalid_args(error.message()))?;
    Ok(ToolchainCommand::Define { name, definition })
}

/// Parses removal of one custom selector with optional atomic disable.
fn parse_custom_toolchain_removal(selector: &str, words: &[&str]) -> Result<ToolchainCommand> {
    let name = selector
        .strip_prefix("custom:")
        .ok_or_else(|| MezError::invalid_args("toolchain remove requires custom:<name>"))?;
    let name = CustomToolchainName::parse(name)
        .map_err(|error| MezError::invalid_args(error.message()))?;
    let mut disable = false;
    let mut confirmed = false;
    for flag in words {
        match *flag {
            "--disable" if !disable => disable = true,
            "--yes" if !confirmed => confirmed = true,
            "--disable" | "--yes" => {
                return Err(MezError::invalid_args(
                    "toolchain remove received a duplicate flag",
                ));
            }
            _ => {
                return Err(MezError::invalid_args(format!(
                    "toolchain remove received unknown flag `{flag}`"
                )));
            }
        }
    }
    if !confirmed {
        return Err(MezError::invalid_args(
            "toolchain remove requires explicit --yes confirmation",
        ));
    }
    Ok(ToolchainCommand::Remove { name, disable })
}

/// Parses a non-empty, duplicate-free ordered selector list.
fn parse_toolchain_selectors(words: &[&str], operation: &str) -> Result<Vec<ToolchainSelection>> {
    if words.is_empty() {
        return Err(MezError::invalid_args(format!(
            "toolchain {operation} requires at least one selector"
        )));
    }
    let mut selectors = Vec::with_capacity(words.len());
    for word in words {
        let selector = ToolchainSelection::parse(word).map_err(|_| {
            MezError::invalid_args(format!(
                "toolchain {operation} received an unsupported selector"
            ))
        })?;
        if selectors.contains(&selector) {
            return Err(MezError::invalid_args(format!(
                "toolchain {operation} received a duplicate selector"
            )));
        }
        selectors.push(selector);
    }
    Ok(selectors)
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
        SandboxToolchainKind::Go => {
            let projection = resolve_toolchain_projection(&[kind], environment_managers, host_os)
                .map_err(|error| MezError::invalid_state(error.message()))?
                .ok_or_else(|| {
                    MezError::invalid_state("Go projection unexpectedly resolved empty")
                })?;
            let root = projection
                .roots
                .first()
                .ok_or_else(|| MezError::invalid_state("Go projection is missing its SDK root"))?;
            Ok(format!(
                "go_root={} sandbox_path={}",
                json_escape(&root.host_path.display().to_string()),
                SANDBOX_GO_PATH,
            ))
        }
        SandboxToolchainKind::Deno => {
            let projection = resolve_toolchain_projection(&[kind], environment_managers, host_os)
                .map_err(|error| MezError::invalid_state(error.message()))?
                .ok_or_else(|| {
                    MezError::invalid_state("Deno projection unexpectedly resolved empty")
                })?;
            let root = projection.roots.first().ok_or_else(|| {
                MezError::invalid_state("Deno projection is missing its runtime root")
            })?;
            Ok(format!(
                "deno_root={} sandbox_path={}",
                json_escape(&root.host_path.display().to_string()),
                SANDBOX_DENO_PATH,
            ))
        }
        SandboxToolchainKind::Bun => {
            let projection = resolve_toolchain_projection(&[kind], environment_managers, host_os)
                .map_err(|error| MezError::invalid_state(error.message()))?
                .ok_or_else(|| {
                    MezError::invalid_state("Bun projection unexpectedly resolved empty")
                })?;
            let root = projection.roots.first().ok_or_else(|| {
                MezError::invalid_state("Bun projection is missing its distribution root")
            })?;
            Ok(format!(
                "bun_root={} sandbox_path={}",
                json_escape(&root.host_path.display().to_string()),
                SANDBOX_BUN_PATH,
            ))
        }
        SandboxToolchainKind::Node => {
            let projection = resolve_toolchain_projection(&[kind], environment_managers, host_os)
                .map_err(|error| MezError::invalid_state(error.message()))?
                .ok_or_else(|| {
                    MezError::invalid_state("Node.js projection unexpectedly resolved empty")
                })?;
            let root = projection.roots.first().ok_or_else(|| {
                MezError::invalid_state("Node.js projection is missing its distribution root")
            })?;
            Ok(format!(
                "node_root={} sandbox_path={}",
                json_escape(&root.host_path.display().to_string()),
                SANDBOX_NODE_PATH,
            ))
        }
        SandboxToolchainKind::Python => {
            let projection = resolve_toolchain_projection(&[kind], environment_managers, host_os)
                .map_err(|error| MezError::invalid_state(error.message()))?
                .ok_or_else(|| {
                    MezError::invalid_state("Python projection unexpectedly resolved empty")
                })?;
            let root = projection.roots.first().ok_or_else(|| {
                MezError::invalid_state("Python projection is missing its runtime root")
            })?;
            Ok(format!(
                "python_root={} sandbox_path={}",
                json_escape(&root.host_path.display().to_string()),
                SANDBOX_PYTHON_PATH,
            ))
        }
    }
}

/// Renders the stable descriptor catalog as a searchable Markdown table.
fn render_toolchain_list(sandbox: &SandboxConfig) -> String {
    let mut body = String::from(
        "# Supported Toolchains\n\n| Kind | Platform | Evidence | Sandbox projection | Companions |\n| --- | --- | --- | --- | --- |\n",
    );
    for kind in SUPPORTED_SANDBOX_TOOLCHAIN_KINDS {
        let descriptor = toolchain_descriptor(kind);
        body.push_str(&format!(
            "| `{}` | {} | {} | {} | {} |\n",
            kind.as_str(),
            toolchain_platform_name(descriptor.platform),
            descriptor_root_labels(descriptor),
            descriptor_sandbox_destinations(descriptor),
            descriptor_companions(descriptor),
        ));
    }
    if let SandboxConfig::Bubblewrap(config) = sandbox
        && !config.custom_toolchains.is_empty()
    {
        body.push_str("\n## Custom Toolchains\n\n| Selector | Description | Configured |\n| --- | --- | --- |\n");
        for (name, definition) in &config.custom_toolchains {
            let selector = format!("custom:{name}");
            body.push_str(&format!(
                "| `{selector}` | {} | {} |\n",
                definition
                    .description
                    .as_deref()
                    .map(markdown_cell)
                    .unwrap_or_else(|| "—".to_string()),
                yes_no(
                    config
                        .toolchain_selections
                        .iter()
                        .any(|selection| selection.as_str() == selector)
                ),
            ));
        }
    }
    body.push_str("\nSelections are typed and constrained; they do not grant arbitrary PATH or mount authority.\n");
    body
}

/// Renders one successful detection as structured pane-local Markdown.
fn render_toolchain_detection(
    pane_id: &str,
    kind: SandboxToolchainKind,
    detail: &str,
    status: &ToolchainStatus,
) -> String {
    let descriptor = toolchain_descriptor(kind);
    format!(
        "# Toolchain Detection\n\n| Field | Value |\n| --- | --- |\n| Kind | `{}` |\n| Available | yes |\n| Configured | {} |\n| Pane | `{}` |\n| Generation | {} |\n| Evidence source | active-pane bootstrap |\n| Host evidence | `{}` |\n| Sandbox projection | {} |\n",
        kind.as_str(),
        if status.configured.iter().any(|name| name == kind.as_str()) {
            "yes"
        } else {
            "no"
        },
        markdown_cell(pane_id),
        status.generation,
        markdown_cell(detail),
        descriptor_sandbox_destinations(descriptor),
    )
}

/// Returns a compact platform label for descriptor tables.
const fn toolchain_platform_name(platform: ToolchainPlatform) -> &'static str {
    match platform {
        ToolchainPlatform::Any => "any",
        ToolchainPlatform::Linux => "Linux",
        ToolchainPlatform::MacOs => "macOS",
        ToolchainPlatform::Windows => "Windows",
    }
}

/// Renders descriptor evidence labels without consulting the host.
fn descriptor_root_labels(descriptor: &ToolchainDescriptor) -> String {
    descriptor
        .roots
        .iter()
        .map(|root| format!("`{}`", root.evidence_kind))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Renders fixed descriptor-owned sandbox destinations.
fn descriptor_sandbox_destinations(descriptor: &ToolchainDescriptor) -> String {
    descriptor
        .roots
        .iter()
        .map(|root| format!("`{}`", root.sandbox_destination))
        .collect::<Vec<_>>()
        .join("<br>")
}

/// Renders required and optional descriptor companions.
fn descriptor_companions(descriptor: &ToolchainDescriptor) -> String {
    let required = descriptor
        .coupling
        .required
        .iter()
        .map(|kind| kind.as_str())
        .collect::<Vec<_>>();
    let optional = descriptor
        .coupling
        .optional
        .iter()
        .map(|kind| kind.as_str())
        .collect::<Vec<_>>();
    match (required.is_empty(), optional.is_empty()) {
        (true, true) => "none".to_string(),
        (false, true) => format!("required: {}", required.join(", ")),
        (true, false) => format!("optional: {}", optional.join(", ")),
        (false, false) => format!(
            "required: {}; optional: {}",
            required.join(", "),
            optional.join(", ")
        ),
    }
}

/// Escapes table delimiters and line breaks in pane-local Markdown cells.
fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', "<br>")
}

/// Renders the complete pane-local status without ambient environment data.
fn render_toolchain_status(
    pane_id: &str,
    status: &ToolchainStatus,
    requested: Option<SandboxToolchainKind>,
) -> String {
    let mut body = format!(
        "# Toolchains\n\n**Pane:** `{}`  \n**Backend:** `{}`  \n**Generation:** {}  \n**Discovery:** {}  \n**Effective state:** {}\n\n| Kind | Configured | Discoverable | Effective | Host evidence | Sandbox projection |\n| --- | --- | --- | --- | --- | --- |\n",
        markdown_cell(pane_id),
        status.backend,
        status.generation,
        status.discovery_state,
        status.effective_state,
    );
    for kind in SUPPORTED_SANDBOX_TOOLCHAIN_KINDS
        .into_iter()
        .filter(|kind| requested.is_none_or(|requested| requested == *kind))
    {
        let descriptor = toolchain_descriptor(kind);
        let configured = status
            .configured
            .iter()
            .any(|configured| configured == kind.as_str());
        let discoverable = status
            .discoverable
            .iter()
            .any(|available| available == kind.as_str());
        body.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} |\n",
            kind.as_str(),
            yes_no(configured),
            yes_no(discoverable),
            toolchain_kind_effective_state(configured, discoverable, status),
            toolchain_status_host_evidence(kind, status),
            descriptor_sandbox_destinations(descriptor),
        ));
    }
    if let Some(error) = status.discovery_error.as_deref() {
        body.push_str(&format!(
            "\n## Discovery diagnostics\n\n- {}\n",
            markdown_cell(error)
        ));
    }
    body
}

/// Returns a stable boolean label for Markdown presentation rows.
const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// Computes one descriptor row's effective state from shared status facts.
fn toolchain_kind_effective_state(
    configured: bool,
    discoverable: bool,
    status: &ToolchainStatus,
) -> &'static str {
    match (configured, discoverable, status.effective_state) {
        (true, true, "active") => "active",
        (true, true, _) => "selected-inactive",
        (true, false, _) => "selected-unavailable",
        (false, true, _) => "available-disabled",
        (false, false, _) => "disabled-unavailable",
    }
}

/// Projects pane-local host evidence into one descriptor-oriented table cell.
fn toolchain_status_host_evidence(kind: SandboxToolchainKind, status: &ToolchainStatus) -> String {
    let values = match kind {
        SandboxToolchainKind::Rust => [status.cargo_bin.as_deref(), status.rustup_home.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
        SandboxToolchainKind::Zig => vec![status.zig_root.as_deref()]
            .into_iter()
            .flatten()
            .collect(),
        SandboxToolchainKind::Go => vec![status.go_root.as_deref()]
            .into_iter()
            .flatten()
            .collect(),
        SandboxToolchainKind::Deno => vec![status.deno_root.as_deref()]
            .into_iter()
            .flatten()
            .collect(),
        SandboxToolchainKind::Bun => vec![status.bun_root.as_deref()]
            .into_iter()
            .flatten()
            .collect(),
        SandboxToolchainKind::Node => vec![status.node_root.as_deref()]
            .into_iter()
            .flatten()
            .collect(),
        SandboxToolchainKind::Python => vec![status.python_root.as_deref()]
            .into_iter()
            .flatten()
            .collect(),
    };
    if values.is_empty() {
        "—".to_string()
    } else {
        values
            .into_iter()
            .map(|value| format!("`{}`", markdown_cell(value)))
            .collect::<Vec<_>>()
            .join("<br>")
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
    use crate::runtime::{
        CustomToolchainDefinition, CustomToolchainName, SandboxToolchainKind, ToolchainSelection,
    };
    use std::collections::BTreeMap;

    /// Verifies the command grammar accepts only documented operations and
    /// requires explicit confirmation for every persisted mutation.
    #[test]
    fn toolchain_parser_is_strict_and_requires_confirmation() {
        assert_eq!(
            parse_toolchain_command("").unwrap(),
            ToolchainCommand::Status(None)
        );
        assert_eq!(
            parse_toolchain_command("detect rust").unwrap(),
            ToolchainCommand::Detect(ToolchainSelection::BuiltIn(SandboxToolchainKind::Rust,))
        );
        assert_eq!(
            parse_toolchain_command("status custom:acme").unwrap(),
            ToolchainCommand::Status(Some(ToolchainSelection::parse("custom:acme").unwrap(),))
        );
        assert_eq!(
            parse_toolchain_command("detect custom:acme").unwrap(),
            ToolchainCommand::Detect(ToolchainSelection::parse("custom:acme").unwrap())
        );
        assert_eq!(
            parse_toolchain_command("enable rust --yes").unwrap(),
            ToolchainCommand::Enable(vec![ToolchainSelection::BuiltIn(
                SandboxToolchainKind::Rust,
            )])
        );
        assert_eq!(
            parse_toolchain_command("enable zig --yes").unwrap(),
            ToolchainCommand::Enable(vec![
                ToolchainSelection::BuiltIn(SandboxToolchainKind::Zig,)
            ])
        );
        assert_eq!(
            parse_toolchain_command("detect python").unwrap(),
            ToolchainCommand::Detect(ToolchainSelection::BuiltIn(SandboxToolchainKind::Python,))
        );
        assert_eq!(
            parse_toolchain_command("enable python --yes").unwrap(),
            ToolchainCommand::Enable(vec![ToolchainSelection::BuiltIn(
                SandboxToolchainKind::Python,
            )])
        );
        assert_eq!(
            parse_toolchain_command("enable rust custom:acme --yes").unwrap(),
            ToolchainCommand::Enable(vec![
                ToolchainSelection::BuiltIn(SandboxToolchainKind::Rust),
                ToolchainSelection::parse("custom:acme").unwrap(),
            ])
        );
        assert_eq!(
            parse_toolchain_command(
                "define acme --root /opt/acme --path 0:bin --require 0:bin/acme --env-root ACME_HOME=0:. --description 'Acme SDK' --yes"
            )
            .unwrap(),
            ToolchainCommand::Define {
                name: CustomToolchainName::parse("acme").unwrap(),
                definition: CustomToolchainDefinition::new(
                    Some("Acme SDK".to_string()),
                    vec!["/opt/acme".to_string()],
                    vec!["0:bin".to_string()],
                    vec!["0:bin/acme".to_string()],
                    BTreeMap::from([("ACME_HOME".to_string(), "0:.".to_string())]),
                )
                .unwrap(),
            }
        );
        assert_eq!(
            parse_toolchain_command("remove custom:acme --disable --yes").unwrap(),
            ToolchainCommand::Remove {
                name: CustomToolchainName::parse("acme").unwrap(),
                disable: true,
            }
        );
        for invalid in [
            "enable rust",
            "disable rust",
            "enable rust --yes --yes",
            "enable rust rust --yes",
            "define acme --root /opt/acme --path 0:bin",
            "define acme --root /opt/acme --path 0:bin --description one --description two --yes",
            "define acme --root /opt/acme --path ../bin --yes",
            "remove acme --yes",
            "remove custom:acme --disable --disable --yes",
            "detect unknown",
            "status extra",
            "reload extra",
        ] {
            assert!(parse_toolchain_command(invalid).is_err(), "{invalid}");
        }
    }
}
