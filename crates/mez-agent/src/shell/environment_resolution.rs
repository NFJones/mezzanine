//! Pane-local environment-variable evidence protocol.
//!
//! Requests contain only validated portable names. The active pane process
//! reads exported values and returns one bounded base64 JSON record. Callers
//! must keep successful values out of logs, status, diagnostics, and context.

use super::{
    AgentShellValidationError, AgentShellValidationResult, ShellClassification, fish_quote,
    shell_quote,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet};

const ENVIRONMENT_EVIDENCE_MARKER: &str = "MEZ_ENVIRONMENT_EVIDENCE_V1\t";
pub const MAX_ENVIRONMENT_VARIABLES: usize = 128;
pub const MAX_ENVIRONMENT_NAME_BYTES: usize = 16 * 1024;
pub const MAX_ENVIRONMENT_VALUE_BYTES: usize = 16 * 1024;
pub const MAX_ENVIRONMENT_TOTAL_VALUE_BYTES: usize = 64 * 1024;

const ENVIRONMENT_EVIDENCE_PYTHON: &str = r#"import base64,json,os,sys
payload=json.loads(base64.b64decode(sys.argv[1],validate=True))
entries=[]
total=0
for name in payload["names"]:
    if name not in os.environ:
        entries.append({"name":name,"status":"unset"})
        continue
    try:
        raw=os.environ[name].encode("utf-8","strict")
    except UnicodeEncodeError:
        entries.append({"name":name,"status":"omitted","reason":"non_text"})
        continue
    if len(raw)>payload["max_value_bytes"]:
        entries.append({"name":name,"status":"omitted","reason":"oversized"})
        continue
    if total+len(raw)>payload["max_total_value_bytes"]:
        entries.append({"name":name,"status":"omitted","reason":"aggregate_limit"})
        continue
    total+=len(raw)
    entries.append({"name":name,"status":"present","value":base64.b64encode(raw).decode("ascii")})
result={"version":1,"entries":entries}
encoded=base64.b64encode(json.dumps(result,separators=(",",":"),ensure_ascii=True).encode("ascii")).decode("ascii")
print("MEZ_ENVIRONMENT_EVIDENCE_V1\t"+encoded)
"#;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct PaneEnvironmentRequest {
    pub names: Vec<String>,
}

impl PaneEnvironmentRequest {
    pub fn new(names: Vec<String>) -> AgentShellValidationResult<Self> {
        if names.len() > MAX_ENVIRONMENT_VARIABLES {
            return Err(AgentShellValidationError::invalid_args(
                "environment evidence request exceeds the variable count limit",
            ));
        }
        let mut bytes = 0usize;
        let mut seen = BTreeSet::new();
        for name in &names {
            bytes = bytes.saturating_add(name.len());
            if !portable_environment_name(name) {
                return Err(AgentShellValidationError::invalid_args(
                    "environment evidence names must match [A-Za-z_][A-Za-z0-9_]*",
                ));
            }
            if !seen.insert(name.clone()) {
                return Err(AgentShellValidationError::invalid_args(
                    "environment evidence request contains duplicate names",
                ));
            }
        }
        if bytes > MAX_ENVIRONMENT_NAME_BYTES {
            return Err(AgentShellValidationError::invalid_args(
                "environment evidence request exceeds the name byte limit",
            ));
        }
        Ok(Self { names })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PaneEnvironmentEvidence {
    pub values: BTreeMap<String, String>,
    pub omitted: BTreeMap<String, String>,
    pub value_sha256: String,
}

impl std::fmt::Debug for PaneEnvironmentEvidence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneEnvironmentEvidence")
            .field("effective_names", &self.values.keys().collect::<Vec<_>>())
            .field("omitted", &self.omitted)
            .field("value_sha256", &self.value_sha256)
            .finish()
    }
}

impl PaneEnvironmentEvidence {
    /// Builds validated evidence from protected values and redacted omissions.
    pub fn from_parts(
        request: &PaneEnvironmentRequest,
        values: BTreeMap<String, String>,
        omitted: BTreeMap<String, String>,
    ) -> AgentShellValidationResult<Self> {
        let expected = request.names.iter().cloned().collect::<BTreeSet<_>>();
        let observed = values
            .keys()
            .chain(omitted.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        if observed != expected || values.keys().any(|name| omitted.contains_key(name)) {
            return Err(AgentShellValidationError::invalid_args(
                "environment evidence parts must cover every requested name exactly once",
            ));
        }
        let mut total = 0usize;
        for value in values.values() {
            total = total.saturating_add(value.len());
            if value.len() > MAX_ENVIRONMENT_VALUE_BYTES
                || total > MAX_ENVIRONMENT_TOTAL_VALUE_BYTES
                || value.as_bytes().contains(&0)
                || value.bytes().any(|byte| byte.is_ascii_control())
            {
                return Err(AgentShellValidationError::invalid_args(
                    "environment evidence contains an unsafe or oversized value",
                ));
            }
        }
        let value_sha256 = evidence_digest(request, &values, &omitted);
        Ok(Self {
            values,
            omitted,
            value_sha256,
        })
    }

    /// Builds a restrictive result that omits every requested mapping.
    pub fn restrictive(request: &PaneEnvironmentRequest, reason: &str) -> Self {
        let omitted = request
            .names
            .iter()
            .cloned()
            .map(|name| (name, reason.to_string()))
            .collect::<BTreeMap<_, _>>();
        Self::from_parts(request, BTreeMap::new(), omitted)
            .expect("validated environment request must produce restrictive evidence")
    }
}

pub fn pane_environment_evidence_command(
    request: &PaneEnvironmentRequest,
    classification: ShellClassification,
) -> AgentShellValidationResult<String> {
    let wire = EnvironmentRequestWire {
        version: 1,
        names: request.names.clone(),
        max_value_bytes: MAX_ENVIRONMENT_VALUE_BYTES,
        max_total_value_bytes: MAX_ENVIRONMENT_TOTAL_VALUE_BYTES,
    };
    let payload = serde_json::to_vec(&wire).map_err(|error| {
        AgentShellValidationError::invalid_args(format!(
            "environment evidence request could not be encoded: {error}"
        ))
    })?;
    let payload = base64::engine::general_purpose::STANDARD.encode(payload);
    if classification == ShellClassification::Fish {
        Ok(format!(
            "set -l MEZ_ENV_PYTHON\nif test -x /usr/bin/python3\n    set MEZ_ENV_PYTHON /usr/bin/python3\nelse if test -x /bin/python3\n    set MEZ_ENV_PYTHON /bin/python3\nelse\n    exit 127\nend\ncommand $MEZ_ENV_PYTHON -I -S -c {} {}\n",
            fish_quote(ENVIRONMENT_EVIDENCE_PYTHON),
            fish_quote(&payload)
        ))
    } else {
        Ok(format!(
            "if [ -x /usr/bin/python3 ]; then MEZ_ENV_PYTHON=/usr/bin/python3; elif [ -x /bin/python3 ]; then MEZ_ENV_PYTHON=/bin/python3; else exit 127; fi\n\"$MEZ_ENV_PYTHON\" -I -S -c {} {}\n",
            shell_quote(ENVIRONMENT_EVIDENCE_PYTHON),
            shell_quote(&payload)
        ))
    }
}

pub fn parse_pane_environment_evidence(
    output: &str,
    request: &PaneEnvironmentRequest,
) -> AgentShellValidationResult<PaneEnvironmentEvidence> {
    let encoded = output
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(ENVIRONMENT_EVIDENCE_MARKER))
        .ok_or_else(|| {
            AgentShellValidationError::invalid_args(
                "environment evidence output did not contain the expected protocol record",
            )
        })?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| {
            AgentShellValidationError::invalid_args(
                "environment evidence output contained invalid base64",
            )
        })?;
    let result: EnvironmentResultWire = serde_json::from_slice(&bytes).map_err(|_| {
        AgentShellValidationError::invalid_args(
            "environment evidence output contained invalid JSON",
        )
    })?;
    if result.version != 1 {
        return Err(AgentShellValidationError::invalid_args(
            "environment evidence output used an unsupported protocol version",
        ));
    }
    let expected = request.names.iter().cloned().collect::<BTreeSet<_>>();
    let mut observed = BTreeSet::new();
    let mut values = BTreeMap::new();
    let mut omitted = BTreeMap::new();
    let mut total = 0usize;
    for entry in result.entries {
        if !expected.contains(&entry.name) || !observed.insert(entry.name.clone()) {
            return Err(AgentShellValidationError::invalid_args(
                "environment evidence output contained an unexpected or duplicate name",
            ));
        }
        match entry.status.as_str() {
            "unset" => {
                omitted.insert(entry.name, "unset".to_string());
            }
            "omitted" => {
                omitted.insert(entry.name, safe_reason(entry.reason.as_deref()));
            }
            "present" => {
                let decoded = entry
                    .value
                    .as_deref()
                    .and_then(|value| base64::engine::general_purpose::STANDARD.decode(value).ok());
                let Some(decoded) = decoded else {
                    omitted.insert(entry.name, "protocol_invalid".to_string());
                    continue;
                };
                if decoded.len() > MAX_ENVIRONMENT_VALUE_BYTES
                    || total.saturating_add(decoded.len()) > MAX_ENVIRONMENT_TOTAL_VALUE_BYTES
                {
                    omitted.insert(entry.name, "oversized".to_string());
                    continue;
                }
                let Ok(value) = String::from_utf8(decoded) else {
                    omitted.insert(entry.name, "non_text".to_string());
                    continue;
                };
                if value.as_bytes().contains(&0)
                    || value.bytes().any(|byte| byte.is_ascii_control())
                {
                    omitted.insert(entry.name, "unsafe_control".to_string());
                    continue;
                }
                total = total.saturating_add(value.len());
                values.insert(entry.name, value);
            }
            _ => {
                omitted.insert(entry.name, "protocol_invalid".to_string());
            }
        }
    }
    for name in expected.difference(&observed) {
        omitted.insert(name.clone(), "missing_record".to_string());
    }
    PaneEnvironmentEvidence::from_parts(request, values, omitted)
}

fn evidence_digest(
    request: &PaneEnvironmentRequest,
    values: &BTreeMap<String, String>,
    omitted: &BTreeMap<String, String>,
) -> String {
    let mut digest = sha2::Sha256::new();
    digest.update(b"mez-pane-environment-evidence-v1\0");
    for name in &request.names {
        digest.update((name.len() as u64).to_be_bytes());
        digest.update(name.as_bytes());
        if let Some(value) = values.get(name) {
            digest.update(b"present\0");
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value.as_bytes());
        } else if let Some(reason) = omitted.get(name) {
            digest.update(b"omitted\0");
            digest.update((reason.len() as u64).to_be_bytes());
            digest.update(reason.as_bytes());
        }
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn safe_reason(reason: Option<&str>) -> String {
    match reason {
        Some("non_text") => "non_text",
        Some("oversized") => "oversized",
        Some("aggregate_limit") => "aggregate_limit",
        _ => "protocol_invalid",
    }
    .to_string()
}

fn portable_environment_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && (bytes[0] == b"_"[0] || bytes[0].is_ascii_alphabetic())
        && bytes[1..]
            .iter()
            .all(|byte| *byte == b"_"[0] || byte.is_ascii_alphanumeric())
}

#[derive(Serialize)]
struct EnvironmentRequestWire {
    version: u8,
    names: Vec<String>,
    max_value_bytes: usize,
    max_total_value_bytes: usize,
}

#[derive(Deserialize)]
struct EnvironmentResultWire {
    version: u8,
    entries: Vec<EnvironmentEntryWire>,
}

#[derive(Deserialize)]
struct EnvironmentEntryWire {
    name: String,
    status: String,
    value: Option<String>,
    reason: Option<String>,
}
