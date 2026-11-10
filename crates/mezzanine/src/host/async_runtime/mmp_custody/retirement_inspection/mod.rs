//! Optional candidate analysis/preparation on the original serialized store lane.
//!
//! Capture and actual joins remain root-owned across observer cancellation.
//! Typed dependency refusal is distinct from integrity failure. A lost worker
//! permits one exact fenced connection reopen after join, but the store remains
//! gated until an explicit fresh inspection succeeds. No cleanup timer is
//! installed, and no analysis or delivery effect is automatically replayed.
//! Explicit durable preparation writes only q1 evidence and gates the live root;
//! cold cancellation verifies intact originals before removing that evidence.
//! A separate explicit ACK-only non-native operation can replace one original
//! atomically; it never uses q1 cancellation as reclamation authority.

use super::*;
use crate::storage::mmp_custody::retirement_inspection::{
    SourceRetirementCandidate, SourceRetirementFailure,
};
use std::task::Context;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Inspect,
    Stage,
    ReclaimAcknowledged,
}

/// Exact read provenance survives observer/worker loss and repeated refusal.
pub(super) struct Admission {
    plan: AcceptancePlan,
    sequence: u64,
    repair: Option<RepairGate>,
    /// Root escrow keeps the reservation through actual join and adoption/loss.
    _staging: Option<Arc<mez_agent::messaging::MessageSourcePreservationReservation>>,
    _encoding: Option<Arc<mez_agent::messaging::MessageSourcePreservationReservation>>,
    _rewrite:
        Option<Arc<crate::storage::mmp_custody::physical_capacity::RewriteCapacityReservation>>,
}

pub(super) struct RepairGate {
    sequence: u64,
    pub(super) error: MezError,
}

pub(super) struct Written {
    opened: Opened,
    durable: bool,
    pub(super) candidate:
        std::result::Result<Option<SourceRetirementCandidate>, SourceRetirementFailure>,
}

impl MmpCustodyRoot {
    /// Captures charged source-absence evidence only after securing an idle or
    /// explicitly refused lane. Live/future seeds, Stop and foreign roots issue
    /// no I/O. Repair inspection is explicit and has the ordinary scratch cap.
    pub(crate) fn start_retirement_inspection(
        &self,
        service: &MessageService,
        sequence: u64,
    ) -> Result<()> {
        self.start_retirement_analysis(service, sequence, None, None, Mode::Inspect)
    }

    /// Funds bounded bodyless evidence before starting I/O on the same owned
    /// lane. A small limit may produce typed refusal, never a partial result.
    /// This reserves RAM only, not physical staging/rewrite space or deletion.
    pub(crate) fn start_retirement_preservation(
        &self,
        service: &MessageService,
        sequence: u64,
        encoded_bytes_limit: usize,
    ) -> Result<()> {
        self.start_retirement_analysis(
            service,
            sequence,
            Some(encoded_bytes_limit),
            None,
            Mode::Inspect,
        )
    }

    /// Reserves shared physical rewrite headroom before worker issuance; the
    /// actual filesystem envelope is then checked on the owned blocking lane.
    /// This creates no file/intent and grants no native or deletion authority.
    pub(crate) fn start_retirement_rewrite_preparation(
        &self,
        service: &MessageService,
        sequence: u64,
        encoded_bytes_limit: usize,
        rewrite_bytes: usize,
    ) -> Result<()> {
        self.start_retirement_analysis(
            service,
            sequence,
            Some(encoded_bytes_limit),
            Some(rewrite_bytes),
            Mode::Inspect,
        )
    }

    /// Publishes only bounded non-destructive preparation. Success gates this
    /// live root until cold cancellation validates the intact originals; it is
    /// neither a native dependency-closed intent nor permission to delete mail.
    pub(crate) fn start_durable_retirement_preparation(
        &self,
        service: &MessageService,
        sequence: u64,
        encoded_bytes_limit: usize,
        rewrite_bytes: usize,
    ) -> Result<()> {
        self.start_retirement_analysis(
            service,
            sequence,
            Some(encoded_bytes_limit),
            Some(rewrite_bytes),
            Mode::Stage,
        )
    }

    /// Atomically retires one absent, fully acknowledged non-native payload.
    /// Larger, native and terminal cases refuse without partial cleanup. Uses
    /// the same source-absence capture, worker and actual root rewrite reserve.
    pub(crate) fn start_acknowledged_source_reclamation(
        &self,
        service: &MessageService,
        sequence: u64,
        encoded_bytes_limit: usize,
        rewrite_bytes: usize,
    ) -> Result<()> {
        self.start_retirement_analysis(
            service,
            sequence,
            Some(encoded_bytes_limit),
            Some(rewrite_bytes),
            Mode::ReclaimAcknowledged,
        )
    }

    fn start_retirement_analysis(
        &self,
        service: &MessageService,
        sequence: u64,
        staging_limit: Option<usize>,
        rewrite_bytes: Option<usize>,
        mode: Mode,
    ) -> Result<()> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let opened = match &*state {
            State::Idle(opened) => opened,
            State::RetirementInspectionRefused { opened, gate } if sequence == gate.sequence => {
                opened
            }
            _ => return Err(ownership_error()),
        };
        if self.0.stopping.load(Ordering::Acquire) {
            return Err(ownership_error());
        }
        let input = service
            .prepare_source_retirement_inspection(&self.0.storage_scope)
            .map_err(lower_error)?;
        if !input.has_no_live_source(sequence)
            || !service.source_retirement_inspection_is_original(&input)
        {
            return Err(ownership_error());
        }
        let staging = staging_limit
            .map(|limit| {
                service
                    .reserve_source_preservation_staging(&input, limit)
                    .map(Arc::new)
                    .map_err(lower_error)
            })
            .transpose()?;
        let encoding = (mode == Mode::ReclaimAcknowledged).then(|| {
            service.reserve_source_preservation_staging(
                &input,
                crate::storage::mmp_custody::retirement_inspection::acknowledged::WRITE_BUFFER_BYTES,
            ).map(Arc::new).map_err(lower_error)
        }).transpose()?;
        let plan = opened.anchor.clone();
        let rewrite = rewrite_bytes
            .map(|bytes| {
                self.0
                    .ownership
                    .lock()
                    .map_err(|_| ownership_error())?
                    .as_ref()
                    .ok_or_else(ownership_error)?
                    .reserve_rewrite_capacity(&plan, bytes)
            })
            .transpose()?;
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        let (mut opened, repair) = match previous {
            State::Idle(opened) => (opened, None),
            State::RetirementInspectionRefused { opened, gate } => (opened, Some(gate)),
            _ => return Err(ownership_error()),
        };
        let admission = Admission {
            plan,
            sequence,
            repair,
            _staging: staging.clone(),
            _encoding: encoding.clone(),
            _rewrite: rewrite.clone(),
        };
        let ownership = self.0.ownership.clone();
        let scope = self.0.storage_scope.clone();
        #[cfg(test)]
        let probe = self.0.probe.clone();
        let worker = self.0.runtime.spawn_blocking(move || {
            #[cfg(test)]
            if let Some(probe) = &probe {
                probe.before_write();
            }
            let fence = ownership
                .lock()
                .map_err(|_| ownership_error())?
                .as_ref()
                .cloned()
                .ok_or_else(ownership_error)?;
            let mut input = Arc::new(input);
            let mut durable = false;
            let candidate = {
                let mut owned = fence.bind_store(&mut opened.store)?;
                if !owned
                    .store_mut()
                    .acceptance_plan(0)?
                    .same_position(&opened.anchor)
                {
                    return Err(ownership_error());
                }
                if rewrite
                    .as_ref()
                    .is_some_and(|r| !r.matches(&fence, &opened.anchor))
                {
                    return Err(ownership_error());
                }
                if rewrite.is_some() && !owned.qualify_physical_capacity()? {
                    // No analysis or write occurred. Keep the actual opened
                    // store; do not misclassify expected refusal as worker loss.
                    owned.revalidate()?;
                    return Ok(Written {
                        opened,
                        durable: false,
                        candidate: Ok(None),
                    });
                }
                let mut candidate = if staging.is_some() {
                    owned.inspect_source_retirement_with_preservation(
                        &mut input, &scope, sequence, staging,
                    )
                } else {
                    owned.inspect_source_retirement_candidate(&mut input, &scope, sequence)
                };
                if let (Ok(Some(candidate)), Some(reservation)) = (&mut candidate, &rewrite) {
                    candidate.retain_rewrite_capacity(reservation.clone())?;
                }
                owned.revalidate()?;
                if !owned
                    .store_mut()
                    .acceptance_plan(0)?
                    .same_position(&opened.anchor)
                {
                    return Err(ownership_error());
                }
                if mode != Mode::Inspect
                    && let Ok(Some(prepared)) = &candidate
                {
                    let changed = match mode {
                        Mode::Stage => owned.persist_retirement_preparation(prepared)?,
                        Mode::ReclaimAcknowledged => owned.reclaim_acknowledged_source(
                            prepared,
                            sequence,
                            encoding.as_deref().ok_or_else(ownership_error)?,
                        )?,
                        Mode::Inspect => return Err(ownership_error()),
                    };
                    durable = changed && mode == Mode::Stage;
                    if changed {
                        opened.anchor = owned.store_mut().acceptance_plan(0)?;
                    } else {
                        candidate = Ok(None);
                    }
                }
                candidate
            };
            #[cfg(test)]
            if let Some(probe) = &probe {
                probe.after_payload_read();
            }
            {
                let mut bound = fence.bind_store(&mut opened.store)?;
                if !bound
                    .store_mut()
                    .acceptance_plan(0)?
                    .same_position(&opened.anchor)
                {
                    return Err(ownership_error());
                }
            }
            Ok(Written {
                opened,
                candidate,
                durable,
            })
        });
        *state = State::InspectingRetirement { admission, worker };
        Ok(())
    }

    /// Consumes one actual joined result. Only typed expected refusal can return
    /// None and release the lane. Every integrity failure gates admission; no
    /// weaker validator may downgrade it. Stale lower owners receive no result.
    /// Actual worker loss permits one exact reopen, still gated until explicit
    /// fresh analysis, not an automatic rescan or a fabricated healthy store.
    pub(crate) fn finish_retirement_inspection(
        &self,
        service: &MessageService,
    ) -> Result<Option<SourceRetirementCandidate>> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        if !matches!(&*state, State::InspectedRetirement { .. }) {
            return Err(ownership_error());
        }
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        let State::InspectedRetirement { admission, result } = previous else {
            return Err(ownership_error());
        };
        match result {
            Ok(written) => {
                let Written {
                    opened,
                    candidate,
                    durable,
                } = *written;
                match candidate {
                    Err(failure) => {
                        let error = failure.error;
                        *state = State::RetirementInspectionRefused {
                            opened,
                            gate: RepairGate {
                                sequence: failure.sequence,
                                error: error.clone(),
                            },
                        };
                        Err(error)
                    }
                    Ok(candidate) => {
                        if let Some(gate) = admission.repair
                            && candidate.as_ref().is_none_or(|c| {
                                !c.is_original(service)
                                    || !c
                                        .sources()
                                        .iter()
                                        .any(|source| source.sequence == gate.sequence)
                            })
                        {
                            let error = gate.error.clone();
                            *state = State::RetirementInspectionRefused { opened, gate };
                            return Err(error);
                        }
                        *state = if durable {
                            State::Unavailable(MezError::invalid_state(
                                "MMP durable retirement preparation awaits cold cancellation",
                            ))
                        } else {
                            State::Idle(opened)
                        };
                        if candidate.as_ref().is_some_and(|c| !c.is_original(service)) {
                            return Err(ownership_error());
                        }
                        Ok(candidate)
                    }
                }
            }
            Err(error) => {
                let gate = admission.repair.unwrap_or_else(|| RepairGate {
                    sequence: admission.sequence,
                    error: error.clone(),
                });
                let plan = admission.plan;
                let destination = self.0.destination.clone();
                let ownership = self.0.ownership.clone();
                let worker = self.0.runtime.spawn_blocking(move || {
                    let fence = ownership
                        .lock()
                        .map_err(|_| ownership_error())?
                        .as_ref()
                        .cloned()
                        .ok_or_else(ownership_error)?;
                    fence.validate_root()?;
                    let mut store = MmpCustodyStore::open_existing(
                        &destination.root,
                        &destination.owner,
                        destination.limits,
                    )?;
                    let mut owned = fence.bind_store(&mut store)?;
                    if !owned.store_mut().acceptance_plan(0)?.same_position(&plan) {
                        return Err(ownership_error());
                    }
                    owned.revalidate()?;
                    Ok(Opened {
                        store,
                        anchor: plan,
                    })
                });
                *state = State::ReopeningRetirement { worker, gate };
                Err(error)
            }
        }
    }
}

/// Polls only retained actual handles; cancellation cannot remove either worker.
pub(super) fn poll_join(state: &mut State, cx: &mut Context<'_>) -> Poll<()> {
    if let State::ReopeningRetirement { worker, .. } = state {
        let result = match Pin::new(worker).poll(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(result) => result.map_err(|_| worker_error()).and_then(|r| r),
        };
        let previous = std::mem::replace(state, State::Unavailable(ownership_error()));
        if let State::ReopeningRetirement { gate, .. } = previous {
            *state = match result {
                Ok(opened) => State::RetirementInspectionRefused { opened, gate },
                Err(reopen_error) => State::Unavailable(reopen_error),
            };
        }
        return Poll::Ready(());
    }
    let State::InspectingRetirement { worker, .. } = state else {
        return Poll::Ready(());
    };
    let result = match Pin::new(worker).poll(cx) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(result) => result.map_err(|_| worker_error()).and_then(|r| r),
    };
    let previous = std::mem::replace(state, State::Unavailable(ownership_error()));
    if let State::InspectingRetirement { admission, .. } = previous {
        *state = State::InspectedRetirement {
            admission,
            result: result.map(Box::new),
        };
    }
    Poll::Ready(())
}
