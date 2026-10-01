//! Retirement of shell transactions and their exact process input leases.
//!
//! Marker state remains in the existing runtime service. Retirement releases
//! sandbox activity and process-generation leases before removing the marker;
//! shared render-pending state survives while another transaction owns it.

use super::*;

impl RuntimeSessionService {
    /// Registers one live editor handoff for the current process epoch.
    pub(super) fn register_managed_shell_handoff(
        &mut self,
        marker: &str,
        shell: ManagedShellKind,
        parent_proof: Option<String>,
    ) {
        let Some(pane_id) = self
            .process
            .running_shell_transactions
            .get(marker)
            .map(|transaction| transaction.pane_id.clone())
        else {
            return;
        };
        let handoff = self.process.pane_shell_handoffs.get(&pane_id);
        let primary_process_id = handoff
            .map(|handoff| handoff.primary_process_id)
            .or_else(|| self.primary_pid_for_live_pane_process(&pane_id));
        let interaction_generation = handoff
            .map(|handoff| handoff.interaction_generation)
            .or_else(|| {
                self.process
                    .pane_shell_interaction_generations
                    .get(&pane_id)
                    .copied()
            });
        let identity = ManagedShellHandoffIdentity {
            marker: marker.to_string(),
            process_instance: self.adapter_owned_pane_process_instance(&pane_id),
            primary_process_id,
            interaction_generation,
            parent_proof,
        };
        self.process
            .pane_managed_shell_handoffs
            .insert(pane_id, ManagedShellHandoff::new(shell, identity));
    }

    /// Removes one live shell transaction by marker.
    pub(crate) fn remove_running_shell_transaction(
        &mut self,
        marker: &str,
    ) -> Option<RunningShellTransactionRef> {
        let pane_id = self
            .process
            .running_shell_transactions
            .get(marker)
            .map(|transaction| transaction.pane_id.clone());
        self.process.managed_home_activity_locks.remove(marker);
        self.process.seatbelt_workload_leases.remove(marker);
        self.release_shell_transaction_input_lease(marker);
        self.process
            .shell_transaction_encoded_output_markers
            .remove(marker);
        if let Some(pane_id) = pane_id
            && !self
                .process
                .shell_transaction_encoded_output_markers
                .iter()
                .any(|owner| {
                    self.process
                        .running_shell_transactions
                        .get(owner)
                        .is_some_and(|transaction| transaction.pane_id == pane_id)
                })
        {
            self.process
                .pane_shell_output_render_pending
                .remove(&pane_id);
        }
        self.process
            .shell_transaction_wrapper_filter_commands
            .remove(marker);
        self.process
            .shell_transaction_receiver_acknowledgements
            .remove(marker);
        self.process
            .shell_transaction_output_utf8_pending
            .remove(marker);
        self.process
            .shell_transaction_start_boundary_pending
            .remove(marker);
        self.process
            .shell_transaction_end_boundary_pending
            .remove(marker);
        self.process
            .shell_transaction_control_osc_pending
            .remove(marker);
        self.process.running_shell_transactions.remove(marker)
    }

    /// Releases the exact pane-process input lease owned by one transaction.
    fn release_shell_transaction_input_lease(&mut self, marker: &str) {
        let Some(instance) = self.process.shell_transaction_input_leases.remove(marker) else {
            return;
        };
        self.persistence
            .queue_pane_input(RuntimeSideEffect::PaneProcessIo {
                instance,
                effect: PaneProcessIoEffect::ReleaseShellInputLease {
                    owner_id: marker.to_string(),
                },
            });
    }

    /// Clears all live shell transactions and marker protocol state.
    pub(crate) fn clear_all_shell_transaction_state(&mut self) {
        let markers = self
            .process
            .running_shell_transactions
            .keys()
            .chain(self.process.shell_transaction_input_leases.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        for marker in markers {
            self.remove_running_shell_transaction(&marker);
            self.release_shell_transaction_input_lease(&marker);
        }
        self.process
            .shell_transaction_wrapper_filter_commands
            .clear();
        self.process.shell_transaction_require_start_markers.clear();
        self.process.shell_transaction_started_markers.clear();
        self.process
            .shell_transaction_start_boundary_pending
            .clear();
        self.process.shell_transaction_end_boundary_pending.clear();
        self.process.shell_transaction_control_osc_pending.clear();
        self.process.shell_transaction_output_utf8_pending.clear();
        self.process
            .shell_transaction_receiver_acknowledgements
            .clear();
        self.process.shell_receiver_pending_payloads.clear();
        self.process.shell_receiver_completion_required.clear();
        self.process.shell_receiver_pending_ends.clear();
        self.process
            .pending_deferred_foreign_transaction_ends
            .clear();
        self.process
            .shell_transaction_encoded_output_markers
            .clear();
        self.process.pane_shell_output_render_pending.clear();
        self.process.managed_home_activity_locks.clear();
        self.process.seatbelt_workload_leases.clear();
        self.process.sandboxed_shell_transaction_plans.clear();
    }
}
