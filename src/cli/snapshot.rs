//! Cli Snapshot implementation.
//!
//! This module owns the cli snapshot boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::serve::{ServeCliArgs, assign_unique_live_session_id};
use super::{
    Args, CliEnv, CliOutputFormat, ConfigPaths, LoadedRuntimeConfig, MezError, ParsedServeOptions,
    RestoredSnapshotDaemonRequest, Result, RuntimeDaemonStartup, RuntimeSessionService, Serialize,
    SnapshotKind, SnapshotRepository, SnapshotRestoreResult, SnapshotResumePlan,
    SnapshotRollbackPlan, SnapshotState, SocketSelection, Subcommand, Write,
    apply_default_serve_auxiliary_sockets, cli_idempotency_key, current_unix_seconds, json_escape,
    json_optional, json_string_array, load_runtime_config_layers, resolve_shell,
    run_control_request, run_foreground_control_daemon, selected_socket_path, serialize_json,
    validate_serve_options, write_json_or_plain,
};

// Snapshot subcommands and restored daemon startup.

/// Structured JSON payload emitted when a snapshot delete command completes.
#[derive(Serialize)]
struct SnapshotDeleteJson {
    /// Whether a snapshot manifest and payload were removed.
    deleted: bool,
}

/// Runs the run snapshot operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn run_snapshot<W: Write>(
    parsed: SnapshotCliArgs,
    env: CliEnv,
    socket_selection: &SocketSelection,
    interactive: bool,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let paths = env.config_paths()?;
    let repository = SnapshotRepository::new(paths.root().join("snapshots"));

    match parsed.command.unwrap_or(SnapshotCliCommand::List) {
        SnapshotCliCommand::List => {
            let output = snapshots_json(&repository.list()?);
            write_json_or_plain(stdout, output_format, &output)?;
        }
        SnapshotCliCommand::Inspect { snapshot_id } => {
            let manifest = repository.inspect(&snapshot_id)?;
            let output = snapshot_json(&manifest.state);
            write_json_or_plain(stdout, output_format, &output)?;
        }
        SnapshotCliCommand::Delete { snapshot_id } => {
            let deleted = repository.delete(&snapshot_id)?;
            let output = serialize_json(&SnapshotDeleteJson { deleted })?;
            write_json_or_plain(stdout, output_format, &output)?;
        }
        SnapshotCliCommand::ResumePlan { snapshot_id } => {
            let plan = repository.resume_plan(&snapshot_id)?;
            let output = resume_plan_json(&plan);
            write_json_or_plain(stdout, output_format, &output)?;
        }
        SnapshotCliCommand::LatestPlan { session_id } => {
            let plan = repository.latest_resume_plan(session_id.as_deref())?;
            let output = resume_plan_json(&plan);
            write_json_or_plain(stdout, output_format, &output)?;
        }
        SnapshotCliCommand::Resume(resume) => {
            let restored =
                repository.restore_session(&resume.snapshot_id, resolve_shell(env.shell)?)?;
            let payload = repository.inspect_payload(&resume.snapshot_id)?;
            if let Some(options) =
                snapshot_resume_serve_options(&resume.serve_args, resume.serve, interactive)?
            {
                run_restored_snapshot_daemon(
                    RestoredSnapshotDaemonRequest {
                        restored,
                        payload,
                        restart_command: resume.restart_command.clone(),
                        paths: &paths,
                        socket_selection,
                        owner_uid: env.runtime.uid,
                        options,
                    },
                    output_format,
                    stdout,
                )
                .await?;
            } else {
                let output =
                    restored_snapshot_json(restored, resume.restart_command.as_deref(), &paths)?;
                write_json_or_plain(stdout, output_format, &output)?;
            }
        }
        SnapshotCliCommand::ResumeLatest(resume) => {
            if let Some(options) =
                snapshot_resume_serve_options(&resume.serve_args, resume.serve, interactive)?
            {
                let latest = repository
                    .latest(resume.session_id.as_deref())?
                    .ok_or_else(|| {
                        MezError::new(
                            crate::error::MezErrorKind::NotFound,
                            "no matching snapshot found",
                        )
                    })?;
                let restored = repository.restore_session(&latest.id, resolve_shell(env.shell)?)?;
                let payload = repository.inspect_payload(&latest.id)?;
                run_restored_snapshot_daemon(
                    RestoredSnapshotDaemonRequest {
                        restored,
                        payload,
                        restart_command: resume.restart_command.clone(),
                        paths: &paths,
                        socket_selection,
                        owner_uid: env.runtime.uid,
                        options,
                    },
                    output_format,
                    stdout,
                )
                .await?;
            } else {
                let restored = repository.restore_latest_session(
                    resume.session_id.as_deref(),
                    resolve_shell(env.shell)?,
                )?;
                let output =
                    restored_snapshot_json(restored, resume.restart_command.as_deref(), &paths)?;
                write_json_or_plain(stdout, output_format, &output)?;
            }
        }
        SnapshotCliCommand::RollbackPlan { snapshot_id } => {
            let plan = repository.rollback_plan(&snapshot_id)?;
            let output = rollback_plan_json(&plan);
            write_json_or_plain(stdout, output_format, &output)?;
        }
        SnapshotCliCommand::Create { name } => {
            let name_json = name
                .as_deref()
                .map(|name| format!(r#","name":"{}""#, json_escape(name)))
                .unwrap_or_default();
            let params = format!(
                r#"{{"target":{{"default":true}},"idempotency_key":"{}"{name_json}}}"#,
                cli_idempotency_key("snapshot-create")
            );
            run_control_request(
                socket_selection,
                "snapshot/create",
                &params,
                output_format,
                stdout,
            )?;
        }
    }
    Ok(())
}

/// Typed process CLI arguments for `mez snapshot`.
#[derive(Debug, Clone, Args)]
pub(super) struct SnapshotCliArgs {
    /// Optional snapshot subcommand, defaulting to `list`.
    #[command(subcommand)]
    command: Option<SnapshotCliCommand>,
}

/// Typed process CLI subcommands for snapshot management.
#[derive(Debug, Clone, Subcommand)]
enum SnapshotCliCommand {
    /// Lists persisted snapshots.
    List,
    /// Creates a live snapshot through the control socket.
    Create {
        /// Optional snapshot name.
        #[arg(short = 'n', long)]
        name: Option<String>,
    },
    /// Inspects one snapshot manifest.
    Inspect {
        /// Snapshot id.
        snapshot_id: String,
    },
    /// Deletes one snapshot manifest and payload.
    Delete {
        /// Snapshot id.
        snapshot_id: String,
    },
    /// Restores one snapshot into a model or live daemon.
    Resume(SnapshotResumeCliArgs),
    /// Restores the latest matching snapshot into a model or live daemon.
    ResumeLatest(SnapshotResumeLatestCliArgs),
    /// Shows the restore plan for one snapshot.
    ResumePlan {
        /// Snapshot id.
        snapshot_id: String,
    },
    /// Shows the restore plan for the latest matching snapshot.
    #[command(alias = "resume-latest-plan")]
    LatestPlan {
        /// Optional session id filter.
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Shows whether one snapshot can act as a rollback point.
    RollbackPlan {
        /// Snapshot id.
        snapshot_id: String,
    },
}

/// Typed process CLI arguments for `mez snapshot resume`.
#[derive(Debug, Clone, clap::Args)]
pub(super) struct SnapshotResumeCliArgs {
    /// Snapshot id.
    snapshot_id: String,
    /// Restores the snapshot into a live foreground daemon.
    #[arg(long)]
    serve: bool,
    /// Command used to restart restorable pane processes.
    #[arg(long, allow_hyphen_values = true)]
    restart_command: Option<String>,
    /// Serve options accepted when `--serve` is present.
    #[command(flatten)]
    serve_args: ServeCliArgs,
}

/// Typed process CLI arguments for `mez snapshot resume-latest`.
#[derive(Debug, Clone, clap::Args)]
pub(super) struct SnapshotResumeLatestCliArgs {
    /// Optional session id filter.
    #[arg(long)]
    session_id: Option<String>,
    /// Restores the snapshot into a live foreground daemon.
    #[arg(long)]
    serve: bool,
    /// Command used to restart restorable pane processes.
    #[arg(long, allow_hyphen_values = true)]
    restart_command: Option<String>,
    /// Serve options accepted when `--serve` is present.
    #[command(flatten)]
    serve_args: ServeCliArgs,
}

/// Returns serve options for snapshot restore commands when requested.
///
/// # Parameters
/// - `args`: The typed serve option group parsed by `clap`.
/// - `serve`: Whether the restore command should launch a foreground daemon.
/// - `interactive`: Whether the caller has an interactive terminal.
fn snapshot_resume_serve_options(
    args: &ServeCliArgs,
    serve: bool,
    interactive: bool,
) -> Result<Option<ParsedServeOptions>> {
    if !serve {
        if args.any_present() {
            return Err(MezError::invalid_args(
                "snapshot serve options require --serve",
            ));
        }
        return Ok(None);
    }
    let options = args.clone().into_parsed()?;
    if options.attach_primary && !interactive {
        return Err(MezError::forbidden(
            "starting an attached primary client requires an interactive terminal",
        ));
    }
    Ok(Some(options))
}

/// Runs the run restored snapshot daemon operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn run_restored_snapshot_daemon<W: Write>(
    request: RestoredSnapshotDaemonRequest<'_>,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let RestoredSnapshotDaemonRequest {
        mut restored,
        payload,
        restart_command,
        paths,
        socket_selection,
        owner_uid,
        mut options,
    } = request;
    if let Some(command) = restart_command.as_deref()
        && command.trim().is_empty()
    {
        return Err(MezError::invalid_args(
            "--restart-command must not be empty",
        ));
    }

    let config_path = paths.ensure_default_config()?;
    let socket_path = selected_socket_path(socket_selection).clone();
    assign_unique_live_session_id(&mut restored.session)?;
    if options.attach_primary {
        let primary_client_id = restored.session.attach_primary("primary", true)?;
        options.attached_primary_client_id = Some(primary_client_id);
    }
    apply_default_serve_auxiliary_sockets(&mut options, &socket_path)?;
    validate_serve_options(&options)?;

    let session_id = restored.session.id.to_string();
    let message_socket_json = options
        .message_socket
        .as_ref()
        .map(|path| format!(r#""{}""#, json_escape(&path.to_string_lossy())))
        .unwrap_or_else(|| "null".to_string());
    let event_socket_json = options
        .event_socket
        .as_ref()
        .map(|path| format!(r#""{}""#, json_escape(&path.to_string_lossy())))
        .unwrap_or_else(|| "null".to_string());
    let restarted = true;

    let startup = format!(
        r#"{{"serving":true,"restored":true,"live":true,"restarted":{},"session_id":"{}","socket":"{}","message_socket":{},"event_socket":{},"config":"{}","control":true,"message":{},"event":{},"resume_plan":{}}}"#,
        restarted,
        json_escape(&session_id),
        json_escape(&socket_path.to_string_lossy()),
        message_socket_json,
        event_socket_json,
        json_escape(&config_path.to_string_lossy()),
        options.message_socket.is_some(),
        options.event_socket.is_some(),
        resume_plan_json(&restored.resume_plan),
    );
    write_json_or_plain(stdout, output_format, &startup)?;
    stdout.flush()?;

    run_foreground_control_daemon(
        restored.session,
        socket_path,
        owner_uid,
        current_unix_seconds()?,
        LoadedRuntimeConfig {
            layers: load_runtime_config_layers(paths)?,
            root: paths.root().to_path_buf(),
        },
        options,
        RuntimeDaemonStartup::RestoredSnapshot {
            payload: Box::new(payload),
            restart_command,
        },
    )
    .await
}
/// Runs the snapshots json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn snapshots_json(snapshots: &[SnapshotState]) -> String {
    format!(
        "[{}]",
        snapshots
            .iter()
            .map(snapshot_json)
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Runs the snapshot json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn snapshot_json(snapshot: &SnapshotState) -> String {
    let limitations = json_string_array(&snapshot.limitations);
    format!(
        r#"{{"snapshot_id":"{}","version":{},"session_id":"{}","name":{},"created_at":"{}","kind":"{}","restorable":{},"window_count":{},"pane_count":{},"limitations":{},"storage_ref":"{}"}}"#,
        json_escape(&snapshot.id),
        snapshot.version,
        json_escape(&snapshot.session_id),
        json_optional(snapshot.name.as_deref()),
        json_escape(&snapshot.created_at),
        snapshot_kind_name(snapshot.kind),
        snapshot.restorable,
        snapshot.window_count,
        snapshot.pane_count,
        limitations,
        json_escape(&snapshot.storage_ref)
    )
}

/// Runs the resume plan json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn resume_plan_json(plan: &SnapshotResumePlan) -> String {
    let restart_required = plan
        .restart_required_panes
        .iter()
        .map(|pane| format!(r#""{}""#, json_escape(pane)))
        .collect::<Vec<_>>()
        .join(",");
    let limitations = json_string_array(&plan.limitations);
    format!(
        r#"{{"session_id":"{}","window_count":{},"pane_count":{},"restart_required_panes":[{}],"limitations":{}}}"#,
        json_escape(&plan.session_id),
        plan.window_count,
        plan.pane_count,
        restart_required,
        limitations
    )
}

/// Runs the restored snapshot json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn restored_snapshot_json(
    restored: SnapshotRestoreResult,
    restart_command: Option<&str>,
    paths: &ConfigPaths,
) -> Result<String> {
    if let Some(command) = restart_command
        && command.trim().is_empty()
    {
        return Err(MezError::invalid_args(
            "--restart-command must not be empty",
        ));
    }

    let Some(command) = restart_command else {
        return Ok(format!(
            r#"{{"restored":true,"live":false,"session":{},"resume_plan":{}}}"#,
            restored_session_json(&restored.session),
            resume_plan_json(&restored.resume_plan)
        ));
    };

    let socket_path = paths.root().join("runtime").join("snapshot-resume.sock");
    let mut service = RuntimeSessionService::with_event_log(
        restored.session,
        socket_path,
        current_unix_seconds()?,
        16,
        4096,
    )?;
    let starts = service.restart_restored_pane_processes(Some(command))?;
    let session_json = restored_session_json(service.session());
    let restarted_json = pane_process_starts_json(&starts);
    service.pane_processes_mut().terminate_all()?;
    Ok(format!(
        r#"{{"restored":true,"live":false,"restarted":true,"restarted_panes":{},"session":{},"resume_plan":{}}}"#,
        restarted_json,
        session_json,
        resume_plan_json(&restored.resume_plan)
    ))
}

/// Runs the pane process starts json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pane_process_starts_json(starts: &[crate::runtime::PaneProcessStart]) -> String {
    format!(
        "[{}]",
        starts
            .iter()
            .map(|start| {
                format!(
                    r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"columns":{},"rows":{}}}"#,
                    json_escape(&start.pane_id),
                    json_escape(&start.window_id),
                    start.primary_pid,
                    start.size.columns,
                    start.size.rows
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Runs the restored session json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn restored_session_json(session: &crate::session::Session) -> String {
    let window_count = session.windows().len();
    let pane_count = session
        .windows()
        .iter()
        .map(|window| window.panes().len())
        .sum::<usize>();
    let active_window_id = session
        .active_window()
        .map(|window| window.id.to_string())
        .unwrap_or_default();
    format!(
        r#"{{"session_id":"{}","name":"{}","state":"{}","window_count":{},"pane_count":{},"active_window_id":"{}"}}"#,
        json_escape(session.id.as_str()),
        json_escape(&session.name),
        session_state_name(session.state),
        window_count,
        pane_count,
        json_escape(&active_window_id)
    )
}

/// Runs the snapshot kind name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn snapshot_kind_name(kind: SnapshotKind) -> &'static str {
    match kind {
        SnapshotKind::Live => "live",
        SnapshotKind::Manual => "manual",
        SnapshotKind::Automatic => "automatic",
        SnapshotKind::CrashRecovery => "crash_recovery",
    }
}

/// Runs the session state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn session_state_name(state: crate::session::SessionState) -> &'static str {
    match state {
        crate::session::SessionState::Running => "running",
        crate::session::SessionState::Detached => "detached",
        crate::session::SessionState::Empty => "empty",
        crate::session::SessionState::Stopping => "stopping",
        crate::session::SessionState::Failed => "failed",
    }
}

/// Runs the rollback plan json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn rollback_plan_json(plan: &SnapshotRollbackPlan) -> String {
    let restart_required = plan
        .restart_required_panes
        .iter()
        .map(|pane| format!(r#""{}""#, json_escape(pane)))
        .collect::<Vec<_>>()
        .join(",");
    let limitations = json_string_array(&plan.limitations);
    format!(
        r#"{{"snapshot_id":"{}","session_id":"{}","available":{},"restore_command":{},"restart_required_panes":[{}],"limitations":{}}}"#,
        json_escape(&plan.snapshot_id),
        json_escape(&plan.session_id),
        plan.available,
        json_optional(plan.restore_command.as_deref()),
        restart_required,
        limitations
    )
}
