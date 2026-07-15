//! Purpose-named agent test modules.
//!
//! Shared fixtures stay at parent scope while explicit bounded leaves own the
//! test harness entry points. Test function names and behavior remain unchanged.

// Regression coverage for the agent tests subsystem.
//
// These tests describe the behavior protected by the repository
// specification and workflow guidance. Keeping the scenarios documented
// makes failures easier to map back to the user-visible contract.

// Agent module tests.

use super::actions::action_result_transcript_content;
use super::{
    AgentCapability, AgentContext, AgentLogLevel, AgentPromptProfile, AgentShellCommandOutcome,
    AgentShellStore, AgentShellVisibility, AgentTurnExecution, AgentTurnLedger, AgentTurnRecord,
    AgentTurnRunner, AsyncModelProvider, AsyncProviderHttpTransport, CHATGPT_ACCOUNT_ID_HEADER,
    ContextBlock, ContextCachePolicy, ContextSourceKind, ContextStability,
    DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME, DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME,
    DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME, DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS,
    EnvironmentSignature, MarkerToken, McpActionExecutor, ModelMessage, ModelMessageRole,
    ModelProfile, ModelProvider, ModelResponse, OpenAiResponsesProvider, PaneShellExecutor,
    ProviderHttpTransport, Result, ShellClassification, ShellExecutionOutput,
    ShellExecutionRequest, ShellTransaction, ShellTransactionInput,
    ShellTransactionOutputTransport, ToolDiscoveryCache, ToolInventory,
    action_result_context_content, agent_subshell_enter_command, append_mcp_context,
    append_memory_context, append_permission_policy_context, append_project_guidance_context,
    append_scheduler_context, apply_default_action_gates, apply_patch_read_plan_for_paths,
    apply_patch_write_plan_from_read_output, apply_patch_write_plan_from_read_outputs,
    assemble_model_request, bootstrap_script, bootstrap_script_for_classification,
    build_agent_system_prompt, build_deepseek_chat_completions_http_request,
    compact_model_context_for_budget, compact_model_context_for_budget_with_retained_tail_percent,
    decode_shell_output_transport, decode_shell_output_transport_with_diagnostics,
    deepseek_chat_completions_provider_from_auth_store_with_provider_options,
    discover_tools_through_pane_shell, execute_agent_shell_command,
    execute_agent_shell_command_with_mcp, execute_agent_shell_command_with_permissions,
    execute_mcp_action_through_runtime, execute_network_action_with_transport_async,
    execute_shell_action_through_pane, local_action_plan,
    openai_compatible_provider_from_auth_store_with_provider_options,
    openai_provider_from_auth_store_with_options,
    openai_provider_from_auth_store_with_provider_options,
    openai_provider_from_auth_store_with_transport,
    openai_responses_provider_from_auth_store_with_provider_options, parse_bootstrap_env_output,
    parse_fenced_maap_action_batch, parse_maap_action_batch_json,
    parse_maap_action_batch_json_for_turn, parse_openai_models_http_body, parse_slash_command,
    persist_turn_execution_transcript, postprocess_shell_action_success_output,
    readiness_probe_command_for_classification, set_project_guidance_context,
    tool_discovery_script, transcript_entries_for_execution,
};
use super::{prompt, semantic, shell};
use crate::auth::{AuthStore, OpenAiProviderCredential};
use crate::mcp::McpRegistry;
use crate::permissions::{PathScopes, PermissionPolicy, SessionApprovalStore};
use crate::test_support::agent::ActionBuilder;
use crate::test_support::temp::TestTempDir;
use crate::transcript::{AgentTranscriptStore, TranscriptRole as DurableTranscriptRole};
use base64::Engine;
use mez_agent::instructions::DiscoveredInstructionFile;
use mez_agent::semantic_patch::try_convert_unified_diff_to_mez_patch;
use mez_agent::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload,
    AgentTranscriptRole as TranscriptRole, AgentTurnState, AgentTurnTrigger,
    CHATGPT_RESPONSES_ENDPOINT, MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME,
    MaapBatch, McpExecutionRequest, McpExecutionResponse, McpPromptTool, MemoryContextRecord,
    MemoryContextScope, ModelRequest, ModelTokenUsage, OPENAI_MODELS_ENDPOINT,
    OPENAI_RESPONSES_ENDPOINT, ProviderHttpRequest, ProviderHttpResponse, ProviderTranscriptEvent,
    SlashCommandEffect, baseline_slash_commands, openai_models_endpoint_for_responses_endpoint,
    openai_prompt_cache_diagnostics_for_request, openai_responses_endpoint_for_base_url,
    openai_responses_request_body, openai_stable_prefix_material_for_request,
    provider_quota_usage_from_headers, shell_quote,
};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;
use wait_timeout::ChildExt;

/// Runs the looks like uuid v4 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn looks_like_uuid_v4(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && bytes[14] == b'4'
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_hexdigit())
}

/// Builds a representative MCP tool state for agent-shell display tests. The
/// registry normalizes server id, availability, and approval from the owning
/// server config, so tests can override only the fields relevant to each
/// display-state case before calling `mark_available`.
fn agent_shell_test_mcp_tool(name: &str) -> crate::mcp::McpToolState {
    crate::mcp::McpToolState {
        server_id: String::new(),
        name: name.to_string(),
        available: true,
        blacklisted: false,
        permission_required: true,
        effects: crate::mcp::McpToolEffects {
            reads_filesystem: true,
            ..crate::mcp::McpToolEffects::none()
        },
        approval: crate::mcp::McpApprovalSetting::Inherit,
        description: format!("{name} description"),
        input_schema_json: r#"{"type":"object"}"#.to_string(),
    }
}

/// Executes `/list-mcp` against an injected registry and returns the display body so
/// each state-focused test can assert the user-visible command output without
/// repeating shell session setup.
fn agent_shell_test_mcp_body(registry: &McpRegistry) -> String {
    let mut store = AgentShellStore::default();
    store.enter_or_resume("%1").unwrap();
    let summary = registry.agent_shell_summary();
    match execute_agent_shell_command_with_mcp(&mut store, "%1", "/list-mcp", Some(&summary))
        .unwrap()
        .unwrap()
    {
        AgentShellCommandOutcome::Display { body, .. } => body,
        outcome => panic!("expected /list-mcp display outcome, got {outcome:?}"),
    }
}

/// Carries Fake Pane Shell Executor state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default)]
struct FakePaneShellExecutor {
    /// Stores the requests value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    requests: Vec<ShellExecutionRequest>,
    /// Stores the output value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    output: Option<ShellExecutionOutput>,
}

impl PaneShellExecutor for FakePaneShellExecutor {
    /// Runs the execute shell operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn execute_shell(&mut self, request: &ShellExecutionRequest) -> Result<ShellExecutionOutput> {
        self.requests.push(request.clone());
        Ok(self.output.clone().unwrap_or(ShellExecutionOutput {
            exit_code: Some(0),
            signal: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            interrupted: false,
            transport_diagnostics: Default::default(),
        }))
    }
}

/// Carries Fake Provider Http Transport state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
struct FakeProviderHttpTransport {
    /// Stores the requests value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    requests: RefCell<Vec<ProviderHttpRequest>>,
    /// Stores the response value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    response: ProviderHttpResponse,
}

impl ProviderHttpTransport for FakeProviderHttpTransport {
    /// Runs the send operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send(
        &self,
        request: &ProviderHttpRequest,
    ) -> mez_agent::ProviderHttpResult<ProviderHttpResponse> {
        self.requests.borrow_mut().push(request.clone());
        Ok(self.response.clone())
    }
}

/// Carries a sequence of fake provider HTTP responses for retry tests.
///
/// The type records each outgoing request and returns responses in declaration
/// order so provider retry behavior can be asserted without live network I/O.
#[derive(Debug)]
struct SequencedFakeProviderHttpTransport {
    /// Provider requests issued during the test.
    requests: RefCell<Vec<ProviderHttpRequest>>,
    /// Responses returned to the provider adapter in FIFO order.
    responses: RefCell<std::collections::VecDeque<ProviderHttpResponse>>,
}

impl SequencedFakeProviderHttpTransport {
    /// Creates a fake transport from an ordered list of responses.
    ///
    /// # Parameters
    /// - `responses`: The responses to return, one per provider request.
    fn new(responses: Vec<ProviderHttpResponse>) -> Self {
        Self {
            requests: RefCell::new(Vec::new()),
            responses: RefCell::new(responses.into()),
        }
    }
}

impl ProviderHttpTransport for SequencedFakeProviderHttpTransport {
    /// Records one request and returns the next queued provider response.
    fn send(
        &self,
        request: &ProviderHttpRequest,
    ) -> mez_agent::ProviderHttpResult<ProviderHttpResponse> {
        self.requests.borrow_mut().push(request.clone());
        self.responses.borrow_mut().pop_front().ok_or_else(|| {
            mez_agent::ProviderHttpError::invalid_state("fake provider response queue is empty")
        })
    }
}

/// Carries Async Fake Provider Http Transport state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
struct AsyncFakeProviderHttpTransport {
    /// Stores the requests value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    requests: std::sync::Mutex<Vec<ProviderHttpRequest>>,
    /// Stores the response value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    response: ProviderHttpResponse,
}

impl AsyncProviderHttpTransport for AsyncFakeProviderHttpTransport {
    /// Runs the send async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_async<'a>(
        &'a self,
        request: &'a ProviderHttpRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = mez_agent::ProviderHttpResult<ProviderHttpResponse>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.clone());
            Ok(self.response.clone())
        })
    }
}

/// Carries Fake Mcp Action Executor state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
struct FakeMcpActionExecutor {
    /// Stores the plans value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    plans: Vec<McpExecutionRequest>,
    /// Stores the response value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    response: McpExecutionResponse,
}

impl McpActionExecutor for FakeMcpActionExecutor {
    /// Runs the execute mcp call operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn execute_mcp_call(&mut self, request: &McpExecutionRequest) -> Result<McpExecutionResponse> {
        self.plans.push(request.clone());
        Ok(self.response.clone())
    }
}

/// Runs the marker operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn marker() -> MarkerToken {
    MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap()
}

/// Encodes one fake pane-shell output payload in the shell transport frame.
fn framed_shell_output(text: &str) -> String {
    format!(
        "{}\n{}\n{}\n",
        super::shell::SHELL_OUTPUT_BASE64_BEGIN_MARKER,
        base64::engine::general_purpose::STANDARD.encode(text.as_bytes()),
        super::shell::SHELL_OUTPUT_BASE64_END_MARKER
    )
}

/// Runs one POSIX shell script through stdin rather than `sh -c`.
///
/// Transaction wrappers intentionally stream their command payload after the
/// wrapper function invocation, so tests must execute them through the same
/// input channel used by live pane shells.
fn run_sh_stdin(script: &str) -> Output {
    let mut command = Command::new("/bin/sh");
    run_command_stdin(&mut command, script)
}

/// Runs one streamed transaction through POSIX shell stdin.
///
/// # Parameters
/// - `input`: The wrapper and payload rendered for the shell transaction.
/// - `suffix`: Additional shell input to send after the transaction payload.
fn run_sh_transaction(input: &ShellTransactionInput, suffix: &str) -> Output {
    let mut command = Command::new("/bin/sh");
    run_command_transaction_stdin(&mut command, input, suffix)
}

/// Runs one command while writing transaction wrapper and payload separately.
///
/// # Parameters
/// - `command`: The process builder to spawn.
/// - `input`: The transaction input to stream.
/// - `suffix`: Additional shell input to send after the transaction payload.
fn run_command_transaction_stdin(
    command: &mut Command,
    input: &ShellTransactionInput,
    suffix: &str,
) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.as_mut().unwrap();
    stdin.write_all(input.wrapper.as_bytes()).unwrap();
    thread::sleep(Duration::from_millis(50));
    stdin.write_all(input.payload.as_bytes()).unwrap();
    stdin.write_all(suffix.as_bytes()).unwrap();
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

/// Runs one command while writing a script to its stdin.
///
/// # Parameters
/// - `command`: The process builder to spawn.
/// - `script`: The exact stdin bytes supplied to the process.
fn run_command_stdin(command: &mut Command, script: &str) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

/// Runs the test env signature operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
fn test_env_signature(
    host: &str,
    user: &str,
    shell_path: &str,
    working_directory: &str,
) -> EnvironmentSignature {
    EnvironmentSignature::new(
        "linux",
        "x86_64",
        None,
        host,
        user,
        shell_path,
        ShellClassification::classify(shell_path),
        None,
        None,
        working_directory,
        None,
        false,
        None,
        Vec::new(),
    )
    .expect("test environment signature should be valid")
}

/// Runs the turn operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn turn() -> AgentTurnRecord {
    AgentTurnRecord {
        turn_id: "turn-1".to_string(),
        agent_id: "agent-1".to_string(),
        pane_id: "%1".to_string(),
        trigger: AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0),
        policy_profile: "ask".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        cooperation_mode: None,
        initial_capability: None,
        state: AgentTurnState::Queued,
    }
}

/// Runs the shell action operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn shell_action(id: &str) -> AgentAction {
    ActionBuilder::shell(id)
}

/// Runs the say action operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn say_action(id: &str, text: &str) -> AgentAction {
    ActionBuilder::say(id, text)
}

/// Builds an abort action for validating that provider-authored aborts stay
/// outside the exposed action surface.
fn abort_action(id: &str, reason: &str) -> AgentAction {
    ActionBuilder::abort(id, reason)
}

/// Runs the mcp action operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mcp_action(id: &str) -> AgentAction {
    ActionBuilder::mcp(id)
}

/// Builds a live configuration mutation action for approval-policy tests.
fn config_change_action(id: &str) -> AgentAction {
    ActionBuilder::config_change(id)
}

/// Builds a non-executing capability request action for runner tests.
///
/// The helper keeps capability negotiation explicit in tests that need to
/// exercise executable actions after the first provider round-trip.
fn capability_action(id: &str, capability: AgentCapability) -> AgentAction {
    ActionBuilder::capability(id, capability)
}

/// Creates a unique temporary directory for tests without adding another
/// dependency to the crate under test. Callers remove the directory after the
/// assertions that need it complete.
fn test_temp_dir(label: &str) -> TestTempDir {
    TestTempDir::new(label)
}

/// Builds a Mezzanine add-file patch for one relative path and exact content.
///
/// The helper keeps file-content action tests focused on the reduced
/// model-facing mutation surface while preserving explicit coverage for large,
/// empty, and shell-sensitive payloads.
fn add_file_patch(path: &str, content: &str) -> String {
    let mut patch = format!("*** Begin Patch\n*** Add File: {path}\n");
    for line in content.split_inclusive('\n') {
        patch.push('+');
        patch.push_str(line);
    }
    if !content.ends_with('\n') && !content.is_empty() {
        patch.push('\n');
    }
    patch.push_str("*** End Patch");
    patch
}

/// Executes an `apply_patch` action through its read and write phases in one
/// temporary working directory.
///
/// Tests use this helper when validating file-content behavior that used to be
/// covered by broader semantic mutation actions.
fn run_apply_patch_action(cwd: &Path, patch: &str) -> Output {
    let action = AgentAction {
        id: "patch".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ApplyPatch {
            patch: patch.to_string(),
            strip: None,
        },
    };
    let read_plan = local_action_plan(&action).unwrap().unwrap();
    let read_output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&read_plan.command)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        read_output.status.success(),
        "read phase failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&read_output.stdout),
        String::from_utf8_lossy(&read_output.stderr)
    );
    let write_plan = apply_patch_write_plan_from_read_output(
        patch,
        &String::from_utf8_lossy(&read_output.stdout),
    )
    .unwrap();
    Command::new("/bin/sh")
        .arg("-c")
        .arg(&write_plan.command)
        .current_dir(cwd)
        .output()
        .unwrap()
}

/// Returns the write-phase error message for one semantic patch action.
///
/// Tests use this helper for matcher failures that should be reported before
/// any generated write command is emitted.
fn apply_patch_write_error(cwd: &Path, patch: &str) -> String {
    let action = AgentAction {
        id: "patch-error".to_string(),
        rationale: String::new(),
        payload: AgentActionPayload::ApplyPatch {
            patch: patch.to_string(),
            strip: None,
        },
    };
    let read_plan = local_action_plan(&action).unwrap().unwrap();
    let read_output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&read_plan.command)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        read_output.status.success(),
        "read phase failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&read_output.stdout),
        String::from_utf8_lossy(&read_output.stderr)
    );
    apply_patch_write_plan_from_read_output(patch, &String::from_utf8_lossy(&read_output.stdout))
        .unwrap_err()
        .message()
        .to_string()
}

/// Runs the mcp plan operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mcp_plan() -> McpExecutionRequest {
    McpExecutionRequest {
        server_id: "state".to_string(),
        tool_name: "list".to_string(),
        arguments_json: r#"{"path":"."}"#.to_string(),
        timeout_ms: 1000,
    }
}

/// Carries Echo Provider state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
struct EchoProvider;

impl ModelProvider for EchoProvider {
    /// Runs the provider id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn provider_id(&self) -> &str {
        "echo"
    }

    /// Runs the send request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_request(&self, request: &ModelRequest) -> Result<ModelResponse> {
        Ok(ModelResponse {
            provider: self.provider_id().to_string(),
            model: request.model.clone(),
            raw_text: "ok".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: None,
            provider_transcript_events: Vec::new(),
        })
    }
}

/// Carries Batch Provider state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
struct BatchProvider {
    /// Stores the response value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    response: ModelResponse,
}

impl ModelProvider for BatchProvider {
    /// Runs the provider id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn provider_id(&self) -> &str {
        "batch"
    }

    /// Runs the send request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_request(&self, _request: &ModelRequest) -> Result<ModelResponse> {
        Ok(self.response.clone())
    }
}

/// Test provider that performs a capability request before returning a fixed
/// executable response. This mirrors the runtime interaction shape expected by
/// the runner while keeping action-planning tests focused on their target
/// behavior.
struct CapabilityBatchProvider {
    /// Capability requested on the first provider call.
    capability: AgentCapability,
    /// Response returned after the capability is granted.
    response: ModelResponse,
    /// Requests sent to the provider in call order.
    requests: std::sync::Mutex<Vec<ModelRequest>>,
}

impl CapabilityBatchProvider {
    /// Creates a provider that negotiates the supplied capability before
    /// returning the supplied response.
    fn new(capability: AgentCapability, response: ModelResponse) -> Self {
        Self {
            capability,
            response,
            requests: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl ModelProvider for CapabilityBatchProvider {
    /// Runs the provider id operation for this subsystem.
    fn provider_id(&self) -> &str {
        "batch"
    }

    /// Returns a capability request on the first call and the configured
    /// executable response thereafter.
    fn send_request(&self, request: &ModelRequest) -> Result<ModelResponse> {
        let mut requests = self.requests.lock().unwrap();
        let call_index = requests.len();
        requests.push(request.clone());
        drop(requests);

        if call_index == 0 {
            return Ok(ModelResponse {
                provider: self.provider_id().to_string(),
                model: request.model.clone(),
                raw_text: format!("request {}", self.capability.as_str()),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test action batch rationale".to_string(),
                    thought: None,
                    turn_id: request.turn_id.clone(),
                    agent_id: request.agent_id.clone(),
                    actions: vec![capability_action("capability-1", self.capability)],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            });
        }

        Ok(self.response.clone())
    }
}

/// Carries Request Capturing Provider state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
struct RequestCapturingProvider {
    /// Stores the response value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    response: ModelResponse,
    /// Stores the last request value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    last_request: RefCell<Option<ModelRequest>>,
}

impl ModelProvider for RequestCapturingProvider {
    /// Runs the provider id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn provider_id(&self) -> &str {
        "batch"
    }

    /// Runs the send request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_request(&self, request: &ModelRequest) -> Result<ModelResponse> {
        self.last_request.replace(Some(request.clone()));
        Ok(self.response.clone())
    }
}

/// Test provider that returns a deterministic sequence of model responses and
/// records each request so retry prompts can be inspected without relying on a
/// network-backed provider.
struct SequencedProvider {
    /// Queued responses returned one per provider call.
    responses: std::sync::Mutex<std::collections::VecDeque<Result<ModelResponse>>>,
    /// Requests sent to the provider in call order.
    requests: std::sync::Mutex<Vec<ModelRequest>>,
}

impl SequencedProvider {
    /// Creates a sequenced provider with the supplied response queue.
    fn new(responses: Vec<Result<ModelResponse>>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into()),
            requests: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Pops the next response after recording the request.
    fn next_response(&self, request: &ModelRequest) -> Result<ModelResponse> {
        self.requests.lock().unwrap().push(request.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(crate::MezError::invalid_state("no queued response")))
    }

    /// Returns the captured provider requests.
    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl ModelProvider for SequencedProvider {
    /// Returns the stable provider id used by tests.
    fn provider_id(&self) -> &str {
        "batch"
    }

    /// Returns the next queued response.
    fn send_request(&self, request: &ModelRequest) -> Result<ModelResponse> {
        self.next_response(request)
    }
}

impl AsyncModelProvider for SequencedProvider {
    /// Returns the stable provider id used by tests.
    fn provider_id(&self) -> &str {
        "batch"
    }

    /// Returns the next queued response through the async provider trait.
    fn send_request_async<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ModelResponse>> + Send + 'a>>
    {
        Box::pin(async move { self.next_response(request) })
    }
}

/// Verifies every object in a strict OpenAI schema has an exhaustive required
/// list matching its advertised properties.
fn assert_openai_strict_schema_shape(schema: &serde_json::Value) {
    assert_openai_strict_schema_shape_at(schema, "$");
}

/// Finds a named OpenAI function tool in a Responses request body.
fn openai_function_tool<'a>(body: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    body["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("OpenAI request body does not contain tools: {body}"))
        .iter()
        .find(|tool| tool["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("OpenAI request body does not contain tool {name}: {body}"))
}

/// Returns the action schema variants for one OpenAI MAAP function tool.
fn openai_tool_action_schemas(tool: &serde_json::Value) -> &Vec<serde_json::Value> {
    tool["parameters"]["properties"]["actions"]["items"]["anyOf"]
        .as_array()
        .unwrap_or_else(|| panic!("OpenAI tool does not contain MAAP action variants: {tool}"))
}

/// Returns the MAAP action type names advertised by one OpenAI function tool.
fn openai_tool_action_types(tool: &serde_json::Value) -> Vec<String> {
    openai_tool_action_schemas(tool)
        .iter()
        .filter_map(|schema| {
            schema["properties"]["type"]["enum"][0]
                .as_str()
                .map(str::to_string)
        })
        .collect()
}

/// Finds the single Mezzanine function tool in a DeepSeek Chat Completions request.
fn deepseek_maap_function_tool(body: &serde_json::Value) -> &serde_json::Value {
    body["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("DeepSeek request body does not contain tools: {body}"))
        .iter()
        .find(|tool| {
            matches!(
                tool["function"]["name"].as_str(),
                Some(DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME)
                    | Some(DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME)
                    | Some(DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME)
            )
        })
        .unwrap_or_else(|| panic!("DeepSeek request body does not contain Mezzanine tool: {body}"))
}

/// Returns the MAAP action type names advertised by one DeepSeek action tool.
fn deepseek_tool_action_types(tool: &serde_json::Value) -> Vec<String> {
    tool["function"]["parameters"]["properties"]["actions"]["items"]["anyOf"]
        .as_array()
        .unwrap_or_else(|| panic!("DeepSeek tool does not contain MAAP action variants: {tool}"))
        .iter()
        .filter_map(|schema| {
            schema["properties"]["type"]["enum"][0]
                .as_str()
                .map(str::to_string)
        })
        .collect()
}

/// Recursively validates strict-schema object requirements with a path that
/// makes provider 400 regressions diagnosable from test failures.
fn assert_openai_strict_schema_shape_at(schema: &serde_json::Value, path: &str) {
    if let Some(object) = schema.as_object() {
        if let Some(properties) = object
            .get("properties")
            .and_then(serde_json::Value::as_object)
        {
            let required = object
                .get("required")
                .unwrap_or_else(|| panic!("schema object at {path} is missing required"))
                .as_array()
                .unwrap_or_else(|| panic!("schema required at {path} is not an array"));
            let mut property_names = properties.keys().cloned().collect::<Vec<_>>();
            let mut required_names = required
                .iter()
                .map(|field| {
                    field
                        .as_str()
                        .unwrap_or_else(|| panic!("schema required field at {path} is not string"))
                        .to_string()
                })
                .collect::<Vec<_>>();
            property_names.sort();
            required_names.sort();
            assert_eq!(
                required_names, property_names,
                "strict schema object at {path} must require every property"
            );
            assert_eq!(
                object.get("additionalProperties"),
                Some(&serde_json::Value::Bool(false)),
                "strict schema object at {path} must deny additional properties"
            );
        }
        for (key, child) in object {
            assert_openai_strict_schema_shape_at(child, &format!("{path}.{key}"));
        }
    } else if let Some(items) = schema.as_array() {
        for (index, child) in items.iter().enumerate() {
            assert_openai_strict_schema_shape_at(child, &format!("{path}[{index}]"));
        }
    }
}

/// Returns the rendered OpenAI stable-prefix instructions and input messages
/// for request-shape tests.
fn openai_test_stable_prefix_parts(request: &ModelRequest) -> (String, Vec<serde_json::Value>) {
    let material = openai_stable_prefix_material_for_request(request).unwrap();
    let value: serde_json::Value = serde_json::from_str(&material).unwrap();
    let instructions = value["instructions"].as_str().unwrap().to_string();
    let stable_input = value["stable_input"].as_array().unwrap().clone();
    (instructions, stable_input)
}

/// Builds a minimal OpenAI request for prompt-cache retention tests.
fn openai_prompt_cache_retention_test_request(model: &str) -> ModelRequest {
    assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: model.to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user".to_string(),
            content: "debug this failing test".to_string(),
        }])
        .unwrap(),
    )
    .unwrap()
}

/// Builds a memory search action for runner budget tests.
///
/// Keeping this helper local to the memory-planning regressions makes the
/// tests read in terms of the behavior under test instead of repeating the
/// same MAAP payload fields.
fn memory_search_action(id: &str) -> AgentAction {
    AgentAction {
        id: id.to_string(),
        rationale: "retrieve a bounded durable memory hint".to_string(),
        payload: AgentActionPayload::MemorySearch {
            query: format!("durable context {id}"),
            limit: Some(1),
        },
    }
}

/// Builds a memory store action for runner budget tests.
///
/// Store actions share a longer payload than searches, so the helper keeps the
/// guardrail assertions focused on runtime behavior rather than durable memory
/// schema boilerplate.
fn memory_store_action(id: &str) -> AgentAction {
    AgentAction {
        id: id.to_string(),
        rationale: "store durable project context".to_string(),
        payload: AgentActionPayload::MemoryStore {
            kind: "fact".to_string(),
            priority: Some(80),
            scope: Some("project".to_string()),
            keywords: vec!["memory".to_string(), "guardrail".to_string()],
            content: format!("durable reusable context from {id}"),
            expires_in_days: Some(30),
        },
    }
}

mod action_results;
mod agent_runtime;
mod agent_shell;
mod deepseek_provider;
mod maap_protocol;
mod mcp_runtime;
mod model_context;
mod network_actions;
mod openai_cache;
mod openai_provider;
mod openai_requests;
mod provider_contract;
mod semantic_patch;
mod shell_bootstrap;
mod shell_execution;
mod shell_transport;
mod system_prompt;
mod tool_discovery;
mod transcript;
mod turn_ledger;
mod turn_runner;
