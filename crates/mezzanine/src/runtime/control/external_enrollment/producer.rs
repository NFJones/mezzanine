//! Typed retained producer facts, separate from ingress/socket-writer authority.
//!
//! A native verified parent is not a socket-origin substitute. Constructing facts
//! grants no registration, observer, lease, client or usage rights. Initial parent
//! admission still needs a source-qualified policy/session/actor consumer.

use super::*;
use crate::runtime::UnixParentProcess;
use mez_mux::process::ProcessParentIdentity;

#[derive(Debug, Clone)]
/// Retained native facts with an explicit source-kind boundary. Neither variant
/// constructs a registration or makes another kind's socket an observer.
pub(super) enum ProducerEvidence {
    /// Original kernel-qualified socket process, not a payload-selected PID.
    Socket(Arc<UnixOriginProcess>),
    /// Previously verified direct parent whose owned lifetime survives its helper.
    #[allow(
        dead_code,
        reason = "source-qualified initial parent admission is unfinished"
    )]
    VerifiedParent(Arc<UnixParentProcess>),
}

impl ProducerEvidence {
    /// Returns captured native UID metadata without granting transport authority.
    pub(super) fn uid(&self) -> u32 {
        match self {
            Self::Socket(source) => source.uid(),
            Self::VerifiedParent(source) => source.uid(),
        }
    }
    /// Returns the immutable paired PID/parent/start record of the retained owner.
    pub(super) fn identity(&self) -> ProcessParentIdentity {
        match self {
            Self::Socket(source) => source.identity,
            Self::VerifiedParent(source) => source.identity,
        }
    }
    /// Polls the original kernel lifetime; it does not verify observer freshness.
    pub(super) fn is_live(&self) -> bool {
        match self {
            Self::Socket(source) => source.is_live(),
            Self::VerifiedParent(source) => source.is_live(),
        }
    }
    /// Reobserves this exact source within its native lifetime; unavailable,
    /// changed birth/relationship/UID or unsupported evidence fails closed.
    pub(super) fn reobserve(&self) -> std::io::Result<ProcessParentIdentity> {
        match self {
            Self::Socket(source) => source.reobserve(),
            Self::VerifiedParent(source) => source.reobserve(),
        }
    }
    /// Metadata equality cannot cross evidence kinds or replace an exact owner.
    pub(super) fn same_owner(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Socket(left), Self::Socket(right)) => Arc::ptr_eq(left, right),
            (Self::VerifiedParent(left), Self::VerifiedParent(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
    /// Only a socket-backed producer can authorize reconnect/capability use or
    /// an observer transport; a parent's matching UID/PID/start is not authority.
    pub(super) fn matches_socket(&self, origin: &UnixOriginProcess) -> bool {
        matches!(self, Self::Socket(source) if source.uid() == origin.uid() && source.identity == origin.identity)
    }
    /// Authorizes only a current sender-confirmed socket of the same socket source
    /// kind/UID/native record. Parent facts and unavailable native observations
    /// return Forbidden; this check alone is not ancestry/root/actor authorization.
    pub(super) fn authorize_socket(&self, origin: &UnixOriginProcess) -> Result<()> {
        if !origin.writer_confirmed() || !self.matches_socket(origin) {
            return Err(MezError::forbidden(
                "external producer differs from socket enrollment",
            ));
        }
        origin
            .reobserve()
            .map_err(|_| MezError::forbidden("external producer unavailable"))?;
        self.reobserve()
            .map_err(|_| MezError::forbidden("external enrolled producer unavailable"))?;
        Ok(())
    }
}
