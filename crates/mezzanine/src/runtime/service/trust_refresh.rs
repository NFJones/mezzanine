//! Persisted project-trust refresh and live-state reconciliation.
//!
//! Long-running services retain trust in memory, while standalone CLI commands
//! can update the shared database. This module refreshes that state at explicit
//! trust-sensitive boundaries, invalidates generation-keyed authority evidence,
//! and contracts live permissions before reporting malformed external state.

use super::{
    ConfigScope, EventKind, MezError, Path, ProjectTrustStore, Result, RuntimeSessionService,
    TrustDecision, discover_project_root, json_escape,
};
use crate::runtime::runtime_path_under_project_root;
use crate::security::project::ProjectTrustSnapshot;

impl RuntimeSessionService {
    /// Reloads the configured trust database when its exact content changed.
    ///
    /// Returns `true` only for a semantic trust change. Byte-only rewrites
    /// update the retained revision without invalidating runtime caches.
    pub(crate) fn refresh_project_trust_store_from_disk_if_changed(&mut self) -> Result<bool> {
        let Some(path) = self
            .integration
            .project_trust_database_path()
            .map(Path::to_path_buf)
        else {
            return Ok(false);
        };
        let snapshot = match ProjectTrustStore::load_snapshot_from_file(&path) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.fail_closed_project_trust_refresh();
                return Err(MezError::invalid_state(format!(
                    "failed to reload project trust database {}: {error}",
                    path.display()
                )));
            }
        };
        if self.integration.project_trust_revision() == Some(&snapshot.revision) {
            return Ok(false);
        }
        self.install_project_trust_snapshot(snapshot, "external_project_trust_refresh")
    }

    /// Installs one persisted snapshot and reconciles all trust-derived state.
    pub(crate) fn install_project_trust_snapshot(
        &mut self,
        snapshot: ProjectTrustSnapshot,
        event_source: &str,
    ) -> Result<bool> {
        let Some(changed_layers) = self.reconcile_project_trust_snapshot(snapshot)? else {
            return Ok(false);
        };
        if !changed_layers.is_empty() {
            self.apply_runtime_config_layers()?;
        }
        self.append_primary_lifecycle_event(
            EventKind::ConfigChanged,
            format!(
                r#"{{"source":"{}","changed_layers":{}}}"#,
                json_escape(event_source),
                changed_layers.len()
            ),
        )?;
        Ok(true)
    }

    /// Replaces live trust from one persisted snapshot and invalidates every
    /// authority artifact derived from the prior semantic state.
    pub(crate) fn reconcile_project_trust_snapshot(
        &mut self,
        snapshot: ProjectTrustSnapshot,
    ) -> Result<Option<Vec<String>>> {
        let semantic_change = self
            .integration
            .project_trust_store()
            .is_none_or(|store| store != &snapshot.store);
        if !semantic_change {
            self.integration
                .set_project_trust_revision(Some(snapshot.revision));
            return Ok(None);
        }

        let old_store = self
            .integration
            .project_trust_store()
            .cloned()
            .unwrap_or_default();
        let revoked_roots = old_store
            .records()
            .filter(|record| record.state == TrustDecision::Trusted)
            .filter(|record| {
                snapshot
                    .store
                    .get(&record.project_root)
                    .is_none_or(|current| current.state != TrustDecision::Trusted)
            })
            .map(|record| record.project_root.clone())
            .collect::<Vec<_>>();

        self.integration
            .set_project_trust_store(Some(snapshot.store));
        self.integration
            .set_project_trust_revision(Some(snapshot.revision));
        self.integration.clear_project_trust_root_announcements();
        let changed_layers = self.reconcile_project_overlay_trust();
        self.session.advance_config_generation();

        for root in revoked_roots {
            if let Some(config_root) = self.integration.config_root() {
                let _ =
                    crate::security::sandbox::remove_bubblewrap_managed_home(config_root, &root);
            }
        }
        Ok(Some(changed_layers))
    }

    /// Removes stale authority immediately after a persisted reload failure.
    fn fail_closed_project_trust_refresh(&mut self) {
        let had_state = self
            .integration
            .project_trust_store()
            .is_some_and(|store| store.records().next().is_some());
        self.integration
            .set_project_trust_store(Some(ProjectTrustStore::default()));
        self.integration.set_project_trust_revision(None);
        self.integration.clear_project_trust_root_announcements();
        let changed_layers = self.reconcile_project_overlay_trust();
        if had_state || !changed_layers.is_empty() {
            self.session.advance_config_generation();
            let _ = self.apply_runtime_config_layers();
        }
    }

    /// Recomputes every loaded project overlay against the current trust store.
    pub(crate) fn reconcile_project_overlay_trust(&mut self) -> Vec<String> {
        let trusted_roots = self
            .integration
            .project_trust_store()
            .map(|store| {
                store
                    .records()
                    .filter(|record| record.state == TrustDecision::Trusted)
                    .map(|record| record.project_root.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut changed = Vec::new();
        for layer in self.integration.config_layers_mut() {
            if layer.scope != ConfigScope::ProjectOverlay {
                continue;
            }
            let Some(path) = layer.path.as_ref() else {
                continue;
            };
            let project_root = path
                .parent()
                .map(discover_project_root)
                .unwrap_or_else(|| discover_project_root(path));
            let trusted = trusted_roots
                .iter()
                .any(|root| runtime_path_under_project_root(&project_root, root));
            if layer.trusted != trusted {
                layer.trusted = trusted;
                changed.push(layer.name.clone());
            }
        }
        changed
    }
}
