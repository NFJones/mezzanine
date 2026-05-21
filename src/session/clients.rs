//! Client attachment, primary ownership, and observer-request operations.
//!
//! Client methods enforce primary exclusivity, observer approval visibility,
//! control-client role restrictions, and detach semantics.

use crate::error::{MezError, Result};
use crate::ids::{ClientId, ObserverRequestId};

use super::time::current_unix_seconds;
use super::types::{
    Client, ClientRole, ClientState, ClientTerminalDescriptor, ObserverDecisionState,
    ObserverRequest, Session, SessionState,
};

impl Session {
    /// Runs the attach primary operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn attach_primary(
        &mut self,
        name: impl Into<String>,
        interactive: bool,
    ) -> Result<ClientId> {
        self.attach_primary_with_terminal(name, interactive, None)
    }

    /// Runs the attach primary with terminal operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn attach_primary_with_terminal(
        &mut self,
        name: impl Into<String>,
        interactive: bool,
        terminal: Option<ClientTerminalDescriptor>,
    ) -> Result<ClientId> {
        if !interactive {
            return Err(MezError::forbidden(
                "primary clients must attach through an interactive terminal",
            ));
        }
        if let Some(terminal) = terminal.as_ref() {
            validate_client_terminal_descriptor(terminal)?;
        }
        if self.primary_client_id.is_some() {
            return Err(MezError::conflict(
                "session already has an attached primary client",
            ));
        }

        let client_id = self.ids.client();
        let attached_at = current_unix_seconds();
        self.clients.push(Client {
            id: client_id.clone(),
            name: name.into(),
            role: ClientRole::Primary,
            state: ClientState::Attached,
            interactive,
            terminal,
            attached_at_unix_seconds: Some(attached_at),
            last_seen_at_unix_seconds: Some(attached_at),
        });
        self.primary_client_id = Some(client_id.clone());
        self.last_attached_at_unix_seconds = Some(attached_at);
        self.record_event();
        Ok(client_id)
    }

    /// Runs the select primary client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn select_primary_client(
        &mut self,
        authority_client_id: Option<&ClientId>,
        target_client_id: &str,
    ) -> Result<ClientId> {
        if let Some(current_primary) = self.primary_client_id.as_ref()
            && authority_client_id != Some(current_primary)
        {
            return Err(MezError::forbidden(
                "primary transfer requires the attached primary client",
            ));
        }

        let target_index = self
            .clients
            .iter()
            .position(|client| client.id.as_str() == target_client_id)
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "client not found")
            })?;
        if !self.clients[target_index].interactive {
            return Err(MezError::forbidden(
                "primary client selection requires an interactive target client",
            ));
        }

        for client in &mut self.clients {
            if matches!(client.role, ClientRole::Primary) {
                client.role = ClientRole::Automation;
            }
        }
        let target_id = self.clients[target_index].id.clone();
        let selected_at = current_unix_seconds();
        self.clients[target_index].role = ClientRole::Primary;
        self.clients[target_index].state = ClientState::Attached;
        self.clients[target_index]
            .attached_at_unix_seconds
            .get_or_insert(selected_at);
        self.clients[target_index].last_seen_at_unix_seconds = Some(selected_at);
        self.primary_client_id = Some(target_id.clone());
        self.last_attached_at_unix_seconds = Some(selected_at);
        self.state = SessionState::Running;
        self.record_event();
        Ok(target_id)
    }

    /// Runs the request observer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn request_observer(&mut self, name: impl Into<String>) -> (ClientId, ObserverRequestId) {
        self.request_observer_with_terminal(name, None)
    }

    /// Runs the request observer with terminal operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn request_observer_with_terminal(
        &mut self,
        name: impl Into<String>,
        terminal: Option<ClientTerminalDescriptor>,
    ) -> (ClientId, ObserverRequestId) {
        let name = name.into();
        let client_id = self.ids.client();
        let observer_id = self.ids.observer_request();
        self.clients.push(Client {
            id: client_id.clone(),
            name: name.clone(),
            role: ClientRole::PendingObserver,
            state: ClientState::Pending,
            interactive: false,
            terminal: None,
            attached_at_unix_seconds: None,
            last_seen_at_unix_seconds: None,
        });
        self.observers.push(ObserverRequest {
            id: observer_id.clone(),
            client_id: client_id.clone(),
            state: ObserverDecisionState::Pending,
            descriptor_name: name,
            descriptor_interactive: false,
            descriptor_terminal: terminal,
            requested_at_unix_seconds: Some(current_unix_seconds()),
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            visible_from_event_id: None,
            visible_from_unix_seconds: None,
            reason: None,
        });
        self.record_event();
        (client_id, observer_id)
    }

    /// Runs the attach control client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn attach_control_client(
        &mut self,
        name: impl Into<String>,
        role: ClientRole,
        interactive: bool,
    ) -> Result<ClientId> {
        if matches!(
            role,
            ClientRole::Primary | ClientRole::PendingObserver | ClientRole::Observer
        ) {
            return Err(MezError::invalid_args(
                "attach_control_client supports only agent and automation roles",
            ));
        }
        let client_id = self.ids.client();
        let attached_at = current_unix_seconds();
        self.clients.push(Client {
            id: client_id.clone(),
            name: name.into(),
            role,
            state: ClientState::Attached,
            interactive,
            terminal: None,
            attached_at_unix_seconds: Some(attached_at),
            last_seen_at_unix_seconds: Some(attached_at),
        });
        self.record_event();
        Ok(client_id)
    }

    /// Runs the approve observer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn approve_observer(
        &mut self,
        primary_client_id: &ClientId,
        observer_id: &ObserverRequestId,
    ) -> Result<()> {
        self.approve_observer_target(primary_client_id, observer_id.as_str())
    }

    /// Runs the approve observer target operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn approve_observer_target(
        &mut self,
        primary_client_id: &ClientId,
        observer_id: &str,
    ) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let visible_from_event_id = self.record_event();
        self.approve_observer_target_with_visible_from(
            primary_client_id,
            observer_id,
            visible_from_event_id,
        )
    }

    /// Runs the approve observer target with visible from event id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn approve_observer_target_with_visible_from_event_id(
        &mut self,
        primary_client_id: &ClientId,
        observer_id: &str,
        visible_from_event_id: u64,
    ) -> Result<()> {
        self.require_primary(primary_client_id)?;
        self.record_event();
        self.approve_observer_target_with_visible_from(
            primary_client_id,
            observer_id,
            visible_from_event_id,
        )
    }

    /// Runs the approve observer target with visible from operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn approve_observer_target_with_visible_from(
        &mut self,
        primary_client_id: &ClientId,
        observer_id: &str,
        visible_from_event_id: u64,
    ) -> Result<()> {
        let observer = self
            .observers
            .iter_mut()
            .find(|observer| observer.id.as_str() == observer_id)
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "observer not found")
            })?;

        let decided_at = current_unix_seconds();
        observer.state = ObserverDecisionState::Approved;
        observer.decided_at_unix_seconds = Some(decided_at);
        observer.decided_by_client_id = Some(primary_client_id.to_string());
        observer.visible_from_event_id = Some(visible_from_event_id);
        observer.visible_from_unix_seconds = Some(decided_at);

        if let Some(client) = self
            .clients
            .iter_mut()
            .find(|client| client.id == observer.client_id)
        {
            client.role = ClientRole::Observer;
            client.state = ClientState::Attached;
            client.attached_at_unix_seconds = Some(decided_at);
            client.last_seen_at_unix_seconds = Some(decided_at);
        }

        Ok(())
    }

    /// Runs the reject observer target operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn reject_observer_target(
        &mut self,
        primary_client_id: &ClientId,
        observer_id: &str,
    ) -> Result<()> {
        self.reject_observer_target_with_reason(primary_client_id, observer_id, None)
    }

    /// Runs the reject observer target with reason operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn reject_observer_target_with_reason(
        &mut self,
        primary_client_id: &ClientId,
        observer_id: &str,
        reason: Option<String>,
    ) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let observer = self
            .observers
            .iter_mut()
            .find(|observer| observer.id.as_str() == observer_id)
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "observer not found")
            })?;
        let decided_at = current_unix_seconds();
        observer.state = ObserverDecisionState::Rejected;
        observer.decided_at_unix_seconds = Some(decided_at);
        observer.decided_by_client_id = Some(primary_client_id.to_string());
        observer.reason = reason;
        if let Some(client) = self
            .clients
            .iter_mut()
            .find(|client| client.id == observer.client_id)
        {
            client.state = ClientState::Revoked;
            client.last_seen_at_unix_seconds = Some(decided_at);
        }
        self.record_event();
        Ok(())
    }

    /// Runs the revoke observer client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn revoke_observer_client(
        &mut self,
        primary_client_id: &ClientId,
        client_id: &str,
    ) -> Result<()> {
        self.revoke_observer_client_with_reason(primary_client_id, client_id, None)
    }

    /// Runs the revoke observer client with reason operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn revoke_observer_client_with_reason(
        &mut self,
        primary_client_id: &ClientId,
        client_id: &str,
        reason: Option<String>,
    ) -> Result<()> {
        self.require_primary(primary_client_id)?;
        let client = self
            .clients
            .iter_mut()
            .find(|client| client.id.as_str() == client_id)
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "client not found")
            })?;
        if client.role != ClientRole::Observer {
            return Err(MezError::invalid_args(
                "revoke-observer requires an approved observer client",
            ));
        }
        let decided_at = current_unix_seconds();
        client.state = ClientState::Revoked;
        client.last_seen_at_unix_seconds = Some(decided_at);
        if let Some(observer) = self
            .observers
            .iter_mut()
            .find(|observer| observer.client_id.as_str() == client_id)
        {
            observer.state = ObserverDecisionState::Revoked;
            observer.decided_at_unix_seconds = Some(decided_at);
            observer.decided_by_client_id = Some(primary_client_id.to_string());
            observer.reason = reason;
        }
        self.record_event();
        Ok(())
    }

    /// Runs the detach client target operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn detach_client_target(
        &mut self,
        primary_client_id: &ClientId,
        client_id: &str,
    ) -> Result<()> {
        self.require_primary(primary_client_id)?;
        if primary_client_id.as_str() == client_id {
            return self.detach_primary(primary_client_id);
        }
        let client = self
            .clients
            .iter_mut()
            .find(|client| client.id.as_str() == client_id)
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "client not found")
            })?;
        client.state = ClientState::Detached;
        client.last_seen_at_unix_seconds = Some(current_unix_seconds());
        self.record_event();
        Ok(())
    }

    /// Runs the detach primary operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn detach_primary(&mut self, primary_client_id: &ClientId) -> Result<()> {
        self.require_primary(primary_client_id)?;
        if let Some(client) = self
            .clients
            .iter_mut()
            .find(|client| client.id == *primary_client_id)
        {
            client.state = ClientState::Detached;
            client.last_seen_at_unix_seconds = Some(current_unix_seconds());
        }
        self.primary_client_id = None;
        self.state = SessionState::Detached;
        self.record_event();
        Ok(())
    }

    /// Runs the require primary operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn require_primary(&self, client_id: &ClientId) -> Result<()> {
        if self.primary_client_id.as_ref() == Some(client_id) {
            Ok(())
        } else {
            Err(MezError::forbidden("operation requires the primary client"))
        }
    }
}

/// Runs the validate client terminal descriptor operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_client_terminal_descriptor(terminal: &ClientTerminalDescriptor) -> Result<()> {
    if terminal.columns == 0 || terminal.rows == 0 {
        return Err(MezError::invalid_args(
            "client terminal descriptor dimensions must be non-zero",
        ));
    }
    if terminal.term.trim().is_empty() {
        return Err(MezError::invalid_args(
            "client terminal descriptor requires term",
        ));
    }
    if terminal
        .features
        .iter()
        .any(|feature| feature.trim().is_empty())
    {
        return Err(MezError::invalid_args(
            "client terminal descriptor features must be non-empty",
        ));
    }
    Ok(())
}
