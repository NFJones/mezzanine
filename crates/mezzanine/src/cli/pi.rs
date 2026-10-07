//! Explicit local Pi launch with parent-held lifecycle authority.
//!
//! This user command, unlike a vendor hook, may initialize a primary and ask
//! for an exact pane-root capability. The credential never enters child argv,
//! environment or output. Only a session hint and observation-only fd3 cross
//! exec. The child retains ordinary stdio and vendor policy; telemetry failure
//! neither kills nor relaunches it. Reload preserves sequencing; new/resume/fork
//! require ordered retirement and fresh parent-issued exact-root authority.

use std::ffi::OsString;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use secrecy::SecretString;
use zeroize::Zeroizing;

use super::{Args, MezError, Result, SocketSelection};
use crate::integrations::bootstrap::{pi_launch, pi_owner, pi_session, pi_transport};

/// Owns ordered parent reauthorization and immutable vendor-session handoff.
mod binding;

/// User-selected executable and pane. No executable/version probing or install
/// is implicit; remaining arguments are forwarded without shell expansion.
#[derive(Debug, Clone, Args)]
pub(super) struct PiCliArgs {
    /// Absolute path to the existing Pi executable.
    #[arg(long)]
    executable: PathBuf,
    /// Explicit local pane whose current root owns this launch.
    #[arg(long)]
    pane: String,
    /// Inert observed version; no minimum or exact-match requirement.
    #[arg(long, default_value = "best-effort")]
    vendor_version: String,
    /// Pi options after --; session selection is owned by this launcher.
    #[arg(last = true)]
    arguments: Vec<OsString>,
}

/// Credential-bearing acknowledgment intentionally has no Debug implementation.
#[derive(serde::Deserialize)]
pub(super) struct Grant {
    protocol: String,
    #[serde(deserialize_with = "deserialize_secret")]
    pub(super) launch_token: SecretString,
    pub(super) generation: u64,
    expires_at_unix_seconds: u64,
    lease_seconds: u64,
}

/// Exact envelope identity and bounded reads precede capability construction.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantReply {
    jsonrpc: String,
    id: String,
    result: Grant,
}

/// Moves decoded credential text immediately into zeroizing secret storage.
fn deserialize_secret<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<SecretString, D::Error> {
    use serde::Deserialize;
    String::deserialize(decoder).map(SecretString::from)
}

/// Redacted errors never expose a server response, credential or vendor argv.
fn unavailable() -> MezError {
    MezError::invalid_state("Pi launch authorization unavailable")
}

/// Reject session selectors before authorization/spawn rather than overriding
/// user intent. Disabled extensions are passed unchanged and stay disabled.
fn validate(args: &PiCliArgs) -> Result<()> {
    if !args.executable.is_absolute()
        || args.pane.is_empty()
        || args.pane.len() > 64
        || args.pane.chars().any(char::is_control)
        || args.vendor_version.is_empty()
        || args.vendor_version.len() > 128
        || args.vendor_version.chars().any(char::is_control)
    {
        return Err(MezError::invalid_args(
            "Pi launch requires an absolute executable and bounded pane/version",
        ));
    }
    for value in &args.arguments {
        let text = value.to_string_lossy();
        if [
            "--session-id",
            "--session",
            "--continue",
            "--resume",
            "-c",
            "-r",
        ]
        .iter()
        .any(|flag| text == *flag || text.starts_with(&format!("{flag}=")))
        {
            return Err(MezError::invalid_args(
                "Pi launch owns session selection; start a fresh session",
            ));
        }
    }
    Ok(())
}

/// Exchanges explicit-user authorization through a unique temporary primary,
/// never the real frontend's name. One total deadline covers authentication,
/// initialization and issuance, including slow-drip replies. Disconnect removes
/// only this command's client. Environment hints grant no authority.
async fn authorize(socket: &Path, pane: &str, version: &str) -> Result<Grant> {
    authorize_root(socket, pane, version, None).await
}

/// Explicit primary issuance narrowed to the retained predecessor pane root.
/// A witness is inert provenance, not a credential or permission escalation.
async fn authorize_root(
    socket: &Path,
    pane: &str,
    version: &str,
    root_generation: Option<u64>,
) -> Result<Grant> {
    authorize_harness(socket, pane, "pi", version, root_generation).await
}

/// Shared explicit-parent issuance for compiled local adapters only. Hook code
/// cannot invoke it; a unique primary and same-user finite exchange still gate
/// every grant. This neither probes nor enables a vendor or alters its policy.
pub(super) async fn authorize_harness(
    socket: &Path,
    pane: &str,
    harness: &str,
    version: &str,
    root_generation: Option<u64>,
) -> Result<Grant> {
    if !matches!(harness, "pi" | "opencode") {
        return Err(unavailable());
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut stream = tokio::net::UnixStream::connect(socket).await.map_err(|_| unavailable())?;
        crate::runtime::authenticated_unix_peer_uid(stream.as_raw_fd(), crate::runtime::current_effective_uid())
            .map_err(|_| unavailable())?;
        let name = super::cli_idempotency_key("pi-launch");
        let init = serde_json::json!({"jsonrpc":"2.0","id":"pi-init","method":"control/initialize","params":{
            "client_name":name,"requested_version":2,"requested_role":"primary","detach_primary_on_disconnect":true,
            "client":{"name":name,"interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}
        }}).to_string();
        let initialized = exchange(&mut stream, &init).await?;
        let initialized: serde_json::Value = serde_json::from_str(&initialized).map_err(|_| unavailable())?;
        if initialized.get("error").is_some() || initialized.get("result").is_none()
            || initialized["id"] != "pi-init" || initialized["jsonrpc"] != "2.0" {
            return Err(unavailable());
        }
        let mut params = serde_json::json!({"pane_id":pane,"harness":harness,"version":version});
        if let Some(generation) = root_generation { params["root_generation"] = generation.into(); }
        let request = serde_json::json!({"jsonrpc":"2.0","id":"cli","method":"agent/external/launch", "params":params}).to_string();
        let reply = exchange(&mut stream, &request).await?;
        let reply: GrantReply = serde_json::from_str(&reply).map_err(|_| unavailable())?;
        if reply.jsonrpc != "2.0" || reply.id != "cli" || reply.result.protocol != "external-agent/1"
            || reply.result.generation == 0
            || root_generation.is_some_and(|old| reply.result.generation <= old)
            || reply.result.expires_at_unix_seconds <= super::current_unix_seconds().map_err(|_| unavailable())?
            || reply.result.lease_seconds == 0 || reply.result.lease_seconds > 60 {
            return Err(unavailable());
        }
        Ok(reply.result)
    }).await.map_err(|_| unavailable())?
}

/// Bounded private reply storage; caller supplies the shared absolute deadline.
async fn exchange(stream: &mut tokio::net::UnixStream, body: &str) -> Result<Zeroizing<String>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream
        .write_all(&crate::control::encode_control_body(body))
        .await
        .map_err(|_| unavailable())?;
    stream.flush().await.map_err(|_| unavailable())?;
    let mut bytes = Zeroizing::new(Vec::new());
    let mut buffer = Zeroizing::new([0; 1024]);
    loop {
        let count = stream.read(&mut *buffer).await.map_err(|_| unavailable())?;
        if count == 0 || bytes.len() + count > 65536 {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Ok((body, consumed)) = crate::control::decode_control_frame(&bytes, 65536) {
            if consumed != bytes.len() {
                return Err(unavailable());
            }
            return Ok(Zeroizing::new(body));
        }
    }
}

/// Captures the invoking user's environment without inherited Mezzanine routing
/// or capability markers. Vendor credentials/settings are not modified.
fn environment(session: &str) -> Vec<(OsString, OsString)> {
    let mut values: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| !key.to_string_lossy().starts_with("MEZ_"))
        .collect();
    values.push(("MEZ_PI_OBSERVER_FD".into(), "3".into()));
    values.push(("MEZ_PI_OBSERVER_SESSION".into(), session.into()));
    values.push(("MEZ_PI_OBSERVER_PROTOCOL".into(), "2".into()));
    values
}

/// Launches after bounded parent authorization, then reaps exactly one child.
/// Missing/disabled extensions and delivery failures preserve vendor behavior.
pub(super) async fn run(args: PiCliArgs, selection: &SocketSelection) -> Result<u8> {
    validate(&args)?;
    let directory = std::env::current_dir()?;
    let socket = super::selected_socket_path(selection).clone();
    let pane = args.pane.clone();
    let version = args.vendor_version.clone();
    let grant = authorize(&socket, &pane, &version).await?;
    let session = crate::storage::token_usage::new_token_usage_event_id();
    let mut context = binding::Context::new(socket, pane, version, &session, grant)?;
    let mut arguments = vec!["--session-id".into(), session.clone().into()];
    arguments.extend(args.arguments);
    let launched = pi_launch::spawn(pi_launch::LaunchSpec {
        executable: args.executable,
        directory,
        arguments,
        environment: environment(&session),
        stdin: Stdio::inherit(),
        stdout: Stdio::inherit(),
        stderr: Stdio::inherit(),
    })?;
    supervise_binding(launched, &mut context).await
}

/// Reaps one child independently of its binding coordinator. Finite draining
/// and exact cleanup preserve observed effects without replaying provider work.
async fn supervise_binding(
    launched: pi_launch::Launched,
    context: &mut binding::Context,
) -> Result<u8> {
    let mut child = launched.child;
    let (stop, cancellation) = tokio::sync::watch::channel(false);
    let status = {
        let observer = context.run(launched.observer, cancellation);
        tokio::pin!(observer);
        tokio::select! {
            status = child.wait() => {
                let _ = tokio::time::timeout(Duration::from_millis(750), &mut observer).await;
                status
            }
            _ = &mut observer => child.wait().await,
        }
    };
    stop.send_replace(true);
    context.retire().await;
    let status = status.map_err(|_| MezError::invalid_state("Pi child wait failed"))?;
    use std::os::unix::process::ExitStatusExt;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)) as u8)
}

/// Joins telemetry and child lifetimes without detached tasks. Child completion
/// allows a finite final-fact drain; telemetry completion alone only drops the
/// observer and stops renewal. Retirement is best-effort exact-capability work,
/// not proof of remote acknowledgment; server expiry is the fallback.
#[cfg(test)]
async fn supervise(
    launched: pi_launch::Launched,
    owner: &mut pi_owner::LifecycleOwner,
    transport: &pi_transport::CapabilityTransport,
) -> Result<u8> {
    let mut child = launched.child;
    let (stop, cancellation) = tokio::sync::watch::channel(false);
    let activated = std::cell::Cell::new(false);
    let status = {
        let observer = async {
            let mut stream = launched.observer;
            let session = owner.transport_binding().0.to_string();
            let fact =
                crate::integrations::bootstrap::pi_ipc::wait_for_start(&mut stream, &session)
                    .await?;
            owner.observe(owner.observer_epoch(), &session, fact)?;
            activated.set(true);
            pi_session::run_observer(owner, transport, "pi", stream, cancellation).await
        };
        tokio::pin!(observer);
        tokio::select! {
            status = child.wait() => {
                let _ = tokio::time::timeout(Duration::from_millis(750), &mut observer).await;
                status
            }
            _ = &mut observer => child.wait().await,
        }
    };
    stop.send_replace(true);
    if activated.get() {
        let _ = transport.retire().await;
    }
    let status = status.map_err(|_| MezError::invalid_state("Pi child wait failed"))?;
    use std::os::unix::process::ExitStatusExt;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)) as u8)
}

#[cfg(test)]
mod tests;
