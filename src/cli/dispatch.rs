//! Cli Dispatch implementation.
//!
//! This module owns the cli dispatch boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    CliCommand, CliInvocation, CliInvocationParse, ConfigPaths, IsTerminal, MezError, OsString,
    PathBuf, Result, RuntimeEnv, Write, cli_idempotency_key, io, json_escape,
    prune_stale_socket_files_in_directory, run_attach, run_auth, run_config, run_control_request,
    run_issue, run_list, run_mcp, run_memory, run_new, run_serve, run_snapshot,
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
        Ok(()) => 0,
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
        }
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

/// Runs the run with operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn run_with<W: Write, E: Write>(
    args: Vec<String>,
    env: CliEnv,
    interactive: bool,
    stdout: &mut W,
    _stderr: &mut E,
) -> Result<()> {
    let invocation = match CliInvocation::parse_or_display(&args, &env.runtime, env.mez.as_ref())? {
        CliInvocationParse::Invocation(invocation) => *invocation,
        CliInvocationParse::Display(display) => {
            write!(stdout, "{display}")?;
            return Ok(());
        }
    };
    cleanup_startup_stale_socket_files(&invocation, env.runtime.uid)?;
    let socket_selection = invocation.socket_selection;
    let command = invocation.command;
    let output_format = invocation.output_format;

    match command {
        None => {
            let uid = env.runtime.uid;
            let registry = crate::registry::SessionRegistry::new(
                super::registry_root(&socket_selection)?,
                uid,
            );
            let _ = registry.prune_stale()?;
            let sessions = registry.list()?;
            if let Some(session) = sessions.iter().find(|record| record.primary_available) {
                return run_attach(
                    &super::SocketSelection::Explicit(session.socket_path.clone()),
                    super::attach::AttachCliArgs {
                        observer: false,
                        session_id: None,
                    },
                    env,
                    interactive,
                    output_format,
                    stdout,
                )
                .await;
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
        Some(CliCommand::Config(args)) => run_config(args, env, output_format, stdout)?,
        Some(CliCommand::New(args)) => {
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
        Some(CliCommand::List) => run_list(&socket_selection, env, output_format, stdout)?,
        Some(CliCommand::Attach(args)) => {
            run_attach(
                &socket_selection,
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
            run_control_request(
                &socket_selection,
                "client/detach",
                &params,
                output_format,
                stdout,
            )?;
        }
        Some(CliCommand::KillSession(args)) => {
            let force = args.force;
            let params = format!(
                r#"{{"idempotency_key":"{}","force":{force}}}"#,
                cli_idempotency_key("session-kill")
            );
            run_control_request(
                &socket_selection,
                "session/kill",
                &params,
                output_format,
                stdout,
            )?;
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
    }

    Ok(())
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
            let _ = prune_stale_socket_files_in_directory(&root, owner_uid)?;
            Ok(())
        }
        super::SocketSelection::Explicit(_) => Ok(()),
    }
}
