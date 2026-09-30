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

impl SandboxLifecycleFailure {
    /// Permits bounded advice only for a complete, valid missing-exit report.
    /// This is not execution authority: unknown effects forbid automatic replay.
    pub(crate) fn permits_model_guidance(&self) -> bool {
        self.class == SandboxLifecycleFailureClass::MissingExit
            && self.child_record_present.is_some()
            && self.exit_record_present == Some(false)
            && self.outer_exit_code.is_some()
            && self.outer_signal.is_none()
            && !self.stderr_truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Advice requires valid closed missing-exit status and bounded diagnostics;
    /// unknown facts, transport faults, signals and truncation remain fail-closed.
    #[test]
    fn lifecycle_guidance_is_not_authorized_by_insufficient_evidence() {
        let valid = SandboxLifecycleFailure {
            backend: "bubblewrap".to_string(),
            class: SandboxLifecycleFailureClass::MissingExit,
            outer_exit_code: Some(7),
            outer_signal: None,
            child_record_present: Some(true),
            exit_record_present: Some(false),
            stderr: "diagnostic".to_string(),
            stderr_truncated: false,
        };
        assert!(valid.permits_model_guidance());
        for class in [
            SandboxLifecycleFailureClass::Malformed,
            SandboxLifecycleFailureClass::Transport,
            SandboxLifecycleFailureClass::Truncated,
            SandboxLifecycleFailureClass::InvalidUtf8,
            SandboxLifecycleFailureClass::ContradictoryExit,
        ] {
            assert!(
                !SandboxLifecycleFailure {
                    class,
                    ..valid.clone()
                }
                .permits_model_guidance()
            );
        }
        assert!(
            !SandboxLifecycleFailure {
                child_record_present: None,
                ..valid.clone()
            }
            .permits_model_guidance()
        );
        assert!(
            !SandboxLifecycleFailure {
                outer_signal: Some(15),
                ..valid.clone()
            }
            .permits_model_guidance()
        );
        assert!(
            !SandboxLifecycleFailure {
                stderr_truncated: true,
                ..valid
            }
            .permits_model_guidance()
        );
    }
}
