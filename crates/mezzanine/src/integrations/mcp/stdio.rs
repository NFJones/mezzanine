//! Stdio transport support for local MCP subprocesses.
//!
//! This module owns subprocess spawning, newline-delimited JSON-RPC transport,
//! bounded stdout reads, stderr draining, discovery, and registry integration.

use std::collections::BTreeMap;
#[cfg(test)]
use std::io::Read;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc::{self, Receiver, Sender};

use crate::error::{MezError, Result};

use mez_agent::mcp::{
    DEFAULT_MCP_MAX_MESSAGE_BYTES, McpInitializeResponse, McpStartupPlan, McpStartupTransportPlan,
    McpToolCallPlan, McpToolCallResponse, McpToolsListResponse, build_mcp_initialized_notification,
    json_id_matches, mcp_initialize_operation, mcp_tools_call_operation, mcp_tools_list_operation,
};
#[cfg(test)]
use mez_agent::mcp::{McpRegistry, McpStdioDiscovery, McpToolListPagination};

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
    pub async fn list_tools(
        &mut self,
        cursor: Option<&str>,
        timeout_ms: u64,
    ) -> Result<McpToolsListResponse> {
        let operation = mcp_tools_list_operation(self.allocate_id(), cursor, timeout_ms);
        let response = self
            .send_request(
                operation.request_body(),
                operation.request_id(),
                operation.timeout_ms(),
            )
            .await?;
        Ok(operation.parse_response(&response)?)
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
    if let Some(stderr) = child.stderr.take() {
        drain_stdio_log(stderr);
    }
    let (stdout_tx, stdout_rx) = mpsc::channel(64);
    spawn_stdio_reader(stdout, max_message_bytes, stdout_tx);

    reject_if_stdio_child_exited_during_startup(&mut child, &plan.server_id).await?;

    Ok(McpStdioConnection {
        server_id: plan.server_id.clone(),
        child,
        stdin,
        stdout_rx,
        next_request_id: 1,
    })
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
    let initialize = connection
        .initialize(client_name, client_version, plan.timeout_ms)
        .await?;
    connection.send_initialized_notification().await?;
    let mut tools = Vec::new();
    if initialize.supports_tools {
        let mut cursor = None;
        let mut pagination = McpToolListPagination::default();
        loop {
            let response = connection
                .list_tools(cursor.as_deref(), plan.timeout_ms)
                .await?;
            tools.extend(response.tools);
            let Some(next_cursor) = pagination.advance(&plan.server_id, response.next_cursor)?
            else {
                break;
            };
            cursor = Some(next_cursor);
        }
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
            registry.mark_available_from_discovered_tools(
                server_id,
                discovery.tools.clone(),
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

/// Runs the spawn stdio reader operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn spawn_stdio_reader<R>(reader: R, max_message_bytes: usize, sender: Sender<McpStdioReadEvent>)
where
    R: AsyncRead + Send + Unpin + 'static,
{
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
    });
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

/// Runs the drain stdio log operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn drain_stdio_log<R>(mut reader: R)
where
    R: AsyncRead + Send + Unpin + 'static,
{
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
    });
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
