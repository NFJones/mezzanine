//! Pane input delivery through the current PTY ownership boundary.
//!
//! Generated shell records retain transaction identity and platform pacing.
//! Deferred writes and cancellation target the exact current process generation;
//! queue priority does not bypass logical-record validation.

use super::*;

impl RuntimeSessionService {
    /// Writes pane input immediately when the synchronous manager still owns
    /// the pane, or records it for the pane I/O adapter when ownership has moved.
    pub(in crate::runtime) fn write_runtime_pane_input(
        &mut self,
        pane_id: &str,
        input: &[u8],
    ) -> Result<()> {
        self.write_runtime_pane_input_with_priority(pane_id, input, false)
    }

    /// Writes generated interactive shell source using platform-native pacing.
    /// Darwin adapter-owned panes wait for fresh output between bounded records;
    /// Linux and synchronously owned panes retain the ordinary write path.
    pub(in crate::runtime) fn write_runtime_pane_shell_input(
        &mut self,
        pane_id: &str,
        input: &[u8],
    ) -> Result<()> {
        let delivery = self
            .process
            .running_shell_transactions
            .iter()
            .find_map(|(marker, transaction)| {
                (transaction.pane_id == pane_id).then(|| marker.clone())
            })
            .map_or_else(
                || mez_mux::process::ShellInputDelivery::generated_source(input.to_vec()),
                |marker| {
                    mez_mux::process::ShellInputDelivery::generated_source_for_transaction(
                        input.to_vec(),
                        marker,
                    )
                },
            );
        self.write_runtime_pane_shell_delivery(pane_id, delivery)
    }

    /// Writes one typed shell delivery without dropping pacing or identity.
    pub(in crate::runtime) fn write_runtime_pane_shell_delivery(
        &mut self,
        pane_id: &str,
        delivery: mez_mux::process::ShellInputDelivery,
    ) -> Result<()> {
        if delivery.bytes.is_empty() {
            return Err(MezError::invalid_args("pane input must not be empty"));
        }
        delivery
            .validate_logical_records()
            .map_err(|error| MezError::invalid_args(error.to_string()))?;
        #[cfg(not(target_os = "macos"))]
        if self.process.pane_processes.contains_pane(pane_id) {
            return Ok(self
                .process
                .pane_processes
                .write_pane_shell_delivery(pane_id, &delivery)?);
        }
        #[cfg(target_os = "macos")]
        if self.process.pane_processes.contains_pane(pane_id) {
            return Ok(self
                .process
                .pane_processes
                .write_pane_shell_delivery(pane_id, &delivery)?);
        }
        if let Some(instance) = self.adapter_owned_pane_process_instance(pane_id) {
            self.persistence
                .queue_pane_input(RuntimeSideEffect::PaneProcessIo {
                    instance,
                    effect: PaneProcessIoEffect::WriteShellInput { delivery },
                });
            return Ok(());
        }
        Err(MezError::new(
            crate::error::MezErrorKind::NotFound,
            "pane process not found",
        ))
    }

    /// Cancels the unsent tail of one transaction-scoped shell delivery.
    /// Stale transactions cannot discard input for replacement process generations.
    pub(in crate::runtime) fn cancel_runtime_pane_shell_delivery(
        &mut self,
        pane_id: &str,
        delivery_id: &str,
    ) {
        let Some(instance) = self.adapter_owned_pane_process_instance(pane_id) else {
            return;
        };
        self.persistence
            .queue_shell_input_cancellation(instance, delivery_id.to_string());
    }

    /// Writes pane input with optional async queue priority.
    fn write_runtime_pane_input_with_priority(
        &mut self,
        pane_id: &str,
        input: &[u8],
        priority: bool,
    ) -> Result<()> {
        if input.is_empty() {
            return Err(MezError::invalid_args("pane input must not be empty"));
        }
        #[cfg(test)]
        if std::mem::take(&mut self.process.require_registered_transaction_on_next_write)
            && !self
                .process
                .running_shell_transactions
                .values()
                .any(|transaction| transaction.pane_id == pane_id)
        {
            return Err(MezError::invalid_state(
                "pane transaction must be registered before delivery",
            ));
        }
        #[cfg(test)]
        if std::mem::take(&mut self.process.fail_next_pane_input_write) {
            return Err(MezError::new(
                crate::error::MezErrorKind::Io,
                "injected pane input write failure",
            ));
        }
        #[cfg(test)]
        if input == b"\x03" && std::mem::take(&mut self.process.fail_next_pane_interrupt_write) {
            return Err(MezError::new(
                crate::error::MezErrorKind::Io,
                "injected pane interrupt write failure",
            ));
        }
        if self.process.pane_processes.contains_pane(pane_id) {
            return Ok(self
                .process
                .pane_processes
                .write_pane_input(pane_id, input)?);
        }
        if let Some(instance) = self.adapter_owned_pane_process_instance(pane_id) {
            self.persistence
                .queue_pane_input(RuntimeSideEffect::PaneProcessIo {
                    instance,
                    effect: if priority {
                        PaneProcessIoEffect::WriteInputPriority {
                            bytes: input.to_vec(),
                        }
                    } else {
                        PaneProcessIoEffect::WriteInput {
                            bytes: input.to_vec(),
                        }
                    },
                });
            return Ok(());
        }
        Err(MezError::new(
            crate::error::MezErrorKind::NotFound,
            "pane process not found",
        ))
    }

    /// Writes pane input ahead of later queued input for the same async pane.
    pub(in crate::runtime) fn write_runtime_pane_input_priority(
        &mut self,
        pane_id: &str,
        input: &[u8],
    ) -> Result<()> {
        self.write_runtime_pane_input_with_priority(pane_id, input, true)
    }
}
