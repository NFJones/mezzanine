//! Cli Dispatch implementation.
//!
//! This module owns the cli dispatch boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use clap::CommandFactory;

use super::env::CliArgv;
use super::{
    CliCommand, CliInvocation, CliInvocationParse, ConfigPaths, IsTerminal, MezError, OsString,
    PathBuf, Result, RuntimeEnv, Write, cli_idempotency_key, ensure_host_available,
    ensure_private_socket_directory, force_kill_iroh_host_session, host_create_session,
    host_list_sessions_with_all, host_resolve_or_create_session, host_resolve_session, io,
    json_escape, list_iroh_host_sessions, prune_stale_socket_files_in_directory, run_attach,
    run_auth, run_config, run_control_request_for_target, run_host, run_issue, run_lease, run_list,
    run_mcp, run_memory, run_new, run_remote, run_sandbox, run_serve, run_session_catalog,
    run_snapshot,
};

// Top-level CLI run and command dispatch.

/// Runs the run operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn run() -> u8 {
    let args = std::env::args().collect::<Vec<_>>();
    let env = CliEnv::from_process();
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();

    match run_with(args, env, interactive, &mut stdout, &mut stderr).await {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(stderr, "mez: {error}");
            1
        }
    }
}

/// Carries Cli Env state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default)]
pub struct CliEnv {
    /// Stores the home value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub home: Option<PathBuf>,
    /// Stores the shell value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub shell: Option<OsString>,
    /// Stores the mez value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mez: Option<OsString>,
    /// Stores the runtime value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub runtime: RuntimeEnv,
    /// Injected sandbox platform evidence for deterministic unit tests.
    #[cfg(test)]
    pub sandbox_platform_availability:
        Option<crate::security::sandbox::SandboxPlatformAvailability>,
}

impl CliEnv {
    /// Runs the from process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_process() -> Self {
        Self {
            home: std::env::var_os("HOME").map(PathBuf::from),
            shell: std::env::var_os("SHELL"),
            mez: std::env::var_os("MEZ"),
            runtime: RuntimeEnv::from_process(),
            #[cfg(test)]
            sandbox_platform_availability: None,
        }
    }

    /// Returns current or test-injected fixed-executable platform evidence.
    pub(super) fn sandbox_platform_availability(
        &self,
    ) -> crate::security::sandbox::SandboxPlatformAvailability {
        #[cfg(test)]
        if let Some(platform) = self.sandbox_platform_availability {
            return platform;
        }
        crate::security::sandbox::SandboxPlatformAvailability::current()
    }

    /// Runs the config paths operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn config_paths(&self) -> Result<ConfigPaths> {
        match &self.home {
            Some(home) => Ok(ConfigPaths::from_home(home.clone())),
            None => ConfigPaths::from_process_env(),
        }
    }
}

/// Runs one CLI invocation and preserves command-specific process exit status.
///
/// The complete command dispatcher is boxed because command-specific async
/// state includes foreground servers and framed management clients. Keeping
/// that state on the heap prevents unrelated short-lived CLI commands from
/// exhausting bounded caller stacks merely because they share this dispatcher.
pub fn run_with<'a, W: Write + 'a, E: Write + 'a>(
    args: Vec<String>,
    env: CliEnv,
    interactive: bool,
    stdout: &'a mut W,
    stderr: &'a mut E,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<u8>> + 'a>> {
    Box::pin(run_with_inner(args, env, interactive, stdout, stderr))
}

async fn run_with_inner<W: Write, E: Write>(
    args: Vec<String>,
    env: CliEnv,
    interactive: bool,
    stdout: &mut W,
    _stderr: &mut E,
) -> Result<u8> {
    let invocation = match CliInvocation::parse_or_display(&args, &env.runtime, env.mez.as_ref())? {
        CliInvocationParse::Invocation(invocation) => *invocation,
        CliInvocationParse::Display(display) => {
            write!(stdout, "{display}")?;
            return Ok(0);
        }
    };
    if !invocation.control_target.is_unix()
        && !matches!(
            invocation.command.as_ref(),
            Some(
                CliCommand::Attach(_)
                    | CliCommand::New(_)
                    | CliCommand::List(_)
                    | CliCommand::Kill(_)
                    | CliCommand::Detach(_)
                    | CliCommand::Lease(_)
            )
        )
    {
        return Err(MezError::invalid_args(
            "explicit Iroh targets support attach, new, list, kill, and detach",
        ));
    }
    if invocation.control_target.is_unix()
        && !matches!(invocation.command.as_ref(), Some(CliCommand::Sandbox(_)))
    {
        cleanup_startup_stale_socket_files(&invocation, env.runtime.uid)?;
    }
    let socket_selection = invocation.socket_selection;
    let control_target = invocation.control_target;
    let command = invocation.command;
    let output_format = invocation.output_format;
    let prefer_host =
        control_target.is_unix() && matches!(socket_selection, super::SocketSelection::Default(_));
    let mut exit_code = 0;

    match command {
        None => {
            if !interactive {
                return Err(MezError::forbidden(
                    "attaching as the primary client requires an interactive terminal",
                ));
            }
            if prefer_host && ensure_host_available(&env).await? {
                let socket = host_resolve_or_create_session(&env).await?;
                return run_attach(
                    &super::SocketSelection::Explicit(socket),
                    &control_target,
                    super::attach::AttachCliArgs {
                        observer: false,
                        x11: false,
                        x11_trusted: false,
                        x11_takeover: false,
                        default: false,
                        session_id: None,
                        create: false,
                        create_name: None,
                    },
                    env,
                    interactive,
                    output_format,
                    stdout,
                )
                .await
                .map(|()| 0);
            }
            let uid = env.runtime.uid;
            let registry = crate::storage::registry::SessionRegistry::new(
                super::registry_root(&socket_selection)?,
                uid,
            );
            let _ = registry.prune_stale()?;
            let sessions = registry.list()?;
            if let Some(session) = sessions.iter().find(|record| record.accepts_primary) {
                return run_attach(
                    &super::SocketSelection::Explicit(session.socket_path.clone()),
                    &control_target,
                    super::attach::AttachCliArgs {
                        observer: false,
                        x11: false,
                        x11_trusted: false,
                        x11_takeover: false,
                        default: false,
                        session_id: None,
                        create: false,
                        create_name: None,
                    },
                    env,
                    interactive,
                    output_format,
                    stdout,
                )
                .await
                .map(|()| 0);
            }
            run_new(
                &socket_selection,
                super::serve::NewCliArgs::default(),
                env,
                interactive,
                output_format,
                stdout,
            )
            .await?;
        }
        Some(CliCommand::Version) => write!(stdout, "{}", super::render_cli_version()?)?,
        Some(CliCommand::Completion(args)) => {
            let mut command = CliArgv::command();
            clap_complete::generate(args.shell, &mut command, "mez", stdout);
        }
        Some(CliCommand::Config(args)) => run_config(args, env, output_format, stdout)?,
        Some(CliCommand::Host(args)) => {
            Box::pin(run_host(args, env, output_format, stdout)).await?
        }
        Some(CliCommand::Lease(args)) => {
            Box::pin(run_lease(
                args,
                &control_target,
                &env,
                output_format,
                stdout,
            ))
            .await?
        }
        Some(CliCommand::New(args)) => {
            if !control_target.is_unix() {
                if args.dry_run {
                    return Err(MezError::invalid_args(
                        "remote session creation does not support --dry-run",
                    ));
                }
                return run_attach(
                    &socket_selection,
                    &control_target,
                    super::attach::AttachCliArgs {
                        observer: false,
                        x11: false,
                        x11_trusted: false,
                        x11_takeover: false,
                        default: false,
                        session_id: None,
                        create: true,
                        create_name: args.name,
                    },
                    env,
                    interactive,
                    output_format,
                    stdout,
                )
                .await
                .map(|()| 0);
            }
            if prefer_host && !args.dry_run && ensure_host_available(&env).await? {
                if !interactive {
                    return Err(MezError::forbidden(
                        "creating a primary-attached session requires an interactive terminal",
                    ));
                }
                let socket = host_create_session(&env, args.name.as_deref()).await?;
                run_attach(
                    &super::SocketSelection::Explicit(socket),
                    &control_target,
                    super::attach::AttachCliArgs {
                        observer: false,
                        x11: false,
                        x11_trusted: false,
                        x11_takeover: false,
                        default: false,
                        session_id: None,
                        create: false,
                        create_name: None,
                    },
                    env,
                    interactive,
                    output_format,
                    stdout,
                )
                .await?;
                return Ok(0);
            }
            run_new(
                &socket_selection,
                args,
                env,
                interactive,
                output_format,
                stdout,
            )
            .await?
        }
        Some(CliCommand::Serve(args)) => {
            run_serve(
                &socket_selection,
                args,
                env,
                interactive,
                output_format,
                stdout,
            )
            .await?
        }
        Some(CliCommand::List(args)) => {
            if !control_target.is_unix() {
                if args.all {
                    return Err(MezError::invalid_args(
                        "remote host list already returns every visible session and does not support --all",
                    ));
                }
                let body = list_iroh_host_sessions(&control_target, &env).await?;
                super::write_control_response(stdout, output_format, &body)?;
            } else if prefer_host && ensure_host_available(&env).await? {
                let sessions = host_list_sessions_with_all(&env, args.all).await?;
                let output = super::serialize_json(&sessions)?;
                super::write_json_or_plain(stdout, output_format, &output)?;
            } else {
                if args.all {
                    return Err(MezError::invalid_args(
                        "list --all requires the persistent local host",
                    ));
                }
                run_list(&socket_selection, env, output_format, stdout)?;
            }
        }
        Some(CliCommand::Attach(args)) => {
            if prefer_host && ensure_host_available(&env).await? {
                let role = if args.observer { "observer" } else { "primary" };
                let socket = host_resolve_session(&env, args.session_id.as_deref(), role).await?;
                run_attach(
                    &super::SocketSelection::Explicit(socket),
                    &control_target,
                    super::attach::AttachCliArgs {
                        observer: args.observer,
                        x11: args.x11,
                        x11_trusted: args.x11_trusted,
                        x11_takeover: args.x11_takeover,
                        default: false,
                        session_id: None,
                        create: false,
                        create_name: None,
                    },
                    env,
                    interactive,
                    output_format,
                    stdout,
                )
                .await?;
                return Ok(0);
            }
            run_attach(
                &socket_selection,
                &control_target,
                args,
                env,
                interactive,
                output_format,
                stdout,
            )
            .await?
        }
        Some(CliCommand::Detach(args)) => {
            let params = match args.client_id.as_deref() {
                Some(client_id) => format!(
                    r#"{{"idempotency_key":"{}","client_id":"{}"}}"#,
                    cli_idempotency_key("client-detach"),
                    json_escape(client_id)
                ),
                None => format!(
                    r#"{{"idempotency_key":"{}"}}"#,
                    cli_idempotency_key("client-detach")
                ),
            };
            run_control_request_for_target(
                &control_target,
                &socket_selection,
                &env,
                "client/detach",
                &params,
                output_format,
                stdout,
            )
            .await?;
        }
        Some(CliCommand::Kill(args)) => {
            let force = args.force;
            if !control_target.is_unix() {
                if !force {
                    return Err(MezError::invalid_args(
                        "remote session kill requires --force",
                    ));
                }
                let target = args.session_id.as_deref().ok_or_else(|| {
                    MezError::invalid_args(
                        "remote session kill requires a lease id, session id, or exact name",
                    )
                })?;
                let body = force_kill_iroh_host_session(&control_target, &env, target).await?;
                super::write_control_response(stdout, output_format, &body)?;
            } else {
                let socket_selection = if prefer_host && ensure_host_available(&env).await? {
                    super::SocketSelection::Explicit(
                        host_resolve_session(&env, args.session_id.as_deref(), "primary").await?,
                    )
                } else {
                    match args.session_id.as_deref() {
                        Some(session_id) => super::attach::socket_selection_for_registry_session(
                            &socket_selection,
                            env.runtime.uid,
                            session_id,
                        )?,
                        None => socket_selection,
                    }
                };
                let params = format!(
                    r#"{{"idempotency_key":"{}","force":{force}}}"#,
                    cli_idempotency_key("session-kill")
                );
                run_control_request_for_target(
                    &control_target,
                    &socket_selection,
                    &env,
                    "session/kill",
                    &params,
                    output_format,
                    stdout,
                )
                .await?;
            }
        }
        Some(CliCommand::Snapshot(args)) => {
            run_snapshot(
                args,
                env,
                &socket_selection,
                interactive,
                output_format,
                stdout,
            )
            .await?;
        }
        Some(CliCommand::Auth(args)) => {
            run_auth(args, env, interactive, output_format, stdout).await?;
        }
        Some(CliCommand::Mcp(args)) => {
            run_mcp(args, env, interactive, output_format, stdout).await?;
        }
        Some(CliCommand::Issue(args)) => {
            run_issue(args, env, output_format, stdout)?;
        }
        Some(CliCommand::Memory(args)) => {
            run_memory(args, env, output_format, stdout)?;
        }
        Some(CliCommand::SessionCatalog(args)) => {
            run_session_catalog(args, env, output_format, stdout)?;
        }
        Some(CliCommand::Remote(args)) => {
            run_remote(args, &socket_selection, &env, output_format, stdout).await?;
        }
        Some(CliCommand::Sandbox(args)) => {
            exit_code = run_sandbox(args, env, interactive, output_format, stdout)?;
        }
    }

    Ok(exit_code)
}

/// Removes unserved sockets from Mezzanine-owned runtime directories at CLI
/// startup.
///
/// # Parameters
/// - `invocation`: The parsed CLI invocation whose socket selection determines
///   the cleanup scope.
/// - `owner_uid`: The current effective user id.
fn cleanup_startup_stale_socket_files(invocation: &CliInvocation, owner_uid: u32) -> Result<()> {
    match &invocation.socket_selection {
        super::SocketSelection::Default(_)
        | super::SocketSelection::Named(_)
        | super::SocketSelection::InPane(_) => {
            let root = match &invocation.socket_selection {
                super::SocketSelection::Default(socket_path)
                | super::SocketSelection::InPane(socket_path) => {
                    socket_path.parent().map(PathBuf::from).ok_or_else(|| {
                        MezError::invalid_args(
                            "default control socket path must have a parent directory",
                        )
                    })?
                }
                super::SocketSelection::Named(_) => {
                    super::registry_root(&invocation.socket_selection)?
                }
                super::SocketSelection::Explicit(_) => {
                    unreachable!("explicit selections are handled by the outer match")
                }
            };
            ensure_private_socket_directory(&root, owner_uid)?;
            let _ = prune_stale_socket_files_in_directory(&root, owner_uid)?;
            Ok(())
        }
        super::SocketSelection::Explicit(_) => Ok(()),
    }
}
