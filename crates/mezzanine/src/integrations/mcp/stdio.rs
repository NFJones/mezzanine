//! Stdio transport support for local MCP subprocesses.
//!
//! This module owns subprocess spawning, newline-delimited JSON-RPC transport,
//! bounded stdout reads, stderr draining, discovery, and registry integration.
//!
//! Reader-task lifetime is part of the transport contract. A connection owns
//! the exact stdout protocol-reader and stderr-drain task handles and cancels
//! them on explicit retirement, partial startup failure, and `Drop`.
//! Cancellation never waits for child or descendant EOF, never signals an
//! unowned process group, and never spawns an untracked cleanup task. Explicit
//! asynchronous retirement observes bounded settlement; `Drop` is the
//! synchronous safety net for an abandoned transport.

use std::collections::BTreeMap;
#[cfg(test)]
use std::io::Read;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::task::JoinHandle;

use crate::error::{MezError, Result};

use mez_agent::mcp::{
    DEFAULT_MCP_MAX_MESSAGE_BYTES, McpInitializeResponse, McpStartupPlan, McpStartupTransportPlan,
    McpToolCallPlan, McpToolCallResponse, McpToolsListResponse, build_mcp_initialized_notification,
    json_id_matches, mcp_initialize_operation, mcp_tools_call_operation, mcp_tools_list_operation,
};
#[cfg(test)]
use mez_agent::mcp::{
    McpRegistry, McpStdioDiscovery, McpToolDiscoveryBudget, McpToolListPagination,
};

/// Carries Mcp Stdio Read Event state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
enum McpStdioReadEvent {
    /// Represents the Message case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Message(String),
    /// Represents the Protocol Error case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ProtocolError(String),
}

/// Bounded grace period for one explicit retirement to observe reader-task and
/// direct-child settlement before reporting cleanup uncertainty.
const STDIO_RETIREMENT_SETTLEMENT_TIMEOUT: Duration = Duration::from_secs(2);

/// Owns exactly one detached stdio pump task for its connection.
///
/// The handle is created at spawn time, retained by [`McpStdioConnection`],
/// and cancelled by explicit retirement, partial startup failure, or `Drop`.
/// Cancellation is a synchronous request; [`Self::settle`] is the asynchronous
/// observation step used by explicit retirement, so a connection never relies
/// on child or descendant EOF to release a reader.
struct McpStdioReaderTask {
    /// Stable label used in bounded cleanup diagnostics.
    label: &'static str,
    /// Handle for the spawned pump task.
    handle: JoinHandle<()>,
}

impl McpStdioReaderTask {
    /// Wraps one spawned pump task with its diagnostic label.
    fn new(label: &'static str, handle: JoinHandle<()>) -> Self {
        Self { label, handle }
    }

    /// Requests task cancellation without awaiting settlement.
    ///
    /// Dropping the task future closes its pipe descriptor read end, so
    /// reader destruction never depends on child or descendant EOF. This path
    /// is deliberately synchronous so `Drop` never creates another task.
    fn cancel(&self) {
        self.handle.abort();
    }

    /// Waits for the already-cancelled task to finish by one shared deadline.
    async fn settle(self, deadline: tokio::time::Instant) -> Result<()> {
        match tokio::time::timeout_at(deadline, self.handle).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) if error.is_cancelled() => Ok(()),
            Ok(Err(error)) => Err(MezError::invalid_state(format!(
                "MCP stdio {} reader task failed to join after cancellation: {error}",
                self.label
            ))),
            Err(_) => Err(MezError::invalid_state(format!(
                "MCP stdio {} reader task did not settle within the retirement budget",
                self.label
            ))),
        }
    }
}

/// Carries Mcp Stdio Connection state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub struct McpStdioConnection {
    /// Stores the server id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    server_id: String,
    /// Stores the child value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    child: Child,
    /// Stores the stdin value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    stdin: ChildStdin,
    /// Stores the stdout rx value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    stdout_rx: Receiver<McpStdioReadEvent>,
    /// Owns the stdout protocol-reader task for this connection's lifetime.
    stdout_reader: Option<McpStdioReaderTask>,
    /// Owns the stderr drain task for this connection's lifetime.
    stderr_reader: Option<McpStdioReaderTask>,
    /// Stores the next request id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    next_request_id: u64,
}

impl McpStdioConnection {
    /// Runs the server id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// Runs the initialize operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn initialize(
        &mut self,
        client_name: &str,
        client_version: &str,
        timeout_ms: u64,
    ) -> Result<McpInitializeResponse> {
        let operation =
            mcp_initialize_operation(self.allocate_id(), client_name, client_version, timeout_ms);
        let response = self
            .send_request(
                operation.request_body(),
                operation.request_id(),
                operation.timeout_ms(),
            )
            .await?;
        Ok(operation.parse_response(&response)?)
    }

    /// Runs the send initialized notification operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn send_initialized_notification(&mut self) -> Result<()> {
        self.send_notification(&build_mcp_initialized_notification())
            .await
    }

    /// Runs the list tools operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub async fn list_tools(
        &mut self,
        cursor: Option<&str>,
        timeout_ms: u64,
    ) -> Result<McpToolsListResponse> {
        self.list_tools_page(cursor, timeout_ms)
            .await
            .map(|(response, _)| response)
    }

    /// Lists one tool page and returns its raw response byte count.
    pub async fn list_tools_page(
        &mut self,
        cursor: Option<&str>,
        timeout_ms: u64,
    ) -> Result<(McpToolsListResponse, usize)> {
        let operation = mcp_tools_list_operation(self.allocate_id(), cursor, timeout_ms);
        let response = self
            .send_request(
                operation.request_body(),
                operation.request_id(),
                operation.timeout_ms(),
            )
            .await?;
        let response_bytes = response.len();
        Ok((operation.parse_response(&response)?, response_bytes))
    }

    /// Runs the call tool operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn call_tool(&mut self, plan: &McpToolCallPlan) -> Result<McpToolCallResponse> {
        let operation = mcp_tools_call_operation(self.allocate_id(), plan)?;
        let response = self
            .send_request(
                operation.request_body(),
                operation.request_id(),
                operation.timeout_ms(),
            )
            .await?;
        Ok(operation.parse_response(&response)?)
    }

    /// Runs the send request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn send_request(
        &mut self,
        request: &str,
        expected_id: u64,
        timeout_ms: u64,
    ) -> Result<String> {
        let server_id = self.server_id.clone();
        let child = &mut self.child;
        let stdin = &mut self.stdin;
        let stdout_rx = &mut self.stdout_rx;
        write_stdio_message(stdin, request).await?;
        read_response(
            server_id.as_str(),
            child,
            stdout_rx,
            expected_id,
            timeout_ms,
        )
        .await
    }

    /// Runs the send notification operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn send_notification(&mut self, notification: &str) -> Result<()> {
        write_stdio_message(&mut self.stdin, notification).await
    }

    /// Cancels this connection's transport and observes bounded local cleanup.
    ///
    /// The method requests cancellation of both retained reader tasks, kills the
    /// direct child, and waits within a fixed deadline for every reader task and
    /// the direct child to retire. It reports cleanup uncertainty instead of a
    /// clean retirement when those bounded waits expire.
    ///
    /// Retirement only releases local transport resources. It never re-sends or
    /// replays an MCP request, never clears approvals or schema fences, and never
    /// signals descendants by guessed PID or process group. A call that was
    /// already written to the server remains an outcome of unknown remote effect;
    /// callers must settle that uncertainty separately instead of retrying here.
    pub async fn retire(mut self) -> Result<()> {
        let server_id = self.server_id.clone();
        let deadline = tokio::time::Instant::now() + STDIO_RETIREMENT_SETTLEMENT_TIMEOUT;
        let stdout_reader = self.stdout_reader.take();
        let stderr_reader = self.stderr_reader.take();
        if let Some(reader) = &stdout_reader {
            reader.cancel();
        }
        if let Some(reader) = &stderr_reader {
            reader.cancel();
        }
        let kill_request = self.child.start_kill();

        // Every safe cleanup stage runs even when an earlier stage is slow or
        // fails, so the reported uncertainty names all unresolved resources
        // instead of hiding later ones behind the first error.
        let mut unresolved = Vec::new();
        if let Some(reader) = stdout_reader
            && let Err(error) = reader.settle(deadline).await
        {
            unresolved.push(format!("stdout reader: {}", error.message()));
        }
        if let Some(reader) = stderr_reader
            && let Err(error) = reader.settle(deadline).await
        {
            unresolved.push(format!("stderr reader: {}", error.message()));
        }
        let child_failure = match tokio::time::timeout_at(deadline, self.child.wait()).await {
            Ok(Ok(_)) => None,
            Ok(Err(wait_error)) => Some(format!("direct child could not be reaped: {wait_error}")),
            Err(_) => Some("direct child did not exit within the retirement budget".to_string()),
        };
        if let Some(failure) = child_failure {
            match kill_request {
                Ok(()) => unresolved.push(failure),
                Err(kill_error) => unresolved.push(format!(
                    "{failure} (kill request also failed: {kill_error})"
                )),
            }
        }

        if unresolved.is_empty() {
            Ok(())
        } else {
            Err(MezError::invalid_state(format!(
                "MCP stdio server `{server_id}` retirement reported cleanup uncertainty: {}",
                unresolved.join("; ")
            )))
        }
    }

    /// Runs the allocate id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn allocate_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }
}

impl Drop for McpStdioConnection {
    /// Runs the drop operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn drop(&mut self) {
        if let Some(reader) = self.stdout_reader.take() {
            reader.cancel();
        }
        if let Some(reader) = self.stderr_reader.take() {
            reader.cancel();
        }
        let _ = self.child.start_kill();
    }
}

/// Runs the spawn stdio mcp connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn spawn_stdio_mcp_connection(
    plan: &McpStartupPlan,
    environment: &BTreeMap<String, String>,
) -> Result<McpStdioConnection> {
    spawn_stdio_mcp_connection_with_limit(plan, environment, DEFAULT_MCP_MAX_MESSAGE_BYTES).await
}

/// Runs the spawn stdio mcp connection with limit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn spawn_stdio_mcp_connection_with_limit(
    plan: &McpStartupPlan,
    environment: &BTreeMap<String, String>,
    max_message_bytes: usize,
) -> Result<McpStdioConnection> {
    if max_message_bytes == 0 {
        return Err(MezError::invalid_args(
            "MCP stdio max message bytes must be non-zero",
        ));
    }
    let McpStartupTransportPlan::Stdio {
        command,
        args,
        environment: mcp_environment,
        cwd,
    } = &plan.transport
    else {
        return Err(MezError::invalid_args(
            "MCP startup plan does not use stdio transport",
        ));
    };

    let mut command_builder = Command::new(command);
    command_builder
        .args(args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if !mcp_environment.set.contains_key("PATH")
        && !mcp_environment.pass.iter().any(|name| name == "PATH")
        && let Some(path) = environment
            .get("PATH")
            .cloned()
            .or_else(|| std::env::var("PATH").ok())
    {
        command_builder.env("PATH", path);
    }
    command_builder.envs(mcp_environment.set.iter());
    for name in &mcp_environment.pass {
        if let Some(value) = environment.get(name) {
            command_builder.env(name, value);
        }
    }
    if let Some(cwd) = cwd {
        command_builder.current_dir(cwd);
    }

    let mut child = command_builder.spawn().map_err(|error| {
        MezError::new(
            crate::error::MezErrorKind::Io,
            format!(
                "failed to spawn MCP stdio server `{}`: {error}",
                plan.server_id
            ),
        )
    })?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| MezError::invalid_state("MCP stdio server stdin was not captured"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| MezError::invalid_state("MCP stdio server stdout was not captured"))?;
    let stderr_reader = child.stderr.take().map(drain_stdio_log);
    let (stdout_tx, stdout_rx) = mpsc::channel(64);
    let stdout_reader = spawn_stdio_reader(stdout, max_message_bytes, stdout_tx);

    // Construct the owner before the startup liveness check so a child that
    // exits during startup cannot strand either reader task: returning that
    // error drops this connection, and `Drop` cancels both readers.
    let mut connection = McpStdioConnection {
        server_id: plan.server_id.clone(),
        child,
        stdin,
        stdout_rx,
        stdout_reader: Some(stdout_reader),
        stderr_reader,
        next_request_id: 1,
    };
    reject_if_stdio_child_exited_during_startup(&mut connection.child, &plan.server_id).await?;

    Ok(connection)
}

/// Combines one primary MCP discovery failure with retirement uncertainty.
///
/// The primary error kind and message stay authoritative; explicitly observed
/// local cleanup uncertainty is appended so a handshake or schema failure does
/// not hide unresolved reader-task or direct-child settlement.
pub(crate) fn combine_mcp_discovery_retirement_error(
    primary: MezError,
    retirement: MezError,
) -> MezError {
    MezError::new(
        primary.kind(),
        format!(
            "{}; transport retirement also reported cleanup uncertainty: {}",
            primary.message(),
            retirement.message()
        ),
    )
}

/// Runs the discover stdio mcp server operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub async fn discover_stdio_mcp_server(
    plan: &McpStartupPlan,
    environment: &BTreeMap<String, String>,
    client_name: &str,
    client_version: &str,
) -> Result<McpStdioDiscovery> {
    let mut connection = spawn_stdio_mcp_connection(plan, environment).await?;
    let discovery = discover_stdio_mcp_server_with_connection(
        &mut connection,
        plan,
        client_name,
        client_version,
    )
    .await;
    match (discovery, connection.retire().await) {
        (Ok(discovery), Ok(())) => Ok(discovery),
        (Ok(_), Err(retirement_error)) => Err(retirement_error),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(retirement_error)) => Err(combine_mcp_discovery_retirement_error(
            error,
            retirement_error,
        )),
    }
}

/// Runs one discovery exchange against an already spawned stdio connection.
///
/// The caller retains ownership and retires the connection exactly once after
/// the exchange, so every paginated `tools/list` page runs against the same
/// transport and reader tasks remain owned for the whole handshake.
#[cfg(test)]
async fn discover_stdio_mcp_server_with_connection(
    connection: &mut McpStdioConnection,
    plan: &McpStartupPlan,
    client_name: &str,
    client_version: &str,
) -> Result<McpStdioDiscovery> {
    let initialize = connection
        .initialize(client_name, client_version, plan.timeout_ms)
        .await?;
    connection.send_initialized_notification().await?;
    let mut tools = Vec::new();
    if initialize.supports_tools {
        tools = tokio::time::timeout(Duration::from_millis(plan.timeout_ms), async {
            let mut tools = Vec::new();
            let mut cursor = None;
            let mut pagination = McpToolListPagination::default();
            let mut budget = McpToolDiscoveryBudget::default();
            loop {
                let (response, response_bytes) = connection
                    .list_tools_page(cursor.as_deref(), plan.timeout_ms)
                    .await?;
                budget.accept_page(&plan.server_id, response_bytes, &response.tools)?;
                tools.extend(response.tools);
                let Some(next_cursor) =
                    pagination.advance(&plan.server_id, response.next_cursor)?
                else {
                    break;
                };
                cursor = Some(next_cursor);
            }
            Ok::<_, MezError>(tools)
        })
        .await
        .map_err(|_| {
            MezError::invalid_state(format!(
                "MCP server `{}` exceeded the aggregate tools/list deadline of {} ms",
                plan.server_id, plan.timeout_ms
            ))
        })??;
    }
    Ok(McpStdioDiscovery { initialize, tools })
}

/// Runs the discover stdio mcp server into registry operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub async fn discover_stdio_mcp_server_into_registry(
    registry: &mut McpRegistry,
    server_id: &str,
    environment: &BTreeMap<String, String>,
    client_name: &str,
    client_version: &str,
) -> Result<McpStdioDiscovery> {
    let checked_at = super::current_mcp_unix_seconds();
    let plan = registry.startup_plan(server_id, environment, checked_at)?;
    match discover_stdio_mcp_server(&plan, environment, client_name, client_version).await {
        Ok(discovery) => {
            registry.mark_available_from_discovery(
                server_id,
                discovery.tools.clone(),
                discovery.initialize.instructions.as_deref(),
                checked_at,
            )?;
            Ok(discovery)
        }
        Err(error) => {
            let reason = error.message().to_string();
            let _ = registry.blacklist_for_session(server_id, reason, checked_at);
            Err(error)
        }
    }
}

/// Runs the read response operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn read_response(
    server_id: &str,
    child: &mut Child,
    stdout_rx: &mut Receiver<McpStdioReadEvent>,
    expected_id: u64,
    timeout_ms: u64,
) -> Result<String> {
    let timeout = Duration::from_millis(timeout_ms);
    let deadline = tokio::time::Instant::now() + timeout;
    let expected = expected_id.to_string();
    loop {
        let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
            return Err(MezError::invalid_state(format!(
                "MCP stdio server `{server_id}` timed out waiting for response id {expected_id}",
            )));
        };
        tokio::select! {
            biased;
            status = child.wait() => {
                let status = status?;
                return Err(MezError::invalid_state(format!(
                    "MCP stdio server `{server_id}` exited before response id {expected_id} with status {status}",
                )));
            }
            event = stdout_rx.recv() => match event {
                Some(McpStdioReadEvent::Message(message)) => {
                if json_id_matches(&message, expected.as_str()) {
                    return Ok(message);
                }
            }
                Some(McpStdioReadEvent::ProtocolError(message)) => {
                    return Err(MezError::invalid_state(message));
                }
                None => {
                    return Err(MezError::invalid_state(format!(
                        "MCP stdio server `{server_id}` stdout closed before response id {expected_id}",
                    )));
                }
            },
            _ = tokio::time::sleep(remaining) => {
                return Err(MezError::invalid_state(format!(
                    "MCP stdio server `{server_id}` timed out waiting for response id {expected_id}",
                )));
            }
        }
    }
}

/// Runs the reject if stdio child exited during startup operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn reject_if_stdio_child_exited_during_startup(
    child: &mut Child,
    server_id: &str,
) -> Result<()> {
    tokio::select! {
        biased;
        status = child.wait() => {
            let status = status?;
            Err(MezError::invalid_state(format!(
                "MCP stdio server `{server_id}` exited during startup with status {status}",
            )))
        }
        _ = tokio::task::yield_now() => Ok(()),
    }
}

/// Runs the write stdio message operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn write_stdio_message(stdin: &mut ChildStdin, message: &str) -> Result<()> {
    let message = message.trim_end();
    if message.is_empty() {
        return Err(MezError::invalid_args(
            "MCP stdio message must not be empty",
        ));
    }
    if message.contains('\n') || message.contains('\r') {
        return Err(MezError::invalid_args(
            "MCP stdio messages must be newline-delimited and contain no embedded newlines",
        ));
    }
    stdin.write_all(message.as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    Ok(())
}

/// Spawns the bounded stdout protocol reader and returns its owned task.
///
/// The caller retains the returned task for the connection lifetime so
/// cancellation can destroy the reader without waiting for server or
/// descendant EOF. Message parsing, the configured byte limit, and the bounded
/// channel backpressure are unchanged.
fn spawn_stdio_reader<R>(
    reader: R,
    max_message_bytes: usize,
    sender: Sender<McpStdioReadEvent>,
) -> McpStdioReaderTask
where
    R: AsyncRead + Send + Unpin + 'static,
{
    McpStdioReaderTask::new(
        "stdout",
        tokio::spawn(async move {
            let mut reader = reader;
            loop {
                match read_bounded_protocol_line_async(&mut reader, max_message_bytes).await {
                    Ok(Some(message)) => {
                        if sender
                            .send(McpStdioReadEvent::Message(message))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = sender
                            .send(McpStdioReadEvent::ProtocolError(
                                error.message().to_string(),
                            ))
                            .await;
                        break;
                    }
                }
            }
        }),
    )
}

/// Runs the read bounded protocol line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) fn read_bounded_protocol_line<R: Read>(
    reader: &mut R,
    max_message_bytes: usize,
) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) if bytes.is_empty() => return Ok(None),
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                if bytes.len() >= max_message_bytes {
                    return Err(MezError::invalid_state(
                        "MCP stdio server emitted a message larger than the configured limit",
                    ));
                }
                bytes.push(byte[0]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| MezError::invalid_state("MCP stdio server emitted non-UTF-8 output"))
}

/// Spawns the stderr discard reader and returns its owned task.
///
/// Stderr remains drained and discarded exactly as before; retaining the task
/// lets the connection cancel it when the transport retires even if a
/// descendant keeps the stderr writer open.
fn drain_stdio_log<R>(mut reader: R) -> McpStdioReaderTask
where
    R: AsyncRead + Send + Unpin + 'static,
{
    McpStdioReaderTask::new(
        "stderr",
        tokio::spawn(async move {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }),
    )
}

/// Runs the read bounded protocol line async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn read_bounded_protocol_line_async<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_message_bytes: usize,
) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte).await {
            Ok(0) if bytes.is_empty() => return Ok(None),
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                if bytes.len() >= max_message_bytes {
                    return Err(MezError::invalid_state(
                        "MCP stdio server emitted a message larger than the configured limit",
                    ));
                }
                bytes.push(byte[0]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| MezError::invalid_state("MCP stdio server emitted non-UTF-8 output"))
}
