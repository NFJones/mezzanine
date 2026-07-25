//! Typed sandbox toolchain discovery and projection metadata.
//!
//! This module is the single owner for allowlisted toolchain names, fixed
//! in-sandbox projection paths, and validation of host roots supplied either
//! by pane bootstrap evidence or the direct-user CLI adapter. Runtime
//! discovery never consults ambient process state, and discovery alone never
//! grants filesystem authority; final launch compilation still checks the
//! validated roots against pane-resolved maximum read authority.

use std::fs;
use std::path::{Path, PathBuf};

use crate::runtime::SandboxToolchainKind;

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
