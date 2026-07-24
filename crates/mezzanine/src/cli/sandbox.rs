//! Direct-user sandbox status and diagnostic commands.
//!
//! These commands assemble the same typed sandbox workflow projection used by
//! future setup operations while remaining strictly read-only. They do not
//! migrate configuration, mutate trust, create managed homes, or populate a
//! pane-specific Bubblewrap capability cache.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use rustix::fs::{FlockOperation, flock};

use super::{CliEnv, CliOutputFormat, MezError, Result, Serialize, serialize_json};
use crate::config::{
    ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation, ConfigMutationValue,
    ConfigScope, DEFAULT_CONFIG_TOML, compose_effective_config, persist_config_mutation,
    persist_config_text, plan_config_mutations,
};
use crate::runtime::{runtime_configured_permissions_from_config, runtime_effective_config_value};
use crate::security::project::{
    ProjectRootInputSource, ProjectRootMarkerKind, ProjectTrustStore, TrustDecision,
    default_trust_database_path, discover_existing_overlays, discover_project_root_with_metadata,
};
use crate::security::sandbox::{
    SandboxDiagnosticSeverity, SandboxWorkflowPlan, SandboxWorkflowRequest, plan_sandbox_workflow,
};

/// Typed arguments accepted by `mez sandbox`.
#[derive(Debug, Clone, Args)]
pub(super) struct SandboxCliArgs {
    /// Optional sandbox subcommand, defaulting to `status`.
    #[command(subcommand)]
    command: Option<SandboxCliCommand>,
}

/// Read-only sandbox workflow commands.
#[derive(Debug, Clone, Subcommand)]
enum SandboxCliCommand {
    /// Reports configured and effective sandbox state.
    Status {
        /// Project path to inspect instead of the current directory.
        path: Option<PathBuf>,
        /// Includes stable diagnostics and remedies in plain output.
        #[arg(long)]
        verbose: bool,
    },
    /// Diagnoses sandbox readiness without changing local state.
    Doctor {
        /// Project path to inspect instead of the current directory.
        path: Option<PathBuf>,
    },
    /// Previews a code-owned sandbox preset without changing local state.
    Plan(SandboxSetupArgs),
    /// Applies a code-owned sandbox preset after direct-user confirmation.
    Enable(SandboxSetupArgs),
    /// Applies one named code-owned sandbox preset.
    Preset {
        /// Preset workflow to run.
        #[command(subcommand)]
        command: SandboxPresetCommand,
    },
    /// Selects policy-only execution while retaining other sandbox settings.
    Disable(SandboxMutationArgs),
    /// Trusts the independently discovered current project.
    TrustCurrentProject(SandboxMutationArgs),
    /// Detects or enables allowlisted read-only developer toolchains.
    Toolchains {
        /// Toolchain workflow to run.
        #[command(subcommand)]
        command: SandboxToolchainsCommand,
    },
}

/// Shared preview and mutation arguments for guided sandbox setup.
#[derive(Debug, Clone, Args)]
struct SandboxSetupArgs {
    /// Code-owned preset: project-safe, project-auto, or project-read-only.
    #[arg(long, default_value = "project-safe")]
    preset: String,
    /// Authority source: trusted-project or explicit-scope.
    #[arg(long)]
    authority: Option<String>,
    /// Project path to resolve independently instead of the current directory.
    #[arg(long)]
    path: Option<PathBuf>,
    /// Builds and displays the complete plan without persisting it.
    #[arg(long)]
    dry_run: bool,
    /// Confirms every planned mutation.
    #[arg(long)]
    yes: bool,
}

/// Shared confirmation arguments for one guided mutation.
#[derive(Debug, Clone, Args)]
struct SandboxMutationArgs {
    /// Builds and displays the complete plan without persisting it.
    #[arg(long)]
    dry_run: bool,
    /// Confirms every planned mutation.
    #[arg(long)]
    yes: bool,
}

/// Named preset application commands.
#[derive(Debug, Clone, Subcommand)]
enum SandboxPresetCommand {
    /// Applies one code-owned preset by stable name.
    Apply(SandboxSetupArgs),
}

/// Normalized guided setup operation dispatched to the transactional owner.
#[derive(Debug, Clone)]
enum SandboxSetupCommand {
    /// Previews one preset without persistence.
    Plan(SandboxSetupArgs),
    /// Applies one preset after confirmation.
    Enable(SandboxSetupArgs),
    /// Selects policy-only execution while retaining other settings.
    Disable(SandboxMutationArgs),
    /// Trusts the independently discovered current project.
    TrustCurrentProject(SandboxMutationArgs),
}

/// Stable preview and application result for guided sandbox setup.
#[derive(Debug, Serialize)]
struct SandboxSetupResult {
    version: u32,
    project_root: PathBuf,
    preset: String,
    authority: String,
    mutations: Vec<String>,
    trust_current_project: bool,
    confirmation_required: bool,
    dry_run: bool,
    applied: bool,
    warning: Option<String>,
}

/// Direct-user Rust toolchain discovery and activation commands.
#[derive(Debug, Clone, Subcommand)]
enum SandboxToolchainsCommand {
    /// Detects canonical allowlisted toolchain roots without changing config.
    Detect {
        /// Project path reported with the detection result.
        path: Option<PathBuf>,
    },
    /// Enables one or more allowlisted toolchain kinds in user config.
    Enable {
        /// Allowlisted toolchain kinds; currently only `rust` is supported.
        #[arg(required = true)]
        kinds: Vec<String>,
        /// Confirms the read-only host path projection.
        #[arg(long)]
        yes: bool,
    },
}

/// Runs one read-only sandbox workflow command and returns its process status.
pub(super) fn run_sandbox<W: Write>(
    args: SandboxCliArgs,
    env: CliEnv,
    interactive: bool,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<u8> {
    let (path, input_source, doctor, verbose) = match args.command {
        Some(SandboxCliCommand::Toolchains { command }) => {
            return run_sandbox_toolchains(command, env, interactive, output_format, stdout);
        }
        Some(SandboxCliCommand::Plan(args)) => {
            return run_sandbox_setup(
                SandboxSetupCommand::Plan(args),
                env,
                interactive,
                output_format,
                stdout,
            );
        }
        Some(SandboxCliCommand::Enable(args)) => {
            return run_sandbox_setup(
                SandboxSetupCommand::Enable(args),
                env,
                interactive,
                output_format,
                stdout,
            );
        }
        Some(SandboxCliCommand::Preset {
            command: SandboxPresetCommand::Apply(args),
        }) => {
            return run_sandbox_setup(
                SandboxSetupCommand::Enable(args),
                env,
                interactive,
                output_format,
                stdout,
            );
        }
        Some(SandboxCliCommand::Disable(args)) => {
            return run_sandbox_setup(
                SandboxSetupCommand::Disable(args),
                env,
                interactive,
                output_format,
                stdout,
            );
        }
        Some(SandboxCliCommand::TrustCurrentProject(args)) => {
            return run_sandbox_setup(
                SandboxSetupCommand::TrustCurrentProject(args),
                env,
                interactive,
                output_format,
                stdout,
            );
        }
        None => (
            std::env::current_dir()?,
            ProjectRootInputSource::CurrentDirectory,
            false,
            false,
        ),
        Some(SandboxCliCommand::Status { path, verbose }) => match path {
            Some(path) => (path, ProjectRootInputSource::ExplicitPath, false, verbose),
            None => (
                std::env::current_dir()?,
                ProjectRootInputSource::CurrentDirectory,
                false,
                verbose,
            ),
        },
        Some(SandboxCliCommand::Doctor { path }) => match path {
            Some(path) => (path, ProjectRootInputSource::ExplicitPath, true, true),
            None => (
                std::env::current_dir()?,
                ProjectRootInputSource::CurrentDirectory,
                true,
                true,
            ),
        },
    };
    let paths = env.config_paths()?;
    let discovery = discover_project_root_with_metadata(&path, input_source)?;
    let trust_store =
        ProjectTrustStore::load_from_file(&default_trust_database_path(paths.root()))?;
    let git_marker = match discovery.marker_kind {
        ProjectRootMarkerKind::GitDirectory | ProjectRootMarkerKind::GitFile => {
            Some(discovery.canonical_root.join(".git"))
        }
        ProjectRootMarkerKind::Fallback => None,
    };
    let trust_state = trust_store
        .get_for_project(&discovery.canonical_root, git_marker.as_deref())
        .map_or(TrustDecision::Pending, |record| record.state);
    let layers = load_read_only_config_layers(
        &paths,
        &discovery.canonical_root,
        &discovery.canonical_start,
        trust_state == TrustDecision::Trusted,
    )?;
    let effective = compose_effective_config(&layers)?;
    let structured = runtime_effective_config_value(&layers)?;
    let permissions = runtime_configured_permissions_from_config(&structured)?;
    let plan = plan_sandbox_workflow(SandboxWorkflowRequest {
        permissions: &permissions,
        discovery: &discovery,
        trust_state,
        config_root: paths.root(),
        sandbox_source: effective
            .source_for("permissions.sandbox")
            .unwrap_or("default"),
        approval_policy_source: effective
            .source_for("permissions.approval_policy")
            .unwrap_or("default"),
        read_scopes_source: effective
            .source_for("permissions.read_scopes")
            .unwrap_or("default"),
        write_scopes_source: effective
            .source_for("permissions.write_scopes")
            .unwrap_or("default"),
    });

    if output_format.is_json() {
        writeln!(stdout, "{}", serialize_json(&plan)?)?;
    } else {
        write!(stdout, "{}", sandbox_plan_plain_text(&plan, verbose))?;
    }
    Ok(if doctor { plan.doctor_exit_code() } else { 0 })
}

/// Loads effective local configuration without migrations or persistence.
fn load_read_only_config_layers(
    paths: &crate::config::ConfigPaths,
    project_root: &Path,
    current_dir: &Path,
    trusted: bool,
) -> Result<Vec<ConfigLayer>> {
    let mut layers = Vec::new();
    if let Some(path) = paths.select_primary_file()? {
        layers.push(ConfigLayer {
            name: "primary".to_string(),
            format: ConfigFormat::from_path(&path)?,
            text: fs::read_to_string(&path)?,
            path: Some(path),
            scope: ConfigScope::Primary,
            trusted: true,
        });
    } else {
        layers.push(ConfigLayer {
            name: "primary".to_string(),
            format: ConfigFormat::Toml,
            text: DEFAULT_CONFIG_TOML.to_string(),
            path: None,
            scope: ConfigScope::Primary,
            trusted: true,
        });
    }
    let overlays = discover_existing_overlays(project_root, current_dir)?;
    let overlay_count = overlays.len();
    for (index, path) in overlays.into_iter().enumerate() {
        layers.push(ConfigLayer {
            name: if overlay_count == 1 {
                "project".to_string()
            } else {
                format!("project:{}", index + 1)
            },
            format: ConfigFormat::from_path(&path)?,
            text: fs::read_to_string(&path)?,
            path: Some(path),
            scope: ConfigScope::ProjectOverlay,
            trusted,
        });
    }
    Ok(layers)
}

/// Plans or applies one code-owned guided sandbox setup transaction.
fn run_sandbox_setup<W: Write>(
    command: SandboxSetupCommand,
    env: CliEnv,
    interactive: bool,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<u8> {
    let (preset, authority, path, dry_run, yes, force_read_only, trust_only) = match command {
        SandboxSetupCommand::Plan(args) => (
            args.preset,
            args.authority,
            args.path,
            true,
            false,
            true,
            false,
        ),
        SandboxSetupCommand::Enable(args) => (
            args.preset,
            args.authority,
            args.path,
            args.dry_run,
            args.yes,
            false,
            false,
        ),
        SandboxSetupCommand::Disable(args) => (
            "off".to_string(),
            Some("retained".to_string()),
            None,
            args.dry_run,
            args.yes,
            false,
            false,
        ),
        SandboxSetupCommand::TrustCurrentProject(args) => (
            "trust-current-project".to_string(),
            Some("trusted-project".to_string()),
            None,
            args.dry_run,
            args.yes,
            false,
            true,
        ),
    };
    let path = path.unwrap_or(std::env::current_dir()?);
    let discovery = discover_project_root_with_metadata(
        &path,
        if path == std::env::current_dir()? {
            ProjectRootInputSource::CurrentDirectory
        } else {
            ProjectRootInputSource::ExplicitPath
        },
    )?;
    let authority = match authority {
        Some(authority) => authority,
        None if interactive && !output_format.is_json() => "trusted-project".to_string(),
        None => {
            return Err(MezError::invalid_args(
                "noninteractive sandbox setup requires --authority trusted-project|explicit-scope",
            ));
        }
    };
    let project = discovery.canonical_root.to_string_lossy().into_owned();
    let mut trust_current_project = trust_only;
    let mutations = if trust_only {
        Vec::new()
    } else {
        sandbox_setup_mutations(&preset, &authority, &project, &mut trust_current_project)?
    };
    let paths = env.config_paths()?;
    let config_path = paths
        .select_primary_file()?
        .unwrap_or_else(|| paths.default_primary_file());
    let original_exists = config_path.is_file();
    let original_text = if original_exists {
        fs::read_to_string(&config_path)?
    } else {
        DEFAULT_CONFIG_TOML.to_string()
    };
    let batch = plan_config_mutations(
        ConfigFormat::from_path(&config_path)?,
        &original_text,
        ConfigScope::Primary,
        mutations,
    )?;
    let warning = if trust_current_project {
        Some(
            "Trusting this project may activate applicable project overlays, macros, and skills."
                .to_string(),
        )
    } else if discovery.marker_kind == ProjectRootMarkerKind::Fallback {
        Some("The selected authority uses a non-repository fallback root.".to_string())
    } else {
        None
    };
    let mut result = SandboxSetupResult {
        version: 1,
        project_root: discovery.canonical_root.clone(),
        preset,
        authority,
        mutations: batch
            .mutations
            .iter()
            .map(|mutation| mutation.path.clone())
            .collect(),
        trust_current_project,
        confirmation_required: !force_read_only,
        dry_run: dry_run || force_read_only,
        applied: false,
        warning,
    };
    if force_read_only || dry_run || !yes {
        write_setup_result(stdout, output_format, &result)?;
        return Ok(if force_read_only || dry_run { 0 } else { 1 });
    }

    if !original_exists {
        paths.ensure_default_config()?;
    }
    let _transaction_lock = acquire_sandbox_setup_lock(paths.root())?;
    let current_text = fs::read_to_string(&config_path)?;
    if current_text != original_text {
        return Err(MezError::conflict(
            "sandbox setup configuration changed after planning; rerun the command",
        ));
    }
    if batch.changed {
        persist_config_text(&config_path, ConfigScope::Primary, &batch.text)?;
    }
    if trust_current_project {
        let trust_path = default_trust_database_path(paths.root());
        let trust_result = (|| -> Result<()> {
            let mut store = ProjectTrustStore::load_from_file(&trust_path)?;
            let git_marker = match discovery.marker_kind {
                ProjectRootMarkerKind::GitDirectory | ProjectRootMarkerKind::GitFile => {
                    Some(discovery.canonical_root.join(".git"))
                }
                ProjectRootMarkerKind::Fallback => None,
            };
            store.decide(
                discovery.canonical_root.clone(),
                TrustDecision::Trusted,
                git_marker,
            )?;
            store.save_to_file(&trust_path)
        })();
        if let Err(error) = trust_result {
            let rollback = if original_exists {
                persist_config_text(&config_path, ConfigScope::Primary, &original_text)
            } else {
                fs::remove_file(&config_path).map_err(MezError::from)
            };
            return match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(MezError::invalid_state(format!(
                    "project trust persistence failed and config rollback also failed: {error}; rollback: {rollback_error}"
                ))),
            };
        }
    }
    result.applied = true;
    result.confirmation_required = false;
    write_setup_result(stdout, output_format, &result)?;
    Ok(0)
}

/// Exclusive process lock serializing guided config and trust persistence.
struct SandboxSetupTransactionLock {
    _file: fs::File,
}

fn acquire_sandbox_setup_lock(config_root: &Path) -> Result<SandboxSetupTransactionLock> {
    let lock_path = config_root.join(".sandbox-setup.lock");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))?;
    flock(&file, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
    Ok(SandboxSetupTransactionLock { _file: file })
}

fn sandbox_setup_mutations(
    preset: &str,
    authority: &str,
    project: &str,
    trust_current_project: &mut bool,
) -> Result<Vec<ConfigMutation>> {
    let mut mutations = Vec::new();
    if preset == "off" {
        mutations.push(set_setup_string("permissions.sandbox", "policy-only"));
        return Ok(mutations);
    }
    let approval_policy = match preset {
        "project-safe" | "project-read-only" => "ask",
        "project-auto" => "auto-allow",
        _ => {
            return Err(MezError::invalid_args(
                "sandbox preset must be project-safe, project-auto, project-read-only, or off",
            ));
        }
    };
    mutations.push(set_setup_string("permissions.sandbox", "bubblewrap"));
    mutations.push(set_setup_string(
        "permissions.approval_policy",
        approval_policy,
    ));
    if preset == "project-read-only" {
        mutations.push(set_setup_array("permissions.read_scopes", &[project]));
        mutations.push(set_setup_array("permissions.write_scopes", &[]));
        return Ok(mutations);
    }
    match authority {
        "trusted-project" => {
            *trust_current_project = true;
            mutations.push(unset_setup("permissions.read_scopes"));
            mutations.push(unset_setup("permissions.write_scopes"));
        }
        "explicit-scope" => {
            mutations.push(set_setup_array("permissions.read_scopes", &[project]));
            mutations.push(set_setup_array("permissions.write_scopes", &[project]));
        }
        _ => {
            return Err(MezError::invalid_args(
                "sandbox authority must be trusted-project or explicit-scope",
            ));
        }
    }
    Ok(mutations)
}

fn set_setup_string(path: &str, value: &str) -> ConfigMutation {
    ConfigMutation {
        path: path.to_string(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::String(value.to_string())),
    }
}

fn set_setup_array(path: &str, values: &[&str]) -> ConfigMutation {
    ConfigMutation {
        path: path.to_string(),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(
            values.iter().map(|value| (*value).to_string()).collect(),
        )),
    }
}

fn unset_setup(path: &str) -> ConfigMutation {
    ConfigMutation {
        path: path.to_string(),
        operation: ConfigMutationOperation::Unset,
    }
}

fn write_setup_result<W: Write>(
    stdout: &mut W,
    output_format: CliOutputFormat,
    result: &SandboxSetupResult,
) -> Result<()> {
    if output_format.is_json() {
        writeln!(stdout, "{}", serialize_json(result)?)?;
    } else {
        writeln!(stdout, "preset: {}", result.preset)?;
        writeln!(stdout, "authority: {}", result.authority)?;
        writeln!(stdout, "project_root: {}", result.project_root.display())?;
        writeln!(stdout, "mutations: {}", result.mutations.join(","))?;
        writeln!(
            stdout,
            "trust_current_project: {}",
            result.trust_current_project
        )?;
        writeln!(
            stdout,
            "confirmation_required: {}",
            result.confirmation_required
        )?;
        writeln!(stdout, "dry_run: {}", result.dry_run)?;
        writeln!(stdout, "applied: {}", result.applied)?;
        if let Some(warning) = &result.warning {
            writeln!(stdout, "warning: {warning}")?;
        }
    }
    Ok(())
}

fn sandbox_plan_plain_text(plan: &SandboxWorkflowPlan, verbose: bool) -> String {
    let mut output = format!(
        "project_root: {}\nproject_source: {}\nproject_marker: {}\ntrust_state: {}\nsandbox_configured: {}\nsandbox_effective: {}\napproval_policy: {}\nscope_provenance: {}\nbubblewrap_executable_state: {}\nbubblewrap_probe_state: {}\nmanaged_home_state: {}\ntoolchains: {}\ntoolchain_state: {}\nnetwork_isolated: {}\nreload_freshness: {}\n",
        plan.project.canonical_root.display(),
        plan.project.input_source,
        plan.project.marker_kind,
        plan.project.trust_state,
        plan.configured.sandbox,
        plan.effective.sandbox,
        plan.configured.approval_policy,
        plan.effective.scope_provenance,
        plan.effective.bubblewrap_executable_state,
        plan.effective.bubblewrap_probe_state,
        plan.effective.managed_home_state,
        if plan.configured.toolchains.is_empty() {
            "none".to_string()
        } else {
            plan.configured.toolchains.join(",")
        },
        plan.effective.toolchain_state,
        plan.effective.network_isolated,
        plan.effective.reload_freshness,
    );
    if verbose {
        for diagnostic in &plan.diagnostics {
            let severity = match diagnostic.severity {
                SandboxDiagnosticSeverity::Info => "info",
                SandboxDiagnosticSeverity::Warning => "warning",
                SandboxDiagnosticSeverity::Error => "error",
            };
            output.push_str(&format!(
                "diagnostic: {} severity={} summary={} remedy={}\n",
                diagnostic.id, severity, diagnostic.summary, diagnostic.remedy
            ));
        }
    }
    output
}

/// Stable direct-user projection for Rust toolchain detection and activation.
#[derive(Debug, Serialize)]
struct SandboxToolchainResult {
    version: u32,
    project_root: PathBuf,
    kind: &'static str,
    available: bool,
    cargo_bin: Option<PathBuf>,
    rustup_home: Option<PathBuf>,
    sandbox_path: &'static str,
    read_only: bool,
    applied: bool,
    confirmation_required: bool,
    message: String,
}

fn run_sandbox_toolchains<W: Write>(
    command: SandboxToolchainsCommand,
    env: CliEnv,
    interactive: bool,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<u8> {
    match command {
        SandboxToolchainsCommand::Detect { path } => {
            let input_source = if path.is_some() {
                ProjectRootInputSource::ExplicitPath
            } else {
                ProjectRootInputSource::CurrentDirectory
            };
            let path = path.unwrap_or(std::env::current_dir()?);
            let project = discover_project_root_with_metadata(&path, input_source)?;
            let detection = detect_rust_toolchain(env.home.as_deref())?;
            let result = toolchain_result(project.canonical_root, detection, false, false);
            write_toolchain_result(stdout, output_format, &result)?;
            Ok(0)
        }
        SandboxToolchainsCommand::Enable { kinds, yes } => {
            if kinds.iter().any(|kind| kind != "rust") {
                return Err(MezError::invalid_args(
                    "sandbox toolchains enable currently supports only rust",
                ));
            }
            let project = discover_project_root_with_metadata(
                &std::env::current_dir()?,
                ProjectRootInputSource::CurrentDirectory,
            )?;
            let detection = detect_rust_toolchain(env.home.as_deref())?;
            if !detection.available {
                return Err(MezError::invalid_state(
                    "Rust toolchain detection requires canonical .cargo and .rustup directories",
                ));
            }
            if !yes {
                let mut result = toolchain_result(project.canonical_root, detection, false, true);
                result.message = if interactive {
                    "Review the read-only roots and rerun with --yes to enable Rust.".to_string()
                } else {
                    "Noninteractive toolchain mutation requires --yes.".to_string()
                };
                write_toolchain_result(stdout, output_format, &result)?;
                return Ok(1);
            }
            let paths = env.config_paths()?;
            let config_path = paths.ensure_default_config()?;
            persist_config_mutation(
                &config_path,
                ConfigScope::Primary,
                ConfigMutation {
                    path: "permissions.bubblewrap.toolchains".to_string(),
                    operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(
                        vec!["rust".to_string()],
                    )),
                },
            )?;
            let result = toolchain_result(project.canonical_root, detection, true, false);
            write_toolchain_result(stdout, output_format, &result)?;
            Ok(0)
        }
    }
}

#[derive(Debug)]
struct RustToolchainDetection {
    available: bool,
    cargo_bin: Option<PathBuf>,
    rustup_home: Option<PathBuf>,
}

fn detect_rust_toolchain(home: Option<&Path>) -> Result<RustToolchainDetection> {
    let Some(home) = home else {
        return Ok(RustToolchainDetection {
            available: false,
            cargo_bin: None,
            rustup_home: None,
        });
    };
    let cargo_bin = canonical_toolchain_root(&home.join(".cargo/bin"), "bin")?;
    let rustup_home = canonical_toolchain_root(&home.join(".rustup"), ".rustup")?;
    Ok(RustToolchainDetection {
        available: cargo_bin.is_some() && rustup_home.is_some(),
        cargo_bin,
        rustup_home,
    })
}

fn canonical_toolchain_root(path: &Path, expected_name: &str) -> Result<Option<PathBuf>> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(None);
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(MezError::invalid_state(format!(
            "toolchain root {} must be a real directory, not a symlink",
            path.display()
        )));
    }
    let canonical = path.canonicalize()?;
    if canonical.file_name().and_then(|name| name.to_str()) != Some(expected_name) {
        return Err(MezError::invalid_state(
            "detected toolchain root has an unexpected canonical directory name",
        ));
    }
    Ok(Some(canonical))
}

fn toolchain_result(
    project_root: PathBuf,
    detection: RustToolchainDetection,
    applied: bool,
    confirmation_required: bool,
) -> SandboxToolchainResult {
    SandboxToolchainResult {
        version: 1,
        project_root,
        kind: "rust",
        available: detection.available,
        cargo_bin: detection.cargo_bin,
        rustup_home: detection.rustup_home,
        sandbox_path: "/opt/mez/toolchains/rust/cargo-bin:/usr/bin:/bin",
        read_only: true,
        applied,
        confirmation_required,
        message: if applied {
            "Rust toolchain projection enabled; live sessions require reload.".to_string()
        } else {
            "Rust toolchain detection completed without changing configuration.".to_string()
        },
    }
}

fn write_toolchain_result<W: Write>(
    stdout: &mut W,
    output_format: CliOutputFormat,
    result: &SandboxToolchainResult,
) -> Result<()> {
    if output_format.is_json() {
        writeln!(stdout, "{}", serialize_json(result)?)?;
    } else {
        writeln!(stdout, "kind: {}", result.kind)?;
        writeln!(stdout, "available: {}", result.available)?;
        writeln!(
            stdout,
            "cargo_bin: {}",
            result
                .cargo_bin
                .as_deref()
                .map_or_else(|| "none".to_string(), |path| path.display().to_string())
        )?;
        writeln!(
            stdout,
            "rustup_home: {}",
            result
                .rustup_home
                .as_deref()
                .map_or_else(|| "none".to_string(), |path| path.display().to_string())
        )?;
        writeln!(stdout, "sandbox_path: {}", result.sandbox_path)?;
        writeln!(stdout, "read_only: {}", result.read_only)?;
        writeln!(stdout, "applied: {}", result.applied)?;
        writeln!(
            stdout,
            "confirmation_required: {}",
            result.confirmation_required
        )?;
        writeln!(stdout, "message: {}", result.message)?;
    }
    Ok(())
}
