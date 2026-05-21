//! Typed event and side-effect model for the async runtime migration.
//!
//! The current live runtime still exposes compatibility request methods that
//! call into `RuntimeSessionService` synchronously. This module defines the
//! event vocabulary that the Tokio-native runtime will use as those callers are
//! migrated. Keeping these types separate from the compatibility request enum
//! prevents the async refactor from growing another ad hoc command channel and
//! gives tests a stable shape for event ordering before production I/O is moved.

use crate::agent::{AgentTurnExecution, ModelResponse};
use crate::audit::AuditRetentionPolicy;
use crate::hooks::{HookExecutionPlan, HookExecutionResult};
use crate::ids::{AgentId, ClientId};
use crate::layout::Size;
use crate::registry::SessionRegistry;
use crate::runtime::RuntimeRegistryUpdatePlan;
use crate::terminal::{AttachedTerminalOutputModes, TerminalStyleSpan};
use crate::transcript::{AgentTranscriptStore, TranscriptEntry};
use std::path::PathBuf;

/// Source of a runtime event entering the single-owner session actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeEvent {
    /// An attached client produced input, resized, disconnected, or changed
    /// render readiness.
    Client(ClientEvent),
    /// A pane PTY produced output or completed a pane-side I/O operation.
    Pane(PaneEvent),
    /// A process lifecycle observer reported spawn, exit, or failure state.
    Process(ProcessEvent),
    /// An agent provider task completed or failed outside the runtime actor.
    AgentProvider(AgentProviderEvent),
    /// A model-backed conversation compaction completed or failed outside the
    /// runtime actor.
    AgentCompaction(AgentCompactionEvent),
    /// A hook worker task completed or failed outside the runtime actor.
    Hook(AsyncHookEvent),
    /// A persistence worker completed or failed a write outside the runtime actor.
    Persistence(PersistenceEvent),
    /// A runtime-owned timer fired.
    Timer(TimerEvent),
    /// The supervisor requested runtime shutdown.
    Shutdown(ShutdownEvent),
}

impl RuntimeEvent {
    /// Returns the stable event-family name used in trace output and tests.
    pub const fn family(&self) -> &'static str {
        match self {
            Self::Client(_) => "client",
            Self::Pane(_) => "pane",
            Self::Process(_) => "process",
            Self::AgentProvider(_) => "agent_provider",
            Self::AgentCompaction(_) => "agent_compaction",
            Self::Hook(_) => "hook",
            Self::Persistence(_) => "persistence",
            Self::Timer(_) => "timer",
            Self::Shutdown(_) => "shutdown",
        }
    }
}

/// Event emitted by an attached primary or observer client task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    /// Raw input bytes received from a client terminal.
    Input {
        /// Attached client identity.
        client_id: ClientId,
        /// Input bytes in terminal order.
        bytes: Vec<u8>,
    },
    /// Client terminal size changed.
    Resize {
        /// Attached client identity.
        client_id: ClientId,
        /// New terminal size.
        size: Size,
    },
    /// Host terminal reported that its size may have changed.
    ResizeSignal {
        /// Attached client identity.
        client_id: ClientId,
    },
    /// Client output became writable after prior backpressure.
    OutputReady {
        /// Attached client identity.
        client_id: ClientId,
    },
    /// Client detached or its terminal file descriptors were closed.
    Disconnected {
        /// Attached client identity.
        client_id: ClientId,
        /// Human-readable disconnect reason.
        reason: String,
    },
}

/// Event emitted by a pane PTY driver task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneEvent {
    /// A pane produced PTY output bytes.
    Output {
        /// Pane identity.
        pane_id: String,
        /// Bytes in PTY read order.
        bytes: Vec<u8>,
    },
    /// A previous pane write completed successfully.
    InputWritten {
        /// Pane identity.
        pane_id: String,
        /// Number of bytes accepted by the PTY.
        bytes: usize,
    },
    /// A pane write failed.
    WriteFailed {
        /// Pane identity.
        pane_id: String,
        /// Human-readable I/O failure.
        error: String,
    },
    /// A pane resize was applied to the PTY.
    Resized {
        /// Pane identity.
        pane_id: String,
        /// New visible PTY size.
        size: Size,
    },
    /// The pane worker observed foreground process metadata for title refresh.
    ForegroundProcess {
        /// Pane identity.
        pane_id: String,
        /// Foreground process display name.
        process_name: String,
        /// Foreground process group id.
        process_group_id: u32,
        /// Current working directory of the foreground process when known.
        current_working_directory: Option<String>,
    },
}

/// Event emitted by a process lifecycle watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessEvent {
    /// A pane process started.
    Spawned {
        /// Pane identity.
        pane_id: String,
        /// Operating-system process id when known.
        pid: Option<u32>,
    },
    /// A pane process exited.
    Exited {
        /// Pane identity.
        pane_id: String,
        /// Exit status code when available.
        exit_code: Option<i32>,
        /// Signal name or number when available.
        signal: Option<String>,
    },
    /// Process lifecycle management failed.
    Failed {
        /// Pane identity.
        pane_id: String,
        /// Human-readable failure.
        error: String,
    },
}

/// Event emitted by an async agent provider worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentProviderEvent {
    /// Provider work completed outside the runtime actor and the runtime should
    /// apply the produced turn execution.
    Completed {
        /// Agent identity.
        agent_id: AgentId,
        /// Turn identity.
        turn_id: String,
        /// Provider-produced turn execution to apply through actor-owned state
        /// transition logic.
        execution: Box<AgentTurnExecution>,
    },
    /// Provider work failed before producing an execution.
    Failed {
        /// Agent identity.
        agent_id: AgentId,
        /// Turn identity.
        turn_id: String,
        /// Stable failure kind for diagnostics.
        kind: String,
        /// Human-readable failure.
        message: String,
        /// Structured provider failure payload when the provider returned one.
        provider_failure_json: Option<String>,
        /// Raw provider text when the provider produced malformed output.
        provider_raw_text: Option<String>,
    },
}

/// Event emitted by an async conversation compaction worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCompactionEvent {
    /// Provider work completed with a model response containing the summary.
    Completed {
        /// Pane whose conversation was compacted.
        pane_id: String,
        /// Provider response produced by the compaction worker.
        response: Box<ModelResponse>,
    },
    /// Provider work failed before producing a summary response.
    Failed {
        /// Pane whose conversation compaction failed.
        pane_id: String,
        /// Stable failure kind for diagnostics.
        kind: String,
        /// Human-readable failure.
        message: String,
        /// Structured provider failure payload when the provider returned one.
        provider_failure_json: Option<String>,
        /// Raw provider text when the provider produced malformed output.
        provider_raw_text: Option<String>,
    },
}

/// Event emitted by an async hook worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsyncHookEvent {
    /// Program hook execution completed on an async hook worker.
    ProgramCompleted {
        /// Original hook plan used for audit and failure policy.
        plan: Box<HookExecutionPlan>,
        /// Execution result produced by the worker.
        result: Box<HookExecutionResult>,
        /// Whether the hook was triggered by a completed lifecycle event.
        triggering_event_completed: bool,
    },
    /// Hook execution completed.
    Completed {
        /// Hook run identity.
        hook_id: String,
        /// Exit status code when available.
        exit_code: Option<i32>,
        /// Bounded stdout/stderr preview.
        output_preview: String,
    },
    /// Hook execution failed before normal completion.
    Failed {
        /// Hook run identity.
        hook_id: String,
        /// Human-readable failure.
        error: String,
    },
}

/// Event emitted by an async persistence worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistenceEvent {
    /// A persistence write completed.
    Completed {
        /// Persistence family that handled the write.
        target: PersistenceTarget,
        /// Destination path written by the worker.
        path: PathBuf,
        /// Number of payload bytes written.
        bytes: usize,
    },
    /// A persistence write failed without crashing the worker.
    Failed {
        /// Persistence family that handled the write.
        target: PersistenceTarget,
        /// Destination path the worker attempted to write.
        path: PathBuf,
        /// Human-readable write failure.
        error: String,
    },
}

/// Runtime timer identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeTimerKey {
    /// Timer family.
    pub kind: RuntimeTimerKind,
    /// Runtime object identity the timer belongs to.
    pub owner_id: String,
    /// Monotonic generation used to ignore stale timer firings.
    pub generation: u64,
}

impl RuntimeTimerKey {
    /// Builds a runtime timer key.
    pub fn new(kind: RuntimeTimerKind, owner_id: impl Into<String>, generation: u64) -> Self {
        Self {
            kind,
            owner_id: owner_id.into(),
            generation,
        }
    }
}

/// Runtime timer family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeTimerKind {
    /// Shell command action timeout.
    ShellTransaction,
    /// Shell readiness probe timeout.
    ReadinessProbe,
    /// Agent bootstrap timeout.
    Bootstrap,
    /// Focused-shell hook timeout.
    FocusedShellHook,
    /// Attached terminal resize debounce.
    ResizeDebounce,
    /// Cursor blink frame update.
    CursorBlink,
    /// Right-side window status line refresh.
    StatusRefresh,
    /// Idle cleanup.
    IdleCleanup,
    /// Ordinary provider dispatch wakeup for pending work.
    ProviderPoll,
    /// Delayed provider retry after a transient provider failure.
    ProviderRetry,
    /// Timeout for a provider task claimed by an async worker.
    ProviderClaim,
    /// Short one-shot check for command-backed pane pipe completion or failure.
    PanePipeHealth,
}

/// Event emitted when a runtime-owned timer fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerEvent {
    /// Timer identity.
    pub key: RuntimeTimerKey,
    /// Actor-local current time in milliseconds.
    pub now_ms: u64,
}

/// Supervisor-initiated shutdown event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownEvent {
    /// Human-readable shutdown reason.
    pub reason: String,
    /// Whether shutdown should interrupt live pane processes.
    pub force: bool,
    /// Whether shutdown was caused by a supervisor or critical-service failure.
    pub failed: bool,
}

/// Side-effect request emitted by the runtime actor after handling events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeSideEffect {
    /// Write bytes to a pane PTY.
    WritePaneInput {
        /// Stores the pane id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        pane_id: String,
        /// Stores the bytes value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        bytes: Vec<u8>,
    },
    /// Write bytes to a pane PTY ahead of already queued input for that pane.
    WritePaneInputPriority {
        /// Pane whose PTY should receive the bytes.
        pane_id: String,
        /// Bytes to write to the pane PTY.
        bytes: Vec<u8>,
    },
    /// Resize a pane PTY.
    ResizePane {
        /// Stores the pane id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        pane_id: String,
        /// Stores the size value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        size: Size,
    },
    /// Request pane process termination.
    TerminatePane {
        /// Stores the pane id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        pane_id: String,
        /// Stores the force value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        force: bool,
    },
    /// Invalidate or render an attached client frame.
    RenderClient {
        /// Stores the client id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        client_id: ClientId,
        /// Stores the reason value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reason: RenderInvalidationReason,
    },
    /// Schedule a runtime-owned timer.
    ScheduleTimer {
        /// Stores the key value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        key: RuntimeTimerKey,
        /// Stores the delay ms value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        delay_ms: u64,
    },
    /// Cancel a runtime-owned timer.
    CancelTimer {
        /// Stores the key value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        key: RuntimeTimerKey,
    },
    /// Start an agent provider request outside the actor.
    DispatchAgentProvider {
        /// Stores the agent id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        agent_id: AgentId,
        /// Stores the turn id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        turn_id: String,
    },
    /// Start a model-backed conversation compaction outside the actor.
    DispatchAgentCompaction {
        /// Pane whose active conversation should be compacted.
        pane_id: String,
    },
    /// Execute a non-blocking program hook outside the actor.
    RunProgramHook {
        /// Original hook plan to execute.
        plan: Box<HookExecutionPlan>,
        /// Whether the hook was triggered by a completed lifecycle event.
        triggering_event_completed: bool,
    },
    /// Write data through a persistence worker.
    Persist {
        /// Persistence family that should own the write.
        target: PersistenceTarget,
        /// Destination path for the write.
        path: PathBuf,
        /// Bytes to write.
        bytes: Vec<u8>,
        /// Write behavior to use for the destination.
        mode: PersistenceWriteMode,
    },
    /// Append one audit JSONL record and apply retention on the persistence worker.
    PersistAuditLog {
        /// Destination audit log path.
        path: PathBuf,
        /// Encoded JSONL bytes, including the trailing newline.
        bytes: Vec<u8>,
        /// Retention policy to apply after the append completes.
        retention: AuditRetentionPolicy,
    },
    /// Append agent transcript entries through the transcript store.
    PersistTranscriptEntries {
        /// Transcript store that owns validation, paths, and permissions.
        store: AgentTranscriptStore,
        /// Destination transcript file used for diagnostics.
        path: PathBuf,
        /// Entries to append in sequence order.
        entries: Vec<TranscriptEntry>,
    },
    /// Append one submitted prompt to the shared prompt-history file.
    PersistPromptHistory {
        /// Transcript store that owns the shared history rewrite.
        store: AgentTranscriptStore,
        /// Destination prompt-history file used for diagnostics.
        path: PathBuf,
        /// Conversation identity used for validation.
        conversation_id: String,
        /// Prompt text to append.
        prompt: String,
    },
    /// Append one submitted command prompt entry to the shared command
    /// prompt-history file.
    PersistCommandPromptHistory {
        /// Transcript store that owns the command prompt history rewrite.
        store: AgentTranscriptStore,
        /// Destination command prompt-history file used for diagnostics.
        path: PathBuf,
        /// Command text to append.
        command: String,
    },
    /// Apply a session registry update through the persistence worker.
    PersistRegistry {
        /// Filesystem-backed session registry handle.
        registry: SessionRegistry,
        /// Registry mutation derived from current actor-owned session state.
        update: RuntimeRegistryUpdatePlan,
    },
    /// Flush styled output to an attached client.
    FlushClientOutput {
        /// Stores the client id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        client_id: ClientId,
        /// Stores the lines value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        lines: Vec<String>,
        /// Stores the line style spans value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        line_style_spans: Vec<Vec<TerminalStyleSpan>>,
        /// Stores the modes value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        modes: AttachedTerminalOutputModes,
    },
}

/// Reason a client render was invalidated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderInvalidationReason {
    /// Pane output changed visible content.
    PaneOutput,
    /// Layout geometry changed.
    Layout,
    /// Client size changed.
    Resize,
    /// Agent prompt or display state changed.
    AgentPrompt,
    /// Command, help, or copy-mode overlay changed.
    Overlay,
    /// Runtime theme or presentation settings changed.
    Configuration,
    /// Cursor blink phase changed and clients should repaint the cursor.
    CursorBlink,
    /// Runtime status-line fields changed.
    StatusLine,
    /// A full repaint is required after mode exit or presentation recovery.
    FullRedraw,
}

/// Persistence worker target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceTarget {
    /// Audit JSONL.
    AuditLog,
    /// Agent transcript storage.
    Transcript,
    /// Snapshot repository.
    Snapshot,
    /// Configuration file.
    Config,
    /// Session registry file.
    Registry,
    /// Project-local configuration file.
    ProjectConfig,
    /// Project instruction scaffold file.
    ProjectInstruction,
    /// File-backed pane pipe output.
    PanePipe,
}

impl PersistenceTarget {
    /// Returns the stable target name used in runtime diagnostics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuditLog => "audit_log",
            Self::Transcript => "transcript",
            Self::Snapshot => "snapshot",
            Self::Config => "config",
            Self::Registry => "registry",
            Self::ProjectConfig => "project_config",
            Self::ProjectInstruction => "project_instruction",
            Self::PanePipe => "pane_pipe",
        }
    }
}

/// Persistence worker file-write behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceWriteMode {
    /// Append bytes to the end of the file, creating it when absent.
    Append,
    /// Replace the file contents, creating it when absent.
    Replace,
    /// Create a new file and fail if the destination already exists.
    CreateNew,
}

/// Ordered batch of runtime events received from one async wakeup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeEventBatch {
    /// Events in delivery order.
    pub events: Vec<RuntimeEvent>,
}

impl RuntimeEventBatch {
    /// Creates an empty event batch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one event to the batch.
    pub fn push(&mut self, event: RuntimeEvent) {
        self.events.push(event);
    }

    /// Returns stable event-family names in delivery order.
    pub fn families(&self) -> Vec<&'static str> {
        self.events.iter().map(RuntimeEvent::family).collect()
    }

    /// Builds an actor ingress report without mutating runtime state.
    pub fn ingress_report(&self) -> RuntimeEventIngressReport {
        RuntimeEventIngressReport {
            accepted: self.events.len(),
            applied: 0,
            side_effects: 0,
            families: self.families().into_iter().map(str::to_string).collect(),
        }
    }
}

/// Report returned after the actor accepts an async runtime event batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEventIngressReport {
    /// Number of events accepted by the actor.
    pub accepted: usize,
    /// Number of events applied to live runtime state by the actor.
    pub applied: usize,
    /// Number of actor side effects queued while applying the batch.
    pub side_effects: usize,
    /// Stable event-family names in delivery order.
    pub families: Vec<String>,
}
