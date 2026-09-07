//! Native Linux power inhibition through direct D-Bus protocols.
//!
//! The backend maps system inhibition to logind's `idle` inhibitor and display
//! inhibition to the desktop ScreenSaver protocol. It never invokes helper
//! programs or crosses into a Windows host from WSL. Native calls remain owned
//! by the dedicated power worker and return opaque RAII leases to the shared
//! transition controller.

mod host_kind;
mod logind;
#[cfg(test)]
mod private_bus;
mod screensaver;

use std::fmt;
use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use tokio::runtime::Runtime;
use zbus::Connection;
use zbus::connection::Builder as ConnectionBuilder;

use host_kind::LinuxHostKind;
use logind::{LogindInhibitRequest, LogindLease};
use screensaver::{ScreenSaverInhibitRequest, ScreenSaverLease, ScreenSaverUninhibitor};

use super::{
    PowerInhibitionBackend, PowerInhibitionBackendKind, PowerInhibitionLease,
    PowerInhibitionResource,
};

/// Stable, secret-safe reason sent to both native Linux inhibition services.
const INHIBITION_REASON: &str = "Mezzanine is running an active agent turn";
const CONNECT_DEADLINE: Duration = Duration::from_secs(2);
const ACQUIRE_DEADLINE: Duration = Duration::from_secs(2);
const RELEASE_DEADLINE: Duration = Duration::from_secs(2);

/// Bounded Linux adapter failures. Display output never includes D-Bus reply
/// text, bus addresses, environment values, or other attacker-controlled data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinuxPowerInhibitionFailure {
    MissingBus,
    MissingService,
    Denied,
    Timeout,
    Disconnected,
    OwnerChanged,
    MalformedReply,
    WslUnsupported,
    KernelEvidenceUnavailable,
    Unavailable,
}

impl fmt::Display for LinuxPowerInhibitionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MissingBus => "required D-Bus is unavailable",
            Self::MissingService => "required D-Bus service is unavailable",
            Self::Denied => "D-Bus request was denied",
            Self::Timeout => "D-Bus operation timed out",
            Self::Disconnected => "D-Bus connection was lost",
            Self::OwnerChanged => "D-Bus service owner changed",
            Self::MalformedReply => "D-Bus service returned a malformed reply",
            Self::WslUnsupported => "WSL cannot inhibit Windows host power",
            Self::KernelEvidenceUnavailable => "Linux host kind could not be verified",
            Self::Unavailable => "native Linux power inhibition is unavailable",
        })
    }
}

/// Synchronous native-call seam owned by the Linux power worker.
trait LinuxPowerInhibitionApi: fmt::Debug + Send + Sync + 'static {
    /// Acquires one logind idle inhibitor and returns its owned descriptor.
    fn inhibit_logind(
        &self,
        request: LogindInhibitRequest,
    ) -> Result<LogindLease, LinuxPowerInhibitionFailure>;

    /// Acquires one owner-bound desktop ScreenSaver inhibitor.
    fn inhibit_screensaver(
        &self,
        request: ScreenSaverInhibitRequest,
    ) -> Result<ScreenSaverLease, LinuxPowerInhibitionFailure>;
}

/// Tokio runtime whose final drop never blocks an enclosing async runtime.
struct LinuxDbusRuntime {
    runtime: Option<Runtime>,
}

impl LinuxDbusRuntime {
    fn new() -> Option<Self> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()
            .map(|runtime| Self {
                runtime: Some(runtime),
            })
    }

    fn runtime(&self) -> &Runtime {
        self.runtime
            .as_ref()
            .expect("Linux D-Bus runtime is present until final drop")
    }
}

impl Drop for LinuxDbusRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Direct zbus implementation of the native Linux inhibition protocols.
pub(crate) struct NativeLinuxPowerInhibitionApi {
    runtime: Option<Arc<LinuxDbusRuntime>>,
}

impl fmt::Debug for NativeLinuxPowerInhibitionApi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeLinuxPowerInhibitionApi")
            .field("runtime_available", &self.runtime.is_some())
            .finish()
    }
}

impl NativeLinuxPowerInhibitionApi {
    fn new() -> Self {
        let runtime = LinuxDbusRuntime::new().map(Arc::new);
        Self { runtime }
    }

    fn runtime(&self) -> Result<&Runtime, LinuxPowerInhibitionFailure> {
        self.runtime
            .as_ref()
            .map(|runtime| runtime.runtime())
            .ok_or(LinuxPowerInhibitionFailure::Unavailable)
    }

    fn connect(&self, bus: LinuxBus) -> Result<Connection, LinuxPowerInhibitionFailure> {
        let builder = match bus {
            LinuxBus::System => ConnectionBuilder::system(),
            LinuxBus::Session => ConnectionBuilder::session(),
        }
        .map_err(|error| classify_zbus_error(&error, LinuxPowerInhibitionFailure::MissingBus))?;
        run_zbus(
            self.runtime()?,
            CONNECT_DEADLINE,
            builder.method_timeout(ACQUIRE_DEADLINE).build(),
            LinuxPowerInhibitionFailure::MissingBus,
        )
    }
}

impl LinuxPowerInhibitionApi for NativeLinuxPowerInhibitionApi {
    fn inhibit_logind(
        &self,
        request: LogindInhibitRequest,
    ) -> Result<LogindLease, LinuxPowerInhibitionFailure> {
        let connection = self.connect(LinuxBus::System)?;
        self.inhibit_logind_on(connection, Some(logind::DESTINATION), request)
    }

    fn inhibit_screensaver(
        &self,
        request: ScreenSaverInhibitRequest,
    ) -> Result<ScreenSaverLease, LinuxPowerInhibitionFailure> {
        let connection = self.connect(LinuxBus::Session)?;
        self.inhibit_screensaver_on(connection, Some(screensaver::DESTINATION), request)
    }
}

impl NativeLinuxPowerInhibitionApi {
    fn inhibit_logind_on(
        &self,
        connection: Connection,
        destination: Option<&str>,
        request: LogindInhibitRequest,
    ) -> Result<LogindLease, LinuxPowerInhibitionFailure> {
        let reply = run_zbus(
            self.runtime()?,
            ACQUIRE_DEADLINE,
            connection.call_method(
                destination,
                logind::PATH,
                Some(logind::INTERFACE),
                logind::METHOD,
                &request.body(),
            ),
            LinuxPowerInhibitionFailure::Unavailable,
        )?;
        let fd = reply
            .body()
            .deserialize::<zbus::zvariant::OwnedFd>()
            .map_err(|_| LinuxPowerInhibitionFailure::MalformedReply)?;
        Ok(LogindLease::new(fd.into()))
    }

    fn inhibit_screensaver_on(
        &self,
        connection: Connection,
        destination: Option<&str>,
        request: ScreenSaverInhibitRequest,
    ) -> Result<ScreenSaverLease, LinuxPowerInhibitionFailure> {
        let reply = run_zbus(
            self.runtime()?,
            ACQUIRE_DEADLINE,
            connection.call_method(
                destination,
                screensaver::PATH,
                Some(screensaver::INTERFACE),
                screensaver::INHIBIT_METHOD,
                &request.body(),
            ),
            LinuxPowerInhibitionFailure::MissingService,
        )?;
        let cookie = reply
            .body()
            .deserialize::<u32>()
            .map_err(|_| LinuxPowerInhibitionFailure::MalformedReply)?;
        let owner = reply
            .header()
            .sender()
            .ok_or(LinuxPowerInhibitionFailure::MalformedReply)?
            .to_owned()
            .into();
        Ok(ScreenSaverLease::new(
            Arc::new(NativeScreenSaverUninhibitor {
                runtime: Arc::clone(
                    self.runtime
                        .as_ref()
                        .ok_or(LinuxPowerInhibitionFailure::Unavailable)?,
                ),
                connection,
                owner,
            }),
            cookie,
        ))
    }
}

#[derive(Debug, Clone, Copy)]
enum LinuxBus {
    System,
    Session,
}

struct NativeScreenSaverUninhibitor {
    runtime: Arc<LinuxDbusRuntime>,
    connection: Connection,
    owner: zbus::names::OwnedUniqueName,
}

impl fmt::Debug for NativeScreenSaverUninhibitor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeScreenSaverUninhibitor")
            .finish_non_exhaustive()
    }
}

impl ScreenSaverUninhibitor for NativeScreenSaverUninhibitor {
    fn uninhibit(&self, cookie: u32) -> Result<(), LinuxPowerInhibitionFailure> {
        run_zbus(
            self.runtime.runtime(),
            RELEASE_DEADLINE,
            async {
                self.connection
                    .call_method(
                        Some(&self.owner),
                        screensaver::PATH,
                        Some(screensaver::INTERFACE),
                        screensaver::UNINHIBIT_METHOD,
                        &(cookie,),
                    )
                    .await
                    .map(|_| ())
            },
            LinuxPowerInhibitionFailure::Disconnected,
        )
        .map_err(|failure| match failure {
            LinuxPowerInhibitionFailure::MissingService => {
                LinuxPowerInhibitionFailure::OwnerChanged
            }
            failure => failure,
        })
    }

    fn owner_is_active(&self) -> Result<bool, LinuxPowerInhibitionFailure> {
        let reply = run_zbus(
            self.runtime.runtime(),
            RELEASE_DEADLINE,
            self.connection.call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "GetNameOwner",
                &(screensaver::DESTINATION,),
            ),
            LinuxPowerInhibitionFailure::Disconnected,
        )?;
        let current_owner = reply
            .body()
            .deserialize::<zbus::names::OwnedUniqueName>()
            .map_err(|_| LinuxPowerInhibitionFailure::MalformedReply)?;
        Ok(current_owner == self.owner)
    }
}

fn run_zbus<T>(
    runtime: &Runtime,
    deadline: Duration,
    future: impl Future<Output = zbus::Result<T>>,
    fallback: LinuxPowerInhibitionFailure,
) -> Result<T, LinuxPowerInhibitionFailure> {
    match runtime.block_on(async { tokio::time::timeout(deadline, future).await }) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(classify_zbus_error(&error, fallback)),
        Err(_) => Err(LinuxPowerInhibitionFailure::Timeout),
    }
}

fn classify_zbus_error(
    error: &zbus::Error,
    fallback: LinuxPowerInhibitionFailure,
) -> LinuxPowerInhibitionFailure {
    match error {
        zbus::Error::InputOutput(error) | zbus::Error::Connection(error, _) => {
            classify_io_error(error, fallback)
        }
        zbus::Error::MethodError(name, _, _) => classify_method_error(name.as_str(), fallback),
        zbus::Error::FDO(error) => classify_fdo_error(error, fallback),
        zbus::Error::Variant(_)
        | zbus::Error::InvalidReply
        | zbus::Error::InvalidField
        | zbus::Error::MissingField
        | zbus::Error::IncorrectEndian => LinuxPowerInhibitionFailure::MalformedReply,
        zbus::Error::Handshake(_) | zbus::Error::Address(_) => {
            LinuxPowerInhibitionFailure::MissingBus
        }
        _ => fallback,
    }
}

fn classify_io_error(
    error: &io::Error,
    fallback: LinuxPowerInhibitionFailure,
) -> LinuxPowerInhibitionFailure {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => LinuxPowerInhibitionFailure::Timeout,
        io::ErrorKind::PermissionDenied => LinuxPowerInhibitionFailure::Denied,
        io::ErrorKind::BrokenPipe
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::NotConnected
        | io::ErrorKind::UnexpectedEof => LinuxPowerInhibitionFailure::Disconnected,
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound => {
            LinuxPowerInhibitionFailure::MissingBus
        }
        _ => fallback,
    }
}

fn classify_method_error(
    name: &str,
    fallback: LinuxPowerInhibitionFailure,
) -> LinuxPowerInhibitionFailure {
    match name {
        "org.freedesktop.DBus.Error.ServiceUnknown"
        | "org.freedesktop.DBus.Error.NameHasNoOwner"
        | "org.freedesktop.DBus.Error.UnknownObject"
        | "org.freedesktop.DBus.Error.UnknownInterface"
        | "org.freedesktop.DBus.Error.UnknownMethod" => LinuxPowerInhibitionFailure::MissingService,
        "org.freedesktop.DBus.Error.AccessDenied"
        | "org.freedesktop.DBus.Error.AuthFailed"
        | "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired" => {
            LinuxPowerInhibitionFailure::Denied
        }
        "org.freedesktop.DBus.Error.NoReply"
        | "org.freedesktop.DBus.Error.Timeout"
        | "org.freedesktop.DBus.Error.TimedOut" => LinuxPowerInhibitionFailure::Timeout,
        "org.freedesktop.DBus.Error.Disconnected" | "org.freedesktop.DBus.Error.NoServer" => {
            LinuxPowerInhibitionFailure::Disconnected
        }
        "org.freedesktop.DBus.Error.InvalidArgs"
        | "org.freedesktop.DBus.Error.InvalidSignature"
        | "org.freedesktop.DBus.Error.InconsistentMessage" => {
            LinuxPowerInhibitionFailure::MalformedReply
        }
        _ => fallback,
    }
}

fn classify_fdo_error(
    error: &zbus::fdo::Error,
    fallback: LinuxPowerInhibitionFailure,
) -> LinuxPowerInhibitionFailure {
    match error {
        zbus::fdo::Error::ZBus(error) => classify_zbus_error(error, fallback),
        zbus::fdo::Error::ServiceUnknown(_)
        | zbus::fdo::Error::NameHasNoOwner(_)
        | zbus::fdo::Error::UnknownObject(_)
        | zbus::fdo::Error::UnknownInterface(_)
        | zbus::fdo::Error::UnknownMethod(_) => LinuxPowerInhibitionFailure::MissingService,
        zbus::fdo::Error::AccessDenied(_)
        | zbus::fdo::Error::AuthFailed(_)
        | zbus::fdo::Error::InteractiveAuthorizationRequired(_) => {
            LinuxPowerInhibitionFailure::Denied
        }
        zbus::fdo::Error::NoReply(_)
        | zbus::fdo::Error::Timeout(_)
        | zbus::fdo::Error::TimedOut(_) => LinuxPowerInhibitionFailure::Timeout,
        zbus::fdo::Error::Disconnected(_) | zbus::fdo::Error::NoServer(_) => {
            LinuxPowerInhibitionFailure::Disconnected
        }
        zbus::fdo::Error::InvalidArgs(_)
        | zbus::fdo::Error::InvalidSignature(_)
        | zbus::fdo::Error::InconsistentMessage(_) => LinuxPowerInhibitionFailure::MalformedReply,
        _ => fallback,
    }
}

/// Native Linux backend selected for production sessions outside WSL.
#[derive(Debug)]
pub(crate) struct LinuxDbusPowerInhibitionBackend<A = NativeLinuxPowerInhibitionApi> {
    host_kind: LinuxHostKind,
    api: Arc<A>,
}

impl LinuxDbusPowerInhibitionBackend {
    /// Detects the Linux host boundary before constructing the native API.
    pub(crate) fn new() -> Self {
        Self {
            host_kind: host_kind::detect(),
            api: Arc::new(NativeLinuxPowerInhibitionApi::new()),
        }
    }
}

#[cfg(test)]
impl<A> LinuxDbusPowerInhibitionBackend<A> {
    fn with_api(host_kind: LinuxHostKind, api: A) -> Self {
        Self {
            host_kind,
            api: Arc::new(api),
        }
    }
}

impl<A: LinuxPowerInhibitionApi> PowerInhibitionBackend for LinuxDbusPowerInhibitionBackend<A> {
    fn kind(&self) -> PowerInhibitionBackendKind {
        PowerInhibitionBackendKind::LinuxDbus
    }

    fn acquire(
        &mut self,
        resource: PowerInhibitionResource,
    ) -> Result<Box<dyn PowerInhibitionLease>, String> {
        match self.host_kind {
            LinuxHostKind::Wsl => {
                return Err(LinuxPowerInhibitionFailure::WslUnsupported.to_string());
            }
            LinuxHostKind::Unknown => {
                return Err(LinuxPowerInhibitionFailure::KernelEvidenceUnavailable.to_string());
            }
            LinuxHostKind::Native => {}
        }

        let lease: Box<dyn PowerInhibitionLease> = match resource {
            PowerInhibitionResource::System => Box::new(
                self.api
                    .inhibit_logind(LogindInhibitRequest::for_active_turn(INHIBITION_REASON))
                    .map_err(|failure| failure.to_string())?,
            ),
            PowerInhibitionResource::Display => Box::new(
                self.api
                    .inhibit_screensaver(ScreenSaverInhibitRequest::for_active_turn(
                        INHIBITION_REASON,
                    ))
                    .map_err(|failure| failure.to_string())?,
            ),
        };
        Ok(lease)
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::fd::OwnedFd;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::host::power_inhibition::{
        PowerInhibitionController, PowerInhibitionMode, PowerInhibitionState,
    };
    use screensaver::ScreenSaverUninhibitor;

    #[derive(Debug, Default)]
    struct FakeApiState {
        logind_requests: Vec<LogindInhibitRequest>,
        screensaver_requests: Vec<ScreenSaverInhibitRequest>,
        system_failure: Option<LinuxPowerInhibitionFailure>,
        display_failure: Option<LinuxPowerInhibitionFailure>,
    }

    #[derive(Debug, Default)]
    struct FakeApi {
        state: Arc<Mutex<FakeApiState>>,
        owner_generation: Arc<AtomicUsize>,
    }

    #[derive(Debug)]
    struct FakeUninhibitor {
        owner_generation: Arc<AtomicUsize>,
        lease_generation: usize,
    }

    impl ScreenSaverUninhibitor for FakeUninhibitor {
        fn uninhibit(&self, _: u32) -> Result<(), LinuxPowerInhibitionFailure> {
            Ok(())
        }

        fn owner_is_active(&self) -> Result<bool, LinuxPowerInhibitionFailure> {
            Ok(self.owner_generation.load(Ordering::Acquire) == self.lease_generation)
        }
    }

    impl LinuxPowerInhibitionApi for FakeApi {
        fn inhibit_logind(
            &self,
            request: LogindInhibitRequest,
        ) -> Result<LogindLease, LinuxPowerInhibitionFailure> {
            let mut state = self.state.lock().unwrap();
            state.logind_requests.push(request);
            if let Some(failure) = state.system_failure {
                return Err(failure);
            }
            drop(state);
            let file =
                File::open("/dev/null").map_err(|_| LinuxPowerInhibitionFailure::Unavailable)?;
            Ok(LogindLease::new(OwnedFd::from(file)))
        }

        fn inhibit_screensaver(
            &self,
            request: ScreenSaverInhibitRequest,
        ) -> Result<ScreenSaverLease, LinuxPowerInhibitionFailure> {
            let mut state = self.state.lock().unwrap();
            state.screensaver_requests.push(request);
            if let Some(failure) = state.display_failure {
                return Err(failure);
            }
            let lease_generation = self.owner_generation.load(Ordering::Acquire);
            Ok(ScreenSaverLease::new(
                Arc::new(FakeUninhibitor {
                    owner_generation: Arc::clone(&self.owner_generation),
                    lease_generation,
                }),
                41,
            ))
        }
    }

    /// Verifies native Linux system inhibition uses only logind's automatic
    /// idle category and never broad sleep, lid, key, or shutdown categories.
    #[test]
    fn backend_sends_exact_logind_idle_protocol_arguments() {
        let api = FakeApi::default();
        let state = Arc::clone(&api.state);
        let mut backend = LinuxDbusPowerInhibitionBackend::with_api(LinuxHostKind::Native, api);

        let mut lease = backend.acquire(PowerInhibitionResource::System).unwrap();
        lease.release().unwrap();

        assert_eq!(
            state.lock().unwrap().logind_requests,
            [LogindInhibitRequest {
                what: "idle",
                who: "Mezzanine",
                why: INHIBITION_REASON,
                mode: "block",
            }]
        );
    }

    /// Verifies Linux display inhibition sends the stable application identity
    /// and bounded reason required by the ScreenSaver protocol.
    #[test]
    fn backend_sends_exact_screensaver_protocol_arguments() {
        let api = FakeApi::default();
        let state = Arc::clone(&api.state);
        let mut backend = LinuxDbusPowerInhibitionBackend::with_api(LinuxHostKind::Native, api);

        let mut lease = backend.acquire(PowerInhibitionResource::Display).unwrap();
        lease.release().unwrap();

        assert_eq!(
            state.lock().unwrap().screensaver_requests,
            [ScreenSaverInhibitRequest {
                application: "io.mezzanine.Mez",
                reason: INHIBITION_REASON,
            }]
        );
    }

    /// Verifies a missing desktop service degrades combined mode to the held
    /// logind lease rather than discarding system protection.
    #[test]
    fn missing_screensaver_service_degrades_to_system_only() {
        let api = FakeApi::default();
        api.state.lock().unwrap().display_failure =
            Some(LinuxPowerInhibitionFailure::MissingService);
        let mut controller = PowerInhibitionController::new(
            LinuxDbusPowerInhibitionBackend::with_api(LinuxHostKind::Native, api),
        );

        controller.reconcile(PowerInhibitionMode::SystemAndDisplay);

        assert_eq!(controller.state(), PowerInhibitionState::SystemOnly);
        assert_eq!(
            controller.last_error(),
            Some("required D-Bus service is unavailable")
        );
    }

    /// Verifies a missing or denied logind service leaves combined mode
    /// unavailable and suppresses the weaker desktop acquisition entirely.
    #[test]
    fn unavailable_logind_skips_screensaver_acquisition() {
        let api = FakeApi::default();
        api.state.lock().unwrap().system_failure = Some(LinuxPowerInhibitionFailure::Denied);
        let state = Arc::clone(&api.state);
        let mut controller = PowerInhibitionController::new(
            LinuxDbusPowerInhibitionBackend::with_api(LinuxHostKind::Native, api),
        );

        controller.reconcile(PowerInhibitionMode::SystemAndDisplay);

        assert_eq!(controller.state(), PowerInhibitionState::Unavailable);
        assert_eq!(controller.last_error(), Some("D-Bus request was denied"));
        assert_eq!(state.lock().unwrap().logind_requests.len(), 1);
        assert!(state.lock().unwrap().screensaver_requests.is_empty());
    }

    /// Verifies a changed desktop service owner invalidates its stale cookie
    /// and the next reconciliation acquires a fresh owner-bound display lease.
    #[test]
    fn screensaver_owner_change_reacquires_display_lease() {
        let api = FakeApi::default();
        let state = Arc::clone(&api.state);
        let owner_generation = Arc::clone(&api.owner_generation);
        let mut controller = PowerInhibitionController::new(
            LinuxDbusPowerInhibitionBackend::with_api(LinuxHostKind::Native, api),
        );

        controller.reconcile(PowerInhibitionMode::SystemAndDisplay);
        owner_generation.fetch_add(1, Ordering::AcqRel);
        controller.reconcile(PowerInhibitionMode::SystemAndDisplay);

        assert_eq!(controller.state(), PowerInhibitionState::SystemAndDisplay);
        assert_eq!(state.lock().unwrap().logind_requests.len(), 1);
        assert_eq!(state.lock().unwrap().screensaver_requests.len(), 2);
    }

    /// Verifies WSL is rejected before either native bus acquisition path can
    /// run, so a Linux guest never claims control over Windows host power.
    #[test]
    fn wsl_host_kind_prevents_every_bus_attempt() {
        let api = FakeApi::default();
        let state = Arc::clone(&api.state);
        let mut backend = LinuxDbusPowerInhibitionBackend::with_api(LinuxHostKind::Wsl, api);

        assert_eq!(
            backend
                .acquire(PowerInhibitionResource::System)
                .unwrap_err(),
            "WSL cannot inhibit Windows host power"
        );
        assert!(state.lock().unwrap().logind_requests.is_empty());
        assert!(state.lock().unwrap().screensaver_requests.is_empty());
    }

    /// Verifies attacker-controlled D-Bus details are reduced to fixed,
    /// bounded classifications before entering controller diagnostics.
    #[test]
    fn dbus_error_classification_does_not_expose_error_details() {
        let secret = "private bus address and attacker-controlled details";
        let cases = [
            (
                zbus::fdo::Error::AccessDenied(secret.to_string()),
                LinuxPowerInhibitionFailure::Denied,
            ),
            (
                zbus::fdo::Error::ServiceUnknown(secret.to_string()),
                LinuxPowerInhibitionFailure::MissingService,
            ),
            (
                zbus::fdo::Error::TimedOut(secret.to_string()),
                LinuxPowerInhibitionFailure::Timeout,
            ),
            (
                zbus::fdo::Error::InvalidArgs(secret.to_string()),
                LinuxPowerInhibitionFailure::MalformedReply,
            ),
        ];

        for (error, expected) in cases {
            let failure = classify_fdo_error(&error, LinuxPowerInhibitionFailure::Unavailable);
            assert_eq!(failure, expected);
            assert!(!failure.to_string().contains(secret));
        }
    }

    /// Verifies the adapter-owned runtime can execute deadline-wrapped D-Bus
    /// work from Tokio's blocking pool without blocking or nesting the actor runtime.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_adapter_runtime_isolated_from_tokio_actor_runtime() {
        let result = tokio::task::spawn_blocking(|| {
            let api = NativeLinuxPowerInhibitionApi::new();
            run_zbus(
                api.runtime().unwrap(),
                Duration::from_millis(100),
                async { Ok::<_, zbus::Error>(41_u32) },
                LinuxPowerInhibitionFailure::Unavailable,
            )
        })
        .await
        .unwrap();

        assert_eq!(result, Ok(41));
    }
}
