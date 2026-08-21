//! Private filesystem persistence for Iroh identity and remote trust.
//!
//! Endpoint keys and invitation tokens are security-sensitive. The endpoint
//! key is persisted as a private fixed-size file while invitation persistence
//! retains only a SHA-256 verifier. Advisory locks serialize trust mutations,
//! and the endpoint identity lock is retained for the lifetime of a server so
//! two live processes cannot use the same Iroh identity concurrently.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use base64::Engine;
use iroh::SecretKey;
use rand::Rng;
use rustix::fs::{FlockOperation, Mode, OFlags, flock, open};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};

use crate::control::RequestedRole;
use crate::error::{MezError, MezErrorKind, Result};
use crate::runtime::current_effective_uid;

use super::types::{
    RemoteInvitationRecord, RemotePairingInvitation, RemotePairingRedemption, RemotePrincipal,
    RemoteRoleCeiling, RemoteTrustDatabase, RemoteTrustRecord,
};

const REMOTE_DIRECTORY_NAME: &str = "remote";
const ENDPOINT_KEY_FILE_NAME: &str = "endpoint.key";
const ENDPOINT_LOCK_FILE_NAME: &str = "endpoint.lock";
const TRUST_FILE_NAME: &str = "trust.json";
const TRUST_LOCK_FILE_NAME: &str = "trust.lock";
const ENDPOINT_KEY_BYTES: usize = 32;
const INVITATION_TOKEN_BYTES: usize = 32;
const MAX_LABEL_BYTES: usize = 128;
const MAX_REVOCATION_REASON_BYTES: usize = 512;
const MAX_INVITATION_RECORDS: usize = 256;
const MAX_TRUST_RECORDS: usize = 256;
pub(super) const MAX_TRUST_DATABASE_BYTES: u64 = 2 * 1024 * 1024;
const REDEMPTION_ROLLBACK_GRACE_SECONDS: u64 = 60;

/// Persistent Iroh endpoint key with an exclusive live-use lock.
#[derive(Debug)]
pub(crate) struct RemoteEndpointIdentity {
    #[allow(
        dead_code,
        reason = "the pinned key is consumed by the follow-up Iroh listener issue"
    )]
    secret_key: SecretKey,
    endpoint_id: String,
    _lock: fs::File,
}

impl RemoteEndpointIdentity {
    /// Loads or atomically creates the endpoint key under one config root.
    ///
    /// The returned value retains a nonblocking exclusive lock. A second live
    /// process attempting to use the same key receives a conflict error.
    pub(crate) fn load_or_create(config_root: &Path, session_id: &str) -> Result<Self> {
        let directory = session_remote_directory(config_root, session_id)?;
        ensure_remote_directory_chain(&directory)?;
        let lock_path = directory.join(ENDPOINT_LOCK_FILE_NAME);
        let lock = open_private_lock(&lock_path)?;
        match flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => {
                return Err(MezError::conflict(
                    "Iroh endpoint identity is already in use by another live process",
                ));
            }
            Err(error) => return Err(std::io::Error::from(error).into()),
        }

        let key_path = directory.join(ENDPOINT_KEY_FILE_NAME);
        let secret_key = match fs::symlink_metadata(&key_path) {
            Ok(_) => {
                let mut file = open_private_file_read(&key_path)?;
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                let bytes: [u8; ENDPOINT_KEY_BYTES] = bytes.try_into().map_err(
                    |bytes: Vec<u8>| {
                        MezError::invalid_state(format!(
                            "Iroh endpoint key must contain exactly {ENDPOINT_KEY_BYTES} bytes, found {}",
                            bytes.len()
                        ))
                    },
                )?;
                SecretKey::from_bytes(&bytes)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let key = SecretKey::generate();
                write_private_atomic(&key_path, &key.to_bytes())?;
                key
            }
            Err(error) => return Err(error.into()),
        };
        let endpoint_id = secret_key.public().to_string();
        Ok(Self {
            secret_key,
            endpoint_id,
            _lock: lock,
        })
    }

    /// Returns the secret key for constructing the Iroh endpoint.
    #[allow(
        dead_code,
        reason = "the accessor is consumed by the follow-up Iroh listener issue"
    )]
    pub(crate) fn secret_key(&self) -> &SecretKey {
        &self.secret_key
    }

    /// Returns the stable public endpoint identity.
    pub(crate) fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }
}

/// Durable endpoint-bound trust and single-use invitation store.
#[derive(Debug, Clone)]
pub(crate) struct RemoteTrustStore {
    directory: PathBuf,
}

/// Validated invitation material that has not yet consumed persistent state.
#[derive(Clone)]
pub(crate) struct RemotePairingPreparation {
    invitation_id: String,
    invitation_verifier: String,
    record: RemoteTrustRecord,
    requested_role: RequestedRole,
    redeemed_at_unix_seconds: u64,
    device_credential: SecretString,
}

impl std::fmt::Debug for RemotePairingPreparation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemotePairingPreparation")
            .field("invitation_id", &self.invitation_id)
            .field("invitation_verifier", &"[REDACTED]")
            .field("record", &self.record)
            .field("requested_role", &self.requested_role)
            .field("redeemed_at_unix_seconds", &self.redeemed_at_unix_seconds)
            .field("device_credential", &"[REDACTED]")
            .finish()
    }
}

impl RemotePairingPreparation {
    /// Returns the provisional principal used only against cloned initialize state.
    pub(crate) fn principal(&self) -> RemotePrincipal {
        RemotePrincipal {
            trust_record_id: self.record.id.clone(),
            endpoint_id: self.record.endpoint_id.clone(),
            role_ceiling: self.record.role_ceiling,
            requested_role: self.requested_role,
        }
    }
}

impl RemoteTrustStore {
    /// Creates a store beneath the primary-user configuration root.
    pub(crate) fn under_config_root(config_root: &Path, session_id: &str) -> Result<Self> {
        Ok(Self {
            directory: session_remote_directory(config_root, session_id)?,
        })
    }

    #[cfg(test)]
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    /// Creates one short-lived, single-use invitation.
    pub(crate) fn create_invitation(
        &self,
        server_endpoint_id: &str,
        role_ceiling: RemoteRoleCeiling,
        ttl_seconds: u64,
        now_unix_seconds: u64,
    ) -> Result<RemotePairingInvitation> {
        validate_endpoint_id(server_endpoint_id)?;
        if !(30..=86_400).contains(&ttl_seconds) {
            return Err(MezError::invalid_args(
                "remote invitation TTL must be from 30 to 86400 seconds",
            ));
        }
        let invitation_id = random_identifier("invite", 16);
        let token = random_token();
        let expires_at_unix_seconds = now_unix_seconds
            .checked_add(ttl_seconds)
            .ok_or_else(|| MezError::invalid_args("remote invitation expiration overflow"))?;
        let verifier = invitation_verifier(token.expose_secret(), server_endpoint_id, role_ceiling);
        self.mutate_database(|database| {
            prune_stale_invitations(database, now_unix_seconds);
            if database.invitations.len() >= MAX_INVITATION_RECORDS {
                return Err(MezError::conflict(
                    "remote invitation record limit has been reached",
                ));
            }
            database.invitations.push(RemoteInvitationRecord {
                id: invitation_id.clone(),
                verifier,
                server_endpoint_id: server_endpoint_id.to_string(),
                role_ceiling,
                created_at_unix_seconds: now_unix_seconds,
                expires_at_unix_seconds,
                redeemed_at_unix_seconds: None,
                redeemed_endpoint_id: None,
            });
            Ok(())
        })?;
        Ok(RemotePairingInvitation {
            invitation_id,
            token,
            server_endpoint_id: server_endpoint_id.to_string(),
            role_ceiling,
            expires_at_unix_seconds,
        })
    }

    /// Validates an invitation and prepares trust material without mutating persistence.
    pub(crate) fn prepare_invitation(
        &self,
        token: &SecretString,
        server_endpoint_id: &str,
        client_endpoint_id: &str,
        label: &str,
        requested_role: RequestedRole,
        now_unix_seconds: u64,
    ) -> Result<RemotePairingPreparation> {
        validate_endpoint_id(server_endpoint_id)?;
        validate_endpoint_id(client_endpoint_id)?;
        validate_label(label)?;
        let verifier_and_role = self.with_locked_database(|database| {
            let invitation = database
                .invitations
                .iter()
                .find(|invitation| {
                    invitation.server_endpoint_id == server_endpoint_id
                        && invitation.verifier
                            == invitation_verifier(
                                token.expose_secret(),
                                server_endpoint_id,
                                invitation.role_ceiling,
                            )
                })
                .ok_or_else(|| MezError::forbidden("remote pairing invitation is invalid"))?;
            validate_invitation_for_redemption(
                invitation,
                &database,
                client_endpoint_id,
                requested_role,
                now_unix_seconds,
            )?;
            Ok((
                invitation.id.clone(),
                invitation.verifier.clone(),
                invitation.role_ceiling,
            ))
        })?;
        let (invitation_id, invitation_verifier, role_ceiling) = verifier_and_role;
        let record_id = random_identifier("remote", 16);
        let device_credential = random_token();
        let record = RemoteTrustRecord {
            id: record_id.clone(),
            endpoint_id: client_endpoint_id.to_string(),
            server_endpoint_id: server_endpoint_id.to_string(),
            label: label.to_string(),
            role_ceiling,
            created_at_unix_seconds: now_unix_seconds,
            last_used_at_unix_seconds: None,
            revoked_at_unix_seconds: None,
            revocation_reason: None,
            credential_version: 1,
            credential_verifier: device_credential_verifier(
                device_credential.expose_secret(),
                server_endpoint_id,
                client_endpoint_id,
                &record_id,
            ),
        };
        Ok(RemotePairingPreparation {
            invitation_id,
            invitation_verifier,
            record,
            requested_role,
            redeemed_at_unix_seconds: now_unix_seconds,
            device_credential,
        })
    }

    /// Atomically commits one previously validated invitation preparation.
    pub(crate) fn commit_invitation(
        &self,
        preparation: RemotePairingPreparation,
        now_unix_seconds: u64,
    ) -> Result<RemotePairingRedemption> {
        self.mutate_database(|database| {
            let invitation_index = database
                .invitations
                .iter()
                .position(|invitation| {
                    invitation.id == preparation.invitation_id
                        && invitation.verifier == preparation.invitation_verifier
                        && invitation.server_endpoint_id == preparation.record.server_endpoint_id
                })
                .ok_or_else(|| MezError::forbidden("remote pairing invitation is invalid"))?;
            {
                let invitation = &database.invitations[invitation_index];
                validate_invitation_for_redemption(
                    invitation,
                    database,
                    &preparation.record.endpoint_id,
                    preparation.requested_role,
                    now_unix_seconds,
                )?;
                if invitation.role_ceiling != preparation.record.role_ceiling {
                    return Err(MezError::forbidden(
                        "remote pairing invitation role changed before redemption",
                    ));
                }
            }
            if database.records.len() >= MAX_TRUST_RECORDS {
                return Err(MezError::conflict(
                    "remote trust record limit has been reached",
                ));
            }
            let invitation = &mut database.invitations[invitation_index];
            invitation.redeemed_at_unix_seconds = Some(now_unix_seconds);
            invitation.redeemed_endpoint_id = Some(preparation.record.endpoint_id.clone());
            database.records.push(preparation.record.clone());
            Ok(RemotePairingRedemption {
                record: preparation.record,
                device_credential: preparation.device_credential,
                invitation_id: preparation.invitation_id,
                redeemed_at_unix_seconds: now_unix_seconds,
            })
        })
    }

    /// Atomically redeems an invitation and creates an endpoint trust record.
    #[cfg(test)]
    pub(crate) fn redeem_invitation(
        &self,
        token: &SecretString,
        server_endpoint_id: &str,
        client_endpoint_id: &str,
        label: &str,
        requested_role: RequestedRole,
        now_unix_seconds: u64,
    ) -> Result<RemotePairingRedemption> {
        let preparation = self.prepare_invitation(
            token,
            server_endpoint_id,
            client_endpoint_id,
            label,
            requested_role,
            now_unix_seconds,
        )?;
        self.commit_invitation(preparation, now_unix_seconds)
    }

    /// Restores an invitation when later initialization side effects fail.
    pub(crate) fn rollback_invitation_redemption(
        &self,
        redemption: &RemotePairingRedemption,
    ) -> Result<()> {
        self.mutate_database(|database| {
            let invitation = database
                .invitations
                .iter_mut()
                .find(|invitation| invitation.id == redemption.invitation_id)
                .ok_or_else(|| MezError::invalid_state("redeemed invitation is missing"))?;
            if invitation.redeemed_at_unix_seconds != Some(redemption.redeemed_at_unix_seconds)
                || invitation.redeemed_endpoint_id.as_deref()
                    != Some(redemption.record.endpoint_id.as_str())
            {
                return Err(MezError::invalid_state(
                    "redeemed invitation changed before rollback",
                ));
            }
            let record_index = database
                .records
                .iter()
                .position(|record| record == &redemption.record)
                .ok_or_else(|| {
                    MezError::invalid_state("redeemed trust record changed before rollback")
                })?;
            database.records.remove(record_index);
            invitation.redeemed_at_unix_seconds = None;
            invitation.redeemed_endpoint_id = None;
            Ok(())
        })
    }

    /// Validates durable device authority without updating usage metadata.
    pub(crate) fn validate_principal(
        &self,
        server_endpoint_id: &str,
        endpoint_id: &str,
        device_credential: &SecretString,
        requested_role: RequestedRole,
    ) -> Result<RemotePrincipal> {
        validate_endpoint_id(endpoint_id)?;
        self.with_locked_database(|database| {
            let record = database
                .records
                .iter()
                .find(|record| record.endpoint_id == endpoint_id)
                .ok_or_else(|| MezError::forbidden("remote endpoint is not paired"))?;
            validate_device_record(
                record,
                server_endpoint_id,
                endpoint_id,
                device_credential,
                requested_role,
            )?;
            Ok(RemotePrincipal {
                trust_record_id: record.id.clone(),
                endpoint_id: endpoint_id.to_string(),
                role_ceiling: record.role_ceiling,
                requested_role,
            })
        })
    }

    /// Resolves an authenticated endpoint to role-limited Mezzanine authority.
    pub(crate) fn resolve_principal(
        &self,
        server_endpoint_id: &str,
        endpoint_id: &str,
        device_credential: &SecretString,
        requested_role: RequestedRole,
        now_unix_seconds: u64,
    ) -> Result<RemotePrincipal> {
        validate_endpoint_id(endpoint_id)?;
        self.mutate_database(|database| {
            let record = database
                .records
                .iter_mut()
                .find(|record| record.endpoint_id == endpoint_id)
                .ok_or_else(|| MezError::forbidden("remote endpoint is not paired"))?;
            if record.revoked() {
                return Err(MezError::forbidden("remote endpoint trust is revoked"));
            }
            if record.server_endpoint_id != server_endpoint_id {
                return Err(MezError::forbidden(
                    "remote device trust is bound to a different server identity",
                ));
            }
            let verifier = device_credential_verifier(
                device_credential.expose_secret(),
                &record.server_endpoint_id,
                endpoint_id,
                &record.id,
            );
            if !constant_time_equal(verifier.as_bytes(), record.credential_verifier.as_bytes()) {
                return Err(MezError::forbidden("remote device credential is invalid"));
            }
            if !record.role_ceiling.permits(requested_role) {
                return Err(MezError::forbidden(
                    "remote endpoint requested a role above its trust ceiling",
                ));
            }
            record.last_used_at_unix_seconds = Some(now_unix_seconds);
            Ok(RemotePrincipal {
                trust_record_id: record.id.clone(),
                endpoint_id: endpoint_id.to_string(),
                role_ceiling: record.role_ceiling,
                requested_role,
            })
        })
    }

    /// Returns all durable trust records ordered by stable ID.
    pub(crate) fn list_records(&self) -> Result<Vec<RemoteTrustRecord>> {
        self.with_locked_database(|database| {
            let mut records = database.records;
            records.sort_by(|left, right| left.id.cmp(&right.id));
            Ok(records)
        })
    }

    /// Renames one paired device.
    pub(crate) fn rename_record(&self, record_id: &str, label: &str) -> Result<RemoteTrustRecord> {
        validate_label(label)?;
        self.mutate_database(|database| {
            let record = find_record_mut(database, record_id)?;
            record.label = label.to_string();
            Ok(record.clone())
        })
    }

    /// Revokes one paired device without deleting its audit-safe history.
    pub(crate) fn revoke_record(
        &self,
        record_id: &str,
        reason: Option<&str>,
        now_unix_seconds: u64,
    ) -> Result<RemoteTrustRecord> {
        if let Some(reason) = reason {
            validate_reason(reason)?;
        }
        self.mutate_database(|database| {
            let record = find_record_mut(database, record_id)?;
            if record.revoked() {
                return Err(MezError::conflict("remote trust record is already revoked"));
            }
            record.revoked_at_unix_seconds = Some(now_unix_seconds);
            record.revocation_reason = reason.map(str::to_string);
            Ok(record.clone())
        })
    }

    fn mutate_database<T>(
        &self,
        operation: impl FnOnce(&mut RemoteTrustDatabase) -> Result<T>,
    ) -> Result<T> {
        ensure_remote_directory_chain(&self.directory)?;
        let lock = open_private_lock(&self.directory.join(TRUST_LOCK_FILE_NAME))?;
        flock(&lock, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        let mut database = self.load_database()?;
        let result = operation(&mut database)?;
        self.write_database(&database)?;
        Ok(result)
    }

    fn with_locked_database<T>(
        &self,
        operation: impl FnOnce(RemoteTrustDatabase) -> Result<T>,
    ) -> Result<T> {
        ensure_remote_directory_chain(&self.directory)?;
        let lock = open_private_lock(&self.directory.join(TRUST_LOCK_FILE_NAME))?;
        flock(&lock, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        operation(self.load_database()?)
    }

    fn load_database(&self) -> Result<RemoteTrustDatabase> {
        let path = self.directory.join(TRUST_FILE_NAME);
        let file = match fs::symlink_metadata(&path) {
            Ok(_) => open_private_file_read(&path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RemoteTrustDatabase::default());
            }
            Err(error) => return Err(error.into()),
        };
        if file.metadata()?.len() > MAX_TRUST_DATABASE_BYTES {
            return Err(MezError::invalid_state(
                "remote trust database exceeds the protected size limit",
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_TRUST_DATABASE_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_TRUST_DATABASE_BYTES {
            return Err(MezError::invalid_state(
                "remote trust database exceeds the protected size limit",
            ));
        }
        let database: RemoteTrustDatabase = serde_json::from_slice(&bytes).map_err(|error| {
            MezError::invalid_state(format!("remote trust database is malformed: {error}"))
        })?;
        if database.version != 1 {
            return Err(MezError::invalid_state(format!(
                "unsupported remote trust database version {}",
                database.version
            )));
        }
        Ok(database)
    }

    fn write_database(&self, database: &RemoteTrustDatabase) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(database).map_err(|error| {
            MezError::invalid_state(format!("failed to encode remote trust database: {error}"))
        })?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_TRUST_DATABASE_BYTES {
            return Err(MezError::invalid_state(
                "remote trust database exceeds the protected size limit",
            ));
        }
        write_private_atomic(&self.directory.join(TRUST_FILE_NAME), &bytes)
    }
}

fn prune_stale_invitations(database: &mut RemoteTrustDatabase, now_unix_seconds: u64) {
    database.invitations.retain(|invitation| {
        if let Some(redeemed_at) = invitation.redeemed_at_unix_seconds {
            return now_unix_seconds.saturating_sub(redeemed_at)
                <= REDEMPTION_ROLLBACK_GRACE_SECONDS;
        }
        now_unix_seconds <= invitation.expires_at_unix_seconds
    });
}

fn validate_device_record(
    record: &RemoteTrustRecord,
    server_endpoint_id: &str,
    endpoint_id: &str,
    device_credential: &SecretString,
    requested_role: RequestedRole,
) -> Result<()> {
    if record.revoked() {
        return Err(MezError::forbidden("remote endpoint trust is revoked"));
    }
    if record.server_endpoint_id != server_endpoint_id {
        return Err(MezError::forbidden(
            "remote device trust is bound to a different server identity",
        ));
    }
    let verifier = device_credential_verifier(
        device_credential.expose_secret(),
        &record.server_endpoint_id,
        endpoint_id,
        &record.id,
    );
    if !constant_time_equal(verifier.as_bytes(), record.credential_verifier.as_bytes()) {
        return Err(MezError::forbidden("remote device credential is invalid"));
    }
    if !record.role_ceiling.permits(requested_role) {
        return Err(MezError::forbidden(
            "remote endpoint requested a role above its trust ceiling",
        ));
    }
    Ok(())
}

fn validate_invitation_for_redemption(
    invitation: &RemoteInvitationRecord,
    database: &RemoteTrustDatabase,
    client_endpoint_id: &str,
    requested_role: RequestedRole,
    now_unix_seconds: u64,
) -> Result<()> {
    if !invitation.role_ceiling.permits(requested_role) {
        return Err(MezError::forbidden(
            "remote pairing invitation does not permit the requested role",
        ));
    }
    if invitation.redeemed_at_unix_seconds.is_some() {
        return Err(MezError::conflict(
            "remote pairing invitation was already redeemed",
        ));
    }
    if now_unix_seconds > invitation.expires_at_unix_seconds {
        return Err(MezError::forbidden("remote pairing invitation has expired"));
    }
    if database
        .records
        .iter()
        .any(|record| record.endpoint_id == client_endpoint_id && !record.revoked())
    {
        return Err(MezError::conflict(
            "remote endpoint already has an active trust record",
        ));
    }
    Ok(())
}

fn find_record_mut<'a>(
    database: &'a mut RemoteTrustDatabase,
    record_id: &str,
) -> Result<&'a mut RemoteTrustRecord> {
    database
        .records
        .iter_mut()
        .find(|record| record.id == record_id)
        .ok_or_else(|| MezError::new(MezErrorKind::NotFound, "remote trust record not found"))
}

fn session_remote_directory(config_root: &Path, session_id: &str) -> Result<PathBuf> {
    if session_id.trim().is_empty()
        || session_id.len() > 512
        || session_id.chars().any(char::is_control)
    {
        return Err(MezError::invalid_args(
            "remote session id must be non-empty printable text",
        ));
    }
    let digest = Sha256::digest(session_id.as_bytes());
    let key = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(config_root
        .join(REMOTE_DIRECTORY_NAME)
        .join("sessions")
        .join(key))
}

fn ensure_remote_directory_chain(directory: &Path) -> Result<()> {
    let sessions = directory
        .parent()
        .ok_or_else(|| MezError::invalid_args("remote session path has no sessions directory"))?;
    let remote = sessions
        .parent()
        .ok_or_else(|| MezError::invalid_args("remote session path has no remote directory"))?;
    ensure_private_directory(remote)?;
    ensure_private_directory(sessions)?;
    ensure_private_directory(directory)
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_directory_metadata(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            let metadata = fs::symlink_metadata(path)?;
            validate_private_directory_metadata(path, &metadata)
        }
        Err(error) => Err(error.into()),
    }
}

fn validate_private_directory_metadata(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(MezError::forbidden(format!(
            "remote security path {} must be a private directory",
            path.display()
        )));
    }
    validate_owner_and_mode(path, metadata, true)
}

fn validate_private_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    validate_private_file_metadata(path, &metadata)
}

fn validate_private_file_metadata(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(MezError::forbidden(format!(
            "remote security path {} must be a private regular file",
            path.display()
        )));
    }
    validate_owner_and_mode(path, metadata, false)
}

fn open_private_file_read(path: &Path) -> Result<fs::File> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let file = fs::File::from(descriptor);
    validate_private_file_metadata(path, &file.metadata()?)?;
    Ok(file)
}

fn validate_owner_and_mode(path: &Path, metadata: &fs::Metadata, directory: bool) -> Result<()> {
    if metadata.uid() != current_effective_uid() {
        return Err(MezError::forbidden(format!(
            "remote security path {} has unsafe ownership",
            path.display()
        )));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 || (directory && mode & 0o100 == 0) {
        return Err(MezError::forbidden(format!(
            "remote security path {} must be user-private",
            path.display()
        )));
    }
    Ok(())
}

fn open_private_lock(path: &Path) -> Result<fs::File> {
    let descriptor = open(
        path,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(std::io::Error::from)?;
    let file = fs::File::from(descriptor);
    validate_private_file_metadata(path, &file.metadata()?)?;
    Ok(file)
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| MezError::invalid_args("remote security path has no parent"))?;
    ensure_private_directory(parent)?;
    if path.exists() {
        validate_private_file(path)?;
    }
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("remote"),
        random_identifier("write", 8)
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        validate_private_file(path)?;
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn validate_endpoint_id(endpoint_id: &str) -> Result<()> {
    endpoint_id
        .parse::<iroh::EndpointId>()
        .map(|_| ())
        .map_err(|_| MezError::invalid_args("invalid Iroh endpoint identity"))
}

fn validate_label(label: &str) -> Result<()> {
    if label.trim().is_empty()
        || label.len() > MAX_LABEL_BYTES
        || label.chars().any(char::is_control)
    {
        return Err(MezError::invalid_args(
            "remote device label must be non-empty printable text up to 128 bytes",
        ));
    }
    Ok(())
}

fn validate_reason(reason: &str) -> Result<()> {
    if reason.len() > MAX_REVOCATION_REASON_BYTES || reason.chars().any(char::is_control) {
        return Err(MezError::invalid_args(
            "remote revocation reason must be printable text up to 512 bytes",
        ));
    }
    Ok(())
}

fn random_token() -> SecretString {
    let mut bytes = [0u8; INVITATION_TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    SecretString::from(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

fn random_identifier(prefix: &str, byte_count: usize) -> String {
    let mut bytes = vec![0u8; byte_count];
    rand::rng().fill_bytes(&mut bytes);
    format!(
        "{prefix}-{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

fn invitation_verifier(
    token: &str,
    server_endpoint_id: &str,
    role_ceiling: RemoteRoleCeiling,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"mezzanine-iroh-invitation-v1\0");
    digest.update(server_endpoint_id.as_bytes());
    digest.update(b"\0");
    digest.update(role_ceiling.as_str().as_bytes());
    digest.update(b"\0");
    digest.update(token.as_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn device_credential_verifier(
    credential: &str,
    server_endpoint_id: &str,
    client_endpoint_id: &str,
    record_id: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"mezzanine-iroh-device-v1\0");
    digest.update(server_endpoint_id.as_bytes());
    digest.update(b"\0");
    digest.update(client_endpoint_id.as_bytes());
    digest.update(b"\0");
    digest.update(record_id.as_bytes());
    digest.update(b"\0");
    digest.update(credential.as_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}
