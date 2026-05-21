//! Cli Dispatch implementation.
//!
//! This module owns the cli dispatch boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    CliInvocation, ConfigPaths, IsTerminal, MezError, OsString, Parser, PathBuf, Result,
    RuntimeEnv, Write, cli_idempotency_key, io, is_cli_help_request, json_escape, parse_cli_args,
    print_help, run_attach, run_auth, run_config, run_control_request, run_list, run_mcp,
    run_memory, run_new, run_serve, run_snapshot,
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
    stderr: &mut E,
) -> Result<()> {
    let invocation = CliInvocation::parse(&args, &env.runtime, env.mez.as_ref())?;
    let command = invocation.command.as_str();
    let command_args = invocation.args.as_slice();
    let output_format = invocation.output_format;

    match command {
        "" => {
            let uid = env.runtime.uid;
            let registry = crate::registry::SessionRegistry::new(
                super::registry_root(&invocation.socket_selection)?,
                uid,
            );
            let _ = registry.prune_stale()?;
            let sessions = registry.list()?;
            if let Some(session) = sessions.iter().find(|record| record.primary_available) {
                return run_attach(
                    &super::SocketSelection::Explicit(session.socket_path.clone()),
                    &[],
                    env,
                    interactive,
                    output_format,
                    stdout,
                )
                .await;
            }
            run_new(
                &invocation.socket_selection,
                command_args,
                env,
                interactive,
                output_format,
                stdout,
            )
            .await?;
        }
        "-h" | "--help" | "help" => print_help(stdout),
        "-V" | "--version" | "version" => writeln!(stdout, "mez {}", env!("CARGO_PKG_VERSION"))?,
        "config" => run_config(command_args, env, output_format, stdout)?,
        "new" | "new-session" => {
            run_new(
                &invocation.socket_selection,
                command_args,
                env,
                interactive,
                output_format,
                stdout,
            )
            .await?
        }
        "serve" | "daemon" => {
            run_serve(
                &invocation.socket_selection,
                command_args,
                env,
                interactive,
                output_format,
                stdout,
            )
            .await?
        }
        "list" | "list-sessions" => {
            run_list(&invocation.socket_selection, env, output_format, stdout)?
        }
        "attach" | "attach-session" => {
            run_attach(
                &invocation.socket_selection,
                command_args,
                env,
                interactive,
                output_format,
                stdout,
            )
            .await?
        }
        "detach" | "detach-client" => {
            if is_cli_help_request(command_args) {
                writeln!(stdout, "usage: mez detach [--client-id ID]")?;
                return Ok(());
            }
            let parsed = parse_cli_args::<DetachCliArgs>("mez detach", command_args)?;
            let params = match parsed.client_id.as_deref() {
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
                &invocation.socket_selection,
                "client/detach",
                &params,
                output_format,
                stdout,
            )?;
        }
        "kill-session" => {
            if is_cli_help_request(command_args) {
                writeln!(stdout, "usage: mez kill-session [--force]")?;
                return Ok(());
            }
            let parsed = parse_cli_args::<KillSessionCliArgs>("mez kill-session", command_args)?;
            let force = parsed.force;
            let params = format!(
                r#"{{"idempotency_key":"{}","force":{force}}}"#,
                cli_idempotency_key("session-kill")
            );
            run_control_request(
                &invocation.socket_selection,
                "session/kill",
                &params,
                output_format,
                stdout,
            )?;
        }
        "snapshot" => {
            run_snapshot(
                command_args,
                env,
                &invocation.socket_selection,
                interactive,
                output_format,
                stdout,
            )
            .await?;
        }
        "auth" => {
            run_auth(command_args, env, interactive, output_format, stdout).await?;
        }
        "mcp" => run_mcp(command_args, env, output_format, stdout)?,
        "memory" => {
            run_memory(command_args, env, output_format, stdout)?;
        }
        unknown => {
            writeln!(stderr, "unknown command: {unknown}")?;
            return Err(MezError::invalid_args("run `mez help` for usage"));
        }
    }

    Ok(())
}

/// Typed process CLI arguments for `mez detach`.
#[derive(Debug, Parser)]
#[command(
    name = "mez detach",
    disable_help_flag = true,
    disable_help_subcommand = true
)]
struct DetachCliArgs {
    /// Control client id to detach instead of the current client.
    #[arg(long, value_name = "ID")]
    client_id: Option<String>,
}

/// Typed process CLI arguments for `mez kill-session`.
#[derive(Debug, Parser)]
#[command(
    name = "mez kill-session",
    disable_help_flag = true,
    disable_help_subcommand = true
)]
struct KillSessionCliArgs {
    /// Confirms intentional termination of the live session.
    #[arg(short, long)]
    force: bool,
}
