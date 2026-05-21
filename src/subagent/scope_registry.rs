//! Active write-scope registry implementation.
//!
//! The registry keeps conflict detection independent from process or pane
//! lifetime management. Callers register scopes before allowing writes and
//! unregister them when the child agent completes.

use std::collections::BTreeMap;

use crate::error::{MezError, Result};

use super::types::{ActiveWriteScope, CooperationMode, ScopeConflict, ScopeRegistry};
use super::validation::{normalize_scope, scopes_overlap};

impl ScopeRegistry {
    /// Creates an empty active scope registry.
    pub fn new() -> Self {
        Self {
            active: BTreeMap::new(),
        }
    }

    /// Registers write scopes for an active agent.
    ///
    /// Returns an invalid-arguments error for an empty agent id and a conflict
    /// error when any requested scope overlaps an incompatible active scope.
    pub fn register(
        &mut self,
        agent_id: impl Into<String>,
        mode: CooperationMode,
        write_scopes: &[String],
        serial_lock: Option<String>,
    ) -> Result<()> {
        let agent_id = agent_id.into();
        if agent_id.is_empty() {
            return Err(MezError::invalid_args("subagent id must not be empty"));
        }
        let conflicts = self.conflicts(mode, write_scopes, serial_lock.as_deref());
        if let Some(conflict) = conflicts.first() {
            return Err(MezError::conflict(format!(
                "write scope `{}` overlaps active scope `{}` owned by {}",
                conflict.requested_scope, conflict.existing_scope, conflict.existing_agent_id
            )));
        }

        self.active.insert(
            agent_id.clone(),
            write_scopes
                .iter()
                .map(|scope| ActiveWriteScope {
                    agent_id: agent_id.clone(),
                    mode,
                    scope: normalize_scope(scope),
                    serial_lock: serial_lock.clone(),
                })
                .collect(),
        );
        Ok(())
    }

    /// Removes all active scopes for an agent.
    ///
    /// Returns true when the agent had a registration.
    pub fn unregister(&mut self, agent_id: &str) -> bool {
        self.active.remove(agent_id).is_some()
    }

    /// Returns the active write scopes currently registered for an agent.
    ///
    /// The returned scopes are normalized in the same form used for conflict
    /// checks. An empty vector means the agent has no active write-scope
    /// ownership in the registry.
    pub fn active_write_scopes_for(&self, agent_id: &str) -> Vec<ActiveWriteScope> {
        self.active.get(agent_id).cloned().unwrap_or_default()
    }

    /// Returns every active write scope currently registered.
    ///
    /// Results are grouped by agent id according to the registry's stable map
    /// order, which keeps diagnostics and prompt context deterministic.
    pub fn active_write_scopes(&self) -> Vec<ActiveWriteScope> {
        self.active
            .values()
            .flat_map(|scopes| scopes.iter().cloned())
            .collect()
    }

    /// Returns the total number of active write-scope registrations.
    pub fn active_write_scope_count(&self) -> usize {
        self.active.values().map(Vec::len).sum()
    }

    /// Returns conflicts between a requested write registration and active
    /// write scopes without mutating the registry.
    pub fn conflicts(
        &self,
        requested_mode: CooperationMode,
        requested_scopes: &[String],
        requested_serial_lock: Option<&str>,
    ) -> Vec<ScopeConflict> {
        if requested_mode == CooperationMode::ExploreOnly {
            return Vec::new();
        }

        let mut conflicts = Vec::new();
        for requested_scope in requested_scopes.iter().map(|scope| normalize_scope(scope)) {
            for active in self.active.values().flatten() {
                if !scopes_overlap(&requested_scope, &active.scope) {
                    continue;
                }
                if requested_mode == CooperationMode::SerialWrite
                    && active.mode == CooperationMode::SerialWrite
                    && active.serial_lock.as_deref() == requested_serial_lock
                    && requested_serial_lock.is_some()
                {
                    continue;
                }
                conflicts.push(ScopeConflict {
                    existing_agent_id: active.agent_id.clone(),
                    existing_scope: active.scope.clone(),
                    requested_scope: requested_scope.clone(),
                });
            }
        }
        conflicts
    }
}

impl Default for ScopeRegistry {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self::new()
    }
}
