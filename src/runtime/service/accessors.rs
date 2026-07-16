//! Project trust and agent scheduler accessors.

use super::*;

impl RuntimeSessionService {
    /// Runs the project trust store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn project_trust_store(&self) -> Option<&ProjectTrustStore> {
        self.integration.project_trust_store()
    }

    /// Runs the set project trust store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_project_trust_store(
        &mut self,
        store: ProjectTrustStore,
        database_path: Option<PathBuf>,
    ) {
        self.integration.set_project_trust_store(Some(store));
        self.integration
            .set_project_trust_database_path(database_path);
    }
}
