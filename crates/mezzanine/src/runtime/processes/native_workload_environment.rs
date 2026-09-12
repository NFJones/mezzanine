//! Native workload environment composition for spawned-shell execution.
//!
//! Native shell mode composes the environment of every process it launches
//! instead of inheriting the ambient `mez` process environment. This module is
//! the single owner of that composition: the native policy-only, host-access,
//! Bubblewrap, and Seatbelt launch paths all start from a cleared base and add
//! only validated pane-root evidence plus the narrowly enumerated runtime
//! requirements declared here. An ambient-only credential that exists only in
//! the daemon environment therefore never reaches a workload shell, a workload
//! interpreter, or a code-owned sandbox launcher.
//!
//! The guarantee is composition, not credential non-possession: a value the
//! pane root itself carries, including one the pane inherited when it was
//! created, is authoritative pane evidence and is forwarded by design. Pane
//! creation owns its own environment-inheritance boundary in
//! `pane_creation_environment.rs`, which this module does not filter.
//!
//! Composition rules:
//! - Every entry key MUST be a portable environment name
//!   (`[A-Za-z_][A-Za-z0-9_]*`), MUST contain no `=` or NUL byte, and MUST stay
//!   inside the documented name, value, entry-count, and aggregate-size budget.
//!   Malformed or oversized optional evidence is dropped deterministically.
//! - Validated pane-root evidence is authoritative: it overrides declared
//!   runtime fallbacks for overlapping keys, and the last valid occurrence wins
//!   inside the evidence itself.
//! - The ambient `mez` environment is consulted only for requirement keys that
//!   explicitly declare ambient forwarding (`PATH`, `HOME`, and the launcher
//!   search path) and only when pane-root evidence does not supply the key.
//! - The launcher/control bucket is applied only to code-owned launcher
//!   processes such as the Seatbelt child supervisor or `bwrap`. Workload
//!   credentials are never copied into that bucket.
//! - Sandbox-owned `HOME`, `XDG_*`, and whitelist projections keep their
//!   existing precedence because the sandbox plan applies them to the sandboxed
//!   payload, not this module to the outer launcher.
//! - Pane identity requirement values are the only required keys; a missing or
//!   malformed required value is a typed pre-dispatch error that names the
//!   requirement category and key before any process is created.
//! - Capability probes are payload-free, host-owned checks whose sandbox
//!   environment the compiled proof plan owns (`--clearenv` plus fixed
//!   `--setenv`, or the Seatbelt probe profile). They carry no pane or workload
//!   data, so an ambient probe value is never projected into a probe sandbox.

use std::os::unix::ffi::OsStringExt;
use std::path::Path;

use mez_mux::process::RawEnvironmentEntry;

use crate::error::{MezError, Result};

/// Documented fallback command-search `PATH` used when neither pane-root
/// evidence nor the declared ambient forwarding source supplies `PATH`.
pub(crate) const NATIVE_WORKLOAD_PATH_FALLBACK: &str = "/usr/local/bin:/usr/bin:/bin";
/// Maximum validated entries accepted for one composed native environment.
pub(crate) const NATIVE_WORKLOAD_MAX_ENTRIES: usize = 512;
/// Maximum validated variable-name bytes.
const NATIVE_WORKLOAD_MAX_NAME_BYTES: usize = 128;
/// Maximum validated variable-value bytes.
const NATIVE_WORKLOAD_MAX_VALUE_BYTES: usize = 16 * 1024;
/// Maximum aggregate validated value bytes for one composed environment.
pub(crate) const NATIVE_WORKLOAD_MAX_TOTAL_VALUE_BYTES: usize = 256 * 1024;

/// Which launch role consumes one composed environment bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum NativeLaunchEnvironmentRole {
    /// The payload: the workload shell or one direct dialect interpreter.
    #[default]
    Workload,
    /// A code-owned launcher or sandbox supervisor that carries payload argv.
    SandboxLauncher,
}

impl NativeLaunchEnvironmentRole {
    /// Selects the launch role for one native dispatch.
    ///
    /// A compiled Bubblewrap or Seatbelt dispatch hands the payload argv to a
    /// code-owned launcher process, so both backends launch with the launcher
    /// bucket; every other native launch is the workload itself.
    pub(crate) const fn for_native_dispatch(
        bubblewrap_dispatch_active: bool,
        seatbelt_dispatch_active: bool,
    ) -> Self {
        if bubblewrap_dispatch_active || seatbelt_dispatch_active {
            Self::SandboxLauncher
        } else {
            Self::Workload
        }
    }
}

/// Visibility of one declared runtime environment requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeLaunchEnvironmentVisibility {
    /// The workload process may see the key.
    Workload,
    /// Only a code-owned launcher process may see the key.
    Launcher,
}

/// Resolution policy for one declared runtime environment requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeLaunchEnvironmentPresence {
    /// A missing or malformed value is a typed pre-dispatch error.
    Required,
    /// A missing value uses the caller-owned fallback value.
    Fallback,
    /// A missing value omits the key unless a fallback value is supplied.
    Omitted,
}

/// One code-owned native environment requirement.
///
/// The declaration names the stable error category, the exact environment key,
/// the launch bucket that may see the key, how absent evidence is handled, and
/// whether the ambient `mez` value may be forwarded when pane-root evidence
/// does not supply the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeLaunchEnvironmentRequirement {
    /// Stable category named by typed requirement errors.
    pub(crate) category: &'static str,
    /// Exact environment key the requirement supplies.
    pub(crate) key: &'static str,
    /// Launch bucket allowed to see the key.
    pub(crate) visibility: NativeLaunchEnvironmentVisibility,
    /// Absent-value resolution policy.
    pub(crate) presence: NativeLaunchEnvironmentPresence,
    /// Whether the ambient `mez` value is deliberately forwarded.
    pub(crate) ambient_forwarded: bool,
}

impl NativeLaunchEnvironmentRequirement {
    /// Builds one requirement declaration from its explicit fields.
    const fn new(
        category: &'static str,
        key: &'static str,
        visibility: NativeLaunchEnvironmentVisibility,
        presence: NativeLaunchEnvironmentPresence,
        ambient_forwarded: bool,
    ) -> Self {
        Self {
            category,
            key,
            visibility,
            presence,
            ambient_forwarded,
        }
    }

    /// Declares one workload-visible key whose absence is a pre-dispatch error.
    pub(crate) const fn workload_required(category: &'static str, key: &'static str) -> Self {
        Self::new(
            category,
            key,
            NativeLaunchEnvironmentVisibility::Workload,
            NativeLaunchEnvironmentPresence::Required,
            false,
        )
    }

    /// Declares one workload-visible key with an optional fallback value.
    pub(crate) const fn workload_fallback(
        category: &'static str,
        key: &'static str,
        ambient_forwarded: bool,
    ) -> Self {
        Self::new(
            category,
            key,
            NativeLaunchEnvironmentVisibility::Workload,
            NativeLaunchEnvironmentPresence::Fallback,
            ambient_forwarded,
        )
    }

    /// Declares one workload-visible key that is omitted when absent.
    pub(crate) const fn workload_omitted(
        category: &'static str,
        key: &'static str,
        ambient_forwarded: bool,
    ) -> Self {
        Self::new(
            category,
            key,
            NativeLaunchEnvironmentVisibility::Workload,
            NativeLaunchEnvironmentPresence::Omitted,
            ambient_forwarded,
        )
    }

    /// Declares one launcher-only key with an optional fallback value.
    pub(crate) const fn launcher_fallback(
        category: &'static str,
        key: &'static str,
        ambient_forwarded: bool,
    ) -> Self {
        Self::new(
            category,
            key,
            NativeLaunchEnvironmentVisibility::Launcher,
            NativeLaunchEnvironmentPresence::Fallback,
            ambient_forwarded,
        )
    }

    /// Workload command-search `PATH`.
    ///
    /// Pane-root evidence wins. The ambient `mez` `PATH` is the deliberate
    /// compatibility fallback for panes whose root-process environment is
    /// unreadable, and the documented constant closes the chain.
    pub(crate) const WORKLOAD_PATH: Self = Self::workload_fallback("workload_path", "PATH", true);
    /// Workload `SHELL`.
    ///
    /// Pane-root evidence wins; otherwise the shell selected by the native
    /// inference chain is recorded so the workload keeps a truthful shell name.
    pub(crate) const WORKLOAD_SHELL: Self =
        Self::workload_omitted("workload_shell", "SHELL", false);
    /// Workload `HOME`.
    ///
    /// Pane-root evidence wins. The ambient `mez` home is forwarded only when
    /// the pane root supplied none, because both processes run as the same
    /// user identity; the key is omitted when neither source exists.
    pub(crate) const WORKLOAD_HOME: Self = Self::workload_omitted("workload_home", "HOME", true);
    /// Launcher-only command-search `PATH`.
    ///
    /// This is the single deliberate launcher exception: a configured sandbox
    /// launcher executable may be a bare name, so the code-owned launcher keeps
    /// a search path while workload credentials stay out of its bucket.
    pub(crate) const LAUNCHER_SEARCH_PATH: Self =
        Self::launcher_fallback("launcher_search_path", "PATH", true);
    /// Pane identity required by one admitted pane-status provider launch.
    pub(crate) const PANE_IDENTITY: Self = Self::workload_required("pane_identity", "MEZ_PANE_ID");
}

/// One fully composed native launch environment.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct NativeWorkloadEnvironment {
    /// Validated, normalized pane-root evidence.
    pane_root_evidence: Vec<RawEnvironmentEntry>,
    /// Workload-visible entries: cleared base, declared defaults, evidence.
    workload: Vec<RawEnvironmentEntry>,
    /// Launcher-only entries: cleared base plus declared launcher requirements.
    launcher: Vec<RawEnvironmentEntry>,
}

impl NativeWorkloadEnvironment {
    /// Returns the validated pane-root evidence used as the authoritative
    /// overlay source for signatures and forwarding evidence.
    pub(crate) fn pane_root_evidence(&self) -> &[RawEnvironmentEntry] {
        &self.pane_root_evidence
    }

    /// Returns the workload-visible environment bucket.
    pub(crate) fn workload(&self) -> &[RawEnvironmentEntry] {
        &self.workload
    }

    /// Returns the launcher-only environment bucket.
    pub(crate) fn launcher(&self) -> &[RawEnvironmentEntry] {
        &self.launcher
    }

    /// Returns the bucket one launch role receives.
    pub(crate) fn for_role(&self, role: NativeLaunchEnvironmentRole) -> &[RawEnvironmentEntry] {
        match role {
            NativeLaunchEnvironmentRole::Workload => self.workload(),
            NativeLaunchEnvironmentRole::SandboxLauncher => self.launcher(),
        }
    }

    /// Returns one workload-visible value as text when it is valid UTF-8.
    pub(crate) fn workload_value(&self, key: &str) -> Option<&str> {
        lookup_value(&self.workload, key.as_bytes())
            .and_then(|value| std::str::from_utf8(value).ok())
    }

    /// Returns a copy that keeps only the launcher control bucket.
    ///
    /// Admitted pane-status providers run their payload inside a compiled
    /// sandbox that owns the payload environment, so the outer launcher keeps
    /// its search path while pane evidence and workload credentials are
    /// dropped.
    pub(crate) fn restricted_to_launcher(&self) -> Self {
        Self {
            pane_root_evidence: Vec::new(),
            workload: Vec::new(),
            launcher: self.launcher.clone(),
        }
    }

    /// Returns a copy that additionally carries one required workload value.
    ///
    /// # Errors
    /// Returns the typed pre-dispatch error when the requirement is required
    /// and the supplied value is missing, malformed, or oversized.
    pub(crate) fn with_required_workload_value(
        &self,
        requirement: NativeLaunchEnvironmentRequirement,
        value: &str,
    ) -> Result<Self> {
        Ok(NativeWorkloadEnvironmentBuilder::from_environment(self)
            .with_requirement_value(requirement, value)?
            .build())
    }
}

/// Incremental owner of native workload environment composition.
#[derive(Debug, Clone, Default)]
pub(crate) struct NativeWorkloadEnvironmentBuilder {
    /// Validated pane-root evidence.
    evidence: Vec<RawEnvironmentEntry>,
    /// Ambient `mez` environment consulted only for declared forwarding.
    ambient: Vec<RawEnvironmentEntry>,
    /// Declared workload defaults overridden by pane-root evidence.
    workload_defaults: Vec<RawEnvironmentEntry>,
    /// Runtime-owned workload values that override pane-root evidence.
    workload_required: Vec<RawEnvironmentEntry>,
    /// Declared launcher-only entries.
    launcher: Vec<RawEnvironmentEntry>,
}

impl NativeWorkloadEnvironmentBuilder {
    /// Starts composition from a cleared environment.
    pub(crate) const fn new() -> Self {
        Self {
            evidence: Vec::new(),
            ambient: Vec::new(),
            workload_defaults: Vec::new(),
            workload_required: Vec::new(),
            launcher: Vec::new(),
        }
    }

    /// Continues composition from one already composed environment.
    pub(crate) fn from_environment(environment: &NativeWorkloadEnvironment) -> Self {
        Self {
            evidence: environment.pane_root_evidence().to_vec(),
            ambient: Vec::new(),
            workload_defaults: environment.workload().to_vec(),
            workload_required: Vec::new(),
            launcher: environment.launcher().to_vec(),
        }
    }

    /// Adds validated pane-root evidence as the authoritative overlay source.
    pub(crate) fn with_pane_root_evidence(mut self, raw: &[RawEnvironmentEntry]) -> Self {
        self.evidence = validated_environment_entries(raw);
        self
    }

    /// Adds the ambient `mez` environment as a declared-forwarding source.
    pub(crate) fn with_ambient_environment(mut self, raw: &[RawEnvironmentEntry]) -> Self {
        self.ambient = validated_environment_entries(raw);
        self
    }

    /// Resolves one declared requirement from evidence, ambient forwarding, and
    /// the caller-owned fallback value.
    ///
    /// # Parameters
    /// - `requirement`: Code-owned declaration for the key and its policy.
    /// - `fallback`: Value used when neither evidence nor ambient supplies it.
    ///
    /// # Errors
    /// Returns the typed pre-dispatch error when the requirement is required
    /// and no source produced a value.
    pub(crate) fn with_requirement(
        mut self,
        requirement: NativeLaunchEnvironmentRequirement,
        fallback: Option<&str>,
    ) -> Result<Self> {
        if let Some(value) = self.resolved_requirement_value(requirement) {
            self.insert_requirement(requirement, value.to_vec());
            return Ok(self);
        }
        if let Some(fallback) = fallback
            .map(str::as_bytes)
            .filter(|value| environment_value_is_valid(value))
        {
            self.insert_requirement(requirement, fallback.to_vec());
            return Ok(self);
        }
        match requirement.presence {
            NativeLaunchEnvironmentPresence::Required => {
                Err(missing_requirement_error(requirement))
            }
            NativeLaunchEnvironmentPresence::Fallback
            | NativeLaunchEnvironmentPresence::Omitted => Ok(self),
        }
    }

    /// Adds one requirement satisfied by a runtime-owned value instead of
    /// host evidence.
    ///
    /// # Errors
    /// Returns the typed pre-dispatch error when the requirement is required
    /// and the supplied value is empty, malformed, or oversized.
    pub(crate) fn with_requirement_value(
        mut self,
        requirement: NativeLaunchEnvironmentRequirement,
        value: &str,
    ) -> Result<Self> {
        let bytes = value.as_bytes();
        if !bytes.is_empty() && runtime_value_is_valid(bytes) {
            let entries = match requirement.visibility {
                NativeLaunchEnvironmentVisibility::Workload => &mut self.workload_required,
                NativeLaunchEnvironmentVisibility::Launcher => &mut self.launcher,
            };
            insert_entry(entries, requirement.key.as_bytes().to_vec(), bytes.to_vec());
            return Ok(self);
        }
        match requirement.presence {
            NativeLaunchEnvironmentPresence::Required => {
                Err(missing_requirement_error(requirement))
            }
            NativeLaunchEnvironmentPresence::Fallback
            | NativeLaunchEnvironmentPresence::Omitted => Ok(self),
        }
    }

    /// Finishes composition into one native workload environment.
    ///
    /// The composed buckets are capped by the documented entry-count and
    /// aggregate-value budget, so the allowance of one source cannot push the
    /// launch past the documented environment size.
    pub(crate) fn build(mut self) -> NativeWorkloadEnvironment {
        let mut workload = std::mem::take(&mut self.workload_defaults);
        for entry in &self.evidence {
            insert_entry(&mut workload, entry.key.clone(), entry.value.clone());
        }
        for entry in &self.workload_required {
            insert_entry(&mut workload, entry.key.clone(), entry.value.clone());
        }
        NativeWorkloadEnvironment {
            pane_root_evidence: self.evidence,
            workload: enforce_composed_environment_budget(workload, &self.workload_required),
            launcher: enforce_composed_environment_budget(self.launcher, &[]),
        }
    }

    /// Returns the value one requirement resolves to, if any.
    fn resolved_requirement_value(
        &self,
        requirement: NativeLaunchEnvironmentRequirement,
    ) -> Option<&[u8]> {
        let key = requirement.key.as_bytes();
        if let Some(value) = lookup_value(&self.evidence, key) {
            return Some(value);
        }
        if requirement.ambient_forwarded {
            return lookup_value(&self.ambient, key);
        }
        None
    }

    /// Records one resolved requirement in its launch bucket.
    fn insert_requirement(
        &mut self,
        requirement: NativeLaunchEnvironmentRequirement,
        value: Vec<u8>,
    ) {
        let entries = match requirement.visibility {
            NativeLaunchEnvironmentVisibility::Workload => &mut self.workload_defaults,
            NativeLaunchEnvironmentVisibility::Launcher => &mut self.launcher,
        };
        insert_entry(entries, requirement.key.as_bytes().to_vec(), value);
    }
}

/// Composes the native workload environment for one inferred pane context.
///
/// # Parameters
/// - `pane_root_environment`: Host-reported pane root-process environment.
/// - `ambient_environment`: Ambient `mez` environment consulted only through
///   declared forwarding requirements.
/// - `selected_shell_path`: Shell selected by the inference fallback chain and
///   used as the documented `SHELL` value when the pane root supplies none.
///
/// # Errors
/// Returns the typed pre-dispatch error when a declared required requirement
/// cannot be satisfied.
pub(crate) fn compose_native_workload_environment(
    pane_root_environment: &[RawEnvironmentEntry],
    ambient_environment: &[RawEnvironmentEntry],
    selected_shell_path: &Path,
) -> Result<NativeWorkloadEnvironment> {
    let shell_value = selected_shell_path.to_string_lossy().into_owned();
    Ok(NativeWorkloadEnvironmentBuilder::new()
        .with_pane_root_evidence(pane_root_environment)
        .with_ambient_environment(ambient_environment)
        .with_requirement(
            NativeLaunchEnvironmentRequirement::WORKLOAD_PATH,
            Some(NATIVE_WORKLOAD_PATH_FALLBACK),
        )?
        .with_requirement(
            NativeLaunchEnvironmentRequirement::WORKLOAD_SHELL,
            Some(shell_value.as_str()),
        )?
        .with_requirement(NativeLaunchEnvironmentRequirement::WORKLOAD_HOME, None)?
        .with_requirement(
            NativeLaunchEnvironmentRequirement::LAUNCHER_SEARCH_PATH,
            Some(NATIVE_WORKLOAD_PATH_FALLBACK),
        )?
        .build())
}

/// Captures the ambient `mez` process environment as raw entries.
///
/// The snapshot is consulted only for requirements that declare ambient
/// forwarding, so ambient-only credentials cannot reach a workload even though
/// the snapshot itself contains them.
pub(crate) fn native_ambient_environment() -> Vec<RawEnvironmentEntry> {
    std::env::vars_os()
        .map(|(key, value)| RawEnvironmentEntry {
            key: key.into_vec(),
            value: value.into_vec(),
        })
        .collect()
}

/// Composes the launcher control environment for one code-owned launcher
/// process that runs outside a pane launch, such as a capability-probe
/// launcher.
///
/// The bucket carries only the declared launcher command-search `PATH`, so an
/// ambient credential or a loader variable such as `LD_PRELOAD` or
/// `DYLD_INSERT_LIBRARIES` cannot enter a probe launcher process. The probe
/// payload environment stays owned by the compiled proof plan.
pub(crate) fn launcher_control_environment(
    ambient_environment: &[RawEnvironmentEntry],
) -> Vec<RawEnvironmentEntry> {
    NativeWorkloadEnvironmentBuilder::new()
        .with_ambient_environment(ambient_environment)
        .with_requirement(
            NativeLaunchEnvironmentRequirement::LAUNCHER_SEARCH_PATH,
            Some(NATIVE_WORKLOAD_PATH_FALLBACK),
        )
        .map_or_else(
            // The declared launcher requirement is optional, so composition
            // never fails; an absent search path stays absent rather than
            // restoring the ambient environment.
            |_| Vec::new(),
            |builder| builder.build().launcher().to_vec(),
        )
}

/// Validates, normalizes, and deduplicates raw environment entries.
///
/// Malformed or oversized entries are dropped deterministically, the last
/// valid occurrence of a duplicate key wins, and the first-seen position of a
/// key is preserved so composition order stays stable across runs.
fn validated_environment_entries(raw: &[RawEnvironmentEntry]) -> Vec<RawEnvironmentEntry> {
    let mut entries: Vec<RawEnvironmentEntry> = Vec::new();
    let mut total_value_bytes = 0usize;
    for entry in raw {
        if entries.len() >= NATIVE_WORKLOAD_MAX_ENTRIES {
            break;
        }
        if !environment_entry_is_valid(entry) {
            continue;
        }
        let previous = lookup_value(&entries, &entry.key).map_or(0, <[u8]>::len);
        if total_value_bytes
            .saturating_sub(previous)
            .saturating_add(entry.value.len())
            > NATIVE_WORKLOAD_MAX_TOTAL_VALUE_BYTES
        {
            continue;
        }
        total_value_bytes = total_value_bytes.saturating_sub(previous) + entry.value.len();
        insert_entry(&mut entries, entry.key.clone(), entry.value.clone());
    }
    entries
}

/// Enforces the documented entry-count and aggregate-value budget on one
/// composed bucket.
///
/// Runtime-owned required entries are charged against the budget first, so a
/// flood of optional pane-root evidence can never evict a required value.
/// Optional entries then keep their composition order, and an entry that does
/// not fit either budget is dropped while later entries are still considered.
/// That matches the existing optional-evidence policy of dropping the offending
/// entry deterministically instead of failing the launch.
fn enforce_composed_environment_budget(
    entries: Vec<RawEnvironmentEntry>,
    required: &[RawEnvironmentEntry],
) -> Vec<RawEnvironmentEntry> {
    let mut kept: Vec<RawEnvironmentEntry> =
        Vec::with_capacity(entries.len().min(NATIVE_WORKLOAD_MAX_ENTRIES));
    let mut total_value_bytes = 0usize;
    for entry in &entries {
        if required.iter().any(|value| value.key == entry.key) {
            charge_composed_entry(entry, &mut kept, &mut total_value_bytes);
        }
    }
    for entry in &entries {
        if !required.iter().any(|value| value.key == entry.key) {
            charge_composed_entry(entry, &mut kept, &mut total_value_bytes);
        }
    }
    kept
}

/// Charges one composed entry against the documented budget when it still fits.
fn charge_composed_entry(
    entry: &RawEnvironmentEntry,
    kept: &mut Vec<RawEnvironmentEntry>,
    total_value_bytes: &mut usize,
) {
    if kept.len() >= NATIVE_WORKLOAD_MAX_ENTRIES
        || total_value_bytes.saturating_add(entry.value.len())
            > NATIVE_WORKLOAD_MAX_TOTAL_VALUE_BYTES
    {
        return;
    }
    *total_value_bytes = total_value_bytes.saturating_add(entry.value.len());
    kept.push(entry.clone());
}

/// Returns true when one raw entry is a safe optional environment mapping.
fn environment_entry_is_valid(entry: &RawEnvironmentEntry) -> bool {
    portable_environment_key(&entry.key) && environment_value_is_valid(&entry.value)
}

/// Returns true when one raw key is a portable environment name.
pub(crate) fn portable_environment_key(key: &[u8]) -> bool {
    !key.is_empty()
        && key.len() <= NATIVE_WORKLOAD_MAX_NAME_BYTES
        && (key[0] == b'_' || key[0].is_ascii_alphabetic())
        && key[1..]
            .iter()
            .all(|byte| *byte == b'_' || byte.is_ascii_alphanumeric())
}

/// Returns true when one raw value stays inside the documented value budget.
pub(crate) fn environment_value_is_valid(value: &[u8]) -> bool {
    value.len() <= NATIVE_WORKLOAD_MAX_VALUE_BYTES && !value.contains(&0)
}

/// Returns true when one runtime-owned requirement value is safe to project.
fn runtime_value_is_valid(value: &[u8]) -> bool {
    value.len() <= NATIVE_WORKLOAD_MAX_VALUE_BYTES
        && !value.contains(&0)
        && !value.iter().any(u8::is_ascii_control)
}

/// Inserts or replaces one entry while preserving the first-seen position.
fn insert_entry(entries: &mut Vec<RawEnvironmentEntry>, key: Vec<u8>, value: Vec<u8>) {
    if let Some(existing) = entries.iter_mut().find(|entry| entry.key == key) {
        existing.value = value;
        return;
    }
    entries.push(RawEnvironmentEntry { key, value });
}

/// Returns the last validated value stored for one key.
fn lookup_value<'a>(entries: &'a [RawEnvironmentEntry], key: &[u8]) -> Option<&'a [u8]> {
    entries
        .iter()
        .rev()
        .find(|entry| entry.key == key)
        .map(|entry| entry.value.as_slice())
}

/// Builds the typed pre-dispatch error for one unsatisfied requirement.
fn missing_requirement_error(requirement: NativeLaunchEnvironmentRequirement) -> MezError {
    MezError::invalid_state(format!(
        "native workload environment rejected the launch before dispatch: required {} value for {} is missing or malformed",
        requirement.category, requirement.key
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one raw environment entry fixture.
    fn entry(key: &str, value: &str) -> RawEnvironmentEntry {
        RawEnvironmentEntry {
            key: key.as_bytes().to_vec(),
            value: value.as_bytes().to_vec(),
        }
    }

    /// Builds the ambient daemon fixture used by composition tests.
    ///
    /// The fixture models a daemon environment that carries a harness-only
    /// credential and a duplicate key, without mutating the test process
    /// environment.
    fn daemon_ambient_fixture() -> Vec<RawEnvironmentEntry> {
        vec![
            entry("PATH", "/daemon/bin:/usr/bin:/bin"),
            entry("HOME", "/home/daemon-user"),
            entry("MEZ_DAEMON_ONLY_SENTINEL", "harness-only-credential"),
            entry("MEZ_DUPLICATE_KEY", "daemon"),
        ]
    }

    /// Builds one composed fixture environment for the supplied evidence.
    fn compose(evidence: &[RawEnvironmentEntry]) -> NativeWorkloadEnvironment {
        compose_native_workload_environment(
            evidence,
            &daemon_ambient_fixture(),
            Path::new("/bin/bash"),
        )
        .expect("fixture composition succeeds")
    }

    /// Builds the synthetic compatibility inventory of daemon-only environment
    /// categories that composition must classify: command search, proxies,
    /// locale, agent sockets, toolchain roots, and one harness credential.
    fn compatibility_inventory_fixture() -> Vec<RawEnvironmentEntry> {
        vec![
            entry("PATH", "/daemon/bin:/usr/bin:/bin"),
            entry("HOME", "/home/daemon-user"),
            entry("HTTPS_PROXY", "http://daemon-proxy.invalid:3128"),
            entry("NO_PROXY", "localhost,127.0.0.1"),
            entry("LANG", "en_US.UTF-8"),
            entry("LC_ALL", "en_US.UTF-8"),
            entry("SSH_AUTH_SOCK", "/run/daemon-agent.sock"),
            entry("CARGO_HOME", "/daemon/.cargo"),
            entry("RUSTUP_HOME", "/daemon/.rustup"),
            entry("MEZ_HARNESS_DAEMON_TOKEN", "harness-only"),
        ]
    }

    /// Verifies the daemon compatibility inventory classifies every ambient
    /// category deliberately: only the declared ambient-forwarded requirements
    /// survive, while daemon-only proxies, locale, agent sockets, toolchain
    /// roots, and harness credentials are dropped.
    #[test]
    fn compatibility_inventory_drops_ambient_only_dependencies() {
        let environment = compose_native_workload_environment(
            &[entry("MEZ_PANE_PROVIDED", "pane-value")],
            &compatibility_inventory_fixture(),
            Path::new("/bin/bash"),
        )
        .expect("inventory composition succeeds");
        let workload = environment.workload();

        assert_eq!(
            lookup_value(workload, b"PATH"),
            Some(b"/daemon/bin:/usr/bin:/bin".as_slice())
        );
        assert_eq!(
            lookup_value(workload, b"HOME"),
            Some(b"/home/daemon-user".as_slice())
        );
        for key in [
            "HTTPS_PROXY",
            "NO_PROXY",
            "LANG",
            "LC_ALL",
            "SSH_AUTH_SOCK",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "MEZ_HARNESS_DAEMON_TOKEN",
        ] {
            assert_eq!(
                lookup_value(workload, key.as_bytes()),
                None,
                "ambient-only {key} must not reach the workload"
            );
        }
    }

    /// Verifies intentional pane exports of the same categories stay available,
    /// because the pane root process is the authoritative source for a pane's own
    /// proxy, locale, agent socket, and toolchain environment.
    #[test]
    fn compatibility_inventory_forwards_intentional_pane_exports() {
        let environment = compose_native_workload_environment(
            &[
                entry("HTTPS_PROXY", "http://pane-proxy.invalid:3128"),
                entry("LANG", "fr_FR.UTF-8"),
                entry("SSH_AUTH_SOCK", "/run/pane-agent.sock"),
                entry("CARGO_HOME", "/home/pane/.cargo"),
            ],
            &compatibility_inventory_fixture(),
            Path::new("/bin/bash"),
        )
        .expect("pane export composition succeeds");
        let workload = environment.workload();

        assert_eq!(
            lookup_value(workload, b"HTTPS_PROXY"),
            Some(b"http://pane-proxy.invalid:3128".as_slice())
        );
        assert_eq!(
            lookup_value(workload, b"LANG"),
            Some(b"fr_FR.UTF-8".as_slice())
        );
        assert_eq!(
            lookup_value(workload, b"SSH_AUTH_SOCK"),
            Some(b"/run/pane-agent.sock".as_slice())
        );
        assert_eq!(
            lookup_value(workload, b"CARGO_HOME"),
            Some(b"/home/pane/.cargo".as_slice())
        );
        assert_eq!(lookup_value(workload, b"RUSTUP_HOME"), None);
    }

    /// Verifies composition drops a daemon-only sentinel while preserving a
    /// distinct pane-provided value that wins on a duplicate key.
    #[test]
    fn composition_drops_daemon_only_sentinel_and_prefers_pane_values() {
        let environment = compose(&[
            entry("MEZ_PANE_PROVIDED", "pane-value"),
            entry("MEZ_DUPLICATE_KEY", "pane"),
        ]);
        let workload = environment.workload();

        assert_eq!(lookup_value(workload, b"MEZ_DAEMON_ONLY_SENTINEL"), None);
        assert_eq!(
            lookup_value(workload, b"MEZ_PANE_PROVIDED"),
            Some(b"pane-value".as_slice())
        );
        assert_eq!(
            lookup_value(workload, b"MEZ_DUPLICATE_KEY"),
            Some(b"pane".as_slice())
        );
        assert_eq!(
            environment.workload_value("MEZ_DUPLICATE_KEY"),
            Some("pane")
        );
    }

    /// Verifies the ambient source is consulted only for declared forwarding
    /// requirements instead of for every ambient key.
    #[test]
    fn composition_forwards_only_declared_ambient_requirements() {
        let environment = compose(&[entry("MEZ_PANE_PROVIDED", "pane-value")]);
        let workload = environment.workload();

        assert_eq!(
            lookup_value(workload, b"PATH"),
            Some(b"/daemon/bin:/usr/bin:/bin".as_slice())
        );
        assert_eq!(
            lookup_value(workload, b"HOME"),
            Some(b"/home/daemon-user".as_slice())
        );
        assert_eq!(lookup_value(workload, b"MEZ_DAEMON_ONLY_SENTINEL"), None);
    }

    /// Verifies pane-root evidence overrides declared ambient forwarding and
    /// runtime fallbacks for overlapping keys.
    #[test]
    fn composition_keeps_pane_evidence_authoritative() {
        let environment = compose(&[
            entry("PATH", "/pane/bin"),
            entry("HOME", "/home/pane-user"),
            entry("SHELL", "/bin/zsh"),
        ]);
        let workload = environment.workload();

        assert_eq!(
            lookup_value(workload, b"PATH"),
            Some(b"/pane/bin".as_slice())
        );
        assert_eq!(
            lookup_value(workload, b"HOME"),
            Some(b"/home/pane-user".as_slice())
        );
        assert_eq!(
            lookup_value(workload, b"SHELL"),
            Some(b"/bin/zsh".as_slice())
        );
    }

    /// Verifies optional absent values fall back to documented defaults instead
    /// of failing composition.
    #[test]
    fn composition_falls_back_for_absent_optional_values() {
        let environment = compose_native_workload_environment(&[], &[], Path::new("/bin/sh"))
            .expect("optional absence never fails");
        let workload = environment.workload();

        assert_eq!(
            lookup_value(workload, b"PATH"),
            Some(NATIVE_WORKLOAD_PATH_FALLBACK.as_bytes())
        );
        assert_eq!(
            lookup_value(workload, b"SHELL"),
            Some(b"/bin/sh".as_slice())
        );
        assert_eq!(lookup_value(workload, b"HOME"), None);
    }

    /// Verifies malformed optional evidence is dropped deterministically while
    /// well-formed neighbors survive.
    #[test]
    fn composition_drops_malformed_optional_evidence() {
        let environment = compose(&[
            entry("1INVALID", "value"),
            entry("MEZ_NUL_VALUE", "bad\u{0}value"),
            entry("MEZ_VALID", "kept"),
        ]);
        let workload = environment.workload();

        assert_eq!(lookup_value(workload, b"1INVALID"), None);
        assert_eq!(lookup_value(workload, b"MEZ_NUL_VALUE"), None);
        assert_eq!(
            lookup_value(workload, b"MEZ_VALID"),
            Some(b"kept".as_slice())
        );
    }

    /// Verifies duplicate pane-root evidence resolves to the last valid
    /// occurrence, matching the existing evidence precedence rules.
    #[test]
    fn composition_uses_last_duplicate_evidence_occurrence() {
        let environment = compose(&[
            entry("MEZ_DUPLICATE_KEY", "first"),
            entry("MEZ_DUPLICATE_KEY", "last"),
        ]);

        assert_eq!(
            lookup_value(environment.workload(), b"MEZ_DUPLICATE_KEY"),
            Some(b"last".as_slice())
        );
    }

    /// Verifies the launcher control bucket never carries workload credentials
    /// while it keeps the deliberate launcher search path exception.
    #[test]
    fn launcher_bucket_excludes_workload_credentials() {
        let environment = compose(&[
            entry("MEZ_PANE_CREDENTIAL", "pane-secret"),
            entry("PATH", "/pane/bin"),
        ]);
        let launcher = environment.launcher();

        assert_eq!(
            lookup_value(launcher, b"PATH"),
            Some(b"/pane/bin".as_slice())
        );
        assert_eq!(lookup_value(launcher, b"MEZ_PANE_CREDENTIAL"), None);
        assert_eq!(lookup_value(launcher, b"MEZ_DAEMON_ONLY_SENTINEL"), None);
        assert!(matches!(
            environment.for_role(NativeLaunchEnvironmentRole::SandboxLauncher),
            launcher_entries if std::ptr::eq(launcher_entries, launcher)
        ));
    }

    /// Verifies the restricted launcher environment drops pane evidence and
    /// workload entries while keeping launcher control entries.
    #[test]
    fn restricted_environment_keeps_only_launcher_control_entries() {
        let environment = compose(&[
            entry("MEZ_PANE_CREDENTIAL", "pane-secret"),
            entry("PATH", "/pane/bin"),
        ])
        .restricted_to_launcher();

        assert!(environment.pane_root_evidence().is_empty());
        assert!(environment.workload().is_empty());
        assert_eq!(
            lookup_value(environment.launcher(), b"PATH"),
            Some(b"/pane/bin".as_slice())
        );
    }

    /// Verifies a missing required requirement produces the typed pre-dispatch
    /// error naming both the category and the exact key.
    #[test]
    fn missing_required_requirement_is_a_typed_pre_dispatch_error() {
        let environment = compose(&[entry("MEZ_PANE_PROVIDED", "pane-value")]);
        let error = environment
            .with_required_workload_value(NativeLaunchEnvironmentRequirement::PANE_IDENTITY, "")
            .expect_err("an empty required value must fail before dispatch");
        let message = error.to_string();

        assert!(message.contains("pane_identity"), "message was {message}");
        assert!(message.contains("MEZ_PANE_ID"), "message was {message}");
        assert!(message.contains("before dispatch"), "message was {message}");
    }

    /// Verifies a malformed required requirement value fails closed instead of
    /// falling back to a default.
    #[test]
    fn malformed_required_requirement_value_fails_closed() {
        let environment = compose(&[]);
        let error = environment
            .with_required_workload_value(NativeLaunchEnvironmentRequirement::PANE_IDENTITY, "%1\n")
            .expect_err("a malformed required value must fail before dispatch");

        assert!(error.to_string().contains("MEZ_PANE_ID"));
    }

    /// Verifies a runtime-owned required value overrides overlapping pane-root
    /// evidence so the launch cannot be redirected by stale host metadata.
    #[test]
    fn runtime_required_value_overrides_pane_evidence() {
        let environment = compose(&[entry("MEZ_PANE_ID", "%stale")])
            .with_required_workload_value(
                NativeLaunchEnvironmentRequirement::PANE_IDENTITY,
                "%live",
            )
            .expect("a valid pane identity is accepted");

        assert_eq!(environment.workload_value("MEZ_PANE_ID"), Some("%live"));
    }

    /// Verifies the composed workload environment enforces the documented
    /// entry-count budget on the composed result instead of per source.
    ///
    /// One source can fill its own allowance, so the composed bucket must be
    /// capped as a whole: declared requirements are charged first and optional
    /// pane-root evidence past the cap is dropped deterministically.
    #[test]
    fn composed_environment_enforces_entry_count_budget() {
        let evidence: Vec<RawEnvironmentEntry> = (0..NATIVE_WORKLOAD_MAX_ENTRIES * 2)
            .map(|index| entry(&format!("MEZ_EVIDENCE_{index:04}"), "value"))
            .collect();
        let environment = NativeWorkloadEnvironmentBuilder::new()
            .with_pane_root_evidence(&evidence)
            .with_ambient_environment(&[])
            .with_requirement(
                NativeLaunchEnvironmentRequirement::WORKLOAD_PATH,
                Some(NATIVE_WORKLOAD_PATH_FALLBACK),
            )
            .expect("the workload path requirement has a documented fallback")
            .with_requirement(
                NativeLaunchEnvironmentRequirement::WORKLOAD_SHELL,
                Some("/bin/sh"),
            )
            .expect("the workload shell requirement has a documented fallback")
            .build();
        let workload = environment.workload();

        assert_eq!(
            environment.pane_root_evidence().len(),
            NATIVE_WORKLOAD_MAX_ENTRIES
        );
        assert_eq!(workload.len(), NATIVE_WORKLOAD_MAX_ENTRIES);
        assert_eq!(
            lookup_value(workload, b"PATH"),
            Some(NATIVE_WORKLOAD_PATH_FALLBACK.as_bytes())
        );
        assert!(lookup_value(workload, b"MEZ_EVIDENCE_0000").is_some());
        assert!(
            lookup_value(workload, b"MEZ_EVIDENCE_0511").is_none(),
            "optional evidence past the composed entry budget must be dropped"
        );
    }

    /// Verifies the composed workload environment enforces the documented
    /// aggregate-value budget and never evicts a required runtime value.
    #[test]
    fn composed_environment_enforces_aggregate_value_budget_and_keeps_required_values() {
        let evidence_count = 48usize;
        let value = "v".repeat(8 * 1024);
        let evidence: Vec<RawEnvironmentEntry> = (0..evidence_count)
            .map(|index| entry(&format!("MEZ_EVIDENCE_{index:04}"), &value))
            .collect();
        let environment = compose_native_workload_environment(&evidence, &[], Path::new("/bin/sh"))
            .expect("fixture composition succeeds")
            .with_required_workload_value(
                NativeLaunchEnvironmentRequirement::PANE_IDENTITY,
                "%live",
            )
            .expect("a valid pane identity is accepted");
        let workload = environment.workload();
        let total_value_bytes: usize = workload.iter().map(|entry| entry.value.len()).sum();
        let evidence_entries = workload
            .iter()
            .filter(|entry| entry.key.starts_with(b"MEZ_EVIDENCE_"))
            .count();

        assert!(
            total_value_bytes <= NATIVE_WORKLOAD_MAX_TOTAL_VALUE_BYTES,
            "composed value bytes {total_value_bytes} must stay inside the documented budget"
        );
        assert!(
            evidence_entries < evidence_count,
            "the composed aggregate budget must drop optional evidence instead of concatenating past it"
        );
        assert_eq!(environment.workload_value("MEZ_PANE_ID"), Some("%live"));
    }

    /// Verifies the probe launcher bucket carries only the declared launcher
    /// command-search path, so ambient credentials and loader variables cannot
    /// enter a capability-probe launcher process.
    #[test]
    fn launcher_control_environment_keeps_only_the_declared_search_path() {
        let mut ambient = daemon_ambient_fixture();
        ambient.push(entry("LD_PRELOAD", "/tmp/daemon-injection.so"));
        ambient.push(entry(
            "DYLD_INSERT_LIBRARIES",
            "/tmp/daemon-injection.dylib",
        ));
        let launcher = launcher_control_environment(&ambient);

        assert_eq!(launcher.len(), 1, "launcher bucket was {launcher:?}");
        assert_eq!(launcher[0].key.as_slice(), b"PATH");
        assert_eq!(launcher[0].value.as_slice(), b"/daemon/bin:/usr/bin:/bin");
        assert!(lookup_value(&launcher, b"MEZ_DAEMON_ONLY_SENTINEL").is_none());
        assert!(lookup_value(&launcher, b"LD_PRELOAD").is_none());
        assert!(lookup_value(&launcher, b"DYLD_INSERT_LIBRARIES").is_none());
    }
}
