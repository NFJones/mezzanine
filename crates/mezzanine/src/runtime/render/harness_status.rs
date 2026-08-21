//! Source-isolated pane status state for external harness integrations.

use super::RuntimePresentationComponent;

/// One harness-owned pane status projected into the pane frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimePaneHarnessStatus {
    /// Semantic state used for status coloring and animation.
    pub(crate) state: String,
    /// Optional bounded display text supplied by the harness.
    pub(crate) text: Option<String>,
}

/// Stored status with update order used to select the visible source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimePaneHarnessStatusEntry {
    pub(super) status: RuntimePaneHarnessStatus,
    pub(super) sequence: u64,
}

impl RuntimePresentationComponent {
    /// Sets or clears one source-owned status without affecting other sources.
    pub(crate) fn set_pane_harness_status(
        &mut self,
        pane_id: &str,
        source: &str,
        status: Option<RuntimePaneHarnessStatus>,
    ) {
        if let Some(status) = status {
            let sequence = self.next_pane_harness_status_sequence;
            self.next_pane_harness_status_sequence = sequence.saturating_add(1);
            self.pane_harness_statuses
                .entry(pane_id.to_string())
                .or_default()
                .insert(
                    source.to_string(),
                    RuntimePaneHarnessStatusEntry { status, sequence },
                );
            return;
        }
        if let Some(statuses) = self.pane_harness_statuses.get_mut(pane_id) {
            statuses.remove(source);
            if statuses.is_empty() {
                self.pane_harness_statuses.remove(pane_id);
            }
        }
    }

    /// Returns the most recently updated source-owned status for one pane.
    pub(crate) fn pane_harness_status(&self, pane_id: &str) -> Option<&RuntimePaneHarnessStatus> {
        self.pane_harness_statuses
            .get(pane_id)?
            .values()
            .max_by_key(|entry| entry.sequence)
            .map(|entry| &entry.status)
    }
}
