//! Typed terminal selection handoff and in-memory catalog ownership.
//!
//! Command outcomes, not rendered Markdown, select the browser producer. The
//! synchronous handoff owns one client's overlay generation and is consumed
//! once. Selection executes product semantics separately from agent slash
//! commands; titles are inert and catalog refresh never reads a store.

use crate::error::{MezError, Result};
use crate::runtime::service_state::{
    RuntimeRecordBrowserOverlaySource, RuntimeRecordBrowserOverlayState,
};
use crate::runtime::{CommandOutcome, RuntimeSessionService};
use mez_core::ids::ClientId;
use mez_mux::record_browser::{RecordBrowser, RecordBrowserRecord};

/// One command-owned, exact-client presentation capability, never persisted.
struct TerminalBrowserHandoff {
    client: ClientId,
    generation: u32,
    state: RuntimeRecordBrowserOverlayState,
}

impl RuntimeSessionService {
    /// Activates one retained stable window ID only for its original primary/group.
    /// Stale targets keep the browser open with an error rather than redirect focus.
    pub(crate) fn activate_terminal_window_selection(
        &mut self,
        client: &ClientId,
        owner: &str,
        group: &str,
        id: &str,
    ) -> Result<bool> {
        let activation = (|| -> Result<()> {
            self.require_live()?;
            if client.as_str() != owner || !self.session.is_attached_primary(client) {
                return Err(MezError::forbidden(
                    "terminal selection belongs to another primary",
                ));
            }
            if self.session.active_group_for(client)?.id.as_str() != group
                || !self
                    .session
                    .active_group_windows()
                    .iter()
                    .any(|window| window.id.as_str() == id)
            {
                return Err(MezError::conflict(
                    "selected window changed; refresh the list",
                ));
            }
            let focus_before = self.capture_zen_focus_snapshots();
            self.session.select_window(client, id)?;
            self.acknowledge_focused_pane_completion();
            self.reconcile_zen_focus_snapshots(focus_before);
            Ok(())
        })();
        match activation {
            Ok(()) => {
                self.presentation.primary_display_overlay = None;
                Ok(true)
            }
            Err(error) => {
                self.set_active_record_browser_error(error.message());
                self.reflow_primary_record_browser_overlay();
                Ok(true)
            }
        }
    }

    /// Rebuilds an in-memory catalog while preserving visible interaction state.
    pub(crate) fn refresh_terminal_window_browser(&mut self, client: &ClientId) -> Result<bool> {
        let (owner, group, selected) = self
            .presentation
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .and_then(|state| match state.source.as_ref()? {
                RuntimeRecordBrowserOverlaySource::TerminalWindows {
                    client_id,
                    group_id,
                } => Some((
                    client_id.clone(),
                    group_id.clone(),
                    state.browser.active_record_id().map(str::to_string),
                )),
                _ => None,
            })
            .ok_or_else(|| MezError::conflict("terminal catalog unavailable"))?;
        if owner != client.as_str()
            || !self.session.is_attached_primary(client)
            || self.session.active_group_for(client)?.id.as_str() != group
        {
            return Err(MezError::conflict("terminal catalog owner changed"));
        }
        let mut browser = self.terminal_window_record_browser()?;
        if let Some(id) = selected {
            browser.set_active_record_id(&id);
        }
        if let Some(state) = self
            .presentation
            .primary_display_overlay
            .as_mut()
            .and_then(|overlay| overlay.record_browser.as_mut())
        {
            state.browser = browser;
        }
        Ok(self.reflow_primary_record_browser_overlay())
    }

    /// Builds literal stable-ID rows from the current window catalog.
    pub(crate) fn terminal_window_record_browser(&self) -> Result<RecordBrowser> {
        let records = self
            .session
            .active_group_windows()
            .into_iter()
            .map(|window| RecordBrowserRecord {
                id: window.id.to_string(),
                open_command: None,
                title: window.name.clone(),
                metadata: vec![
                    ("Name".into(), window.name.clone()),
                    ("Panes".into(), window.panes().len().to_string()),
                ],
                markdown: format!("Window {}", window.id),
            })
            .collect();
        let mut browser = RecordBrowser::new("Choose window", records, Vec::new())?;
        browser.set_table_columns(vec!["Name".into(), "Panes".into()]);
        browser.set_help(
            Some(
                "**Keys:** `Enter` focus · `r` refresh · `/` search · `s` save · `Esc` dismiss"
                    .into(),
            ),
            None,
        );
        Ok(browser)
    }

    /// Executes terminal outcomes and presents their typed browser when applicable.
    /// RPC/offline execution remains textual and does not install an overlay.
    pub(crate) fn execute_and_present_terminal_command(
        &mut self,
        client: &ClientId,
        input: &str,
    ) -> Result<String> {
        self.prepare_client_render(client, mez_mux::presentation::ClientViewRole::Primary)?;
        self.require_live()?;
        let generation = self
            .presentation
            .overlay_action_registry
            .current_generation();
        let outcomes = crate::runtime::execute_runtime_command_sequence(self, client, input)?;
        let output = crate::runtime::runtime_command_outcomes_json(&outcomes);
        if self.require_live().is_err() {
            return Ok(output);
        }
        let handoff = if matches!(outcomes.last(), Some(CommandOutcome::Display { command, .. }) if command == "choose-window")
            && outcomes
                .iter()
                .filter(|outcome| {
                    matches!(
                        outcome,
                        CommandOutcome::Display { .. } | CommandOutcome::LiveDisplay { .. }
                    )
                })
                .count()
                == 1
        {
            let group_id = self
                .session
                .active_group()
                .ok_or_else(|| MezError::invalid_state("window group unavailable"))?
                .id
                .to_string();
            Some(TerminalBrowserHandoff {
                client: client.clone(),
                generation,
                state: RuntimeRecordBrowserOverlayState {
                    pane_id: self.active_pane_id()?.to_string(),
                    command: "choose-window".into(),
                    source: Some(RuntimeRecordBrowserOverlaySource::TerminalWindows {
                        client_id: client.as_str().into(),
                        group_id,
                    }),
                    browser: self.terminal_window_record_browser()?,
                    stack: Vec::new(),
                },
            })
        } else {
            None
        };
        let content = super::display_content::runtime_command_display_overlay_content(
            &output,
            &self.presentation.settings.ui_theme,
            usize::from(self.session.authoritative_size.columns),
            self.presentation.settings.terminal_agent_wrap_column_cap,
        )?;
        if let Some(handoff) = handoff {
            if handoff.client != *client
                || handoff.generation
                    != self
                        .presentation
                        .overlay_action_registry
                        .current_generation()
                || !self.session.is_attached_primary(client)
            {
                return Err(MezError::conflict("terminal browser handoff changed"));
            }
            self.show_primary_display_overlay_inner(
                content.lines,
                content.line_style_spans,
                content.line_copy_texts,
                Vec::new(),
                false,
            )?;
            if let Some(overlay) = self.presentation.primary_display_overlay.as_mut() {
                overlay.record_browser = Some(handoff.state);
            }
            self.reflow_primary_record_browser_overlay();
        } else {
            self.present_runtime_command_display_content(content)?;
        }
        Ok(output)
    }
}
