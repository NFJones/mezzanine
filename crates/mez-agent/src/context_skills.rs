//! Skill-related model-context action-surface constraints.
//!
//! This module recognizes already-returned skill catalogs and loaded skill
//! bodies in provider-bound messages, then removes redundant model-selected
//! discovery actions without depending on product skill storage or loading.

use crate::{AllowedAction, ContextSourceKind, ModelMessage, ModelMessageRole, ModelRequest};

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
    let state = skill_action_surface_context_from_messages(request.messages.iter());
    if !state.has_redundant_skill_state() {
        return;
    }
    request.allowed_actions.remove(AllowedAction::RequestSkills);
    if state.skill_loaded {
        request.allowed_actions.remove(AllowedAction::CallSkill);
    }
}

/// Extracts accumulated skill state from provider-bound messages.
fn skill_action_surface_context_from_messages<'a>(
    messages: impl IntoIterator<Item = &'a ModelMessage>,
) -> SkillActionSurfaceContext {
    let messages = messages.into_iter().collect::<Vec<_>>();
    let active_user_index = messages.iter().rposition(|message| {
        message.role == ModelMessageRole::User
            && message.source == ContextSourceKind::UserInstruction
    });
    let current_boundary_start = active_user_index.map_or(0, |active_user_index| {
        messages[..active_user_index]
            .iter()
            .rposition(|message| {
                matches!(
                    message.role,
                    ModelMessageRole::User | ModelMessageRole::Assistant | ModelMessageRole::Tool
                )
            })
            .map_or(0, |index| index.saturating_add(1))
    });
    let mut state = SkillActionSurfaceContext::default();
    for message in messages.into_iter().skip(current_boundary_start) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContextPlacement;

    fn message(role: ModelMessageRole, source: ContextSourceKind, content: &str) -> ModelMessage {
        ModelMessage {
            role,
            source,
            placement: ContextPlacement::ConversationAppend,
            content: content.to_string(),
        }
    }

    /// Verifies a skill loaded for a completed historical prompt does not
    /// suppress skill actions for a later ordinary user turn.
    #[test]
    fn historical_skill_context_does_not_narrow_current_action_surface() {
        let messages = vec![
            message(
                ModelMessageRole::Context,
                ContextSourceKind::SkillInstruction,
                "[explicit skill review]\nReview the requested files.",
            ),
            message(
                ModelMessageRole::User,
                ContextSourceKind::TranscriptUser,
                "$review inspect src/lib.rs",
            ),
            message(
                ModelMessageRole::Assistant,
                ContextSourceKind::TranscriptAssistant,
                "The review is complete.",
            ),
            message(
                ModelMessageRole::User,
                ContextSourceKind::UserInstruction,
                "Load another skill if useful.",
            ),
        ];

        assert_eq!(
            skill_action_surface_context_from_messages(&messages),
            SkillActionSurfaceContext::default()
        );
    }

    /// Verifies a skill prelude attached to the active user event still
    /// suppresses redundant discovery and loading actions for that turn.
    #[test]
    fn active_prompt_skill_context_narrows_current_action_surface() {
        let messages = vec![
            message(
                ModelMessageRole::Assistant,
                ContextSourceKind::TranscriptAssistant,
                "Prior work is complete.",
            ),
            message(
                ModelMessageRole::Context,
                ContextSourceKind::SkillInstruction,
                "[explicit skill review]\nReview the requested files.",
            ),
            message(
                ModelMessageRole::User,
                ContextSourceKind::UserInstruction,
                "$review inspect src/lib.rs",
            ),
        ];

        assert_eq!(
            skill_action_surface_context_from_messages(&messages),
            SkillActionSurfaceContext {
                catalog_loaded: false,
                skill_loaded: true,
            }
        );
    }
}
