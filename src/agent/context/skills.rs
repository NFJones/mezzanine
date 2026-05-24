//! Skill-related context surface constraints.
//!
//! This module owns the small amount of model-request shaping needed after
//! skill catalog or skill-body context has already been loaded into a turn.

use super::{AllowedAction, ModelMessage, ModelRequest};

/// Runtime-visible skill state already present in provider-bound context.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SkillActionSurfaceContext {
    /// Whether the skill catalog has already been returned for this turn.
    catalog_loaded: bool,
    /// Whether a full skill body has already been loaded for this turn.
    skill_loaded: bool,
}

impl SkillActionSurfaceContext {
    /// Merges one observed state into this accumulated state.
    fn merge(&mut self, other: Self) {
        self.catalog_loaded |= other.catalog_loaded;
        self.skill_loaded |= other.skill_loaded;
    }

    /// Returns whether any skill action should be hidden from this request.
    fn has_redundant_skill_state(self) -> bool {
        self.catalog_loaded || self.skill_loaded
    }
}

/// Applies runtime skill-action constraints to one request's action surface.
///
/// Skill lookup and load actions are intentionally non-effecting continuation
/// aids. When the current context already contains the catalog or a loaded
/// skill body, keeping those actions in the strict provider surface invites
/// models to rediscover or reload the same workflow instead of requesting the
/// concrete action capability needed for the task.
pub fn constrain_skill_actions_for_loaded_context(request: &mut ModelRequest) {
    let state = skill_action_surface_context_from_messages(&request.messages);
    if !state.has_redundant_skill_state() {
        return;
    }
    if state.catalog_loaded || state.skill_loaded {
        request.allowed_actions.remove(AllowedAction::RequestSkills);
    }
    if state.skill_loaded {
        request.allowed_actions.remove(AllowedAction::CallSkill);
    }
}

/// Extracts skill-action surface state from provider-bound messages.
fn skill_action_surface_context_from_messages(
    messages: &[ModelMessage],
) -> SkillActionSurfaceContext {
    let mut state = SkillActionSurfaceContext::default();
    for message in messages {
        state.merge(skill_action_surface_context_from_text(&message.content));
    }
    state
}

/// Extracts skill-action surface state from one context text block.
fn skill_action_surface_context_from_text(content: &str) -> SkillActionSurfaceContext {
    let mut state = SkillActionSurfaceContext::default();
    if content.lines().next().is_some_and(|line| {
        line.starts_with("[explicit skill ") || line.starts_with("[explicit skill invocation ")
    }) {
        state.skill_loaded = true;
    }
    for line in content.lines() {
        if line.starts_with("[action_result ")
            && line.contains(" request_skills ")
            && line.ends_with(" succeeded]")
        {
            state.catalog_loaded = true;
        }
        if line.starts_with("[action_result ")
            && line.contains(" call_skill ")
            && line.ends_with(" succeeded]")
        {
            state.skill_loaded = true;
        }
    }
    state
}
