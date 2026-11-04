//! Qualified native custody before whole-session process/listener publication.
//!
//! This internal startup uses the actual private active metadata, existing
//! original store and one owned recovery worker. The old active checkpoint is
//! retained until matching opaque native results install. Generic startup never
//! enters this path. Complete canonical dedup/journals/quotas still gate normal
//! durable product activation; this entry does not resume tasks or providers.

use super::*;
use crate::host::async_runtime::mmp_custody::{
    CustodyRecoveryRequest, CustodyWorkerStatus, MmpCustodyRoot,
};
use crate::storage::mmp_custody::CustodyStoreLimits;
use sha2::{Digest, Sha256};

/// Original root retained through all later fallible setup and process creation.
pub(super) struct Prepared {
    root: MmpCustodyRoot,
    command: Option<String>,
}
impl Prepared {
    /// Starts processes only after private native/context qualification; caller
    /// retains the root through registry and actor construction before services.
    pub(super) fn start_panes(self, service: &mut RuntimeSessionService) -> Result<MmpCustodyRoot> {
        service.restart_restored_pane_processes(self.command.as_deref())?;
        Ok(self.root)
    }
}

/// Derives a private session namespace from trusted configuration, never a path
/// carried by a message or saved native tuple. Hashing bounds arbitrary ID text.
pub(super) fn root_path(config: &Path, session: &str) -> PathBuf {
    let mut hash = Sha256::new();
    hash.update(b"mez-message-custody-session-v1\0");
    hash.update(session.as_bytes());
    let key: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    config.join("mmp-custody").join(key)
}

/// Consumes the actual cold snapshot only under the factory's original session
/// and config scope. A joined failure withholds startup; no blank fallback or
/// fresh read/worker is substituted for a lost or refused original result.
pub(super) async fn prepare(
    service: &mut RuntimeSessionService,
    startup: SessionRuntimeStartup,
    config: &Path,
) -> Result<Prepared> {
    let SessionRuntimeStartup::RestoredSnapshot {
        mut payload,
        restart_command,
    } = startup
    else {
        return Err(MezError::invalid_args(
            "native custody recovery requires whole-session snapshot startup",
        ));
    };
    let checkpoint = payload
        .message_state
        .take()
        .ok_or_else(|| MezError::invalid_state("native custody original checkpoint missing"))?;
    if checkpoint.schema_version != 10 {
        return Err(MezError::invalid_state(
            "native custody requires original cold checkpoint",
        ));
    }
    service.seed_terminal_screens_from_snapshot_payload(&payload)?;
    let context = service.prepare_native_custody_context()?;
    let root = MmpCustodyRoot::recover_native(
        root_path(config, service.session().id.as_str()),
        service.session().id.to_string(),
        CustodyStoreLimits::default(),
        tokio::runtime::Handle::current(),
        CustodyRecoveryRequest {
            checkpoint,
            limits: Default::default(),
            retention_messages: 1000,
            retention_bytes: 1024 * 1024,
        },
        context,
    );
    match root.observe().await? {
        CustodyWorkerStatus::RecoveryAwaitingAdoption => (),
        _ => {
            return Err(root.failure()?.unwrap_or_else(|| {
                MezError::invalid_state("native original recovery did not qualify")
            }));
        }
    }
    let recovered = root.adopt_recovery()?;
    recovered.install_native_context(service)?;
    *service.message_service_mut() = recovered.into_service();
    Ok(Prepared {
        root,
        command: restart_command,
    })
}

/// Preserves ordinary listener construction order while native recovery defers
/// every Unix path until qualification. The artifact guard owns partial binds.
pub(super) fn bind_listeners(
    sockets: &SessionSocketPublication,
    owner: u32,
    artifacts: &mut SessionRuntimeArtifacts,
) -> Result<AsyncRuntimeDaemonListeners> {
    let control = bind_optional_listener(sockets.publish_control, &sockets.control_path, owner)?;
    if control.is_some() {
        artifacts.track_path(sockets.control_path.clone());
    }
    let message = bind_optional_path(sockets.message_path.as_deref(), owner)?;
    if message.is_some()
        && let Some(path) = &sockets.message_path
    {
        artifacts.track_path(path.clone());
    }
    let event = bind_optional_path(sockets.event_path.as_deref(), owner)?;
    if event.is_some()
        && let Some(path) = &sockets.event_path
    {
        artifacts.track_path(path.clone());
    }
    Ok(AsyncRuntimeDaemonListeners {
        control,
        message,
        event,
    })
}

#[cfg(test)]
mod tests;
