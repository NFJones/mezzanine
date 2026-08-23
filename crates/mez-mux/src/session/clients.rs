//! Client attachment, primary ownership, and observer-request operations.
//!
//! Client methods enforce primary exclusivity, observer approval visibility,
//! control-client role restrictions, and detach semantics.

use crate::{MuxError as MezError, MuxErrorKind, Result};
use mez_core::{ClientId, ObserverRequestId};

use super::time::current_unix_seconds;
use super::types::{
    Client, ClientRole, ClientState, ClientTerminalDescriptor, MAX_ATTACHED_PRIMARY_CLIENTS,
    MAX_RETAINED_DETACHED_CLIENTS, ObserverDecisionState, ObserverRequest, PrimaryLifecycleEdge,
    PrimaryMembershipTransition, Session, SessionState,
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
        Ok(self
            .attach_primary_with_terminal_transition(name, interactive, terminal)?
            .client_id)
    }

    /// Attaches one fresh primary and reports membership and ownership edges.
    pub fn attach_primary_with_terminal_transition(
        &mut self,
        name: impl Into<String>,
        interactive: bool,
        terminal: Option<ClientTerminalDescriptor>,
    ) -> Result<PrimaryMembershipTransition> {
        if !interactive {
            return Err(MezError::forbidden(
                "primary clients must attach through an interactive terminal",
            ));
        }
        if let Some(terminal) = terminal.as_ref() {
            validate_client_terminal_descriptor(terminal)?;
        }
        let primary_count_before = self.attached_primaries().count();
        if primary_count_before >= MAX_ATTACHED_PRIMARY_CLIENTS {
            return Err(MezError::conflict(format!(
                "session already has the maximum of {MAX_ATTACHED_PRIMARY_CLIENTS} attached primary clients"
            )));
        }

        let layout_owner_before = self.layout_owner_client_id.clone();
        let authoritative_size_before = self.authoritative_size;
        let terminal_size = terminal
            .as_ref()
            .map(|terminal| crate::layout::Size::new(terminal.columns, terminal.rows))
            .transpose()?;
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
            navigation: Some(self.navigation_from_landing()),
        });
        let resize_effects = if self.layout_owner_client_id.is_none() {
            self.layout_owner_client_id = Some(client_id.clone());
            self.layout_revision = self.layout_revision.saturating_add(1);
            terminal_size
                .map(|size| self.apply_authoritative_layout_size(size))
                .transpose()?
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self.last_attached_at_unix_seconds = Some(attached_at);
        self.state = SessionState::Running;
        self.record_event();
        Ok(PrimaryMembershipTransition {
            client_id,
            primary_count_before,
            primary_count_after: primary_count_before.saturating_add(1),
            layout_owner_before,
            layout_owner_after: self.layout_owner_client_id.clone(),
            authoritative_size_before,
            authoritative_size_after: self.authoritative_size,
            resize_effects,
            revoked_observer_client_ids: Vec::new(),
            lifecycle_edge: if primary_count_before == 0 {
                PrimaryLifecycleEdge::Attached
            } else {
                PrimaryLifecycleEdge::None
            },
        })
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
        Ok(self
            .select_layout_owner_transition(authority_client_id, target_client_id)?
            .client_id)
    }

    /// Transfers canonical layout ownership to one attached primary.
    pub fn select_layout_owner_transition(
        &mut self,
        authority_client_id: Option<&ClientId>,
        target_client_id: &str,
    ) -> Result<PrimaryMembershipTransition> {
        let authority_client_id = authority_client_id.ok_or_else(|| {
            MezError::forbidden("layout ownership transfer requires an attached primary client")
        })?;
        self.require_primary(authority_client_id)?;

        let target_index = self
            .clients
            .iter()
            .position(|client| client.id.as_str() == target_client_id)
            .ok_or_else(|| MezError::new(MuxErrorKind::NotFound, "client not found"))?;
        if !self.clients[target_index].interactive
            || self.clients[target_index].role != ClientRole::Primary
            || self.clients[target_index].state != ClientState::Attached
        {
            return Err(MezError::forbidden(
                "layout ownership requires an attached interactive primary client",
            ));
        }

        let primary_count = self.attached_primaries().count();
        let layout_owner_before = self.layout_owner_client_id.clone();
        let authoritative_size_before = self.authoritative_size;
        let target_id = self.clients[target_index].id.clone();
        let target_size = self.clients[target_index]
            .terminal
            .as_ref()
            .map(|terminal| crate::layout::Size::new(terminal.columns, terminal.rows))
            .transpose()?;
        let selected_at = current_unix_seconds();
        self.clients[target_index].last_seen_at_unix_seconds = Some(selected_at);
        let resize_effects = if self.layout_owner_client_id.as_ref() != Some(&target_id) {
            self.layout_owner_client_id = Some(target_id.clone());
            self.layout_revision = self.layout_revision.saturating_add(1);
            target_size
                .map(|size| self.apply_authoritative_layout_size(size))
                .transpose()?
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self.record_event();
        Ok(PrimaryMembershipTransition {
            client_id: target_id,
            primary_count_before: primary_count,
            primary_count_after: primary_count,
            layout_owner_before,
            layout_owner_after: self.layout_owner_client_id.clone(),
            authoritative_size_before,
            authoritative_size_after: self.authoritative_size,
            resize_effects,
            revoked_observer_client_ids: Vec::new(),
            lifecycle_edge: PrimaryLifecycleEdge::None,
        })
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
            navigation: None,
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
            view_source_client_id: None,
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
            navigation: None,
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
        self.approve_observer_target_with_source(primary_client_id, observer_id, primary_client_id)
    }

    /// Approves an observer with one explicit attached-primary view source.
    pub fn approve_observer_target_with_source(
        &mut self,
        primary_client_id: &ClientId,
        observer_id: &str,
        view_source_client_id: &ClientId,
    ) -> Result<()> {
        self.require_primary(primary_client_id)?;
        self.require_primary(view_source_client_id)?;
        let observer_index =
            self.require_observer_transition(observer_id, ObserverDecisionState::Approved)?;
        let visible_from_event_id = self.record_event();
        self.approve_observer_target_with_visible_from(
            primary_client_id,
            observer_index,
            visible_from_event_id,
            view_source_client_id,
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
        self.approve_observer_target_with_visible_from_event_id_and_source(
            primary_client_id,
            observer_id,
            visible_from_event_id,
            primary_client_id,
        )
    }

    /// Approves an observer with an explicit visibility marker and view source.
    pub fn approve_observer_target_with_visible_from_event_id_and_source(
        &mut self,
        primary_client_id: &ClientId,
        observer_id: &str,
        visible_from_event_id: u64,
        view_source_client_id: &ClientId,
    ) -> Result<()> {
        self.require_primary(primary_client_id)?;
        self.require_primary(view_source_client_id)?;
        let observer_index =
            self.require_observer_transition(observer_id, ObserverDecisionState::Approved)?;
        self.record_event();
        self.approve_observer_target_with_visible_from(
            primary_client_id,
            observer_index,
            visible_from_event_id,
            view_source_client_id,
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
        observer_index: usize,
        visible_from_event_id: u64,
        view_source_client_id: &ClientId,
    ) -> Result<()> {
        let observer = self
            .observers
            .get_mut(observer_index)
            .ok_or_else(|| MezError::new(MuxErrorKind::NotFound, "observer not found"))?;

        let decided_at = current_unix_seconds();
        observer.state = ObserverDecisionState::Approved;
        observer.decided_at_unix_seconds = Some(decided_at);
        observer.decided_by_client_id = Some(primary_client_id.to_string());
        observer.view_source_client_id = Some(view_source_client_id.clone());
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
        let observer_index =
            self.require_observer_transition(observer_id, ObserverDecisionState::Rejected)?;
        let observer = self
            .observers
            .get_mut(observer_index)
            .ok_or_else(|| MezError::new(MuxErrorKind::NotFound, "observer not found"))?;
        let decided_at = current_unix_seconds();
        let observer_client_id = observer.client_id.clone();
        observer.state = ObserverDecisionState::Rejected;
        observer.decided_at_unix_seconds = Some(decided_at);
        observer.decided_by_client_id = Some(primary_client_id.to_string());
        observer.reason = reason;
        if let Some(client) = self
            .clients
            .iter_mut()
            .find(|client| client.id == observer_client_id)
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
        let client_index = self
            .clients
            .iter()
            .position(|client| client.id.as_str() == client_id)
            .ok_or_else(|| MezError::new(MuxErrorKind::NotFound, "client not found"))?;
        let observer_index = self
            .observers
            .iter()
            .position(|observer| observer.client_id.as_str() == client_id)
            .ok_or_else(|| MezError::new(MuxErrorKind::NotFound, "observer not found"))?;
        self.require_observer_transition_at(observer_index, ObserverDecisionState::Revoked)?;
        let client = &self.clients[client_index];
        if client.role != ClientRole::Observer {
            return Err(MezError::invalid_args(
                "revoke-observer requires an approved observer client",
            ));
        }
        if client.state != ClientState::Attached {
            return Err(MezError::conflict(
                "revoke-observer requires an attached observer client",
            ));
        }
        let decided_at = current_unix_seconds();
        let client = &mut self.clients[client_index];
        client.state = ClientState::Revoked;
        client.last_seen_at_unix_seconds = Some(decided_at);
        let observer = &mut self.observers[observer_index];
        observer.state = ObserverDecisionState::Revoked;
        observer.decided_at_unix_seconds = Some(decided_at);
        observer.decided_by_client_id = Some(primary_client_id.to_string());
        observer.reason = reason;
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
        if self
            .clients
            .iter()
            .any(|client| client.id.as_str() == client_id && client.role == ClientRole::Primary)
        {
            let target_id = self
                .clients
                .iter()
                .find(|client| client.id.as_str() == client_id)
                .map(|client| client.id.clone())
                .expect("primary target was just resolved");
            return self.detach_primary(&target_id);
        }
        let client = self
            .clients
            .iter_mut()
            .find(|client| client.id.as_str() == client_id)
            .ok_or_else(|| MezError::new(MuxErrorKind::NotFound, "client not found"))?;
        let detached_at = current_unix_seconds();
        client.state = ClientState::Detached;
        client.last_seen_at_unix_seconds = Some(detached_at);
        self.terminalize_observer_for_detach(
            client_id,
            detached_at,
            Some(primary_client_id),
            "client detached by primary",
        );
        self.record_event();
        Ok(())
    }

    /// Detaches one session client acting on its own authenticated identity.
    ///
    /// This does not grant authority over any other client. Observer records
    /// are revoked so a short-lived pairing or connectivity-check connection
    /// cannot leave a pending or approved observer request behind.
    pub fn detach_client_self(&mut self, client_id: &ClientId) -> Result<()> {
        if self.is_attached_primary(client_id) {
            return self.detach_primary(client_id);
        }
        let client = self
            .clients
            .iter_mut()
            .find(|client| client.id == *client_id)
            .ok_or_else(|| MezError::new(MuxErrorKind::NotFound, "client not found"))?;
        let detached_at = current_unix_seconds();
        client.state = ClientState::Detached;
        client.last_seen_at_unix_seconds = Some(detached_at);
        self.terminalize_observer_for_detach(
            client_id.as_str(),
            detached_at,
            None,
            "client detached itself",
        );
        self.record_event();
        Ok(())
    }

    /// Runs the detach primary operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn detach_primary(&mut self, primary_client_id: &ClientId) -> Result<()> {
        self.detach_primary_transition(primary_client_id)?;
        Ok(())
    }

    /// Detaches one exact primary and elects a replacement layout owner.
    pub fn detach_primary_transition(
        &mut self,
        primary_client_id: &ClientId,
    ) -> Result<PrimaryMembershipTransition> {
        self.require_primary(primary_client_id)?;
        let primary_count_before = self.attached_primaries().count();
        let layout_owner_before = self.layout_owner_client_id.clone();
        let authoritative_size_before = self.authoritative_size;
        if let Some(client) = self
            .clients
            .iter_mut()
            .find(|client| client.id == *primary_client_id)
        {
            client.state = ClientState::Detached;
            client.last_seen_at_unix_seconds = Some(current_unix_seconds());
        }
        let revoked_at = current_unix_seconds();
        let revoked_observer_client_ids = self
            .observers
            .iter_mut()
            .filter(|observer| {
                observer.state == ObserverDecisionState::Approved
                    && observer.view_source_client_id.as_ref() == Some(primary_client_id)
            })
            .map(|observer| {
                observer.state = ObserverDecisionState::Revoked;
                observer.decided_at_unix_seconds = Some(revoked_at);
                observer.decided_by_client_id = None;
                observer.reason = Some("view source detached".to_string());
                observer.client_id.clone()
            })
            .collect::<Vec<_>>();
        for observer_client_id in &revoked_observer_client_ids {
            if let Some(client) = self
                .clients
                .iter_mut()
                .find(|client| client.id == *observer_client_id)
            {
                client.state = ClientState::Revoked;
                client.last_seen_at_unix_seconds = Some(revoked_at);
            }
        }
        let primary_count_after = primary_count_before.saturating_sub(1);
        let resize_effects = if self.layout_owner_client_id.as_ref() == Some(primary_client_id) {
            self.layout_owner_client_id = self
                .attached_primaries()
                .min_by(|left, right| {
                    left.attached_at_unix_seconds
                        .cmp(&right.attached_at_unix_seconds)
                        .then_with(|| left.id.as_str().cmp(right.id.as_str()))
                })
                .map(|client| client.id.clone());
            self.layout_revision = self.layout_revision.saturating_add(1);
            let elected_size = self
                .layout_owner_client_id
                .as_ref()
                .and_then(|owner_id| self.clients.iter().find(|client| client.id == *owner_id))
                .and_then(|client| client.terminal.as_ref())
                .map(|terminal| crate::layout::Size::new(terminal.columns, terminal.rows))
                .transpose()?;
            elected_size
                .map(|size| self.apply_authoritative_layout_size(size))
                .transpose()?
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if primary_count_after == 0 {
            self.state = SessionState::Detached;
        }
        self.record_event();
        Ok(PrimaryMembershipTransition {
            client_id: primary_client_id.clone(),
            primary_count_before,
            primary_count_after,
            layout_owner_before,
            layout_owner_after: self.layout_owner_client_id.clone(),
            authoritative_size_before,
            authoritative_size_after: self.authoritative_size,
            resize_effects,
            revoked_observer_client_ids,
            lifecycle_edge: if primary_count_after == 0 {
                PrimaryLifecycleEdge::Detached
            } else {
                PrimaryLifecycleEdge::None
            },
        })
    }

    /// Validates one observer decision by request id before any mutation.
    ///
    /// Only pending requests may be approved or rejected, and only approved
    /// requests may be revoked. Terminal decisions return a conflict without
    /// advancing the event sequence or changing observer/client state.
    fn require_observer_transition(
        &self,
        observer_id: &str,
        next_state: ObserverDecisionState,
    ) -> Result<usize> {
        let observer_index = self
            .observers
            .iter()
            .position(|observer| observer.id.as_str() == observer_id)
            .ok_or_else(|| MezError::new(MuxErrorKind::NotFound, "observer not found"))?;
        self.require_observer_transition_at(observer_index, next_state)?;
        Ok(observer_index)
    }

    /// Validates one observer decision by its already-resolved request index.
    fn require_observer_transition_at(
        &self,
        observer_index: usize,
        next_state: ObserverDecisionState,
    ) -> Result<()> {
        let observer = self
            .observers
            .get(observer_index)
            .ok_or_else(|| MezError::new(MuxErrorKind::NotFound, "observer not found"))?;
        let transition_allowed = matches!(
            (observer.state, next_state),
            (
                ObserverDecisionState::Pending,
                ObserverDecisionState::Approved | ObserverDecisionState::Rejected
            ) | (
                ObserverDecisionState::Approved,
                ObserverDecisionState::Revoked
            )
        );
        if transition_allowed {
            Ok(())
        } else {
            Err(MezError::conflict(format!(
                "observer request cannot transition from {:?} to {:?}",
                observer.state, next_state
            )))
        }
    }

    /// Terminalizes a pending or approved observer when its client detaches.
    ///
    /// Pending requests become rejected because no live requester remains;
    /// approved observers become revoked. Existing terminal decisions are
    /// preserved so detach cannot rewrite their attribution or reason.
    fn terminalize_observer_for_detach(
        &mut self,
        client_id: &str,
        decided_at: u64,
        decided_by_client_id: Option<&ClientId>,
        reason: &str,
    ) {
        let Some(observer) = self
            .observers
            .iter_mut()
            .find(|observer| observer.client_id.as_str() == client_id)
        else {
            return;
        };
        observer.state = match observer.state {
            ObserverDecisionState::Pending => ObserverDecisionState::Rejected,
            ObserverDecisionState::Approved => ObserverDecisionState::Revoked,
            ObserverDecisionState::Rejected | ObserverDecisionState::Revoked => return,
        };
        observer.decided_at_unix_seconds = Some(decided_at);
        observer.decided_by_client_id = decided_by_client_id.map(ToString::to_string);
        observer.reason = Some(reason.to_string());
    }

    /// Runs the require primary operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn require_primary(&self, client_id: &ClientId) -> Result<()> {
        if self.is_attached_primary(client_id) {
            Ok(())
        } else {
            Err(MezError::forbidden("operation requires the primary client"))
        }
    }

    /// Returns whether an exact client is an attached interactive primary.
    pub fn is_attached_primary(&self, client_id: &ClientId) -> bool {
        self.clients.iter().any(|client| {
            client.id == *client_id
                && client.role == ClientRole::Primary
                && client.state == ClientState::Attached
                && client.interactive
        })
    }

    /// Returns all attached primary records without enabling additional ingress.
    pub fn attached_primaries(&self) -> impl Iterator<Item = &Client> {
        self.clients.iter().filter(|client| {
            client.role == ClientRole::Primary
                && client.state == ClientState::Attached
                && client.interactive
        })
    }

    /// Prunes the oldest unreferenced detached client summaries to the retained limit.
    ///
    /// Observer request identities are always protected. Product adapters may
    /// additionally protect clients referenced by unsettled external work.
    pub fn prune_detached_client_summaries(
        &mut self,
        additionally_protected: &std::collections::HashSet<ClientId>,
    ) -> usize {
        let observer_clients = self
            .observers
            .iter()
            .map(|observer| observer.client_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let mut removable = self
            .clients
            .iter()
            .enumerate()
            .filter(|(_, client)| {
                client.state == ClientState::Detached
                    && !observer_clients.contains(&client.id)
                    && !additionally_protected.contains(&client.id)
            })
            .map(|(index, client)| {
                (
                    index,
                    client.last_seen_at_unix_seconds.unwrap_or_default(),
                    client.attached_at_unix_seconds.unwrap_or_default(),
                    client.id.as_str().to_string(),
                )
            })
            .collect::<Vec<_>>();
        if removable.len() <= MAX_RETAINED_DETACHED_CLIENTS {
            return 0;
        }
        removable.sort_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| left.3.cmp(&right.3))
        });
        let remove_count = removable.len() - MAX_RETAINED_DETACHED_CLIENTS;
        let remove_indices = removable
            .into_iter()
            .take(remove_count)
            .map(|(index, _, _, _)| index)
            .collect::<std::collections::HashSet<_>>();
        let mut index = 0usize;
        self.clients.retain(|_| {
            let retain = !remove_indices.contains(&index);
            index = index.saturating_add(1);
            retain
        });
        remove_count
    }

    /// Returns caller-local navigation for an attached primary.
    pub fn navigation(&self, client_id: &ClientId) -> Result<&super::types::ClientNavigationState> {
        self.clients
            .iter()
            .find(|client| client.id == *client_id)
            .filter(|client| {
                client.role == ClientRole::Primary && client.state == ClientState::Attached
            })
            .and_then(|client| client.navigation.as_ref())
            .ok_or_else(|| MezError::forbidden("client has no attached-primary navigation"))
    }

    /// Returns mutable caller-local navigation for an attached primary.
    pub fn navigation_mut(
        &mut self,
        client_id: &ClientId,
    ) -> Result<&mut super::types::ClientNavigationState> {
        self.clients
            .iter_mut()
            .find(|client| client.id == *client_id)
            .filter(|client| {
                client.role == ClientRole::Primary && client.state == ClientState::Attached
            })
            .and_then(|client| client.navigation.as_mut())
            .ok_or_else(|| MezError::forbidden("client has no attached-primary navigation"))
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
