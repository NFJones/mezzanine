//! Original-root sender-fact lookup, canonical write and consumption continuation.
//!
//! Historical lookup precedes any live context preflight and horizon selection.
//! The runtime owns at most one live projection or sealed positive. A joined
//! historical positive only settles original transport; qualified absence can
//! select only a genuine same-runtime accepted action. Physical uncertainty uses
//! the root's retained job; unissued admission failures use local bounded backoff.

use super::*;
use mez_agent::messaging::MessageTerminalHandoffAnchor;

impl AsyncRuntimeSessionActor {
    /// Applies a retained positive independently of physical worker ownership.
    pub(super) fn pump_terminal_handoff_application(&mut self) {
        if let Err(error) = self.service.apply_terminal_handoff_commit() {
            self.service.defer_terminal_handoff_completion(error);
        }
    }

    /// Starts one canonical probe/write or proof-qualified lower consumption.
    pub(super) fn pump_terminal_handoff(&mut self) {
        let Some(owner) = self.message_custody.as_ref() else {
            return;
        };
        if owner.pending.is_some()
            || owner.wake.is_some()
            || owner.failure.is_some()
            || owner.native_result.is_some()
            || !self.service.native_input_storage_required()
            || owner
                .handoff_retry
                .as_ref()
                .is_some_and(|(at, source)| source.is_none() && *at > Instant::now())
        {
            return;
        }
        if let Some(proof) = self.service.pending_terminal_handoff_consumption() {
            let Some(owner) = self.message_custody.as_mut() else {
                return;
            };
            let ticket = match self
                .service
                .message_service_mut()
                .prepare_storage_sender_consumption(
                    owner.root.storage_scope(),
                    &proof,
                    8 * 1024 * 1024,
                ) {
                Ok(ticket) => ticket,
                Err(error) => {
                    self.service.defer_terminal_handoff_completion(error.into());
                    return;
                }
            };
            match owner
                .root
                .start_terminal(self.service.message_service_mut(), &ticket)
            {
                Ok(()) => {
                    owner.pending = Some(Pending::TerminalConsumption);
                    owner.wake = Some(Wake::Observe);
                    if owner
                        .handoff_retry
                        .as_ref()
                        .is_some_and(|(_, source)| source.is_none())
                    {
                        owner.handoff_retry = None;
                    }
                }
                Err(error) => {
                    match self
                        .service
                        .message_service_mut()
                        .cancel_storage_terminal(&ticket)
                    {
                        Ok(()) => self.service.defer_terminal_handoff_completion(error),
                        Err(_) => {
                            // Cancellation failure retains the real selected
                            // ticket; never discard its original lower custody.
                            owner.pending = Some(Pending::TerminalConsumption);
                            owner.failure = Some(error);
                            owner.wake = Some(Wake::Retry(
                                Instant::now() + std::time::Duration::from_secs(1),
                            ));
                        }
                    }
                }
            }
            return;
        }
        if let Some((source, target, projection)) = self.service.pending_terminal_handoff_work() {
            let Some(owner) = self.message_custody.as_mut() else {
                return;
            };
            match owner.root.start_terminal_handoff(
                self.service.message_service_mut(),
                source.clone(),
                target,
                Some(projection),
            ) {
                Ok(()) => {
                    self.service.note_terminal_handoff_write_issued();
                    owner.pending = Some(Pending::TerminalHandoff { source, live: true });
                    owner.wake = Some(Wake::Observe);
                    if owner
                        .handoff_retry
                        .as_ref()
                        .is_some_and(|(_, source)| source.is_none())
                    {
                        owner.handoff_retry = None;
                    }
                }
                Err(_) => {
                    owner.handoff_retry =
                        Some((Instant::now() + std::time::Duration::from_secs(1), None))
                }
            }
            return;
        }
        if self.service.has_pending_terminal_handoff() {
            return;
        }
        let Some(owner) = self.message_custody.as_mut() else {
            return;
        };
        if owner.shutdown.is_some() {
            return;
        }
        let retry = if owner
            .handoff_retry
            .as_ref()
            .is_some_and(|(at, _)| *at <= Instant::now())
        {
            let source = owner.handoff_retry.take().and_then(|(_, source)| source);
            owner.request_handoff_scan();
            source
        } else {
            None
        };
        let retrying = retry.is_some();
        if !owner.handoff_scan_pending && !retrying {
            return;
        }
        let source = match retry {
            Some(source) => source,
            None => match self
                .service
                .message_service()
                .next_terminal_handoff_lookup(owner.handoff_scan_after)
            {
                Ok(Some(source)) => source,
                Ok(None) => {
                    owner.handoff_scan_pending = std::mem::take(&mut owner.handoff_scan_again);
                    owner.handoff_scan_after = None;
                    return;
                }
                Err(_) => {
                    owner.handoff_retry =
                        Some((Instant::now() + std::time::Duration::from_secs(1), None));
                    return;
                }
            },
        };
        let Some(target) = self.service.agent_transcript_store().cloned() else {
            // Historical canonical proof needs its original private store. A
            // missing target cannot be converted into absence or horizon release.
            owner.park_handoff_source(source);
            return;
        };
        match owner.root.start_terminal_handoff(
            self.service.message_service_mut(),
            source.clone(),
            target,
            None,
        ) {
            Ok(()) => {
                if !retrying {
                    owner.handoff_scan_after = Some((source.sequence(), source.binding_index()));
                }
                owner.pending = Some(Pending::TerminalHandoff {
                    source,
                    live: false,
                });
                owner.wake = Some(Wake::Observe);
            }
            Err(_) => {
                owner.handoff_retry = Some((
                    Instant::now() + std::time::Duration::from_secs(1),
                    Some(source),
                ))
            }
        }
    }

    /// Qualified absence grants no task authority: runtime checks its actual
    /// accepted owner and real context before creating any neutral occurrence.
    pub(super) fn activate_absent_terminal_handoff(
        &mut self,
        source: MessageTerminalHandoffAnchor,
    ) {
        if self
            .message_custody
            .as_ref()
            .is_none_or(|owner| owner.shutdown.is_some())
        {
            return;
        }
        if (self.service.terminal_handoff_preflight_blocked(&source)
            || self.service.prepare_terminal_handoff(&source).is_err())
            && let Some(owner) = self.message_custody.as_mut()
        {
            owner.park_handoff_source(source);
        }
    }
}
