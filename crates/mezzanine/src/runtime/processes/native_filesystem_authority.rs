//! Backend-neutral filesystem authority composed by the existing runtime actor.
//!
//! Trust provenance, configured grants, planning and inherited scope intersections
//! remain owned by RuntimeSessionService. This adapter accepts physical cwd only;
//! it never selects shells, forwards environments or probes sandbox executables.

use std::path::Path;

use mez_agent::AgentTurnRecord;
use mez_agent::permissions::PathScopes;

use crate::error::{MezError, Result};
use crate::runtime::RuntimeSessionService;
use crate::security::filesystem::host_resolved_path_scopes;

impl RuntimeSessionService {
    /// Resolves maximum current turn authority without shell context. None means
    /// no configured or deepest-trusted-project grant; child scopes only narrow.
    pub(crate) fn native_filesystem_scopes_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        cwd: &Path,
    ) -> Result<Option<PathScopes>> {
        self.refresh_project_trust_store_from_disk_if_changed()?;
        let resources = &self.configured_permissions().resources;
        let (mut reads, writes) =
            if !resources.read_scopes.is_empty() || !resources.write_scopes.is_empty() {
                (
                    resources.read_scopes.clone(),
                    resources.write_scopes.clone(),
                )
            } else if let Some(root) = self.integration.project_trust_store().and_then(|store| {
                crate::security::project::resolve_project_trust_provenance(store, cwd)
                    .trusted_root()
                    .map(Path::to_path_buf)
            }) {
                let root = root.to_string_lossy().into_owned();
                (vec![root.clone()], vec![root])
            } else {
                return Ok(None);
            };
        #[cfg(target_os = "macos")]
        for root in self.seatbelt_toolchain_read_scopes_for_pane(&turn.pane_id) {
            // Optional loader reads must not invalidate otherwise authorized
            // project access when this process cannot traverse the host root.
            let accessible =
                crate::security::filesystem::resolve_host_path(cwd, root).is_ok_and(|evidence| {
                    evidence.kind == mez_agent::permissions::ResolvedPathKind::Existing
                        && evidence.object_kind
                            == mez_agent::permissions::ResolvedPathObjectKind::Directory
                        && evidence.canonical_path == root
                });
            if accessible && !reads.iter().any(|scope| scope == root) {
                reads.push(root.to_string());
            }
        }
        // Existing PathScopes grants snapshot reads for writable paths. Preserve
        // those reads while planning strips every publication grant.
        let planning = self.agent_planning_enabled(&turn.pane_id);
        if planning {
            reads.extend(writes.iter().cloned());
        }
        let writes = if planning { Vec::new() } else { writes };
        let primary = host_resolved_path_scopes(cwd, &reads, &writes, &[])?;
        let Some(child) = self.subagent_scope_declaration_for_turn(turn) else {
            return Ok(Some(primary));
        };
        let child = host_resolved_path_scopes(
            Path::new(&child.current_directory),
            &child.read_scopes,
            &child.write_scopes,
            &[],
        )?;
        primary
            .intersection(&child)
            .map_err(|error| MezError::invalid_state(error.message()))
            .map(Some)
    }
}
