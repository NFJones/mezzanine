//! Human administrative agent snapshots and exact-target operation reducers.
//!
//! Session discovery is uncapped and distinct from model audience discovery.
//! Display text is inert; retained identities, root incarnations and task targets
//! authorize explicit activation only. Confirmation never follows a neighboring
//! row after refresh. External agents have focus but no native lifecycle controls.

use super::agent_lifecycle::RuntimeAgentLifecycleTarget;
use crate::error::{MezError, Result};
use crate::runtime::processes::RuntimePaneProcessIdentity;
use crate::runtime::{AgentTurnState, RuntimeSessionService};
use mez_core::ids::ClientId;
use mez_mux::record_browser::{RecordBrowser, RecordBrowserRecord};
use std::collections::BTreeMap;

#[cfg(test)]
thread_local! {
    /// Test-local counts isolate synchronous snapshot work from parallel fixtures.
    static PROJECT_LOOKUPS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// Takes and clears this test thread's snapshot and unique-pane lookup counts.
#[cfg(test)]
pub(crate) fn take_agent_browser_project_lookup_counts() -> (usize, usize) {
    PROJECT_LOOKUPS.with(|counts| counts.replace((0, 0)))
}

/// Inert project labels qualified only during browser snapshot construction.
/// No accounting mapping, MMP audience, pane title or vendor path grants a label.
#[derive(Clone)]
struct AgentBrowserProject {
    /// Full canonical trusted root, known absence, or unavailable evidence.
    label: String,
    /// Bounded provenance fact, separate from the visible table column.
    status: String,
}

impl AgentBrowserProject {
    /// Reports unavailable evidence without disclosing failed path diagnostics.
    fn unavailable() -> Self {
        Self {
            label: "unavailable".into(),
            status: "unavailable".into(),
        }
    }
}

/// Retained authority evidence, separate from clipped labels and objectives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentBrowserTarget {
    pub(crate) identity: mez_agent::messaging::SenderIdentity,
    pub(crate) process: Option<RuntimePaneProcessIdentity>,
    pub(crate) conversation: Option<String>,
    pub(crate) external_generation: Option<u64>,
    pub(crate) lifecycle: Option<RuntimeAgentLifecycleTarget>,
    pub(crate) pause_generation: Option<u64>,
}

/// Exact armed close operation; overlay generation rejects stale confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentCloseConfirmation {
    pub(crate) id: String,
    pub(crate) target: AgentBrowserTarget,
    pub(crate) generation: u32,
}

impl RuntimeSessionService {
    /// Builds an uncapped live-registration snapshot without saved conversations.
    pub(crate) fn agent_management_browser(
        &mut self,
        client: &ClientId,
    ) -> Result<(RecordBrowser, BTreeMap<String, AgentBrowserTarget>)> {
        self.require_live()?;
        if !self.session.is_attached_primary(client) {
            return Err(MezError::forbidden(
                "agent management requires attached primary",
            ));
        }
        // Refresh may reconcile runtime authority; do it before capturing any
        // targets. Failure keeps the list usable but withholds every trust label.
        #[cfg(test)]
        PROJECT_LOOKUPS.with(|counts| {
            let (snapshots, panes) = counts.get();
            counts.set((snapshots + 1, panes));
        });
        let trust_available = self
            .refresh_project_trust_store_from_disk_if_changed()
            .is_ok();
        self.reconcile_external_agent_registrations();
        self.reconcile_human_pauses()?;
        let mut targets = BTreeMap::new();
        let mut records = Vec::new();
        let mut projects = BTreeMap::<String, AgentBrowserProject>::new();
        for identity in self.message_service().discover_agents() {
            let id = identity.agent_id.as_str().to_string();
            let external = self.external_agent_metadata(&id);
            if identity
                .capabilities
                .iter()
                .any(|value| value == "external-harness")
                && external.is_none()
            {
                continue;
            }
            let pane = identity.pane_id.as_ref().map(|pane| pane.as_str());
            let descriptor = pane.and_then(|pane| self.find_pane_descriptor(pane));
            let project = if let Some(pane) = pane.filter(|_| descriptor.is_some()) {
                projects
                    .entry(pane.to_string())
                    .or_insert_with(|| self.agent_browser_project(pane, trust_available))
                    .clone()
            } else {
                AgentBrowserProject::unavailable()
            };
            let session = pane.and_then(|pane| self.agent_shell_store().get(pane));
            let process = pane
                .filter(|_| descriptor.is_some())
                .and_then(|pane| self.pane_process_identity(pane).ok());
            let lifecycle = pane
                .filter(|_| external.is_none())
                .and_then(|pane| self.capture_agent_lifecycle_target(client, pane).ok())
                .filter(|target| target.agent_id == id);
            let turn = self.agent_turn_ledger().turns().iter().rev().find(|turn| {
                turn.agent_id == id
                    && session.is_none_or(|session| turn.conversation_id == session.session_id)
            });
            let status = pane
                .and_then(|pane| self.agent_human_pause_status(pane))
                .map(str::to_string)
                .or_else(|| {
                    external
                        .as_ref()
                        .and_then(|row| row["status"].as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| {
                    match turn.map(|turn| turn.state) {
                        Some(AgentTurnState::Queued) => "queued",
                        Some(AgentTurnState::Running) => "running",
                        Some(AgentTurnState::Blocked)
                            if turn.is_some_and(|turn| {
                                self.agent_scheduler()
                                    .waiting_turns()
                                    .any(|work| work.turn_id == turn.turn_id)
                            }) =>
                        {
                            "waiting"
                        }
                        Some(AgentTurnState::Blocked) => "blocked",
                        Some(AgentTurnState::Interrupted) => "interrupted",
                        Some(AgentTurnState::Failed) => "failed",
                        _ => "idle",
                    }
                    .into()
                });
            let name = external
                .as_ref()
                .and_then(|row| row["display_name"].as_str())
                .map(str::to_string)
                .or_else(|| {
                    self.subagent_lineage(&id)
                        .map(|lineage| lineage.display_name.clone())
                })
                .or_else(|| {
                    pane.and_then(|pane| self.primary_agent_display_name(pane))
                        .map(str::to_string)
                })
                .unwrap_or_else(|| id.clone());
            let controls = if lifecycle.is_some() {
                "focus · i interrupt · p pause/resume · d close"
            } else if process.is_some() {
                "focus only; native controls unavailable"
            } else {
                "unavailable/no pane"
            };
            let metadata = vec![
                ("Name".into(), name.clone()),
                ("Kind".into(), self.runtime_agent_kind(&id).as_str().into()),
                ("State".into(), status),
                ("Pane".into(), pane.unwrap_or("unavailable").into()),
                (
                    "Window".into(),
                    descriptor
                        .as_ref()
                        .map(|d| d.window_id.to_string())
                        .unwrap_or_else(|| "unavailable".into()),
                ),
                (
                    "Objective".into(),
                    identity.objective.clone().unwrap_or_default(),
                ),
                ("Role".into(), identity.role.clone().unwrap_or_default()),
                (
                    "Group".into(),
                    descriptor
                        .as_ref()
                        .and_then(|descriptor| {
                            self.session
                                .window_groups()
                                .iter()
                                .find(|group| group.window_ids.contains(&descriptor.window_id))
                        })
                        .map(|group| group.id.to_string())
                        .unwrap_or_else(|| "unavailable".into()),
                ),
                (
                    "Conversation".into(),
                    session
                        .map(|session| session.session_id.clone())
                        .unwrap_or_default(),
                ),
                (
                    "Task".into(),
                    turn.map(|turn| turn.turn_id.clone()).unwrap_or_default(),
                ),
                ("Project".into(), project.label),
                ("Project status".into(), project.status),
                ("Controls".into(), controls.into()),
            ];
            records.push(RecordBrowserRecord {
                id: id.clone(),
                open_command: None,
                title: name,
                metadata,
                markdown: format!("Agent {id}"),
            });
            let pause_generation = pane.and_then(|pane| self.agent_human_pause_generation(pane));
            targets.insert(
                id,
                AgentBrowserTarget {
                    conversation: session.map(|session| session.session_id.clone()),
                    external_generation: external
                        .as_ref()
                        .and_then(|row| row["generation"].as_u64()),
                    identity,
                    process,
                    lifecycle,
                    pause_generation,
                },
            );
        }
        let mut browser = RecordBrowser::new("Live session agents", records, Vec::new())?;
        browser.set_table_columns(vec![
            "Name".into(),
            "Kind".into(),
            "State".into(),
            "Pane".into(),
            "Window".into(),
            "Group".into(),
            "Role".into(),
            "Objective".into(),
            "Project".into(),
        ]);
        browser.set_help(Some("**Keys:** Enter focus · i interrupt · p pause/resume · d confirm close · r refresh · / search · s save · Esc dismiss. Children may continue while their parent is paused.".into()), None);
        if !trust_available {
            browser.set_error(Some(
                "Project trust refresh failed; project labels unavailable.".into(),
            ));
        }
        Ok((browser, targets))
    }

    /// Qualifies current pane cwd before using the canonical-only trust resolver.
    /// At most one lookup per pane is cached within a snapshot, never across
    /// refreshes. The external row describes its bound pane, not vendor workspace.
    fn agent_browser_project(&self, pane: &str, trust_available: bool) -> AgentBrowserProject {
        use crate::security::project::{
            ProjectTrustProvenance, resolve_canonical_project_trust_provenance,
        };

        #[cfg(test)]
        PROJECT_LOOKUPS.with(|counts| {
            let (snapshots, panes) = counts.get();
            counts.set((snapshots, panes + 1));
        });
        if !trust_available || self.pane_process_identity(pane).is_err() {
            return AgentBrowserProject::unavailable();
        }
        let Some(store) = self.integration.project_trust_store() else {
            return AgentBrowserProject::unavailable();
        };
        let Some(cwd) = self
            .pane_current_working_directory(pane)
            .filter(|cwd| cwd.is_absolute())
        else {
            return AgentBrowserProject::unavailable();
        };
        let Ok(cwd) = std::fs::canonicalize(cwd) else {
            return AgentBrowserProject::unavailable();
        };
        if !cwd.is_dir() {
            return AgentBrowserProject::unavailable();
        }
        let provenance = resolve_canonical_project_trust_provenance(store, &cwd);
        match provenance {
            ProjectTrustProvenance::TrustedRoot(root) => AgentBrowserProject {
                label: root.to_string_lossy().into_owned(),
                status: "trusted".into(),
            },
            other => AgentBrowserProject {
                label: "—".into(),
                status: other.withheld_provenance().unwrap_or("no-decision").into(),
            },
        }
    }

    /// Revalidates live registration, conversation and exact root before focus.
    pub(crate) fn validate_agent_browser_target(
        &self,
        client: &ClientId,
        target: &AgentBrowserTarget,
    ) -> Result<String> {
        self.require_live()?;
        if !self.session.is_attached_primary(client) {
            return Err(MezError::forbidden(
                "agent management requires attached primary",
            ));
        }
        if self
            .message_service()
            .registered_identity(&target.identity.agent_id)
            != Some(&target.identity)
        {
            return Err(MezError::conflict("agent registration changed; refresh"));
        }
        let pane = target
            .identity
            .pane_id
            .as_ref()
            .ok_or_else(|| MezError::conflict("agent has no pane"))?
            .as_str();
        if self.find_pane_descriptor(pane).is_none()
            || target
                .process
                .as_ref()
                .is_none_or(|process| !self.pane_process_identity_is_current(pane, process))
        {
            return Err(MezError::conflict(
                "agent pane incarnation unavailable; refresh",
            ));
        }
        if self
            .agent_shell_store()
            .get(pane)
            .map(|session| session.session_id.as_str())
            != target.conversation.as_deref()
            || self
                .external_agent_metadata(target.identity.agent_id.as_str())
                .and_then(|row| row["generation"].as_u64())
                != target.external_generation
        {
            return Err(MezError::conflict("agent ownership changed; refresh"));
        }
        Ok(pane.into())
    }
}
