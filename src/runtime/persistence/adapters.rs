//! External persistence-adapter ownership flags.

use super::RuntimePersistenceComponent;

impl RuntimePersistenceComponent {
    /// Assigns audit persistence to the external effect adapter.
    pub(crate) fn enable_audit_adapter(&mut self) {
        self.audit_effects_use_adapter = true;
    }

    /// Reports whether audit persistence is adapter-owned.
    pub(crate) fn audit_uses_adapter(&self) -> bool {
        self.audit_effects_use_adapter
    }

    /// Assigns pane-pipe execution and persistence to external adapters.
    pub(crate) fn enable_pane_pipe_adapter(&mut self) {
        self.pane_pipe_effects_use_adapter = true;
    }

    /// Reports whether pane-pipe work is adapter-owned.
    pub(crate) fn pane_pipe_uses_adapter(&self) -> bool {
        self.pane_pipe_effects_use_adapter
    }

    /// Assigns transcript persistence to the external effect adapter.
    pub(crate) fn enable_transcript_adapter(&mut self) {
        self.transcript_effects_use_adapter = true;
    }

    /// Reports whether transcript persistence is adapter-owned.
    pub(crate) fn transcript_uses_adapter(&self) -> bool {
        self.transcript_effects_use_adapter
    }

    /// Assigns session-registry persistence to the external effect adapter.
    pub(crate) fn enable_registry_adapter(&mut self) {
        self.registry_effects_use_adapter = true;
    }

    /// Reports whether registry persistence is adapter-owned.
    pub(crate) fn registry_uses_adapter(&self) -> bool {
        self.registry_effects_use_adapter
    }

    /// Assigns configuration persistence to the external effect adapter.
    pub(crate) fn enable_config_adapter(&mut self) {
        self.config_effects_use_adapter = true;
    }

    /// Reports whether configuration persistence is adapter-owned.
    pub(crate) fn config_uses_adapter(&self) -> bool {
        self.config_effects_use_adapter
    }

    /// Assigns non-blocking program-hook execution to an external adapter.
    pub(crate) fn enable_hook_adapter(&mut self) {
        self.hook_effects_use_adapter = true;
    }

    /// Reports whether program-hook execution is adapter-owned.
    pub(crate) fn hook_uses_adapter(&self) -> bool {
        self.hook_effects_use_adapter
    }
}
