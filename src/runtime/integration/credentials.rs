//! Credential refresh and project-trust integration state.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::auth::{AuthStore, DEFAULT_PROVIDER_AUTH_REFRESH_LEEWAY_SECONDS};
use crate::project::ProjectTrustStore;

/// Owns credential and trust-store bindings used by product integrations.
#[derive(Debug)]
pub(super) struct RuntimeCredentialState {
    auth_store: Option<AuthStore>,
    provider_auth_refresh_leeway_seconds: u64,
    project_trust_store: Option<ProjectTrustStore>,
    project_trust_database_path: Option<PathBuf>,
    announced_project_trust_roots: BTreeSet<PathBuf>,
}

impl Default for RuntimeCredentialState {
    fn default() -> Self {
        Self {
            auth_store: None,
            provider_auth_refresh_leeway_seconds: DEFAULT_PROVIDER_AUTH_REFRESH_LEEWAY_SECONDS,
            project_trust_store: None,
            project_trust_database_path: None,
            announced_project_trust_roots: BTreeSet::new(),
        }
    }
}

impl RuntimeCredentialState {
    pub(super) fn auth_store(&self) -> Option<&AuthStore> {
        self.auth_store.as_ref()
    }

    pub(super) fn set_auth_store(&mut self, store: Option<AuthStore>) {
        self.auth_store = store;
    }

    pub(super) fn provider_auth_refresh_leeway_seconds(&self) -> u64 {
        self.provider_auth_refresh_leeway_seconds
    }

    pub(super) fn set_provider_auth_refresh_leeway_seconds(&mut self, seconds: u64) {
        self.provider_auth_refresh_leeway_seconds = seconds;
    }

    pub(super) fn project_trust_store(&self) -> Option<&ProjectTrustStore> {
        self.project_trust_store.as_ref()
    }

    pub(super) fn project_trust_store_mut(&mut self) -> Option<&mut ProjectTrustStore> {
        self.project_trust_store.as_mut()
    }

    pub(super) fn set_project_trust_store(&mut self, store: Option<ProjectTrustStore>) {
        self.project_trust_store = store;
    }

    pub(super) fn project_trust_database_path(&self) -> Option<&Path> {
        self.project_trust_database_path.as_deref()
    }

    pub(super) fn set_project_trust_database_path(&mut self, path: Option<PathBuf>) {
        self.project_trust_database_path = path;
    }

    pub(super) fn mark_project_trust_root_announced(&mut self, root: PathBuf) -> bool {
        self.announced_project_trust_roots.insert(root)
    }

    pub(super) fn clear_project_trust_root_announcement(&mut self, root: &Path) -> bool {
        self.announced_project_trust_roots.remove(root)
    }
}
