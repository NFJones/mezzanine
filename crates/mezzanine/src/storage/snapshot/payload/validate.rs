//! Payload content detection, validation, and resume-plan projection.

use std::collections::HashSet;

use super::helpers::{validate_message_snapshot_state, validate_snapshot_window_groups};
use super::{LayoutLoadPlan, MezError, Result, SessionSnapshotPayload};
use crate::storage::snapshot::types::MAX_UNSETTLED_PEER_PRESENTATIONS;
impl SessionSnapshotPayload {
    /// Runs the contains terminal history operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn contains_terminal_history(&self) -> bool {
        self.windows.iter().any(|window| {
            window.panes.iter().any(|pane| {
                !pane.terminal_history.is_empty()
                    || (!pane.alternate_screen_active && !pane.visible_lines.is_empty())
            })
        })
    }

    /// Runs the contains agent transcripts operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn contains_agent_transcripts(&self) -> bool {
        self.windows.iter().any(|window| {
            window
                .panes
                .iter()
                .any(|pane| !pane.transcript_refs.is_empty())
        })
    }

    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn validate(&self) -> Result<()> {
        if self.session_id.is_empty() || self.name.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot payload session identity fields must not be empty",
            ));
        }
        if self.authoritative_columns == 0 || self.authoritative_rows == 0 {
            return Err(MezError::invalid_args(
                "snapshot payload authoritative size must be non-zero",
            ));
        }
        self.shell.validate()?;
        for layer in &self.active_config_layers {
            layer.validate()?;
        }
        self.frame_state.validate()?;
        for agent_session in &self.agent_sessions {
            agent_session.validate()?;
        }
        for grant in &self.approval_grants {
            grant.validate()?;
        }
        for request in &self.approval_requests {
            request.validate()?;
        }
        if let Some(message_state) = &self.message_state {
            validate_message_snapshot_state(message_state)?;
        }
        if self.unsettled_peer_presentations.len() > MAX_UNSETTLED_PEER_PRESENTATIONS {
            return Err(MezError::invalid_args(
                "snapshot peer presentation outbox exceeds its entry bound",
            ));
        }
        if self.payload_version >= 6
            && !self.unsettled_peer_presentations.is_empty()
            && self.message_state.is_none()
        {
            return Err(MezError::invalid_args(
                "snapshot peer presentation outbox requires message state",
            ));
        }
        let snapshot_pane_owners = self
            .windows
            .iter()
            .flat_map(|window| {
                window
                    .panes
                    .iter()
                    .map(move |pane| (pane.pane_id.as_str(), window.window_id.as_str()))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let mut peer_receipt_identities = HashSet::new();
        for receipt in &self.unsettled_peer_presentations {
            receipt.validate()?;
            let Some(window_id) = snapshot_pane_owners.get(receipt.pane_id.as_str()) else {
                return Err(MezError::invalid_args(
                    "snapshot peer presentation outbox pane is unknown",
                ));
            };
            if !peer_receipt_identities.insert(receipt.identity.as_str()) {
                return Err(MezError::invalid_args(
                    "snapshot peer presentation outbox identities must be unique",
                ));
            }
            let Some(message_state) = self.message_state.as_ref() else {
                continue;
            };
            let Some(identity) = message_state
                .registered_agents
                .iter()
                .find(|identity| identity.agent_id == receipt.recipient_agent_id)
            else {
                return Err(MezError::invalid_args(
                    "snapshot peer presentation outbox recipient identity is missing",
                ));
            };
            if identity.pane_id.as_deref() != Some(receipt.pane_id.as_str()) {
                return Err(MezError::invalid_args(
                    "snapshot peer presentation outbox recipient pane does not match",
                ));
            }
            if identity
                .window_id
                .as_deref()
                .is_some_and(|identity_window_id| identity_window_id != *window_id)
            {
                return Err(MezError::invalid_args(
                    "snapshot peer presentation outbox recipient window does not own pane",
                ));
            }
            let Some(subscription) = message_state
                .subscriptions
                .iter()
                .find(|subscription| subscription.recipient == receipt.recipient_agent_id)
            else {
                return Err(MezError::invalid_args(
                    "snapshot peer presentation outbox recipient subscription is missing",
                ));
            };
            if subscription.last_sequence < receipt.sequence {
                return Err(MezError::invalid_args(
                    "snapshot peer presentation outbox receipt exceeds recipient cursor",
                ));
            }
            if !self.agent_sessions.iter().any(|session| {
                session.pane_id == receipt.pane_id
                    && session.conversation_id == receipt.conversation_id
            }) {
                return Err(MezError::invalid_args(
                    "snapshot peer presentation outbox pane conversation owner is missing",
                ));
            }
        }
        for server in &self.mcp_servers {
            server.validate()?;
        }
        for group in &self.window_groups {
            group.validate()?;
        }
        for window in &self.windows {
            window.validate()?;
        }
        validate_snapshot_window_groups(self)?;
        self.validate_landing_navigation()?;
        Ok(())
    }

    /// Validates the v5 landing group/window/pane parent chain.
    fn validate_landing_navigation(&self) -> Result<()> {
        let group_id = self
            .landing_navigation
            .active_group_id
            .as_deref()
            .ok_or_else(|| MezError::invalid_args("snapshot landing group is missing"))?;
        let window_id = self
            .landing_navigation
            .active_window_id
            .as_deref()
            .ok_or_else(|| MezError::invalid_args("snapshot landing window is missing"))?;
        let pane_id = self
            .landing_navigation
            .active_pane_id
            .as_deref()
            .ok_or_else(|| MezError::invalid_args("snapshot landing pane is missing"))?;
        let group = self
            .window_groups
            .iter()
            .find(|group| group.group_id == group_id)
            .ok_or_else(|| MezError::invalid_args("snapshot landing group is unknown"))?;
        if !group.window_ids.iter().any(|id| id == window_id) {
            return Err(MezError::invalid_args(
                "snapshot landing window is not in the landing group",
            ));
        }
        let window = self
            .windows
            .iter()
            .find(|window| window.window_id == window_id)
            .ok_or_else(|| MezError::invalid_args("snapshot landing window is unknown"))?;
        if !window.panes.iter().any(|pane| pane.pane_id == pane_id) {
            return Err(MezError::invalid_args(
                "snapshot landing pane is not in the landing window",
            ));
        }
        Ok(())
    }

    /// Runs the resume plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resume_plan(&self) -> LayoutLoadPlan {
        let restart_required_panes = self
            .windows
            .iter()
            .flat_map(|window| window.panes.iter())
            .filter(|pane| pane.live_at_snapshot)
            .map(|pane| pane.pane_id.clone())
            .collect::<Vec<_>>();
        let pane_count = self
            .windows
            .iter()
            .map(|window| window.panes.len())
            .sum::<usize>();
        let running_agent_sessions = self
            .agent_sessions
            .iter()
            .filter(|session| session.running_turn_id.is_some())
            .map(|session| session.pane_id.clone())
            .collect::<Vec<_>>();
        let mut limitations = if restart_required_panes.is_empty() {
            Vec::new()
        } else {
            vec![
                "pane primary processes cannot be restored from snapshot and must be restarted"
                    .to_string(),
            ]
        };
        if !running_agent_sessions.is_empty() {
            limitations.push(
                "running agent turns are restored as interrupted and require explicit user confirmation before retrying non-idempotent actions"
                    .to_string(),
            );
        }
        if !self.mcp_servers.is_empty() {
            limitations.push(
                "MCP runtime transports are not restored from snapshot metadata and must be rediscovered"
                    .to_string(),
            );
        }

        LayoutLoadPlan {
            session_id: self.session_id.clone(),
            window_count: self.windows.len(),
            pane_count,
            restart_required_panes,
            limitations,
        }
    }
}
