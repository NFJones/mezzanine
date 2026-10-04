//! Exact-client theme activation and in-memory retained-list refresh.
//!
//! Keyboard and mouse converge on a visible stable name, independently of
//! rendered metadata. Full set-theme settlement supplies persistence and partial
//! failure evidence. Browsing is inert; applying never dismisses or rolls back
//! earlier explicit selections. Search/save prompts retain shared precedence.

use crate::error::{MezError, Result};
use crate::runtime::service_state::RuntimeRecordBrowserOverlaySource;
use crate::runtime::{CommandInvocation, RuntimeSessionService};
use mez_core::ids::ClientId;

impl RuntimeSessionService {
    /// Handles only theme apply/refresh after the shared search and prompt gates.
    pub(crate) fn apply_theme_browser_input(
        &mut self,
        client: &ClientId,
        input: &[u8],
    ) -> Result<Option<bool>> {
        let Some(overlay) = self.presentation.primary_display_overlay.as_ref() else {
            return Ok(None);
        };
        let Some(state) = overlay.record_browser.as_ref() else {
            return Ok(None);
        };
        let Some(RuntimeRecordBrowserOverlaySource::Themes { client_id }) = state.source.as_ref()
        else {
            return Ok(None);
        };
        if client_id != client.as_str() || !self.session.is_attached_primary(client) {
            return Ok(Some(false));
        }
        if !matches!(input, b"r" | b"\r" | b"\n") {
            return Ok(None);
        }
        let selected = overlay
            .active_selection_index
            .and_then(|index| overlay.selections.get(index))
            .and_then(|selection| state.browser.records().get(selection.logical_id))
            .map(|record| record.id.clone());
        if input != b"r" && selected.is_none() {
            return Ok(Some(false));
        }
        let retained = selected
            .clone()
            .or_else(|| state.browser.active_record_id().map(str::to_string));
        let status = if input == b"r" {
            None
        } else {
            let name = selected.ok_or_else(|| MezError::conflict("theme selection unavailable"))?;
            let invocation = CommandInvocation {
                name: "set-theme".into(),
                args: vec![name],
            };
            Some(
                match crate::runtime::runtime_set_theme_command(self, &invocation) {
                    Ok(response) => response,
                    Err(error) => error.message().to_string(),
                },
            )
        };
        let mut browser = match self.theme_record_browser() {
            Ok(browser) => browser,
            Err(error) => {
                self.set_active_record_browser_error(error.message());
                self.reflow_primary_record_browser_overlay();
                return Ok(Some(true));
            }
        };
        if let Some(id) = retained {
            browser.set_active_record_id(&id);
        }
        browser.set_error(status);
        if let Some(state) = self
            .presentation
            .primary_display_overlay
            .as_mut()
            .and_then(|overlay| overlay.record_browser.as_mut())
        {
            state.browser = browser;
        }
        self.reflow_primary_record_browser_overlay();
        Ok(Some(true))
    }
}
