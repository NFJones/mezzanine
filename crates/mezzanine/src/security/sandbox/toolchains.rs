//! Typed sandbox toolchain discovery and projection metadata.
//!
//! This module is the single owner for allowlisted toolchain names, fixed
//! in-sandbox projection paths, and validation of host roots supplied either
//! by pane bootstrap evidence or the direct-user CLI adapter. Runtime
//! discovery never consults ambient process state, and discovery alone never
//! grants filesystem authority; final launch compilation still checks the
//! validated roots against pane-resolved maximum read authority.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::runtime::SandboxToolchainKind;
use mez_agent::permissions::PathScopes;

use super::{
    SandboxCompileError, SandboxCompileErrorKind, path_is_credential_directory, path_overlaps,
    validate_printable_absolute_path,
};

/// Stable supported toolchain kinds in display and completion order.
pub(crate) const SUPPORTED_SANDBOX_TOOLCHAIN_KINDS: [SandboxToolchainKind; 1] =
    [SandboxToolchainKind::Rust];

/// Fixed Cargo executable projection inside Bubblewrap.
pub(crate) const SANDBOX_RUST_CARGO_BIN: &str = "/opt/mez/toolchains/rust/cargo-bin";
/// Fixed Rustup home projection inside Bubblewrap.
pub(crate) const SANDBOX_RUSTUP_HOME: &str = "/opt/mez/toolchains/rust/rustup";
/// Fixed executable search path used when Rust is projected.
pub(crate) const SANDBOX_RUST_PATH: &str = "/opt/mez/toolchains/rust/cargo-bin:/usr/bin:/bin";

/// Security class assigned to one descriptor-owned projection resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolchainAuthorityClass {
    /// Immutable runtime or SDK content projected read-only.
    Runtime,
    /// Repository-controlled executable state already covered by project authority.
    ProjectEnvironment,
    /// Separately selected user-installed executable content.
    UserTools,
    /// Writable state created only beneath the Mezzanine-managed home.
    ManagedState,
    /// Credential or user configuration state that remains hidden.
    Credential,
}

/// Host platform constraint declared by one toolchain descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolchainPlatform {
    /// Descriptor is portable across supported Bubblewrap host platforms.
    Any,
    /// Descriptor is supported only for Linux pane environments.
    Linux,
    /// Descriptor is supported only for macOS pane environments.
    MacOs,
    /// Descriptor is supported only for Windows pane environments.
    Windows,
}

impl ToolchainPlatform {
    /// Reports whether one normalized pane operating-system spelling is supported.
    pub(super) fn supports(self, host_os: &str) -> bool {
        self == Self::Any
            || matches!(
                (self, host_os),
                (Self::Linux, "linux")
                    | (Self::MacOs, "macos" | "darwin")
                    | (Self::Windows, "windows")
            )
    }
}

/// One fixed root expected from bounded pane-bootstrap evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolchainRootDescriptor {
    /// Stable environment-manager evidence record name.
    pub(crate) evidence_kind: &'static str,
    /// Human-readable label used in fail-closed diagnostics.
    pub(crate) label: &'static str,
    /// Fixed code-owned destination inside the sandbox.
    pub(crate) sandbox_destination: &'static str,
    /// Allowed final canonical path components.
    pub(crate) allowed_names: &'static [&'static str],
    /// Optional allowed parent components for narrow executable directories.
    pub(crate) allowed_parent_names: &'static [&'static str],
    /// Security class governing this root.
    pub(crate) authority_class: ToolchainAuthorityClass,
}

/// One synthesized child environment value owned by a descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolchainEnvironmentVariable {
    /// Environment variable name.
    pub(crate) name: &'static str,
    /// Fixed sandbox-visible value.
    pub(crate) value: &'static str,
}

/// One writable state location created beneath the managed sandbox home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ManagedToolchainState {
    /// Stable state purpose used by status and future quota reporting.
    pub(crate) purpose: &'static str,
    /// Fixed sandbox path beneath `/home/mez`.
    pub(crate) sandbox_path: &'static str,
}

/// Required and optional companion kinds for a descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolchainCoupling {
    /// Kinds that must also be selected.
    pub(crate) required: &'static [SandboxToolchainKind],
    /// Kinds that may be composed when selected explicitly.
    pub(crate) optional: &'static [SandboxToolchainKind],
}

/// Stable code-owned behavior for one allowlisted toolchain kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolchainDescriptor {
    /// Persisted typed kind.
    pub(crate) kind: SandboxToolchainKind,
    /// Accepted user-facing aliases.
    pub(crate) aliases: &'static [&'static str],
    /// Fixed roots resolved from bounded evidence.
    pub(crate) roots: &'static [ToolchainRootDescriptor],
    /// Parent directories created before fixed mounts.
    pub(crate) sandbox_directories: &'static [&'static str],
    /// Executable search paths in descriptor-owned priority order.
    pub(crate) path_entries: &'static [&'static str],
    /// Explicit child environment synthesized from sandbox paths.
    pub(crate) environment: &'static [ToolchainEnvironmentVariable],
    /// Writable state redirected beneath the managed home.
    pub(crate) managed_state: &'static [ManagedToolchainState],
    /// Host descendants that this descriptor never projects.
    pub(crate) forbidden_descendants: &'static [&'static str],
    /// Supported host platform.
    pub(crate) platform: ToolchainPlatform,
    /// Companion dependency contract.
    pub(crate) coupling: ToolchainCoupling,
    /// Whether explicitly modeled roots may contain or overlap one another.
    pub(crate) allow_root_overlap: bool,
}

const RUST_ROOTS: [ToolchainRootDescriptor; 2] = [
    ToolchainRootDescriptor {
        evidence_kind: "cargo-bin",
        label: "Cargo bin",
        sandbox_destination: SANDBOX_RUST_CARGO_BIN,
        allowed_names: &["bin"],
        allowed_parent_names: &[".cargo", "cargo"],
        authority_class: ToolchainAuthorityClass::UserTools,
    },
    ToolchainRootDescriptor {
        evidence_kind: "rustup",
        label: "Rustup home",
        sandbox_destination: SANDBOX_RUSTUP_HOME,
        allowed_names: &[".rustup", "rustup"],
        allowed_parent_names: &[],
        authority_class: ToolchainAuthorityClass::Runtime,
    },
];
const RUST_ENVIRONMENT: [ToolchainEnvironmentVariable; 2] = [
    ToolchainEnvironmentVariable {
        name: "CARGO_HOME",
        value: "/home/mez/.cargo",
    },
    ToolchainEnvironmentVariable {
        name: "RUSTUP_HOME",
        value: SANDBOX_RUSTUP_HOME,
    },
];
const RUST_MANAGED_STATE: [ManagedToolchainState; 1] = [ManagedToolchainState {
    purpose: "cargo-home",
    sandbox_path: "/home/mez/.cargo",
}];
const RUST_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Rust,
    aliases: &["rust"],
    roots: &RUST_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/rust",
    ],
    path_entries: &[SANDBOX_RUST_CARGO_BIN],
    environment: &RUST_ENVIRONMENT,
    managed_state: &RUST_MANAGED_STATE,
    forbidden_descendants: &["credentials", "credentials.toml", "config.toml"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

/// One validated host root and its fixed sandbox destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedToolchainRoot {
    /// Security class inherited from the descriptor.
    pub(crate) authority_class: ToolchainAuthorityClass,
    /// Canonical host source from pane bootstrap evidence.
    pub(crate) host_path: PathBuf,
    /// Fixed code-owned sandbox destination.
    pub(crate) sandbox_destination: &'static str,
}

/// One descriptor resolved from active-pane evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedToolchain {
    /// Typed descriptor kind.
    pub(crate) kind: SandboxToolchainKind,
    /// Validated roots in descriptor order.
    pub(crate) roots: Vec<ResolvedToolchainRoot>,
}

/// Deterministically composed projection consumed by Bubblewrap compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedToolchainProjection {
    /// Kinds in code-owned descriptor priority order.
    pub(crate) kinds: Vec<SandboxToolchainKind>,
    /// Parent directories created before mounts.
    pub(crate) sandbox_directories: Vec<&'static str>,
    /// Validated fixed read-only mounts.
    pub(crate) roots: Vec<ResolvedToolchainRoot>,
    /// Ordered executable search paths excluding the system suffix.
    pub(crate) path_entries: Vec<&'static str>,
    /// Explicit synthesized environment excluding PATH.
    pub(crate) environment: BTreeMap<&'static str, &'static str>,
    /// Managed-state declarations for status and future quotas.
    pub(crate) managed_state: Vec<ManagedToolchainState>,
}

impl ResolvedToolchainProjection {
    /// Validates every projected host root against pane-resolved maximum read authority.
    pub(crate) fn validate_authority(
        &self,
        authority: &PathScopes,
    ) -> Result<(), SandboxCompileError> {
        for root in &self.roots {
            if !authority
                .read_scopes
                .iter()
                .any(|scope| root.host_path.starts_with(Path::new(scope)))
            {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::ToolchainOutsideAuthority,
                    format!(
                        "{} falls outside maximum sandbox read authority",
                        root.sandbox_destination
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Revalidates descriptor-owned roots before final launch compilation.
    pub(crate) fn validate(&self) -> Result<(), SandboxCompileError> {
        let kinds = self.kinds.iter().copied().collect::<BTreeSet<_>>();
        if kinds.len() != self.kinds.len() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                "resolved toolchain projection contains duplicate kinds",
            ));
        }
        let mut expected_root_count = 0;
        let mut expected_directories = Vec::new();
        let mut expected_path_entries = Vec::new();
        let mut expected_environment = BTreeMap::new();
        let mut expected_managed_state = Vec::new();
        for kind in &self.kinds {
            let descriptor = toolchain_descriptor(*kind);
            expected_root_count += descriptor.roots.len();
            for directory in descriptor.sandbox_directories {
                if !expected_directories.contains(directory) {
                    expected_directories.push(*directory);
                }
            }
            expected_path_entries.extend_from_slice(descriptor.path_entries);
            for variable in descriptor.environment {
                if expected_environment
                    .insert(variable.name, variable.value)
                    .is_some()
                {
                    return Err(SandboxCompileError::new(
                        SandboxCompileErrorKind::InvalidInput,
                        format!(
                            "resolved toolchain projection contains duplicate {} metadata",
                            variable.name
                        ),
                    ));
                }
            }
            expected_managed_state.extend_from_slice(descriptor.managed_state);
            for expected in descriptor.roots {
                let root = self
                    .roots
                    .iter()
                    .find(|root| root.sandbox_destination == expected.sandbox_destination)
                    .ok_or_else(|| {
                        SandboxCompileError::new(
                            SandboxCompileErrorKind::InvalidInput,
                            format!(
                                "resolved {} projection is missing {}",
                                kind.as_str(),
                                expected.label
                            ),
                        )
                    })?;
                validate_descriptor_root(&root.host_path, expected)?;
                if root.authority_class != expected.authority_class
                    || matches!(
                        root.authority_class,
                        ToolchainAuthorityClass::ManagedState | ToolchainAuthorityClass::Credential
                    )
                {
                    return Err(SandboxCompileError::new(
                        SandboxCompileErrorKind::InvalidInput,
                        format!(
                            "resolved {} projection has an invalid authority class",
                            expected.label
                        ),
                    ));
                }
            }
        }
        if self.roots.len() != expected_root_count
            || self.sandbox_directories != expected_directories
            || self.path_entries != expected_path_entries
            || self.environment != expected_environment
            || self.managed_state != expected_managed_state
        {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                "resolved toolchain projection does not match descriptor metadata",
            ));
        }
        for (index, root) in self.roots.iter().enumerate() {
            if self.roots.iter().skip(index + 1).any(|other| {
                root.sandbox_destination == other.sandbox_destination
                    || root.host_path == other.host_path
                    || root.host_path.starts_with(&other.host_path)
                    || other.host_path.starts_with(&root.host_path)
            }) {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "resolved toolchain projection contains colliding mounts",
                ));
            }
        }
        Ok(())
    }

    /// Builds deterministic PATH with the fixed system suffix.
    pub(crate) fn executable_path(&self) -> String {
        self.path_entries
            .iter()
            .copied()
            .chain(["/usr/bin", "/bin"])
            .collect::<Vec<_>>()
            .join(":")
    }
}

/// Returns stable descriptor metadata for one allowlisted kind.
pub(crate) const fn toolchain_descriptor(
    kind: SandboxToolchainKind,
) -> &'static ToolchainDescriptor {
    match kind {
        SandboxToolchainKind::Rust => &RUST_DESCRIPTOR,
    }
}

/// Resolves and composes every selected descriptor from active-pane evidence.
pub(crate) fn resolve_toolchain_projection(
    selected: &[SandboxToolchainKind],
    environment_managers: &[String],
    host_os: &str,
) -> Result<Option<ResolvedToolchainProjection>, SandboxCompileError> {
    if selected.is_empty() {
        return Ok(None);
    }
    let selected_set = selected.iter().copied().collect::<BTreeSet<_>>();
    if selected_set.len() != selected.len() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            "selected toolchain kinds must not contain duplicates",
        ));
    }
    let mut resolved = Vec::new();
    for kind in SUPPORTED_SANDBOX_TOOLCHAIN_KINDS {
        if !selected_set.contains(&kind) {
            continue;
        }
        let descriptor = toolchain_descriptor(kind);
        if !descriptor.platform.supports(host_os) {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::UnsupportedRequirement,
                format!("{} toolchain is unsupported on {host_os}", kind.as_str()),
            ));
        }
        for required in descriptor.coupling.required {
            if !selected_set.contains(required) {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::UnsupportedRequirement,
                    format!(
                        "{} toolchain requires selected companion {}",
                        kind.as_str(),
                        required.as_str()
                    ),
                ));
            }
        }
        resolved.push(resolve_descriptor(descriptor, environment_managers)?);
    }
    compose_toolchain_projection(&resolved).map(Some)
}

/// Resolves one descriptor from bounded pane-bootstrap evidence.
fn resolve_descriptor(
    descriptor: &ToolchainDescriptor,
    environment_managers: &[String],
) -> Result<ResolvedToolchain, SandboxCompileError> {
    let mut roots = Vec::with_capacity(descriptor.roots.len());
    for root_descriptor in descriptor.roots {
        let host_path = unique_manager_path(environment_managers, root_descriptor.evidence_kind)?
            .ok_or_else(|| {
            SandboxCompileError::new(
                SandboxCompileErrorKind::UnsupportedRequirement,
                format!(
                    "selected {} toolchain requires {} from pane bootstrap",
                    descriptor.kind.as_str(),
                    root_descriptor.label
                ),
            )
        })?;
        validate_descriptor_root(&host_path, root_descriptor)?;
        if descriptor.forbidden_descendants.iter().any(|forbidden| {
            host_path.components().any(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .is_some_and(|component| component == *forbidden)
            })
        }) {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                format!(
                    "{} toolchain root contains a forbidden credential or configuration component",
                    descriptor.kind.as_str()
                ),
            ));
        }
        roots.push(ResolvedToolchainRoot {
            authority_class: root_descriptor.authority_class,
            host_path,
            sandbox_destination: root_descriptor.sandbox_destination,
        });
    }
    if !descriptor.allow_root_overlap {
        for (index, root) in roots.iter().enumerate() {
            if roots.iter().skip(index + 1).any(|other| {
                root.host_path == other.host_path
                    || root.host_path.starts_with(&other.host_path)
                    || other.host_path.starts_with(&root.host_path)
            }) {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::ForbiddenHostPath,
                    format!(
                        "{} toolchain roots must be distinct and non-overlapping",
                        descriptor.kind.as_str()
                    ),
                ));
            }
        }
    }
    Ok(ResolvedToolchain {
        kind: descriptor.kind,
        roots,
    })
}

/// Composes resolved descriptors in stable descriptor priority order.
fn compose_toolchain_projection(
    resolved: &[ResolvedToolchain],
) -> Result<ResolvedToolchainProjection, SandboxCompileError> {
    let resolved_by_kind = resolved
        .iter()
        .map(|toolchain| (toolchain.kind, toolchain))
        .collect::<BTreeMap<_, _>>();
    if resolved_by_kind.len() != resolved.len() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            "resolved toolchain kinds must not contain duplicates",
        ));
    }

    let mut projection = ResolvedToolchainProjection {
        kinds: Vec::new(),
        sandbox_directories: Vec::new(),
        roots: Vec::new(),
        path_entries: Vec::new(),
        environment: BTreeMap::new(),
        managed_state: Vec::new(),
    };
    let mut destinations = BTreeSet::new();
    let mut managed_paths = BTreeSet::new();
    for kind in SUPPORTED_SANDBOX_TOOLCHAIN_KINDS {
        let Some(toolchain) = resolved_by_kind.get(&kind) else {
            continue;
        };
        let descriptor = toolchain_descriptor(kind);
        for optional in descriptor.coupling.optional {
            if resolved_by_kind.contains_key(optional) {
                let _ = toolchain_descriptor(*optional);
            }
        }
        projection.kinds.push(kind);
        for directory in descriptor.sandbox_directories {
            if !projection.sandbox_directories.contains(directory) {
                projection.sandbox_directories.push(directory);
            }
        }
        for root in &toolchain.roots {
            if !destinations.insert(root.sandbox_destination) {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!(
                        "toolchain descriptors collide at fixed destination {}",
                        root.sandbox_destination
                    ),
                ));
            }
            if projection.roots.iter().any(|existing| {
                root.host_path == existing.host_path
                    || root.host_path.starts_with(&existing.host_path)
                    || existing.host_path.starts_with(&root.host_path)
            }) {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::ForbiddenHostPath,
                    "toolchain descriptors resolve overlapping host roots",
                ));
            }
            projection.roots.push(root.clone());
        }
        for path_entry in descriptor.path_entries {
            if projection.path_entries.contains(path_entry) {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("toolchain descriptors collide in PATH at {path_entry}"),
                ));
            }
            projection.path_entries.push(path_entry);
        }
        for variable in descriptor.environment {
            if let Some(existing) = projection.environment.insert(variable.name, variable.value)
                && existing != variable.value
            {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!(
                        "toolchain descriptors synthesize conflicting {} values",
                        variable.name
                    ),
                ));
            }
        }
        for state in descriptor.managed_state {
            if !state.sandbox_path.starts_with("/home/mez/") {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!(
                        "managed toolchain state {} must remain beneath /home/mez",
                        state.purpose
                    ),
                ));
            }
            if !managed_paths.insert(state.sandbox_path) {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!(
                        "toolchain descriptors collide in managed state at {}",
                        state.sandbox_path
                    ),
                ));
            }
            projection.managed_state.push(*state);
        }
    }
    projection.validate()?;
    Ok(projection)
}

/// Validates one root against its descriptor-owned structural contract.
fn validate_descriptor_root(
    path: &Path,
    descriptor: &ToolchainRootDescriptor,
) -> Result<(), SandboxCompileError> {
    validate_toolchain_root(path, descriptor.label, descriptor.allowed_names)?;
    if !descriptor.allowed_parent_names.is_empty() {
        let parent = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str());
        if !parent.is_some_and(|parent| descriptor.allowed_parent_names.contains(&parent)) {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                format!(
                    "{} must be directly beneath an allowlisted toolchain root",
                    descriptor.label
                ),
            ));
        }
    }
    Ok(())
}

/// Canonical host roots for one discovered Rust toolchain projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustToolchainDiscovery {
    /// Canonical Cargo executable directory containing rustup shims.
    pub(crate) cargo_bin: PathBuf,
    /// Canonical Rustup home containing installed toolchains and metadata.
    pub(crate) rustup_home: PathBuf,
}

/// Independently discovered direct-user Rust roots for CLI status output.
///
/// The CLI preserves partial availability so users can see which conventional
/// root is missing. Runtime pane discovery remains all-or-nothing because a
/// sandbox projection requires both roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustToolchainHomeDiscovery {
    /// Canonical Cargo executable directory when it exists and is valid.
    pub(crate) cargo_bin: Option<PathBuf>,
    /// Canonical Rustup home when it exists and is valid.
    pub(crate) rustup_home: Option<PathBuf>,
}

impl RustToolchainHomeDiscovery {
    /// Returns whether both roots required for a Rust projection are present.
    pub(crate) const fn available(&self) -> bool {
        self.cargo_bin.is_some() && self.rustup_home.is_some()
    }
}

/// Parses one stable allowlisted toolchain spelling.
pub(crate) fn parse_sandbox_toolchain_kind(value: &str) -> Option<SandboxToolchainKind> {
    SUPPORTED_SANDBOX_TOOLCHAIN_KINDS
        .into_iter()
        .find(|kind| kind.as_str() == value)
}

/// Discovers Rust roots from explicit active-pane bootstrap records.
///
/// Records for unrelated environment managers are ignored. Rust records must
/// use exactly `cargo-bin:<absolute-path>` and `rustup:<absolute-path>` once
/// each; malformed, missing, or duplicate records fail closed.
pub(crate) fn discover_rust_from_environment_managers(
    environment_managers: &[String],
) -> Result<RustToolchainDiscovery, SandboxCompileError> {
    let cargo_bin = unique_manager_path(environment_managers, "cargo-bin")?.ok_or_else(|| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::UnsupportedRequirement,
            "selected Rust toolchain requires a canonical Cargo bin directory from pane bootstrap",
        )
    })?;
    let rustup_home = unique_manager_path(environment_managers, "rustup")?.ok_or_else(|| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::UnsupportedRequirement,
            "selected Rust toolchain requires a canonical Rustup home from pane bootstrap",
        )
    })?;
    RustToolchainDiscovery::validated(cargo_bin, rustup_home)
}

/// Discovers Rust roots from the direct user's home directory without
/// changing configuration or creating any filesystem state.
///
/// Missing roots report unavailable as `None`. Existing roots must be real
/// directories rather than symlinks and are canonicalized before the shared
/// validation boundary.
pub(crate) fn discover_rust_from_home(
    home: Option<&Path>,
) -> Result<RustToolchainHomeDiscovery, SandboxCompileError> {
    let Some(home) = home else {
        return Ok(RustToolchainHomeDiscovery {
            cargo_bin: None,
            rustup_home: None,
        });
    };
    let cargo_bin = canonical_existing_directory(&home.join(".cargo/bin"), "Cargo bin")?;
    let rustup_home = canonical_existing_directory(&home.join(".rustup"), "Rustup home")?;
    if let Some(cargo_bin) = cargo_bin.as_ref() {
        validate_cargo_bin(cargo_bin)?;
    }
    if let Some(rustup_home) = rustup_home.as_ref() {
        validate_toolchain_root(rustup_home, "Rustup home", &[".rustup", "rustup"])?;
    }
    if let (Some(cargo_bin), Some(rustup_home)) = (&cargo_bin, &rustup_home) {
        RustToolchainDiscovery::validated(cargo_bin.clone(), rustup_home.clone())?;
    }
    Ok(RustToolchainHomeDiscovery {
        cargo_bin,
        rustup_home,
    })
}

impl RustToolchainDiscovery {
    /// Validates already-resolved roots without adding filesystem authority.
    pub(super) fn validate(&self) -> Result<(), SandboxCompileError> {
        validate_cargo_bin(&self.cargo_bin)?;
        validate_toolchain_root(&self.rustup_home, "Rustup home", &[".rustup", "rustup"])?;
        if self.cargo_bin == self.rustup_home
            || self.cargo_bin.starts_with(&self.rustup_home)
            || self.rustup_home.starts_with(&self.cargo_bin)
        {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "Cargo and Rustup homes must be distinct non-overlapping roots",
            ));
        }
        Ok(())
    }

    fn validated(cargo_bin: PathBuf, rustup_home: PathBuf) -> Result<Self, SandboxCompileError> {
        let discovery = Self {
            cargo_bin,
            rustup_home,
        };
        discovery.validate()?;
        Ok(discovery)
    }
}

fn unique_manager_path(
    environment_managers: &[String],
    kind: &str,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let prefix = format!("{kind}:");
    let mut matched = None;
    for manager in environment_managers {
        if manager == kind || manager.starts_with(&prefix) {
            let Some(path) = manager
                .strip_prefix(&prefix)
                .filter(|path| !path.is_empty())
            else {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("pane bootstrap {kind} record must contain one non-empty path"),
                ));
            };
            if matched.replace(PathBuf::from(path)).is_some() {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("pane bootstrap contains duplicate {kind} records"),
                ));
            }
        }
    }
    Ok(matched)
}

fn canonical_existing_directory(
    path: &Path,
    label: &str,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                format!("failed to inspect {label}: {error}"),
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("{label} must be a real directory, not a symlink"),
        ));
    }
    path.canonicalize().map(Some).map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to canonicalize {label}: {error}"),
        )
    })
}

fn validate_toolchain_root(
    path: &Path,
    label: &str,
    allowed_names: &[&str],
) -> Result<(), SandboxCompileError> {
    let rendered = path.to_string_lossy();
    validate_printable_absolute_path(&rendered, label)?;
    let name = path.file_name().and_then(|name| name.to_str());
    if !name.is_some_and(|name| allowed_names.contains(&name)) {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("{label} must use an allowlisted toolchain directory name"),
        ));
    }
    if rendered == "/"
        || rendered == "/home"
        || path_is_credential_directory(&rendered)
        || path_overlaps(&rendered, "/run/user")
        || path_overlaps(&rendered, "/var/run")
    {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("{label} overlaps a forbidden host path"),
        ));
    }
    Ok(())
}

fn validate_cargo_bin(path: &Path) -> Result<(), SandboxCompileError> {
    validate_toolchain_root(path, "Cargo bin", &["bin"])?;
    let parent = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str());
    if !matches!(parent, Some(".cargo" | "cargo")) {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            "Cargo bin must be directly beneath an allowlisted Cargo home",
        ));
    }
    Ok(())
}
