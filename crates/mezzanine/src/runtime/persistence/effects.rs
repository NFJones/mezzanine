//! Deferred external-effect queue operations.

use super::{RuntimePersistenceComponent, RuntimeSideEffect, TerminalSize};

impl RuntimePersistenceComponent {
    /// Queues one pane-input effect in dispatch order.
    pub(crate) fn queue_pane_input(&mut self, effect: RuntimeSideEffect) {
        self.queued_pane_input_effects.push(effect);
    }

    /// Replaces the queued resize for one pane.
    pub(crate) fn queue_pane_resize(
        &mut self,
        pane_id: impl Into<String>,
        effect: RuntimeSideEffect,
    ) {
        let pane_id = pane_id.into();
        if let RuntimeSideEffect::PaneProcessIo {
            effect: crate::runtime::PaneProcessIoEffect::Resize { size },
            ..
        } = &effect
        {
            self.expected_pane_resize_sizes
                .insert(pane_id.clone(), *size);
        }
        self.queued_pane_resize_effects.insert(pane_id, effect);
    }

    /// Consumes the expected async resize size when one completion arrives.
    ///
    /// Returns `false` only when a later queued resize has made this completion
    /// stale; completions without a tracked adapter request remain valid.
    pub(crate) fn accept_pane_resize_completion(
        &mut self,
        pane_id: &str,
        size: TerminalSize,
    ) -> bool {
        let Some(expected) = self.expected_pane_resize_sizes.get(pane_id) else {
            return true;
        };
        if *expected != size {
            return false;
        }
        self.expected_pane_resize_sizes.remove(pane_id);
        true
    }

    /// Replaces the queued termination for one pane.
    pub(crate) fn queue_pane_termination(
        &mut self,
        pane_id: impl Into<String>,
        effect: RuntimeSideEffect,
    ) {
        self.queued_pane_termination_effects
            .insert(pane_id.into(), effect);
    }

    /// Drains input, resize, and termination effects in canonical order.
    pub(crate) fn take_pane_io_effects(&mut self) -> Vec<RuntimeSideEffect> {
        let mut effects = std::mem::take(&mut self.queued_pane_input_effects);
        effects.extend(std::mem::take(&mut self.queued_pane_resize_effects).into_values());
        effects.extend(std::mem::take(&mut self.queued_pane_termination_effects).into_values());
        effects
    }

    /// Removes obsolete queued pane I/O and pipe effects for a closed pane.
    ///
    /// A queued termination is deliberately retained: pane cleanup runs after
    /// the runtime has requested termination from an async process owner, and
    /// that owner still needs the effect even though the pane has left the
    /// session layout.
    pub(crate) fn cleanup_pane_io(&mut self, pane_id: &str) {
        self.queued_pane_input_effects
            .retain(|effect| match effect {
                RuntimeSideEffect::WritePaneInput {
                    pane_id: target, ..
                }
                | RuntimeSideEffect::WritePaneInputPriority {
                    pane_id: target, ..
                } => target != pane_id,
                _ => true,
            });
        self.queued_pane_resize_effects.remove(pane_id);
        self.expected_pane_resize_sizes.remove(pane_id);
        self.queued_pane_pipe_effects
            .retain(|(queued_pane_id, _)| queued_pane_id != pane_id);
    }

    /// Queues one pane-pipe effect together with its cleanup owner.
    pub(crate) fn queue_pane_pipe(
        &mut self,
        pane_id: impl Into<String>,
        effect: RuntimeSideEffect,
    ) {
        self.queued_pane_pipe_effects.push((pane_id.into(), effect));
    }

    /// Drains pane-pipe effects while discarding cleanup keys.
    pub(crate) fn take_pane_pipe_effects(&mut self) -> Vec<RuntimeSideEffect> {
        std::mem::take(&mut self.queued_pane_pipe_effects)
            .into_iter()
            .map(|(_, effect)| effect)
            .collect()
    }

    /// Queues one audit persistence effect.
    pub(crate) fn queue_audit(&mut self, effect: RuntimeSideEffect) {
        self.queued_audit_effects.push(effect);
    }

    /// Drains queued audit persistence effects.
    pub(crate) fn take_audit_effects(&mut self) -> Vec<RuntimeSideEffect> {
        std::mem::take(&mut self.queued_audit_effects)
    }

    /// Queues one transcript or prompt-history persistence effect.
    pub(crate) fn queue_transcript(&mut self, effect: RuntimeSideEffect) {
        self.queued_transcript_effects.push(effect);
    }

    /// Drains queued transcript and prompt-history effects.
    pub(crate) fn take_transcript_effects(&mut self) -> Vec<RuntimeSideEffect> {
        std::mem::take(&mut self.queued_transcript_effects)
    }

    /// Queues one configuration persistence effect.
    pub(crate) fn queue_config(&mut self, effect: RuntimeSideEffect) {
        self.queued_config_effects.push(effect);
    }

    /// Drains queued configuration persistence effects.
    pub(crate) fn take_config_effects(&mut self) -> Vec<RuntimeSideEffect> {
        std::mem::take(&mut self.queued_config_effects)
    }

    /// Queues one non-blocking program-hook effect.
    pub(crate) fn queue_program_hook(&mut self, effect: RuntimeSideEffect) {
        self.queued_program_hook_effects.push(effect);
    }

    /// Drains queued non-blocking program-hook effects.
    pub(crate) fn take_program_hook_effects(&mut self) -> Vec<RuntimeSideEffect> {
        std::mem::take(&mut self.queued_program_hook_effects)
    }
}
