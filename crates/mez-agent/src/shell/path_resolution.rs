//! Pane-shell canonical path-resolution protocol.
//!
//! This module renders a bounded, read-only command that resolves configured
//! authority paths inside the pane environment and parses its encoded result.
//! Paths are transported as base64-encoded JSON rather than interpolated into
//! shell source. The product runtime remains responsible for dispatching the
//! command, bounding its lifetime, caching results, and treating failures as
//! unresolved authority.

use super::{
    AgentShellValidationError, AgentShellValidationResult, ShellClassification, fish_quote,
    shell_quote,
};
use crate::permissions::{PathScopes, ResolvedPathEvidence, ResolvedPathKind};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const PATH_RESOLUTION_PROTOCOL_MARKER: &str = "MEZ_PATH_RESOLUTION_V1\t";
const MAX_PATH_RESOLUTION_REQUESTS: usize = 4096;
const MAX_PATH_RESOLUTION_REQUEST_BYTES: usize = 1024 * 1024;

const PATH_RESOLUTION_PYTHON: &str = r#"import base64,json,os,sys
payload=json.loads(base64.b64decode(sys.argv[1],validate=True))
cwd=os.path.realpath(os.getcwd())
entries=[]
for requested in payload["paths"]:
    if not requested or "\x00" in requested or requested.startswith("~"):
        raise ValueError("invalid requested path")
    target=requested if os.path.isabs(requested) else os.path.join(cwd,requested)
    target=os.path.abspath(target)
    probe=target
    while not os.path.lexists(probe):
        parent=os.path.dirname(probe)
        if parent==probe:
            raise ValueError("path has no existing parent")
        probe=parent
    nearest=os.path.realpath(probe)
    if os.path.lexists(target):
        canonical=os.path.realpath(target)
        kind="existing"
        nearest=canonical
    else:
        relative=os.path.relpath(target,probe)
        suffix=relative.split(os.sep)
        if any(part in ("",".","..") for part in suffix):
            raise ValueError("ambiguous create target")
        canonical=os.path.normpath(os.path.join(nearest,*suffix))
        kind="create-target"
    entries.append({"requested":requested,"canonical_path":canonical,"kind":kind,"nearest_existing_parent":nearest})
result={"version":1,"current_directory":cwd,"entries":entries}
encoded=base64.b64encode(json.dumps(result,separators=(",",":"),ensure_ascii=False).encode()).decode()
print("MEZ_PATH_RESOLUTION_V1\t"+encoded)
"#;

/// Paths that one pane-shell resolution transaction must canonicalize.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PanePathResolutionRequest {
    /// Configured maximum read scopes, before pane-shell resolution.
    pub read_scopes: Vec<String>,
    /// Configured maximum write scopes, before pane-shell resolution.
    pub write_scopes: Vec<String>,
    /// Additional command, rule-effect, or executable paths to resolve.
    pub additional_paths: Vec<String>,
}

impl PanePathResolutionRequest {
    /// Validates and normalizes one bounded path-resolution request.
    pub fn new(
        read_scopes: Vec<String>,
        write_scopes: Vec<String>,
        additional_paths: Vec<String>,
    ) -> AgentShellValidationResult<Self> {
        let mut total_bytes = 0usize;
        for path in read_scopes
            .iter()
            .chain(&write_scopes)
            .chain(&additional_paths)
        {
            if path.is_empty() || path.contains('\0') || path.starts_with('~') {
                return Err(AgentShellValidationError::invalid_args(
                    "path-resolution requests must be non-empty, unexpanded paths without NUL bytes",
                ));
            }
            total_bytes = total_bytes.saturating_add(path.len());
        }
        let path_count = read_scopes
            .len()
            .saturating_add(write_scopes.len())
            .saturating_add(additional_paths.len());
        if path_count > MAX_PATH_RESOLUTION_REQUESTS
            || total_bytes > MAX_PATH_RESOLUTION_REQUEST_BYTES
        {
            return Err(AgentShellValidationError::invalid_args(
                "path-resolution request exceeds the bounded path count or byte limit",
            ));
        }
        Ok(Self {
            read_scopes: stable_unique(read_scopes),
            write_scopes: stable_unique(write_scopes),
            additional_paths: stable_unique(additional_paths),
        })
    }

    /// Returns every distinct requested path in deterministic order.
    fn all_paths(&self) -> Vec<String> {
        let mut paths = self
            .read_scopes
            .iter()
            .chain(&self.write_scopes)
            .chain(&self.additional_paths)
            .cloned()
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }
}

/// Canonical path evidence returned by a pane-shell resolution transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanePathResolutionResult {
    /// Canonical physical working directory observed by the pane process.
    pub current_directory: String,
    /// Evidence for every requested authority or command path.
    pub path_evidence: BTreeMap<String, ResolvedPathEvidence>,
}

impl PanePathResolutionResult {
    /// Converts the result into validated read/write authority for its request.
    pub fn into_path_scopes(
        self,
        request: &PanePathResolutionRequest,
    ) -> crate::permissions::PermissionResult<PathScopes> {
        let read_scopes = resolved_requested_paths(&request.read_scopes, &self.path_evidence)?;
        let write_scopes = resolved_requested_paths(&request.write_scopes, &self.path_evidence)?;
        PathScopes::try_shell_resolved_with_evidence(
            self.current_directory,
            read_scopes,
            write_scopes,
            self.path_evidence,
        )
    }
}

/// Renders a read-only resolver command for the active pane shell family.
pub fn pane_path_resolution_command(
    request: &PanePathResolutionRequest,
    classification: ShellClassification,
) -> AgentShellValidationResult<String> {
    let payload = PathResolutionRequestWire {
        version: 1,
        paths: request.all_paths(),
    };
    let payload = serde_json::to_vec(&payload).map_err(|error| {
        AgentShellValidationError::invalid_args(format!(
            "path-resolution request could not be encoded: {error}"
        ))
    })?;
    let payload = base64::engine::general_purpose::STANDARD.encode(payload);
    if classification == ShellClassification::Fish {
        Ok(format!(
            "set -l MEZ_PATH_PYTHON (command -s python3 2>/dev/null; or command -s python 2>/dev/null)\nif test -z \"$MEZ_PATH_PYTHON\"\n    printf '%s\\n' 'python3 or python is required for Mezzanine path resolution' >&2\n    exit 127\nend\ncommand $MEZ_PATH_PYTHON -c {} {}\n",
            fish_quote(PATH_RESOLUTION_PYTHON),
            fish_quote(&payload),
        ))
    } else {
        Ok(format!(
            "MEZ_PATH_PYTHON=$(command -v python3 2>/dev/null || command -v python 2>/dev/null) || exit 127\nif [ -z \"$MEZ_PATH_PYTHON\" ]; then printf '%s\\n' 'python3 or python is required for Mezzanine path resolution' >&2; exit 127; fi\n\"$MEZ_PATH_PYTHON\" -c {} {}\n",
            shell_quote(PATH_RESOLUTION_PYTHON),
            shell_quote(&payload),
        ))
    }
}

/// Parses and validates one encoded pane-shell path-resolution result.
pub fn parse_pane_path_resolution_output(
    output: &str,
    request: &PanePathResolutionRequest,
) -> AgentShellValidationResult<PanePathResolutionResult> {
    let encoded = output
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(PATH_RESOLUTION_PROTOCOL_MARKER))
        .ok_or_else(|| {
            AgentShellValidationError::invalid_args(
                "path-resolution output did not contain the expected protocol record",
            )
        })?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| {
            AgentShellValidationError::invalid_args(
                "path-resolution output contained invalid base64",
            )
        })?;
    let result: PathResolutionResultWire = serde_json::from_slice(&bytes).map_err(|error| {
        AgentShellValidationError::invalid_args(format!(
            "path-resolution output contained invalid JSON: {error}"
        ))
    })?;
    if result.version != 1 {
        return Err(AgentShellValidationError::invalid_args(
            "path-resolution output used an unsupported protocol version",
        ));
    }

    let expected = request.all_paths().into_iter().collect::<BTreeSet<_>>();
    let mut path_evidence = BTreeMap::new();
    for entry in result.entries {
        if !expected.contains(&entry.requested) || path_evidence.contains_key(&entry.requested) {
            return Err(AgentShellValidationError::invalid_args(
                "path-resolution output contained an unexpected or duplicate path",
            ));
        }
        let kind = match entry.kind.as_str() {
            "existing" => ResolvedPathKind::Existing,
            "create-target" => ResolvedPathKind::CreateTarget,
            _ => {
                return Err(AgentShellValidationError::invalid_args(
                    "path-resolution output contained an unknown path kind",
                ));
            }
        };
        path_evidence.insert(
            entry.requested,
            ResolvedPathEvidence {
                canonical_path: entry.canonical_path,
                kind,
                nearest_existing_parent: entry.nearest_existing_parent,
            },
        );
    }
    if path_evidence.keys().cloned().collect::<BTreeSet<_>>() != expected {
        return Err(AgentShellValidationError::invalid_args(
            "path-resolution output omitted one or more requested paths",
        ));
    }
    Ok(PanePathResolutionResult {
        current_directory: result.current_directory,
        path_evidence,
    })
}

fn stable_unique(paths: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

fn resolved_requested_paths(
    requested: &[String],
    evidence: &BTreeMap<String, ResolvedPathEvidence>,
) -> crate::permissions::PermissionResult<Vec<String>> {
    requested
        .iter()
        .map(|path| {
            evidence
                .get(path)
                .map(|entry| entry.canonical_path.clone())
                .ok_or_else(|| {
                    crate::permissions::PermissionError::invalid_args(format!(
                        "path-resolution result omitted requested scope `{path}`"
                    ))
                })
        })
        .collect()
}

#[derive(Serialize)]
struct PathResolutionRequestWire {
    version: u8,
    paths: Vec<String>,
}

#[derive(Deserialize)]
struct PathResolutionResultWire {
    version: u8,
    current_directory: String,
    entries: Vec<PathResolutionEntryWire>,
}

#[derive(Deserialize)]
struct PathResolutionEntryWire {
    requested: String,
    canonical_path: String,
    kind: String,
    nearest_existing_parent: String,
}
