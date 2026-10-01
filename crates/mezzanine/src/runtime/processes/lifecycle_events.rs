//! Stable lifecycle payload serialization for pane process transitions.
//!
//! These helpers publish through the existing lifecycle log after their callers
//! settle state. Payload shape and event ordering are unchanged; this module
//! owns no process, session, or event-log state.

use super::*;

impl RuntimeSessionService {
    /// Publishes the exact running-process identity and initial geometry.
    pub(in crate::runtime) fn append_pane_start_event(
        &mut self,
        update: &PaneProcessStart,
    ) -> Result<()> {
        self.append_lifecycle_event(EventKind::PaneChanged, format!(
            r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","columns":{},"rows":{}}}"#,
            json_escape(&update.pane_id), json_escape(&update.window_id), update.primary_pid,
            update.size.columns, update.size.rows,
        ))
    }

    /// Publishes settled process geometry as a pane lifecycle event.
    pub(in crate::runtime) fn append_pane_resize_event(
        &mut self,
        update: &PaneResizeUpdate,
    ) -> Result<()> {
        self.append_lifecycle_event(EventKind::PaneChanged, format!(
            r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","layout":"resized","columns":{},"rows":{}}}"#,
            json_escape(&update.pane_id), json_escape(&update.window_id), update.primary_pid,
            update.size.columns, update.size.rows,
        ))
    }

    /// Publishes output counters without retaining the PTY payload.
    pub(in crate::runtime) fn append_pane_output_event(
        &mut self,
        update: &PaneOutputUpdate,
    ) -> Result<()> {
        self.append_lifecycle_event(EventKind::PaneChanged, format!(
            r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","output_bytes":{},"activity_events":{},"bell_events":{},"background":{}}}"#,
            json_escape(&update.pane_id), json_escape(&update.window_id), update.primary_pid,
            update.bytes_read, update.activity_events, update.bell_events, update.background,
        ))
    }

    /// Publishes the current pane title with the settled process identity.
    pub(in crate::runtime) fn append_pane_title_event(
        &mut self,
        update: &PaneOutputUpdate,
    ) -> Result<()> {
        let title = self
            .find_pane_title(update.pane_id.as_str())
            .unwrap_or_else(|| "shell".to_string());
        self.append_lifecycle_event(EventKind::PaneChanged, format!(
            r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","title":"{}"}}"#,
            json_escape(&update.pane_id), json_escape(&update.window_id), update.primary_pid, json_escape(&title),
        ))
    }

    /// Publishes settled exit status and resulting session topology flags.
    pub(in crate::runtime) fn append_pane_exit_event(
        &mut self,
        update: &PaneExitUpdate,
    ) -> Result<()> {
        self.append_lifecycle_event(EventKind::PaneChanged, format!(
            r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"exited","exit_status":{},"exit_code":{},"signal":{},"closed_window":{},"session_empty":{}}}"#,
            json_escape(&update.pane_id), json_escape(&update.window_id), update.primary_pid,
            update.exit_status.to_json(), optional_i32_json(update.exit_status.code),
            optional_i32_json(update.exit_status.signal), update.closed_window, update.session_empty,
        ))
    }

    /// Publishes pane closure after its owned process termination is scheduled.
    pub(in crate::runtime) fn append_pane_close_event(
        &mut self,
        pane_id: &str,
        window_id: &str,
        terminated_panes: usize,
        session_empty: bool,
    ) -> Result<()> {
        self.append_lifecycle_event(EventKind::PaneChanged, format!(
            r#"{{"pane_id":"{}","window_id":"{}","state":"closed","closed":true,"terminated_panes":{},"session_empty":{}}}"#,
            json_escape(pane_id), json_escape(window_id), terminated_panes, session_empty,
        ))
    }

    /// Publishes window closure with termination count and session-empty status.
    pub(in crate::runtime) fn append_window_close_event(
        &mut self,
        window_id: &str,
        terminated_panes: usize,
        session_empty: bool,
    ) -> Result<()> {
        self.append_lifecycle_event(EventKind::WindowChanged, format!(
            r#"{{"window_id":"{}","state":"closed","closed":true,"terminated_panes":{},"session_empty":{}}}"#,
            json_escape(window_id), terminated_panes, session_empty,
        ))
    }
}
