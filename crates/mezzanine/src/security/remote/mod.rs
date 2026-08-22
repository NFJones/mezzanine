//! Protected identity and application trust for the optional Iroh transport.
//!
//! Iroh endpoint authentication proves possession of a network key. This
//! module deliberately keeps that fact separate from Mezzanine authority: a
//! remote endpoint becomes an application principal only through a durable,
//! non-revoked trust record with an explicit role ceiling. Endpoint key and
//! trust persistence remain usable before the network listener is introduced.

mod client;
mod store;
mod types;

pub(crate) use client::{
    RemoteClientIdentity, RemoteClientProfile, RemoteClientProfileStore,
    read_remote_invitation_file,
};
pub(crate) use store::{RemoteEndpointIdentity, RemotePairingPreparation, RemoteTrustStore};
pub(crate) use types::{
    RemotePairingRedemption, RemotePrincipal, RemoteRoleCeiling, RemoteTrustRecord,
};

#[cfg(test)]
mod tests;
