//! Authenticated transport identity for control connections.
//!
//! Transport authentication establishes which OS user or remote endpoint is
//! connected. It does not grant a Mezzanine role, session scope, or method
//! permission; those remain application-level authorization decisions made
//! during and after `control/initialize`.

/// Identity established by a concrete control transport before shared framing
/// or request dispatch begins.
#[allow(
    dead_code,
    reason = "the remote variant is consumed by the planned Iroh control adapter"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthenticatedPeer {
    /// Local peer authenticated from Unix-domain socket credentials.
    UnixUser {
        /// Effective user ID reported by the operating system.
        uid: u32,
    },
    /// Remote peer authenticated by an Iroh endpoint key.
    IrohEndpoint {
        /// Stable textual endpoint identifier supplied by the Iroh adapter.
        endpoint_id: String,
    },
}

impl AuthenticatedPeer {
    /// Creates an authenticated local Unix-user identity.
    pub fn unix_user(uid: u32) -> Self {
        Self::UnixUser { uid }
    }

    /// Creates an authenticated remote Iroh endpoint identity.
    ///
    /// The resulting identity is transport evidence only and must be resolved
    /// through pairing and trust records before it grants application access.
    #[allow(
        dead_code,
        reason = "the constructor is consumed by the planned Iroh control adapter"
    )]
    pub fn iroh_endpoint(endpoint_id: impl Into<String>) -> Self {
        Self::IrohEndpoint {
            endpoint_id: endpoint_id.into(),
        }
    }
}
