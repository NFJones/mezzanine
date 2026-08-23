//! Caller-navigation reconciliation after shared topology mutations.
//!
//! The topology remains session-owned, while every attached primary retains a
//! stable-ID view. This module validates those views in one pass after a pane,
//! window, or group mutation without recording passive repair as navigation
//! history.

use std::collections::{HashMap, HashSet};

use mez_core::{ClientId, PaneId, WindowGroupId, WindowId};

use super::types::{ClientNavigationState, ClientRole, ClientState, Session};

/// Identifies client views changed by one topology reconciliation pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TopologyReconciliation {
    /// Attached primary clients whose navigation changed.
    pub affected_client_ids: Vec<ClientId>,
    /// Whether client-independent landing navigation changed.
    pub landing_changed: bool,
}

#[derive(Debug)]
struct NavigationTopology {
    group_ids: Vec<WindowGroupId>,
    group_windows: HashMap<WindowGroupId, Vec<WindowId>>,
    window_group: HashMap<WindowId, WindowGroupId>,
    window_ids: Vec<WindowId>,
    window_panes: HashMap<WindowId, Vec<PaneId>>,
    pane_window: HashMap<PaneId, WindowId>,
}

impl Session {
    /// Reconciles every attached-primary cursor and landing focus once.
    ///
    /// Valid stable identities survive moves and swaps. Stale active values use
    /// the newest surviving history value, then the first structural member.
    /// Passive repair removes stale history and zoom without adding MRU entries.
    pub fn reconcile_client_navigation(&mut self) -> TopologyReconciliation {
        let topology = NavigationTopology::from_session(self);
        let landing_before = self.landing_navigation.clone();
        reconcile_landing(self, &topology);

        let mut affected_client_ids = Vec::new();
        for client in &mut self.clients {
            if client.role != ClientRole::Primary || client.state != ClientState::Attached {
                continue;
            }
            let Some(navigation) = client.navigation.as_mut() else {
                continue;
            };
            if reconcile_navigation(navigation, &topology, &self.landing_navigation) {
                affected_client_ids.push(client.id.clone());
            }
        }

        TopologyReconciliation {
            affected_client_ids,
            landing_changed: self.landing_navigation != landing_before,
        }
    }
}

impl NavigationTopology {
    fn from_session(session: &Session) -> Self {
        let group_ids = session
            .window_groups
            .iter()
            .map(|group| group.id.clone())
            .collect::<Vec<_>>();
        let mut group_windows = HashMap::new();
        let mut window_group = HashMap::new();
        for group in &session.window_groups {
            group_windows.insert(group.id.clone(), group.window_ids.clone());
            for window_id in &group.window_ids {
                window_group.insert(window_id.clone(), group.id.clone());
            }
        }

        let window_ids = session
            .windows
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        let mut window_panes = HashMap::new();
        let mut pane_window = HashMap::new();
        for window in &session.windows {
            let pane_ids = window
                .panes()
                .iter()
                .map(|pane| pane.id.clone())
                .collect::<Vec<_>>();
            for pane_id in &pane_ids {
                pane_window.insert(pane_id.clone(), window.id.clone());
            }
            window_panes.insert(window.id.clone(), pane_ids);
        }
        Self {
            group_ids,
            group_windows,
            window_group,
            window_ids,
            window_panes,
            pane_window,
        }
    }
}

fn reconcile_navigation(
    navigation: &mut ClientNavigationState,
    topology: &NavigationTopology,
    landing: &super::types::LandingNavigationState,
) -> bool {
    let before = navigation.clone();
    let prior_group = navigation.groups.active.clone();
    let mut active_window = prior_group
        .as_ref()
        .and_then(|group_id| navigation.windows_by_group.get(group_id))
        .and_then(|cursor| cursor.active.clone());
    let mut active_pane = active_window
        .as_ref()
        .and_then(|window_id| navigation.panes_by_window.get(window_id))
        .and_then(|cursor| cursor.active.clone());

    if let Some(pane_id) = active_pane.as_ref() {
        active_window = topology.pane_window.get(pane_id).cloned();
    }
    let mut active_group = active_window
        .as_ref()
        .and_then(|window_id| topology.window_group.get(window_id))
        .cloned();
    if active_group.is_none() {
        active_group = valid_value(navigation.groups.active.as_ref(), &topology.group_ids)
            .or_else(|| newest_valid(&navigation.groups.history, &topology.group_ids))
            .or_else(|| valid_value(landing.active_group_id.as_ref(), &topology.group_ids))
            .or_else(|| topology.group_ids.first().cloned());
    }
    navigation.groups.active = active_group.clone();
    navigation.groups.last = valid_value(navigation.groups.last.as_ref(), &topology.group_ids);
    retain_valid_unique(&mut navigation.groups.history, &topology.group_ids);

    let old_windows = std::mem::take(&mut navigation.windows_by_group);
    for group_id in &topology.group_ids {
        if !old_windows.contains_key(group_id) && active_group.as_ref() != Some(group_id) {
            continue;
        }
        let members = topology
            .group_windows
            .get(group_id)
            .cloned()
            .unwrap_or_default();
        let mut cursor = old_windows.get(group_id).cloned().unwrap_or_default();
        cursor.last = valid_value(cursor.last.as_ref(), &members);
        retain_valid_unique(&mut cursor.history, &members);
        let followed = active_window
            .as_ref()
            .filter(|window_id| topology.window_group.get(*window_id) == Some(group_id))
            .cloned();
        cursor.active = followed
            .or_else(|| valid_value(cursor.active.as_ref(), &members))
            .or_else(|| newest_valid(&cursor.history, &members))
            .or_else(|| members.first().cloned());
        if active_group.as_ref() == Some(group_id) {
            active_window = cursor.active.clone();
        }
        navigation.windows_by_group.insert(group_id.clone(), cursor);
    }

    if let Some(pane_id) = active_pane.as_ref()
        && !topology.pane_window.contains_key(pane_id)
    {
        active_pane = None;
    }
    let old_panes = std::mem::take(&mut navigation.panes_by_window);
    for window_id in &topology.window_ids {
        if !old_panes.contains_key(window_id) && active_window.as_ref() != Some(window_id) {
            continue;
        }
        let members = topology
            .window_panes
            .get(window_id)
            .cloned()
            .unwrap_or_default();
        let mut cursor = old_panes.get(window_id).cloned().unwrap_or_default();
        cursor.last = valid_value(cursor.last.as_ref(), &members);
        retain_valid_unique(&mut cursor.history, &members);
        let followed = active_pane
            .as_ref()
            .filter(|pane_id| topology.pane_window.get(*pane_id) == Some(window_id))
            .cloned();
        cursor.active = followed
            .or_else(|| valid_value(cursor.active.as_ref(), &members))
            .or_else(|| newest_valid(&cursor.history, &members))
            .or_else(|| members.first().cloned());
        if active_window.as_ref() == Some(window_id) {
            active_pane = cursor.active.clone();
        }
        navigation.panes_by_window.insert(window_id.clone(), cursor);
    }

    navigation
        .zoomed_panes_by_window
        .retain(|window_id, pane_id| {
            topology
                .window_panes
                .get(window_id)
                .is_some_and(|panes| panes.contains(pane_id))
        });
    let changed = navigation.groups != before.groups
        || navigation.windows_by_group != before.windows_by_group
        || navigation.panes_by_window != before.panes_by_window
        || navigation.zoomed_panes_by_window != before.zoomed_panes_by_window;
    navigation.revision = if changed {
        before.revision.saturating_add(1)
    } else {
        before.revision
    };
    changed
}

fn reconcile_landing(session: &mut Session, topology: &NavigationTopology) {
    let mut active_window = session
        .landing_navigation
        .active_pane_id
        .as_ref()
        .and_then(|pane_id| topology.pane_window.get(pane_id))
        .cloned()
        .or_else(|| {
            valid_value(
                session.landing_navigation.active_window_id.as_ref(),
                &topology.window_ids,
            )
        });
    let active_group = active_window
        .as_ref()
        .and_then(|window_id| topology.window_group.get(window_id))
        .cloned()
        .or_else(|| topology.group_ids.first().cloned());
    if active_window.is_none() {
        active_window = active_group
            .as_ref()
            .and_then(|group_id| topology.group_windows.get(group_id))
            .and_then(|windows| windows.first())
            .cloned();
    }
    let active_pane = active_window
        .as_ref()
        .and_then(|window_id| topology.window_panes.get(window_id))
        .and_then(|panes| {
            valid_value(session.landing_navigation.active_pane_id.as_ref(), panes)
                .or_else(|| panes.first().cloned())
        });
    session.landing_navigation.active_group_id = active_group;
    session.landing_navigation.active_window_id = active_window;
    session.landing_navigation.active_pane_id = active_pane;
}

fn valid_value<T: Clone + PartialEq>(value: Option<&T>, valid: &[T]) -> Option<T> {
    value.filter(|value| valid.contains(value)).cloned()
}

fn newest_valid<T: Clone + PartialEq>(history: &[T], valid: &[T]) -> Option<T> {
    history
        .iter()
        .rev()
        .find(|value| valid.contains(value))
        .cloned()
}

fn retain_valid_unique<T: Clone + Eq + std::hash::Hash>(history: &mut Vec<T>, valid: &[T]) {
    let mut seen = HashSet::new();
    history.retain(|value| valid.contains(value) && seen.insert(value.clone()));
    if history.len() > 10 {
        history.drain(..history.len() - 10);
    }
}
