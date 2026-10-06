//! One consumed frontend operation with owner-resolved protected evidence.
//!
//! Setup never connects, redeems invitations or creates sessions. It reuses the
//! existing control-initialize parser and role/scope contract. A frontend sends
//! a profile alias and credential-free initialization, or a protected invitation
//! path for the separate pairing owner. Preparation itself performs no redemption.
//! Private
//! device proof is loaded in bounded worker ownership and never returned on IPC.
//! Consuming the frontend prevents a second setup from retargeting one stream.
//! Profile I/O may outlive cancellation of its waiter, but cannot start network
//! work. The returned owner retains the exact local stream and endpoint lifetime.

use super::*;
use crate::control::{RequestedRole, SessionIntent, initialize_params_from_json};
use crate::security::remote::{
    RemoteClientProfile, RemoteClientProfileScope, RemoteClientProfileStore,
};

/// Strict setup envelope; route addresses and authentication are not inputs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Setup {
    handle: FrontendHandle,
    profile: String,
    initialize: serde_json::Value,
}

/// Validated local setup, not an authenticated remote connection or session.
/// Private fields deliberately prevent credential export or frontend retargeting.
pub(crate) struct PreparedFrontend {
    frontend: AdmittedFrontend,
    profile: RemoteClientProfile,
    initialize: serde_json::Value,
}

mod connection;
mod pairing;

/// One consumed, validated operation; pairing never masquerades as a profile.
pub(super) enum PreparedOperation {
    /// Existing paired-profile setup with unchanged initialization semantics.
    Profile(PreparedFrontend),
    /// Protected invitation evidence retained only by the endpoint owner.
    Pairing(pairing::PreparedPairing),
}

impl AdmittedFrontend {
    /// Consumes one strict bounded setup request and resolves its protected alias.
    /// Rejection/cancellation disposes this local stream, never replaying input.
    /// A total deadline includes request receive and profile-worker completion.
    pub(crate) async fn prepare(self, deadline: Duration) -> Result<PreparedFrontend> {
        match self.prepare_operation(deadline).await? {
            PreparedOperation::Profile(profile) => Ok(profile),
            PreparedOperation::Pairing(_) => Err(MezError::invalid_args(
                "pairing requires operation-aware consumption",
            )),
        }
    }

    /// Consumes exactly one bounded profile or invitation operation. Blocking
    /// work retains endpoint and admission capacity until it actually exits.
    pub(super) async fn prepare_operation(self, deadline: Duration) -> Result<PreparedOperation> {
        if !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&deadline) {
            return Err(MezError::invalid_args(
                "outbound setup deadline unavailable",
            ));
        }
        tokio::time::timeout(deadline, self.prepare_inner())
            .await
            .map_err(|_| MezError::invalid_state("outbound frontend setup timed out"))?
    }

    /// Keeps source parsing and protected-profile work in one consumed transition.
    async fn prepare_inner(mut self) -> Result<PreparedOperation> {
        let frame = self
            .stream
            .next()
            .await
            .transpose()?
            .ok_or_else(|| MezError::invalid_state("outbound frontend setup unavailable"))?;
        if frame.content_type != CONTENT_TYPE {
            return Err(MezError::invalid_args(
                "outbound frontend content type unsupported",
            ));
        }
        let envelope: serde_json::Value = serde_json::from_str(&frame.body)
            .map_err(|_| MezError::invalid_args("outbound frontend setup invalid"))?;
        if envelope.get("operation").is_some() {
            return pairing::prepare(self, &frame.body)
                .await
                .map(PreparedOperation::Pairing);
        }
        let setup: Setup = serde_json::from_str(&frame.body)
            .map_err(|_| MezError::invalid_args("outbound frontend setup invalid"))?;
        if setup.handle != self.handle {
            return Err(MezError::conflict("outbound frontend handle changed"));
        }
        if setup.profile.is_empty()
            || setup.profile.len() > 128
            || setup.profile.chars().any(char::is_control)
        {
            return Err(MezError::invalid_args("outbound profile alias invalid"));
        }
        let object = setup
            .initialize
            .as_object()
            .ok_or_else(|| MezError::invalid_args("outbound initialize must be an object"))?;
        if object.contains_key("authentication") {
            return Err(MezError::forbidden(
                "outbound frontend must not supply credentials",
            ));
        }
        let params = initialize_params_from_json(&setup.initialize.to_string())?;
        if !matches!(
            params.requested_role,
            RequestedRole::Primary | RequestedRole::Observer
        ) {
            return Err(MezError::forbidden("outbound frontend role unsupported"));
        }
        let root = self._endpoint.frontend_config_root()?.to_path_buf();
        // A blocking profile lock/read cannot be cancelled by dropping its
        // waiter. Retain this frontend's finite slot and endpoint until the
        // worker actually exits, so cancellation cannot accumulate orphan I/O.
        let slot = self._slot.clone();
        let endpoint = self._endpoint.clone();
        let profile = tokio::task::spawn_blocking(move || {
            let _slot = slot;
            endpoint.frontend_config_root()?;
            let result = RemoteClientProfileStore::under_config_root(&root)
                .load_for_outbound(&setup.profile);
            endpoint.frontend_config_root()?;
            result
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound profile worker unavailable"))?
        .map_err(|_| MezError::invalid_state("outbound protected profile unavailable"))?
        .ok_or_else(|| MezError::new(MezErrorKind::NotFound, "outbound profile unavailable"))?;
        self._endpoint.frontend_config_root()?;
        if !profile.role.permits(params.requested_role) {
            return Err(MezError::forbidden(
                "outbound profile role ceiling exceeded",
            ));
        }
        match profile.scope {
            RemoteClientProfileScope::Host if params.requested_version != 3 => {
                return Err(MezError::invalid_args(
                    "host profile requires protocol-v3 session intent",
                ));
            }
            RemoteClientProfileScope::LegacySession
                if params.requested_version != 2
                    || params.session_intent.is_some()
                    || params.session_target_json.is_some() =>
            {
                return Err(MezError::invalid_args(
                    "legacy profile supports direct protocol-v2 attachment only",
                ));
            }
            _ => {}
        }
        if params.session_intent == Some(SessionIntent::Create)
            && params.requested_role != RequestedRole::Primary
        {
            return Err(MezError::forbidden(
                "outbound fresh creation requires primary role",
            ));
        }
        Ok(PreparedOperation::Profile(PreparedFrontend {
            frontend: self,
            profile,
            initialize: setup.initialize,
        }))
    }
}

#[cfg(test)]
mod tests;
