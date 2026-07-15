//! Environment signatures and cached shell tool-discovery results.
//!
//! These value types model environment identity and discovered command state;
//! callers remain responsible for obtaining and persisting probe output.

use super::{AgentShellValidationError, AgentShellValidationResult, ShellClassification};
use sha2::Digest;
use std::collections::BTreeMap;

/// Carries Environment Signature state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnvironmentSignature {
    /// Stores the os value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub os: String,
    /// Stores the arch value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub arch: String,
    /// Stores the kernel version value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub kernel_version: Option<String>,
    /// Stores the host value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub host: String,
    /// Stores the user value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub user: String,
    /// Stores the shell path value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub shell_path: String,
    /// Stores the shell classification value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub shell_classification: ShellClassification,
    /// Stores the shell version value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub shell_version: Option<String>,
    /// Stores the path value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub path: Option<String>,
    /// Stores the working directory value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub working_directory: String,
    /// Stores the project root value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub project_root: Option<String>,
    /// Stores the git repo value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub git_repo: bool,
    /// Stores the container value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub container: Option<String>,
    /// Stores the environment managers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub environment_managers: Vec<String>,
}

impl EnvironmentSignature {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        os: impl Into<String>,
        arch: impl Into<String>,
        kernel_version: Option<String>,
        host: impl Into<String>,
        user: impl Into<String>,
        shell_path: impl Into<String>,
        shell_classification: ShellClassification,
        shell_version: Option<String>,
        path: Option<String>,
        working_directory: impl Into<String>,
        project_root: Option<String>,
        git_repo: bool,
        container: Option<String>,
        environment_managers: Vec<String>,
    ) -> AgentShellValidationResult<Self> {
        let signature = Self {
            os: os.into(),
            arch: arch.into(),
            kernel_version,
            host: host.into(),
            user: user.into(),
            shell_path: shell_path.into(),
            shell_classification,
            shell_version,
            path,
            working_directory: working_directory.into(),
            project_root,
            git_repo,
            container,
            environment_managers,
        };
        if signature.os.is_empty()
            || signature.arch.is_empty()
            || signature.host.is_empty()
            || signature.user.is_empty()
            || signature.shell_path.is_empty()
            || signature.working_directory.is_empty()
        {
            return Err(AgentShellValidationError::invalid_args(
                "environment signature core fields must not be empty",
            ));
        }
        Ok(signature)
    }

    /// Runs the unknown operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn unknown() -> Self {
        Self {
            os: "unknown".to_string(),
            arch: "unknown".to_string(),
            kernel_version: None,
            host: "unknown".to_string(),
            user: "unknown".to_string(),
            shell_path: "/bin/sh".to_string(),
            shell_classification: ShellClassification::UnknownUnix,
            shell_version: None,
            path: None,
            working_directory: "/".to_string(),
            project_root: None,
            git_repo: false,
            container: None,
            environment_managers: Vec::new(),
        }
    }

    /// Reports whether this signature is the unknown sentinel used before the
    /// runtime can collect real environment details.
    ///
    /// Unknown signatures are intentionally treated as uncached bootstrap
    /// requests so a previously-recorded sentinel cannot suppress discovery for
    /// later sessions that still lack concrete environment identity.
    pub fn is_unknown(&self) -> bool {
        self.os == "unknown"
            && self.arch == "unknown"
            && self.host == "unknown"
            && self.user == "unknown"
            && self.shell_path == "/bin/sh"
            && self.shell_classification == ShellClassification::UnknownUnix
            && self.shell_version.is_none()
            && self.path.is_none()
            && self.working_directory == "/"
            && self.project_root.is_none()
            && !self.git_repo
            && self.container.is_none()
            && self.environment_managers.is_empty()
    }

    /// Runs the known fields operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn known_fields(&self) -> Vec<String> {
        let mut fields = Vec::new();
        fields.push(format!("os={}", self.os));
        fields.push(format!("arch={}", self.arch));
        if let Some(ref kv) = self.kernel_version {
            fields.push(format!("kernel_version={kv}"));
        }
        fields.push(format!("host={}", self.host));
        fields.push(format!("user={}", self.user));
        fields.push(format!("shell_path={}", self.shell_path));
        fields.push(format!(
            "shell_classification={}",
            self.shell_classification.as_str()
        ));
        if let Some(ref sv) = self.shell_version {
            fields.push(format!("shell_version={sv}"));
        }
        if let Some(ref p) = self.path {
            fields.push(format!("path={p}"));
        }
        fields.push(format!("working_directory={}", self.working_directory));
        if let Some(ref pr) = self.project_root {
            fields.push(format!("project_root={pr}"));
        }
        fields.push(format!(
            "git_repo={}",
            if self.git_repo { "1" } else { "0" }
        ));
        if let Some(ref c) = self.container {
            fields.push(format!("container={c}"));
        }
        for manager in &self.environment_managers {
            fields.push(format!("environment_manager={manager}"));
        }
        fields
    }

    /// Returns a stable SHA-256 digest of the full canonical signature.
    ///
    /// The digest lets model-facing context identify the current environment
    /// without copying large or sensitive host details such as `PATH`, host
    /// names, user names, or shell version banners into every request.
    pub fn stable_hash(&self) -> String {
        sha256_hex(self.canonical_hash_fields().join("\n").as_bytes())
    }

    /// Returns compact fields intended for model prompt context.
    ///
    /// This projection keeps execution-critical facts visible while replacing
    /// noisy host details with a fixed-width hash. Runtime caches and audits can
    /// still use the full typed signature.
    pub fn model_context_fields(&self) -> Vec<String> {
        let mut fields = Vec::new();
        fields.push(format!("env_signature=sha256:{}", self.stable_hash()));
        fields.push(format!("cwd={}", self.working_directory));
        fields.push(format!("shell={}", self.shell_classification.as_str()));
        fields.push(format!("shell_path={}", self.shell_path));
        fields.push(format!(
            "git_repo={}",
            if self.git_repo { "1" } else { "0" }
        ));
        if let Some(ref pr) = self.project_root {
            fields.push(format!("project_root={pr}"));
        }
        if let Some(ref container) = self.container {
            fields.push(format!("container={container}"));
        }
        if !self.environment_managers.is_empty() {
            fields.push(format!(
                "environment_managers={}",
                self.environment_managers.join(",")
            ));
        }
        if let Some(ref path) = self.path {
            fields.push(format!(
                "path_entries={}",
                path.split(':').filter(|entry| !entry.is_empty()).count()
            ));
        }
        fields
    }

    /// Returns deterministic full-signature fields for hashing.
    fn canonical_hash_fields(&self) -> Vec<String> {
        let mut fields = self.known_fields();
        let mut managers = self.environment_managers.clone();
        managers.sort();
        fields.retain(|field| !field.starts_with("environment_manager="));
        for manager in managers {
            fields.push(format!("environment_manager={manager}"));
        }
        fields
    }
}

/// Returns a lowercase hex SHA-256 digest for stable model-visible IDs.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

/// Carries Tool Probe state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolProbe {
    /// Tool name requested by the bootstrap probe.
    pub name: String,
    /// Whether the lookup command found an executable.
    pub available: bool,
    /// Resolved executable path returned by the pane shell, when available.
    pub path: Option<String>,
    /// First line of version output, when the tool supports a version probe.
    pub version: Option<String>,
    /// Lookup command used for the probe.
    pub lookup_command: String,
    /// Exit status from the lookup command.
    pub lookup_exit_status: Option<i32>,
    /// Version command used after a successful lookup.
    pub version_command: Option<String>,
    /// Exit status from the version command.
    pub version_exit_status: Option<i32>,
    /// Unix timestamp reported by the pane shell for the discovery run.
    pub discovered_at_unix_seconds: Option<u64>,
}

/// Carries Tool Inventory state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInventory {
    /// Stores the sed value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub sed: bool,
    /// Stores the grep value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub grep: bool,
    /// Stores the python value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub python: bool,
    /// Stores the rg value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub rg: bool,
    /// Stores the modern tools value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub modern_tools: Vec<String>,
    /// Stores the tools value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub tools: BTreeMap<String, ToolProbe>,
}

impl ToolInventory {
    /// Runs the parse bootstrap output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn parse_bootstrap_output(output: &str) -> Self {
        let mut inventory = Self {
            sed: false,
            grep: false,
            python: false,
            rg: false,
            modern_tools: Vec::new(),
            tools: BTreeMap::new(),
        };

        for line in output.lines() {
            if let Some(probe) = tool_probe_from_structured_line(line) {
                inventory.record_tool_probe(probe);
                continue;
            }
            let Some((name, present)) = line.split_once('=') else {
                continue;
            };
            let present = present.trim() == "1";
            inventory.record_legacy_probe(name.trim(), present);
        }

        inventory.modern_tools.sort();
        inventory.modern_tools.dedup();
        inventory
    }

    /// Runs the record legacy probe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn record_legacy_probe(&mut self, name: &str, available: bool) {
        self.record_tool_probe(ToolProbe {
            name: name.to_string(),
            available,
            path: None,
            version: None,
            lookup_command: format!("command -v {name}"),
            lookup_exit_status: Some(if available { 0 } else { 1 }),
            version_command: None,
            version_exit_status: None,
            discovered_at_unix_seconds: None,
        });
    }

    /// Runs the record tool probe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn record_tool_probe(&mut self, probe: ToolProbe) {
        match probe.name.as_str() {
            "sed" => self.sed = probe.available,
            "grep" => self.grep = probe.available,
            "python" => self.python = probe.available,
            "rg" => self.rg = probe.available,
            tool if probe.available => self.modern_tools.push(tool.to_string()),
            _ => {}
        }
        self.tools.insert(probe.name.clone(), probe);
    }
}

/// Runs the tool probe from structured line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn tool_probe_from_structured_line(line: &str) -> Option<ToolProbe> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 10 || fields[0] != "tool" {
        return None;
    }

    let name = fields[1].trim();
    if name.is_empty() {
        return None;
    }
    Some(ToolProbe {
        name: name.to_string(),
        available: fields[2] == "1",
        path: optional_tool_field(fields[3]),
        version: optional_tool_field(fields[4]),
        lookup_command: fields[5].to_string(),
        lookup_exit_status: optional_i32_field(fields[6]),
        version_command: optional_tool_field(fields[7]),
        version_exit_status: optional_i32_field(fields[8]),
        discovered_at_unix_seconds: optional_u64_field(fields[9]),
    })
}

/// Runs the optional tool field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_tool_field(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Runs the optional i32 field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_i32_field(value: &str) -> Option<i32> {
    (!value.is_empty())
        .then(|| value.parse::<i32>().ok())
        .flatten()
}

/// Runs the optional u64 field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_u64_field(value: &str) -> Option<u64> {
    (!value.is_empty())
        .then(|| value.parse::<u64>().ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::{optional_tool_field, tool_probe_from_structured_line};

    /// Verifies whitespace-only optional tool metadata normalizes to `None`
    /// so the discovery cache does not preserve meaningless placeholder text.
    #[test]
    fn optional_tool_field_rejects_whitespace_only_values() {
        assert_eq!(optional_tool_field("   \t  "), None);
    }

    /// Verifies structured tool probe parsing trims blank optional fields to
    /// `None` while preserving the required probe metadata.
    #[test]
    fn tool_probe_from_structured_line_normalizes_blank_optional_fields() {
        let probe = tool_probe_from_structured_line(
            "tool\trg\t0\t \t \tcommand -v rg\t127\t \t\t1710000000",
        )
        .expect("tool probe line should parse");

        assert_eq!(probe.name, "rg");
        assert!(!probe.available);
        assert_eq!(probe.path, None);
        assert_eq!(probe.version, None);
        assert_eq!(probe.lookup_command, "command -v rg");
        assert_eq!(probe.lookup_exit_status, Some(127));
        assert_eq!(probe.version_command, None);
        assert_eq!(probe.version_exit_status, None);
        assert_eq!(probe.discovered_at_unix_seconds, Some(1710000000));
    }
}

/// Carries Tool Discovery Cache state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default)]
pub struct ToolDiscoveryCache {
    /// Stores the inventories value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) inventories: BTreeMap<EnvironmentSignature, ToolInventory>,
}

impl ToolDiscoveryCache {
    /// Runs the requires bootstrap operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn requires_bootstrap(&self, signature: &EnvironmentSignature) -> bool {
        signature.is_unknown() || !self.inventories.contains_key(signature)
    }

    /// Runs the record operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn record(&mut self, signature: EnvironmentSignature, inventory: ToolInventory) {
        if signature.is_unknown() {
            return;
        }
        self.inventories.insert(signature, inventory);
    }

    /// Runs the get operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn get(&self, signature: &EnvironmentSignature) -> Option<&ToolInventory> {
        self.inventories.get(signature)
    }
}
