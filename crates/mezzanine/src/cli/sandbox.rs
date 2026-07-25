//! Direct-user sandbox status and setup commands.
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
use serde::Deserialize;

use super::{CliEnv, CliOutputFormat, MezError, Result, Serialize, serialize_json};
use crate::config::{
    ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation, ConfigMutationValue,
    ConfigScope, DEFAULT_CONFIG_TOML, compose_effective_config, persist_config_mutation,
    persist_config_text, plan_config_mutations,
};
use crate::runtime::{
    SandboxToolchainKind, runtime_configured_permissions_from_config,
    runtime_effective_config_value,
};
use crate::security::project::{
    ProjectRootInputSource, ProjectRootMarkerKind, ProjectTrustStore, TrustDecision,
    default_trust_database_path, discover_existing_overlays, discover_project_root_with_metadata,
};
use crate::security::sandbox::{
    BubblewrapManagedHomeMaintenance, RustToolchainHomeDiscovery, SANDBOX_BUN_PATH,
    SANDBOX_DENO_PATH, SANDBOX_GO_PATH, SANDBOX_RUST_PATH, SANDBOX_ZIG_PATH,
    SandboxDiagnosticSeverity, SandboxWorkflowPlan, SandboxWorkflowRequest,
    clear_bubblewrap_managed_home, discover_bun_from_search_path, discover_deno_from_search_path,
    discover_go_from_search_path, discover_rust_from_home, discover_zig_from_search_path,
    inspect_bubblewrap_managed_home, parse_sandbox_toolchain_kind, plan_sandbox_workflow,
    prune_bubblewrap_managed_homes,
};

/// Typed arguments accepted by `mez sandbox`.
#[derive(Debug, Clone, Args)]
pub(super) struct SandboxCliArgs {
    /// Optional sandbox subcommand, defaulting to `status`.
    #[command(subcommand)]
    command: Option<SandboxCliCommand>,
}

/// Sandbox workflow commands.
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
    /// Imports or exports a sanitized, versioned sandbox setup recipe.
    Profile {
        /// Profile workflow to run.
        #[command(subcommand)]
        command: SandboxProfileCommand,
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
    /// Inspects or safely removes Mezzanine-managed Bubblewrap homes.
    Cache {
        /// Managed-home cache workflow to run.
        #[command(subcommand)]
        command: SandboxCacheCommand,
    },
}

/// Direct-user managed-home inspection and maintenance commands.
#[derive(Debug, Clone, Subcommand)]
enum SandboxCacheCommand {
    /// Reports usage and activity for the selected project's managed home.
    Status {
        /// Project path to inspect instead of the current directory.
        path: Option<PathBuf>,
    },
    /// Removes the selected project's inactive managed home.
    Clear {
        /// Project path to clear instead of the current directory.
        path: Option<PathBuf>,
        /// Reports the deletion candidate without removing it.
        #[arg(long)]
        dry_run: bool,
        /// Confirms deletion after reviewing the candidate.
        #[arg(long)]
        yes: bool,
    },
    /// Removes every inactive managed home.
    Prune {
        /// Reports all deletion candidates without removing them.
        #[arg(long)]
        dry_run: bool,
        /// Confirms deletion after reviewing all candidates.
        #[arg(long)]
        yes: bool,
    },
}

/// Stable projection returned by managed-home cache workflows.
#[derive(Debug, Serialize)]
struct SandboxCacheResult {
    version: u32,
    operation: &'static str,
    project_root: Option<PathBuf>,
    homes: Vec<BubblewrapManagedHomeMaintenance>,
    total_bytes: u64,
    active_homes: usize,
    candidate_homes: usize,
    removed_homes: usize,
    dry_run: bool,
    confirmation_required: bool,
    applied: bool,
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

/// Sanitized sandbox setup profile commands.
#[derive(Debug, Clone, Subcommand)]
enum SandboxProfileCommand {
    /// Exports the effective safe preset fields as deterministic JSON.
    Export {
        /// Project path to inspect instead of the current directory.
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Previews or imports one strict JSON recipe.
    Import {
        /// Recipe file to review and import.
        file: PathBuf,
        /// Local project path resolved independently from recipe contents.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Builds the complete local plan without persistence.
        #[arg(long)]
        dry_run: bool,
        /// Confirms the local policy mutation.
        #[arg(long)]
        yes: bool,
    },
}

/// Portable recipe containing only allowlisted sandbox setup selections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SandboxProfileRecipe {
    /// Recipe contract version.
    version: u32,
    /// Code-owned preset name.
    preset: String,
    /// Local authority strategy, never a host path.
    authority: String,
    /// Allowlisted typed toolchain kinds.
    #[serde(default)]
    toolchains: Vec<String>,
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
    /// Imports one reviewed sanitized profile.
    ProfileImport(SandboxSetupArgs, Vec<String>),
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

/// Direct-user typed toolchain discovery and activation commands.
#[derive(Debug, Clone, Subcommand)]
enum SandboxToolchainsCommand {
    /// Detects canonical allowlisted toolchain roots without changing config.
    Detect {
        /// Allowlisted kind to inspect; omitted for backwards-compatible Rust detection.
        #[arg(long, default_value = "rust")]
        kind: String,
        /// Project path reported with the detection result.
        path: Option<PathBuf>,
    },
    /// Enables one allowlisted toolchain kind in user config.
    Enable {
        /// Allowlisted toolchain kind.
        kind: String,
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
    let (path, input_source, verbose) = match args.command {
        Some(SandboxCliCommand::Cache { command }) => {
            return run_sandbox_cache(command, env, output_format, stdout);
        }
        Some(SandboxCliCommand::Toolchains { command }) => {
            return run_sandbox_toolchains(command, env, interactive, output_format, stdout);
        }
        Some(SandboxCliCommand::Profile { command }) => {
            return run_sandbox_profile(command, env, interactive, output_format, stdout);
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
        ),
        Some(SandboxCliCommand::Status { path, verbose }) => match path {
            Some(path) => (path, ProjectRootInputSource::ExplicitPath, verbose),
            None => (
                std::env::current_dir()?,
                ProjectRootInputSource::CurrentDirectory,
                verbose,
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
    Ok(0)
}

/// Runs one read-only or explicitly confirmed managed-home cache workflow.
fn run_sandbox_cache<W: Write>(
    command: SandboxCacheCommand,
    env: CliEnv,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<u8> {
    let paths = env.config_paths()?;
    let (operation, project_root, homes, dry_run, confirmation_required, applied) = match command {
        SandboxCacheCommand::Status { path } => {
            let path = path.unwrap_or(std::env::current_dir()?);
            let discovery =
                discover_project_root_with_metadata(&path, ProjectRootInputSource::ExplicitPath)?;
            let inspection =
                inspect_bubblewrap_managed_home(paths.root(), &discovery.canonical_root)
                    .map_err(|error| MezError::invalid_state(error.message()))?;
            let home = BubblewrapManagedHomeMaintenance {
                project_key: inspection.project_key,
                exists: inspection.exists,
                bytes: inspection.bytes,
                active: inspection.active,
                candidate: inspection.exists && !inspection.active,
                removed: false,
            };
            (
                "status",
                Some(discovery.canonical_root),
                vec![home],
                true,
                false,
                false,
            )
        }
        SandboxCacheCommand::Clear { path, dry_run, yes } => {
            let path = path.unwrap_or(std::env::current_dir()?);
            let discovery =
                discover_project_root_with_metadata(&path, ProjectRootInputSource::ExplicitPath)?;
            let preview_only = dry_run || !yes;
            let home = clear_bubblewrap_managed_home(
                paths.root(),
                &discovery.canonical_root,
                preview_only,
            )
            .map_err(|error| MezError::invalid_state(error.message()))?;
            (
                "clear",
                Some(discovery.canonical_root),
                vec![home],
                preview_only,
                !dry_run && !yes,
                yes && !dry_run,
            )
        }
        SandboxCacheCommand::Prune { dry_run, yes } => {
            let preview_only = dry_run || !yes;
            let homes = prune_bubblewrap_managed_homes(paths.root(), preview_only)
                .map_err(|error| MezError::invalid_state(error.message()))?;
            (
                "prune",
                None,
                homes,
                preview_only,
                !dry_run && !yes,
                yes && !dry_run,
            )
        }
    };
    let result = sandbox_cache_result(
        operation,
        project_root,
        homes,
        dry_run,
        confirmation_required,
        applied,
    )?;
    write_sandbox_cache_result(stdout, output_format, &result)?;
    Ok(if result.confirmation_required { 1 } else { 0 })
}

fn sandbox_cache_result(
    operation: &'static str,
    project_root: Option<PathBuf>,
    homes: Vec<BubblewrapManagedHomeMaintenance>,
    dry_run: bool,
    confirmation_required: bool,
    applied: bool,
) -> Result<SandboxCacheResult> {
    let total_bytes = homes.iter().try_fold(0_u64, |total, home| {
        total
            .checked_add(home.bytes)
            .ok_or_else(|| MezError::invalid_state("managed-home byte total overflowed"))
    })?;
    Ok(SandboxCacheResult {
        version: 1,
        operation,
        project_root,
        active_homes: homes.iter().filter(|home| home.active).count(),
        candidate_homes: homes.iter().filter(|home| home.candidate).count(),
        removed_homes: homes.iter().filter(|home| home.removed).count(),
        homes,
        total_bytes,
        dry_run,
        confirmation_required,
        applied,
    })
}

fn write_sandbox_cache_result<W: Write>(
    stdout: &mut W,
    output_format: CliOutputFormat,
    result: &SandboxCacheResult,
) -> Result<()> {
    if output_format.is_json() {
        writeln!(stdout, "{}", serialize_json(result)?)?;
    } else {
        writeln!(stdout, "operation: {}", result.operation)?;
        if let Some(project_root) = &result.project_root {
            writeln!(stdout, "project_root: {}", project_root.display())?;
        }
        writeln!(stdout, "total_bytes: {}", result.total_bytes)?;
        writeln!(stdout, "active_homes: {}", result.active_homes)?;
        writeln!(stdout, "candidate_homes: {}", result.candidate_homes)?;
        writeln!(stdout, "removed_homes: {}", result.removed_homes)?;
        writeln!(stdout, "dry_run: {}", result.dry_run)?;
        writeln!(
            stdout,
            "confirmation_required: {}",
            result.confirmation_required
        )?;
        for home in &result.homes {
            writeln!(
                stdout,
                "home: {} exists={} bytes={} active={} candidate={} removed={}",
                home.project_key,
                home.exists,
                home.bytes,
                home.active,
                home.candidate,
                home.removed
            )?;
        }
    }
    Ok(())
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

/// Runs strict sanitized profile import and export workflows.
fn run_sandbox_profile<W: Write>(
    command: SandboxProfileCommand,
    env: CliEnv,
    interactive: bool,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<u8> {
    match command {
        SandboxProfileCommand::Export { path } => {
            let path = path.unwrap_or(std::env::current_dir()?);
            let paths = env.config_paths()?;
            let discovery =
                discover_project_root_with_metadata(&path, ProjectRootInputSource::ExplicitPath)?;
            let layers = load_read_only_config_layers(
                &paths,
                &discovery.canonical_root,
                &discovery.canonical_start,
                false,
            )?;
            let structured = runtime_effective_config_value(&layers)?;
            let permissions = runtime_configured_permissions_from_config(&structured)?;
            let (preset, authority, toolchains) = match permissions.sandbox {
                crate::runtime::SandboxConfig::PolicyOnly => ("off", "explicit-scope", Vec::new()),
                crate::runtime::SandboxConfig::Bubblewrap(config) => {
                    let preset = if permissions.resources.write_scopes.is_empty()
                        && !permissions.resources.read_scopes.is_empty()
                    {
                        "project-read-only"
                    } else if permissions.authorization.approval_policy
                        == mez_agent::ApprovalPolicy::AutoAllow
                    {
                        "project-auto"
                    } else {
                        "project-safe"
                    };
                    let authority = if permissions.resources.read_scopes.is_empty()
                        && permissions.resources.write_scopes.is_empty()
                    {
                        "trusted-project"
                    } else {
                        "explicit-scope"
                    };
                    let toolchains = config
                        .toolchains
                        .iter()
                        .map(|kind| kind.as_str().to_string())
                        .collect();
                    (preset, authority, toolchains)
                }
            };
            let recipe = SandboxProfileRecipe {
                version: 1,
                preset: preset.to_string(),
                authority: authority.to_string(),
                toolchains,
            };
            writeln!(stdout, "{}", serialize_json(&recipe)?)?;
            Ok(0)
        }
        SandboxProfileCommand::Import {
            file,
            path,
            dry_run,
            yes,
        } => {
            let text = fs::read_to_string(&file)?;
            let recipe: SandboxProfileRecipe = serde_json::from_str(&text).map_err(|error| {
                MezError::invalid_args(format!("invalid sandbox profile recipe: {error}"))
            })?;
            validate_sandbox_profile_recipe(&recipe)?;
            run_sandbox_setup(
                SandboxSetupCommand::ProfileImport(
                    SandboxSetupArgs {
                        preset: recipe.preset,
                        authority: Some(recipe.authority),
                        path,
                        dry_run,
                        yes,
                    },
                    recipe.toolchains,
                ),
                env,
                interactive,
                output_format,
                stdout,
            )
        }
    }
}

fn validate_sandbox_profile_recipe(recipe: &SandboxProfileRecipe) -> Result<()> {
    if recipe.version != 1 {
        return Err(MezError::invalid_args("sandbox profile version must be 1"));
    }
    if !matches!(
        recipe.preset.as_str(),
        "project-safe" | "project-auto" | "project-read-only" | "off"
    ) {
        return Err(MezError::invalid_args(
            "sandbox profile contains an unsupported preset",
        ));
    }
    if !matches!(
        recipe.authority.as_str(),
        "trusted-project" | "explicit-scope"
    ) {
        return Err(MezError::invalid_args(
            "sandbox profile contains an unsupported authority",
        ));
    }
    let mut selected = Vec::new();
    for name in &recipe.toolchains {
        let kind = parse_sandbox_toolchain_kind(name).ok_or_else(|| {
            MezError::invalid_args("sandbox profile contains an unsupported toolchain kind")
        })?;
        if selected.contains(&kind) {
            return Err(MezError::invalid_args(
                "sandbox profile contains duplicate toolchain kinds",
            ));
        }
        selected.push(kind);
    }
    Ok(())
}

/// Plans or applies one code-owned guided sandbox setup transaction.
fn run_sandbox_setup<W: Write>(
    command: SandboxSetupCommand,
    env: CliEnv,
    interactive: bool,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<u8> {
    let (preset, authority, path, dry_run, yes, force_read_only, trust_only, profile_toolchains) =
        match command {
            SandboxSetupCommand::Plan(args) => (
                args.preset,
                args.authority,
                args.path,
                true,
                false,
                true,
                false,
                None,
            ),
            SandboxSetupCommand::Enable(args) => (
                args.preset,
                args.authority,
                args.path,
                args.dry_run,
                args.yes,
                false,
                false,
                None,
            ),
            SandboxSetupCommand::Disable(args) => (
                "off".to_string(),
                Some("retained".to_string()),
                None,
                args.dry_run,
                args.yes,
                false,
                false,
                None,
            ),
            SandboxSetupCommand::TrustCurrentProject(args) => (
                "trust-current-project".to_string(),
                Some("trusted-project".to_string()),
                None,
                args.dry_run,
                args.yes,
                false,
                true,
                None,
            ),
            SandboxSetupCommand::ProfileImport(args, toolchains) => (
                args.preset,
                args.authority,
                args.path,
                args.dry_run,
                args.yes,
                false,
                false,
                Some(toolchains),
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
    let mut mutations = if trust_only {
        Vec::new()
    } else {
        sandbox_setup_mutations(&preset, &authority, &project, &mut trust_current_project)?
    };
    if let Some(toolchains) = profile_toolchains {
        mutations.push(ConfigMutation {
            path: "permissions.bubblewrap.toolchains".to_string(),
            operation: ConfigMutationOperation::Set(ConfigMutationValue::StringArray(toolchains)),
        });
    }
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
            let git_marker = match discovery.marker_kind {
                ProjectRootMarkerKind::GitDirectory | ProjectRootMarkerKind::GitFile => {
                    Some(discovery.canonical_root.join(".git"))
                }
                ProjectRootMarkerKind::Fallback => None,
            };
            ProjectTrustStore::update_file(&trust_path, |store| {
                store.decide(
                    discovery.canonical_root.clone(),
                    TrustDecision::Trusted,
                    git_marker,
                )
            })?;
            Ok(())
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
        "project_root: {}\nproject_source: {}\nproject_marker: {}\ntrust_state: {}\nsandbox_configured: {}\nsandbox_effective: {}\napproval_policy: {}\nscope_provenance: {}\nbubblewrap_executable_state: {}\nbubblewrap_probe_state: {}\nmanaged_home_state: {}\nmanaged_home_bytes: {}\nmanaged_home_active: {}\ntoolchains: {}\ntoolchain_state: {}\nnetwork_isolated: {}\nreload_freshness: {}\n",
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
        plan.effective.managed_home_bytes,
        plan.effective.managed_home_active,
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

/// Stable direct-user projection for typed toolchain detection and activation.
#[derive(Debug, Serialize)]
struct SandboxToolchainResult {
    version: u32,
    project_root: PathBuf,
    kind: &'static str,
    available: bool,
    cargo_bin: Option<PathBuf>,
    rustup_home: Option<PathBuf>,
    zig_root: Option<PathBuf>,
    go_root: Option<PathBuf>,
    deno_root: Option<PathBuf>,
    bun_root: Option<PathBuf>,
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
        SandboxToolchainsCommand::Detect { kind, path } => {
            let kind = parse_sandbox_toolchain_kind(&kind).ok_or_else(|| {
                MezError::invalid_args("sandbox toolchains detect received an unsupported kind")
            })?;
            let input_source = if path.is_some() {
                ProjectRootInputSource::ExplicitPath
            } else {
                ProjectRootInputSource::CurrentDirectory
            };
            let path = path.unwrap_or(std::env::current_dir()?);
            let project = discover_project_root_with_metadata(&path, input_source)?;
            let detection = detect_direct_toolchain(kind, &env)?;
            let result = toolchain_result(project.canonical_root, detection, false, false);
            write_toolchain_result(stdout, output_format, &result)?;
            Ok(0)
        }
        SandboxToolchainsCommand::Enable { kind, yes } => {
            let kind = parse_sandbox_toolchain_kind(&kind).ok_or_else(|| {
                MezError::invalid_args("sandbox toolchains enable received an unsupported kind")
            })?;
            let project = discover_project_root_with_metadata(
                &std::env::current_dir()?,
                ProjectRootInputSource::CurrentDirectory,
            )?;
            let detection = detect_direct_toolchain(kind, &env)?;
            if !detection.available() {
                return Err(MezError::invalid_state(format!(
                    "{} toolchain detection did not find a canonical distribution",
                    kind.as_str()
                )));
            }
            if !yes {
                let mut result = toolchain_result(project.canonical_root, detection, false, true);
                result.message = if interactive {
                    format!(
                        "Review the read-only roots and rerun with --yes to enable {}.",
                        kind.as_str()
                    )
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
                        vec![kind.as_str().to_string()],
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
    discovery: RustToolchainHomeDiscovery,
}

#[derive(Debug)]
enum DirectToolchainDetection {
    Rust(RustToolchainDetection),
    Zig(Option<PathBuf>),
    Go(Option<PathBuf>),
    Deno(Option<PathBuf>),
    Bun(Option<PathBuf>),
}

impl DirectToolchainDetection {
    fn available(&self) -> bool {
        match self {
            Self::Rust(detection) => detection.discovery.available(),
            Self::Zig(root) => root.is_some(),
            Self::Go(root) => root.is_some(),
            Self::Deno(root) => root.is_some(),
            Self::Bun(root) => root.is_some(),
        }
    }

    const fn kind(&self) -> SandboxToolchainKind {
        match self {
            Self::Rust(_) => SandboxToolchainKind::Rust,
            Self::Zig(_) => SandboxToolchainKind::Zig,
            Self::Go(_) => SandboxToolchainKind::Go,
            Self::Deno(_) => SandboxToolchainKind::Deno,
            Self::Bun(_) => SandboxToolchainKind::Bun,
        }
    }
}

fn detect_direct_toolchain(
    kind: SandboxToolchainKind,
    env: &CliEnv,
) -> Result<DirectToolchainDetection> {
    match kind {
        SandboxToolchainKind::Rust => discover_rust_from_home(env.home.as_deref())
            .map(|discovery| DirectToolchainDetection::Rust(RustToolchainDetection { discovery }))
            .map_err(|error| MezError::invalid_state(error.to_string())),
        SandboxToolchainKind::Zig => discover_zig_from_search_path(env.path.as_deref())
            .map(DirectToolchainDetection::Zig)
            .map_err(|error| MezError::invalid_state(error.to_string())),
        SandboxToolchainKind::Go => discover_go_from_search_path(env.path.as_deref())
            .map(DirectToolchainDetection::Go)
            .map_err(|error| MezError::invalid_state(error.to_string())),
        SandboxToolchainKind::Deno => discover_deno_from_search_path(env.path.as_deref())
            .map(DirectToolchainDetection::Deno)
            .map_err(|error| MezError::invalid_state(error.to_string())),
        SandboxToolchainKind::Bun => discover_bun_from_search_path(env.path.as_deref())
            .map(DirectToolchainDetection::Bun)
            .map_err(|error| MezError::invalid_state(error.to_string())),
    }
}

fn toolchain_result(
    project_root: PathBuf,
    detection: DirectToolchainDetection,
    applied: bool,
    confirmation_required: bool,
) -> SandboxToolchainResult {
    let available = detection.available();
    let kind = detection.kind();
    let (cargo_bin, rustup_home, zig_root, go_root, deno_root, bun_root, sandbox_path) =
        match detection {
            DirectToolchainDetection::Rust(detection) => (
                detection.discovery.cargo_bin,
                detection.discovery.rustup_home,
                None,
                None,
                None,
                None,
                SANDBOX_RUST_PATH,
            ),
            DirectToolchainDetection::Zig(root) => {
                (None, None, root, None, None, None, SANDBOX_ZIG_PATH)
            }
            DirectToolchainDetection::Go(root) => {
                (None, None, None, root, None, None, SANDBOX_GO_PATH)
            }
            DirectToolchainDetection::Deno(root) => {
                (None, None, None, None, root, None, SANDBOX_DENO_PATH)
            }
            DirectToolchainDetection::Bun(root) => {
                (None, None, None, None, None, root, SANDBOX_BUN_PATH)
            }
        };
    SandboxToolchainResult {
        version: 1,
        project_root,
        kind: kind.as_str(),
        available,
        cargo_bin,
        rustup_home,
        zig_root,
        go_root,
        deno_root,
        bun_root,
        sandbox_path,
        read_only: true,
        applied,
        confirmation_required,
        message: if applied {
            format!(
                "{} toolchain projection enabled; live sessions require reload.",
                kind.as_str()
            )
        } else {
            format!(
                "{} toolchain detection completed without changing configuration.",
                kind.as_str()
            )
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
        writeln!(
            stdout,
            "zig_root: {}",
            result
                .zig_root
                .as_deref()
                .map_or_else(|| "none".to_string(), |path| path.display().to_string())
        )?;
        writeln!(
            stdout,
            "go_root: {}",
            result
                .go_root
                .as_deref()
                .map_or_else(|| "none".to_string(), |path| path.display().to_string())
        )?;
        writeln!(
            stdout,
            "deno_root: {}",
            result
                .deno_root
                .as_deref()
                .map_or_else(|| "none".to_string(), |path| path.display().to_string())
        )?;
        writeln!(
            stdout,
            "bun_root: {}",
            result
                .bun_root
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
