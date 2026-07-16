//! Project trust and agent scheduler accessors.

use super::*;

impl RuntimeSessionService {
    /// Runs the project trust store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn project_trust_store(&self) -> Option<&ProjectTrustStore> {
        self.project_trust_store.as_ref()
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
        self.project_trust_store = Some(store);
        self.project_trust_database_path = database_path;
    }

    /// Runs the agent scheduler operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn agent_scheduler(&self) -> &AgentScheduler {
        &self.agent_scheduler
    }

    /// Runs the agent scheduler mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn agent_scheduler_mut(&mut self) -> &mut AgentScheduler {
        &mut self.agent_scheduler
    }
}
