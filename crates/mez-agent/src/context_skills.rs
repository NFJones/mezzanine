//! Skill-related model-context action-surface constraints.
//!
//! This module recognizes already-returned skill catalogs and loaded skill
//! bodies in provider-bound messages, then removes redundant model-selected
//! discovery actions without depending on product skill storage or loading.

use crate::{AllowedAction, ModelMessage, ModelRequest};

/// Skill state already visible in one provider request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SkillActionSurfaceContext {
    catalog_loaded: bool,
    skill_loaded: bool,
}

impl SkillActionSurfaceContext {
    /// Merges one observation into accumulated skill state.
    fn merge(&mut self, other: Self) {
        self.catalog_loaded |= other.catalog_loaded;
        self.skill_loaded |= other.skill_loaded;
    }

    /// Returns whether any skill action is redundant for this request.
    fn has_redundant_skill_state(self) -> bool {
        self.catalog_loaded || self.skill_loaded
    }
}

/// Removes redundant skill actions after catalog or skill context is loaded.
pub fn constrain_skill_actions_for_loaded_context(request: &mut ModelRequest) {
    let state = skill_action_surface_context_from_messages(&request.messages);
    if !state.has_redundant_skill_state() {
        return;
    }
    request.allowed_actions.remove(AllowedAction::RequestSkills);
    if state.skill_loaded {
        request.allowed_actions.remove(AllowedAction::CallSkill);
    }
}

/// Extracts accumulated skill state from provider-bound messages.
fn skill_action_surface_context_from_messages(
    messages: &[ModelMessage],
) -> SkillActionSurfaceContext {
    let mut state = SkillActionSurfaceContext::default();
    for message in messages {
        state.merge(skill_action_surface_context_from_text(&message.content));
    }
    state
}

/// Extracts skill state from one context payload.
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
