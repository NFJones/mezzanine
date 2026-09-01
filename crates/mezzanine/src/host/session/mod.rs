//! Reusable ownership boundary for one runtime session.
//!
//! A `SessionFactory` constructs exactly one `RuntimeSessionService` and one
//! `AsyncRuntimeSessionActor`, wires the session-scoped stores and workers, and
//! optionally publishes Unix listeners and a live registry record. The
//! resulting `SessionRuntime` is independent of CLI foreground concerns: a
//! caller may inject terminal or signal services when running it, while a
//! persistent host can retain the typed handle and route directly to the actor.
//!
//! Socket files, live registry publication, endpoint shutdown, actor shutdown,
//! and pane-process cleanup stay owned by this boundary so partial startup and
//! normal completion cannot leave session artifacts behind.

use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};

use mez_mux::layout::Size;
use mez_mux::session::Session;

use crate::config::ConfigLayer;
use crate::error::{MezError, Result};
use crate::host::async_runtime::{
    AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeDaemonConfig,
    AsyncRuntimeDaemonListeners, AsyncRuntimeService, AsyncRuntimeServiceExit,
    AsyncRuntimeSessionActor, AsyncRuntimeSessionHandle, AsyncRuntimeSupervisionReport,
    build_async_runtime_daemon_services, build_async_runtime_session_services,
    supervise_async_runtime_services,
};
use crate::runtime::x11::RuntimeX11Proxy;
use crate::runtime::{
    RuntimeEvent, RuntimeEventBatch, RuntimeIrohShutdownHandle, RuntimeLifecycleState,
    RuntimeSessionService, ShutdownEvent, bind_control_socket, build_runtime_iroh_control_service,
};
use crate::security::auth::{AuthPaths, AuthStore};
use crate::security::project::{ProjectTrustStore, default_trust_database_path};
use crate::storage::registry::SessionRegistry;
use crate::storage::snapshot::{SessionSnapshotPayload, SnapshotRepository};
use crate::storage::token_usage::TokenUsageStore;
use crate::storage::transcript::AgentTranscriptStore;

#[allow(
    dead_code,
    reason = "the persistent host consumes the completed supervisor in the next architecture phase"
)]
mod supervisor;

#[allow(
    unused_imports,
    reason = "the persistent host consumes these supervisor contracts in the next architecture phase"
)]
pub(crate) use supervisor::{SessionSupervisor, SessionSupervisorSnapshot, SessionSupervisorState};

/// Configuration layers and protected storage root used by one session.
#[derive(Debug, Clone)]
pub(crate) struct SessionRuntimeConfig {
    /// Ordered configuration layers initialized before pane startup.
    pub(crate) layers: Vec<ConfigLayer>,
    /// Primary configuration root containing session-scoped stores.
    pub(crate) root: PathBuf,
}

/// Selects initial or checkpoint-restored pane startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionRuntimeStartup {
    /// Starts the default pane, optionally with an explicit command.
    Initial {
        /// Explicit initial pane command.
        explicit_command: Option<String>,
        /// Explicit caller launch directory, or the daemon directory for compatibility.
        start_directory: Option<PathBuf>,
        /// Bounded environment overrides accepted by the local host boundary.
        environment: Option<Vec<(String, String)>>,
    },
    /// Seeds terminal state and starts fresh processes from a snapshot payload.
    RestoredSnapshot {
        /// Versioned snapshot payload used to seed the runtime.
        payload: Box<SessionSnapshotPayload>,
        /// Optional command used for every restorable pane.
        restart_command: Option<String>,
    },
}

/// Optional Unix publication owned by one session runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSocketPublication {
    /// Control-socket path retained in session metadata.
    pub(crate) control_path: PathBuf,
    /// Whether the runtime binds and serves the control socket.
    pub(crate) publish_control: bool,
    /// Optional message-protocol socket.
    pub(crate) message_path: Option<PathBuf>,
    /// Optional event-protocol socket.
    pub(crate) event_path: Option<PathBuf>,
    /// Whether to publish the session in the live registry beside the control socket.
    pub(crate) publish_registry: bool,
}

/// Per-session listener and delivery limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionRuntimeLimits {
    /// Maximum accepted control connections.
    pub(crate) max_control_connections: u64,
    /// Maximum accepted message connections.
    pub(crate) max_message_connections: u64,
    /// Maximum accepted event connections.
    pub(crate) max_event_connections: u64,
    /// Maximum event batches delivered on one connection.
    pub(crate) max_event_batches_per_connection: u64,
}

impl Default for SessionRuntimeLimits {
    fn default() -> Self {
        Self {
            max_control_connections: u64::MAX,
            max_message_connections: u64::MAX,
            max_event_connections: u64::MAX,
            max_event_batches_per_connection: u64::MAX,
        }
    }
}

/// Complete construction request for one isolated session runtime.
#[derive(Debug)]
pub(crate) struct SessionFactoryRequest {
    /// Prepared session model with a stable identity.
    pub(crate) session: Session,
    /// Owner UID accepted by local Unix listeners.
    pub(crate) owner_uid: u32,
    /// Session creation timestamp persisted to discovery metadata.
    pub(crate) created_at_unix_seconds: u64,
    /// Runtime configuration layers and storage root.
    pub(crate) config: SessionRuntimeConfig,
    /// Optional local publication paths.
    pub(crate) sockets: SessionSocketPublication,
    /// Listener and delivery limits.
    pub(crate) limits: SessionRuntimeLimits,
    /// Initial or restored startup operation.
    pub(crate) startup: SessionRuntimeStartup,
}

/// Constructs reusable one-actor session runtimes.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SessionFactory;

impl SessionFactory {
    /// Constructs and readies one isolated session runtime.
    ///
    /// The returned handle is usable immediately. Any error removes sockets
    /// and registry publication created during this attempt. Pane startup is
    /// deliberately last among fallible configuration and binding work.
    pub(crate) async fn create(request: SessionFactoryRequest) -> Result<SessionRuntime> {
        validate_request(&request)?;
        let session_id = request.session.id.to_string();
        let registry = request
            .sockets
            .publish_registry
            .then(|| registry_for_control_path(&request.sockets.control_path, request.owner_uid))
            .transpose()?;
        let mut artifacts = SessionRuntimeArtifacts::new(registry.clone(), session_id.clone());

        let control_listener = bind_optional_listener(
            request.sockets.publish_control,
            &request.sockets.control_path,
            request.owner_uid,
        )?;
        if control_listener.is_some() {
            artifacts.track_path(request.sockets.control_path.clone());
        }
        let message_listener =
            bind_optional_path(request.sockets.message_path.as_deref(), request.owner_uid)?;
        if message_listener.is_some()
            && let Some(path) = request.sockets.message_path.clone()
        {
            artifacts.track_path(path);
        }
        let event_listener =
            bind_optional_path(request.sockets.event_path.as_deref(), request.owner_uid)?;
        if event_listener.is_some()
            && let Some(path) = request.sockets.event_path.clone()
        {
            artifacts.track_path(path);
        }

        let mut service = RuntimeSessionService::with_event_log(
            request.session,
            request.sockets.control_path.clone(),
            request.created_at_unix_seconds,
            1024,
            4096,
        )?;
        if let Some(registry) = registry.clone() {
            service.set_session_registry(registry);
        }
        let config_root = request.config.root.clone();
        let snapshots = initialize_session_dependencies(
            &mut service,
            request.config,
            request.created_at_unix_seconds,
        )
        .await?;
        let x11_policy = service.configured_iroh_transport_policy()?.x11;
        service.set_applied_runtime_x11_policy(x11_policy.clone());
        let x11_proxy = if x11_policy.enabled {
            let proxy = RuntimeX11Proxy::prepare_with_policy(&config_root, x11_policy)?;
            service.set_runtime_x11_proxy(proxy.handle());
            Some(proxy)
        } else {
            None
        };
        let iroh_endpoint = service.bind_configured_iroh_endpoint().await?;
        spawn_auth_refresh_if_needed(&service);
        let daemon_config = AsyncRuntimeDaemonConfig {
            control: AsyncRuntimeControlConnectionConfig::new(1024 * 1024, request.owner_uid)?,
            snapshots: Some(snapshots.clone()),
            max_control_connections: if control_listener.is_some() {
                request.limits.max_control_connections
            } else {
                0
            },
            max_message_connections: if message_listener.is_some() {
                request.limits.max_message_connections
            } else {
                0
            },
            max_event_connections: if event_listener.is_some() {
                request.limits.max_event_connections
            } else {
                0
            },
            max_event_batches_per_connection: request.limits.max_event_batches_per_connection,
            ..AsyncRuntimeDaemonConfig::default()
        };
        validate_daemon_config_for_publication(
            &daemon_config,
            control_listener.is_some() || message_listener.is_some() || event_listener.is_some(),
        )?;

        if let Err(error) = start_session(&mut service, request.startup) {
            let _ = service.terminate_all_pane_processes();
            return Err(error);
        }
        if let Err(error) = service.persist_registry_update() {
            let _ = service.terminate_all_pane_processes();
            return Err(error);
        }
        let attached_client_size = service.session().authoritative_size;
        let (handle, mut actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default())?;
        let listeners = AsyncRuntimeDaemonListeners {
            control: control_listener,
            message: message_listener,
            event: event_listener,
        };
        let has_unix_listener =
            listeners.control.is_some() || listeners.message.is_some() || listeners.event.is_some();
        let services_result = if has_unix_listener {
            build_async_runtime_daemon_services(handle.clone(), listeners, daemon_config.clone())
        } else {
            build_async_runtime_session_services(handle.clone(), &daemon_config)
        };
        let mut services = match services_result {
            Ok(services) => services,
            Err(error) => {
                let _ = actor.terminate_owned_pane_processes();
                return Err(error);
            }
        };
        if let Some(proxy) = x11_proxy {
            services.push(build_runtime_x11_proxy_service(proxy));
        }
        let iroh_shutdown = iroh_endpoint
            .as_ref()
            .map(|endpoint| endpoint.shutdown_handle());
        let has_iroh_listener = iroh_endpoint.is_some();
        if let Some(endpoint) = iroh_endpoint {
            services.push(build_runtime_iroh_control_service(
                endpoint,
                handle.clone(),
                daemon_config.control,
                daemon_config.snapshots.clone(),
            ));
        }
        services.push(build_provider_refresh_service(handle.clone()));
        if !has_unix_listener && !has_iroh_listener {
            services.push(build_actor_lifetime_service(handle.clone()));
        }

        Ok(SessionRuntime {
            handle: SessionRuntimeHandle {
                session_id,
                actor: handle,
            },
            actor: Some(actor),
            services: Some(services),
            iroh_shutdown,
            _artifacts: artifacts,
            attached_client_size,
        })
    }
}

/// Cloneable control and lifecycle handle for one session actor.
#[derive(Debug, Clone)]
pub(crate) struct SessionRuntimeHandle {
    session_id: String,
    actor: AsyncRuntimeSessionHandle,
}

impl SessionRuntimeHandle {
    /// Stable session identity owned by this handle.
    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Underlying session actor handle used by connection and terminal adapters.
    pub(crate) fn actor(&self) -> &AsyncRuntimeSessionHandle {
        &self.actor
    }

    /// Current actor-owned lifecycle state.
    pub(crate) async fn lifecycle_state(&self) -> Result<RuntimeLifecycleState> {
        self.actor.lifecycle_state().await
    }

    /// Requests graceful teardown through the typed supervisor event boundary.
    pub(crate) async fn graceful_shutdown(&self, reason: impl Into<String>) -> Result<()> {
        self.request_supervisor_shutdown(reason, false).await
    }

    /// Forces session teardown through the typed supervisor event boundary.
    pub(crate) async fn force_shutdown(&self, reason: impl Into<String>) -> Result<()> {
        self.request_supervisor_shutdown(reason, true).await
    }

    async fn request_supervisor_shutdown(
        &self,
        reason: impl Into<String>,
        force: bool,
    ) -> Result<()> {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: reason.into(),
            force,
            failed: false,
        }));
        self.actor.submit_runtime_events(batch).await?;
        Ok(())
    }
}

/// Ready one-session runtime with actor, workers, and cleanup ownership.
pub(crate) struct SessionRuntime {
    handle: SessionRuntimeHandle,
    actor: Option<AsyncRuntimeSessionActor>,
    services: Option<Vec<AsyncRuntimeService>>,
    iroh_shutdown: Option<RuntimeIrohShutdownHandle>,
    _artifacts: SessionRuntimeArtifacts,
    attached_client_size: Size,
}

impl std::fmt::Debug for SessionRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionRuntime")
            .field("session_id", &self.handle.session_id())
            .field("attached_client_size", &self.attached_client_size)
            .finish_non_exhaustive()
    }
}

impl SessionRuntime {
    /// Returns a cloneable session lifecycle and actor handle.
    pub(crate) fn handle(&self) -> SessionRuntimeHandle {
        self.handle.clone()
    }

    /// Authoritative size captured after startup and before actor handoff.
    pub(crate) fn attached_client_size(&self) -> Size {
        self.attached_client_size
    }

    /// Runs actor and per-session services to completion.
    ///
    /// `additional_services` is intended for owner-specific adapters such as a
    /// foreground terminal. Cancellation closes Iroh first, requests actor
    /// shutdown after workers settle, terminates remaining pane processes, and
    /// finally drops the artifact guard.
    pub(crate) async fn run<C>(
        mut self,
        mut additional_services: Vec<AsyncRuntimeService>,
        cancellation: C,
    ) -> Result<SessionRuntimeCompletion>
    where
        C: Future<Output = ()>,
    {
        let mut services = self
            .services
            .take()
            .ok_or_else(|| MezError::invalid_state("session runtime services are unavailable"))?;
        services.append(&mut additional_services);
        let shutdown_handle = self.handle.actor.clone();
        let iroh_shutdown = self.iroh_shutdown.take();
        let actor = self
            .actor
            .take()
            .ok_or_else(|| MezError::invalid_state("session runtime actor is unavailable"))?;
        let daemon = async move {
            let cancellation = async move {
                cancellation.await;
                if let Some(iroh_shutdown) = iroh_shutdown {
                    let _ = iroh_shutdown.close().await;
                }
            };
            let result = supervise_async_runtime_services(services, cancellation).await;
            let _ = shutdown_handle.shutdown().await;
            result
        };
        let (supervision, mut actor_exit) = tokio::join!(daemon, actor.run());
        actor_exit.service.terminate_all_pane_processes()?;
        let supervision = supervision?;
        Ok(SessionRuntimeCompletion {
            service: actor_exit.service,
            supervision,
        })
    }
}

impl Drop for SessionRuntime {
    fn drop(&mut self) {
        if let Some(actor) = self.actor.as_mut() {
            let _ = actor.terminate_owned_pane_processes();
        }
    }
}

/// Final state returned after one session runtime has stopped and cleaned up.
#[derive(Debug)]
pub(crate) struct SessionRuntimeCompletion {
    /// Session service recovered from the stopped actor.
    pub(crate) service: RuntimeSessionService,
    /// Named service supervision report.
    pub(crate) supervision: AsyncRuntimeSupervisionReport,
}

#[derive(Debug)]
struct SessionRuntimeArtifacts {
    paths: Vec<PathBuf>,
    registry: Option<SessionRegistry>,
    session_id: String,
}

impl SessionRuntimeArtifacts {
    fn new(registry: Option<SessionRegistry>, session_id: String) -> Self {
        Self {
            paths: Vec::new(),
            registry,
            session_id,
        }
    }

    fn track_path(&mut self, path: PathBuf) {
        self.paths.push(path);
    }
}

impl Drop for SessionRuntimeArtifacts {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = fs::remove_file(path);
        }
        if let Some(registry) = &self.registry {
            let _ = registry.remove(&self.session_id);
        }
    }
}

fn validate_request(request: &SessionFactoryRequest) -> Result<()> {
    if request.sockets.publish_registry && !request.sockets.publish_control {
        return Err(MezError::invalid_args(
            "session registry publication requires a control socket listener",
        ));
    }
    if request.sockets.publish_control && request.limits.max_control_connections == 0 {
        return Err(MezError::invalid_args(
            "published control socket requires a positive connection limit",
        ));
    }
    if request.sockets.message_path.is_some() && request.limits.max_message_connections == 0 {
        return Err(MezError::invalid_args(
            "published message socket requires a positive connection limit",
        ));
    }
    if request.sockets.event_path.is_some() && request.limits.max_event_connections == 0 {
        return Err(MezError::invalid_args(
            "published event socket requires a positive connection limit",
        ));
    }
    Ok(())
}

fn registry_for_control_path(path: &Path, owner_uid: u32) -> Result<SessionRegistry> {
    let root = path.parent().map(PathBuf::from).ok_or_else(|| {
        MezError::invalid_args("control socket path must have a parent directory")
    })?;
    Ok(SessionRegistry::new(root, owner_uid))
}

fn bind_optional_listener(
    enabled: bool,
    path: &Path,
    owner_uid: u32,
) -> Result<Option<tokio::net::UnixListener>> {
    enabled.then(|| bind_listener(path, owner_uid)).transpose()
}

fn bind_optional_path(
    path: Option<&Path>,
    owner_uid: u32,
) -> Result<Option<tokio::net::UnixListener>> {
    path.map(|path| bind_listener(path, owner_uid)).transpose()
}

fn bind_listener(path: &Path, owner_uid: u32) -> Result<tokio::net::UnixListener> {
    let listener = bind_control_socket(path, owner_uid)?;
    listener.set_nonblocking(true)?;
    tokio::net::UnixListener::from_std(listener).map_err(Into::into)
}

async fn initialize_session_dependencies(
    service: &mut RuntimeSessionService,
    config: SessionRuntimeConfig,
    created_at_unix_seconds: u64,
) -> Result<SnapshotRepository> {
    service.set_config_root(config.root.clone());
    let token_usage_store = TokenUsageStore::under_config_root(&config.root);
    token_usage_store.initialize(created_at_unix_seconds)?;
    service.set_token_usage_store(token_usage_store);
    let transcript_store = AgentTranscriptStore::under_config_root(config.root.clone());
    transcript_store.initialize(created_at_unix_seconds)?;
    service.set_agent_transcript_store(transcript_store);
    service.set_auth_store(AuthStore::new(AuthPaths::under_config_root(&config.root)));
    let trust_path = default_trust_database_path(&config.root);
    service.set_project_trust_store(
        ProjectTrustStore::load_from_file(&trust_path)?,
        Some(trust_path),
    );
    let snapshots = SnapshotRepository::new(config.root.join("layouts"));
    service.set_snapshot_repository(snapshots.clone());
    service
        .initialize_config_layers_async(config.layers)
        .await?;
    Ok(snapshots)
}

fn spawn_auth_refresh_if_needed(service: &RuntimeSessionService) {
    let Some(auth_store) = service.auth_store().cloned() else {
        return;
    };
    let leeway_seconds = service.provider_auth_refresh_leeway_seconds();
    let _ = spawn_auth_store_refresh_if_needed(auth_store, leeway_seconds);
}

/// Starts a best-effort provider credential refresh only when its leeway requires one.
///
/// The boolean result lets startup regression tests verify that ordinary fresh
/// credentials do not schedule network work while keeping the policy with the
/// runtime owner that consumes it.
pub(crate) fn spawn_auth_store_refresh_if_needed(
    auth_store: AuthStore,
    leeway_seconds: u64,
) -> bool {
    match auth_store.openai_refresh_needed_with_leeway(leeway_seconds) {
        Ok(true) => {
            tokio::spawn(async move {
                let _ = auth_store
                    .refresh_openai_provider_credential_if_needed_with_leeway_async(leeway_seconds)
                    .await;
            });
            true
        }
        Ok(false) | Err(_) => false,
    }
}

fn start_session(
    service: &mut RuntimeSessionService,
    startup: SessionRuntimeStartup,
) -> Result<()> {
    match startup {
        SessionRuntimeStartup::Initial {
            explicit_command,
            start_directory,
            environment,
        } => {
            service.start_initial_pane_process_with_launch_context(
                explicit_command.as_deref(),
                start_directory.as_deref(),
                environment.as_deref(),
            )?;
            service.restore_agent_sessions_from_transcript_store()?;
            Ok(())
        }
        SessionRuntimeStartup::RestoredSnapshot {
            payload,
            restart_command,
        } => {
            service.seed_terminal_screens_from_snapshot_payload(&payload)?;
            service.restart_restored_pane_processes(restart_command.as_deref())?;
            Ok(())
        }
    }
}

fn validate_daemon_config_for_publication(
    config: &AsyncRuntimeDaemonConfig,
    has_listener: bool,
) -> Result<()> {
    if has_listener {
        config.validate()
    } else if config.message_max_content_length == 0 || config.message_fanout_limit == 0 {
        Err(MezError::invalid_args(
            "session runtime worker limits must be greater than zero",
        ))
    } else {
        Ok(())
    }
}

fn build_provider_refresh_service(handle: AsyncRuntimeSessionHandle) -> AsyncRuntimeService {
    AsyncRuntimeService::new_auxiliary("startup-provider-info-refresh", async move {
        let _ = handle.refresh_provider_info().await;
        Ok(AsyncRuntimeServiceExit::completed(1))
    })
}

fn build_runtime_x11_proxy_service(proxy: RuntimeX11Proxy) -> AsyncRuntimeService {
    AsyncRuntimeService::new("x11-proxy", async move {
        let handled = proxy.serve().await?;
        Ok(AsyncRuntimeServiceExit::completed(handled))
    })
}

fn build_actor_lifetime_service(handle: AsyncRuntimeSessionHandle) -> AsyncRuntimeService {
    AsyncRuntimeService::new("session-lifetime", async move {
        let mut lifecycle = handle.lifecycle_state_watcher();
        loop {
            if matches!(
                *lifecycle.borrow(),
                RuntimeLifecycleState::Stopping
                    | RuntimeLifecycleState::Killed
                    | RuntimeLifecycleState::Failed
            ) {
                return Ok(AsyncRuntimeServiceExit::shutdown(0));
            }
            if lifecycle.changed().await.is_err() {
                return Ok(AsyncRuntimeServiceExit::completed(0));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use crate::config::{ConfigFormat, ConfigScope};
    use crate::host::shell::{ResolvedShell, ShellSource};
    use mez_core::ids::SessionId;

    use super::*;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

    /// Two reusable runtimes must retain independent actors and lifecycle state.
    #[tokio::test(flavor = "current_thread")]
    async fn session_factory_constructs_and_isolates_independent_runtimes() {
        let first = create_test_runtime("first").await;
        let second = create_test_runtime("second").await;
        let first_handle = first.handle();
        let second_handle = second.handle();
        assert_ne!(first_handle.session_id(), second_handle.session_id());

        let first_task = tokio::spawn(first.run(Vec::new(), std::future::pending()));
        let second_task = tokio::spawn(second.run(Vec::new(), std::future::pending()));
        tokio::task::yield_now().await;
        assert_eq!(
            first_handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        assert_eq!(
            second_handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Running
        );

        first_handle
            .force_shutdown("test first shutdown")
            .await
            .unwrap();
        let first_completion = tokio::time::timeout(Duration::from_secs(2), first_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            first_completion.service.lifecycle_state(),
            RuntimeLifecycleState::Killed
        );
        assert_eq!(
            second_handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Running
        );

        second_handle
            .force_shutdown("test second shutdown")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), second_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    /// A listener bind failure after control publication must remove the
    /// already-created control socket before returning the construction error.
    #[tokio::test(flavor = "current_thread")]
    async fn session_factory_rolls_back_partial_socket_publication() {
        let root = test_root("partial-bind");
        let control = root.join("control.sock");
        let request = test_request("partial", root.clone(), control.clone());
        let request = SessionFactoryRequest {
            sockets: SessionSocketPublication {
                message_path: Some(PathBuf::from("relative.sock")),
                ..request.sockets
            },
            ..request
        };

        let error = SessionFactory::create(request).await.unwrap_err();
        assert!(!error.message().is_empty());
        assert!(!control.exists());
        let _ = fs::remove_dir_all(root);
    }

    /// Dropping a ready runtime before supervision starts must remove its live
    /// discovery artifacts instead of leaving a stale socket or registry row.
    #[tokio::test(flavor = "current_thread")]
    async fn session_runtime_drop_cleans_published_artifacts() {
        let root = test_root("drop-cleanup");
        let control = root.join("control.sock");
        let request = test_request("drop-cleanup", root.clone(), control.clone());
        let request = SessionFactoryRequest {
            sockets: SessionSocketPublication {
                publish_control: true,
                publish_registry: true,
                ..request.sockets
            },
            ..request
        };

        let runtime = SessionFactory::create(request).await.unwrap();
        let registry = SessionRegistry::new(root.clone(), crate::runtime::current_effective_uid());
        assert!(control.exists());
        assert_eq!(registry.list().unwrap().len(), 1);

        drop(runtime);

        assert!(!control.exists());
        assert!(registry.list().unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    /// Initial session startup must use the caller launch directory carried
    /// by the factory request instead of inheriting the persistent host cwd.
    #[tokio::test(flavor = "current_thread")]
    async fn session_factory_uses_explicit_initial_launch_directory() {
        let root = test_root("launch-directory");
        let launch_directory = root.join("caller-project");
        fs::create_dir_all(&launch_directory).unwrap();
        let observed = root.join("observed-cwd.txt");
        let mut request = test_request("launch-directory", root.clone(), root.join("control.sock"));
        request.startup = SessionRuntimeStartup::Initial {
            explicit_command: Some(format!(
                "printf '%s\\n%s\\n%s\\n%s\\n%s\\n' \"$PWD\" \"$HOME\" \"$COLUMNS\" \"$LINES\" \"${{CARGO_MANIFEST_DIR-unset}}\" > {}; sleep 30",
                observed.to_string_lossy()
            )),
            start_directory: Some(launch_directory.clone()),
            environment: Some(vec![
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
                ("HOME".to_string(), root.to_string_lossy().into_owned()),
                ("COLUMNS".to_string(), "101".to_string()),
                ("LINES".to_string(), "37".to_string()),
            ]),
        };

        let runtime = SessionFactory::create(request).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while fs::read_to_string(&observed).map_or(true, |value| value.trim().is_empty()) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let observed = fs::read_to_string(&observed).unwrap();
        let lines = observed.lines().collect::<Vec<_>>();
        assert_eq!(lines[0], launch_directory.to_string_lossy());
        assert_eq!(lines[1], root.to_string_lossy());
        assert_eq!(lines[2..], ["101", "37", "unset"]);

        drop(runtime);
        let _ = fs::remove_dir_all(root);
    }

    /// X11 policy must prepare the loopback proxy before the first pane starts,
    /// protect its environment from caller overrides, and remove private state
    /// when a ready runtime is dropped before supervision begins.
    #[tokio::test(flavor = "current_thread")]
    async fn session_factory_prepares_protected_x11_environment_before_initial_pane() {
        let root = test_root("x11-initial-environment");
        let observed = root.join("observed-x11.txt");
        let mut request = test_request(
            "x11-initial-environment",
            root.clone(),
            root.join("control.sock"),
        );
        request.config.layers[0].text.push_str(
            "\n[transport.iroh.x11]\nenabled = true\nallow_trusted = false\nmax_connections_per_route = 16\nsetup_timeout_ms = 5000\n",
        );
        request.startup = SessionRuntimeStartup::Initial {
            explicit_command: Some(format!(
                "printf '%s\\n%s\\n' \"$DISPLAY\" \"$XAUTHORITY\" > {}; sleep 30",
                observed.to_string_lossy()
            )),
            start_directory: None,
            environment: Some(vec![
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
                ("HOME".to_string(), root.to_string_lossy().into_owned()),
                ("DISPLAY".to_string(), "remote.invalid:99".to_string()),
                (
                    "XAUTHORITY".to_string(),
                    "/tmp/unsafe-authority".to_string(),
                ),
            ]),
        };

        let runtime = SessionFactory::create(request).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while fs::read_to_string(&observed).map_or(true, |value| value.lines().count() < 2) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let observed = fs::read_to_string(&observed).unwrap();
        let values = observed.lines().collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        assert!(values[0].starts_with("127.0.0.1:"), "{observed}");
        assert_ne!(values[0], "remote.invalid:99");
        let authority_path = PathBuf::from(values[1]);
        assert_ne!(authority_path, PathBuf::from("/tmp/unsafe-authority"));
        assert!(authority_path.starts_with(root.join("x11-sessions")));
        assert_eq!(fs::read(&authority_path).unwrap(), Vec::<u8>::new());
        assert_eq!(
            fs::metadata(authority_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&authority_path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        drop(runtime);
        assert!(!authority_path.exists());
        assert!(!root.join("x11-sessions").exists());
        let _ = fs::remove_dir_all(root);
    }

    /// Omitted X11 policy must not allocate proxy artifacts or change the
    /// pane environment of an otherwise ordinary session.
    #[tokio::test(flavor = "current_thread")]
    async fn session_factory_keeps_x11_disabled_sessions_unchanged() {
        let root = test_root("x11-disabled");
        let observed = root.join("observed-x11.txt");
        let mut request = test_request("x11-disabled", root.clone(), root.join("control.sock"));
        request.startup = SessionRuntimeStartup::Initial {
            explicit_command: Some(format!(
                "printf '%s\\n%s\\n' \"${{DISPLAY-unset}}\" \"${{XAUTHORITY-unset}}\" > {}; sleep 30",
                observed.to_string_lossy()
            )),
            start_directory: None,
            environment: Some(vec![
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
                ("HOME".to_string(), root.to_string_lossy().into_owned()),
            ]),
        };

        let runtime = SessionFactory::create(request).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while fs::read_to_string(&observed).map_or(true, |value| value.lines().count() < 2) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        assert_eq!(fs::read_to_string(&observed).unwrap(), "unset\nunset\n");
        assert!(!root.join("x11-sessions").exists());
        drop(runtime);
        let _ = fs::remove_dir_all(root);
    }

    async fn create_test_runtime(name: &str) -> SessionRuntime {
        let root = test_root(name);
        let control = root.join("control.sock");
        SessionFactory::create(test_request(name, root, control))
            .await
            .unwrap()
    }

    fn test_request(name: &str, root: PathBuf, control_path: PathBuf) -> SessionFactoryRequest {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let mut session = Session::new_default(
            ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
            Size::new(80, 24).unwrap(),
        );
        session.id = SessionId::new('$', id);
        session.name = name.to_string();
        SessionFactoryRequest {
            session,
            owner_uid: crate::runtime::current_effective_uid(),
            created_at_unix_seconds: 100,
            config: SessionRuntimeConfig {
                layers: vec![ConfigLayer {
                    name: "session-factory-test".to_string(),
                    path: None,
                    format: ConfigFormat::Toml,
                    scope: ConfigScope::Primary,
                    trusted: true,
                    text: "[agents]\nshell_mode = \"pane\"\n[permissions]\nsandbox = \"policy-only\"\n"
                        .to_string(),
                }],
                root,
            },
            sockets: SessionSocketPublication {
                control_path,
                publish_control: false,
                message_path: None,
                event_path: None,
                publish_registry: false,
            },
            limits: SessionRuntimeLimits::default(),
            startup: SessionRuntimeStartup::Initial {
                explicit_command: Some("cat >/dev/null".to_string()),
                start_directory: None,
                environment: None,
            },
        }
    }

    fn test_root(name: &str) -> PathBuf {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mez-session-factory-{}-{name}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }
}
