//! Direct actor authority bridge for filesystem preparation and exact commits.
//!
//! This borrowed adapter exercises the same actor owner used by the upcoming
//! async request bridge. It never promotes a scope snapshot into a write lease:
//! mutations return to the serialized owner to acquire exact single-use commit
//! admission and shared in-flight scheduling before publication.

use mez_agent::native_action::{NativeCommitIdentity, NativePatchPhase};
use mez_agent::permissions::PathScopes;

use super::NativeFilesystemAttempt;
use crate::error::Result;
use crate::runtime::RuntimeSessionService;
use crate::runtime::processes::native_patch::lease::{
    NativePatchCommitLease, NativePatchCommitProposal,
};
use crate::runtime::processes::native_patch::worker::NativePatchAuthority;

/// Direct borrowed actor bridge; this is not an asynchronous dispatch client.
pub(crate) struct RuntimeNativeFilesystemAuthority<'a> {
    /// Actual actor owner; caller retains exclusive serialized access.
    service: &'a mut RuntimeSessionService,
    /// Opaque exact attempt rather than reconstructed action/CID labels.
    attempt: &'a NativeFilesystemAttempt,
}

impl<'a> RuntimeNativeFilesystemAuthority<'a> {
    /// Borrows actual actor state for focused direct-worker integration.
    pub(crate) fn new(
        service: &'a mut RuntimeSessionService,
        attempt: &'a NativeFilesystemAttempt,
    ) -> Self {
        Self { service, attempt }
    }
}

impl NativePatchAuthority for RuntimeNativeFilesystemAuthority<'_> {
    /// Rechecks original actor ownership/current scopes for preparation only.
    fn scopes(
        &mut self,
        identity: &NativeCommitIdentity,
        _phase: NativePatchPhase,
        _path: &str,
    ) -> Result<PathScopes> {
        self.service
            .native_filesystem_scope_snapshot(self.attempt, identity)
    }

    /// Returns to actual actor admission for this exact proposed endpoint.
    fn commit(
        &mut self,
        identity: &NativeCommitIdentity,
        proposal: &NativePatchCommitProposal,
    ) -> Result<NativePatchCommitLease> {
        self.service
            .authorize_native_filesystem_commit(self.attempt, identity, proposal)
    }
}
