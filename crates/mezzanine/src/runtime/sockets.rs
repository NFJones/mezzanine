//! Runtime Sockets implementation.
//!
//! This module owns the runtime sockets boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

#[cfg(test)]
use super::DEFAULT_PANE_TERM;
use super::{
    AsRawFd, AuxiliarySocketKind, BorrowedFd, Component, DirBuilder, DirBuilderExt, FileTypeExt,
    MEZ_ENV_FIELD_SEPARATOR, MetadataExt, MezError, OsString, PaneEnvironment, PaneId, Path,
    PathBuf, PermissionsExt, RawFd, Result, RuntimeEnv, RuntimeLifecycleState,
    RuntimeRegistryUpdatePlan, SessionId, SessionRegistry, SocketDirectory, SocketDirectorySource,
    UnixListener, UnixStream, WindowId, fs, geteuid, socket_peercred,
};
#[cfg(test)]
use super::{
    ControlConnectionState, ControlIdempotencyCache, Read, RuntimeSessionService, Session, Write,
    handle_control_frames_for_connection,
};

// Socket directories, binding, peer credentials, and listener helpers.

impl RuntimeLifecycleState {
    /// Runs the from session state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn from_session_state(state: mez_mux::session::SessionState) -> Self {
        match state {
            mez_mux::session::SessionState::Running => Self::Running,
            mez_mux::session::SessionState::Detached => Self::Detached,
            mez_mux::session::SessionState::Empty => Self::Killed,
            mez_mux::session::SessionState::Stopping => Self::Stopping,
            mez_mux::session::SessionState::Failed => Self::Failed,
        }
    }
}

/// Runs the default socket directory operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn default_socket_directory(env: &RuntimeEnv) -> Result<SocketDirectory> {
    if let Some(path) = non_empty_path(&env.mez_tmpdir) {
        ensure_absolute(path)?;
        return Ok(SocketDirectory {
            path: path.join(format!("mez-{}", env.uid)),
            source: SocketDirectorySource::MezTmpdir,
        });
    }

    if let Some(path) = non_empty_path(&env.xdg_runtime_dir) {
        ensure_absolute(path)?;
        return Ok(SocketDirectory {
            path: path.join("mez"),
            source: SocketDirectorySource::XdgRuntimeDir,
        });
    }

    Ok(SocketDirectory {
        path: PathBuf::from(format!("/tmp/mez-{}", env.uid)),
        source: SocketDirectorySource::Tmp,
    })
}

/// Runs the ensure private socket directory operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn ensure_private_socket_directory(path: &Path, owner_uid: u32) -> Result<()> {
    ensure_absolute(path)?;
    ensure_no_mez_separator(path)?;

    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_socket_directory(path, owner_uid, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            builder.mode(0o700);
            builder.create(path)?;
            let metadata = fs::symlink_metadata(path)?;
            validate_private_socket_directory(path, owner_uid, &metadata)
        }
        Err(error) => Err(error.into()),
    }
}

/// Runs the socket path for name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn socket_path_for_name(directory: &Path, name: &str) -> Result<PathBuf> {
    ensure_absolute(directory)?;
    validate_socket_name(name)?;
    Ok(directory.join(name))
}

/// Runs the auxiliary socket path for control socket operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn auxiliary_socket_path_for_control_socket(
    control_socket: &Path,
    kind: AuxiliarySocketKind,
) -> Result<PathBuf> {
    ensure_absolute(control_socket)?;
    ensure_no_mez_separator(control_socket)?;
    let directory = control_socket.parent().ok_or_else(|| {
        MezError::invalid_args("control socket path must have a parent directory")
    })?;
    let file_name = control_socket
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            MezError::invalid_args(
                "control socket filename must be valid UTF-8 to derive auxiliary sockets",
            )
        })?;
    let stem = file_name
        .strip_suffix(".sock")
        .filter(|candidate| !candidate.is_empty())
        .unwrap_or(file_name);
    let suffix = match kind {
        AuxiliarySocketKind::Message => "message",
        AuxiliarySocketKind::Event => "event",
    };
    socket_path_for_name(directory, &format!("{stem}.{suffix}.sock"))
}

/// Runs the bind control socket operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn bind_control_socket(path: &Path, owner_uid: u32) -> Result<UnixListener> {
    let directory = path
        .parent()
        .ok_or_else(|| MezError::invalid_args("socket path must have a parent directory"))?;
    ensure_private_socket_directory(directory, owner_uid)?;
    prepare_socket_path_for_bind(path, owner_uid)?;
    let listener = UnixListener::bind(path)?;
    set_private_socket_permissions(path)?;
    Ok(listener)
}

/// Runs the prepare socket path for bind operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn prepare_socket_path_for_bind(path: &Path, owner_uid: u32) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };

    if metadata.uid() != owner_uid {
        return Err(MezError::conflict(format!(
            "socket path is not owned by the current user and will not be replaced: {}",
            path.display()
        )));
    }
    if !metadata.file_type().is_socket() {
        return Err(MezError::conflict(format!(
            "socket path already exists and is not a socket: {}",
            path.display()
        )));
    }

    match UnixStream::connect(path) {
        Ok(stream) => match unix_peer_uid(stream.as_raw_fd()) {
            Ok(peer_uid) if peer_uid == owner_uid => Err(MezError::conflict(format!(
                "socket path is already served by the current user: {}",
                path.display()
            ))),
            Ok(_) => remove_stale_socket_path(path),
            Err(error) => Err(MezError::conflict(format!(
                "socket path already exists and could not be authenticated: {} ({})",
                path.display(),
                error.message()
            ))),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            remove_stale_socket_path(path)
        }
        Err(error) => Err(MezError::conflict(format!(
            "socket path already exists and staleness could not be proven: {} ({error})",
            path.display()
        ))),
    }
}

/// Runs the remove stale socket path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn remove_stale_socket_path(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Removes stale Mezzanine socket files from a private runtime directory.
///
/// Only current-user-owned Unix socket files are eligible. Live same-user
/// sockets are preserved; refused sockets are removed. Entries that are not
/// socket files are ignored so arbitrary files in the runtime directory are not
/// treated as cleanup targets.
///
/// # Parameters
/// - `directory`: The Mezzanine runtime socket directory to scan.
/// - `owner_uid`: The user id that must own removable socket files.
pub fn prune_stale_socket_files_in_directory(directory: &Path, owner_uid: u32) -> Result<usize> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(MezError::conflict(format!(
                "socket directory path exists and is not a directory: {}",
                directory.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    }
    ensure_private_socket_directory(directory, owner_uid)?;
    let mut removed = 0usize;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("sock") {
            continue;
        }
        if remove_stale_socket_file_if_unserved(&path, owner_uid)? {
            removed = removed.saturating_add(1);
        }
    }
    Ok(removed)
}

/// Removes one stale socket file when no live current-user server owns it.
///
/// # Parameters
/// - `path`: The candidate Unix socket path.
/// - `owner_uid`: The user id that must own removable socket files.
pub fn remove_stale_socket_file_if_unserved(path: &Path, owner_uid: u32) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if metadata.uid() != owner_uid || !metadata.file_type().is_socket() {
        return Ok(false);
    }
    for attempt in 0..2 {
        match UnixStream::connect(path) {
            Ok(stream) => match unix_peer_uid(stream.as_raw_fd()) {
                Ok(peer_uid) if peer_uid == owner_uid => return Ok(false),
                Ok(_) => {
                    remove_stale_socket_path(path)?;
                    return Ok(true);
                }
                Err(_) if attempt == 0 => continue,
                Err(_) => return Ok(false),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                remove_stale_socket_path(path)?;
                return Ok(true);
            }
            Err(_) if attempt == 0 => continue,
            Err(_) => return Ok(false),
        }
    }
    unreachable!("bounded stale-socket retry loop always returns")
}

/// Runs the apply registry update operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn apply_registry_update(
    registry: &SessionRegistry,
    update: &RuntimeRegistryUpdatePlan,
) -> Result<bool> {
    match update {
        RuntimeRegistryUpdatePlan::Upsert(record) => {
            registry.upsert(record.clone())?;
            Ok(true)
        }
        RuntimeRegistryUpdatePlan::Remove { session_id } => registry.remove(session_id),
    }
}

/// Runs the apply registry update async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn apply_registry_update_async(
    registry: &SessionRegistry,
    update: &RuntimeRegistryUpdatePlan,
) -> Result<bool> {
    match update {
        RuntimeRegistryUpdatePlan::Upsert(record) => {
            registry.upsert_async(record.clone()).await?;
            Ok(true)
        }
        RuntimeRegistryUpdatePlan::Remove { session_id } => registry.remove_async(session_id).await,
    }
}

/// Runs the authorize unix peer uid operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn authorize_unix_peer_uid(peer_uid: u32, owner_uid: u32) -> Result<()> {
    if peer_uid == owner_uid {
        Ok(())
    } else {
        Err(MezError::forbidden(
            "Unix control peer uid does not match the session owner",
        ))
    }
}

/// Runs the current effective uid operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn current_effective_uid() -> u32 {
    effective_uid()
}

/// Runs the authorize unix peer raw fd operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn authorize_unix_peer_raw_fd(raw_fd: RawFd, owner_uid: u32) -> Result<()> {
    authorize_unix_peer_uid(unix_peer_uid(raw_fd)?, owner_uid)
}

/// Runs the authorize unix peer operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub fn authorize_unix_peer(stream: &UnixStream, owner_uid: u32) -> Result<()> {
    authorize_unix_peer_raw_fd(stream.as_raw_fd(), owner_uid)
}

/// Runs the serve control connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "test-only adapter retained for focused boundary coverage"
)]
pub fn serve_control_connection(
    stream: &mut UnixStream,
    max_content_length: usize,
    session: &mut Session,
    idempotency: &mut ControlIdempotencyCache,
) -> Result<usize> {
    authorize_unix_peer(stream, effective_uid())?;
    let mut input = vec![0; max_content_length.saturating_add(1024)];
    let read = stream.read(&mut input)?;
    if read == 0 {
        return Ok(0);
    }
    input.truncate(read);
    let mut connection = ControlConnectionState::new(true, true);
    let (responses, consumed) = handle_control_frames_for_connection(
        &input,
        max_content_length,
        session,
        &mut connection,
        idempotency,
    )?;
    stream.write_all(&responses)?;
    stream.flush()?;
    Ok(consumed)
}

/// Runs the serve runtime control connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "test-only adapter retained for focused boundary coverage"
)]
pub fn serve_runtime_control_connection(
    stream: &mut UnixStream,
    max_content_length: usize,
    service: &mut RuntimeSessionService,
) -> Result<usize> {
    authorize_unix_peer(stream, effective_uid())?;
    let mut connection = ControlConnectionState::new(true, true);
    serve_runtime_control_connection_with_state(
        stream,
        max_content_length,
        service,
        &mut connection,
    )
}

/// Runs the serve runtime control connection with state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "test-only adapter retained for focused boundary coverage"
)]
pub fn serve_runtime_control_connection_with_state(
    stream: &mut UnixStream,
    max_content_length: usize,
    service: &mut RuntimeSessionService,
    connection: &mut ControlConnectionState,
) -> Result<usize> {
    authorize_unix_peer(stream, effective_uid())?;
    let mut input = vec![0; max_content_length.saturating_add(1024)];
    let read = stream.read(&mut input)?;
    if read == 0 {
        return Ok(0);
    }
    input.truncate(read);
    let (responses, consumed) =
        service.handle_control_input_for_connection(&input, max_content_length, connection)?;
    stream.write_all(&responses)?;
    stream.flush()?;
    Ok(consumed)
}

/// Runs the pane environment operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub fn pane_environment(
    socket_path: &Path,
    session_id: &SessionId,
    window_id: &WindowId,
    pane_id: &PaneId,
) -> Result<PaneEnvironment> {
    pane_environment_with_term(
        socket_path,
        session_id,
        window_id,
        pane_id,
        DEFAULT_PANE_TERM,
    )
}

/// Runs the pane environment with term operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn pane_environment_with_term(
    socket_path: &Path,
    session_id: &SessionId,
    window_id: &WindowId,
    pane_id: &PaneId,
    term: &str,
) -> Result<PaneEnvironment> {
    ensure_absolute(socket_path)?;
    ensure_no_mez_separator(socket_path)?;
    if term.trim().is_empty() || term.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(MezError::invalid_args("pane TERM value must be printable"));
    }

    let socket = socket_path.to_string_lossy();
    let fields = [
        socket.as_ref().to_string(),
        format!("session={session_id}"),
        format!("window={window_id}"),
        format!("pane={pane_id}"),
        "protocol=mez-control/1".to_string(),
    ];
    let separator = MEZ_ENV_FIELD_SEPARATOR.to_string();

    Ok(PaneEnvironment {
        mez: fields.join(&separator),
        session: session_id.to_string(),
        window: window_id.to_string(),
        pane: pane_id.to_string(),
        term: term.to_string(),
    })
}
/// Runs the non empty path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn non_empty_path(value: &Option<OsString>) -> Option<&Path> {
    value.as_ref().and_then(|value| {
        if value.is_empty() {
            None
        } else {
            Some(Path::new(value))
        }
    })
}

/// Runs the ensure absolute operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn ensure_absolute(path: &Path) -> Result<()> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(MezError::invalid_args(format!(
            "runtime path must be absolute: {}",
            path.display()
        )))
    }
}

/// Runs the ensure no mez separator operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn ensure_no_mez_separator(path: &Path) -> Result<()> {
    if path.to_string_lossy().contains(MEZ_ENV_FIELD_SEPARATOR) {
        Err(MezError::invalid_args(
            "runtime path contains the reserved MEZ field separator",
        ))
    } else {
        Ok(())
    }
}

/// Runs the validate private socket directory operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_private_socket_directory(
    path: &Path,
    owner_uid: u32,
    metadata: &fs::Metadata,
) -> Result<()> {
    if metadata.file_type().is_symlink() {
        return Err(MezError::forbidden(format!(
            "socket directory must not be a symlink: {}",
            path.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(MezError::forbidden(format!(
            "socket path is not a directory: {}",
            path.display()
        )));
    }
    if metadata.uid() != owner_uid {
        return Err(MezError::forbidden(format!(
            "socket directory is not owned by the current user: {}",
            path.display()
        )));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(MezError::forbidden(format!(
            "socket directory grants group or other permissions: {}",
            path.display()
        )));
    }

    Ok(())
}

/// Runs the validate socket name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_socket_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(MezError::invalid_args("socket name must not be empty"));
    }
    let path = Path::new(name);
    if path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(MezError::invalid_args(
            "socket name must be a single relative path component",
        ));
    }
    ensure_no_mez_separator(path)
}

/// Runs the set private socket permissions operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn set_private_socket_permissions(path: &Path) -> Result<()> {
    match fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
            ) => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Runs the unix peer uid operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn unix_peer_uid(raw_fd: RawFd) -> Result<u32> {
    // SAFETY: callers pass a live Unix-stream descriptor and this borrow is
    // consumed immediately by the rustix socket option call.
    let borrowed_fd = unsafe { BorrowedFd::borrow_raw(raw_fd) };
    socket_peercred(borrowed_fd)
        .map(|credentials| credentials.uid.as_raw())
        .map_err(|error| std::io::Error::from(error).into())
}

/// Runs the effective uid operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn effective_uid() -> u32 {
    geteuid().as_raw()
}
