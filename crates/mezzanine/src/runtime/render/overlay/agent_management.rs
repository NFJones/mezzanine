//! Client-local administrative browser operations and exact close confirmation.
//!
//! Selection geometry identifies a retained stable ID, never a displayed label.
//! Native controls reuse lifecycle authority; focus is global but client-local.
//! An armed close binds the snapshot and overlay generation and never follows
//! a neighboring row after navigation or refresh. Search and save stay shared.

use crate::error::{MezError, Result};
use crate::runtime::RuntimeSessionService;
use crate::runtime::control::agent_browser::AgentCloseConfirmation;
use crate::runtime::service_state::RuntimeRecordBrowserOverlaySource;
use mez_core::ids::ClientId;

impl RuntimeSessionService {
    /// Rebuilds the live in-memory snapshot, preserving search/viewport and ID.
    pub(crate) fn refresh_agent_management_overlay(&mut self, client: &ClientId) -> Result<bool> {
        let (owner, selected) = self
            .presentation
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.record_browser.as_ref())
            .and_then(|state| match state.source.as_ref()? {
                RuntimeRecordBrowserOverlaySource::Agents { client_id, .. } => Some((
                    client_id.clone(),
                    state.browser.active_record_id().map(str::to_string),
                )),
                _ => None,
            })
            .ok_or_else(|| MezError::conflict("agent browser unavailable"))?;
        if owner != client.as_str() {
            return Err(MezError::forbidden(
                "agent browser belongs to another primary",
            ));
        }
        let (mut browser, targets) = self.agent_management_browser(client)?;
        if let Some(selected) = selected {
            browser.set_active_record_id(&selected);
        }
        if let Some(state) = self
            .presentation
            .primary_display_overlay
            .as_mut()
            .and_then(|overlay| overlay.record_browser.as_mut())
        {
            state.source = Some(RuntimeRecordBrowserOverlaySource::Agents {
                client_id: owner,
                targets,
                confirmation: None,
            });
            state.browser = browser;
        }
        Ok(self.reflow_primary_record_browser_overlay())
    }

    /// Handles administrative keys only after shared search/prompt precedence.
    pub(crate) fn apply_agent_management_overlay_input(
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
        let Some(RuntimeRecordBrowserOverlaySource::Agents {
            client_id,
            targets,
            confirmation,
        }) = state.source.as_ref()
        else {
            return Ok(None);
        };
        if client_id != client.as_str() || !self.session.is_attached_primary(client) {
            return Ok(Some(false));
        }
        if let Some(confirmation) = confirmation.clone() {
            let result = if input == b"y" {
                (|| -> Result<()> {
                    if confirmation.generation
                        != self
                            .presentation
                            .overlay_action_registry
                            .current_generation()
                    {
                        return Err(MezError::conflict(
                            "close confirmation changed; refresh and confirm again",
                        ));
                    }
                    self.validate_agent_browser_target(client, &confirmation.target)?;
                    let target = confirmation
                        .target
                        .lifecycle
                        .as_ref()
                        .ok_or_else(|| MezError::invalid_state("native close unavailable"))?;
                    self.close_agent_lifecycle_target(client, target, true)?;
                    Ok(())
                })()
            } else if matches!(input, b"n" | b"\x1b") {
                Ok(())
            } else {
                return Ok(Some(true));
            };
            if self.require_live().is_err() {
                return Ok(Some(true));
            }
            self.refresh_agent_management_overlay(client)?;
            if let Err(error) = result {
                self.set_active_record_browser_error(error.message());
                self.reflow_primary_record_browser_overlay();
            }
            return Ok(Some(true));
        }
        if input == b"r" {
            return self.refresh_agent_management_overlay(client).map(Some);
        }
        if !matches!(input, b"\r" | b"\n" | b"i" | b"p" | b"d") {
            return Ok(None);
        }
        let Some(selection) = overlay
            .active_selection_index
            .and_then(|index| overlay.selections.get(index))
        else {
            return Ok(Some(false));
        };
        let Some(record) = state.browser.records().get(selection.logical_id) else {
            return Ok(Some(false));
        };
        let id = record.id.clone();
        let Some(target) = targets.get(&id).cloned() else {
            return Ok(Some(false));
        };
        let result = (|| -> Result<()> {
            let pane = self.validate_agent_browser_target(client, &target)?;
            if matches!(input, b"\r" | b"\n") {
                let focus = self.capture_zen_focus_snapshots();
                self.session.select_pane_global(client, &pane)?;
                self.acknowledge_focused_pane_completion();
                self.reconcile_zen_focus_snapshots(focus);
                self.presentation.primary_display_overlay = None;
                return Ok(());
            }
            let lifecycle = target.lifecycle.as_ref().ok_or_else(|| {
                MezError::invalid_state(
                    "native lifecycle control unavailable for this registration",
                )
            })?;
            match input {
                b"i" => {
                    self.interrupt_agent_lifecycle_target(client, lifecycle)?;
                }
                b"p" => {
                    if let Some(generation) = target.pause_generation {
                        self.resume_agent_lifecycle_target(client, lifecycle, generation)?;
                    } else {
                        self.pause_agent_lifecycle_target(client, lifecycle)?;
                    }
                }
                b"d" => {
                    self.validate_agent_lifecycle_target(client, lifecycle)?;
                    self.set_active_record_browser_error(&format!("Confirm close {id} in pane {pane}, including its live process: y confirm force-close / n or Esc cancel"));
                    self.reflow_primary_record_browser_overlay();
                    let generation = self
                        .presentation
                        .overlay_action_registry
                        .current_generation();
                    if let Some(RuntimeRecordBrowserOverlaySource::Agents {
                        confirmation, ..
                    }) = self
                        .presentation
                        .primary_display_overlay
                        .as_mut()
                        .and_then(|overlay| overlay.record_browser.as_mut())
                        .and_then(|state| state.source.as_mut())
                    {
                        *confirmation = Some(Box::new(AgentCloseConfirmation {
                            id: id.clone(),
                            target: target.clone(),
                            generation,
                        }));
                    }
                    return Ok(());
                }
                _ => {}
            }
            self.refresh_agent_management_overlay(client)?;
            Ok(())
        })();
        if let Err(error) = result {
            self.set_active_record_browser_error(error.message());
            self.reflow_primary_record_browser_overlay();
        }
        Ok(Some(true))
    }
}
