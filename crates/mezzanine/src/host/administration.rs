//! Durable replay state for security-sensitive host administration.
//!
//! The protected Unix host boundary reserves an idempotency key before
//! mutating trust or lease authority. Completed outcomes survive process
//! restart, while a pending entry allows operation-specific idempotent
//! reconciliation after interruption between mutation, audit, and response.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{FlockOperation, Mode, OFlags, flock, open};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{MezError, MezErrorKind, Result};
use crate::runtime::current_effective_uid;
use crate::security::audit::AuditLog;

const JOURNAL_DIRECTORY_NAME: &str = "host-administration";
const JOURNAL_FILE_NAME: &str = "replay.json";
const JOURNAL_LOCK_FILE_NAME: &str = "replay.lock";
const JOURNAL_VERSION: u32 = 1;
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_JOURNAL_ENTRIES: usize = 1024;
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 256;
const MAX_METHOD_BYTES: usize = 128;
const MAX_TARGET_BYTES: usize = 512;
const MAX_ERROR_MESSAGE_BYTES: usize = 2048;
static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(1);

/// Shared serialized audit writer used by local and Iroh host front doors.
pub(crate) type HostAuditLog = std::sync::Arc<std::sync::Mutex<Option<AuditLog>>>;

/// Durable replay repository below one protected host config root.
#[derive(Debug, Clone)]
pub(crate) struct HostAdministrationJournal {
    directory: PathBuf,
}

/// Result of reserving one method-scoped administration idempotency key.
#[derive(Debug)]
pub(crate) enum HostAdministrationBegin {
    Fresh {
        request_fingerprint: String,
        previous_generation: Option<u64>,
    },
    Pending {
        request_fingerprint: String,
        previous_generation: Option<u64>,
    },
    Replay(HostAdministrationReplay),
}

/// Completed outcome returned for an exact durable replay.
#[derive(Debug)]
pub(crate) enum HostAdministrationReplay {
    Success(Value),
    Failure(MezError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HostAdministrationDatabase {
    version: u32,
    entries: Vec<HostAdministrationEntry>,
}

impl Default for HostAdministrationDatabase {
    fn default() -> Self {
        Self {
            version: JOURNAL_VERSION,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HostAdministrationEntry {
    key_hash: String,
    method: String,
    request_fingerprint: String,
    actor_id: String,
    target: Option<String>,
    previous_generation: Option<u64>,
    new_generation: Option<u64>,
    created_at_unix_seconds: u64,
    completed_at_unix_seconds: Option<u64>,
    outcome: Option<PersistedHostAdministrationOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PersistedHostAdministrationOutcome {
    Success { response: Value },
    Failure { error_kind: String, message: String },
}

impl HostAdministrationJournal {
    pub(crate) fn under_config_root(config_root: &Path) -> Self {
        Self {
            directory: config_root.join(JOURNAL_DIRECTORY_NAME),
        }
    }

    /// Reserves or replays one exact mutating administration request.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn begin(
        &self,
        idempotency_key: &str,
        method: &str,
        params: &serde_json::Map<String, Value>,
        actor_id: &str,
        target: Option<&str>,
        previous_generation: Option<u64>,
        now_unix_seconds: u64,
    ) -> Result<HostAdministrationBegin> {
        validate_idempotency_key(idempotency_key)?;
        validate_bounded_text(method, "host administration method", MAX_METHOD_BYTES)?;
        validate_optional_bounded_text(target, "host administration target", MAX_TARGET_BYTES)?;
        let key_hash = administration_key_hash(idempotency_key);
        let request_fingerprint = administration_request_fingerprint(method, params)?;
        self.mutate_database(|database| {
            if let Some(entry) = database
                .entries
                .iter()
                .find(|entry| entry.key_hash == key_hash)
            {
                if entry.method != method || entry.request_fingerprint != request_fingerprint {
                    return Err(MezError::conflict(
                        "host administration idempotency key was reused with different request data",
                    ));
                }
                return match entry.outcome.clone() {
                    Some(outcome) => Ok(HostAdministrationBegin::Replay(outcome.into_replay()?)),
                    None => Ok(HostAdministrationBegin::Pending {
                        request_fingerprint,
                        previous_generation: entry.previous_generation,
                    }),
                };
            }
            prune_completed_entries(database);
            if database.entries.len() >= MAX_JOURNAL_ENTRIES {
                return Err(MezError::conflict(
                    "host administration replay journal is full",
                ));
            }
            database.entries.push(HostAdministrationEntry {
                key_hash,
                method: method.to_string(),
                request_fingerprint: request_fingerprint.clone(),
                actor_id: actor_id.to_string(),
                target: target.map(str::to_string),
                previous_generation,
                new_generation: None,
                created_at_unix_seconds: now_unix_seconds,
                completed_at_unix_seconds: None,
                outcome: None,
            });
            Ok(HostAdministrationBegin::Fresh {
                request_fingerprint,
                previous_generation,
            })
        })
    }

    pub(crate) fn complete_success(
        &self,
        idempotency_key: &str,
        request_fingerprint: &str,
        response: Value,
        new_generation: Option<u64>,
        now_unix_seconds: u64,
    ) -> Result<()> {
        self.complete(
            idempotency_key,
            request_fingerprint,
            PersistedHostAdministrationOutcome::Success { response },
            new_generation,
            now_unix_seconds,
        )
    }

    pub(crate) fn complete_failure(
        &self,
        idempotency_key: &str,
        request_fingerprint: &str,
        error: &MezError,
        now_unix_seconds: u64,
    ) -> Result<()> {
        let mut message = error.message().to_string();
        if message.len() > MAX_ERROR_MESSAGE_BYTES {
            message.truncate(MAX_ERROR_MESSAGE_BYTES);
            while !message.is_char_boundary(message.len()) {
                message.pop();
            }
        }
        self.complete(
            idempotency_key,
            request_fingerprint,
            PersistedHostAdministrationOutcome::Failure {
                error_kind: error_kind_name(error.kind()).to_string(),
                message,
            },
            None,
            now_unix_seconds,
        )
    }

    fn complete(
        &self,
        idempotency_key: &str,
        request_fingerprint: &str,
        outcome: PersistedHostAdministrationOutcome,
        new_generation: Option<u64>,
        now_unix_seconds: u64,
    ) -> Result<()> {
        let key_hash = administration_key_hash(idempotency_key);
        self.mutate_database(|database| {
            let entry = database
                .entries
                .iter_mut()
                .find(|entry| entry.key_hash == key_hash)
                .ok_or_else(|| {
                    MezError::invalid_state("host administration replay reservation is missing")
                })?;
            if entry.request_fingerprint != request_fingerprint {
                return Err(MezError::conflict(
                    "host administration request changed before replay completion",
                ));
            }
            if let Some(existing) = &entry.outcome {
                if existing == &outcome {
                    return Ok(());
                }
                return Err(MezError::conflict(
                    "host administration replay outcome changed after completion",
                ));
            }
            entry.new_generation = new_generation;
            entry.completed_at_unix_seconds = Some(now_unix_seconds);
            entry.outcome = Some(outcome);
            Ok(())
        })
    }

    fn mutate_database<T>(
        &self,
        operation: impl FnOnce(&mut HostAdministrationDatabase) -> Result<T>,
    ) -> Result<T> {
        ensure_private_directory(&self.directory)?;
        let lock = open_private_lock(&self.directory.join(JOURNAL_LOCK_FILE_NAME))?;
        flock(&lock, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        let mut database = self.load_database()?;
        let result = operation(&mut database)?;
        self.write_database(&database)?;
        Ok(result)
    }

    fn load_database(&self) -> Result<HostAdministrationDatabase> {
        let path = self.directory.join(JOURNAL_FILE_NAME);
        let file = match open_private_file_read(&path) {
            Ok(file) => file,
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => {
                return Ok(HostAdministrationDatabase::default());
            }
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() > MAX_JOURNAL_BYTES {
            return Err(MezError::invalid_state(
                "host administration replay journal exceeds its protected size limit",
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_JOURNAL_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(MezError::invalid_state(
                "host administration replay journal exceeds its protected size limit",
            ));
        }
        let database: HostAdministrationDatabase =
            serde_json::from_slice(&bytes).map_err(|error| {
                MezError::invalid_state(format!(
                    "host administration replay journal is malformed: {error}"
                ))
            })?;
        if database.version != JOURNAL_VERSION || database.entries.len() > MAX_JOURNAL_ENTRIES {
            return Err(MezError::invalid_state(
                "host administration replay journal has unsupported bounds or version",
            ));
        }
        Ok(database)
    }

    fn write_database(&self, database: &HostAdministrationDatabase) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(database).map_err(|error| {
            MezError::invalid_state(format!(
                "failed to encode host administration replay journal: {error}"
            ))
        })?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(MezError::invalid_state(
                "host administration replay journal exceeds its protected size limit",
            ));
        }
        write_private_atomic(&self.directory.join(JOURNAL_FILE_NAME), &bytes)
    }
}

impl PersistedHostAdministrationOutcome {
    fn into_replay(self) -> Result<HostAdministrationReplay> {
        match self {
            Self::Success { response } => Ok(HostAdministrationReplay::Success(response)),
            Self::Failure {
                error_kind,
                message,
            } => Ok(HostAdministrationReplay::Failure(MezError::new(
                parse_error_kind(&error_kind)?,
                message,
            ))),
        }
    }
}

impl PartialEq for PersistedHostAdministrationOutcome {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Success { response: left }, Self::Success { response: right }) => left == right,
            (
                Self::Failure {
                    error_kind: left_kind,
                    message: left_message,
                },
                Self::Failure {
                    error_kind: right_kind,
                    message: right_message,
                },
            ) => left_kind == right_kind && left_message == right_message,
            _ => false,
        }
    }
}

fn prune_completed_entries(database: &mut HostAdministrationDatabase) {
    while database.entries.len() >= MAX_JOURNAL_ENTRIES {
        let Some(index) = database
            .entries
            .iter()
            .position(|entry| entry.outcome.is_some())
        else {
            break;
        };
        database.entries.remove(index);
    }
}

fn validate_idempotency_key(value: &str) -> Result<()> {
    validate_bounded_text(
        value,
        "host administration idempotency key",
        MAX_IDEMPOTENCY_KEY_BYTES,
    )
}

fn validate_optional_bounded_text(value: Option<&str>, label: &str, limit: usize) -> Result<()> {
    match value {
        Some(value) => validate_bounded_text(value, label, limit),
        None => Ok(()),
    }
}

fn validate_bounded_text(value: &str, label: &str, limit: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > limit || value.chars().any(char::is_control) {
        return Err(MezError::invalid_args(format!(
            "{label} must be printable text up to {limit} bytes"
        )));
    }
    Ok(())
}

fn administration_key_hash(idempotency_key: &str) -> String {
    sha256_hex(
        b"mezzanine-host-administration-key-v1\0",
        idempotency_key.as_bytes(),
    )
}

pub(crate) fn administration_request_fingerprint(
    method: &str,
    params: &serde_json::Map<String, Value>,
) -> Result<String> {
    let mut params = params.clone();
    params.remove("idempotency_key");
    let canonical = canonical_json(&Value::Object(params))?;
    let mut digest = Sha256::new();
    digest.update(b"mezzanine-host-administration-request-v1\0");
    digest.update(method.as_bytes());
    digest.update(b"\0");
    digest.update(canonical.as_bytes());
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn canonical_json(value: &Value) -> Result<String> {
    match value {
        Value::Null => Ok("null".to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        Value::String(value) => serde_json::to_string(value).map_err(|error| {
            MezError::invalid_state(format!(
                "failed to canonicalize administration JSON: {error}"
            ))
        }),
        Value::Array(values) => {
            let values = values
                .iter()
                .map(canonical_json)
                .collect::<Result<Vec<_>>>()?;
            Ok(format!("[{}]", values.join(",")))
        }
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            let fields = keys
                .into_iter()
                .map(|key| {
                    Ok(format!(
                        "{}:{}",
                        serde_json::to_string(key).map_err(|error| {
                            MezError::invalid_state(format!(
                                "failed to canonicalize administration JSON key: {error}"
                            ))
                        })?,
                        canonical_json(&values[key])?
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(format!("{{{}}}", fields.join(",")))
        }
    }
}

fn sha256_hex(domain: &[u8], value: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(value);
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn error_kind_name(kind: MezErrorKind) -> &'static str {
    match kind {
        MezErrorKind::InvalidArgs => "invalid_args",
        MezErrorKind::InvalidState => "invalid_state",
        MezErrorKind::Config => "config",
        MezErrorKind::Io => "io",
        MezErrorKind::Conflict => "conflict",
        MezErrorKind::NotFound => "not_found",
        MezErrorKind::Forbidden => "forbidden",
        MezErrorKind::RateLimited => "rate_limited",
        MezErrorKind::NotImplemented => "not_implemented",
    }
}

fn parse_error_kind(value: &str) -> Result<MezErrorKind> {
    match value {
        "invalid_args" => Ok(MezErrorKind::InvalidArgs),
        "invalid_state" => Ok(MezErrorKind::InvalidState),
        "config" => Ok(MezErrorKind::Config),
        "io" => Ok(MezErrorKind::Io),
        "conflict" => Ok(MezErrorKind::Conflict),
        "not_found" => Ok(MezErrorKind::NotFound),
        "forbidden" => Ok(MezErrorKind::Forbidden),
        "rate_limited" => Ok(MezErrorKind::RateLimited),
        "not_implemented" => Ok(MezErrorKind::NotImplemented),
        _ => Err(MezError::invalid_state(
            "host administration replay journal contains an unknown error kind",
        )),
    }
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    let existed = path.exists();
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != current_effective_uid()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(MezError::forbidden(
            "host administration replay directory must be private and owned by the current user",
        ));
    }
    if !existed && let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
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
    validate_private_file(path, &file.metadata()?)?;
    Ok(file)
}

fn open_private_file_read(path: &Path) -> Result<fs::File> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let file = fs::File::from(descriptor);
    validate_private_file(path, &file.metadata()?)?;
    Ok(file)
}

fn validate_private_file(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != current_effective_uid()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(MezError::forbidden(format!(
            "host administration replay path {} must be a private regular file owned by the current user",
            path.display()
        )));
    }
    Ok(())
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        MezError::invalid_args("host administration replay path has no parent directory")
    })?;
    ensure_private_directory(parent)?;
    if path.exists() {
        validate_private_file(path, &fs::symlink_metadata(path)?)?;
    }
    let temporary = parent.join(format!(
        ".{JOURNAL_FILE_NAME}.{}.{}.tmp",
        std::process::id(),
        NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        validate_private_file(path, &fs::symlink_metadata(path)?)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mez-host-administration-{label}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn durable_journal_replays_exact_results_and_rejects_conflicting_key_reuse() {
        let root = root("replay");
        let journal = HostAdministrationJournal::under_config_root(&root);
        let params = serde_json::json!({
            "target": "lease-1",
            "terminate": true,
            "idempotency_key": "same-key"
        });
        let HostAdministrationBegin::Fresh {
            request_fingerprint,
            ..
        } = journal
            .begin(
                "same-key",
                "lease/revoke",
                params.as_object().unwrap(),
                "1000",
                Some("lease-1"),
                Some(4),
                10,
            )
            .unwrap()
        else {
            panic!("first reservation must be fresh");
        };
        journal
            .complete_success(
                "same-key",
                &request_fingerprint,
                serde_json::json!({"lease_id":"lease-1","state":"revoked"}),
                Some(5),
                11,
            )
            .unwrap();

        let restarted = HostAdministrationJournal::under_config_root(&root);
        let HostAdministrationBegin::Replay(HostAdministrationReplay::Success(response)) =
            restarted
                .begin(
                    "same-key",
                    "lease/revoke",
                    params.as_object().unwrap(),
                    "1000",
                    Some("lease-1"),
                    Some(5),
                    12,
                )
                .unwrap()
        else {
            panic!("completed reservation must replay");
        };
        assert_eq!(response["state"], "revoked");
        let conflict = restarted
            .begin(
                "same-key",
                "lease/revoke",
                serde_json::json!({
                    "target":"lease-2",
                    "terminate":true,
                    "idempotency_key":"same-key"
                })
                .as_object()
                .unwrap(),
                "1000",
                Some("lease-2"),
                None,
                13,
            )
            .unwrap_err();
        assert_eq!(conflict.kind(), MezErrorKind::Conflict);
        let persisted =
            fs::read_to_string(root.join(JOURNAL_DIRECTORY_NAME).join(JOURNAL_FILE_NAME)).unwrap();
        assert!(!persisted.contains("same-key"), "{persisted}");
        let _ = fs::remove_dir_all(root);
    }
}
