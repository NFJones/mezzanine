//! Explicit parent-authorized local OpenCode launch and acknowledged telemetry.
//! One launcher-owned root session activates after private plugin metadata. The
//! parent holds all credentials, renews while idle and polls renewal during
//! delivery. Failure ends telemetry, not the vendor; no provider/action replay or
//! historical import is performed. Remote/shared attachment is not admitted.

use super::{Args, MezError, Result, SocketSelection};
use crate::integrations::bootstrap::{
    opencode_stream::{Observation, Reader},
    pi_launch,
    pi_owner::LifecycleOwner,
    pi_renewal::{self, ActiveLease},
    pi_transport::CapabilityTransport,
};
use std::{ffi::OsString, path::PathBuf, process::Stdio, time::Duration};
use tokio::sync::watch;

/// Explicit local executable/pane and optional exact existing vendor session.
#[derive(Debug, Clone, Args)]
pub(super) struct OpenCodeCliArgs {
    /// Absolute installed executable; no vendor installation/probe is implicit.
    #[arg(long)]
    executable: PathBuf,
    /// Exact local pane whose live root owns the private launch.
    #[arg(long)]
    pane: String,
    /// Existing root session to resume, otherwise first freshly created root.
    #[arg(long)]
    session: Option<String>,
    /// Observed version is inert metadata, not a compatibility gate.
    #[arg(long, default_value = "best-effort")]
    vendor_version: String,
    /// Literal TUI options only; no remote attach, implicit continue or fork.
    #[arg(last = true)]
    arguments: Vec<OsString>,
}

/// Payload-free diagnostics do not echo credentials or callback content.
fn unavailable() -> MezError {
    MezError::invalid_state("OpenCode telemetry unavailable")
}
/// Validates immutable root binding and prevents hidden session/server switches.
fn validate(args: &OpenCodeCliArgs) -> Result<()> {
    if !args.executable.is_absolute()
        || args.pane.is_empty()
        || args.pane.len() > 64
        || args.pane.chars().any(char::is_control)
        || args.vendor_version.is_empty()
        || args.vendor_version.len() > 128
        || args.vendor_version.chars().any(char::is_control)
        || args.session.as_ref().is_some_and(|id| {
            id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
        })
    {
        return Err(unavailable());
    }
    for arg in &args.arguments {
        let value = arg.to_string_lossy();
        let allowed = matches!(
            value.as_ref(),
            "--pure" | "--mini" | "--no-replay" | "--print-logs"
        ) || value.split_once('=').is_some_and(|(key, body)| {
            !body.is_empty()
                && body.len() <= 1024
                && !body.chars().any(char::is_control)
                && match key {
                    "--model" | "--agent" => true,
                    "--log-level" => matches!(body, "DEBUG" | "INFO" | "WARN" | "ERROR"),
                    "--replay-limit" => body.parse::<u32>().is_ok(),
                    _ => false,
                }
        });
        if !allowed {
            return Err(MezError::invalid_args(
                "OpenCode launch accepts only documented literal long TUI options; select sessions with --session before --",
            ));
        }
    }
    Ok(())
}
/// Captures exact milliseconds before exec, so replayed historical completions
/// cannot recharge when a resumed session receives a fresh accounting owner.
fn now_ms() -> Result<u64> {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| unavailable())?
        .as_millis();
    u64::try_from(ms).map_err(|_| unavailable())
}

/// Authorizes one user-selected local process. Observation hints carry no token;
/// user/vendor environment and stdio are preserved except inherited MEZ routing.
pub(super) async fn run(args: OpenCodeCliArgs, selection: &SocketSelection) -> Result<u8> {
    validate(&args)?;
    let socket = super::selected_socket_path(selection);
    let grant =
        super::pi::authorize_harness(socket, &args.pane, "opencode", &args.vendor_version, None)
            .await?;
    let cutoff = now_ms()?;
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| !key.to_string_lossy().starts_with("MEZ_"))
        .collect();
    env.push(("MEZ_OPENCODE_OBSERVER_FD".into(), "3".into()));
    let mut arguments = Vec::new();
    if let Some(session) = &args.session {
        env.push(("MEZ_OPENCODE_SESSION".into(), session.into()));
        arguments.extend([OsString::from("--session"), session.into()]);
    }
    arguments.extend(args.arguments);
    let launched = pi_launch::spawn(pi_launch::LaunchSpec {
        executable: args.executable,
        directory: std::env::current_dir()?,
        arguments,
        environment: env,
        stdin: Stdio::inherit(),
        stdout: Stdio::inherit(),
        stderr: Stdio::inherit(),
    })?;
    let mut child = launched.child;
    let mut transport = None::<CapabilityTransport>;
    let retired = std::cell::Cell::new(false);
    let (stop, cancellation) = watch::channel(false);
    let result = {
        let observe = run_observer(
            launched.observer,
            socket,
            grant,
            args.session.as_deref(),
            cutoff,
            &mut transport,
            &retired,
            cancellation,
        );
        tokio::pin!(observe);
        tokio::select! {
            result=child.wait()=> { let _=tokio::time::timeout(Duration::from_millis(750),&mut observe).await;result }
            _=&mut observe=>child.wait().await,
        }
    };
    stop.send_replace(true);
    if !retired.get()
        && let Some(transport) = transport
    {
        let _ = transport.retire().await;
    }
    let result = result.map_err(|_| unavailable())?;
    use std::os::unix::process::ExitStatusExt;
    Ok(result
        .code()
        .unwrap_or_else(|| 128 + result.signal().unwrap_or(1)) as u8)
}

/// Activates only a strict matched root start; silent/disabled plugins perform
/// no registration. The parent retains transport for finite exact exit cleanup.
#[allow(
    clippy::too_many_arguments,
    reason = "immutable launch inputs and exact cleanup ownership"
)]
async fn run_observer(
    stream: tokio::net::UnixStream,
    socket: &std::path::Path,
    grant: super::pi::Grant,
    expected: Option<&str>,
    cutoff: u64,
    held: &mut Option<CapabilityTransport>,
    retired: &std::cell::Cell<bool>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let mut reader = Reader::new(stream)?;
    let first = tokio::select! { biased;()=pi_renewal::cancelled(&mut stop)=>return Ok(()),first=reader.next()=>first?.ok_or_else(unavailable)? };
    let session = first.session()?.to_string();
    if !matches!(first, Observation::Start { .. }) || expected.is_some_and(|id| id != session) {
        return Err(unavailable());
    }
    let owner = LifecycleOwner::new(&session)?;
    held.replace(CapabilityTransport::new(
        socket,
        grant.launch_token,
        grant.generation,
        &owner,
    )?);
    let transport = held.as_ref().ok_or_else(unavailable)?;
    let (status, mut lease) = watch::channel(None::<ActiveLease>);
    let renewal = pi_renewal::run(transport, "opencode", status, stop.clone());
    tokio::pin!(renewal);
    loop {
        tokio::select! { biased;
            ()=pi_renewal::cancelled(&mut stop)=>return Ok(()),
            result=&mut renewal=>return result,
            changed=lease.changed()=>{changed.map_err(|_|unavailable())?;if lease.borrow_and_update().as_ref().is_some_and(ActiveLease::is_current){break;}}
        }
    }
    let mut sequence = 0u64;
    loop {
        let item = tokio::select! { biased;
            ()=pi_renewal::cancelled(&mut stop)=>return Ok(()),
            result=&mut renewal=>return result,
            item=reader.next()=>item?,
        };
        let Some(item) = item else {
            return Ok(());
        };
        if item.session()? != session {
            return Err(unavailable());
        }
        let ending = matches!(&item,Observation::Status{state,..} if state=="retire");
        let work = async {
            match item {
                Observation::Start { .. } => Err(unavailable()),
                Observation::Unavailable { reason, .. } => {
                    let _ = reason;
                    Err(unavailable())
                }
                Observation::Status { state, .. } if state == "retire" => {
                    retired.set(true);
                    transport.retire().await
                }
                Observation::Status { state, .. } => {
                    sequence = sequence.checked_add(1).ok_or_else(unavailable)?;
                    transport.present(sequence, &state).await
                }
                Observation::Usage { message, .. } => {
                    if let Some(report) = message.report(&session, cutoff)? {
                        transport.usage(&report).await?;
                    }
                    Ok(())
                }
            }
        };
        let current = lease
            .borrow()
            .clone()
            .filter(ActiveLease::is_current)
            .ok_or_else(unavailable)?;
        tokio::pin!(work);
        tokio::select! { biased;
            ()=pi_renewal::cancelled(&mut stop)=>return Ok(()),
            result=&mut renewal=>return result,
            result=tokio::time::timeout_at(current.deadline(),&mut work)=>result.map_err(|_|unavailable())??,
        }
        if ending {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests;
