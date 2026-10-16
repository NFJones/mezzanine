//! Exact host owner scopes and CLI-only explicit cleanup preparation.
//!
//! Automatic factories capture continuations before first poll. A late owner
//! may contribute initial authority only, never a retry epoch. Explicit minting
//! is isolated to preparation consumed by CliExit's caller-owned retry path;
//! stop, expiry, reconciliation and lease replay never call that path. Every
//! scope remains bounded by actual admitted session owners, not history.

use super::*;
use mez_mux::process::{
    PaneProcessCleanupAttempt, PaneProcessCleanupIdentity, PreparedPaneProcessCleanupRetry,
};

/// One exact generation and its already captured native continuation.
pub(super) struct CapturedNativeOwner {
    pub(super) id: String,
    pub(super) generation: u64,
    pub(super) native: PaneProcessRetirementOwner,
    pub(super) attempt: Option<PaneProcessCleanupAttempt>,
    pub(super) error: Option<MezError>,
    identity: Option<PaneProcessCleanupIdentity>,
    may_cancel: bool,
}

/// Immutable-at-capture owners; first poll can add only late initial0 owners.
pub(crate) struct NativeContinuationScope {
    pub(super) owners: Vec<CapturedNativeOwner>,
}

/// Prepared owners have no effect authority until the explicit caller admits.
struct PreparedOwner {
    id: String,
    generation: u64,
    native: PaneProcessRetirementOwner,
    prepared: Result<PreparedPaneProcessCleanupRetry>,
}

/// Single-use preparation reachable only from explicit CliExit retry.
pub(crate) struct PreparedCliNativeRetry {
    owners: Vec<PreparedOwner>,
}

impl NativeContinuationScope {
    /// Clips effect admission independently of the longer observation budget.
    pub(super) fn effect_deadline(
        &self,
        id: &str,
        generation: u64,
        outer: tokio::time::Instant,
    ) -> tokio::time::Instant {
        self.owners
            .iter()
            .find(|owner| owner.id == id && owner.generation == generation)
            .and_then(|owner| owner.attempt.as_ref())
            .map_or(outer, |attempt| {
                outer.min(tokio::time::Instant::from_std(attempt.deadline()))
            })
    }

    /// Checks the exact captured source before any actor cancellation or stop
    /// admission. A rejected explicit owner receives no cancellation/effect
    /// permission; a replaced source/attempt cannot inherit admission.
    pub(super) fn permits(
        &self,
        id: &str,
        entry: &SupervisorEntry,
        deadline: tokio::time::Instant,
    ) -> bool {
        self.owners
            .iter()
            .find(|owner| owner.id == id && owner.generation == entry.generation)
            .is_some_and(|owner| {
                owner.may_cancel
                    && tokio::time::Instant::now() < deadline
                    && owner
                        .attempt
                        .as_ref()
                        .is_some_and(|attempt| attempt.validate_effects(&entry.native).is_ok())
                    && owner
                        .identity
                        .as_ref()
                        .is_some_and(|identity| identity.validate(&entry.native).is_ok())
            })
    }
}

impl CapturedNativeOwner {
    /// Separates an admitted immutable token from its one retained rejection.
    fn new(
        id: String,
        generation: u64,
        native: PaneProcessRetirementOwner,
        result: Result<PaneProcessCleanupAttempt>,
        identity: Option<PaneProcessCleanupIdentity>,
        may_cancel: bool,
    ) -> Self {
        let (attempt, error) = match result {
            Ok(attempt) => (Some(attempt), None),
            Err(error) => (None, Some(error)),
        };
        Self {
            id,
            generation,
            native,
            attempt,
            error,
            identity,
            may_cancel,
        }
    }
}

impl SessionSupervisor {
    /// Captures automatic native authority synchronously before future polling.
    /// Errors stay per owner so eligible siblings still receive their quanta.
    pub(crate) fn capture_native_continuation_scope(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<NativeContinuationScope> {
        let entries = self.inner.entries()?;
        let owners = entries
            .iter()
            .map(|(id, entry)| {
                let attempt = entry
                    .native
                    .capture_cleanup_attempt(deadline.into_std())
                    .map_err(MezError::from);
                let identity = attempt
                    .as_ref()
                    .ok()
                    .map(PaneProcessCleanupAttempt::identity);
                let may_cancel = attempt.is_ok();
                CapturedNativeOwner::new(
                    id.clone(),
                    entry.generation,
                    entry.native.clone(),
                    attempt,
                    identity,
                    may_cancel,
                )
            })
            .collect();
        Ok(NativeContinuationScope { owners })
    }

    /// Read-only exact preparation for the sole approved explicit CLI caller.
    /// No Host administrative/maintenance or ordinary finalizer uses this API.
    pub(crate) fn prepare_cli_native_retry(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<PreparedCliNativeRetry> {
        let owners = self
            .inner
            .entries()?
            .iter()
            .map(|(id, entry)| PreparedOwner {
                id: id.clone(),
                generation: entry.generation,
                native: entry.native.clone(),
                prepared: entry
                    .native
                    .prepare_explicit_cleanup_retry(deadline.into_std())
                    .map_err(MezError::from),
            })
            .collect();
        Ok(PreparedCliNativeRetry { owners })
    }

    /// Closes admission and applies cancellation only to validated captured
    /// identities. Late sources get initial0 only; replacements never replace
    /// a captured generation's token. No await or native I/O holds the map lock.
    pub(super) fn seal_native_scope(
        &self,
        scope: &mut NativeContinuationScope,
        deadline: tokio::time::Instant,
        cancel_running: bool,
    ) -> Result<()> {
        self.inner
            .native_cleanup_sealed
            .store(true, Ordering::Release);
        let mut entries = self.inner.entries()?;
        for (id, entry) in entries.iter_mut() {
            entry.native.seal();
            if let Some(owner) = scope
                .owners
                .iter_mut()
                .find(|owner| owner.id == *id && owner.generation == entry.generation)
                && let Some(attempt) = &owner.attempt
            {
                match attempt
                    .bind_first_source(&entry.native)
                    .map_err(MezError::from)
                {
                    Ok(attempt) => {
                        owner.identity = Some(attempt.identity());
                        owner.attempt = Some(attempt);
                    }
                    Err(error) => {
                        owner.attempt = None;
                        owner.error.get_or_insert(error);
                        owner.identity = None;
                        owner.may_cancel = false;
                    }
                }
            }
            if !scope.owners.iter().any(|owner| owner.id == *id) {
                let attempt = entry
                    .native
                    .capture_initial_cleanup_attempt(deadline.into_std())
                    .map_err(MezError::from);
                let identity = attempt
                    .as_ref()
                    .ok()
                    .map(PaneProcessCleanupAttempt::identity);
                let may_cancel = attempt.is_ok();
                scope.owners.push(CapturedNativeOwner::new(
                    id.clone(),
                    entry.generation,
                    entry.native.clone(),
                    attempt,
                    identity,
                    may_cancel,
                ));
            } else if !scope
                .owners
                .iter()
                .any(|owner| owner.id == *id && owner.generation == entry.generation)
            {
                scope.owners.push(CapturedNativeOwner::new(
                    id.clone(),
                    entry.generation,
                    entry.native.clone(),
                    Err(MezError::conflict("native scope generation was replaced")),
                    None,
                    false,
                ));
            }
            if scope.permits(id, entry, deadline) {
                match &mut entry.state {
                    SupervisorEntryState::Starting { cancelled } => *cancelled = true,
                    SupervisorEntryState::Running { cancel, .. }
                    | SupervisorEntryState::Stopping { cancel, .. }
                        if cancel_running =>
                    {
                        let _ = cancel.send(true);
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Admits every prepared CLI owner independently under the same outer
    /// deadline. Rejected/busy owners retain their error and receive no new
    /// token, while eligible siblings remain available for fair progression.
    pub(crate) fn admit_cli_native_retry(
        &self,
        prepared: PreparedCliNativeRetry,
    ) -> Result<NativeContinuationScope> {
        self.inner
            .native_cleanup_sealed
            .store(true, Ordering::Release);
        let entries = self.inner.entries()?;
        for entry in entries.values() {
            entry.native.seal();
        }
        let owners = prepared
            .owners
            .into_iter()
            .map(|owner| {
                let generation_matches = entries
                    .get(&owner.id)
                    .is_none_or(|entry| entry.generation == owner.generation);
                let mut identity = None;
                let mut may_cancel = false;
                let attempt = if !generation_matches {
                    Err(MezError::conflict(
                        "prepared CLI native generation was replaced",
                    ))
                } else {
                    owner.native.seal();
                    owner.prepared.and_then(|prepared| {
                        prepared.validate(&owner.native)?;
                        let attempt = prepared.admit().map_err(MezError::from)?;
                        identity = Some(attempt.identity());
                        may_cancel = true;
                        Ok(attempt)
                    })
                };
                CapturedNativeOwner::new(
                    owner.id,
                    owner.generation,
                    owner.native,
                    attempt,
                    identity,
                    may_cancel,
                )
            })
            .collect();
        Ok(NativeContinuationScope { owners })
    }
}
