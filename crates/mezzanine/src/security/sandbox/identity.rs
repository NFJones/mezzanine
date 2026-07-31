//! Pane-local resolution for configured Bubblewrap process identities.
//!
//! This module selects configured group names from credentials reported by the
//! active pane bootstrap. It never reconstructs credentials through controller
//! NSS state and never grants groups absent from the pane shell.

use std::collections::BTreeSet;

#[cfg(test)]
use std::ffi::CStr;

use mez_agent::EnvironmentSignature;
#[cfg(test)]
use mez_agent::{EnvironmentGroup, ShellClassification};
use sha2::{Digest, Sha256};

use crate::runtime::ConfiguredSandboxGroups;

use super::{SandboxCompileError, SandboxCompileErrorKind};

/// One configured host group after canonical NSS resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedHostGroup {
    /// Name retained from primary-user configuration for diagnostics.
    pub(crate) configured_name: String,
    /// Canonical group name returned by NSS.
    pub(crate) canonical_name: String,
    /// Native host GID established before Bubblewrap starts.
    pub(crate) group_id: u32,
}

/// One optional host mapping that could not be projected into the sandbox.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SandboxMappingWarning {
    /// Stable mapping category used by runtime diagnostics.
    pub(crate) mapping_kind: &'static str,
    /// Bounded configured value that was omitted.
    pub(crate) configured_value: String,
    /// Stable bounded reason for the omission.
    pub(crate) reason: &'static str,
}

/// Exact native identity shared by probe, workload, and managed-home plans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedSandboxIdentity {
    /// Native invoking user ID.
    pub(crate) user_id: u32,
    /// Canonical NSS account name.
    pub(crate) user_name: String,
    /// Native invoking primary group ID.
    pub(crate) primary_group_id: u32,
    /// Canonical NSS primary group name.
    pub(crate) primary_group_name: String,
    /// Canonical configured supplementary groups sorted by native GID.
    pub(crate) supplementary_groups: Vec<ResolvedHostGroup>,
    /// Configured supplementary mappings omitted for this pane.
    pub(crate) mapping_warnings: Vec<SandboxMappingWarning>,
    /// Stable digest used by launch plans, caches, and identity projections.
    pub(crate) identity_sha256: String,
}

/// Resolves configured group mappings from the active pane shell credentials.
pub(crate) fn resolve_sandbox_identity(
    configured: &ConfiguredSandboxGroups,
    environment: &EnvironmentSignature,
) -> Result<ResolvedSandboxIdentity, SandboxCompileError> {
    let uid = environment
        .user_id
        .ok_or_else(|| identity_error("active pane bootstrap did not report its effective UID"))?;
    let gid = environment
        .primary_group_id
        .ok_or_else(|| identity_error("active pane bootstrap did not report its primary GID"))?;
    let primary_group_name = environment
        .active_groups
        .iter()
        .find(|group| group.id == gid)
        .map(|group| group.name.clone())
        .unwrap_or_else(|| gid.to_string());
    let mut supplementary_groups = Vec::with_capacity(configured.requested_names.len());
    let mut mapping_warnings = Vec::new();
    let mut resolved_gids = BTreeSet::new();
    for configured_name in &configured.requested_names {
        let Some(group) = environment
            .active_groups
            .iter()
            .find(|group| group.name == *configured_name)
        else {
            mapping_warnings.push(SandboxMappingWarning {
                mapping_kind: "supplementary-group",
                configured_value: configured_name.chars().take(128).collect(),
                reason: "not active in the pane shell",
            });
            continue;
        };
        let canonical_name = group.name.clone();
        let group_id = group.id;
        if group_id == gid {
            mapping_warnings.push(SandboxMappingWarning {
                mapping_kind: "supplementary-group",
                configured_value: configured_name.chars().take(128).collect(),
                reason: "duplicates the automatic primary group",
            });
            continue;
        }
        if !resolved_gids.insert(group_id) {
            mapping_warnings.push(SandboxMappingWarning {
                mapping_kind: "supplementary-group",
                configured_value: configured_name.chars().take(128).collect(),
                reason: "aliases an already selected group ID",
            });
            continue;
        }
        supplementary_groups.push(ResolvedHostGroup {
            configured_name: configured_name.clone(),
            canonical_name,
            group_id,
        });
    }
    supplementary_groups.sort_by_key(|group| group.group_id);
    mapping_warnings.sort();
    let identity_sha256 = identity_sha256(
        uid,
        gid,
        &configured.requested_names,
        &supplementary_groups,
        &mapping_warnings,
    );
    Ok(ResolvedSandboxIdentity {
        user_id: uid,
        user_name: environment.user.clone(),
        primary_group_id: gid,
        primary_group_name,
        supplementary_groups,
        mapping_warnings,
        identity_sha256,
    })
}

/// Builds pane-equivalent identity evidence for local unit and integration tests.
#[cfg(test)]
pub(crate) fn current_process_environment_signature()
-> Result<EnvironmentSignature, SandboxCompileError> {
    let user_id = unsafe { libc::geteuid() };
    let primary_group_id = unsafe { libc::getegid() };
    let mut group_ids = rustix::process::getgroups()
        .map_err(|error| identity_error(format!("test process groups are unavailable: {error}")))?
        .into_iter()
        .map(rustix::process::Gid::as_raw)
        .collect::<BTreeSet<_>>();
    group_ids.insert(primary_group_id);
    let active_groups = group_ids
        .into_iter()
        .map(|id| {
            Ok(EnvironmentGroup {
                id,
                name: current_group_name(id)?,
            })
        })
        .collect::<Result<Vec<_>, SandboxCompileError>>()?;
    EnvironmentSignature::new(
        "linux",
        std::env::consts::ARCH,
        None,
        "test-host",
        "test-user",
        None,
        "/bin/sh",
        ShellClassification::PosixSh,
        None,
        None,
        "/",
        None,
        false,
        None,
        Vec::new(),
    )
    .and_then(|signature| signature.with_process_identity(user_id, primary_group_id, active_groups))
    .map_err(|error| identity_error(error.to_string()))
}

#[cfg(test)]
fn current_group_name(group_id: u32) -> Result<String, SandboxCompileError> {
    let mut capacity = 4096usize;
    while capacity <= 1024 * 1024 {
        let mut group = unsafe { std::mem::zeroed::<libc::group>() };
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0u8; capacity];
        let status = unsafe {
            libc::getgrgid_r(
                group_id,
                &mut group,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE {
            capacity *= 2;
            continue;
        }
        if status != 0 || result.is_null() || group.gr_name.is_null() {
            return Err(identity_error(format!(
                "test process GID {group_id} does not resolve through NSS"
            )));
        }
        return unsafe { CStr::from_ptr(group.gr_name) }
            .to_str()
            .map(str::to_string)
            .map_err(|_| identity_error("test process group name is not valid UTF-8"));
    }
    Err(identity_error(
        "test process group lookup exceeded its limit",
    ))
}

fn identity_sha256(
    uid: u32,
    gid: u32,
    requested_groups: &[String],
    groups: &[ResolvedHostGroup],
    warnings: &[SandboxMappingWarning],
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"mez-sandbox-identity-v2\0");
    digest.update(uid.to_le_bytes());
    digest.update(gid.to_le_bytes());
    let mut requested_groups = requested_groups.iter().collect::<Vec<_>>();
    requested_groups.sort();
    for requested in requested_groups {
        digest.update(b"\0requested\0");
        digest.update(requested.as_bytes());
    }
    for group in groups {
        digest.update(b"\0");
        digest.update(group.canonical_name.as_bytes());
        digest.update(b"\0");
        digest.update(group.group_id.to_le_bytes());
    }
    for warning in warnings {
        digest.update(b"\0omitted\0");
        digest.update(warning.mapping_kind.as_bytes());
        digest.update(b"\0");
        digest.update(warning.configured_value.as_bytes());
        digest.update(b"\0");
        digest.update(warning.reason.as_bytes());
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn identity_error(message: impl Into<String>) -> SandboxCompileError {
    SandboxCompileError::new(SandboxCompileErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds deterministic pane evidence for configured-group resolution tests.
    fn pane_environment() -> EnvironmentSignature {
        EnvironmentSignature::new(
            "linux",
            "x86_64",
            None,
            "pane-host",
            "alice",
            Some("/home/alice".to_string()),
            "/bin/sh",
            ShellClassification::PosixSh,
            None,
            Some("/usr/bin:/bin".to_string()),
            "/workspace",
            None,
            false,
            None,
            Vec::new(),
        )
        .unwrap()
        .with_process_identity(
            1000,
            1000,
            vec![
                EnvironmentGroup {
                    id: 27,
                    name: "sudo".to_string(),
                },
                EnvironmentGroup {
                    id: 998,
                    name: "docker".to_string(),
                },
                EnvironmentGroup {
                    id: 1000,
                    name: "alice".to_string(),
                },
            ],
        )
        .unwrap()
    }

    /// Verifies configured names map only to active pane GIDs and contribute
    /// to a deterministic identity independent of configuration order.
    #[test]
    fn resolves_configured_groups_from_active_pane_identity() {
        let first = ConfiguredSandboxGroups {
            requested_names: vec!["docker".to_string(), "sudo".to_string()],
        };
        let second = ConfiguredSandboxGroups {
            requested_names: vec!["sudo".to_string(), "docker".to_string()],
        };

        let first = resolve_sandbox_identity(&first, &pane_environment()).unwrap();
        let second = resolve_sandbox_identity(&second, &pane_environment()).unwrap();

        assert_eq!(
            first
                .supplementary_groups
                .iter()
                .map(|group| (group.canonical_name.as_str(), group.group_id))
                .collect::<Vec<_>>(),
            vec![("sudo", 27), ("docker", 998)]
        );
        assert_eq!(first.identity_sha256, second.identity_sha256);
    }

    /// Verifies unavailable or primary-group mappings are omitted without
    /// manufacturing or duplicating pane authority.
    #[test]
    fn omits_groups_absent_from_or_primary_in_pane_identity() {
        for name in ["wheel", "alice"] {
            let configured = ConfiguredSandboxGroups {
                requested_names: vec![name.to_string()],
            };
            let identity = resolve_sandbox_identity(&configured, &pane_environment()).unwrap();
            assert!(identity.supplementary_groups.is_empty());
            assert_eq!(identity.mapping_warnings.len(), 1);
            assert_eq!(identity.mapping_warnings[0].configured_value, name);
        }
    }
}
