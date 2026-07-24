//! Direct-user sandbox status and diagnostic commands.
//!
//! These commands assemble the same typed sandbox workflow projection used by
//! future setup operations while remaining strictly read-only. They do not
//! migrate configuration, mutate trust, create managed homes, or populate a
//! pane-specific Bubblewrap capability cache.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};

use super::{CliEnv, CliOutputFormat, MezError, Result, Serialize, serialize_json};
use crate::config::{
    ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation, ConfigMutationValue,
    ConfigScope, DEFAULT_CONFIG_TOML, compose_effective_config, persist_config_mutation,
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
    /// Detects or enables allowlisted read-only developer toolchains.
    Toolchains {
        /// Toolchain workflow to run.
        #[command(subcommand)]
        command: SandboxToolchainsCommand,
    },
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
