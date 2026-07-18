//! Claude Code subprocess provider adapter.
//!
//! This module owns the experimental provider boundary for Claude Code
//! subscription-backed execution. The adapter invokes the local `claude` CLI in
//! noninteractive print mode, owns temporary settings and system-prompt files,
//! serializes session access, and projects lower Claude policy results into
//! product responses without granting direct tool or filesystem authority.

use super::{
    AsyncModelProvider, MaapBatch, MezError, ModelInteractionKind, ModelRequest, ModelResponse,
    ModelTokenUsage, ProviderModelCatalog, Result, validate_non_empty,
};
use mez_agent::{
    CLAUDE_CODE_STRUCTURED_OUTPUT_TOOL, ClaudeCodeOutput, bound_claude_code_text,
    claude_code_auto_sizing_json_schema, claude_code_corrective_retry_instruction,
    claude_code_empty_output_error, claude_code_maap_json_schema,
    claude_code_macro_judge_json_schema, claude_code_prompt, claude_code_system_prompt,
    parse_claude_code_json_output, parse_claude_code_maap_output, redact_claude_code_text,
    validate_claude_code_auto_sizing_output,
};
use std::sync::atomic::AtomicU64;

/// Executable name used for Claude Code subprocess requests.
const CLAUDE_CODE_PROGRAM: &str = "claude";
/// Claude Code native tools that must stay unavailable under Mezzanine-managed
/// execution.
const CLAUDE_CODE_DISALLOWED_NATIVE_TOOLS: &str = concat!(
    "SendUserMessage,Bash,Edit,Read,Agent,Artifact,AskUserQuestion,CronCreate,CronDelete,",
    "CronList,EnterPlanMode,EnterWorktree,ExitPlanMode,ExitWorktree,Glob,Grep,",
    "LSP,Monitor,NotebookEdit,PushNotification,",
    "ReadMcpResourceTool,RemoteTrigger,ScheduleWakeup,SendMessage,",
    "SendUserFile,ShareOnboardingGuide,Skill,TaskCreate,TaskGet,TaskList,TaskOutput,",
    "TaskStop,TaskUpdate,TodoWrite,ToolSearch,WaitForMcpServers,Workflow,Write,",
    "WebFetch,WebSearch",
);
/// Monotonic suffix used to create per-invocation Claude settings files.
static CLAUDE_CODE_SETTINGS_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

mod provider;
mod session;
mod settings_file;
mod transport_adapter;

pub use provider::ClaudeCodeProvider;
use session::run_claude_code_request_with_corrective_retry;
use settings_file::{ClaudeCodeSettingsFile, ClaudeCodeSystemPromptFile};
#[cfg(test)]
use transport_adapter::claude_code_spawn_error_is_transient;
use transport_adapter::{
    ClaudeCodeRequestOutput, ClaudeCodeSubprocessRequest, run_claude_code_subprocess,
};

#[cfg(test)]
mod tests;
