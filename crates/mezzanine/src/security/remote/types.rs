//! Durable and secret-safe remote trust data types.

use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::control::RequestedRole;

/// Maximum Mezzanine role a paired remote device may request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RemoteRoleCeiling {
    /// Device may request observer access only.
    Observer,
    /// Device may request primary or observer access.
    Primary,
}

impl RemoteRoleCeiling {
    /// Returns whether this ceiling permits one requested control role.
    pub(crate) fn permits(self, requested: RequestedRole) -> bool {
        matches!(
            (self, requested),
            (Self::Observer, RequestedRole::Observer)
                | (
                    Self::Primary,
                    RequestedRole::Primary | RequestedRole::Observer
                )
        )
    }

    /// Returns the stable configuration and wire name for this ceiling.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Observer => "observer",
            Self::Primary => "primary",
        }
    }
}

/// Durable, endpoint-bound Mezzanine authorization record.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RemoteTrustRecord {
    /// Stable non-secret record identifier.
    pub id: String,
    /// Iroh endpoint identity authenticated by the transport.
    pub endpoint_id: String,
    /// Server endpoint identity to which this device trust is bound.
    pub server_endpoint_id: String,
    /// User-selected device label.
    pub label: String,
    /// Maximum Mezzanine role the device may request.
    pub role_ceiling: RemoteRoleCeiling,
    /// Record creation time.
    pub created_at_unix_seconds: u64,
    /// Most recent successful trust resolution.
    pub last_used_at_unix_seconds: Option<u64>,
    /// Revocation time, when revoked.
    pub revoked_at_unix_seconds: Option<u64>,
    /// Optional non-secret revocation explanation.
    pub revocation_reason: Option<String>,
    /// Credential contract version retained for future rotation.
    pub credential_version: u32,
    /// Domain-separated verifier for the endpoint-bound device credential.
    pub(super) credential_verifier: String,
}

impl std::fmt::Debug for RemoteTrustRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteTrustRecord")
            .field("id", &self.id)
            .field("endpoint_id", &self.endpoint_id)
            .field("server_endpoint_id", &self.server_endpoint_id)
            .field("label", &self.label)
            .field("role_ceiling", &self.role_ceiling)
            .field("created_at_unix_seconds", &self.created_at_unix_seconds)
            .field("last_used_at_unix_seconds", &self.last_used_at_unix_seconds)
            .field("revoked_at_unix_seconds", &self.revoked_at_unix_seconds)
            .field("revocation_reason", &self.revocation_reason)
            .field("credential_version", &self.credential_version)
            .field("credential_verifier", &"[REDACTED]")
            .finish()
    }
}

impl RemoteTrustRecord {
    /// Returns whether this record is currently revoked.
    pub(crate) fn revoked(&self) -> bool {
        self.revoked_at_unix_seconds.is_some()
    }
}

/// Secret-bearing invitation returned only to its local creator.
#[derive(Clone)]
pub(crate) struct RemotePairingInvitation {
    /// Stable invitation identifier used in audit-safe output.
    pub invitation_id: String,
    /// Single-use bearer token. `SecretString` redacts debug output.
    pub token: SecretString,
    /// Server identity to which the invitation is bound.
    pub server_endpoint_id: String,
    /// Maximum role granted on redemption.
    pub role_ceiling: RemoteRoleCeiling,
    /// Expiration time.
    pub expires_at_unix_seconds: u64,
}

impl std::fmt::Debug for RemotePairingInvitation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemotePairingInvitation")
            .field("invitation_id", &self.invitation_id)
            .field("token", &"[REDACTED]")
            .field("server_endpoint_id", &self.server_endpoint_id)
            .field("role_ceiling", &self.role_ceiling)
            .field("expires_at_unix_seconds", &self.expires_at_unix_seconds)
            .finish()
    }
}

/// Successful first-use pairing result returned only to the redeeming client.
#[derive(Clone)]
pub(crate) struct RemotePairingRedemption {
    /// Durable non-secret trust record created for the endpoint.
    pub record: RemoteTrustRecord,
    /// Endpoint-bound credential required on every later connection.
    pub device_credential: SecretString,
    /// Invitation consumed by this redemption, retained for exact rollback.
    pub(super) invitation_id: String,
    /// Commit timestamp retained for exact rollback.
    pub(super) redeemed_at_unix_seconds: u64,
}

impl std::fmt::Debug for RemotePairingRedemption {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemotePairingRedemption")
            .field("record", &self.record)
            .field("device_credential", &"[REDACTED]")
            .finish()
    }
}

/// Authorized application principal resolved from transport identity and trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemotePrincipal {
    /// Durable trust record that grants authority.
    pub trust_record_id: String,
    /// Authenticated Iroh endpoint identity.
    pub endpoint_id: String,
    /// Role ceiling applied to this principal.
    pub role_ceiling: RemoteRoleCeiling,
    /// Requested role accepted beneath the ceiling.
    pub requested_role: RequestedRole,
}

/// Persisted single-use invitation verifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RemoteInvitationRecord {
    pub id: String,
    pub verifier: String,
    pub server_endpoint_id: String,
    pub role_ceiling: RemoteRoleCeiling,
    pub created_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    pub redeemed_at_unix_seconds: Option<u64>,
    pub redeemed_endpoint_id: Option<String>,
}

/// Versioned trust database persisted as one private atomic document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RemoteTrustDatabase {
    pub version: u32,
    pub records: Vec<RemoteTrustRecord>,
    pub invitations: Vec<RemoteInvitationRecord>,
}

impl Default for RemoteTrustDatabase {
    fn default() -> Self {
        Self {
            version: 1,
            records: Vec::new(),
            invitations: Vec::new(),
        }
    }
}
