//! Client attachment, primary ownership, and observer attachment operations.
//!
//! Client methods enforce primary exclusivity, observer visibility cutoffs,
//! control-client role restrictions, and detach semantics.

use crate::{MuxError as MezError, MuxErrorKind, Result};
use mez_core::ClientId;

use super::time::current_unix_seconds;
use super::types::{
    Client, ClientRole, ClientState, ClientTerminalDescriptor, MAX_ATTACHED_PRIMARY_CLIENTS,
    MAX_RETAINED_DETACHED_CLIENTS, ObserverAttachment, PrimaryLifecycleEdge,
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
        let navigation = if let Some(owner_id) = self.layout_owner_client_id.as_ref() {
            self.navigation_from_primary_source(owner_id)?
        } else {
            self.navigation_from_landing()
        };
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
            navigation: Some(navigation),
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

    /// Attaches one read-only observer to the current exact layout-owner primary.
    ///
    /// Validation and source-navigation capture complete before any identifier
    /// allocation or session mutation, so a missing or stale layout owner leaves
    /// no client, observer, or event residue. The supplied event id is the first
    /// event visible to the attached observer.
    pub fn attach_observer_with_terminal(
        &mut self,
        name: impl Into<String>,
        terminal: Option<ClientTerminalDescriptor>,
        visible_from_event_id: u64,
    ) -> Result<ClientId> {
        let source_client_id = self.layout_owner_client_id.clone().ok_or_else(|| {
            MezError::conflict("observer attachment requires an attached layout-owner primary")
        })?;
        self.require_primary(&source_client_id)?;
        if let Some(terminal) = terminal.as_ref() {
            validate_client_terminal_descriptor(terminal)?;
        }
        let navigation = self.navigation_from_primary_source(&source_client_id)?;

        let name = name.into();
        let client_id = self.ids.client();
        let attached_at = current_unix_seconds();
        self.clients.push(Client {
            id: client_id.clone(),
            name,
            role: ClientRole::Observer,
            state: ClientState::Attached,
            interactive: false,
            terminal,
            attached_at_unix_seconds: Some(attached_at),
            last_seen_at_unix_seconds: Some(attached_at),
            navigation: Some(navigation),
        });
        self.observer_attachments.push(ObserverAttachment {
            client_id: client_id.clone(),
            view_source_client_id: source_client_id,
            visible_from_event_id,
        });
        self.record_event();
        Ok(client_id)
    }

    /// Updates one attached observer's client-local terminal geometry.
    pub fn resize_observer_terminal(
        &mut self,
        client_id: &ClientId,
        size: crate::layout::Size,
    ) -> Result<()> {
        let client = self
            .clients
            .iter_mut()
            .find(|client| client.id == *client_id)
            .filter(|client| {
                client.role == ClientRole::Observer && client.state == ClientState::Attached
            })
            .ok_or_else(|| MezError::forbidden("operation requires an attached observer"))?;
        let terminal = client.terminal.as_mut().ok_or_else(|| {
            MezError::invalid_state("attached observer has no terminal descriptor")
        })?;
        terminal.columns = size.columns;
        terminal.rows = size.rows;
        client.last_seen_at_unix_seconds = Some(current_unix_seconds());
        self.record_event();
        Ok(())
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
        if matches!(role, ClientRole::Primary | ClientRole::Observer) {
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
        self.observer_attachments
            .retain(|observer| observer.client_id.as_str() != client_id);
        self.record_event();
        Ok(())
    }

    /// Detaches one session client acting on its own authenticated identity.
    ///
    /// This does not grant authority over any other client. Observer attachment
    /// metadata is removed so a disconnected read-only client cannot retain
    /// event or rendering authority.
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
        self.observer_attachments
            .retain(|observer| observer.client_id != *client_id);
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
        let final_primary_landing = (primary_count_before == 1)
            .then(|| self.snapshot_landing_navigation(Some(primary_client_id)))
            .transpose()?;
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
            .observer_attachments
            .iter()
            .filter(|observer| observer.view_source_client_id == *primary_client_id)
            .map(|observer| observer.client_id.clone())
            .collect::<Vec<_>>();
        self.observer_attachments
            .retain(|observer| observer.view_source_client_id != *primary_client_id);
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
            if let Some(landing) = final_primary_landing {
                self.landing_navigation = landing;
            }
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
    /// Product adapters may protect clients referenced by unsettled external
    /// work through the supplied identity set.
    pub fn prune_detached_client_summaries(
        &mut self,
        additionally_protected: &std::collections::HashSet<ClientId>,
    ) -> usize {
        let mut removable = self
            .clients
            .iter()
            .enumerate()
            .filter(|(_, client)| {
                client.state == ClientState::Detached
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
