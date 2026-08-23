//! Protected persistent identity and profiles for Iroh clients.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use iroh::{EndpointAddr, SecretKey};
use rustix::fs::{FlockOperation, flock};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{MezError, Result};

use super::RemoteRoleCeiling;
use super::store::{
    ensure_private_directory, open_private_file_read, open_private_lock, write_private_atomic,
};

const REMOTE_DIRECTORY_NAME: &str = "remote";
const CLIENT_DIRECTORY_NAME: &str = "client";
const CLIENT_KEY_FILE_NAME: &str = "endpoint.key";
const CLIENT_KEY_LOCK_FILE_NAME: &str = "endpoint.lock";
const PROFILES_FILE_NAME: &str = "profiles.json";
const PROFILES_LOCK_FILE_NAME: &str = "profiles.lock";
const CREDENTIALS_DIRECTORY_NAME: &str = "credentials";
const ENDPOINT_KEY_BYTES: usize = 32;
const MAX_PROFILE_NAME_BYTES: usize = 128;
const MAX_PROFILE_RECORDS: usize = 256;
const MAX_PROFILE_DATABASE_BYTES: u64 = 2 * 1024 * 1024;

/// Reads one protected invitation file with an explicit size bound.
pub(crate) fn read_remote_invitation_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "Iroh invitation file not found",
            )
        } else {
            error.into()
        }
    })?;
    if metadata.len() > max_bytes {
        return Err(MezError::invalid_args(
            "Iroh invitation file exceeds size limit",
        ));
    }
    let file = open_private_file_read(path)?;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(MezError::invalid_args(
            "Iroh invitation file exceeds size limit",
        ));
    }
    Ok(bytes)
}

/// Creates one owner-only invitation file without replacing an existing path.
///
/// The final path is opened with `create_new`, mode `0600`, and no preflight
/// existence check, so an existing regular file or symlink wins the race and
/// remains untouched. A failed write removes only the file created here.
pub(crate) fn write_remote_invitation_file_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            MezError::conflict(format!(
                "Iroh invitation output {} already exists; choose another path",
                path.display()
            ))
        } else {
            error.into()
        }
    })?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        drop(file);
        let _ = fs::remove_file(path);
    }
    result
}

/// Persistent endpoint identity shared by explicit remote client targets.
#[derive(Debug)]
pub(crate) struct RemoteClientIdentity {
    secret_key: SecretKey,
    _lock: fs::File,
}

impl RemoteClientIdentity {
    /// Loads or atomically creates the protected client endpoint key.
    pub(crate) fn load_or_create(config_root: &Path) -> Result<Self> {
        let directory = client_directory(config_root);
        ensure_client_directory_chain(&directory)?;
        let lock = open_private_lock(&directory.join(CLIENT_KEY_LOCK_FILE_NAME))?;
        match flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => {
                return Err(MezError::conflict(
                    "Iroh client endpoint identity is already in use by another live process",
                ));
            }
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
        let key_path = directory.join(CLIENT_KEY_FILE_NAME);
        let secret_key = match fs::symlink_metadata(&key_path) {
            Ok(_) => {
                let mut file = open_private_file_read(&key_path)?;
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                let bytes: [u8; ENDPOINT_KEY_BYTES] = bytes.try_into().map_err(
                    |bytes: Vec<u8>| {
                        MezError::invalid_state(format!(
                            "Iroh client endpoint key must contain exactly {ENDPOINT_KEY_BYTES} bytes, found {}",
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
        Ok(Self {
            secret_key,
            _lock: lock,
        })
    }

    /// Returns the protected key used to bind the client endpoint.
    pub(crate) fn secret_key(&self) -> &SecretKey {
        &self.secret_key
    }

    /// Returns the stable client endpoint identity.
    #[cfg(test)]
    pub(crate) fn endpoint_id(&self) -> iroh::EndpointId {
        self.secret_key.public()
    }
}

/// One durable endpoint-bound remote server profile.
#[derive(Clone)]
pub(crate) struct RemoteClientProfile {
    pub name: String,
    pub server_addr: EndpointAddr,
    pub role: RemoteRoleCeiling,
    pub device_credential: SecretString,
}

/// Redacted local metadata for one endpoint-bound remote server profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RemoteClientProfileSummary {
    /// Client-local alias used by `--iroh-profile`.
    pub name: String,
    /// Abbreviated pinned server endpoint identity for safe display.
    pub server_fingerprint: String,
    /// Maximum role granted by the protected device credential.
    pub role: RemoteRoleCeiling,
    /// Number of persisted direct IP route hints.
    pub direct_route_count: usize,
    /// Number of persisted relay route hints.
    pub relay_route_count: usize,
}

impl std::fmt::Debug for RemoteClientProfile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteClientProfile")
            .field("name", &self.name)
            .field("server_addr", &self.server_addr)
            .field("role", &self.role)
            .field("device_credential", &"[REDACTED]")
            .finish()
    }
}

/// Private profile metadata and separate credential-file persistence.
#[derive(Debug, Clone)]
pub(crate) struct RemoteClientProfileStore {
    directory: PathBuf,
}

impl RemoteClientProfileStore {
    /// Creates the client profile store below the primary config root.
    pub(crate) fn under_config_root(config_root: &Path) -> Self {
        Self {
            directory: client_directory(config_root),
        }
    }

    /// Loads one named profile without exposing its credential in diagnostics.
    pub(crate) fn load(&self, name: &str) -> Result<Option<RemoteClientProfile>> {
        validate_profile_name(name)?;
        ensure_client_directory_chain(&self.directory)?;
        let lock = open_private_lock(&self.directory.join(PROFILES_LOCK_FILE_NAME))?;
        flock(&lock, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        let database = self.load_database()?;
        let Some(stored) = database
            .profiles
            .into_iter()
            .find(|profile| profile.name == name)
        else {
            return Ok(None);
        };
        validate_credential_file_name(&stored.credential_file)?;
        let credential_path = self
            .directory
            .join(CREDENTIALS_DIRECTORY_NAME)
            .join(&stored.credential_file);
        let mut file = open_private_file_read(&credential_path)?;
        let mut credential = String::new();
        file.read_to_string(&mut credential)?;
        if credential.is_empty() {
            return Err(MezError::invalid_state(
                "remote client device credential must not be empty",
            ));
        }
        Ok(Some(RemoteClientProfile {
            name: stored.name,
            server_addr: stored.server_addr,
            role: stored.role,
            device_credential: SecretString::from(credential),
        }))
    }

    /// Lists redacted profile metadata without opening credential files.
    pub(crate) fn list(&self) -> Result<Vec<RemoteClientProfileSummary>> {
        ensure_client_directory_chain(&self.directory)?;
        let lock = open_private_lock(&self.directory.join(PROFILES_LOCK_FILE_NAME))?;
        flock(&lock, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        let database = self.load_database()?;
        Ok(database
            .profiles
            .iter()
            .map(remote_client_profile_summary)
            .collect())
    }

    /// Loads redacted metadata for one named profile.
    pub(crate) fn summary(&self, name: &str) -> Result<Option<RemoteClientProfileSummary>> {
        validate_profile_name(name)?;
        ensure_client_directory_chain(&self.directory)?;
        let lock = open_private_lock(&self.directory.join(PROFILES_LOCK_FILE_NAME))?;
        flock(&lock, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        let database = self.load_database()?;
        Ok(database
            .profiles
            .iter()
            .find(|profile| profile.name == name)
            .map(remote_client_profile_summary))
    }

    /// Rejects a client-local alias already pinned to another server identity.
    ///
    /// This preflight is intentionally performed before invitation redemption
    /// so a local naming conflict cannot consume remote pairing state.
    pub(crate) fn preflight_name_for_server(
        &self,
        name: &str,
        server_endpoint_id: iroh::EndpointId,
    ) -> Result<()> {
        validate_profile_name(name)?;
        ensure_client_directory_chain(&self.directory)?;
        let lock = open_private_lock(&self.directory.join(PROFILES_LOCK_FILE_NAME))?;
        flock(&lock, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        let database = self.load_database()?;
        if let Some(existing) = database
            .profiles
            .iter()
            .find(|profile| profile.name == name)
            && existing.server_addr.id != server_endpoint_id
        {
            return Err(MezError::conflict(format!(
                "remote client profile `{name}` is pinned to a different server identity; choose a different --save-as name"
            )));
        }
        Ok(())
    }

    /// Renames one client-local profile alias without changing its authority.
    pub(crate) fn rename(
        &self,
        current_name: &str,
        new_name: &str,
    ) -> Result<RemoteClientProfileSummary> {
        validate_profile_name(current_name)?;
        validate_profile_name(new_name)?;
        ensure_client_directory_chain(&self.directory)?;
        let lock = open_private_lock(&self.directory.join(PROFILES_LOCK_FILE_NAME))?;
        flock(&lock, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        let mut database = self.load_database()?;
        let current_index = database
            .profiles
            .iter()
            .position(|profile| profile.name == current_name)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    format!("remote client profile `{current_name}` was not found"),
                )
            })?;
        if current_name != new_name
            && database
                .profiles
                .iter()
                .any(|profile| profile.name == new_name)
        {
            return Err(MezError::conflict(format!(
                "remote client profile `{new_name}` already exists"
            )));
        }
        database.profiles[current_index].name = new_name.to_string();
        let summary = remote_client_profile_summary(&database.profiles[current_index]);
        database
            .profiles
            .sort_by(|left, right| left.name.cmp(&right.name));
        self.write_database(&database)?;
        Ok(summary)
    }

    /// Removes one local reconnect profile and its protected credential file.
    ///
    /// This does not revoke the corresponding server-side trusted-client
    /// record; revocation remains a local Unix-control administration action.
    pub(crate) fn remove(&self, name: &str) -> Result<RemoteClientProfileSummary> {
        validate_profile_name(name)?;
        ensure_client_directory_chain(&self.directory)?;
        let lock = open_private_lock(&self.directory.join(PROFILES_LOCK_FILE_NAME))?;
        flock(&lock, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        let mut database = self.load_database()?;
        let index = database
            .profiles
            .iter()
            .position(|profile| profile.name == name)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    format!("remote client profile `{name}` was not found"),
                )
            })?;
        let removed = database.profiles.remove(index);
        validate_credential_file_name(&removed.credential_file)?;
        self.write_database(&database)?;
        let credential_path = self
            .directory
            .join(CREDENTIALS_DIRECTORY_NAME)
            .join(&removed.credential_file);
        match fs::remove_file(credential_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(remote_client_profile_summary(&removed))
    }

    /// Atomically publishes a profile after its credential is safely persisted.
    pub(crate) fn save(&self, profile: &RemoteClientProfile) -> Result<()> {
        validate_profile_name(&profile.name)?;
        if profile.device_credential.expose_secret().is_empty() {
            return Err(MezError::invalid_args(
                "remote client device credential must not be empty",
            ));
        }
        ensure_client_directory_chain(&self.directory)?;
        let lock = open_private_lock(&self.directory.join(PROFILES_LOCK_FILE_NAME))?;
        flock(&lock, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        let credentials = self.directory.join(CREDENTIALS_DIRECTORY_NAME);
        ensure_private_directory(&credentials)?;

        let mut database = self.load_database()?;
        let existing_index = database
            .profiles
            .iter()
            .position(|stored| stored.name == profile.name);
        if let Some(index) = existing_index
            && database.profiles[index].server_addr.id != profile.server_addr.id
        {
            return Err(MezError::conflict(format!(
                "remote client profile `{}` is pinned to a different server identity; the existing profile was preserved, use an invitation with a distinct profile name",
                profile.name
            )));
        }
        if existing_index.is_none() && database.profiles.len() >= MAX_PROFILE_RECORDS {
            return Err(MezError::conflict(
                "remote client profile record limit has been reached",
            ));
        }

        let credential_file = format!(
            "{}-{:016x}.secret",
            profile_storage_key(&profile.name),
            rand::random::<u64>()
        );
        write_private_atomic(
            &credentials.join(&credential_file),
            profile.device_credential.expose_secret().as_bytes(),
        )?;
        let stored = StoredRemoteClientProfile {
            name: profile.name.clone(),
            server_addr: profile.server_addr.clone(),
            role: profile.role,
            credential_file: credential_file.clone(),
        };
        let replaced_credential = if let Some(index) = existing_index {
            Some(std::mem::replace(&mut database.profiles[index], stored).credential_file)
        } else {
            database.profiles.push(stored);
            None
        };
        database
            .profiles
            .sort_by(|left, right| left.name.cmp(&right.name));
        if let Err(error) = self.write_database(&database) {
            let _ = fs::remove_file(credentials.join(&credential_file));
            return Err(error);
        }
        if let Some(replaced) = replaced_credential {
            validate_credential_file_name(&replaced)?;
            let _ = fs::remove_file(credentials.join(replaced));
        }
        Ok(())
    }

    fn load_database(&self) -> Result<RemoteClientProfileDatabase> {
        let path = self.directory.join(PROFILES_FILE_NAME);
        let file = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.len() > MAX_PROFILE_DATABASE_BYTES {
                    return Err(MezError::invalid_state(
                        "remote client profile database exceeds size limit",
                    ));
                }
                open_private_file_read(&path)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RemoteClientProfileDatabase::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_PROFILE_DATABASE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_PROFILE_DATABASE_BYTES {
            return Err(MezError::invalid_state(
                "remote client profile database exceeds size limit",
            ));
        }
        let database: RemoteClientProfileDatabase =
            serde_json::from_slice(&bytes).map_err(|error| {
                MezError::invalid_state(format!("invalid remote client profile database: {error}"))
            })?;
        if database.version != 1 || database.profiles.len() > MAX_PROFILE_RECORDS {
            return Err(MezError::invalid_state(
                "unsupported or oversized remote client profile database",
            ));
        }
        for profile in &database.profiles {
            validate_profile_name(&profile.name)?;
            validate_credential_file_name(&profile.credential_file)?;
        }
        Ok(database)
    }

    fn write_database(&self, database: &RemoteClientProfileDatabase) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(database).map_err(|error| {
            MezError::invalid_state(format!("failed to encode remote client profiles: {error}"))
        })?;
        if bytes.len() as u64 > MAX_PROFILE_DATABASE_BYTES {
            return Err(MezError::invalid_state(
                "remote client profile database exceeds size limit",
            ));
        }
        write_private_atomic(&self.directory.join(PROFILES_FILE_NAME), &bytes)
    }

    #[cfg(test)]
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRemoteClientProfile {
    name: String,
    server_addr: EndpointAddr,
    role: RemoteRoleCeiling,
    credential_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RemoteClientProfileDatabase {
    version: u32,
    profiles: Vec<StoredRemoteClientProfile>,
}

impl Default for RemoteClientProfileDatabase {
    fn default() -> Self {
        Self {
            version: 1,
            profiles: Vec::new(),
        }
    }
}

fn client_directory(config_root: &Path) -> PathBuf {
    config_root
        .join(REMOTE_DIRECTORY_NAME)
        .join(CLIENT_DIRECTORY_NAME)
}

fn ensure_client_directory_chain(directory: &Path) -> Result<()> {
    let remote = directory
        .parent()
        .ok_or_else(|| MezError::invalid_args("remote client path has no parent"))?;
    ensure_private_directory(remote)?;
    ensure_private_directory(directory)
}

fn validate_profile_name(name: &str) -> Result<()> {
    if name.trim().is_empty()
        || name.len() > MAX_PROFILE_NAME_BYTES
        || name.chars().any(char::is_control)
    {
        return Err(MezError::invalid_args(
            "remote client profile name must be printable text up to 128 bytes",
        ));
    }
    Ok(())
}

fn validate_credential_file_name(name: &str) -> Result<()> {
    let path = Path::new(name);
    if name.is_empty()
        || name.len() > 192
        || name.chars().any(char::is_control)
        || path.file_name().and_then(|value| value.to_str()) != Some(name)
    {
        return Err(MezError::forbidden(
            "remote client credential reference is unsafe",
        ));
    }
    Ok(())
}

fn profile_storage_key(name: &str) -> String {
    Sha256::digest(name.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Returns a stable abbreviated display fingerprint for one public endpoint ID.
pub(crate) fn abbreviated_endpoint_fingerprint(endpoint_id: iroh::EndpointId) -> String {
    endpoint_id.to_string().chars().take(12).collect()
}

/// Builds secret-free metadata from one persisted profile record.
fn remote_client_profile_summary(
    profile: &StoredRemoteClientProfile,
) -> RemoteClientProfileSummary {
    let direct_route_count = profile
        .server_addr
        .addrs
        .iter()
        .filter(|addr| matches!(addr, iroh::TransportAddr::Ip(_)))
        .count();
    let relay_route_count = profile
        .server_addr
        .addrs
        .iter()
        .filter(|addr| matches!(addr, iroh::TransportAddr::Relay(_)))
        .count();
    RemoteClientProfileSummary {
        name: profile.name.clone(),
        server_fingerprint: abbreviated_endpoint_fingerprint(profile.server_addr.id),
        role: profile.role,
        direct_route_count,
        relay_route_count,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mez-remote-client-{label}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ))
    }

    #[test]
    fn client_identity_and_profile_are_private_stable_and_redacted() {
        let root = test_root("persistence");
        let first = RemoteClientIdentity::load_or_create(&root).unwrap();
        let endpoint_id = first.endpoint_id();
        let conflict = RemoteClientIdentity::load_or_create(&root).unwrap_err();
        assert_eq!(conflict.kind(), crate::error::MezErrorKind::Conflict);
        drop(first);
        let second = RemoteClientIdentity::load_or_create(&root).unwrap();
        assert_eq!(second.endpoint_id(), endpoint_id);

        let credential = "durable-device-secret";
        let profile = RemoteClientProfile {
            name: "workstation".to_string(),
            server_addr: EndpointAddr::new(SecretKey::generate().public())
                .with_ip_addr("127.0.0.1:45678".parse().unwrap()),
            role: RemoteRoleCeiling::Primary,
            device_credential: SecretString::from(credential.to_string()),
        };
        let store = RemoteClientProfileStore::under_config_root(&root);
        store.save(&profile).unwrap();
        let loaded = store.load("workstation").unwrap().unwrap();
        assert_eq!(loaded.server_addr, profile.server_addr);
        assert_eq!(loaded.role, RemoteRoleCeiling::Primary);
        assert_eq!(loaded.device_credential.expose_secret(), credential);
        assert!(!format!("{loaded:?}").contains(credential));
        let metadata = fs::metadata(store.directory().join(PROFILES_FILE_NAME)).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o077, 0);
        let persisted = fs::read_to_string(store.directory().join(PROFILES_FILE_NAME)).unwrap();
        assert!(!persisted.contains(credential));
        let _ = fs::remove_dir_all(root);
    }

    /// Verifies a successful pairing cannot replace an unrelated same-named
    /// profile after the profile has been pinned to another server identity.
    ///
    /// The collision must fail before credential publication so the original
    /// route, role, credential, and credential-file set remain unchanged.
    #[test]
    fn profile_save_rejects_cross_server_name_collision_without_mutation() {
        let root = test_root("cross-server-profile-collision");
        let store = RemoteClientProfileStore::under_config_root(&root);
        let original = RemoteClientProfile {
            name: "shared-session".to_string(),
            server_addr: EndpointAddr::new(SecretKey::generate().public())
                .with_ip_addr("127.0.0.1:47001".parse().unwrap()),
            role: RemoteRoleCeiling::Observer,
            device_credential: SecretString::from("original-credential".to_string()),
        };
        store.save(&original).unwrap();
        let credentials = store.directory().join(CREDENTIALS_DIRECTORY_NAME);
        let mut credential_files_before = fs::read_dir(&credentials)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        credential_files_before.sort();

        let replacement = RemoteClientProfile {
            name: original.name.clone(),
            server_addr: EndpointAddr::new(SecretKey::generate().public())
                .with_ip_addr("127.0.0.1:47002".parse().unwrap()),
            role: RemoteRoleCeiling::Primary,
            device_credential: SecretString::from("replacement-credential".to_string()),
        };
        let conflict = store.save(&replacement).unwrap_err();
        assert_eq!(conflict.kind(), crate::error::MezErrorKind::Conflict);
        assert!(conflict.message().contains("shared-session"));
        assert!(conflict.message().contains("different server identity"));

        let loaded = store.load(&original.name).unwrap().unwrap();
        assert_eq!(loaded.server_addr, original.server_addr);
        assert_eq!(loaded.role, original.role);
        assert_eq!(
            loaded.device_credential.expose_secret(),
            original.device_credential.expose_secret()
        );
        let mut credential_files_after = fs::read_dir(&credentials)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        credential_files_after.sort();
        assert_eq!(credential_files_after, credential_files_before);
        let _ = fs::remove_dir_all(root);
    }

    /// Verifies retrying or refreshing a pairing for the same pinned server
    /// identity can update mutable route, role, and credential information.
    ///
    /// Publishing the refresh must replace the obsolete credential file only
    /// after the profile database points at the newly persisted credential.
    #[test]
    fn profile_save_refreshes_same_server_and_removes_old_credential() {
        let root = test_root("same-server-profile-refresh");
        let store = RemoteClientProfileStore::under_config_root(&root);
        let server_endpoint_id = SecretKey::generate().public();
        let original = RemoteClientProfile {
            name: "stable-session".to_string(),
            server_addr: EndpointAddr::new(server_endpoint_id)
                .with_ip_addr("127.0.0.1:47101".parse().unwrap()),
            role: RemoteRoleCeiling::Observer,
            device_credential: SecretString::from("old-credential".to_string()),
        };
        store.save(&original).unwrap();

        let refreshed = RemoteClientProfile {
            name: original.name.clone(),
            server_addr: EndpointAddr::new(server_endpoint_id)
                .with_ip_addr("127.0.0.1:47102".parse().unwrap()),
            role: RemoteRoleCeiling::Primary,
            device_credential: SecretString::from("new-credential".to_string()),
        };
        store.save(&refreshed).unwrap();

        let loaded = store.load(&refreshed.name).unwrap().unwrap();
        assert_eq!(loaded.server_addr, refreshed.server_addr);
        assert_eq!(loaded.role, refreshed.role);
        assert_eq!(
            loaded.device_credential.expose_secret(),
            refreshed.device_credential.expose_secret()
        );
        let credentials = store.directory().join(CREDENTIALS_DIRECTORY_NAME);
        let credential_files = fs::read_dir(&credentials).unwrap().collect::<Vec<_>>();
        assert_eq!(credential_files.len(), 1);
        let persisted_credential =
            fs::read_to_string(credential_files[0].as_ref().unwrap().path()).unwrap();
        assert_eq!(persisted_credential, "new-credential");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_profile_saves_preserve_every_record() {
        use std::sync::{Arc, Barrier};

        let root = test_root("concurrent-profiles");
        let barrier = Arc::new(Barrier::new(8));
        let handles = (0..8)
            .map(|index| {
                let root = root.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let profile = RemoteClientProfile {
                        name: format!("profile-{index}"),
                        server_addr: EndpointAddr::new(SecretKey::generate().public())
                            .with_ip_addr(format!("127.0.0.1:{}", 46000 + index).parse().unwrap()),
                        role: RemoteRoleCeiling::Observer,
                        device_credential: SecretString::from(format!("credential-{index}")),
                    };
                    barrier.wait();
                    RemoteClientProfileStore::under_config_root(&root)
                        .save(&profile)
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }
        let store = RemoteClientProfileStore::under_config_root(&root);
        for index in 0..8 {
            let profile = store.load(&format!("profile-{index}")).unwrap().unwrap();
            assert_eq!(
                profile.device_credential.expose_secret(),
                &format!("credential-{index}")
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    /// Verifies local profile management changes aliases without changing
    /// endpoint authority and removes only the selected protected credential.
    ///
    /// List and show results must remain redacted, rename must reject an
    /// existing alias, and remove must leave unrelated profiles usable.
    #[test]
    fn profile_management_is_redacted_collision_safe_and_local() {
        let root = test_root("profile-management");
        let store = RemoteClientProfileStore::under_config_root(&root);
        let first = RemoteClientProfile {
            name: "home".to_string(),
            server_addr: EndpointAddr::new(SecretKey::generate().public())
                .with_ip_addr("127.0.0.1:47201".parse().unwrap()),
            role: RemoteRoleCeiling::Primary,
            device_credential: SecretString::from("home-secret".to_string()),
        };
        let second = RemoteClientProfile {
            name: "office".to_string(),
            server_addr: EndpointAddr::new(SecretKey::generate().public())
                .with_relay_url("https://relay.example".parse().unwrap()),
            role: RemoteRoleCeiling::Observer,
            device_credential: SecretString::from("office-secret".to_string()),
        };
        store.save(&first).unwrap();
        store.save(&second).unwrap();

        let summaries = store.list().unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].name, "home");
        assert_eq!(summaries[0].direct_route_count, 1);
        assert_eq!(summaries[1].relay_route_count, 1);
        assert!(!format!("{summaries:?}").contains("secret"));

        let collision = store.rename("home", "office").unwrap_err();
        assert_eq!(collision.kind(), crate::error::MezErrorKind::Conflict);
        let renamed = store.rename("home", "home-mez").unwrap();
        assert_eq!(renamed.name, "home-mez");
        assert_eq!(
            store
                .load("home-mez")
                .unwrap()
                .unwrap()
                .device_credential
                .expose_secret(),
            "home-secret"
        );

        let removed = store.remove("home-mez").unwrap();
        assert_eq!(removed.name, "home-mez");
        assert!(store.load("home-mez").unwrap().is_none());
        assert!(store.load("office").unwrap().is_some());
        let credentials = fs::read_dir(store.directory().join(CREDENTIALS_DIRECTORY_NAME))
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(credentials.len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    /// Verifies secure invitation output is owner-only and never replaces an
    /// existing file or symlink target.
    ///
    /// Invitation output contains a bearer token, so no-overwrite creation and
    /// restrictive permissions are required independently of the caller's
    /// process umask.
    #[test]
    fn invitation_output_is_private_and_refuses_overwrite() {
        let root = test_root("invitation-output");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("invite.json");

        write_remote_invitation_file_new(&path, br#"{"token":"secret"}"#).unwrap();

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read(&path).unwrap(), br#"{"token":"secret"}"#);
        let conflict = write_remote_invitation_file_new(&path, b"replacement").unwrap_err();
        assert_eq!(conflict.kind(), crate::error::MezErrorKind::Conflict);
        assert_eq!(fs::read(&path).unwrap(), br#"{"token":"secret"}"#);
        let _ = fs::remove_dir_all(root);
    }
}
