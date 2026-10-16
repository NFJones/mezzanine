//! Search credential transactions and lazy, generation-bound resolution.
//!
//! The common auth flock covers metadata comparison and the single secret
//! load, as well as writes/revocation. Publication is forward-only: destructive
//! cleanup errors cannot revive an earlier generation. Pending cleanup contains
//! only domain-bound opaque records and is retried on the next mutation of that
//! exact logical identity. No status operation loads secrets or probes a store.

use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use rustix::fs::{AtFlags, Mode, OFlags, open, openat, renameat, unlinkat};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use super::super::{
    fs::{auth_commit_lock, ensure_private_dir, reject_existing_symlink_components},
    types::CredentialStore,
};
use super::{
    MezError, Result, SearchCredentialRef, SearchProvider, backend::Backend, validate_name,
};

/// Strict versioned metadata; unknown fields cannot hide inline credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    /// Persistent format version, independent of the primary config schema.
    version: u32,
    /// Logical identities, including tombstones needed for pending deletion.
    entries: BTreeMap<String, Entry>,
}

/// Metadata publication phase, separate from pre-rename errors. Directory
/// syncing covers this metadata entry only, not credential backend durability
/// or newly created ancestor directories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "published-but-unsynced metadata must not trigger precommit credential cleanup"]
enum Publication {
    /// The exact directory used for relative publication was synced.
    DirectorySynced,
    /// New metadata is visible but directory sync failed or is uncertain.
    PublishedUnsynced,
}

impl Default for Metadata {
    /// Creates a new format without a migration or credential read.
    fn default() -> Self {
        Self {
            version: 1,
            entries: BTreeMap::new(),
        }
    }
}

impl Metadata {
    /// Applies the same invariants before publication and after decoding. The
    /// combined record limit reserves room for moving current into pending on
    /// logout, even after repeated uncertain deletion failures.
    fn validate(&self) -> Result<()> {
        if self.version != 1 || self.entries.len() > 256 {
            return Err(MezError::config("unsupported search auth metadata"));
        }
        for (name, entry) in &self.entries {
            validate_name(name)?;
            if name != &entry.name
                || entry.origin != entry.provider.origin()
                || !valid_id(&entry.generation)
                || entry.pending.len() + usize::from(entry.current.is_some()) > 32
            {
                return Err(MezError::config("invalid search auth identity"));
            }
            if entry
                .current
                .as_ref()
                .is_some_and(|record| record.provider != entry.provider)
            {
                return Err(MezError::config("invalid search auth provider binding"));
            }
            for record in entry.current.iter().chain(&entry.pending) {
                if !valid_id(&record.id)
                    || entry
                        .pending
                        .iter()
                        .any(|pending| Some(pending) == entry.current.as_ref())
                {
                    return Err(MezError::config("invalid search auth internal reference"));
                }
            }
        }
        Ok(())
    }
}

/// One logical identity with its current generation and bounded cleanup queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    /// Duplicated identity is validated against the enclosing map key.
    name: String,
    /// Fixed provider, never an inference model-profile selector.
    provider: SearchProvider,
    /// Exact code-owned HTTPS origin, validated before store access.
    origin: String,
    /// Fresh random generation even for logout/recreation (no ABA).
    generation: String,
    /// Current opaque record; absence is a logically revoked tombstone.
    current: Option<Record>,
    /// Superseded records whose exact deletion has not been confirmed.
    pending: Vec<Record>,
}

/// Opaque internal storage identity, not a caller-selected path/reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    /// Closed backend domain.
    backend: Backend,
    /// Provider remains attached to old records across provider replacement.
    provider: SearchProvider,
    /// Fresh random credential incarnation, distinct from logical generation.
    id: String,
}

impl Record {
    /// Domain-binds a record to its logical name and provider in the account.
    fn account(&self, name: &str) -> String {
        let provider = match self.provider {
            SearchProvider::Brave => "brave",
            SearchProvider::Tavily => "tavily",
            SearchProvider::Exa => "exa",
            SearchProvider::Serpapi => "serpapi",
        };
        format!("search-{name}-{provider}-{}", self.id)
    }

    /// Constructs an exact code-owned backend reference after validation.
    fn reference(&self, root: &Path, name: &str) -> String {
        self.backend.reference(root, &self.account(name))
    }
}

/// Metadata-only status. `presence` never implies remote authentication.
#[derive(Debug, Serialize)]
pub struct SearchAuthStatus {
    /// Safe logical identity, not its opaque internal reference.
    pub name: String,
    /// Configured provider, absent when no local identity exists.
    pub provider: Option<SearchProvider>,
    /// Whether an active local credential record is configured.
    pub configured: bool,
    /// `present`/`missing` describe file metadata only; OS records are `unknown`.
    pub presence: &'static str,
    /// False: local storage never verifies provider acceptance or billing.
    pub remotely_verified: bool,
    /// Whether a previous exact deletion remains unconfirmed.
    pub cleanup_pending: bool,
}

/// Immutable non-secret binding captured before request authorization.
///
/// Fields are private to prevent substitution. This is not approval evidence.
/// The future trusted runtime must retain and authorize this exact binding with
/// its request, then call resolution off the actor thread. It must never resolve
/// rejected, unapproved, or cancelled-before-dispatch work.
#[derive(Debug, Clone)]
pub struct SearchBinding {
    /// Captured provider and destination, also for explicit environment refs.
    provider: SearchProvider,
    /// Validated selector, never a captured environment value.
    source: SearchCredentialRef,
    /// Full stored incarnation; none for an explicit environment source.
    entry: Option<Entry>,
}

/// Local search auth owner rooted only at an explicitly supplied config root.
#[derive(Debug, Clone)]
pub struct SearchAuthStore {
    /// Root is injected by AuthStore/CLI, never discovered from HOME here.
    root: PathBuf,
    /// Deterministic fake-only failure at the Nth metadata publication.
    #[cfg(test)]
    fail_write_at: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Deterministic fake-only post-rename directory-sync failure.
    #[cfg(test)]
    fail_directory_sync_at: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Deterministic metadata-stage collision fixture, absent in production.
    #[cfg(test)]
    metadata_stage_name: Option<String>,
}

impl SearchAuthStore {
    /// Creates an inert owner. Does not create paths, read metadata or secrets,
    /// access the environment, or contact an OS credential service.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            #[cfg(test)]
            fail_write_at: Default::default(),
            #[cfg(test)]
            fail_directory_sync_at: Default::default(),
            #[cfg(test)]
            metadata_stage_name: None,
        }
    }

    /// Returns the single versioned metadata location, not a secret file.
    fn path(&self) -> PathBuf {
        self.root.join("search-auth.json")
    }

    /// Validates the entire metadata document before any backend operation.
    fn read(&self) -> Result<Metadata> {
        let path = self.path();
        reject_existing_symlink_components(&path)?;
        let file = match fs::OpenOptions::new()
            .read(true)
            .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC).bits() as i32)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Metadata::default());
            }
            Err(_) => return Err(MezError::config("search auth metadata unavailable")),
        };
        let owned = file.metadata().map_err(|_| MezError::config("search auth metadata unavailable"))?;
        validate_metadata_file(&owned, rustix::process::geteuid().as_raw())?;
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| MezError::config("search auth metadata unavailable"))?;
        if bytes.len() > 1024 * 1024 {
            return Err(MezError::config("search auth metadata too large"));
        }
        let metadata: Metadata = serde_json::from_slice(&bytes)
            .map_err(|_| MezError::config("invalid search auth metadata"))?;
        metadata.validate()?;
        Ok(metadata)
    }

    /// Publishes private metadata relative to one retained directory descriptor.
    /// Err is possible ONLY before rename. Directory-sync uncertainty after
    /// publication is explicit, never a precommit credential-cleanup signal.
    /// This is not a multi-store transaction or whole-backend crash guarantee.
    fn write(&self, metadata: &Metadata) -> Result<Publication> {
        metadata.validate()?;
        #[cfg(test)]
        if self.fail_write_at.fetch_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |remaining| remaining.checked_sub(1),
        ) == Ok(1)
        {
            return Err(MezError::config(
                "synthetic search metadata publication failure",
            ));
        }
        ensure_private_dir(&self.root)?;
        reject_existing_symlink_components(&self.path())?;
        let directory = fs::File::from(open(&self.root, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())
            .map_err(|_| MezError::config("search auth metadata directory unavailable"))?);
        let directory_metadata = directory.metadata().map_err(|_| MezError::config("search auth metadata directory unavailable"))?;
        if !directory_metadata.is_dir() || directory_metadata.uid() != rustix::process::geteuid().as_raw() || directory_metadata.mode() & 0o077 != 0 {
            return Err(MezError::forbidden("search auth metadata directory must be owned and private"));
        }
        let temporary = format!(".search-auth-{}.tmp", new_id());
        #[cfg(test)]
        let temporary = self.metadata_stage_name.clone().unwrap_or(temporary);
        let bytes = serde_json::to_vec(metadata)
            .map_err(|_| MezError::config("search auth metadata encoding failed"))?;
        if bytes.len() > 1024 * 1024 {
            return Err(MezError::config("search auth metadata too large"));
        }
        // Cleanup ownership begins ONLY after exclusive creation succeeds.
        // An existing generated pathname is not ours to remove on EEXIST.
        let descriptor = openat(&directory, temporary.as_str(), OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::RUSR | Mode::WUSR)
            .map_err(|_| MezError::config("search auth metadata staging creation failed"))?;
        let result = (|| {
            let mut file = fs::File::from(descriptor);
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            renameat(&directory, temporary.as_str(), &directory, "search-auth.json").map_err(std::io::Error::from)?;
            Ok(())
        })();
        if result.is_err() {
            let cleanup = unlinkat(&directory, temporary.as_str(), AtFlags::empty());
            if cleanup.is_err_and(|error| error != rustix::io::Errno::NOENT) {
                return Err(MezError::config(
                    "search auth metadata update failed; temporary metadata cleanup incomplete",
                ));
            }
        }
        result.map_err(|_: std::io::Error| MezError::config("search auth metadata update failed"))?;
        #[cfg(test)]
        if self.fail_directory_sync_at.fetch_update(std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst, |remaining| remaining.checked_sub(1)) == Ok(1) {
            return Ok(Publication::PublishedUnsynced);
        }
        Ok(match directory.sync_all() {
            Ok(()) => Publication::DirectorySynced,
            Err(_) => Publication::PublishedUnsynced,
        })
    }

    /// Reports configuration and safe filesystem presence only. OS keyring
    /// presence/availability is unknown until an explicitly authorized load;
    /// neither a probe nor contains_secret is called for status/list.
    pub fn status(&self, name: &str) -> Result<SearchAuthStatus> {
        validate_name(name)?;
        let metadata = self.read()?;
        let entry = metadata.entries.get(name);
        let current = entry.and_then(|entry| entry.current.as_ref());
        let presence = match current {
            None => "missing",
            Some(record) if record.backend == Backend::File => {
                let path = self
                    .root
                    .join("search-auth-secrets")
                    .join(format!("{}.secret", record.account(name)));
                if reject_existing_symlink_components(&path).is_err() {
                    "store_unavailable"
                } else {
                    match fs::symlink_metadata(path) {
                        Ok(metadata) if metadata.is_file() => "present",
                        Ok(_) => "store_unavailable",
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing",
                        Err(_) => "store_unavailable",
                    }
                }
            }
            Some(_) => "unknown",
        };
        Ok(SearchAuthStatus {
            name: name.to_owned(),
            provider: entry.map(|entry| entry.provider),
            configured: current.is_some(),
            presence,
            remotely_verified: false,
            cleanup_pending: entry.is_some_and(|entry| !entry.pending.is_empty()),
        })
    }

    /// Captures only metadata for one explicit selector and exact destination.
    /// This does not authorize the future request or inspect environment values.
    #[allow(
        dead_code,
        reason = "future trusted search dispatch must capture metadata before authorization; runtime wiring is separately owned"
    )]
    pub fn capture(
        &self,
        reference: &SearchCredentialRef,
        provider: SearchProvider,
        origin: &str,
    ) -> Result<SearchBinding> {
        validate_destination(provider, origin)?;
        let reference = match reference {
            SearchCredentialRef::Stored(name) => {
                SearchCredentialRef::parse(&format!("search-auth:{name}"))?
            }
            SearchCredentialRef::Environment(name) => {
                SearchCredentialRef::parse(&format!("env:{name}"))?
            }
        };
        let entry = if let SearchCredentialRef::Stored(name) = &reference {
            let entry = self
                .read()?
                .entries
                .remove(name)
                .filter(|entry| entry.current.is_some())
                .ok_or_else(|| MezError::invalid_state("search credential is not configured"))?;
            if entry.provider != provider || entry.origin != origin {
                return Err(MezError::forbidden(
                    "search credential provider/origin mismatch",
                ));
            }
            Some(entry)
        } else {
            None
        };
        Ok(SearchBinding {
            provider,
            source: reference,
            entry,
        })
    }

    /// Resolves only after the trusted caller has authorized its exact captured
    /// request. This port does not enforce runtime approval on its own. Call it
    /// off actor threads. The returned single SecretString snapshot belongs to
    /// one exchange (and its future retries); do not copy it into plaintext or
    /// resolve again for retries. `named_host_env` must read only the explicitly
    /// named primary host/service variable, never pane env or a copied all-env.
    #[allow(
        dead_code,
        reason = "search runtime binding integration is a separate dependency"
    )]
    pub fn resolve_after_authorization(
        &self,
        binding: &SearchBinding,
        provider: SearchProvider,
        origin: &str,
        named_host_env: impl FnOnce(&str) -> Result<Option<SecretString>>,
    ) -> Result<SecretString> {
        self.resolve_with(binding, provider, origin, named_host_env, |record| {
            record.backend.store(&self.root)
        })
    }

    /// Shares production compare/load ordering with injected fake backends.
    fn resolve_with(
        &self,
        binding: &SearchBinding,
        provider: SearchProvider,
        origin: &str,
        named_host_env: impl FnOnce(&str) -> Result<Option<SecretString>>,
        backend: impl FnOnce(&Record) -> Box<dyn CredentialStore>,
    ) -> Result<SecretString> {
        validate_destination(provider, origin)?;
        if binding.provider != provider {
            return Err(MezError::forbidden("search credential destination changed"));
        }
        let secret = match &binding.source {
            SearchCredentialRef::Environment(name) => named_host_env(name)
                .map_err(|_| {
                    MezError::invalid_state("selected search environment credential unavailable")
                })?
                .ok_or_else(|| {
                    MezError::invalid_state("selected search environment credential missing")
                })?,
            SearchCredentialRef::Stored(name) => {
                let _commit = auth_commit_lock(&self.root)?;
                let current = self
                    .read()?
                    .entries
                    .remove(name)
                    .ok_or_else(|| MezError::conflict("search credential binding is stale"))?;
                // Cleanup bookkeeping may change independently of credential
                // identity; current generation/provider/origin/record may not.
                let mut captured = binding
                    .entry
                    .clone()
                    .ok_or_else(|| MezError::forbidden("invalid search credential binding"))?;
                captured.pending = current.pending.clone();
                if captured != current {
                    return Err(MezError::conflict("search credential binding is stale"));
                }
                let record = current
                    .current
                    .as_ref()
                    .ok_or_else(|| MezError::conflict("search credential binding is revoked"))?;
                backend(record)
                    .load_secret(&record.reference(&self.root, name))
                    .map_err(|_| MezError::invalid_state("search credential store read failed"))?
                    .ok_or_else(|| MezError::invalid_state("search credential missing"))?
            }
        };
        validate_secret(&secret)?;
        Ok(secret)
    }

    /// Stores a local credential only, never selects a provider/profile, probes
    /// HTTP, or changes the primary configuration. A post-publication cleanup
    /// failure reports committed state and retains the new generation/key.
    pub fn login(
        &self,
        name: &str,
        provider: SearchProvider,
        secret: &SecretString,
        selected_store: Option<&str>,
    ) -> Result<SearchAuthStatus> {
        validate_name(name)?;
        validate_secret(secret)?;
        let backend = Backend::select(selected_store)?;
        self.login_with(
            name,
            provider,
            secret,
            backend,
            backend.store(&self.root).as_ref(),
            |record| record.backend.store(&self.root),
        )
    }

    /// Implements staging/publication with fake-injectable existing-store port.
    fn login_with(
        &self,
        name: &str,
        provider: SearchProvider,
        secret: &SecretString,
        backend: Backend,
        store: &dyn CredentialStore,
        cleanup_store: impl Fn(&Record) -> Box<dyn CredentialStore>,
    ) -> Result<SearchAuthStatus> {
        validate_name(name)?;
        validate_secret(secret)?;
        let _commit = auth_commit_lock(&self.root)?;
        let mut metadata = self.read()?;
        if metadata.entries.len() >= 256 && !metadata.entries.contains_key(name) {
            return Err(MezError::invalid_state(
                "search credential identity limit reached",
            ));
        }
        let previous = metadata.entries.get(name).cloned();
        let mut pending = previous
            .as_ref()
            .map(|entry| entry.pending.clone())
            .unwrap_or_default();
        if let Some(record) = previous.and_then(|entry| entry.current) {
            pending.push(record);
        }
        if pending.len() >= 32 {
            return Err(MezError::invalid_state(
                "search credential cleanup required; run logout for this identity",
            ));
        }
        let record = Record {
            backend,
            provider,
            id: new_id(),
        };
        let expected = record.reference(&self.root, name);
        let staged = store.store_secret(&record.account(name), secret);
        if !matches!(&staged, Ok(reference) if reference == &expected) {
            // Even store Err may follow an effect. Only our freshly generated
            // exact reference may be cleaned up; never a backend-returned URI.
            return Err(stage_error(store, &expected));
        }
        metadata.entries.insert(
            name.to_owned(),
            Entry {
                name: name.to_owned(),
                provider,
                origin: provider.origin().to_owned(),
                generation: new_id(),
                current: Some(record),
                pending,
            },
        );
        match self.write(&metadata) {
            Err(error) => {
                return Err(if store.delete_secret(&expected).is_err() {
                    MezError::invalid_state("search credential update failed before commit; staged credential cleanup incomplete")
                } else { error });
            }
            Ok(Publication::PublishedUnsynced) => return Err(publication_uncertain()),
            Ok(Publication::DirectorySynced) => {}
        }
        self.cleanup(&mut metadata, name, cleanup_store)?;
        self.status(name).map_err(|_| {
            MezError::invalid_state(
                "search credential change committed; status metadata unavailable",
            )
        })
    }

    /// Revokes exactly one logical search identity before destructive cleanup.
    /// Repeating logout retries only that identity's retained pending records.
    pub fn logout(&self, name: &str) -> Result<bool> {
        self.logout_with(name, |record| record.backend.store(&self.root))
    }

    /// Shares forward-only revocation with deterministic fake backends.
    fn logout_with(
        &self,
        name: &str,
        backend: impl Fn(&Record) -> Box<dyn CredentialStore>,
    ) -> Result<bool> {
        validate_name(name)?;
        let _commit = auth_commit_lock(&self.root)?;
        let mut metadata = self.read()?;
        let Some(entry) = metadata.entries.get_mut(name) else {
            return Ok(false);
        };
        let changed = entry.current.is_some() || !entry.pending.is_empty();
        if let Some(record) = entry.current.take() {
            entry.pending.push(record);
        }
        entry.generation = new_id();
        if self.write(&metadata)? == Publication::PublishedUnsynced {
            return Err(publication_uncertain());
        }
        self.cleanup(&mut metadata, name, backend)?;
        Ok(changed)
    }

    /// Retains pending records on uncertain deletion or bookkeeping failure.
    /// A later mutation can safely retry an already-missing exact record.
    fn cleanup(
        &self,
        metadata: &mut Metadata,
        name: &str,
        backend: impl Fn(&Record) -> Box<dyn CredentialStore>,
    ) -> Result<()> {
        let entry = metadata
            .entries
            .get_mut(name)
            .ok_or_else(|| MezError::invalid_state("search credential cleanup identity missing"))?;
        for record in &entry.pending {
            backend(record).delete_secret(&record.reference(&self.root, name)).map_err(|_| MezError::invalid_state("search credential change committed; old credential deletion incomplete; retry logout or login for this identity"))?;
        }
        if !entry.pending.is_empty() {
            entry.pending.clear();
            let publication = self.write(metadata).map_err(|_| {
                MezError::invalid_state(
                    "search credential change committed; cleanup bookkeeping incomplete",
                )
            })?;
            if publication == Publication::PublishedUnsynced {
                return Err(publication_uncertain());
            }
        }
        Ok(())
    }
}

/// Validates the opened metadata inode before reading. Nonblocking open avoids
/// FIFO rendezvous; private regular/single-link checks reject special-node and
/// hardlink aliases. Ancestor checks are not an atomic ancestor-CAS guarantee.
fn validate_metadata_file(metadata: &fs::Metadata, expected_uid: u32) -> Result<()> {
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != expected_uid || metadata.mode() & 0o077 != 0 {
        return Err(MezError::forbidden("search auth metadata must be an owned private single-link regular file"));
    }
    if metadata.len() > 1024 * 1024 {
        return Err(MezError::config("search auth metadata too large"));
    }
    Ok(())
}

/// A visible publication must never be treated as a failed staging attempt.
fn publication_uncertain() -> MezError {
    MezError::invalid_state("search credential metadata published; directory sync uncertain; current generation retained")
}

/// Validates exact origin equality, rejecting alternate scheme/port/path/query,
/// userinfo and hostname suffix tricks before any credential/environment read.
fn validate_destination(provider: SearchProvider, origin: &str) -> Result<()> {
    if origin != provider.origin() {
        return Err(MezError::forbidden("invalid search credential destination"));
    }
    Ok(())
}

/// Accepts nonempty bounded visible ASCII suitable for HTTP header values.
/// Secret bytes are deliberately omitted from every error.
pub(super) fn validate_secret(secret: &SecretString) -> Result<()> {
    let value = secret.expose_secret().as_bytes();
    if value.is_empty()
        || value.len() > 8192
        || !value.iter().all(|byte| (0x21..=0x7e).contains(byte))
    {
        return Err(MezError::invalid_args(
            "search credential must be nonempty bounded visible ASCII without whitespace",
        ));
    }
    Ok(())
}

/// Creates a random opaque incarnation without depending on process counters.
fn new_id() -> String {
    format!("{:032x}", rand::random::<u128>())
}

/// Accepts only the canonical bounded opaque identifier grammar, never a URI.
fn valid_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Redacts backend failure/mismatched-reference text and cleans only our stage.
fn stage_error(store: &dyn CredentialStore, expected: &str) -> MezError {
    if store.delete_secret(expected).is_err() {
        MezError::invalid_state(
            "search credential staging failed; staged credential cleanup incomplete",
        )
    } else {
        MezError::invalid_state("search credential staging failed")
    }
}

/// Isolated synthetic backend and metadata fault regressions.
#[cfg(test)]
mod tests;
