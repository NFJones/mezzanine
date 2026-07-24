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

use super::{CliEnv, CliOutputFormat, Result, serialize_json};
use crate::config::{
    ConfigFormat, ConfigLayer, ConfigScope, DEFAULT_CONFIG_TOML, compose_effective_config,
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
}

/// Runs one read-only sandbox workflow command and returns its process status.
pub(super) fn run_sandbox<W: Write>(
    args: SandboxCliArgs,
    env: CliEnv,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<u8> {
    let (path, input_source, doctor, verbose) = match args.command {
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
        "project_root: {}\nproject_source: {}\nproject_marker: {}\ntrust_state: {}\nsandbox_configured: {}\nsandbox_effective: {}\napproval_policy: {}\nscope_provenance: {}\nbubblewrap_executable_state: {}\nbubblewrap_probe_state: {}\nmanaged_home_state: {}\nnetwork_isolated: {}\nreload_freshness: {}\n",
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
