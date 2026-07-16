//! Async Runtime Supervisor implementation.
//!
//! This module owns the async runtime supervisor boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    Duration, Future, HashMap, HashSet, JoinError, JoinSet, MezError, Pin, Result, TokioTaskId,
};

// Async runtime service supervision.

/// Defines the DEFAULT ASYNC RUNTIME COMMAND BUFFER const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_ASYNC_RUNTIME_COMMAND_BUFFER: usize = 64;
/// Defines the DEFAULT ASYNC CONTROL MAX CONTENT LENGTH const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_ASYNC_CONTROL_MAX_CONTENT_LENGTH: usize = 1024 * 1024;
/// Defines the DEFAULT ASYNC EVENT LIMIT PER CONNECTION const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_ASYNC_EVENT_LIMIT_PER_CONNECTION: usize = 100;
/// Default delay for actor-owned idle cleanup work that replaced tick scans.
pub const DEFAULT_ASYNC_IDLE_CLEANUP_INTERVAL: Duration = Duration::from_millis(16);
/// Maximum time an attached foreground terminal waits before checking whether
/// the hosting terminal dimensions changed.
pub const DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT: Duration = Duration::from_millis(100);

/// Defines the Async Runtime Service Future type used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) type AsyncRuntimeServiceFuture =
    Pin<Box<dyn Future<Output = Result<AsyncRuntimeServiceExit>> + Send + 'static>>;
/// Defines the Async Runtime Service Join Result type used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) type AsyncRuntimeServiceJoinResult =
    std::result::Result<(TokioTaskId, Result<AsyncRuntimeServiceExit>), JoinError>;

/// Terminal state reported by one supervised async runtime service.
///
/// Long-lived listener tasks use `Completed` when they naturally drain their
/// assigned work and `Shutdown` when their own lifecycle predicate decides the
/// broader daemon should begin shutdown. Failures stay on the `Result` channel
/// so callers cannot accidentally treat them as successful service reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsyncRuntimeServiceExitKind {
    /// The service finished without requesting supervisor shutdown.
    Completed,
    /// The service observed a shutdown condition and requested peer services stop.
    Shutdown,
}

/// Successful exit details from one supervised async runtime service.
///
/// `work_units` is intentionally generic: listener futures can report accepted
/// connections, event stream futures can report delivered batches, and tests or
/// future daemon tasks can report any local progress counter without teaching
/// the supervisor protocol-specific semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncRuntimeServiceExit {
    /// Whether this service completed normally or requested shutdown.
    pub kind: AsyncRuntimeServiceExitKind,
    /// Generic progress count reported by the service.
    pub work_units: u64,
}

impl AsyncRuntimeServiceExit {
    /// Builds a normal completion exit with a caller-defined progress count.
    pub fn completed(work_units: u64) -> Self {
        Self {
            kind: AsyncRuntimeServiceExitKind::Completed,
            work_units,
        }
    }

    /// Builds a shutdown exit with a caller-defined progress count.
    pub fn shutdown(work_units: u64) -> Self {
        Self {
            kind: AsyncRuntimeServiceExitKind::Shutdown,
            work_units,
        }
    }
}

/// Named async service future scheduled by the runtime service supervisor.
///
/// This wrapper deliberately owns only the service name, auxiliary lifecycle
/// role, and future. Socket binding, runtime session ownership, listener
/// internals, and lifecycle predicates stay with the caller-provided future so
/// the supervisor remains a narrow task orchestration primitive.
pub struct AsyncRuntimeService {
    /// Stores the name value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) name: String,
    /// Stores the auxiliary value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) auxiliary: bool,
    /// Stores the future value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) future: AsyncRuntimeServiceFuture,
}

impl AsyncRuntimeService {
    /// Creates a named service from any `Send + 'static` async task.
    ///
    /// The supervisor validates the final service set, including empty names
    /// and duplicate names, before spawning any tasks.
    pub fn new(
        name: impl Into<String>,
        future: impl Future<Output = Result<AsyncRuntimeServiceExit>> + Send + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            auxiliary: false,
            future: Box::pin(future),
        }
    }

    /// Creates an auxiliary service that can stop once all primary services finish.
    ///
    /// Auxiliary services are useful for background maintenance loops, such as
    /// daemon ticking, that should run alongside listeners but should not keep a
    /// supervision run alive after every primary service has completed.
    pub fn new_auxiliary(
        name: impl Into<String>,
        future: impl Future<Output = Result<AsyncRuntimeServiceExit>> + Send + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            auxiliary: true,
            future: Box::pin(future),
        }
    }

    /// Returns the service name that will be preserved in reports and errors.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl std::fmt::Debug for AsyncRuntimeService {
    /// Runs the fmt operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AsyncRuntimeService")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Named successful completion report for one supervised runtime service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncRuntimeServiceReport {
    /// Service name supplied when the task was scheduled.
    pub name: String,
    /// Successful service exit state.
    pub exit: AsyncRuntimeServiceExit,
}

/// Aggregate report produced by the async runtime service supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncRuntimeSupervisionReport {
    /// Reports for services that completed or were cancelled for shutdown.
    pub services: Vec<AsyncRuntimeServiceReport>,
    /// True when supervision stopped because a service requested shutdown or an
    /// external cancellation future completed.
    pub shutdown_requested: bool,
}

/// Supervises named long-lived async runtime services with Tokio tasks.
///
/// The supervisor schedules all services into a `JoinSet`, validates that the
/// service set is non-empty with unique names, and returns reports tagged with
/// those names. If any service returns an error or panics, the supervisor
/// propagates that failure with service-name context. If a service reports
/// shutdown or the caller-provided cancellation future resolves, pending
/// services are aborted and reported as shutdown.
#[derive(Debug)]
pub struct AsyncRuntimeServiceSupervisor {
    /// Stores the tasks value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) tasks: JoinSet<Result<AsyncRuntimeServiceExit>>,
    /// Stores the task names value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) task_names: HashMap<TokioTaskId, (String, bool)>,
}

impl AsyncRuntimeServiceSupervisor {
    /// Validates and schedules a set of named async runtime services.
    ///
    /// At least one service is required. Names must be non-empty after
    /// trimming and unique within the supervised set.
    pub fn new(services: Vec<AsyncRuntimeService>) -> Result<Self> {
        validate_async_runtime_services(&services)?;

        let mut tasks = JoinSet::new();
        let mut task_names = HashMap::new();
        for service in services {
            let name = service.name;
            let auxiliary = service.auxiliary;
            let abort_handle = tasks.spawn(service.future);
            task_names.insert(abort_handle.id(), (name, auxiliary));
        }

        Ok(Self { tasks, task_names })
    }

    /// Runs supervised services until all finish, one requests shutdown, or
    /// `cancellation` completes.
    ///
    /// When shutdown is requested, still-running services are aborted and reported
    /// with `AsyncRuntimeServiceExitKind::Shutdown`. When only auxiliary services
    /// remain, they are aborted and reported as completed so background loops do not
    /// keep supervision alive after primary listener work has drained.
    pub async fn run_until_shutdown<C>(
        mut self,
        cancellation: C,
    ) -> Result<AsyncRuntimeSupervisionReport>
    where
        C: Future<Output = ()>,
    {
        let mut report = AsyncRuntimeSupervisionReport {
            services: Vec::new(),
            shutdown_requested: false,
        };
        tokio::pin!(cancellation);

        while !self.task_names.is_empty() {
            tokio::select! {
                joined = self.tasks.join_next_with_id() => {
                    let Some(joined) = joined else {
                        break;
                    };
                    let service_report = self.report_joined_service(joined)?;
                    let requested_shutdown =
                        service_report.exit.kind == AsyncRuntimeServiceExitKind::Shutdown;
                    report.services.push(service_report);
                    if requested_shutdown {
                        report.shutdown_requested = true;
                        report.services.extend(self.abort_remaining_as_shutdown().await?);
                        break;
                    }
                    if self.only_auxiliary_services_remaining() {
                        report
                            .services
                            .extend(self.abort_remaining_auxiliary_as_completed().await?);
                        break;
                    }
                }
                () = &mut cancellation => {
                    report.shutdown_requested = true;
                    report.services.extend(self.abort_remaining_as_shutdown().await?);
                    break;
                }
            }
        }

        Ok(report)
    }

    /// Runs the report joined service operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn report_joined_service(
        &mut self,
        joined: AsyncRuntimeServiceJoinResult,
    ) -> Result<AsyncRuntimeServiceReport> {
        match joined {
            Ok((task_id, Ok(exit))) => Ok(AsyncRuntimeServiceReport {
                name: self.take_service_name(task_id),
                exit,
            }),
            Ok((task_id, Err(error))) => {
                let name = self.take_service_name(task_id);
                Err(MezError::new(
                    error.kind(),
                    format!("async runtime service {name} failed: {}", error.message()),
                ))
            }
            Err(error) => {
                let name = self.take_service_name(error.id());
                if error.is_cancelled() {
                    Ok(AsyncRuntimeServiceReport {
                        name,
                        exit: AsyncRuntimeServiceExit::shutdown(0),
                    })
                } else {
                    Err(MezError::invalid_state(format!(
                        "async runtime service {name} task failed: {error}"
                    )))
                }
            }
        }
    }

    /// Runs the take service name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn take_service_name(&mut self, task_id: TokioTaskId) -> String {
        self.task_names
            .remove(&task_id)
            .map(|(name, _auxiliary)| name)
            .unwrap_or_else(|| format!("task {task_id}"))
    }

    /// Runs the abort remaining as shutdown operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn abort_remaining_as_shutdown(&mut self) -> Result<Vec<AsyncRuntimeServiceReport>> {
        self.tasks.abort_all();
        let mut reports = Vec::new();
        while let Some(joined) = self.tasks.join_next_with_id().await {
            reports.push(self.report_joined_service(joined)?);
        }
        Ok(reports)
    }

    /// Runs the only auxiliary services remaining operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn only_auxiliary_services_remaining(&self) -> bool {
        !self.task_names.is_empty()
            && self
                .task_names
                .values()
                .all(|(_name, auxiliary)| *auxiliary)
    }

    /// Runs the abort remaining auxiliary as completed operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn abort_remaining_auxiliary_as_completed(
        &mut self,
    ) -> Result<Vec<AsyncRuntimeServiceReport>> {
        self.tasks.abort_all();
        let mut reports = Vec::new();
        while let Some(joined) = self.tasks.join_next_with_id().await {
            match joined {
                Ok((task_id, Ok(exit))) => reports.push(AsyncRuntimeServiceReport {
                    name: self.take_service_name(task_id),
                    exit,
                }),
                Ok((task_id, Err(error))) => {
                    let name = self.take_service_name(task_id);
                    return Err(MezError::new(
                        error.kind(),
                        format!("async runtime service {name} failed: {}", error.message()),
                    ));
                }
                Err(error) if error.is_cancelled() => reports.push(AsyncRuntimeServiceReport {
                    name: self.take_service_name(error.id()),
                    exit: AsyncRuntimeServiceExit::completed(0),
                }),
                Err(error) => {
                    let name = self.take_service_name(error.id());
                    return Err(MezError::invalid_state(format!(
                        "async runtime service {name} task failed: {error}"
                    )));
                }
            }
        }
        Ok(reports)
    }
}

/// Schedules and supervises named async runtime services until shutdown.
///
/// This convenience wrapper is useful when the caller does not need to retain
/// the supervisor value. Pass `std::future::pending()` when supervision should
/// rely only on service completion or service-reported shutdown.
pub async fn supervise_async_runtime_services<C>(
    services: Vec<AsyncRuntimeService>,
    cancellation: C,
) -> Result<AsyncRuntimeSupervisionReport>
where
    C: Future<Output = ()>,
{
    AsyncRuntimeServiceSupervisor::new(services)?
        .run_until_shutdown(cancellation)
        .await
}

/// Runs the validate async runtime services operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_async_runtime_services(services: &[AsyncRuntimeService]) -> Result<()> {
    if services.is_empty() {
        return Err(MezError::invalid_args(
            "async runtime supervisor requires at least one service",
        ));
    }

    let mut names = HashSet::new();
    for service in services {
        if service.name().trim().is_empty() {
            return Err(MezError::invalid_args(
                "async runtime service name must not be empty",
            ));
        }
        if !names.insert(service.name()) {
            return Err(MezError::invalid_args(format!(
                "async runtime supervisor service name is duplicated: {}",
                service.name()
            )));
        }
    }

    Ok(())
}
