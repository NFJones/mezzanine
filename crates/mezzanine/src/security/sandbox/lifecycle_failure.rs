//! Bounded diagnostic evidence for incomplete native sandbox lifecycles.
//!
//! Only the private status descriptor establishes lifecycle facts. Stderr is
//! untrusted diagnostic text, never proof of execution or permission to retry.

/// Stable classification of missing or untrustworthy completion evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SandboxLifecycleFailureClass {
    /// The reader did not establish EOF or failed to transport status bytes.
    Transport,
    /// Status exceeded the bounded capture budget.
    Truncated,
    /// Status bytes were not UTF-8.
    InvalidUtf8,
    /// Status did not satisfy the backend's strict ordered grammar.
    Malformed,
    /// Valid status omitted the payload completion record.
    MissingExit,
    /// Trusted payload exit contradicted the outer process exit.
    ContradictoryExit,
}

/// Safe evidence retained when native sandbox completion cannot be proven.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct SandboxLifecycleFailure {
    /// Backend selected by the trusted launch plan.
    pub(crate) backend: String,
    /// Parse or transport classification, independent of workload output.
    pub(crate) class: SandboxLifecycleFailureClass,
    /// Outer process exit, not a substituted payload status.
    pub(crate) outer_exit_code: Option<i32>,
    /// Outer process signal, when observed.
    pub(crate) outer_signal: Option<i32>,
    /// Trusted child record presence; unknown when status is invalid.
    pub(crate) child_record_present: Option<bool>,
    /// Trusted exit record presence; unknown when status is invalid.
    pub(crate) exit_record_present: Option<bool>,
    /// Bounded and credential-redacted untrusted stderr; stdout is excluded.
    pub(crate) stderr: String,
    /// Whether diagnostic bytes were dropped or withheld by bounding.
    pub(crate) stderr_truncated: bool,
}
