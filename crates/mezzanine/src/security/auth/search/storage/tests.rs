//! Fake-only search auth regressions. Temporary roots are explicitly injected;
//! no real HOME/config, keyring, environment value, transcript, or HTTP is read.

use std::{cell::RefCell, rc::Rc};

use super::*;
use crate::security::auth::{AuthPaths, AuthStore, CredentialStoreKind};

const CANARY: &str = "synthetic-search-S3cr3t+/=canary";

/// Shared fake store with exact operation journaling and uncertain-failure modes.
#[derive(Clone)]
struct FakeStore {
    root: PathBuf,
    state: Rc<RefCell<State>>,
}

/// Synthetic values live only behind the same SecretString contract as stores.
#[derive(Default)]
struct State {
    secrets: BTreeMap<String, SecretString>,
    calls: Vec<(String, String)>,
    write_error: bool,
    write_then_error: bool,
    read_error: bool,
    delete_error: bool,
    delete_then_error: bool,
    forged_reference: bool,
    block_metadata_write: bool,
}

impl CredentialStore for FakeStore {
    fn kind(&self) -> CredentialStoreKind {
        CredentialStoreKind::PrivateFileFallback
    }
    fn store_secret(&self, account: &str, secret: &SecretString) -> Result<String> {
        let reference = Backend::File.reference(&self.root, account);
        let mut state = self.state.borrow_mut();
        state.calls.push(("store".into(), reference.clone()));
        if !state.write_error {
            state.secrets.insert(reference.clone(), secret.clone());
        }
        if state.block_metadata_write {
            fs::create_dir(self.root.join("search-auth.json")).unwrap();
        }
        if state.write_error || state.write_then_error {
            return Err(MezError::invalid_state(CANARY));
        }
        if state.forged_reference {
            return Ok("file:/outside/inference.secret".into());
        }
        Ok(reference)
    }
    fn load_secret(&self, reference: &str) -> Result<Option<SecretString>> {
        let mut state = self.state.borrow_mut();
        state.calls.push(("load".into(), reference.into()));
        if state.read_error {
            return Err(MezError::invalid_state(CANARY));
        }
        Ok(state.secrets.get(reference).cloned())
    }
    fn delete_secret(&self, reference: &str) -> Result<bool> {
        let mut state = self.state.borrow_mut();
        state.calls.push(("delete".into(), reference.into()));
        let removed = if !state.delete_error {
            state.secrets.remove(reference).is_some()
        } else {
            false
        };
        if state.delete_error || state.delete_then_error {
            return Err(MezError::invalid_state(CANARY));
        }
        Ok(removed)
    }
    fn contains_secret(&self, _: &str) -> Result<bool> {
        panic!("status must never call contains_secret")
    }
}

/// Owns a task-created temporary root and the injectable fake credential store.
struct Fixture {
    store: SearchAuthStore,
    backend: FakeStore,
}

impl Fixture {
    fn new() -> Self {
        let root = fs::canonicalize("/tmp").unwrap().join(format!(
            "mez-search-auth-test-{}-{}",
            std::process::id(),
            new_id()
        ));
        fs::create_dir(&root).unwrap();
        Self {
            store: SearchAuthStore::new(root.clone()),
            backend: FakeStore {
                root,
                state: Default::default(),
            },
        }
    }
    fn login(&self, value: &str) -> Result<SearchAuthStatus> {
        self.store.login_with(
            "work",
            SearchProvider::Brave,
            &SecretString::from(value),
            Backend::File,
            &self.backend,
            |_| Box::new(self.backend.clone()),
        )
    }
    fn capture(&self) -> SearchBinding {
        self.store
            .capture(
                &SearchCredentialRef::Stored("work".into()),
                SearchProvider::Brave,
                SearchProvider::Brave.origin(),
            )
            .unwrap()
    }
    fn resolve(&self, binding: &SearchBinding) -> Result<SecretString> {
        self.store.resolve_with(
            binding,
            SearchProvider::Brave,
            SearchProvider::Brave.origin(),
            |_| panic!("stored resolution must not read env"),
            |_| Box::new(self.backend.clone()),
        )
    }
    fn logout(&self) -> Result<bool> {
        self.store
            .logout_with("work", |_| Box::new(self.backend.clone()))
    }
    fn loads(&self) -> usize {
        self.backend
            .state
            .borrow()
            .calls
            .iter()
            .filter(|(op, _)| op == "load")
            .count()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.store.root);
    }
}

/// Metadata capture/status are zero-load operations, and one authorized
/// resolution yields exactly one redacted snapshot reusable after rotation.
#[test]
fn search_capture_status_and_snapshot_have_no_eager_secret_reads() {
    let f = Fixture::new();
    f.login(CANARY).unwrap();
    let binding = f.capture();
    assert_eq!(f.loads(), 0);
    let status = f.store.status("work").unwrap();
    assert!(status.configured);
    assert!(!status.remotely_verified);
    let metadata = fs::read_to_string(f.store.path()).unwrap();
    for output in [
        metadata,
        format!("{binding:?}"),
        serde_json::to_string(&status).unwrap(),
    ] {
        assert!(!output.contains(CANARY));
        assert!(!output.contains("UzNjcjN0"));
    }
    assert_eq!(f.loads(), 0);
    let snapshot = f.resolve(&binding).unwrap();
    assert_eq!(snapshot.expose_secret(), CANARY);
    assert_eq!(f.loads(), 1);
    f.login("replacement-synthetic-key").unwrap();
    assert_eq!(snapshot.expose_secret(), CANARY);
    assert!(!format!("{snapshot:?}").contains(CANARY));
    assert!(f.resolve(&binding).is_err());
    assert_eq!(f.loads(), 1);
}

/// A lazy env selector reads exactly the named primary-host variable only at
/// resolve. Changed fake env values cannot alter an already-held snapshot.
#[test]
fn search_env_is_explicit_named_lazy_and_snapshot_owned() {
    let f = Fixture::new();
    let reference = SearchCredentialRef::parse("env:SEARCH_KEY").unwrap();
    let binding = f
        .store
        .capture(
            &reference,
            SearchProvider::Tavily,
            SearchProvider::Tavily.origin(),
        )
        .unwrap();
    assert!(!f.store.path().exists());
    let calls = RefCell::new(Vec::new());
    let snapshot = f
        .store
        .resolve_after_authorization(
            &binding,
            SearchProvider::Tavily,
            SearchProvider::Tavily.origin(),
            |name| {
                calls.borrow_mut().push(name.to_owned());
                Ok(Some(SecretString::from(CANARY)))
            },
        )
        .unwrap();
    assert_eq!(&*calls.borrow(), &["SEARCH_KEY"]);
    assert_eq!(snapshot.expose_secret(), CANARY);
    assert!(
        f.store
            .resolve_after_authorization(
                &binding,
                SearchProvider::Exa,
                SearchProvider::Exa.origin(),
                |_| panic!("mismatch before env")
            )
            .is_err()
    );
    let missing = f
        .store
        .resolve_after_authorization(
            &binding,
            SearchProvider::Tavily,
            SearchProvider::Tavily.origin(),
            |_| Ok(None),
        )
        .unwrap_err();
    assert!(missing.message().contains("missing"));
    assert!(
        !f.store
            .resolve_after_authorization(
                &binding,
                SearchProvider::Tavily,
                SearchProvider::Tavily.origin(),
                |_| Err(MezError::invalid_state(CANARY))
            )
            .unwrap_err()
            .to_string()
            .contains(CANARY)
    );
    assert!(f.backend.state.borrow().calls.is_empty());
}

/// Provider/origin/ref injection, including bypassing parse via public variants,
/// is rejected before any backend or environment call.
#[test]
fn search_rejects_injected_names_origins_and_provider_mismatch_before_load() {
    let f = Fixture::new();
    for reference in [
        SearchCredentialRef::Stored("../openai".into()),
        SearchCredentialRef::Environment("BAD|KEY".into()),
        SearchCredentialRef::Stored("file:/tmp/key".into()),
    ] {
        assert!(
            f.store
                .capture(
                    &reference,
                    SearchProvider::Brave,
                    SearchProvider::Brave.origin()
                )
                .is_err()
        );
    }
    f.login(CANARY).unwrap();
    for origin in [
        "http://api.search.brave.com",
        "https://api.search.brave.com/",
        "https://api.search.brave.com:443",
        "https://api.search.brave.com.evil",
        "https://user@api.search.brave.com",
        "https://api.search.brave.com?x",
        "https://api.search.brave.com#x",
    ] {
        assert!(
            f.store
                .capture(
                    &SearchCredentialRef::Stored("work".into()),
                    SearchProvider::Brave,
                    origin
                )
                .is_err()
        );
        assert!(
            f.store
                .resolve_after_authorization(
                    &f.capture(),
                    SearchProvider::Brave,
                    origin,
                    |_| panic!("origin mismatch before env")
                )
                .is_err()
        );
    }
    assert!(
        f.store
            .capture(
                &SearchCredentialRef::Stored("work".into()),
                SearchProvider::Exa,
                SearchProvider::Exa.origin()
            )
            .is_err()
    );
    assert_eq!(f.loads(), 0);
}

/// Persistent fresh generations fence replacement, logout and recreate across
/// independently constructed owners; no stale binding can trigger a load.
#[test]
fn search_rotation_logout_and_recreate_fence_stale_bindings() {
    let f = Fixture::new();
    f.login(CANARY).unwrap();
    let old = f.capture();
    f.login("new-synthetic-key").unwrap();
    assert!(f.resolve(&old).is_err());
    let fresh = f.capture();
    assert!(f.logout().unwrap());
    assert!(!f.store.status("work").unwrap().configured);
    assert!(f.resolve(&fresh).is_err());
    f.login("third-synthetic-key").unwrap();
    let other = SearchAuthStore::new(f.store.root.clone());
    assert!(
        other
            .resolve_with(
                &old,
                SearchProvider::Brave,
                SearchProvider::Brave.origin(),
                |_| panic!("no env"),
                |_| Box::new(f.backend.clone())
            )
            .is_err()
    );
    assert!(f.resolve(&fresh).is_err());
    assert_eq!(f.loads(), 0);
    assert_eq!(
        f.resolve(&f.capture()).unwrap().expose_secret(),
        "third-synthetic-key"
    );
}

/// Delete errors may occur after actual deletion: new metadata/key must remain
/// current and old binding stale. A later exact-identity logout retries pending
/// cleanup without ever reviving an old generation or deleting unrelated keys.
#[test]
fn search_uncertain_delete_is_forward_only_with_retained_cleanup() {
    for after_effect in [false, true] {
        let f = Fixture::new();
        f.login(CANARY).unwrap();
        let old = f.capture();
        f.backend.state.borrow_mut().secrets.insert(
            "inference-unrelated".into(),
            SecretString::from("inference-canary"),
        );
        f.backend
            .state
            .borrow_mut()
            .secrets
            .insert("mcp-unrelated".into(), SecretString::from("mcp-canary"));
        {
            let mut state = f.backend.state.borrow_mut();
            state.delete_error = !after_effect;
            state.delete_then_error = after_effect;
        }
        let error = f.login("replacement-synthetic-key").unwrap_err();
        assert!(error.message().contains("committed"));
        assert!(!format!("{error:?}").contains(CANARY));
        assert!(f.resolve(&old).is_err());
        assert_eq!(
            f.resolve(&f.capture()).unwrap().expose_secret(),
            "replacement-synthetic-key"
        );
        assert!(f.store.status("work").unwrap().cleanup_pending);
        let latest = f.capture();
        assert!(f.logout().is_err());
        assert!(!f.store.status("work").unwrap().configured);
        assert!(f.resolve(&latest).is_err());
        {
            let mut state = f.backend.state.borrow_mut();
            state.delete_error = false;
            state.delete_then_error = false;
        }
        assert!(f.logout().unwrap());
        assert!(!f.store.status("work").unwrap().cleanup_pending);
        let state = f.backend.state.borrow();
        assert_eq!(state.secrets.len(), 2);
        assert!(state.secrets.contains_key("inference-unrelated"));
        assert!(state.secrets.contains_key("mcp-unrelated"));
        assert!(
            !state
                .calls
                .iter()
                .any(|(op, reference)| op == "delete" && reference.ends_with("unrelated"))
        );
    }
}

/// Failed staging (including write-then-error and forged returned ref) conserves
/// old credentials/metadata and cleans only the uniquely constructed stage.
#[test]
fn search_stage_failure_preserves_old_state_and_redacts_backend_errors() {
    for mode in 0..3 {
        let f = Fixture::new();
        f.login(CANARY).unwrap();
        let before = fs::read(f.store.path()).unwrap();
        let old = f.capture();
        {
            let mut state = f.backend.state.borrow_mut();
            state.write_error = mode == 0;
            state.write_then_error = mode == 1;
            state.forged_reference = mode == 2;
        }
        let error = f.login("replacement-synthetic-key").unwrap_err();
        assert!(!format!("{error:?}").contains(CANARY));
        assert_eq!(fs::read(f.store.path()).unwrap(), before);
        assert_eq!(f.backend.state.borrow().secrets.len(), 1);
        assert_eq!(f.resolve(&old).unwrap().expose_secret(), CANARY);
        assert!(
            !f.backend
                .state
                .borrow()
                .calls
                .iter()
                .any(|(_, reference)| reference == "file:/outside/inference.secret")
        );
    }
}

/// Metadata write failure before publication preserves every old secret and
/// cleans the new unique stage. This uses only a task-local filesystem fault.
#[test]
fn search_metadata_write_failure_cleans_stage_without_loading_secrets() {
    let f = Fixture::new();
    f.backend.state.borrow_mut().block_metadata_write = true;
    assert!(f.login(CANARY).is_err());
    assert!(f.backend.state.borrow().secrets.is_empty());
    assert_eq!(f.loads(), 0);
}

/// Publication fails after staging a replacement, so old metadata remains byte
/// identical and its credential remains usable. Stage cleanup uncertainty is
/// reported without changing the old binding or deleting unrelated records.
#[test]
fn search_precommit_failure_conserves_existing_metadata_and_credentials() {
    for cleanup_fails in [false, true] {
        let f = Fixture::new();
        f.login(CANARY).unwrap();
        let old = f.capture();
        let before = fs::read(f.store.path()).unwrap();
        f.store
            .fail_write_at
            .store(1, std::sync::atomic::Ordering::SeqCst);
        f.backend.state.borrow_mut().delete_error = cleanup_fails;
        let error = f.login("replacement-synthetic-key").unwrap_err();
        assert_eq!(fs::read(f.store.path()).unwrap(), before);
        assert_eq!(f.resolve(&old).unwrap().expose_secret(), CANARY);
        assert_eq!(
            f.backend.state.borrow().secrets.len(),
            if cleanup_fails { 2 } else { 1 }
        );
        if cleanup_fails {
            assert!(error.message().contains("cleanup incomplete"));
        }
    }
}

/// A failure updating cleanup bookkeeping is postcommit, not grounds for
/// rollback. The retained pending exact id can be retried after deletion.
#[test]
fn search_cleanup_bookkeeping_failure_never_revives_old_generation() {
    let f = Fixture::new();
    f.login(CANARY).unwrap();
    let old = f.capture();
    f.store
        .fail_write_at
        .store(2, std::sync::atomic::Ordering::SeqCst);
    let error = f.login("replacement-synthetic-key").unwrap_err();
    assert!(error.message().contains("committed"));
    assert!(f.store.status("work").unwrap().cleanup_pending);
    assert!(f.resolve(&old).is_err());
    assert_eq!(
        f.resolve(&f.capture()).unwrap().expose_secret(),
        "replacement-synthetic-key"
    );
    assert!(f.logout().unwrap());
    assert!(f.backend.state.borrow().secrets.is_empty());
}

/// Repeated uncertain cleanup errors cannot consume the capacity needed for
/// logout, poison another identity, or publish a document its reader rejects.
/// A fresh owner can reopen the saturated tombstone and recover exact cleanup.
#[test]
fn search_cleanup_saturation_reserves_logout_capacity_and_recovers() {
    let f = Fixture::new();
    f.login(CANARY).unwrap();
    f.store
        .login_with(
            "other",
            SearchProvider::Brave,
            &SecretString::from("other-canary"),
            Backend::File,
            &f.backend,
            |_| Box::new(f.backend.clone()),
        )
        .unwrap();
    f.backend.state.borrow_mut().delete_error = true;
    for _ in 0..31 {
        assert!(
            f.login("replacement-synthetic-key")
                .unwrap_err()
                .message()
                .contains("committed")
        );
    }
    let before = fs::read(f.store.path()).unwrap();
    assert!(
        f.login("rejected-before-stage")
            .unwrap_err()
            .message()
            .contains("cleanup required")
    );
    assert_eq!(fs::read(f.store.path()).unwrap(), before);
    assert!(f.store.status("work").unwrap().configured);
    assert!(f.logout().unwrap_err().message().contains("committed"));
    let reopened = SearchAuthStore::new(f.store.root.clone());
    assert!(!reopened.status("work").unwrap().configured);
    assert!(reopened.status("other").unwrap().configured);
    assert_eq!(reopened.read().unwrap().entries["work"].pending.len(), 32);
    f.backend.state.borrow_mut().delete_error = false;
    assert!(
        reopened
            .logout_with("work", |_| Box::new(f.backend.clone()))
            .unwrap()
    );
    assert!(!reopened.status("work").unwrap().cleanup_pending);
    assert!(reopened.status("other").unwrap().configured);
    assert_eq!(f.backend.state.borrow().secrets.len(), 1);
}

/// Invalid documents and all domain-crossing internal ref forms fail before
/// load/delete, with source text omitted from errors. There is no migration
/// that reads an inference or MCP secret to populate the search domain.
#[test]
fn search_metadata_forgery_is_rejected_without_backend_access() {
    let f = Fixture::new();
    f.login(CANARY).unwrap();
    let binding = f.capture();
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(f.store.path()).unwrap()).unwrap();
    let mut forged = Vec::new();
    let mut value = original.clone();
    value["version"] = 999.into();
    forged.push(value);
    let mut value = original.clone();
    value["secret"] = CANARY.into();
    forged.push(value);
    for (field, replacement) in [
        ("name", "../openai"),
        ("origin", "https://evil.example"),
        ("generation", "file:/tmp/key"),
    ] {
        let mut value = original.clone();
        value["entries"]["work"][field] = replacement.into();
        forged.push(value);
    }
    for (field, replacement) in [
        ("id", "os-keyring:secret-service/mezzanine/openai"),
        ("backend", "file:/tmp/key"),
        ("provider", "codex"),
    ] {
        let mut value = original.clone();
        value["entries"]["work"]["current"][field] = replacement.into();
        forged.push(value);
    }
    let mut value = original.clone();
    value["entries"]["work"]["current"]["reference"] = "file:/tmp/key".into();
    forged.push(value);
    let before_calls = f.backend.state.borrow().calls.len();
    for value in forged {
        fs::write(f.store.path(), serde_json::to_vec(&value).unwrap()).unwrap();
        let error = f.resolve(&binding).unwrap_err();
        assert!(!format!("{error:?}").contains(CANARY));
        assert!(f.logout().is_err());
        assert!(f.store.status("work").is_err());
    }
    fs::write(f.store.path(), format!("{{\"secret\":\"{CANARY}")).unwrap();
    assert!(!format!("{:?}", f.resolve(&binding).unwrap_err()).contains(CANARY));
    assert_eq!(f.backend.state.borrow().calls.len(), before_calls);
}

/// Exact identity changes after capture, even to syntactically valid ids or
/// backends, fail comparison before any secret backend is selected.
#[test]
fn search_full_binding_comparison_rejects_validly_shaped_substitution() {
    let f = Fixture::new();
    f.login(CANARY).unwrap();
    let binding = f.capture();
    let original = f.store.read().unwrap();
    for mode in 0..4 {
        let mut metadata = original.clone();
        let entry = metadata.entries.get_mut("work").unwrap();
        match mode {
            0 => entry.generation = new_id(),
            1 => entry.current.as_mut().unwrap().id = new_id(),
            2 => entry.current.as_mut().unwrap().backend = Backend::Native,
            _ => {
                entry.provider = SearchProvider::Exa;
                entry.origin = SearchProvider::Exa.origin().into();
                entry.current.as_mut().unwrap().provider = SearchProvider::Exa;
            }
        }
        assert_eq!(
            f.store.write(&metadata).unwrap(),
            Publication::DirectorySynced
        );
        assert!(f.resolve(&binding).is_err());
    }
    assert_eq!(f.loads(), 0);
}

/// A lock held by an independent updater rejects resolution without loading;
/// release restores the same binding. This covers compare/load serialization.
#[test]
fn search_commit_lock_contention_fails_before_secret_load() {
    let f = Fixture::new();
    f.login(CANARY).unwrap();
    let binding = f.capture();
    let guard = auth_commit_lock(&f.store.root).unwrap();
    assert!(f.resolve(&binding).is_err());
    assert_eq!(f.loads(), 0);
    drop(guard);
    assert_eq!(f.resolve(&binding).unwrap().expose_secret(), CANARY);
}

/// Header-unrepresentable, empty or huge values fail before storage; missing,
/// invalid and backend read failures remain redacted and never fall back.
#[test]
fn search_secret_validation_and_read_failures_are_bounded_and_redacted() {
    let f = Fixture::new();
    for value in ["", " ", "key\r\nHeader: x", "key\0", "unicode-ß"] {
        assert!(f.login(value).is_err());
    }
    assert!(f.login(&"x".repeat(8193)).is_err());
    assert!(f.backend.state.borrow().calls.is_empty());
    f.login(CANARY).unwrap();
    let binding = f.capture();
    f.backend.state.borrow_mut().read_error = true;
    assert!(!format!("{:?}", f.resolve(&binding).unwrap_err()).contains(CANARY));
    f.backend.state.borrow_mut().read_error = false;
    let reference = f
        .backend
        .state
        .borrow()
        .secrets
        .keys()
        .next()
        .unwrap()
        .clone();
    f.backend
        .state
        .borrow_mut()
        .secrets
        .insert(reference.clone(), SecretString::from(""));
    assert!(f.resolve(&binding).is_err());
    f.backend.state.borrow_mut().secrets.remove(&reference);
    assert!(
        f.resolve(&binding)
            .unwrap_err()
            .message()
            .contains("missing")
    );
}

/// Status inspects only safe file metadata, including symlink/nonregular and
/// absent-path cases. Metadata I/O failures stay local and never reach a store.
#[test]
fn search_status_reports_filesystem_failures_without_secret_reads() {
    let f = Fixture::new();
    f.login(CANARY).unwrap();
    assert_eq!(f.store.status("work").unwrap().presence, "missing");
    let metadata = f.store.read().unwrap();
    let record = metadata.entries["work"].current.as_ref().unwrap();
    let path = f
        .store
        .root
        .join("search-auth-secrets")
        .join(format!("{}.secret", record.account("work")));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::create_dir(&path).unwrap();
    assert_eq!(
        f.store.status("work").unwrap().presence,
        "store_unavailable"
    );
    fs::remove_dir(&path).unwrap();
    let target = f.store.root.join("synthetic-status-target");
    fs::write(&target, CANARY).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert_eq!(
        f.store.status("work").unwrap().presence,
        "store_unavailable"
    );
    let binding = f.capture();
    fs::rename(f.store.path(), f.store.root.join("metadata-backup")).unwrap();
    fs::create_dir(f.store.path()).unwrap();
    assert!(f.store.status("work").is_err());
    assert!(f.resolve(&binding).is_err());
    assert_eq!(f.loads(), 0);
}

/// Every metadata-consuming core operation rejects an unsafe node with zero
/// operations on the injected credential port. Public login may first probe
/// backend availability; this helper does not claim that probe is exercised.
fn assert_unsafe_metadata_without_store(f: &Fixture, binding: &SearchBinding) {
    let calls = f.backend.state.borrow().calls.len();
    assert!(f.store.status("work").is_err());
    assert!(
        f.store
            .capture(
                &SearchCredentialRef::Stored("work".into()),
                SearchProvider::Brave,
                SearchProvider::Brave.origin()
            )
            .is_err()
    );
    assert!(f.login("replacement-synthetic-key").is_err());
    assert!(f.logout().is_err());
    assert!(f.resolve(binding).is_err());
    assert_eq!(f.backend.state.borrow().calls.len(), calls);
}

/// A no-writer FIFO must fail promptly, not hang inside metadata open while a
/// mutation holds the common auth lock. A controlled nonblocking writer releases
/// the OLD blocking implementation after the negative witness, so a regression
/// fails without leaving a detached hung thread. Remaining consumers are checked
/// only after the prompt result has been proven.
#[test]
fn search_metadata_fifo_is_rejected_without_blocking_or_store_access() {
    use std::time::Duration;
    let f = Fixture::new();
    f.login(CANARY).unwrap();
    let binding = f.capture();
    let original = fs::read(f.store.path()).unwrap();
    fs::remove_file(f.store.path()).unwrap();
    rustix::fs::mkfifoat(rustix::fs::CWD, f.store.path(), Mode::RUSR | Mode::WUSR).unwrap();
    let owned = f.store.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || sender.send(owned.status("work")).unwrap());
    let early = receiver.recv_timeout(Duration::from_secs(2));
    let prompt = early.is_ok();
    if !prompt {
        let mut writer = fs::OpenOptions::new()
            .write(true)
            .custom_flags(OFlags::NONBLOCK.bits() as i32)
            .open(f.store.path())
            .unwrap();
        writer.write_all(b"{}").unwrap();
    }
    let result = match early {
        Ok(result) => result,
        Err(_) => receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
    };
    worker.join().unwrap();
    assert!(
        prompt,
        "metadata FIFO open blocked until the owned writer released it"
    );
    assert!(result.is_err());
    assert_unsafe_metadata_without_store(&f, &binding);
    assert!(std::os::unix::fs::FileTypeExt::is_fifo(
        &fs::symlink_metadata(f.store.path()).unwrap().file_type()
    ));
    fs::remove_file(f.store.path()).unwrap();
    fs::write(f.store.path(), &original).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(f.store.path(), fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(f.resolve(&binding).unwrap().expose_secret(), CANARY);
}

/// Private-file validation rejects overbroad modes, hardlinks, oversized files,
/// directories and symlinks without repairing/chmodding/reading unsafe inputs or
/// touching any secret backend. Owner checking is exercised without chown/root.
#[test]
fn search_metadata_opened_inode_validation_preserves_unsafe_inputs() {
    use std::os::unix::fs::PermissionsExt;
    for kind in 0..5 {
        let f = Fixture::new();
        f.login(CANARY).unwrap();
        let binding = f.capture();
        let original = fs::read(f.store.path()).unwrap();
        let alias = f.store.root.join("synthetic-metadata-alias");
        let private = fs::metadata(f.store.path()).unwrap();
        assert!(validate_metadata_file(&private, private.uid()).is_ok());
        assert!(validate_metadata_file(&private, private.uid().wrapping_add(1)).is_err());
        match kind {
            0 => fs::set_permissions(f.store.path(), fs::Permissions::from_mode(0o644)).unwrap(),
            1 => fs::hard_link(f.store.path(), &alias).unwrap(),
            2 => fs::write(f.store.path(), vec![b' '; 1024 * 1024 + 1]).unwrap(),
            3 => {
                fs::remove_file(f.store.path()).unwrap();
                fs::create_dir(f.store.path()).unwrap();
            }
            _ => {
                fs::rename(f.store.path(), &alias).unwrap();
                std::os::unix::fs::symlink(&alias, f.store.path()).unwrap();
            }
        }
        let before = fs::symlink_metadata(f.store.path()).unwrap();
        assert_unsafe_metadata_without_store(&f, &binding);
        let after = fs::symlink_metadata(f.store.path()).unwrap();
        assert_eq!(
            (after.mode(), after.nlink(), after.len()),
            (before.mode(), before.nlink(), before.len())
        );
        if kind == 0 || kind == 1 {
            assert_eq!(fs::read(f.store.path()).unwrap(), original);
        }
        if kind == 1 || kind == 4 {
            assert_eq!(fs::read(&alias).unwrap(), original);
        }
    }
}

/// First-login post-rename uncertainty retains the newly visible credential;
/// replacement uncertainty keeps both new current and old pending records.
/// Neither outcome can enter precommit stage deletion or revive a stale binding.
#[test]
fn search_published_unsynced_login_conserves_current_and_pending_credentials() {
    let f = Fixture::new();
    f.store
        .fail_directory_sync_at
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let error = f.login(CANARY).unwrap_err();
    assert!(error.message().contains("published") && error.message().contains("sync uncertain"));
    assert_eq!(f.backend.state.borrow().secrets.len(), 1);
    assert!(
        !f.backend
            .state
            .borrow()
            .calls
            .iter()
            .any(|(op, _)| op == "delete")
    );
    assert!(
        SearchAuthStore::new(f.store.root.clone())
            .status("work")
            .unwrap()
            .configured
    );
    let old = f.capture();
    f.store
        .fail_directory_sync_at
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let error = f.login("replacement-synthetic-key").unwrap_err();
    assert!(error.message().contains("published"));
    assert_eq!(f.backend.state.borrow().secrets.len(), 2);
    assert!(
        !f.backend
            .state
            .borrow()
            .calls
            .iter()
            .any(|(op, _)| op == "delete")
    );
    assert!(f.store.status("work").unwrap().cleanup_pending);
    let loads = f.loads();
    assert!(f.resolve(&old).is_err());
    assert_eq!(f.loads(), loads);
    assert_eq!(
        f.resolve(&f.capture()).unwrap().expose_secret(),
        "replacement-synthetic-key"
    );
    assert!(f.logout().unwrap());
    assert!(f.backend.state.borrow().secrets.is_empty());
}

/// Logout publishes its tombstone before syncing, and sync uncertainty defers
/// ALL destructive cleanup. A fresh owner sees revocation; later exact cleanup
/// is safe and cannot touch unrelated inference/MCP records.
#[test]
fn search_published_unsynced_logout_retains_revocation_and_defers_deletion() {
    let f = Fixture::new();
    f.login(CANARY).unwrap();
    let old = f.capture();
    f.backend.state.borrow_mut().secrets.insert(
        "unrelated-inference".into(),
        SecretString::from("inference-canary"),
    );
    f.store
        .fail_directory_sync_at
        .store(1, std::sync::atomic::Ordering::SeqCst);
    assert!(f.logout().unwrap_err().message().contains("published"));
    let reopened = SearchAuthStore::new(f.store.root.clone());
    assert!(!reopened.status("work").unwrap().configured);
    assert!(reopened.status("work").unwrap().cleanup_pending);
    assert_eq!(f.backend.state.borrow().secrets.len(), 2);
    assert!(
        !f.backend
            .state
            .borrow()
            .calls
            .iter()
            .any(|(op, _)| op == "delete")
    );
    let loads = f.loads();
    assert!(f.resolve(&old).is_err());
    assert_eq!(f.loads(), loads);
    assert!(f.logout().unwrap());
    assert_eq!(f.backend.state.borrow().secrets.len(), 1);
    assert!(
        f.backend
            .state
            .borrow()
            .secrets
            .contains_key("unrelated-inference")
    );
}

/// A sync failure during cleanup bookkeeping is already postcommit: the new
/// key stays current even though old exact deletion happened. A later mutation
/// recovers without resurrecting the old generation or removing the new key.
#[test]
fn search_published_unsynced_cleanup_bookkeeping_keeps_new_generation() {
    let f = Fixture::new();
    f.login(CANARY).unwrap();
    let old = f.capture();
    f.store
        .fail_directory_sync_at
        .store(2, std::sync::atomic::Ordering::SeqCst);
    let error = f.login("replacement-synthetic-key").unwrap_err();
    assert!(error.message().contains("published"));
    assert!(!error.message().contains("cleanup deferred"));
    assert_eq!(
        f.backend
            .state
            .borrow()
            .calls
            .iter()
            .filter(|(op, _)| op == "delete")
            .count(),
        1
    );
    assert_eq!(f.backend.state.borrow().secrets.len(), 1);
    assert!(f.resolve(&old).is_err());
    assert_eq!(
        f.resolve(&f.capture()).unwrap().expose_secret(),
        "replacement-synthetic-key"
    );
    assert!(f.logout().unwrap());
    assert!(f.backend.state.borrow().secrets.is_empty());
}

/// An existing metadata staging name is never owned merely because it was
/// generated. Failed exclusive creation must preserve its bytes/mode/inode,
/// old metadata and old current key while cleaning only the new secret stage.
#[test]
fn search_metadata_stage_collision_preserves_unowned_file_and_old_generation() {
    use std::os::unix::fs::PermissionsExt;
    let mut f = Fixture::new();
    f.login(CANARY).unwrap();
    let old = f.capture();
    let metadata_before = fs::read(f.store.path()).unwrap();
    let stage = f.store.root.join(".synthetic-existing-stage.tmp");
    fs::write(&stage, "nonsecret-existing-stage-sentinel").unwrap();
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::metadata(&stage).unwrap();
    f.store.metadata_stage_name = Some(".synthetic-existing-stage.tmp".into());
    assert!(f.login("replacement-synthetic-key").is_err());
    let after = fs::metadata(&stage).unwrap();
    assert_eq!(
        (after.ino(), after.dev(), after.mode(), after.nlink()),
        (before.ino(), before.dev(), before.mode(), before.nlink())
    );
    assert_eq!(
        fs::read_to_string(stage).unwrap(),
        "nonsecret-existing-stage-sentinel"
    );
    assert_eq!(fs::read(f.store.path()).unwrap(), metadata_before);
    assert_eq!(f.backend.state.borrow().secrets.len(), 1);
    assert_eq!(f.resolve(&old).unwrap().expose_secret(), CANARY);
}

/// Preexisting pending cleanup must not be retried when a new publication is
/// visible but unsynced. The current generation and every pending credential
/// remain conserved until a later successfully synced exact-name mutation.
#[test]
fn search_unsynced_replacement_does_not_start_existing_pending_cleanup() {
    let f = Fixture::new();
    f.login(CANARY).unwrap();
    f.backend.state.borrow_mut().delete_error = true;
    assert!(f.login("second-synthetic-key").is_err());
    let old = f.capture();
    {
        let mut state = f.backend.state.borrow_mut();
        state.delete_error = false;
        state.calls.clear();
        state
            .secrets
            .insert("unrelated-mcp".into(), SecretString::from("mcp-canary"));
    }
    f.store
        .fail_directory_sync_at
        .store(1, std::sync::atomic::Ordering::SeqCst);
    assert!(
        f.login("third-synthetic-key")
            .unwrap_err()
            .message()
            .contains("published")
    );
    assert_eq!(f.backend.state.borrow().secrets.len(), 4);
    assert_eq!(f.store.read().unwrap().entries["work"].pending.len(), 2);
    assert!(
        !f.backend
            .state
            .borrow()
            .calls
            .iter()
            .any(|(op, _)| op == "delete")
    );
    assert!(f.resolve(&old).is_err());
    assert_eq!(f.loads(), 0);
    assert!(f.logout().unwrap());
    assert_eq!(f.backend.state.borrow().secrets.len(), 1);
    assert!(
        f.backend
            .state
            .borrow()
            .secrets
            .contains_key("unrelated-mcp")
    );
}

/// Search and inference use the same private-file mechanism but disjoint
/// directories; search refs cannot load inference data and inference AuthStore
/// rejects a reconstructed search internal file reference before value read.
#[test]
fn search_namespaces_are_disjoint_from_existing_auth_stores() {
    let f = Fixture::new();
    let inference = AuthStore::new(AuthPaths::under_config_root(&f.store.root));
    let inference_backend = inference.file_credential_store("work").unwrap();
    inference
        .login_provider_api_key("work", "default", "inference-canary", &inference_backend)
        .unwrap();
    f.login(CANARY).unwrap();
    let metadata = f.store.read().unwrap();
    let record = metadata.entries["work"].current.as_ref().unwrap();
    assert!(
        inference
            .load_secret(&record.reference(&f.store.root, "work"))
            .is_err()
    );
    assert!(SearchCredentialRef::parse("os-keyring:secret-service/mezzanine/work").is_err());
    assert!(SearchCredentialRef::parse("file:/tmp/inference-key").is_err());
    assert!(f.logout().unwrap());
    assert_eq!(
        inference.provider_secret("work").unwrap().expose_secret(),
        "inference-canary"
    );
    let native = crate::security::auth::secret_service::NativeSecretServiceCredentialStore::new();
    assert!(
        native
            .provider_from_reference("os-keyring:secret-service/mez-search-auth/search-work")
            .is_err()
    );
    let search =
        crate::security::auth::secret_service::NativeSecretServiceCredentialStore::search();
    assert!(
        search
            .provider_from_reference("os-keyring:secret-service/mezzanine/work")
            .is_err()
    );
}
