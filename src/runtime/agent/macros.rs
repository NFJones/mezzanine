//! Runtime agent macro discovery helpers.
//!
//! This module keeps pane-scoped macro catalog discovery beside the skill
//! discovery helpers. Macro execution is implemented separately; this file
//! only resolves the effective user/project macro catalog for display,
//! completion, and explicit `#macro` prompt recognition.

use super::*;
use crate::macros::{MacroCatalog, discover_macro_catalog};
use crate::project::TrustDecision;

impl RuntimeSessionService {
    /// Builds the effective macro catalog for one pane.
    ///
    /// User macros are read from the configured user root. Project macros are
    /// included only when the pane is inside a trusted project root.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose current working directory scopes project macros.
    pub(in crate::runtime) fn effective_macro_catalog_for_pane(
        &self,
        pane_id: &str,
    ) -> MacroCatalog {
        let project_root = self.trusted_macro_project_root_for_pane(pane_id);
        discover_macro_catalog(self.config_root.as_deref(), project_root.as_deref())
    }

    /// Returns the trusted project root whose macros may apply to one pane.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose working directory determines project scope.
    fn trusted_macro_project_root_for_pane(&self, pane_id: &str) -> Option<PathBuf> {
        let working_directory = self.pane_current_working_directory(pane_id)?;
        let store = self.project_trust_store.as_ref()?;
        store
            .records()
            .filter(|record| record.state == TrustDecision::Trusted)
            .find(|record| {
                runtime_path_under_project_root(&working_directory, &record.project_root)
            })
            .map(|record| record.project_root.clone())
    }
}
