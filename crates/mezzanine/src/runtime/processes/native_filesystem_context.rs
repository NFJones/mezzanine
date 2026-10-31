//! Shell-independent source binding for native filesystem work.
//!
//! The actor freezes pane/conversation/navigation/mode/root and policy inputs.
//! Blocking preparation captures physical directory and credential witnesses;
//! those witnesses never select an executable or infer a shell environment.
//! Source equality is not permission or worker admission: the eventual dispatch
//! owner must also validate its exact action/attempt and current grants.

use std::path::PathBuf;
use std::sync::Arc;

use mez_agent::permissions::PathScopes;
use mez_agent::{AgentNativeWorkingDirectory, AgentTurnRecord, SubagentScopeDeclaration};
use mez_mux::process::{current_working_directory_for_pid, process_start_token_for_pid};

use crate::error::{MezError, Result};
use crate::runtime::RuntimeSessionService;
use crate::runtime::config::{ResourceAuthorityConfig, ShellMode};
use crate::security::filesystem::capability::NativeFilesystemRoot;

/// Immutable actor source data, deliberately without executable/environment data.
#[derive(Clone)]
#[allow(
    dead_code,
    reason = "source binding is being wired into native patch dispatch"
)]
pub(crate) struct NativeFilesystemContext {
    /// Exact issuing turn, independent of presentation-pane conversation IDs.
    turn: String,
    /// Exact logical owner of the issuing turn.
    agent: String,
    /// Original turn conversation, which routed work may retain separately.
    conversation: String,
    /// Presentation/root pane used by this native execution owner.
    pane: String,
    /// Exact agent-shell incarnation currently attached to that pane.
    session: String,
    /// Kernel root lifetime, never a PID alone.
    root: (u32, u64),
    /// Observed process cwd; navigation never changes this value.
    process_directory: PathBuf,
    /// Selected operation cwd, including current owned `/cd` intent.
    operation_directory: PathBuf,
    /// Navigation generation prevents directory A/B/A ownership refresh.
    navigation: u64,
    /// Exact selected directory owner, including qualified same-pane loop data.
    selected_directory: Option<AgentNativeWorkingDirectory>,
    /// Pane-local mode allocation prevents Native/Pane/Native refresh.
    pane_mode: Arc<()>,
    /// Default-mode allocation is independent of local overrides.
    default_mode: Arc<()>,
    /// Applied configuration identity at source capture.
    config_generation: u64,
    /// Captured configured resource provenance, not a computed grant.
    resources: ResourceAuthorityConfig,
    /// Captured inherited child restriction; replacement cannot widen it.
    child: Option<SubagentScopeDeclaration>,
}

impl NativeFilesystemContext {
    /// Captures physical process/operation cwd witnesses on a blocking worker.
    /// This does not authorize file access or publish the prepared root.
    #[allow(
        dead_code,
        reason = "blocking dispatch preparation is the next integration step"
    )]
    pub(crate) fn capture_root(&self) -> Result<NativeFilesystemRoot> {
        let root =
            NativeFilesystemRoot::capture_at(self.root.0, self.root.1, &self.operation_directory)?;
        if root.process_cwd() != self.process_directory || root.cwd() != self.operation_directory {
            return Err(MezError::conflict(
                "native filesystem source directories changed during preparation",
            ));
        }
        Ok(root)
    }
}

impl RuntimeSessionService {
    /// Captures current native source ownership without shell or environment
    /// discovery. This source token is data, not action or mutation admission.
    #[allow(
        dead_code,
        reason = "source binding is being wired into native patch dispatch"
    )]
    pub(crate) fn capture_native_filesystem_context(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<NativeFilesystemContext> {
        if self.effective_agent_shell_mode_for_pane(&turn.pane_id) != ShellMode::Native {
            return Err(MezError::conflict(
                "native filesystem source is not in native mode",
            ));
        }
        let root = self.native_filesystem_root_source(&turn.pane_id)?;
        let process_directory = current_working_directory_for_pid(root.0)
            .ok_or_else(|| MezError::invalid_state("native filesystem source cwd unavailable"))?;
        let selected_directory = self.native_agent_directory_snapshot(&turn.pane_id).cloned();
        let operation_directory = selected_directory.as_ref().map_or_else(
            || process_directory.clone(),
            |directory| directory.directory().to_path_buf(),
        );
        let session = self.agent_shell_store().get(&turn.pane_id).ok_or_else(|| {
            MezError::invalid_state("native filesystem source session unavailable")
        })?;
        let session_id = session.session_id.clone();
        let navigation = session.native_working_directory_generation();
        let (pane_mode, default_mode) = self.agent.native_directory_mode_epoch(&turn.pane_id);
        if self.native_filesystem_root_source(&turn.pane_id)? != root {
            return Err(MezError::conflict(
                "native filesystem root changed during source capture",
            ));
        }
        Ok(NativeFilesystemContext {
            turn: turn.turn_id.clone(),
            agent: turn.agent_id.clone(),
            conversation: turn.conversation_id.clone(),
            pane: turn.pane_id.clone(),
            session: session_id,
            root,
            process_directory,
            operation_directory,
            navigation,
            selected_directory,
            pane_mode,
            default_mode,
            config_generation: self.session.config_generation,
            resources: self.configured_permissions().resources.clone(),
            child: self.subagent_scope_declaration_for_turn(turn),
        })
    }

    /// Checks exact source identity without rebasing a queued operation. Callers
    /// must separately validate live action/attempt custody and resolve current
    /// grants before using a prepared filesystem root.
    #[allow(
        dead_code,
        reason = "source binding is being wired into native patch dispatch"
    )]
    pub(crate) fn native_filesystem_context_is_current(
        &self,
        turn: &AgentTurnRecord,
        source: &NativeFilesystemContext,
    ) -> Result<bool> {
        let Some(session) = self.agent_shell_store().get(&source.pane) else {
            return Ok(false);
        };
        if turn.turn_id != source.turn
            || turn.agent_id != source.agent
            || turn.conversation_id != source.conversation
            || turn.pane_id != source.pane
            || session.session_id != source.session
            || session.native_working_directory_generation() != source.navigation
            || self.native_agent_directory_snapshot(&source.pane)
                != source.selected_directory.as_ref()
            || self.effective_agent_shell_mode_for_pane(&source.pane) != ShellMode::Native
            || !self.agent.native_directory_mode_is_current(
                &source.pane,
                &source.pane_mode,
                &source.default_mode,
            )
            || self.session.config_generation != source.config_generation
            || self.configured_permissions().resources != source.resources
            || self.subagent_scope_declaration_for_turn(turn) != source.child
            || self.native_filesystem_root_source(&source.pane)? != source.root
        {
            return Ok(false);
        }
        let Some(process_directory) = current_working_directory_for_pid(source.root.0) else {
            return Ok(false);
        };
        let operation_directory = self
            .native_agent_working_directory(&source.pane)
            .unwrap_or(&process_directory);
        Ok(process_directory == source.process_directory
            && operation_directory == source.operation_directory)
    }

    /// Resolves current grants only for unchanged source data. Exact action and
    /// commit-lease admission remain with the native dispatch owner; this helper
    /// cannot turn a source token or prepared root into mutation authority.
    #[allow(
        dead_code,
        reason = "source-bound authorization is being wired into native patch dispatch"
    )]
    pub(crate) fn native_filesystem_scopes_for_context(
        &mut self,
        turn: &AgentTurnRecord,
        source: &NativeFilesystemContext,
    ) -> Result<Option<PathScopes>> {
        if !self.native_filesystem_context_is_current(turn, source)? {
            return Err(MezError::conflict(
                "native filesystem source changed before authorization",
            ));
        }
        let scopes = self.native_filesystem_scopes_for_turn(turn, &source.operation_directory)?;
        if !self.native_filesystem_context_is_current(turn, source)? {
            return Err(MezError::conflict(
                "native filesystem source changed during authorization",
            ));
        }
        Ok(scopes)
    }

    /// Reads the live owned pane root lifetime without executable inference.
    fn native_filesystem_root_source(&self, pane: &str) -> Result<(u32, u64)> {
        let pid = self
            .primary_pid_for_live_pane_process(pane)
            .ok_or_else(|| MezError::invalid_state("native filesystem pane root unavailable"))?;
        let token = process_start_token_for_pid(pid).ok_or_else(|| {
            MezError::invalid_state("native filesystem pane root lifetime unavailable")
        })?;
        Ok((pid, token))
    }
}
