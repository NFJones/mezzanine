//! Dormant original Codex command-hook sources and fresh callback recovery.
//!
//! Silence is unavailable telemetry, never observer health or producer death.
//! The exact first-epoch native source stays bounded in its existing registry
//! slot while its original creator/root remain live. This retains request and
//! replay custody without renewing a lease or granting visible presence. Only a
//! changed lifecycle fact, after common native worker/actor/order proof, restores
//! visibility and a finite lease. Explicit end, root/source replacement and
//! native lifetime loss still retire normally; no source or identity is revived.

use super::*;
use mez_agent::messaging::AgentPresenceStatus;

/// Run-local visibility only; this flag grants no source or freshness authority.
#[derive(Debug, Default)]
pub(super) struct CodexIdle {
    dormant: bool,
    /// Coalesced redraw debt survives suspension during request admission.
    visibility_dirty: bool,
}

impl EnrollmentBinding {
    /// Prevents stale proof or an unrelated generic update exposing dormant state.
    pub(in crate::runtime::control) fn codex_telemetry_dormant(&self) -> bool {
        self.codex_idle.dormant
    }

    /// Drains one real visibility transition for the existing actor redraw path.
    pub(in crate::runtime::control) fn take_codex_visibility_change(&mut self) -> bool {
        std::mem::take(&mut self.codex_idle.visibility_dirty)
    }
}

impl LaunchBinding {
    /// Retains only the exact original local-command source under its original
    /// native lifetime. The caller must also fence the current pane root.
    pub(in crate::runtime::control) fn can_wait_for_codex_callback(&self) -> bool {
        super::codex::original(self).is_ok_and(EnrollmentBinding::provenance_is_live)
    }
}

impl RuntimeSessionService {
    /// Hides expired original Codex telemetry once without renewing anything.
    /// Missing message identity fails closed through the caller's retirement
    /// path; no new identity, mailbox, source, generation or credential is made.
    pub(in crate::runtime::control) fn suspend_idle_codex_source(
        &mut self,
        digest: [u8; 32],
        now: u64,
    ) -> Result<()> {
        let Some(binding) = self.control.external_agents().bindings.get(&digest) else {
            return Ok(());
        };
        if !binding.can_wait_for_codex_callback()
            || binding.has_current_telemetry(now)
            || binding
                .enrollment
                .as_ref()
                .is_some_and(|source| source.codex_idle.dormant)
        {
            return Ok(());
        }
        let registration = binding
            .registration
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("Codex dormant registration unavailable"))?;
        let agent = registration.agent_id.clone();
        let pane = binding.pane_id.clone();
        let owner = format!("external-registration:{}", binding.generation);
        self.control.message_service_mut().update_presence(
            &agent,
            AgentPresenceStatus::Offline,
            now.saturating_mul(1000),
        )?;
        let source = self
            .control
            .external_agents_mut()
            .bindings
            .get_mut(&digest)
            .and_then(|binding| binding.enrollment.as_mut())
            .ok_or_else(|| MezError::invalid_state("Codex dormant source disappeared"))?;
        source.codex_idle.dormant = true;
        source.codex_idle.visibility_dirty = true;
        self.presentation
            .set_pane_harness_status(&pane, &owner, None);
        Ok(())
    }

    /// Restores visibility only after a changed native-authorized lifecycle fact
    /// published its new finite proof. Identical callbacks, source declarations,
    /// model counters and parent survival cannot call this recovery path.
    /// An inconsistent message identity retires rather than recreates a source.
    pub(super) fn recover_codex_after_changed_fact(&mut self, digest: [u8; 32]) -> Result<()> {
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .ok_or_else(|| MezError::forbidden("Codex recovery source unavailable"))?;
        let source = super::codex::original(binding)?;
        if !source.codex_idle.dormant {
            return Ok(());
        }
        if !source
            .curated_observer
            .as_ref()
            .is_some_and(|proof| proof.is_recent())
        {
            return Err(MezError::forbidden("Codex recovery proof unavailable"));
        }
        let agent = binding
            .registration
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("Codex recovery registration unavailable"))?
            .agent_id
            .clone();
        let now = current_unix_seconds();
        if let Err(error) = self.control.message_service_mut().update_presence(
            &agent,
            AgentPresenceStatus::Available,
            now.saturating_mul(1000),
        ) {
            self.retire_external_agent_binding(digest);
            return Err(error.into());
        }
        let binding = self
            .control
            .external_agents_mut()
            .bindings
            .get_mut(&digest)
            .ok_or_else(|| MezError::invalid_state("Codex recovery source disappeared"))?;
        let source = binding
            .enrollment
            .as_mut()
            .ok_or_else(|| MezError::invalid_state("Codex recovery owner disappeared"))?;
        source.codex_idle.dormant = false;
        source.codex_idle.visibility_dirty = true;
        binding.expires = now.saturating_add(super::super::external_agents::LEASE_SECONDS);
        Ok(())
    }
}
