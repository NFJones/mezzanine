//! Client-local lifecycle policy for transient zen-mode focus labels.
//!
//! Focus labels are presentation state, not mux navigation state. Callers take
//! explicit snapshots around committed mutations and reconcile them here; view
//! rendering must never infer or renew focus changes. Records retain stable IDs
//! and absolute deadlines so observers can project their source primary's
//! remaining lifetime without creating independent labels.

use super::{
    RenderInvalidationReason, RuntimePresentationComponent, RuntimeSessionService,
    current_unix_millis,
};
use mez_core::ids::{ClientId, PaneId, WindowGroupId, WindowId};
use mez_mux::session::{ClientRole, ClientState};

/// Stable visible focus chain for one attached primary at a mutation boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RuntimeZenFocusSnapshot {
    /// Monotonic navigation revision used only to deduplicate nested hooks.
    revision: u64,
    /// Active group when the repaired navigation chain remains valid.
    group_id: Option<WindowGroupId>,
    /// Active window when the repaired navigation chain remains valid.
    window_id: Option<WindowId>,
    /// Active pane when the repaired navigation chain remains valid.
    pane_id: Option<PaneId>,
}

/// All attached-primary focus snapshots captured before one mutation.
pub(crate) type RuntimeZenFocusSnapshots =
    std::collections::HashMap<ClientId, RuntimeZenFocusSnapshot>;

/// Scope and stable target carried by one transient focus label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeZenFocusLabelTarget {
    /// A window-group identity.
    Group(WindowGroupId),
    /// A window identity.
    Window(WindowId),
    /// A pane identity.
    Pane(PaneId),
}

/// One client-local label with its visible ancestors and fixed deadline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeZenFocusLabel {
    /// Stable target resolved to display metadata only when rendering.
    pub(crate) target: RuntimeZenFocusLabelTarget,
    /// Active group containing the target at commit time.
    pub(crate) group_id: Option<WindowGroupId>,
    /// Active window containing the target at commit time.
    pub(crate) window_id: Option<WindowId>,
    /// Absolute expiration time in Unix milliseconds.
    pub(crate) expires_at_unix_ms: u64,
}

/// Bounded transient labels retained for one attached primary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RuntimeZenFocusLabelState {
    /// Most recently changed group that remains live.
    pub(crate) group: Option<RuntimeZenFocusLabel>,
    /// Most recently changed window that remains live.
    pub(crate) window: Option<RuntimeZenFocusLabel>,
    /// Most recently changed pane that remains live.
    pub(crate) pane: Option<RuntimeZenFocusLabel>,
    /// Last post-mutation navigation revision processed by any nested hook.
    last_reconciled_navigation_revision: Option<u64>,
}

impl RuntimeZenFocusLabelState {
    /// Removes records whose deadline has elapsed and reports visible change.
    fn expire(&mut self, now_ms: u64) -> bool {
        let before = (
            self.group.is_some(),
            self.window.is_some(),
            self.pane.is_some(),
        );
        if self
            .group
            .as_ref()
            .is_some_and(|label| label.expires_at_unix_ms <= now_ms)
        {
            self.group = None;
        }
        if self
            .window
            .as_ref()
            .is_some_and(|label| label.expires_at_unix_ms <= now_ms)
        {
            self.window = None;
        }
        if self
            .pane
            .as_ref()
            .is_some_and(|label| label.expires_at_unix_ms <= now_ms)
        {
            self.pane = None;
        }
        before
            != (
                self.group.is_some(),
                self.window.is_some(),
                self.pane.is_some(),
            )
    }

    /// Returns the earliest outstanding deadline.
    fn next_due_ms(&self) -> Option<u64> {
        [
            self.group.as_ref(),
            self.window.as_ref(),
            self.pane.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(|label| label.expires_at_unix_ms)
        .min()
    }
}

impl RuntimePresentationComponent {
    /// Removes all retained focus labels, incrementing affected revisions.
    pub(crate) fn clear_all_zen_focus_labels(&mut self) -> bool {
        let mut changed = false;
        for state in self.client_states.values_mut() {
            let group_removed = state.zen_focus_labels.group.take().is_some();
            let window_removed = state.zen_focus_labels.window.take().is_some();
            let pane_removed = state.zen_focus_labels.pane.take().is_some();
            if group_removed || window_removed || pane_removed {
                changed = true;
                state.presentation_revision = state.presentation_revision.saturating_add(1);
            }
        }
        changed
    }

    /// Expires one source primary's labels at an explicit instant.
    pub(crate) fn expire_zen_focus_labels(&mut self, client_id: &ClientId, now_ms: u64) -> bool {
        let Some(state) = self.client_states.get_mut(client_id) else {
            return false;
        };
        let changed = state.zen_focus_labels.expire(now_ms);
        if changed {
            state.presentation_revision = state.presentation_revision.saturating_add(1);
        }
        changed
    }

    /// Returns one source primary's next label deadline after clearing expiry.
    pub(crate) fn zen_focus_label_next_due_ms(
        &mut self,
        client_id: &ClientId,
        now_ms: u64,
    ) -> Option<u64> {
        self.expire_zen_focus_labels(client_id, now_ms);
        self.client_states
            .get(client_id)
            .and_then(|state| state.zen_focus_labels.next_due_ms())
    }

    /// Applies one stable pre/post focus transition to client-local state.
    fn reconcile_zen_focus_label(
        &mut self,
        client_id: &ClientId,
        before: &RuntimeZenFocusSnapshot,
        after: &RuntimeZenFocusSnapshot,
        now_ms: u64,
    ) -> bool {
        let duration_ms = self.settings.terminal_zen_focus_label_duration_ms;
        let client_state = self.client_states.entry(client_id.clone()).or_default();
        let state = &mut client_state.zen_focus_labels;
        if state.last_reconciled_navigation_revision == Some(after.revision) {
            return false;
        }
        state.last_reconciled_navigation_revision = Some(after.revision);
        if state.group.as_ref().is_some_and(|label| {
            !matches!(&label.target, RuntimeZenFocusLabelTarget::Group(target) if after.group_id.as_ref() == Some(target))
        }) {
            state.group = None;
        }
        if state.window.as_ref().is_some_and(|label| {
            !matches!(&label.target, RuntimeZenFocusLabelTarget::Window(target) if after.window_id.as_ref() == Some(target))
                || label.group_id != after.group_id
        }) {
            state.window = None;
        }
        if state.pane.as_ref().is_some_and(|label| {
            !matches!(&label.target, RuntimeZenFocusLabelTarget::Pane(target) if after.pane_id.as_ref() == Some(target))
                || label.group_id != after.group_id
                || label.window_id != after.window_id
        }) {
            state.pane = None;
        }
        if before.group_id == after.group_id
            && before.window_id == after.window_id
            && before.pane_id == after.pane_id
        {
            return false;
        }

        let expires_at_unix_ms = now_ms.saturating_add(duration_ms);
        if before.group_id != after.group_id {
            state.group = after.group_id.clone().map(|group_id| RuntimeZenFocusLabel {
                target: RuntimeZenFocusLabelTarget::Group(group_id),
                group_id: after.group_id.clone(),
                window_id: after.window_id.clone(),
                expires_at_unix_ms,
            });
            state.window = None;
            state.pane = None;
        } else if before.window_id != after.window_id {
            state.window = after
                .window_id
                .clone()
                .map(|window_id| RuntimeZenFocusLabel {
                    target: RuntimeZenFocusLabelTarget::Window(window_id),
                    group_id: after.group_id.clone(),
                    window_id: after.window_id.clone(),
                    expires_at_unix_ms,
                });
            state.pane = None;
        } else if before.pane_id != after.pane_id {
            state.pane = after.pane_id.clone().map(|pane_id| RuntimeZenFocusLabel {
                target: RuntimeZenFocusLabelTarget::Pane(pane_id),
                group_id: after.group_id.clone(),
                window_id: after.window_id.clone(),
                expires_at_unix_ms,
            });
        }
        client_state.presentation_revision = client_state.presentation_revision.saturating_add(1);
        true
    }

    /// Returns cloned live labels for a source primary without renewing them.
    #[allow(
        dead_code,
        reason = "the title-only renderer consumes this lifecycle query in the dependent issue"
    )]
    pub(crate) fn live_zen_focus_labels(
        &self,
        client_id: &ClientId,
        now_ms: u64,
    ) -> Option<RuntimeZenFocusLabelState> {
        let mut state = self.client_states.get(client_id)?.zen_focus_labels.clone();
        state.expire(now_ms);
        (state.group.is_some() || state.window.is_some() || state.pane.is_some()).then_some(state)
    }
}

impl RuntimeSessionService {
    /// Captures valid stable focus chains for every attached primary.
    pub(crate) fn capture_zen_focus_snapshots(&self) -> RuntimeZenFocusSnapshots {
        self.session
            .clients()
            .iter()
            .filter(|client| {
                client.role == ClientRole::Primary && client.state == ClientState::Attached
            })
            .filter_map(|client| {
                let navigation = self.session.navigation(&client.id).ok()?;
                Some((
                    client.id.clone(),
                    RuntimeZenFocusSnapshot {
                        revision: navigation.revision,
                        group_id: self
                            .session
                            .active_group_for(&client.id)
                            .ok()
                            .map(|group| group.id.clone()),
                        window_id: self
                            .session
                            .active_window_for(&client.id)
                            .ok()
                            .map(|window| window.id.clone()),
                        pane_id: self
                            .session
                            .active_pane_for(&client.id)
                            .ok()
                            .map(|pane| pane.id.clone()),
                    },
                ))
            })
            .collect()
    }

    /// Reconciles all primary focus chains at the current wall-clock instant.
    pub(crate) fn reconcile_zen_focus_snapshots(
        &mut self,
        before: RuntimeZenFocusSnapshots,
    ) -> Vec<ClientId> {
        self.reconcile_zen_focus_snapshots_at(before, current_unix_millis())
    }

    /// Reconciles all primary focus chains at an explicit testable instant.
    pub(crate) fn reconcile_zen_focus_snapshots_at(
        &mut self,
        before: RuntimeZenFocusSnapshots,
        now_ms: u64,
    ) -> Vec<ClientId> {
        let after = self.capture_zen_focus_snapshots();
        if !self.presentation.settings.terminal_zen_mode
            || self
                .presentation
                .settings
                .terminal_zen_focus_label_duration_ms
                == 0
        {
            return Vec::new();
        }
        let mut changed = Vec::new();
        for (client_id, after_snapshot) in after {
            let Some(before_snapshot) = before.get(&client_id) else {
                continue;
            };
            if self.presentation.reconcile_zen_focus_label(
                &client_id,
                before_snapshot,
                &after_snapshot,
                now_ms,
            ) {
                changed.push(client_id);
            }
        }
        if !changed.is_empty() {
            let effects = self.render_effects_for_primary_projections(
                &changed,
                RenderInvalidationReason::Overlay,
            );
            self.presentation.defer_render_effects(effects);
        }
        changed
    }

    /// Resolves the source primary whose labels one attached client projects.
    pub(crate) fn zen_focus_label_source_client_id(
        &self,
        client_id: &ClientId,
    ) -> Option<ClientId> {
        let client = self
            .session
            .clients()
            .iter()
            .find(|client| client.id == *client_id && client.state == ClientState::Attached)?;
        match client.role {
            ClientRole::Primary => Some(client.id.clone()),
            ClientRole::Observer => self
                .session
                .observer_attachments()
                .iter()
                .find(|observer| observer.client_id == *client_id)
                .map(|observer| observer.view_source_client_id.clone()),
            ClientRole::Agent | ClientRole::Automation => None,
        }
    }

    /// Returns this view client's next source-label deadline without renewal.
    pub(crate) fn zen_focus_label_next_due_ms_for_client(
        &mut self,
        client_id: &ClientId,
        now_ms: u64,
    ) -> Option<u64> {
        let source = self.zen_focus_label_source_client_id(client_id)?;
        self.presentation
            .zen_focus_label_next_due_ms(&source, now_ms)
    }

    /// Expires the labels projected by one primary or observer client.
    pub(crate) fn expire_zen_focus_labels_for_client(
        &mut self,
        client_id: &ClientId,
        now_ms: u64,
    ) -> bool {
        let Some(source) = self.zen_focus_label_source_client_id(client_id) else {
            return false;
        };
        self.presentation.expire_zen_focus_labels(&source, now_ms)
    }

    /// Returns cloned source-primary labels for later title-only rendering.
    #[allow(
        dead_code,
        reason = "the title-only renderer consumes this lifecycle query in the dependent issue"
    )]
    pub(crate) fn live_zen_focus_labels_for_client(
        &self,
        client_id: &ClientId,
        now_ms: u64,
    ) -> Option<RuntimeZenFocusLabelState> {
        let source = self.zen_focus_label_source_client_id(client_id)?;
        self.presentation.live_zen_focus_labels(&source, now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        AttachedTerminalClientStepPlan, MouseAction, RuntimeSideEffect, RuntimeTimerKind, Size,
        TerminalClientLoopAction,
    };
    use crate::test_support::runtime::RuntimeServiceFixture;
    use mez_mux::input::{MuxAction, WindowFocusTarget};
    use mez_mux::process::PaneExitStatus;

    fn zen_service() -> (RuntimeSessionService, ClientId) {
        let mut service = RuntimeServiceFixture::new().build();
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
            .unwrap();
        service.presentation.settings.terminal_zen_mode = true;
        service
            .presentation
            .settings
            .terminal_zen_focus_label_duration_ms = 1_000;
        (service, primary)
    }

    fn label_deadline(state: &RuntimeZenFocusLabelState) -> u64 {
        state
            .group
            .as_ref()
            .or(state.window.as_ref())
            .or(state.pane.as_ref())
            .unwrap()
            .expires_at_unix_ms
    }

    fn empty_step(action: TerminalClientLoopAction) -> AttachedTerminalClientStepPlan {
        AttachedTerminalClientStepPlan {
            actions: vec![action],
            output_lines: Vec::new(),
            output_line_style_spans: Vec::new(),
            input_hangup: false,
            output_hangup: false,
            error_roles: Vec::new(),
        }
    }

    fn assert_window_label(
        service: &RuntimeSessionService,
        primary: &ClientId,
        expected: &WindowId,
    ) {
        let state = service
            .live_zen_focus_labels_for_client(primary, current_unix_millis())
            .expect("focus mutation should retain one live label");
        assert!(matches!(
            state.window.as_ref().map(|label| &label.target),
            Some(RuntimeZenFocusLabelTarget::Window(target)) if target == expected
        ));
    }

    fn assert_pane_label(service: &RuntimeSessionService, primary: &ClientId, expected: &PaneId) {
        let state = service
            .live_zen_focus_labels_for_client(primary, current_unix_millis())
            .expect("focus mutation should retain one live label");
        assert!(matches!(
            state.pane.as_ref().map(|label| &label.target),
            Some(RuntimeZenFocusLabelTarget::Pane(target)) if target == expected
        ));
    }

    /// A failed later command must not erase a committed earlier focus change;
    /// returning to the original target still counts as two real transitions.
    #[test]
    fn zen_focus_lifecycle_retains_committed_partial_sequence() {
        let (mut service, primary) = zen_service();
        let first = service
            .session
            .active_window_for(&primary)
            .unwrap()
            .id
            .clone();
        let second = service
            .session
            .new_window(&primary, "second", false)
            .unwrap();
        let command = format!(
            "select-window -t {}; select-window -t @missing",
            second.as_str()
        );
        assert!(
            service
                .execute_terminal_command(&primary, &command)
                .is_err()
        );
        assert_window_label(&service, &primary, &second);
        let command = format!(
            "select-window -t {}; select-window -t {}",
            first.as_str(),
            second.as_str()
        );
        service
            .execute_terminal_command(&primary, &command)
            .unwrap();
        assert_window_label(&service, &primary, &second);
    }

    /// Rendering another primary or its observer must not modify any source's
    /// focus records, deadlines, or navigation, even when projections alternate.
    #[test]
    fn zen_focus_lifecycle_survives_alternating_client_renders() {
        let (mut service, primary) = zen_service();
        let second = service
            .attach_primary("second", true, Size::new(80, 24).unwrap(), 2)
            .unwrap();
        service
            .execute_terminal_command(&primary, "new-window source")
            .unwrap();
        let observer = service
            .session
            .attach_observer_with_terminal("observer", None, 3)
            .unwrap();
        let now = current_unix_millis();
        let expected = service
            .live_zen_focus_labels_for_client(&primary, now)
            .unwrap();
        let config = service
            .terminal_client_loop_config(crate::runtime::TerminalClientLoopConfig::default())
            .unwrap();
        for client in [&second, &observer, &primary, &second] {
            let role = if client == &observer {
                crate::runtime::ClientViewRole::Observer
            } else {
                crate::runtime::ClientViewRole::Primary
            };
            service.prepare_client_render(client, role).unwrap();
            service
                .render_client_view_for_client_with_resolved_config(
                    client,
                    role,
                    Size::new(80, 24).unwrap(),
                    &config,
                )
                .unwrap();
            assert_eq!(
                service.live_zen_focus_labels_for_client(&primary, now),
                Some(expected.clone())
            );
            assert_eq!(
                service.live_zen_focus_labels_for_client(&observer, now),
                Some(expected.clone())
            );
            assert!(
                service
                    .live_zen_focus_labels_for_client(&second, now)
                    .is_none()
            );
        }
    }

    /// Disabling labels or leaving zen must clear every coexisting scope in
    /// one pass, increment the owner revision once, and leave no expiry timer.
    #[test]
    fn zen_focus_lifecycle_clears_all_coexisting_scopes() {
        for disable_duration in [true, false] {
            let (mut service, primary) = zen_service();
            let before = service.capture_zen_focus_snapshots();
            service
                .session
                .new_window(&primary, "second", true)
                .unwrap();
            service.reconcile_zen_focus_snapshots_at(before, 100);
            let state = service
                .presentation
                .client_states
                .get_mut(&primary)
                .unwrap();
            let label = state.zen_focus_labels.window.clone().unwrap();
            state.zen_focus_labels.group = Some(label.clone());
            state.zen_focus_labels.pane = Some(label);
            let revision = state.presentation_revision;
            let mut settings = service.presentation.settings.clone();
            if disable_duration {
                settings.terminal_zen_focus_label_duration_ms = 0;
            } else {
                settings.terminal_zen_mode = false;
            }
            service.presentation.apply_settings(settings);
            let state = &service.presentation.client_states[&primary];
            assert!(state.zen_focus_labels.group.is_none());
            assert!(state.zen_focus_labels.window.is_none());
            assert!(state.zen_focus_labels.pane.is_none());
            assert_eq!(state.presentation_revision, revision + 1);
            assert_eq!(state.zen_focus_labels.next_due_ms(), None);
        }
    }

    /// Verifies stable focus tuples choose only the highest changed scope and
    /// nested observation of one navigation revision cannot renew its deadline.
    #[test]
    fn zen_focus_lifecycle_uses_highest_scope_and_deduplicates_nested_hooks() {
        let (mut service, primary) = zen_service();
        let first_window = service
            .session
            .active_window_for(&primary)
            .unwrap()
            .id
            .clone();
        let before = service.capture_zen_focus_snapshots();
        let second_window = service
            .session
            .new_window(&primary, "second", true)
            .unwrap();

        assert_eq!(
            service.reconcile_zen_focus_snapshots_at(before.clone(), 10),
            vec![primary.clone()]
        );
        let state = service
            .presentation
            .live_zen_focus_labels(&primary, 10)
            .unwrap();
        assert!(state.group.is_none());
        assert!(state.pane.is_none());
        assert!(matches!(
            state.window.as_ref().map(|label| &label.target),
            Some(RuntimeZenFocusLabelTarget::Window(target)) if target == &second_window
        ));
        assert_eq!(label_deadline(&state), 1_010);

        assert!(
            service
                .reconcile_zen_focus_snapshots_at(before, 500)
                .is_empty()
        );
        assert_eq!(
            label_deadline(
                &service
                    .presentation
                    .live_zen_focus_labels(&primary, 500)
                    .unwrap()
            ),
            1_010
        );

        let before = service.capture_zen_focus_snapshots();
        service
            .session
            .select_window(&primary, first_window.as_str())
            .unwrap();
        assert_eq!(
            service.reconcile_zen_focus_snapshots_at(before, 700),
            vec![primary.clone()]
        );
        let state = service
            .presentation
            .live_zen_focus_labels(&primary, 700)
            .unwrap();
        assert!(matches!(
            state.window.as_ref().map(|label| &label.target),
            Some(RuntimeZenFocusLabelTarget::Window(target)) if target == &first_window
        ));
        assert_eq!(label_deadline(&state), 1_700);
    }

    /// Verifies expiry is exact, stale timer checks cannot clear replacements,
    /// and an observer inherits the source primary's original deadline.
    #[test]
    fn zen_focus_lifecycle_expires_exactly_and_observer_inherits_deadline() {
        let (mut service, primary) = zen_service();
        let before = service.capture_zen_focus_snapshots();
        service
            .session
            .new_window(&primary, "second", true)
            .unwrap();
        service.reconcile_zen_focus_snapshots_at(before, 100);
        let observer = service
            .session
            .attach_observer_with_terminal("observer", None, 1)
            .unwrap();

        assert_eq!(
            service.zen_focus_label_next_due_ms_for_client(&primary, 1_099),
            Some(1_100)
        );
        assert_eq!(
            service.zen_focus_label_next_due_ms_for_client(&observer, 1_099),
            Some(1_100)
        );

        let before = service.capture_zen_focus_snapshots();
        service.session.previous_window(&primary).unwrap();
        service.reconcile_zen_focus_snapshots_at(before, 500);
        assert!(!service.expire_zen_focus_labels_for_client(&observer, 1_100));
        assert_eq!(
            service.zen_focus_label_next_due_ms_for_client(&primary, 1_100),
            Some(1_500)
        );
        assert!(service.expire_zen_focus_labels_for_client(&primary, 1_500));
        assert_eq!(
            service.zen_focus_label_next_due_ms_for_client(&observer, 1_500),
            None
        );
    }

    /// Verifies no-op and zoom-only navigation revisions do not create labels,
    /// while zero duration and leaving zen mode clear outstanding state.
    #[test]
    fn zen_focus_lifecycle_ignores_non_focus_changes_and_clears_on_configuration() {
        let (mut service, primary) = zen_service();
        let before = service.capture_zen_focus_snapshots();
        service
            .session
            .toggle_active_pane_zoom_transition(&primary)
            .unwrap();
        assert!(
            service
                .reconcile_zen_focus_snapshots_at(before, 100)
                .is_empty()
        );
        assert!(
            service
                .presentation
                .live_zen_focus_labels(&primary, 100)
                .is_none()
        );

        let before = service.capture_zen_focus_snapshots();
        service
            .session
            .new_window(&primary, "second", true)
            .unwrap();
        service.reconcile_zen_focus_snapshots_at(before, 200);
        assert!(
            service
                .presentation
                .live_zen_focus_labels(&primary, 200)
                .is_some()
        );

        let mut settings = service.presentation.settings.clone();
        settings.terminal_zen_focus_label_duration_ms = 0;
        service.presentation.apply_settings(settings);
        assert!(
            service
                .presentation
                .live_zen_focus_labels(&primary, 200)
                .is_none()
        );

        let mut settings = service.presentation.settings.clone();
        settings.terminal_zen_mode = true;
        settings.terminal_zen_focus_label_duration_ms = 1_000;
        service.presentation.apply_settings(settings);
        let before = service.capture_zen_focus_snapshots();
        service.session.previous_window(&primary).unwrap();
        service.reconcile_zen_focus_snapshots_at(before, 300);
        let mut settings = service.presentation.settings.clone();
        settings.terminal_zen_mode = false;
        service.presentation.apply_settings(settings);
        assert!(
            service
                .presentation
                .live_zen_focus_labels(&primary, 300)
                .is_none()
        );
    }

    /// Verifies the existing status-refresh scheduler uses focus-label expiry
    /// for both the source primary and observers and cancels after expiry.
    #[test]
    fn zen_focus_lifecycle_drives_status_refresh_deadlines() {
        let (mut service, primary) = zen_service();
        let before = service.capture_zen_focus_snapshots();
        service
            .session
            .new_window(&primary, "second", true)
            .unwrap();
        service.reconcile_zen_focus_snapshots_at(before, 100);
        let observer = service
            .session
            .attach_observer_with_terminal("observer", None, 1)
            .unwrap();

        for client in [&primary, &observer] {
            let transition = service
                .client_status_refresh_timer_transition(client.as_str(), None, 200)
                .unwrap();
            let [crate::runtime::RuntimeSideEffect::ScheduleTimer { key, delay_ms }] =
                transition.side_effects.as_slice()
            else {
                panic!("expected one focus-label expiry timer")
            };
            assert_eq!(key.kind, RuntimeTimerKind::StatusRefresh);
            assert_eq!(key.generation, 1_100);
            assert_eq!(*delay_ms, 900);
        }

        assert!(service.expire_zen_focus_labels_for_client(&primary, 1_100));
        assert!(
            service
                .client_status_refresh_timer_transition(primary.as_str(), None, 1_100)
                .unwrap()
                .side_effects
                .is_empty()
        );
    }

    /// Verifies terminal commands and attached mux and mouse actions all use
    /// the same committed-navigation lifecycle boundary without render-time detection.
    #[test]
    fn zen_focus_lifecycle_tracks_command_mux_and_mouse_navigation() {
        for path in ["command", "mux", "mouse"] {
            let (mut service, primary) = zen_service();
            let first_window = service
                .session
                .active_window_for(&primary)
                .unwrap()
                .id
                .clone();
            let second_window = service
                .session
                .new_window(&primary, "second", true)
                .unwrap();
            service
                .session
                .select_window(&primary, first_window.as_str())
                .unwrap();

            match path {
                "command" => {
                    service
                        .execute_terminal_command(
                            &primary,
                            &format!("select-window -t {}", second_window.as_str()),
                        )
                        .unwrap();
                }
                "mux" => {
                    service
                        .apply_attached_terminal_step_plan(
                            &primary,
                            &empty_step(TerminalClientLoopAction::ExecuteMux(
                                MuxAction::FocusWindow(WindowFocusTarget::Index(1)),
                            )),
                        )
                        .unwrap();
                }
                "mouse" => {
                    service
                        .apply_attached_terminal_step_plan(
                            &primary,
                            &empty_step(TerminalClientLoopAction::HandleMouse(
                                MouseAction::FocusWindow { index: 1 },
                            )),
                        )
                        .unwrap();
                }
                _ => unreachable!(),
            }

            assert_window_label(&service, &primary, &second_window);
        }
    }

    /// Verifies generic control selection is reconciled at the product runtime
    /// boundary and a successful no-op replay does not renew its deadline.
    #[test]
    fn zen_focus_lifecycle_tracks_generic_control_selection_without_renewing_noop() {
        let (mut service, primary) = zen_service();
        let first_window = service
            .session
            .active_window_for(&primary)
            .unwrap()
            .id
            .clone();
        let second_window = service
            .session
            .new_window(&primary, "second", true)
            .unwrap();
        service
            .session
            .select_window(&primary, first_window.as_str())
            .unwrap();
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"window/select","params":{{"target":{{"window_id":"{}"}},"idempotency_key":"zen-focus-select"}}}}"#,
            second_window.as_str()
        );

        let response = service.dispatch_runtime_control_body(&request, &primary);
        assert!(response.contains(r#""result""#), "{response}");
        let deadline = label_deadline(
            &service
                .live_zen_focus_labels_for_client(&primary, current_unix_millis())
                .unwrap(),
        );
        assert_window_label(&service, &primary, &second_window);

        let replay = service.dispatch_runtime_control_body(&request, &primary);
        assert_eq!(response, replay);
        assert_eq!(
            label_deadline(
                &service
                    .live_zen_focus_labels_for_client(&primary, current_unix_millis())
                    .unwrap()
            ),
            deadline
        );
    }

    /// Verifies explicit close and natural process-exit repair label the new
    /// active pane for every affected primary, while stale exits do nothing.
    #[test]
    fn zen_focus_lifecycle_tracks_close_and_process_exit_structural_repair() {
        for path in ["close", "exit"] {
            let (mut service, primary) = zen_service();
            let survivor = service
                .session
                .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
                .unwrap();
            service.session.select_pane(&primary, "%1").unwrap();
            let second_primary = service
                .attach_primary("second", true, Size::new(80, 24).unwrap(), 2)
                .unwrap();
            service.session.select_pane(&second_primary, "%1").unwrap();

            if path == "close" {
                service
                    .dispatch_runtime_pane_close(&primary, r#"{"pane_id":"%1","force":true}"#)
                    .unwrap();
            } else {
                assert!(
                    service
                        .apply_pane_process_exit_event(
                            "%missing",
                            0,
                            PaneExitStatus {
                                code: Some(0),
                                signal: None,
                                success: true,
                            },
                        )
                        .unwrap()
                        .is_none()
                );
                service
                    .apply_pane_process_exit_event(
                        "%1",
                        0,
                        PaneExitStatus {
                            code: Some(0),
                            signal: None,
                            success: true,
                        },
                    )
                    .unwrap();
            }

            assert_pane_label(&service, &primary, &survivor);
            assert_pane_label(&service, &second_primary, &survivor);
        }
    }

    /// Verifies changing only the future label duration is a configuration
    /// repaint, not a geometry/full-redraw change, and preserves live deadlines.
    #[test]
    fn zen_focus_lifecycle_duration_reload_preserves_existing_deadline() {
        let (mut service, primary) = zen_service();
        let before = service.capture_zen_focus_snapshots();
        service
            .session
            .new_window(&primary, "second", true)
            .unwrap();
        service.reconcile_zen_focus_snapshots_at(before, 100);
        let original_deadline = label_deadline(
            &service
                .presentation
                .live_zen_focus_labels(&primary, 100)
                .unwrap(),
        );
        let mut settings = service.presentation.settings.clone();
        settings.terminal_zen_focus_label_duration_ms = 5_000;

        assert_eq!(
            service.presentation.apply_settings(settings),
            Some(RenderInvalidationReason::Configuration)
        );
        assert_eq!(
            label_deadline(
                &service
                    .presentation
                    .live_zen_focus_labels(&primary, 100)
                    .unwrap()
            ),
            original_deadline
        );

        let transition = service
            .client_status_refresh_timer_transition(primary.as_str(), None, 200)
            .unwrap();
        assert!(matches!(
            transition.side_effects.as_slice(),
            [RuntimeSideEffect::ScheduleTimer { key, delay_ms }]
                if key.generation == original_deadline && *delay_ms == 900
        ));
    }
}
