//! Request-local accounting attribution over qualified project mappings.
//!
//! This owner reuses current deepest trust resolution; mapping possession never
//! grants execution authority. Preparation qualifies opaque IDs off actor, while
//! request freeze captures attribution once. Missing mapping/store/cwd or changed
//! directory identity remains explicitly unattributed rather than falling back.

use crate::runtime::RuntimeSessionService;
use crate::storage::token_usage::{AccountingOrigin, accounting_origin_for_root};

impl RuntimeSessionService {
    /// Selects a project from actor-retained canonical cwd/trust/mapping evidence.
    /// Missing or unqualified evidence stays unavailable; reporting never probes disk.
    pub(crate) fn cached_accounting_project_for_pane(
        &self,
        pane_id: &str,
    ) -> Option<crate::storage::token_usage::AccountingProjectId> {
        let directory = self.pane_current_working_directory(pane_id)?;
        let trust = self.integration.project_trust_store()?;
        let provenance =
            crate::security::project::resolve_canonical_project_trust_provenance(trust, &directory);
        let root = provenance.trusted_root()?;
        self.persistence
            .accounting_projects()?
            .iter()
            .find(|row| row.root == root)?
            .id
            .clone()
    }

    /// Freezes attribution from current eligible trust and qualified mapping.
    /// Later cwd, trust or pane changes cannot alter the returned owned value.
    pub(crate) fn capture_accounting_origin_for_pane(&self, pane_id: &str) -> AccountingOrigin {
        let root = self.trusted_project_root_for_pane(pane_id);
        accounting_origin_for_root(root.as_deref(), self.persistence.accounting_projects())
    }
}
