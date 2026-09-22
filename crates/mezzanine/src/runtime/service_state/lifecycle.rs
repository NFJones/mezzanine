//! Runtime lifecycle, registry-update, and snapshot work contracts.

use super::{Result, SessionRecord};

/// One retained `apply_patch` attempt emitted by the current pane agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeAgentPatchRecord {
    /// Turn id that contained the patch action.
    pub turn_id: String,
    /// Action id assigned by the model to the patch action.
    pub action_id: String,
    /// Lowercase action status observed by the runtime.
    pub status: String,
    /// Patch body exactly as emitted in the MAAP action payload.
    pub patch: String,
    /// Optional `strip` value supplied with the patch payload.
    pub strip: Option<u64>,
    /// Optional structured error code recorded for a failed patch.
    pub error_code: Option<String>,
    /// Optional human-readable error or patch diagnostic for a failed patch.
    pub error_message: Option<String>,
}

/// Carries Runtime Lifecycle State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeLifecycleState {
    /// Represents the Running case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Running,
    /// Represents the Detached case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Detached,
    /// Represents the Stopping case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Stopping,
    /// Represents the Killed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Killed,
    /// Represents the Failed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Failed,
}

/// Carries Runtime Registry Update Plan state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeRegistryUpdatePlan {
    /// Represents the Upsert case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Upsert(SessionRecord),
    /// Represents the Remove case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Remove {
        /// Stores the session id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        session_id: String,
    },
}

/// Owned snapshot creation context captured by the actor before repository I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeSnapshotOwnedCreationContext {
    /// Live pane terminal/process captures.
    pub pane_captures: Vec<crate::storage::snapshot::SnapshotPaneCapture>,
    /// Exact primary whose caller-local navigation becomes snapshot landing focus.
    pub navigation_source_client_id: Option<mez_core::ids::ClientId>,
    /// Active config layers at capture time.
    pub active_config_layers: Vec<crate::storage::snapshot::SnapshotConfigLayerMetadata>,
    /// Live terminal frame state at capture time.
    pub frame_state: crate::storage::snapshot::SnapshotFrameState,
    /// Agent sessions to include in the snapshot payload.
    pub agent_sessions: Vec<crate::storage::snapshot::SnapshotAgentSession>,
    /// Approval grants to include in the snapshot payload.
    pub approval_grants: Vec<crate::storage::snapshot::SnapshotApprovalGrantMetadata>,
    /// Approval requests to include in the snapshot payload.
    pub approval_requests: Vec<crate::storage::snapshot::SnapshotApprovalRequestMetadata>,
    /// Message-service state to include in the snapshot payload.
    pub message_state: mez_agent::messaging::MessageServiceSnapshot,
    /// Receiver-scoped presentation sources awaiting durable settlement.
    pub unsettled_peer_presentations:
        Vec<crate::storage::snapshot::SnapshotUnsettledPeerPresentation>,
    /// MCP server state to include in the snapshot payload.
    pub mcp_servers: Vec<crate::storage::snapshot::SnapshotMcpServerState>,
}

impl RuntimeSnapshotOwnedCreationContext {
    /// Borrows the owned context as the snapshot repository creation context.
    pub(crate) fn as_creation_context(
        &self,
    ) -> crate::storage::snapshot::SnapshotCreationContext<'_> {
        let context = crate::storage::snapshot::SnapshotCreationContext::new(
            &self.pane_captures,
            &self.active_config_layers,
            &self.frame_state,
            &self.agent_sessions,
        );
        let context = self
            .navigation_source_client_id
            .as_ref()
            .map_or(context, |client_id| {
                context.with_navigation_source(client_id)
            });
        context
            .with_approvals(&self.approval_grants, &self.approval_requests)
            .with_message_state(&self.message_state)
            .with_unsettled_peer_presentations(&self.unsettled_peer_presentations)
            .with_mcp_servers(&self.mcp_servers)
    }
}

/// Control operation that can perform blocking or repository work off the actor.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeSnapshotControlAsyncWork {
    /// Parsed JSON-RPC request.
    pub request: crate::control::JsonRpcRequest,
    /// Client authorized to make the request.
    pub caller_client_id: mez_core::ids::ClientId,
    /// Operation-specific repository work.
    pub kind: RuntimeSnapshotControlAsyncWorkKind,
}

/// Repository work shape for actor-deferred snapshot control operations.
#[derive(Debug, Clone)]
pub(crate) enum RuntimeSnapshotControlAsyncWorkKind {
    /// Configuration reload whose disk layers are prepared off actor ownership.
    ConfigReload {
        /// Configuration generation captured before preparation began.
        config_generation: u64,
        /// Active layers whose path-backed contents must be refreshed.
        layers: Vec<crate::config::ConfigLayer>,
        /// Test-only notification sent when asynchronous preparation starts.
        #[cfg(test)]
        preparation_started: Option<std::sync::Arc<tokio::sync::Notify>>,
        /// Test-only gate that keeps preparation observably in flight.
        #[cfg(test)]
        preparation_release: Option<std::sync::Arc<tokio::sync::Notify>>,
    },
    /// Snapshot list/create/delete or plan-only resume dispatch.
    Dispatch {
        /// Session snapshot captured before the repository operation.
        session: Box<mez_mux::session::Session>,
        /// Owned snapshot context captured before the repository operation.
        context: Box<RuntimeSnapshotOwnedCreationContext>,
    },
    /// Live snapshot resume that must return payload metadata for actor apply.
    Resume {
        /// Shell to seed restored panes with.
        shell: crate::host::shell::ResolvedShell,
    },
}

/// One deferred runtime slash command handed to a worker.
///
/// The actor owns prompt, overlay, and presentation state; the worker prepares
/// an owned outcome value from this work item and never reaches into live
/// service state, mirroring the snapshot control and provider persistence work.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeAgentCommandAsyncWork {
    /// Pane whose agent shell prompt submitted the command.
    pub pane_id: String,
    /// Durable conversation that owned the pane when the command was accepted.
    pub conversation_id: String,
    /// Canonical command name the disposition classifier deferred.
    pub command: String,
    /// Full prompt input including the command name and arguments.
    pub input: String,
    /// Actor-owned claim generation compared when the outcome settles.
    pub claim_generation: u64,
    /// Theme captured when the command claim was accepted for off-actor display projection.
    pub display_theme: mez_mux::theme::UiTheme,
    /// Terminal width captured when the command claim was accepted for off-actor display projection.
    pub display_width: usize,
    /// Agent display wrap cap captured with the terminal width for off-actor projection.
    pub display_wrap_column_cap: usize,
    /// Test-only gate used to hold one claimed worker outside actor ownership.
    #[cfg(test)]
    pub deferred_agent_command_started: Option<std::sync::Arc<tokio::sync::Notify>>,
    /// Test-only gate release paired with `deferred_agent_command_started`.
    #[cfg(test)]
    pub deferred_agent_command_release: Option<std::sync::Arc<tokio::sync::Notify>>,
    /// Owned inputs the deferred execution reads instead of live actor state.
    pub prepared: RuntimeAgentCommandPrepared,
}

/// Owned inputs one deferred slash command may read off the serialized actor.
///
/// The actor captures these while it still owns the pane, so the worker never
/// reaches into live service state. Every family that moves off the actor names
/// exactly what it reads here, which keeps the off-actor surface reviewable and
/// makes an accidental dependency on actor state a compile error.
#[derive(Debug, Clone)]
pub(crate) enum RuntimeAgentCommandPrepared {
    /// Renders one skill or macro catalog from the captured roots.
    ///
    /// `/list-skills` and `/list-macros` walk the configured user catalog plus
    /// the pane's trusted project catalog, which is the filesystem work the
    /// inline path used to perform inside the actor request.
    Catalog {
        /// Configured Mezzanine config root whose catalogs are discovered.
        config_root: Option<std::path::PathBuf>,
        /// Trusted project root whose project-scoped catalogs may apply.
        project_root: Option<std::path::PathBuf>,
    },
    /// Reads provider credential status from the captured store.
    ///
    /// `/auth-status` reads credential metadata and credential-store state for
    /// every configured provider, which is the filesystem work the inline path
    /// used to perform inside the actor request.
    AuthStatus {
        /// Configured provider keys in registry order.
        providers: Vec<String>,
        /// Credential store handle, or `None` when no store is configured.
        auth_store: Option<crate::security::auth::AuthStore>,
    },
    /// Reads issue records from the captured local issue store.
    ///
    /// `/issue show` and `/issue query` open the project issue database, which is
    /// the synchronous SQLite work the inline path used to perform inside the
    /// actor request; the mutating sub-commands stay inline because they also
    /// invalidate prompt selector candidates on the actor.
    IssueStore {
        /// Resolved issue database location captured from live config.
        database_path: crate::storage::issues::IssueDatabasePath,
        /// Project key the read is scoped to.
        project: String,
    },
    /// Builds the local issue browser from the captured store read.
    ///
    /// `/show-issues` queries the issue database and renders a record browser; the
    /// worker does both, and the actor installs the browser overlay when the
    /// outcome settles. The `--save` form keeps the inline path because it also
    /// writes a page file.
    IssueBrowser {
        /// Resolved issue database location captured from live config.
        database_path: crate::storage::issues::IssueDatabasePath,
        /// Project key the browser is scoped to by default.
        project: String,
    },
    /// Builds the persistent-memory browser from the captured store read.
    ///
    /// `/show-memories` searches the persistent-memory store and renders a record
    /// browser; the worker does both, and the actor installs the browser overlay
    /// when the outcome settles. The `--save` form keeps the inline path because it
    /// also writes a page file.
    MemoryBrowser {
        /// Configured Mezzanine config root whose memory store is read.
        config_root: std::path::PathBuf,
        /// Pane's effective remember scope when the invocation does not name one.
        pane_scope: mez_agent::memory::MemoryScope,
    },
    /// Reads one pane's transcript into its context browser.
    ///
    /// `/show-context` builds its pager from the pane-filtered transcript, which
    /// is the same read the overlay refresh lane performs after every context
    /// keystroke; the actor keeps only the pane-session lookup that names the
    /// conversation the browser reads.
    ContextBrowser {
        /// Transcript store the pane's conversation lives in.
        store: crate::storage::transcript::AgentTranscriptStore,
        /// Conversation displayed by the browser.
        conversation_id: String,
        /// Pane whose entries are displayed.
        pane_id: String,
    },
    /// Reads context documents from the captured store.
    ///
    /// `/context-doc list` and `/context-doc show` read the context-document
    /// store, which is the synchronous read the inline path used to perform inside
    /// the actor request; the mutating sub-commands stay inline because they write
    /// rows or start an external editor on the actor.
    ContextDocument {
        /// Configured Mezzanine config root whose context store is read.
        config_root: std::path::PathBuf,
        /// Project key the read is scoped to.
        project: String,
    },
    /// Syncs managed built-in skill copies from the captured config root.
    ///
    /// `/sync-builtin-skills` writes managed skill copies under the config root,
    /// which is the filesystem work the inline path used to perform inside the
    /// actor request.
    BuiltinSkillSync {
        /// Configured Mezzanine config root whose skill copies are synced.
        config_root: std::path::PathBuf,
    },
    /// Reads one bounded saved-session catalog page for the `/resume` picker.
    ///
    /// Bare `/resume` reads the saved-session catalog, whose shared `flock` read
    /// is the blocking work that parks the serialized actor; the
    /// conversation-selecting argument forms stay inline.
    SavedSessionsBrowser {
        /// Transcript store handle whose saved-session catalog is queried.
        store: crate::storage::transcript::AgentTranscriptStore,
        /// Pane working directory captured as the picker's directory filter.
        directory: Option<String>,
        /// Maximum catalog rows retained by the picker page.
        limit: usize,
        /// Prompt column budget captured from the pane viewport.
        prompt_width: usize,
        /// Session-title policy captured from live config.
        title_policy: crate::session_title::SessionTitlePolicy,
    },
    /// Reads one selected saved conversation and its durable presentation log.
    ///
    /// The worker returns only immutable durable state. The actor performs the
    /// conversation bind, authority restoration, and rollback transaction.
    DirectResume {
        /// Transcript store containing the selected durable conversation.
        store: crate::storage::transcript::AgentTranscriptStore,
        /// Exact direct-resume selector, including the supported latest aliases.
        selector: String,
        /// Immutable renderer inputs captured before the blocking worker starts.
        projection: Box<RuntimeDirectResumeProjectionWork>,
    },
    /// Renders the pane's tracked modified-file summary.
    ///
    /// `/list-modified-files` formats the pane's retained modification map; the
    /// claim captures a copy so the worker can build the page without touching
    /// actor-owned pane state.
    ModifiedFiles {
        /// Tracked modification summaries for the pane, when any were recorded.
        #[allow(clippy::type_complexity)]
        files: Option<
            std::collections::BTreeMap<
                String,
                crate::runtime::service_state::RuntimeAgentModifiedFileSummary,
            >,
        >,
    },
    /// Reads the pending approval queue for the `/show-approvals` browser.
    ///
    /// The queue is actor-owned, so the claim captures a copy and the worker
    /// builds the browser; an unknown requested id refuses in the worker with the
    /// same not-found error the inline lane produced.
    ApprovalsBrowser {
        /// Pending approval requests captured from the live queue.
        approvals: Vec<mez_agent::permissions::BlockedApprovalRequest>,
    },
    /// Reads one pane's configured personality table.
    ///
    /// The profile map and the pane's effective selection are actor-owned config
    /// state, so the claim captures both and the worker renders the table through
    /// the shared builder; the overlay refresh stays on the actor because it only
    /// re-reads that same in-memory state.
    PersonalitiesBrowser {
        /// Configured profiles in registry order.
        profiles: Vec<(
            String,
            crate::runtime::service_state::RuntimeAgentPersonalityProfile,
        )>,
        /// Effective selection for the pane, when one resolves.
        selected: Option<String>,
        /// Pane-scoped selection before default fallback, when one applies.
        pane_selection: Option<String>,
    },
}

/// Immutable actor snapshot used to project a direct resume outside the actor.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeDirectResumeProjectionWork {
    /// Pane that owns the resumed conversation.
    pub(crate) pane_id: String,
    /// Exact terminal geometry represented by the candidate screen.
    pub(crate) size: crate::runtime::Size,
    /// Clone of the mux layout used by the canonical presentation renderer.
    pub(crate) session: mez_mux::session::Session,
    /// Runtime socket identity needed to construct an isolated renderer.
    pub(crate) socket_path: std::path::PathBuf,
    /// Runtime creation timestamp retained by the isolated renderer.
    pub(crate) created_at_unix_seconds: u64,
    /// Immutable presentation settings captured with the command claim.
    pub(crate) presentation_settings: crate::runtime::RuntimePresentationSettings,
    /// Maximum history retained by the projected terminal screen.
    pub(crate) history_limit: usize,
    /// History rotation batch retained by the projected terminal screen.
    pub(crate) history_rotate_lines: usize,
    /// Current pane session used as the target session template.
    pub(crate) agent_session: mez_agent::AgentShellSession,
    /// Owning runtime session whose checkpoint records must be read off actor.
    pub(crate) mezzanine_session_id: String,
    /// Pane directory captured before worker preparation resolves fallback scope.
    pub(crate) working_directory: Option<std::path::PathBuf>,
    /// Actor-local presentation source revision that authorizes a selected-id candidate.
    ///
    /// Latest-session selection resolves only on the worker, so it relies on
    /// the worker-side durable source recheck instead.
    pub(crate) presentation_source_revision: Option<u64>,
}

/// Fully rendered direct-resume terminal candidate built on the command worker.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeDirectResumeProjection {
    /// Candidate screen ready for one actor-owned state replacement.
    pub(crate) screen: mez_terminal::TerminalScreen,
    /// Prompt history loaded from the resumed conversation.
    pub(crate) prompt_history: Vec<mez_mux::readline::ReadlineHistoryEntry>,
    /// Geometry captured before the worker rendered the candidate.
    pub(crate) size: crate::runtime::Size,
    /// Presentation settings that determined the candidate screen.
    pub(crate) presentation_settings: crate::runtime::RuntimePresentationSettings,
    /// History limit that determined the candidate screen.
    pub(crate) history_limit: usize,
    /// History rotation batch that determined the candidate screen.
    pub(crate) history_rotate_lines: usize,
    /// Actor-local presentation source revision captured before worker projection.
    pub(crate) presentation_source_revision: Option<u64>,
}

/// Immutable durable inputs prepared before a direct saved-session resume.
///
/// Store reads and presentation-log decompression may be expensive for large
/// sessions, while binding and rollback remain actor-owned. The command worker
/// creates this bundle without accessing live runtime state.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeDirectResumeRead {
    /// Conversation selected by the direct resume argument.
    pub(crate) conversation_id: String,
    /// Saved-session catalog and summary state used by the actor commit.
    pub(crate) saved: crate::storage::transcript::SavedAgentSession,
    /// Durable child lineage required before restoring a subagent session.
    pub(crate) subagent_lineage: Option<mez_agent::SubagentSessionLineage>,
    /// Bounded transcript fallback used only when no presentation log exists.
    pub(crate) entries: Vec<mez_agent::transcript::TranscriptEntry>,
    /// Complete durable presentation log retained only by the inline fallback.
    ///
    /// Deferred worker projection consumes and drops these rows before actor
    /// settlement, so large decoded logs never tear down on the actor.
    pub(crate) presentation_entries:
        Option<Vec<crate::storage::transcript::AgentPresentationEntry>>,
    /// Worker-held durable presentation lock retained through actor installation.
    ///
    /// Only deferred projections retain this lease; inline resume remains on
    /// its historical synchronous path.
    pub(crate) presentation_lock: Option<std::sync::Arc<std::fs::File>>,
    /// Durable effective objective restored during actor-owned commit.
    pub(crate) prepared_objective: Option<String>,
    /// Durable model identity restored during actor-owned commit.
    pub(crate) restored_model_identity: Option<(
        String,
        Option<crate::storage::transcript::AgentModelProfileSelection>,
    )>,
    /// Complete current-session metadata retained for rollback without an actor read.
    pub(crate) previous_checkpoint_records: Vec<mez_agent::transcript::AgentSessionMetadata>,
    /// Selected persisted pane metadata, including its durable action catalog.
    pub(crate) resume_metadata: Option<mez_agent::transcript::AgentSessionMetadata>,
    /// Durable action catalog loaded independently of checkpoint metadata.
    pub(crate) allowed_actions: Option<mez_agent::AllowedActionSet>,
    /// Trusted persisted or fallback project membership resolved on the worker.
    pub(crate) prepared_project_scope: Option<mez_agent::messaging::ProjectMembership>,
    /// Whether the saved resume directory existed when the worker prepared it.
    pub(crate) resume_directory_available: bool,
    /// Worker-built terminal and prompt-history projection when deferred.
    pub(crate) projection: Option<RuntimeDirectResumeProjection>,
}

/// Result a worker prepares for the actor to apply.
///
/// Presentation stays byte-identical to the inline path: the actor turns the
/// response body into the same command response it returns today, and reports a
/// failure through the same invalid-command response path.
#[derive(Debug)]
pub(crate) enum RuntimeAgentCommandAsyncOutcome {
    /// Command response body the actor returns to the caller.
    Response {
        /// Body produced by the deferred execution.
        body: String,
    },
    /// Deferred execution could not prepare a response.
    Failed {
        /// Diagnostic reported through the invalid-command response path.
        message: String,
        /// Error kind the inline lane would have reported for this failure.
        ///
        /// Carrying the kind keeps the deferred body's `agent command error: ..
        /// (code)` suffix byte-identical to the inline lane; without it every
        /// deferred failure would render a normalized `invalid_state`.
        kind: crate::error::MezErrorKind,
    },
    /// Deferred execution produced a record browser the actor installs.
    ///
    /// The inline lane registered the overlay and returned the page body in one
    /// actor request; the deferred lane hands the browser back so the completion
    /// can install it with the same call before applying the body.
    RecordBrowser {
        /// Response body the actor applies exactly like the inline lane's.
        body: String,
        /// Command that owns the overlay registration key.
        command: String,
        /// Browser the worker rendered from the captured store read.
        browser: Box<mez_mux::record_browser::RecordBrowser>,
        /// Overlay source retained for refreshes and scope indicators.
        source: Option<super::RuntimeRecordBrowserOverlaySource>,
    },
    /// Durable direct-resume state prepared outside actor ownership.
    DirectResume {
        /// Store retained for actor-owned metadata rollback after a failed commit.
        store: crate::storage::transcript::AgentTranscriptStore,
        /// Immutable durable session, transcript, and presentation data.
        read: Box<RuntimeDirectResumeRead>,
    },
    /// Immutable command display projection built outside actor ownership.
    Projected {
        /// Preparsed and width-resolved display content ready for actor-owned installation.
        display_output: crate::runtime::RuntimeAgentShellDisplayOutput,
        /// Whether the resolved response removes the pane prompt.
        hide_prompt: bool,
        /// Whether the command settles through its ordinary failure lifecycle.
        failed: bool,
        /// Optional record browser that must be registered before overlay installation.
        record_browser: Option<(
            String,
            Box<mez_mux::record_browser::RecordBrowser>,
            Option<super::RuntimeRecordBrowserOverlaySource>,
        )>,
    },
}

/// One deferred slash command the actor queued for off-actor execution.
///
/// The prompt-submission path cannot emit side effects directly, so it records
/// the dispatch here and the actor's step handler drains it into a
/// [`crate::runtime::RuntimeSideEffect::DispatchAgentCommand`] in the same place
/// it drains interactive provider refreshes.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeAgentCommandDispatch {
    /// Primary client that submitted the command.
    pub primary_client_id: mez_core::ids::ClientId,
    /// Pane whose agent shell prompt submitted the command.
    pub pane_id: String,
    /// Durable conversation that owns the command across client reattachment.
    pub conversation_id: String,
    /// Canonical command name the disposition classifier deferred.
    pub command: String,
    /// Full prompt input including the command name and arguments.
    pub input: String,
    /// Actor-owned claim generation used to drop stale outcomes.
    pub claim_generation: u64,
}

/// Actor-owned phase for one accepted deferred command transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeAgentCommandLifecyclePhase {
    /// Accepted by the pane and waiting for a worker claim.
    Queued,
    /// Claimed by one worker and waiting for exactly one settlement.
    Claimed,
    /// Applied successfully to the owning pane conversation.
    Completed,
    /// Failed before execution, during execution, or while applying the result.
    Failed,
    /// Retired because its pane, conversation, or runtime owner ended.
    Cancelled,
}

impl RuntimeAgentCommandLifecyclePhase {
    /// Reports whether this phase permits a later command to replace the record.
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Identity and phase retained while one pane owns a deferred command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeAgentCommandLifecycle {
    /// Unique pane-local command identity.
    pub command_id: u64,
    /// Durable conversation that owns the result across client reattachment.
    pub conversation_id: String,
    /// Current non-terminal transaction phase.
    pub phase: RuntimeAgentCommandLifecyclePhase,
}

/// One deferred record-browser refresh handed to a worker.
///
/// The actor owns overlay and presentation state; the worker rebuilds one page
/// from owned inputs and never reaches into live service state, the same split
/// the deferred slash-command lane uses.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeRecordBrowserRefreshWork {
    /// Overlay refresh key: the pane whose overlay is refreshed, or the
    /// conversation whose row changed for the shared saved-session picker.
    pub refresh_key: String,
    /// Actor-owned refresh generation compared when the outcome settles.
    pub generation: u64,
    /// Overlay source the page is rebuilt from.
    pub source: super::RuntimeRecordBrowserOverlaySource,
    /// Overlay source the claim observed, compared when the outcome settles.
    ///
    /// A filter change rebuilds from a source the overlay does not show yet, so
    /// the token that decides staleness stays separate from the rebuild target.
    pub active_source: super::RuntimeRecordBrowserOverlaySource,
    /// What the worker rebuilds for this claim.
    pub intent: RuntimeRecordBrowserRefreshIntent,
    /// Transcript store the saved-session page reads.
    pub transcript_store: Option<crate::storage::transcript::AgentTranscriptStore>,
    /// Config root the store-backed families resolve their stores from.
    pub config_root: Option<std::path::PathBuf>,
    /// Issue database resolved when the claim was made, when the source is an
    /// issue browser; the resolver reads live config, so the worker cannot.
    pub issue_database_path: Option<crate::storage::issues::IssueDatabasePath>,
    /// Prompt column budget each rebuilt row may use.
    pub prompt_width: usize,
    /// Title policy captured from live configuration.
    pub title_policy: crate::session_title::SessionTitlePolicy,
}

/// What one deferred refresh rebuilds for its key.
#[derive(Debug, Clone)]
pub(crate) enum RuntimeRecordBrowserRefreshIntent {
    /// Rebuild the current page in place, restoring the focused row when one was
    /// focused.
    RefreshInPlace {
        /// Focused record the rebuilt page restores, when one was focused.
        active_record_id: Option<String>,
    },
    /// Fetch the adjacent page after cursor movement crossed a page edge.
    ///
    /// The page identity is captured in memory when the claim is made; the cursor
    /// lookup and the page rebuild both happen in the worker.
    FetchAdjacent {
        /// Signed cursor movement: positive pages forward, negative backward.
        delta: isize,
        /// First record id on the page the cursor moved off.
        first_id: String,
        /// Last record id on the page the cursor moved off.
        last_id: String,
        /// Whether the operator paged from inside a record detail view.
        replaces_detail: bool,
    },
    /// Rebuild the page around the focused row after an in-memory filter change:
    /// a toggled scope, subagent visibility, lifecycle, or search text.
    ApplyFilter {
        /// Source the rebuild targets, derived from the page the operator saw.
        target: Box<super::RuntimeRecordBrowserOverlaySource>,
        /// Focused record the rebuilt page restores, when one was focused.
        active_record_id: Option<String>,
        /// Row index the rebuilt page keeps for the families that have no anchors
        /// to restore by, used when the focused record left the page.
        active_index: Option<usize>,
        /// Whether the operator pressed the key from inside a record detail view.
        ///
        /// The settle still drops when a detail was opened after the claim; this
        /// records that the key itself meant to leave the detail behind.
        replaces_detail: bool,
        /// Settlement error the rebuilt page displays, when one applies.
        error: Option<String>,
    },
    /// Rebuild the page one saved-session delete left behind.
    RefreshAfterDelete {
        /// Row index the rebuilt page keeps: the deleted row held the focus and
        /// its id is gone, so the page restores the raw index instead.
        active_index: usize,
    },
}

/// Result a refresh worker prepares for the actor to install or drop.
#[derive(Debug)]
pub(crate) enum RuntimeRecordBrowserRefreshOutcome {
    /// Rebuilt page ready to install when the overlay is still current.
    Rebuilt {
        /// Browser the worker rendered from the captured store read.
        browser: Box<mez_mux::record_browser::RecordBrowser>,
        /// Source the rebuilt page belongs to, including any resolved anchor.
        source: super::RuntimeRecordBrowserOverlaySource,
        /// Selection the rebuilt page installs, when the intent named one.
        active_index: Option<usize>,
    },
    /// Rebuild failed; the actor keeps the current page.
    Failed {
        /// Diagnostic marked on the current page while it still owns the claim.
        message: String,
        /// Error kind the inline refresh would have reported.
        kind: crate::error::MezErrorKind,
    },
}

/// One deferred refresh the actor queued for off-actor execution.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeRecordBrowserRefreshDispatch {
    /// Overlay refresh key: the pane whose overlay is refreshed, or the
    /// conversation whose row changed for the shared saved-session picker.
    pub refresh_key: String,
    /// Actor-owned refresh generation used to drop stale outcomes.
    pub generation: u64,
}

/// Repository result returned to the actor after async snapshot control work.
#[derive(Debug)]
pub(crate) enum RuntimeSnapshotControlAsyncOutcome {
    /// Prepared and validated configuration reload candidate.
    ConfigReload(Result<RuntimePreparedConfigReload>),
    /// JSON result body produced by the snapshot dispatcher.
    Dispatch(Result<String>),
    /// Live resume payload plus restored session state.
    Resume(
        Box<
            Result<(
                crate::storage::snapshot::SessionSnapshotPayload,
                crate::storage::snapshot::SnapshotRestoreResult,
            )>,
        >,
    ),
}

/// Immutable configuration reload candidate prepared outside actor ownership.
#[derive(Debug)]
pub(crate) struct RuntimePreparedConfigReload {
    /// Refreshed and validated layers to install atomically.
    pub layers: Vec<crate::config::ConfigLayer>,
    /// Runtime subsystem families whose effective configuration changed.
    pub affected: super::RuntimeConfigAffectedSubsystems,
    /// Composed configuration whose layer diagnostics were resolved off actor ownership.
    pub effective: crate::config::EffectiveConfig,
    /// Structured configuration prepared for runtime subsystem projection.
    pub structured: serde_json::Value,
    /// JSON result payload produced from the refreshed layers.
    pub result: String,
}
