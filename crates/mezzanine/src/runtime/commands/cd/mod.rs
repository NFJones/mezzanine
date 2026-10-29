//! Native agent navigation through acceptance, worker preparation and adoption.
//!
//! The existing deferred-command lifecycle owns one operation. Filesystem work
//! prepares a complete cwd/config/instruction snapshot without mutating runtime
//! state. Adoption rechecks its exact conversation, source directory generation,
//! mode incarnation, policy generation and dependencies before publishing. Plain
//! navigation never writes a PTY, changes daemon cwd or retires a pane process.

mod preparation;

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::{ConfigLayer, EffectiveConfig};
use crate::error::{MezError, Result};
use crate::runtime::config::{ResourceAuthorityConfig, ShellMode};
use crate::runtime::{RuntimeAgentCommandAsyncWork, RuntimeSessionService};
use crate::security::project::ProjectTrustStore;
use mez_agent::instructions::DiscoveredInstructionFile;
use mez_agent::slash::{CdOptions, parse_cd_options};
use mez_agent::{AgentShellVisibility, SubagentScopeDeclaration};

/// Immutable acceptance inputs; these are data, never a mutation capability.
#[derive(Clone)]
pub(crate) struct NativeDirectoryWork {
    /// Literal parsed request, with replacement refused before worker admission.
    options: CdOptions,
    /// Exact pane/conversation/command accepted by the actor.
    pub(crate) pane: String,
    /// Exact conversation that accepted the literal request.
    pub(crate) conversation: String,
    /// Checked existing command-lifecycle generation assigned at acceptance.
    pub(crate) operation: u64,
    /// Native root lifetime independently observed around context capture.
    root_identity: (u32, u64),
    /// Exact source cwd selected by the native launch owner.
    source_directory: PathBuf,
    /// Usable home comes only from the native workload, never daemon fallback.
    home: Option<PathBuf>,
    /// Exact source conversation's navigation generation.
    source_generation: u64,
    /// Applied runtime policy generation captured at acceptance.
    config_generation: u64,
    /// Applied grants retain their real provenance for source invalidation.
    source_resources: ResourceAuthorityConfig,
    /// Current controller catalog root, distinct from target project overlays.
    config_root: Option<PathBuf>,
    /// Pane-local mode lifetime, preventing Native/Pane/Native ABA.
    pane_mode: Arc<()>,
    /// Default-mode lifetime, independent of local overrides.
    default_mode: Arc<()>,
    /// Outgoing effective configuration inputs, retained for guarded settlement.
    layers: Vec<ConfigLayer>,
    /// Existing trust decisions; selecting a directory never creates one.
    trust: Option<ProjectTrustStore>,
    /// Frozen inherited grants retain their own original resolution directory.
    child: Option<SubagentScopeDeclaration>,
    /// A local mode override continues to dominate trusted project overlays.
    mode_override: Option<ShellMode>,
}

impl std::fmt::Debug for NativeDirectoryWork {
    /// Keeps captured config text and home/environment values out of diagnostics.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeDirectoryWork")
            .field("pane", &self.pane)
            .field("conversation", &self.conversation)
            .field("operation", &self.operation)
            .field("source_directory", &self.source_directory)
            .field("source_generation", &self.source_generation)
            .field("config_generation", &self.config_generation)
            .field("layer_count", &self.layers.len())
            .finish()
    }
}

/// Complete read-only worker result, awaiting actor ownership checks.
pub(crate) struct NativeDirectoryRead {
    /// Canonical existing accessible target selected without shell expansion.
    directory: PathBuf,
    /// Point-in-time target identity rechecked before publication.
    target_identity: (u64, u64),
    /// Successful discovery, including a positively prepared empty set.
    instructions: Vec<DiscoveredInstructionFile>,
    /// Prepared target overlays replacing all outgoing project layers.
    layers: Vec<ConfigLayer>,
    /// Parsed effective configuration; actor settlement performs no file reads.
    effective: EffectiveConfig,
    /// Structured configuration used by existing runtime projections.
    structured: serde_json::Value,
    /// Exact captured source inputs, independently of the outer display claim.
    source: NativeDirectoryWork,
}

impl std::fmt::Debug for NativeDirectoryRead {
    /// Reports target/count diagnostics without instruction or config bodies.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeDirectoryRead")
            .field("directory", &self.directory)
            .field("instruction_count", &self.instructions.len())
            .field("layer_count", &self.layers.len())
            .finish()
    }
}

impl NativeDirectoryWork {
    /// Performs native read-only preparation on the deferred command worker.
    pub(crate) fn prepare(&self) -> Result<NativeDirectoryRead> {
        preparation::prepare(self)
    }
}

impl RuntimeSessionService {
    /// Observes the exact owned pane root lifetime without idle-shell or PTY
    /// control qualification. Plain navigation does not act on that process.
    fn native_directory_root_identity(&self, pane_id: &str) -> Result<(u32, u64)> {
        let pid = self
            .primary_pid_for_live_pane_process(pane_id)
            .ok_or_else(|| {
                MezError::invalid_state("native navigation root process is unavailable")
            })?;
        let identity =
            mez_mux::process::process_executable_identity_for_pid(pid).map_err(|error| {
                MezError::invalid_state(format!(
                    "native navigation root identity is unavailable for pid {pid}: {error:?}"
                ))
            })?;
        Ok((pid, identity.start_token))
    }

    /// Checks mode before any generic shell/bootstrap side effect or queueing.
    pub(crate) fn check_native_directory_request(
        &self,
        pane_id: &str,
        args: &str,
    ) -> Result<CdOptions> {
        if self.effective_agent_shell_mode_for_pane(pane_id) != ShellMode::Native {
            return Err(MezError::invalid_args(
                "/cd is only available in native shell mode; the pane shell directory was not changed",
            ));
        }
        let options = parse_cd_options(args).map_err(MezError::invalid_args)?;
        if options.sync_pane {
            return Err(MezError::invalid_args(
                "/cd --sync-pane is not implemented; no shell was replaced. A fresh shell would discard shell-local variables, functions, aliases, jobs and unsaved state",
            ));
        }
        if self.agent.agent_command_is_active(pane_id) {
            return Err(MezError::conflict(
                "/cd cannot run while another interactive command or history preparation is active",
            ));
        }
        self.ensure_native_directory_idle(pane_id)?;
        Ok(options)
    }

    /// Rejects work depending on the outgoing cwd; navigation is never queued
    /// for a later turn and never relocates an already-running native process.
    pub(crate) fn ensure_native_directory_idle(&self, pane_id: &str) -> Result<()> {
        let session = self.agent_shell_store().get(pane_id).ok_or_else(|| {
            MezError::invalid_state("native navigation requires an agent session")
        })?;
        if session.running_turn_id.is_some()
            || self.agent.native_directory_dependencies_active(pane_id)
            || self.agent_loop_is_active(pane_id)
        {
            return Err(MezError::conflict(
                "/cd requires an idle agent with no pending turn, native worker, history preparation or compaction",
            ));
        }
        Ok(())
    }

    /// Freezes the request at acceptance rather than rebasing it when a worker
    /// eventually claims it. The existing trust owner refreshes policy first.
    pub(crate) fn capture_native_directory_work(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<NativeDirectoryWork> {
        let invocation = mez_agent::slash::parse_slash_command(input)
            .map_err(|error| MezError::invalid_args(error.to_string()))?
            .ok_or_else(|| MezError::invalid_args("/cd requires slash input"))?;
        let options = self.check_native_directory_request(pane_id, &invocation.args)?;
        self.refresh_project_trust_store_from_disk_if_changed()?;
        let root_identity = self.native_directory_root_identity(pane_id)?;
        let context = self.native_agent_shell_context_for_pane(pane_id)?;
        if self.native_directory_root_identity(pane_id)? != root_identity {
            return Err(MezError::conflict(
                "native root changed during navigation capture",
            ));
        }
        let home = context
            .workload_environment()
            .workload_value("HOME")
            .map(PathBuf::from);
        let session = self
            .agent_shell_store()
            .get(pane_id)
            .ok_or_else(|| MezError::invalid_state("native navigation session disappeared"))?;
        let source_generation = session.native_working_directory_generation();
        let conversation = session.session_id.clone();
        let (pane_mode, default_mode) = self.agent.native_directory_mode_epoch(pane_id);
        Ok(NativeDirectoryWork {
            pane: pane_id.to_string(),
            conversation,
            operation: 0,
            root_identity,
            options,
            source_directory: context.working_directory().to_path_buf(),
            home,
            source_generation,
            config_generation: self.session.config_generation,
            source_resources: self.configured_permissions().resources.clone(),
            config_root: self.integration.config_root().map(PathBuf::from),
            pane_mode,
            default_mode,
            layers: self.integration.config_layers().to_vec(),
            trust: self.project_trust_store().cloned(),
            child: self.subagent_scope_declaration(&format!("agent-{pane_id}")),
            mode_override: self.agent_shell_mode_override(pane_id),
        })
    }

    /// Reports whether navigation owns an active command claim for this pane.
    pub(crate) fn native_directory_command_is_pending(&self, pane_id: &str) -> bool {
        self.agent.native_directory_command_is_pending(pane_id)
    }

    /// Rechecks acceptance before any worker read, rather than applying a
    /// delayed relative operand to a newer conversation or directory.
    pub(crate) fn native_directory_work_for_claim(
        &self,
        pane: &str,
        generation: u64,
    ) -> Result<NativeDirectoryWork> {
        self.ensure_native_directory_idle(pane)?;
        let source = self
            .agent
            .native_directory_work(pane, generation)
            .ok_or_else(|| MezError::conflict("native navigation acceptance is unavailable"))?;
        if !self.native_directory_source_is_current(&source)? {
            return Err(MezError::conflict(
                "native navigation source changed before preparation",
            ));
        }
        Ok(source)
    }

    /// Validates contextual ownership independently of a worker's display body.
    fn native_directory_source_is_current(&self, source: &NativeDirectoryWork) -> Result<bool> {
        let Some(session) = self.agent_shell_store().get(&source.pane) else {
            return Ok(false);
        };
        Ok(session.session_id == source.conversation
            && session.pane_id == source.pane
            && session.visibility == AgentShellVisibility::Visible
            && session.native_working_directory_generation() == source.source_generation
            && self.effective_agent_shell_mode_for_pane(&source.pane) == ShellMode::Native
            && self.session.config_generation == source.config_generation
            && self.configured_permissions().resources == source.source_resources
            && self.integration.config_root() == source.config_root.as_deref()
            && self.integration.config_layers() == source.layers
            && self.project_trust_store() == source.trust.as_ref()
            && self.subagent_scope_declaration(&format!("agent-{}", source.pane)) == source.child
            && self.agent.native_directory_mode_is_current(
                &source.pane,
                &source.pane_mode,
                &source.default_mode,
            )
            && self.native_directory_root_identity(&source.pane)? == source.root_identity
            && self
                .native_agent_shell_context_for_pane(&source.pane)?
                .working_directory()
                == source.source_directory)
    }

    /// Converts one typed preparation into the ordinary command response, with
    /// no provider request or replay. Failed/stale adoption uses existing error
    /// presentation and command-lifecycle settlement.
    pub(crate) fn native_directory_command_response(
        &mut self,
        work: &RuntimeAgentCommandAsyncWork,
        read: &NativeDirectoryRead,
    ) -> crate::runtime::RuntimeAgentCommandAsyncOutcome {
        match self.adopt_native_directory(work, read) {
            Ok(outcome) => crate::runtime::RuntimeAgentCommandAsyncOutcome::Response {
                body: super::runtime_agent_shell_command_response_json(
                    &work.pane_id,
                    &work.input,
                    Some(&outcome),
                ),
            },
            Err(error) => crate::runtime::RuntimeAgentCommandAsyncOutcome::Failed {
                message: error.message().to_string(),
                kind: error.kind(),
            },
        }
    }

    /// Adopts prepared data only under the original actor-owned command and
    /// dependency fences. Failed config application restores its prior layers;
    /// no failed/stale preparation changes cwd or claims shell synchronization.
    pub(crate) fn adopt_native_directory(
        &mut self,
        work: &RuntimeAgentCommandAsyncWork,
        read: &NativeDirectoryRead,
    ) -> Result<super::AgentShellCommandOutcome> {
        self.refresh_project_trust_store_from_disk_if_changed()?;
        self.ensure_native_directory_idle(&work.pane_id)?;
        let source = &read.source;
        let session = self
            .agent_shell_store()
            .get(&work.pane_id)
            .ok_or_else(|| MezError::conflict("native navigation session disappeared"))?;
        let current = source.pane == work.pane_id
            && source.conversation == work.conversation_id
            && source.operation == work.claim_generation
            && self.agent.agent_command_is_claimed(
                &work.pane_id,
                &work.conversation_id,
                work.claim_generation,
            )
            && self.native_directory_source_is_current(source)?;
        if !current {
            return Err(MezError::conflict(
                "native navigation source changed; the agent directory was not changed",
            ));
        }
        let target = std::fs::symlink_metadata(&read.directory)?;
        if !target.is_dir() || (target.dev(), target.ino()) != read.target_identity {
            return Err(MezError::conflict(
                "native navigation target changed; the agent directory was not changed",
            ));
        }
        // Validate the deterministic lower recorder before any config mutation.
        let mut candidate = session.clone();
        candidate
            .record_native_working_directory(read.directory.clone(), source.source_generation)?;
        let before = self.integration.config_layers().to_vec();
        let previous = crate::runtime::config::runtime_effective_config_value(&before)?;
        let affected =
            crate::runtime::RuntimeConfigAffectedSubsystems::between(&previous, &read.structured);
        *self.integration.config_layers_mut() = read.layers.clone();
        if let Err(error) = self.apply_prepared_runtime_config(
            read.effective.clone(),
            read.structured.clone(),
            affected,
        ) {
            *self.integration.config_layers_mut() = before;
            self.restore_runtime_config_layers_without_catalog_freeze()?;
            return Err(error);
        }
        // Actor settlement cannot interleave another command between config
        // projection and this I/O-free adoption. Catalog freezing may have
        // updated session fields, so commit only cwd/discovery to that session.
        if let Err(error) = self
            .agent_shell_store_mut()
            .ensure_session(&work.pane_id)?
            .record_native_working_directory_with_instructions(
                read.directory.clone(),
                source.source_generation,
                read.instructions.clone(),
            )
        {
            *self.integration.config_layers_mut() = before;
            self.restore_runtime_config_layers_without_catalog_freeze()?;
            return Err(error.into());
        }
        self.invalidate_agent_prompt_selector_extra_candidates();
        Ok(super::AgentShellCommandOutcome::Mutated {
            command: "cd".to_string(),
            body: format!("agent working directory: {}", read.directory.display()),
            visibility: AgentShellVisibility::Visible,
        })
    }
}
