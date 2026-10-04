//! Retained conversation-title inputs for storage-free composer presentation.
//!
//! Lifecycle owners install bounded metadata only after accepted mutations or
//! prepared restore. Rendering resolves existing title policy from these inputs;
//! it never loads a catalog, schedules a provider, or borrows another pane's name.
//! Cache entries are pruned to live conversation bindings during updates.

use super::RuntimeSessionService;

/// Exact bounded title-cache snapshot retained by compound binding rollback.
#[derive(Clone)]
pub(crate) struct ComposerTitleSnapshot(std::collections::BTreeMap<String, ComposerTitleInputs>);

/// Bounded display-only title inputs, independent of generated agent identity.
#[derive(Debug, Clone, Default)]
pub(super) struct ComposerTitleInputs {
    name: Option<String>,
    generated: Option<String>,
    objective: Option<String>,
    first_prompt: Option<String>,
    latest_prompt: Option<String>,
}

impl RuntimeSessionService {
    /// Captures title inputs before temporary bindings can prune their owner.
    pub(crate) fn snapshot_composer_titles(&self) -> ComposerTitleSnapshot {
        ComposerTitleSnapshot(self.agent.composer_titles.clone())
    }

    /// Restores exact inputs after rejected binding work, without storage reads.
    pub(crate) fn restore_composer_titles(&mut self, snapshot: ComposerTitleSnapshot) {
        self.agent.composer_titles = snapshot.0;
    }

    /// Resolves the currently bound conversation from retained inputs only.
    pub(crate) fn composer_session_title(&self, pane: &str) -> Option<String> {
        let session = self.agent_shell_store().get(pane)?;
        let inputs = self.agent.composer_titles.get(&session.session_id)?;
        crate::session_title::resolve_session_title(
            inputs.name.as_deref(),
            self.agent_session_title_policy(),
            inputs.generated.as_deref(),
            inputs.objective.as_deref(),
            inputs.first_prompt.as_deref(),
            inputs.latest_prompt.as_deref(),
        )
        .map(|title| title.text)
    }

    /// Updates accepted metadata and queues redraw only when its resolved value changes.
    fn update_composer_title(
        &mut self,
        conversation: &str,
        update: impl FnOnce(&mut ComposerTitleInputs),
    ) {
        let panes = self
            .agent_shell_store()
            .sessions()
            .filter(|session| session.session_id == conversation)
            .map(|session| session.pane_id.clone())
            .collect::<Vec<_>>();
        if panes.is_empty() {
            return;
        }
        let before = self.composer_session_title(&panes[0]);
        self.prune_composer_titles();
        update(
            self.agent
                .composer_titles
                .entry(conversation.into())
                .or_default(),
        );
        if before != self.composer_session_title(&panes[0]) {
            self.invalidate_composer_title_projections(&panes);
        }
    }

    /// Invalidates only attached projections displaying a read-only title.
    /// Focused primary composers use editing labels, while observers inherit
    /// their source window and display all composers read-only.
    pub(crate) fn invalidate_composer_title_projections(&mut self, panes: &[String]) {
        use mez_mux::session::{ClientRole, ClientState};
        let effects: Vec<_> = self
            .session
            .clients()
            .iter()
            .filter(|client| client.state == ClientState::Attached)
            .filter(|client| {
                let source = if client.role == ClientRole::Primary {
                    Some(&client.id)
                } else if client.role == ClientRole::Observer {
                    self.session
                        .observer_attachments()
                        .iter()
                        .find(|observer| observer.client_id == client.id)
                        .map(|observer| &observer.view_source_client_id)
                } else {
                    None
                };
                let Some(source) = source else {
                    return false;
                };
                let Ok(window) = self.session.active_window_for(source) else {
                    return false;
                };
                panes.iter().any(|pane| {
                    window
                        .panes()
                        .iter()
                        .any(|candidate| candidate.id.as_str() == pane)
                        && self.agent_shell_store().get(pane).is_some_and(|session| {
                            session.visibility == mez_agent::AgentShellVisibility::Visible
                        })
                        && (client.role == ClientRole::Observer
                            || self
                                .session
                                .active_pane_for(source)
                                .is_ok_and(|active| active.id.as_str() != pane))
                })
            })
            .map(|client| crate::runtime::RuntimeSideEffect::RenderClient {
                client_id: client.id.clone(),
                reason: crate::runtime::RenderInvalidationReason::FullRedraw,
            })
            .collect();
        self.presentation.defer_render_effects(effects);
    }

    /// Hydrates already-read saved metadata at an accepted conversation binding.
    pub(crate) fn install_composer_saved_title(
        &mut self,
        saved: &crate::storage::transcript::SavedAgentSession,
    ) {
        self.update_composer_title(&saved.summary.conversation_id, |inputs| {
            *inputs = ComposerTitleInputs {
                name: saved
                    .name
                    .as_deref()
                    .and_then(crate::session_title::bound_session_title),
                generated: saved
                    .generated_title
                    .as_deref()
                    .and_then(crate::session_title::bound_session_title),
                objective: saved
                    .objective_title
                    .as_deref()
                    .and_then(crate::session_title::bound_session_title),
                first_prompt: saved
                    .summary
                    .initial_prompt
                    .as_deref()
                    .and_then(crate::session_title::bound_session_title),
                latest_prompt: saved
                    .summary
                    .latest_user_prompt
                    .as_deref()
                    .and_then(crate::session_title::bound_session_title),
            };
        });
    }

    /// Prunes only unbound conversations, retaining frozen loop-parent inputs.
    pub(crate) fn prune_composer_titles(&mut self) {
        let mut live = self
            .agent_shell_store()
            .sessions()
            .map(|session| session.session_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        live.extend(
            self.agent
                .agent_loops_by_id
                .values()
                .map(|state| state.parent_conversation_id.clone()),
        );
        self.agent.composer_titles.retain(|id, _| live.contains(id));
    }

    /// Hydrates at explicit entry/startup, never from rendering or focus changes.
    pub(crate) fn hydrate_composer_title(&mut self, conversation: &str) {
        if let Some(store) = self.persistence.cloned_transcript_store()
            && let Ok(Some(saved)) = store.saved_session(conversation)
        {
            self.install_composer_saved_title(&saved);
        }
    }

    /// Retains a successfully persisted manual name or explicit clear.
    pub(crate) fn set_composer_manual_title(&mut self, conversation: &str, name: Option<&str>) {
        let name = name.and_then(crate::session_title::bound_session_title);
        self.update_composer_title(conversation, |inputs| inputs.name = name);
    }

    /// Retains only a successfully persisted generated title.
    pub(crate) fn set_composer_generated_title(&mut self, conversation: &str, title: &str) {
        let title = crate::session_title::bound_session_title(title);
        self.update_composer_title(conversation, |inputs| inputs.generated = title);
    }

    /// Captures bounded prompt inputs after the user turn is accepted.
    pub(crate) fn note_composer_prompt(&mut self, conversation: &str, prompt: &str) {
        let prompt = crate::session_title::bound_session_title(prompt);
        self.update_composer_title(conversation, |inputs| {
            if inputs.first_prompt.is_none() {
                inputs.first_prompt = prompt.clone();
            }
            inputs.latest_prompt = prompt;
        });
    }

    /// Retains the accepted published objective even without durable storage.
    pub(crate) fn note_composer_objective(&mut self, conversation: &str, objective: Option<&str>) {
        let objective = objective.and_then(crate::session_title::bound_session_title);
        self.update_composer_title(conversation, |inputs| inputs.objective = objective);
    }
}
