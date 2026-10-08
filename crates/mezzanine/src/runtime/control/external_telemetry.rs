//! Registration-owned observational presentation, independent of hook connections.
//!
//! Capability validation stays in the external lifecycle ingress. This reducer
//! accepts only exact-session, monotonic, bounded observations; it cannot focus,
//! inject input, approve, or continue work. Titles are temporary projections and
//! never pin the mux title. Telemetry expiry is not evidence of process death.

use super::{MezError, Result, RuntimeSessionService};

/// Latest bounded observation and its registration-local sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExternalPresentation {
    /// Positive sequence chosen by the bound normalized event source.
    pub(super) sequence: u64,
    /// Coarse reported state, not native execution state or process liveness.
    pub(super) state: String,
    /// Optional inert automatic title; null clears only this source's suggestion.
    pub(super) title: Option<String>,
}

impl RuntimeSessionService {
    /// Settles one already-authenticated registration observation without replay.
    pub(super) fn update_external_presentation(
        &mut self,
        digest: [u8; 32],
        params: &serde_json::Value,
    ) -> Result<String> {
        let session = super::external_agents::text(params, "external_session_id", 128)?;
        let sequence = params
            .get("sequence")
            .and_then(serde_json::Value::as_u64)
            .filter(|n| *n > 0)
            .ok_or_else(|| MezError::invalid_args("presentation sequence must be positive"))?;
        let state = super::external_agents::text(params, "state", 32)?;
        if !matches!(
            state.as_str(),
            "ready"
                | "running"
                | "approval-wait"
                | "input-wait"
                | "complete"
                | "interrupted"
                | "failed"
                | "background"
        ) {
            return Err(MezError::invalid_args(
                "unsupported external presentation state",
            ));
        }
        let title = match params.get("title") {
            None | Some(serde_json::Value::Null) => None,
            _ => Some(super::external_agents::text(params, "title", 128)?),
        };
        let candidate = ExternalPresentation {
            sequence,
            state,
            title,
        };
        let binding = self
            .control
            .external_agents_mut()
            .bindings
            .get_mut(&digest)
            .ok_or_else(|| MezError::forbidden("external registration unavailable"))?;
        let registration = binding
            .registration
            .as_mut()
            .filter(|registration| registration.external_session_id == session)
            .ok_or_else(|| MezError::forbidden("external registration unavailable"))?;
        if let Some(previous) = &registration.presentation {
            if candidate == *previous {
                return Ok(serde_json::json!({"changed":false,"sequence":sequence}).to_string());
            }
            if sequence <= previous.sequence {
                return Err(MezError::conflict(
                    "external presentation observation is stale or conflicting",
                ));
            }
        }
        let pane = binding.pane_id.clone();
        let owner = format!("external-registration:{}", binding.generation);
        let color_state = match candidate.state.as_str() {
            "approval-wait" => "blocked",
            "input-wait" => "waiting",
            "interrupted" => "failed",
            "background" => "running",
            other => other,
        }
        .to_string();
        let text = candidate.state.clone();
        registration.presentation = Some(candidate);
        self.presentation.set_pane_harness_status(
            &pane,
            &owner,
            Some(crate::runtime::RuntimePaneHarnessStatus {
                state: color_state,
                text: Some(text),
            }),
        );
        Ok(serde_json::json!({"changed":true,"sequence":sequence}).to_string())
    }

    /// Returns the latest live launch's title without native metadata reads or
    /// changing provenance; ordinary ancestry uses bounded nonblocking polls.
    pub(crate) fn external_agent_pane_title(&self, pane: &str) -> Option<String> {
        let now = crate::runtime::current_unix_seconds();
        self.control
            .external_agents()
            .bindings
            .values()
            .filter(|binding| {
                binding.pane_id == pane
                    && binding.has_current_telemetry(now)
                    && self.pane_process_identity_is_current(pane, &binding.process)
            })
            .filter_map(|binding| {
                let observation = binding.registration.as_ref()?.presentation.as_ref()?;
                Some((binding.generation, observation.title.clone()?))
            })
            .max_by_key(|(generation, _)| *generation)
            .map(|(_, title)| title)
    }

    /// Gates registration-owned status at projection, even before idle cleanup.
    /// Ordinary client-owned status sources retain their existing ownership rules.
    pub(crate) fn live_pane_harness_status(
        &self,
        pane: &str,
    ) -> Option<&crate::runtime::RuntimePaneHarnessStatus> {
        self.presentation
            .pane_harness_status_filtered(pane, |source| {
                let Some(generation) = source.strip_prefix("external-registration:") else {
                    return true;
                };
                let Ok(generation) = generation.parse::<u64>() else {
                    return false;
                };
                let now = crate::runtime::current_unix_seconds();
                self.control
                    .external_agents()
                    .bindings
                    .values()
                    .any(|binding| {
                        binding.generation == generation
                            && binding.pane_id == pane
                            && binding.has_current_telemetry(now)
                            && binding.registration.is_some()
                            && self.pane_process_identity_is_current(pane, &binding.process)
                    })
            })
    }
}
