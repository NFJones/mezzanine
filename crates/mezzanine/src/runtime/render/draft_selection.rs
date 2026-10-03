//! Source-fenced, client-local draft selection independent of transcript copy.
//!
//! Readline owns entered bytes; the existing prompt layout owns visible cells.
//! Pointer gestures stay in their starting domain, never scroll the log, and
//! default copy never expands hidden paste. Exact projection/conversation
//! checks make changed drafts and rebindings inert rather than copying stale
//! offsets. Selection state lives with the client-local prompt, not history.

use super::{CopyPosition, MouseAction, Result, RuntimeSessionService};
use mez_mux::presentation::ReadlinePromptRegion;
use mez_mux::readline::ReadlineSourceProjection;
use mez_mux::render::WrappedPromptLayout;
use std::sync::Arc;

/// One exact draft selection using UTF-8 display endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DraftSelection {
    /// Conversation owning the draft when selection began.
    conversation: String,
    /// Immutable display and entered-source evidence.
    projection: Arc<ReadlineSourceProjection>,
    /// Original endpoint retained throughout a drag.
    anchor: usize,
    /// Immutable bounds of the pressed grapheme for directional mouse drags.
    anchor_cell: std::ops::Range<usize>,
    /// Current exclusive endpoint.
    end: usize,
    /// Whether subsequent pointer motion belongs to this selection.
    dragging: bool,
    /// Whether explicit draft-copy mode owns keyboard input.
    keyboard: bool,
    /// Last draft press for domain-local double-click recognition.
    click: Option<(CopyPosition, u64)>,
}

/// Exact visible draft geometry and source, rebuilt without filesystem I/O.
struct DraftView {
    region: ReadlinePromptRegion,
    layout: WrappedPromptLayout,
    projection: Arc<ReadlineSourceProjection>,
}

impl DraftView {
    /// Checks ordered source associations without scanning an entire draft for
    /// every visible cell. Provisional completion ranges have no authority.
    fn entered_cell(&self, cell: &mez_mux::render::PromptSourceSpan) -> bool {
        let first = self
            .projection
            .spans
            .partition_point(|span| span.display.end <= cell.input.start);
        self.projection.spans[first..]
            .iter()
            .take_while(|span| span.display.start < cell.input.end)
            .any(|span| span.source.is_some())
    }

    /// Requires an actual entered cell for a gesture start, never padding,
    /// marker, continuation indentation or an unaccepted completion.
    fn entered_at(&self, position: CopyPosition) -> bool {
        if !self.contains(position) {
            return false;
        }
        let row = position.line - self.region.row;
        let column = position.column - self.region.column;
        self.layout.source_spans.get(row).is_some_and(|spans| {
            spans.iter().any(|cell| {
                column >= cell.start && column < cell.start + cell.length && self.entered_cell(cell)
            })
        })
    }

    /// Maps a visible cell to a whole-grapheme display boundary. Outside drag
    /// positions clamp to this draft's visible extent, not to another domain.
    fn endpoint(&self, position: CopyPosition, end: bool) -> Option<usize> {
        let row = position
            .line
            .saturating_sub(self.region.row)
            .min(self.region.rows.saturating_sub(1));
        let column = position.column.saturating_sub(self.region.column);
        let spans = self.layout.source_spans.get(row)?;
        let entered = spans
            .iter()
            .filter(|cell| self.entered_cell(cell))
            .collect::<Vec<_>>();
        let Some(first) = entered.first() else {
            return spans
                .iter()
                .find(|cell| cell.length == 0)
                .map(|cell| cell.input.start);
        };
        for cell in &entered {
            if column < cell.start {
                return Some(cell.input.start);
            }
            if column < cell.start + cell.length {
                return Some(if end {
                    cell.input.end
                } else {
                    cell.input.start
                });
            }
        }
        Some(
            entered
                .last()
                .map_or(first.input.start, |cell| cell.input.end),
        )
    }

    /// Tests membership in the actual editable rectangle, excluding decoration.
    fn contains(&self, position: CopyPosition) -> bool {
        position.line >= self.region.row
            && position.line < self.region.row + self.region.rows
            && position.column >= self.region.column
            && position.column < self.region.column + self.region.columns
    }
}

impl RuntimeSessionService {
    /// Captures the exact current editable rectangle and canonical source map.
    fn draft_selection_view(&self, pane_id: &str) -> Option<DraftView> {
        if self.presentation.primary_display_overlay.is_some()
            || self.presentation.primary_prompt_input.is_some()
            || self.external_editor_session_is_active(pane_id)
            || self.agent_command_is_active(pane_id)
            || self.presented_pane_surface(pane_id) != crate::runtime::PaneSurfaceKind::Agent
        {
            return None;
        }
        let window = self.session.active_window()?;
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.id.as_str() == pane_id)?;
        let (row, column, size) = self.pane_content_mouse_region(window, pane.index)?;
        let geometry = self.agent_composer_layout_for_pane(
            pane_id,
            usize::from(size.columns),
            usize::from(size.rows),
        );
        let mut region = geometry.editable?;
        region.row += row;
        region.column += column;
        let prompt = &self.presentation.agent_prompt_inputs.get(pane_id)?.prompt;
        let (layout, projection) = crate::host::terminal::agent_draft_selection_layout(
            prompt,
            region.columns,
            region.rows,
        )?;
        Some(DraftView {
            region,
            layout,
            projection,
        })
    }

    /// Returns selected entered display/source only while exact ownership holds.
    pub(crate) fn copy_draft_selection(&self, pane_id: &str, source: bool) -> Option<String> {
        let state = self
            .presentation
            .agent_prompt_inputs
            .get(pane_id)?
            .draft_selection
            .as_ref()?;
        let view = self.draft_selection_view(pane_id)?;
        if self.agent_shell_store().get(pane_id)?.session_id != state.conversation
            || view.projection != state.projection
        {
            return None;
        }
        state.projection.copy_range(
            state.anchor.min(state.end)..state.anchor.max(state.end),
            source,
        )
    }

    /// Clears only draft selection, leaving retained transcript copying intact.
    pub(crate) fn clear_draft_selection(&mut self, pane_id: &str) {
        if let Some(input) = self.presentation.agent_prompt_inputs.get_mut(pane_id) {
            input.draft_selection = None;
        }
    }

    /// Starts explicit keyboard draft selection over entered display text.
    /// The existing command lane supplies the discoverable keyboard route.
    pub(crate) fn begin_draft_selection(&mut self, pane_id: &str) -> Result<()> {
        let view = self.draft_selection_view(pane_id).ok_or_else(|| {
            super::MezError::invalid_state("draft is not selectable on this surface")
        })?;
        let conversation = self
            .agent_shell_store()
            .get(pane_id)
            .ok_or_else(|| super::MezError::invalid_state("draft conversation disappeared"))?
            .session_id
            .clone();
        let anchor = view
            .projection
            .spans
            .iter()
            .find(|span| span.source.is_some())
            .map_or(view.projection.display.len(), |span| span.display.start);
        let end = view.projection.display.len();
        if let Some(input) = self.presentation.agent_prompt_inputs.get_mut(pane_id) {
            input.draft_selection = Some(DraftSelection {
                conversation,
                projection: view.projection,
                anchor,
                anchor_cell: anchor..anchor,
                end,
                dragging: false,
                keyboard: true,
                click: None,
            });
        }
        Ok(())
    }

    /// Reports valid draft keyboard ownership, never deriving it from visibility.
    pub(super) fn draft_keyboard_selection_active(&self, pane_id: &str) -> bool {
        self.presentation
            .agent_prompt_inputs
            .get(pane_id)
            .and_then(|input| input.draft_selection.as_ref())
            .is_some_and(|state| state.keyboard)
            && self.copy_draft_selection(pane_id, false).is_some()
    }

    /// Applies explicit draft-copy keys without changing readline or log state.
    /// Space copies the current rendered selection; Escape returns ownership to
    /// the preceding interaction. Horizontal endpoints move by grapheme.
    pub(super) fn apply_draft_copy_key(
        &mut self,
        pane_id: &str,
        action: super::CopyModeKeyAction,
        suppress_clipboard: bool,
    ) -> Result<(bool, Option<String>)> {
        use super::CopyModeKeyAction;
        if action == CopyModeKeyAction::Cancel {
            self.clear_draft_selection(pane_id);
            return Ok((true, None));
        }
        if action == CopyModeKeyAction::BeginSelection {
            let copied = self
                .copy_draft_selection(pane_id, false)
                .ok_or_else(|| super::MezError::invalid_state("draft selection is stale"))?;
            self.copy_text_to_buffer_and_host_clipboard(
                "clipboard",
                copied.clone(),
                format!("pane:{pane_id}:draft"),
                suppress_clipboard,
            )?;
            return Ok((true, Some(copied)));
        }
        if let Some(input) = self.presentation.agent_prompt_inputs.get_mut(pane_id)
            && let Some(state) = input.draft_selection.as_mut()
        {
            let text = &state.projection.display;
            let mut boundaries = vec![0];
            let mut offset = 0;
            for grapheme in mez_terminal::terminal_graphemes(text) {
                offset += grapheme.len();
                boundaries.push(offset);
            }
            state.end = match action {
                CopyModeKeyAction::MoveLeft | CopyModeKeyAction::MoveWordLeft => boundaries
                    .iter()
                    .copied()
                    .take_while(|boundary| *boundary < state.end)
                    .last()
                    .unwrap_or(state.anchor),
                CopyModeKeyAction::MoveRight | CopyModeKeyAction::MoveWordRight => boundaries
                    .iter()
                    .copied()
                    .find(|boundary| *boundary > state.end)
                    .unwrap_or(text.len()),
                CopyModeKeyAction::Top | CopyModeKeyAction::LineStart => state.anchor,
                CopyModeKeyAction::Bottom | CopyModeKeyAction::LineEnd => text.len(),
                _ => state.end,
            };
        }
        Ok((true, None))
    }

    /// Reports an in-flight draft gesture for pointer routing, including a stale
    /// gesture whose release must be consumed rather than sent to the log.
    pub(super) fn draft_mouse_drag_active(&self) -> bool {
        self.presentation.agent_prompt_inputs.values().any(|input| {
            input
                .draft_selection
                .as_ref()
                .is_some_and(|state| state.dragging)
        })
    }

    /// Adds selection highlights only to entered cells in the editable region.
    /// Reflow changes cell positions, not the retained UTF-8 selection anchors.
    pub(super) fn overlay_draft_selections(&self, view: &mut super::RenderedClientView) {
        for (pane_id, input) in self.presentation.agent_prompt_inputs.iter() {
            let Some(state) = input.draft_selection.as_ref() else {
                continue;
            };
            if self.copy_draft_selection(pane_id, false).is_none() {
                continue;
            }
            let Some(draft) = self.draft_selection_view(pane_id) else {
                continue;
            };
            let selected = state.anchor.min(state.end)..state.anchor.max(state.end);
            for (row, spans) in draft.layout.source_spans.iter().enumerate() {
                let Some(target) = view.line_style_spans.get_mut(draft.region.row + row) else {
                    continue;
                };
                for cell in spans {
                    if cell.input.start < selected.end
                        && cell.input.end > selected.start
                        && draft.entered_cell(cell)
                        && cell.start < draft.region.columns
                    {
                        target.push(mez_terminal::TerminalStyleSpan {
                            start: draft.region.column + cell.start,
                            length: cell.length.min(draft.region.columns - cell.start),
                            rendition: super::client_view::copy_selection_rendition(&view.ui_theme),
                        });
                    }
                }
            }
            if state.keyboard {
                view.cursor_visible = false;
            }
        }
    }

    /// Routes draft gestures before transcript hit testing. A log-started drag
    /// cannot switch domains, and draft-started gestures retain the log snapshot.
    pub(super) fn apply_draft_mouse_action(
        &mut self,
        client: &mez_core::ids::ClientId,
        action: &MouseAction,
        suppress_clipboard: bool,
    ) -> Result<Option<(bool, Option<String>)>> {
        if matches!(action, MouseAction::ScrollHistory { .. }) && self.draft_mouse_drag_active() {
            // Draft-local dragging never scrolls the retained log snapshot.
            return Ok(Some((true, None)));
        }
        let position = match action {
            MouseAction::CopySelectionStart(position)
            | MouseAction::CopySelectionUpdate(position)
            | MouseAction::CopySelectionFinish(position)
            | MouseAction::CopyWord(position)
            | MouseAction::FocusPane(position) => *position,
            _ => return Ok(None),
        };
        let dragging = self
            .presentation
            .agent_prompt_inputs
            .iter()
            .find_map(|(pane, input)| {
                input
                    .draft_selection
                    .as_ref()
                    .filter(|state| state.dragging)
                    .map(|_| pane.clone())
            });
        let update = matches!(
            action,
            MouseAction::CopySelectionUpdate(_) | MouseAction::CopySelectionFinish(_)
        );
        if update && dragging.is_none() {
            return Ok(None);
        }
        if !update
            && matches!(action, MouseAction::FocusPane(_))
            && self.presentation.mouse_selection_drag_state.is_some()
        {
            return Ok(None);
        }
        let pane_id = dragging.or_else(|| {
            self.session
                .active_window()?
                .panes()
                .iter()
                .find_map(|pane| {
                    self.draft_selection_view(pane.id.as_str())
                        .filter(|view| view.contains(position))
                        .map(|_| pane.id.to_string())
                })
        });
        let Some(pane_id) = pane_id else {
            return Ok(None);
        };
        let Some(view) = self.draft_selection_view(&pane_id) else {
            self.clear_draft_selection(&pane_id);
            return Ok(Some((true, None)));
        };
        if !update && !view.entered_at(position) {
            return Ok(Some((true, None)));
        }
        self.session.select_pane_global(client, &pane_id)?;
        if update {
            if self.copy_draft_selection(&pane_id, false).is_none() {
                if let Some(input) = self.presentation.agent_prompt_inputs.get_mut(&pane_id) {
                    input.draft_selection = None;
                }
                return Ok(Some((true, None)));
            }
            let Some(start) = view.endpoint(position, false) else {
                if matches!(action, MouseAction::CopySelectionFinish(_)) {
                    self.clear_draft_selection(&pane_id);
                }
                return Ok(Some((true, None)));
            };
            let end = view.endpoint(position, true).unwrap_or(start);
            if let Some(input) = self.presentation.agent_prompt_inputs.get_mut(&pane_id)
                && let Some(state) = input.draft_selection.as_mut()
            {
                if start < state.anchor_cell.start {
                    state.anchor = state.anchor_cell.end;
                    state.end = start;
                } else {
                    state.anchor = state.anchor_cell.start;
                    state.end = end;
                }
                state.dragging = !matches!(action, MouseAction::CopySelectionFinish(_));
            }
            if matches!(action, MouseAction::CopySelectionFinish(_)) {
                let copied = self
                    .copy_draft_selection(&pane_id, false)
                    .unwrap_or_default();
                self.copy_text_to_buffer_and_host_clipboard(
                    "mouse",
                    copied.clone(),
                    format!("pane:{pane_id}:draft"),
                    suppress_clipboard,
                )?;
                return Ok(Some((true, Some(copied))));
            }
            return Ok(Some((true, None)));
        }
        let Some(anchor) = view.endpoint(position, false) else {
            return Ok(Some((true, None)));
        };
        let anchor_end = view.endpoint(position, true).unwrap_or(anchor);
        let now = super::current_unix_millis();
        let repeated = self
            .presentation
            .agent_prompt_inputs
            .get(&pane_id)
            .and_then(|input| input.draft_selection.as_ref())
            .is_some_and(|state| {
                state.projection == view.projection
                    && state.click.is_some_and(|(previous, at)| {
                        previous == position
                            && now.saturating_sub(at)
                                <= super::DOUBLE_CLICK_WORD_SELECTION_WINDOW_MS
                    })
            });
        let word = matches!(action, MouseAction::CopyWord(_)) || repeated;
        let mut selected = anchor..anchor;
        if word {
            let display = &view.projection.display;
            let start = display[..anchor]
                .char_indices()
                .rev()
                .find(|(_, ch)| ch.is_whitespace())
                .map_or(0, |(at, ch)| at + ch.len_utf8());
            let end = display[anchor..]
                .char_indices()
                .find(|(_, ch)| ch.is_whitespace())
                .map_or(display.len(), |(at, _)| anchor + at);
            selected = start..end;
        }
        let conversation = self
            .agent_shell_store()
            .get(&pane_id)
            .map(|session| session.session_id.clone())
            .unwrap_or_default();
        if let Some(input) = self.presentation.agent_prompt_inputs.get_mut(&pane_id) {
            input.draft_selection = Some(DraftSelection {
                conversation,
                projection: view.projection,
                anchor: selected.start,
                anchor_cell: anchor..anchor_end,
                end: selected.end,
                dragging: !word,
                keyboard: false,
                click: Some((position, now)),
            });
        }
        if word {
            let copied = self
                .copy_draft_selection(&pane_id, false)
                .unwrap_or_default();
            self.copy_text_to_buffer_and_host_clipboard(
                "mouse",
                copied.clone(),
                format!("pane:{pane_id}:draft-word"),
                suppress_clipboard,
            )?;
            return Ok(Some((true, Some(copied))));
        }
        Ok(Some((true, None)))
    }
}
