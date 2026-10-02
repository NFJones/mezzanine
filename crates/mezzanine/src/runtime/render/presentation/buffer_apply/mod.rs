//! Runtime service application of projected agent presentation rows.
//!
//! The runtime service owns the sole conversation-bound screen and presentation
//! state. Children separate geometry, source ingestion, immutable worker rendering,
//! freshness acceptance, lifecycle, message acceptance, replay, resize, and shell
//! previews. Ordinary pane writes share these components rather than maintaining
//! another baseline store; delayed work must retain exact response and lineage
//! identity before it can replace visible rows or publish accepted sources.

mod geometry;
mod outbound_messages;
mod replay;
mod resize;
mod say_rendering;
mod shell_previews;
mod streaming_composition;
mod streaming_lifecycle;
mod streaming_projection;
mod streaming_reconciliation;
mod streaming_source;

use super::actions::{
    agent_action_execution_display_header, agent_action_execution_rendered_line,
    agent_action_model_thinking_lines, agent_action_result_display_header,
    agent_macro_lifecycle_display_lines_for_width, agent_thinking_display_lines_for_width,
    bounded_agent_action_result_display_lines, streaming_action_execution_display_header,
};
use super::diff::{
    agent_action_result_uses_diff_preview, cleaned_agent_diff_source_lines,
    readable_agent_diff_display_lines_for_width, streaming_agent_diff_display_lines_for_width,
};
use super::style::{
    AGENT_TERMINAL_MESSAGE_PREFIX, AgentTerminalPresentationStyle, agent_name_marker_rendition,
};
use super::text::{
    AGENT_MESSAGE_CONTINUATION_INDENT, agent_say_text_is_displayed_patch_block,
    agent_terminal_label_rendition, append_styled_agent_terminal_line,
    append_styled_agent_terminal_rendered_line, bounded_agent_terminal_presentation_columns,
    bounded_command_preview_source, command_preview_terminal_rendered_lines,
    render_agent_markdown_body_lines, render_agent_markdown_body_lines_with_prefix,
    sanitized_agent_terminal_line, shell_output_preview_visual_rows,
    wrapped_prefixed_agent_terminal_lines,
};
use super::{
    AGENT_COPY_SKIP_LINE, AgentAction, GraphicRendition, RichTextLine, RichTextLineKind,
    TerminalStyleSpan, UnicodeWidthStr, diff_section_path, frame_markdown_lines,
    parse_unified_diff_sections, prefix_rich_text_lines, wrap_rich_text_lines_to_width,
};
use crate::runtime::render::{
    ActionResult, AgentPresentationEntry, MezError, Result, RuntimeAgentShellPreviewOwner,
    RuntimeSessionService, RuntimeStreamingMessageSource, RuntimeStreamingSayAction,
    RuntimeStreamingSayPresentation, RuntimeStreamingSayProjectionContext, Size, TerminalScreen,
    current_unix_seconds, default_runtime_agent_prompt_input,
};
use crate::runtime::{
    PeerMessageLogMode, runtime_agent_peer_message_log_mode_from_config,
    runtime_effective_config_value, runtime_peer_message_presentation_is_markdown,
    runtime_peer_message_presentation_is_visible,
};
use crate::ui::readline::AGENT_PROMPT_TEXT_PREFIX;
use mez_agent::{
    AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE, AgentActionPayload, AgentShellVisibility,
    agent_output_content_type_is_diff, agent_output_content_type_is_markdown,
};
use mez_mux::{
    copy::{COPY_WRAP_CONTINUATION, encode_copy_source_line_in_group},
    render::{
        markdown_block_copy_lines, markdown_local_continuation_indent_width,
        wrap_rich_text_line_to_width_with_continuation_indent_hard,
        wrap_rich_text_line_to_width_with_prefix_and_continuation_indent_hard,
        wrap_rich_text_line_to_width_with_source_ranges_hard,
    },
};

/// Content type for width-independent styled agent presentation records.
pub(super) const AGENT_PRESENTATION_STYLED_LINES_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.styled-lines+json; charset=utf-8";
/// Content type for a raw user prompt that must be wrapped at replay geometry.
const AGENT_PRESENTATION_USER_PROMPT_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.user-prompt+text; charset=utf-8";
/// Content type for a shell command preview rendered at replay geometry.
const AGENT_PRESENTATION_COMMAND_PREVIEW_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.command-preview+text; charset=utf-8";
/// Content type for a bounded command preview whose source omitted a tail.
const AGENT_PRESENTATION_TRUNCATED_COMMAND_PREVIEW_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.command-preview-truncated+text; charset=utf-8";
/// Content type for one action-execution header rendered at replay geometry.
const AGENT_PRESENTATION_ACTION_HEADER_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.action-header+text; charset=utf-8";
/// Content type for a parent-supplied subagent prompt rendered at replay geometry.
const AGENT_PRESENTATION_PARENT_PROMPT_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.parent-prompt+text; charset=utf-8";
/// Content type for rationale text rendered at replay geometry.
const AGENT_PRESENTATION_THINKING_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.thinking+text; charset=utf-8";
/// Reserved presentation marker for an accepted rationale, never an action index.
const STREAMED_RATIONALE_PRESENTED_MARKER: usize = usize::MAX;
/// Marks a projection dirty only when closing one field can expose a later ordinal.
fn release_later_streaming_action(
    presentation: &mut RuntimeStreamingSayPresentation,
    action_index: usize,
) {
    let later = presentation
        .actions
        .keys()
        .any(|index| *index > action_index)
        || presentation
            .outbound_messages
            .keys()
            .any(|index| *index > action_index)
        || presentation
            .shell_commands
            .keys()
            .any(|index| *index > action_index)
        || presentation
            .shell_summaries
            .keys()
            .any(|index| *index > action_index)
        || presentation
            .action_headers
            .keys()
            .any(|index| *index > action_index);
    if later
        && (presentation.received_actions.is_empty()
            || presentation.received_actions.contains(&action_index))
    {
        presentation.revision = presentation.revision.wrapping_add(1);
        presentation.projected_revision = None;
    }
}
/// Content type for structured macro lifecycle rows rendered at replay geometry.
const AGENT_PRESENTATION_MACRO_LIFECYCLE_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.macro-lifecycle+json; charset=utf-8";
/// Content type for one logged interagent peer message rendered at replay geometry.
///
/// The source is a JSON record holding the peer name and payload exactly as the
/// echo received them, rather than plain text, because the peer name must
/// survive replay: a rebuilt line has to show the same origin as the live line,
/// and the plain-text user-prompt content type cannot carry it. The record keeps
/// the unbounded values so replay can hand the renderer the same input the live
/// writer had and rebuild a byte-identical line.
pub(crate) const AGENT_PRESENTATION_PEER_MESSAGE_CONTENT_TYPE: &str =
    "application/vnd.mezzanine.agent-presentation.peer-message+json; charset=utf-8";
/// Maximum byte length of one persisted peer-message source accepted at replay.
///
/// The writer stores the peer name and payload it was given, so a record larger
/// than this can only come from a corrupted or hand-edited log. Replay skips
/// such a record instead of decoding an arbitrarily large stored source.
const AGENT_PRESENTATION_PEER_MESSAGE_SOURCE_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Placeholder peer name used when a peer identity is absent or empty.
const AGENT_PRESENTATION_UNKNOWN_PEER_LABEL: &str = "agent-unknown";

/// Persisted source for one replayable peer-message log line.
///
/// The stored peer name and payload are the unbounded values the echo received.
/// Bounding and sanitizing happen once, in the renderer that the live writer and
/// the replay decoder both call, so the two paths cannot drift apart.
///
/// The envelope media type is stored so replay applies the same normal-mode
/// canonical-plaintext filter as the live renderer. A record written before
/// commit-time eligibility existed keeps `None`, allowing legacy received rows
/// to retain their historical normal/verbose replay policy.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PeerMessagePresentationSource {
    direction: String,
    /// Stable MMP delivery identity used to recover one receiver-owned row.
    #[serde(default)]
    receive_identity: Option<String>,
    /// Stable sender action identity used only to distinguish durable sent rows.
    #[serde(default)]
    action_identity: Option<String>,
    peer: String,
    payload: String,
    /// Envelope media type the live echo gated on; absent on legacy records.
    #[serde(default)]
    content_type: Option<String>,
    /// Whether the writer resolved this received row as the recipient's direct parent.
    #[serde(default)]
    direct_parent: bool,
    /// Whether receive commit selected this row for receiver presentation.
    #[serde(default)]
    presentation_eligible: Option<bool>,
}

/// Borrowed inputs shared by live rendering and persistence-only retries.
pub(crate) struct PeerMessagePresentation<'a> {
    pub(crate) receive_identity: Option<&'a str>,
    pub(crate) peer_label: &'a str,
    pub(crate) content_type: Option<&'a str>,
    pub(crate) payload: &'a str,
    pub(crate) direct_parent: bool,
    pub(crate) presentation_eligible: bool,
}

/// Returns the receiver-owned identity encoded by one peer presentation source.
///
/// Durable receipt settlement must compare this parsed field exactly. Searching
/// raw JSON would let payload text masquerade as an unrelated receipt identity.
pub(crate) fn peer_message_presentation_receive_identity(
    source_content_type: &str,
    source_text: &str,
) -> Option<String> {
    if source_content_type != AGENT_PRESENTATION_PEER_MESSAGE_CONTENT_TYPE {
        return None;
    }
    if source_text.len() > AGENT_PRESENTATION_PEER_MESSAGE_SOURCE_MAX_BYTES {
        return None;
    }
    let encoded = serde_json::from_str::<PeerMessagePresentationSource>(source_text).ok()?;
    (encoded.direction == "received")
        .then_some(encoded.receive_identity)
        .flatten()
}

/// Returns the bounded, control-character-free peer name shown in a log line.
///
/// The name is untrusted peer data exactly like a discovery string, so it is
/// bounded and sanitized the same way at render time, for a live append and a
/// replayed line alike; an absent identity degrades to a fixed placeholder
/// instead of a bare arrow.
fn peer_message_echo_label(peer_label: &str) -> String {
    let bounded = mez_agent::agent_list_bounded_text(peer_label);
    if bounded.trim().is_empty() {
        AGENT_PRESENTATION_UNKNOWN_PEER_LABEL.to_string()
    } else {
        bounded
    }
}

/// Returns one peer payload bounded exactly like the injected peer context.
///
/// The echoed line must never exceed the peer-context bound, so a logged echo
/// and the model-visible peer block truncate at the same limit.
fn peer_message_echo_payload(payload: &str) -> String {
    crate::runtime::control::runtime_peer_message_logged_payload(payload)
}

/// Renders one Markdown peer payload while preserving its sender indicator.
fn peer_message_markdown_rendered_lines(
    prefix: &str,
    payload: &str,
    ui_theme: &mez_mux::theme::UiTheme,
    table_display_width: usize,
) -> Vec<RichTextLine> {
    let continuation = AGENT_MESSAGE_CONTINUATION_INDENT;
    render_agent_markdown_body_lines_with_prefix(
        payload,
        ui_theme,
        table_display_width,
        prefix,
        continuation,
    )
    .into_iter()
    .flat_map(|line| {
        let first_row = line.display.starts_with(prefix);
        let rest = if first_row {
            line.display.strip_prefix(prefix).unwrap_or_default()
        } else {
            line.display
                .strip_prefix(continuation)
                .unwrap_or(line.display.as_str())
        };
        let indent_width = UnicodeWidthStr::width(continuation)
            .saturating_add(markdown_local_continuation_indent_width(rest))
            .min(table_display_width.saturating_sub(1));
        wrap_rich_text_line_to_width_with_prefix_and_continuation_indent_hard(
            line,
            table_display_width,
            if first_row {
                UnicodeWidthStr::width(prefix)
            } else {
                UnicodeWidthStr::width(continuation)
            },
            &" ".repeat(indent_width),
        )
    })
    .collect()
}

/// Returns presentation and source-copy rows for one canonical plaintext peer payload.
fn peer_message_echo_rendered_lines(
    prefix: &str,
    payload: &str,
    display_width: usize,
    copy_group: &str,
) -> Vec<RichTextLine> {
    let body_indent = AGENT_MESSAGE_CONTINUATION_INDENT;
    let payload = payload.trim_end_matches(['\r', '\n']);
    let payload_lines = if payload.is_empty() {
        vec![""]
    } else {
        payload.lines().collect::<Vec<_>>()
    };
    payload_lines
        .iter()
        .enumerate()
        .flat_map(|(source_index, payload_line)| {
            let source = encode_copy_source_line_in_group(copy_group, source_index, payload_line);
            let line = RichTextLine {
                display: format!(
                    "{}{}",
                    if source_index == 0 {
                        prefix
                    } else {
                        body_indent
                    },
                    sanitized_agent_terminal_line(payload_line),
                ),
                style_spans: Vec::new(),
                copy_text: Some(source.clone()),
                kind: RichTextLineKind::Normal,
            };
            wrap_rich_text_line_to_width_with_continuation_indent_hard(
                line,
                display_width,
                body_indent,
            )
            .into_iter()
            .enumerate()
            .map(move |(index, mut line)| {
                if index > 0 || line.copy_text.as_deref() == Some(COPY_WRAP_CONTINUATION) {
                    line.copy_text = Some(AGENT_COPY_SKIP_LINE.to_string());
                }
                line
            })
        })
        .collect()
}

/// Builds one provisional sender-side peer-message projection.
///
/// Provider output is only a candidate action at this point, so this helper
/// deliberately produces terminal rows and copy metadata only. Settlement and
/// durable presentation remain owned by the message-execution path.
fn streaming_outbound_message_projection_with_theme(
    message: &RuntimeStreamingMessageSource,
    frame_width: usize,
    table_width: usize,
    ui_theme: &mez_mux::theme::UiTheme,
) -> StreamingSayProjection {
    let label = if message.direct_parent {
        "parent".to_string()
    } else {
        peer_message_echo_label(&message.recipient_label)
    };
    let marker = format!("{label}<");
    let prefix = format!("{marker} ");
    let payload = peer_message_echo_payload(&message.text);
    let markdown = runtime_peer_message_presentation_is_markdown(Some(&message.content_type));
    let mut rendered_lines = if markdown {
        peer_message_markdown_rendered_lines(
            prefix.as_str(),
            payload.as_str(),
            ui_theme,
            table_width,
        )
    } else {
        peer_message_echo_rendered_lines(prefix.as_str(), payload.as_str(), frame_width, &label)
    };
    attach_agent_name_marker_span(
        &mut rendered_lines,
        marker.as_str(),
        agent_name_marker_rendition(if message.direct_parent {
            ui_theme.colors.agent_transcript_parent
        } else {
            ui_theme.colors.agent_transcript_peer_recipient
        }),
    );
    let copy_lines = if markdown {
        std::iter::once(message.text.trim_end_matches(['\r', '\n']).to_string())
            .chain(std::iter::repeat_n(
                AGENT_COPY_SKIP_LINE.to_string(),
                rendered_lines.len().saturating_sub(1),
            ))
            .collect()
    } else {
        rendered_lines
            .iter()
            .map(|line| {
                line.copy_text
                    .clone()
                    .unwrap_or_else(|| AGENT_COPY_SKIP_LINE.to_string())
            })
            .collect()
    };
    StreamingSayProjection {
        style: AgentTerminalPresentationStyle::UserPrompt,
        rendered_lines,
        copy_lines,
    }
}

/// Colored name marker prefixing the parent-supplied prompt in a subagent pane.
const AGENT_PARENT_PROMPT_NAME_MARKER: &str = "parent>";

/// Attaches one foreground-only name-marker span to the first rendered line.
///
/// The span covers only the bounded name and its direction glyph, so the echoed
/// payload keeps the terminal's default color and the marker adds no display
/// cells. Continuation lines stay unstyled, and the span is derived only from
/// the persisted direction and bounded label, so a live line and its replayed
/// line carry identical spans.
fn attach_agent_name_marker_span(
    rendered_lines: &mut [RichTextLine],
    marker: &str,
    rendition: GraphicRendition,
) {
    let Some(first) = rendered_lines.first_mut() else {
        return;
    };
    let length = UnicodeWidthStr::width(marker).min(UnicodeWidthStr::width(first.display.as_str()));
    if length == 0 {
        return;
    }
    // The marker starts at column zero and is prepended to the literal
    // plaintext or verbose raw-payload body, so it keeps its own span without
    // changing the payload's terminal styling.
    let mut spans = vec![TerminalStyleSpan {
        start: 0,
        length,
        rendition,
    }];
    spans.append(&mut first.style_spans);
    first.style_spans = spans;
}

/// Encodes one peer-message presentation source for geometry-aware replay.
fn peer_message_presentation_source(
    receive_identity: Option<&str>,
    peer_label: &str,
    payload: &str,
    content_type: Option<&str>,
    direct_parent: bool,
    presentation_eligible: bool,
) -> String {
    serde_json::json!({
        "direction": "received",
        "receive_identity": receive_identity,
        "peer": peer_label,
        "payload": payload,
        "content_type": content_type,
        "direct_parent": direct_parent,
        "presentation_eligible": presentation_eligible,
    })
    .to_string()
}

/// Encodes one accepted sender-side peer-message source for geometry-aware replay.
///
/// Sender records deliberately carry no receipt identity. Their direct-parent
/// fact is captured at acceptance so replay never reclassifies historical rows.
fn sent_peer_message_presentation_source(
    action_identity: &str,
    recipient_label: &str,
    payload: &str,
    content_type: &str,
    direct_parent: bool,
) -> String {
    serde_json::json!({
        "direction": "sent",
        "action_identity": action_identity,
        "peer": recipient_label,
        "payload": payload,
        "content_type": content_type,
        "direct_parent": direct_parent,
        "presentation_eligible": true,
    })
    .to_string()
}

/// Decodes one persisted peer-message source for geometry-aware replay.
///
/// The decoded peer name and payload stay unbounded: the renderer applies the
/// peer-context bound, so a live line and its replayed line are produced from
/// the same input by the same function and stay byte-identical even when the
/// payload exceeds that bound. Malformed, unknown-direction, or oversized
/// records return `None` so replay degrades to skipping that one line instead of
/// panicking or rendering from an untrusted stored length.
fn decoded_peer_message_presentation_source(
    source_text: &str,
) -> Option<PeerMessagePresentationSource> {
    if source_text.len() > AGENT_PRESENTATION_PEER_MESSAGE_SOURCE_MAX_BYTES {
        return None;
    }
    let encoded = serde_json::from_str::<PeerMessagePresentationSource>(source_text).ok()?;
    match encoded.direction.as_str() {
        "received" => Some(encoded),
        "sent"
            if encoded.receive_identity.is_none()
                && encoded
                    .action_identity
                    .as_deref()
                    .is_some_and(|identity| !identity.is_empty())
                && encoded.content_type.is_some()
                && encoded.presentation_eligible == Some(true) =>
        {
            Some(encoded)
        }
        _ => None,
    }
}

/// One media-type-specific projection of accumulated streamed `say` source.
struct StreamingSayProjection {
    style: AgentTerminalPresentationStyle,
    rendered_lines: Vec<RichTextLine>,
    copy_lines: Vec<String>,
}

/// Decodes one typed styled-line presentation record for geometry-aware replay.
fn styled_agent_presentation_source_lines(
    source_text: &str,
) -> Option<Vec<(AgentTerminalPresentationStyle, String)>> {
    let encoded = serde_json::from_str::<Vec<(String, String)>>(source_text).ok()?;
    (!encoded.is_empty()).then(|| {
        encoded
            .into_iter()
            .filter_map(|(style, text)| {
                AgentTerminalPresentationStyle::from_persistence_name(&style)
                    .map(|style| (style, text))
            })
            .collect()
    })
}

/// Decodes one structured macro lifecycle row for geometry-aware replay.
fn macro_lifecycle_presentation_source(
    source_text: &str,
) -> Option<(String, Option<usize>, usize, String, bool)> {
    serde_json::from_str(source_text).ok()
}

/// Runs one terminal presentation operation while containing parser panics.
///
/// A contained panic still becomes an explicit runtime error so callers do not
/// report a dropped presentation batch as successfully rendered.
fn catch_agent_terminal_presentation_panic(context: &str, operation: impl FnOnce()) -> Result<()> {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)).is_err() {
        return Err(MezError::invalid_state(format!(
            "agent terminal presentation feed panicked while {context}"
        )));
    }
    Ok(())
}

impl RuntimeSessionService {
    /// Returns the active conversation and layout size for one presentation target.
    pub(super) fn agent_presentation_target(&self, pane_id: &str) -> Result<(String, Size)> {
        let descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            )
        })?;
        let conversation_id = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .ok_or_else(|| {
                MezError::invalid_state("agent terminal presentation target session not found")
            })?;
        Ok((conversation_id, descriptor.size))
    }

    /// Ensures the agent destination is bound to the pane's active conversation.
    pub(crate) fn ensure_current_agent_presentation_screen(&mut self, pane_id: &str) -> Result<()> {
        if self.find_pane_descriptor(pane_id).is_none() {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent terminal presentation target pane not found",
            ));
        }
        if self.agent_shell_store().get(pane_id).is_none() {
            self.agent_shell_store_mut().ensure_session(pane_id)?;
            if let Err(error) = self.capture_agent_session_allowed_actions_for_pane(pane_id) {
                self.agent_shell_store_mut().remove_session(pane_id);
                return Err(error);
            }
        }
        let (conversation_id, size) = self.agent_presentation_target(pane_id)?;
        let replace = self
            .agent_pane_screen_state(pane_id)
            .is_none_or(|screen| screen.conversation_id() != conversation_id);
        if replace {
            self.presentation
                .agent_shell_output_previews
                .remove(pane_id);
            self.presentation
                .agent_streaming_say_presentations
                .remove(pane_id);
            self.presentation
                .agent_presentation_projection_cache
                .remove(pane_id);
            let screen = TerminalScreen::new_with_history_config(
                size,
                self.terminal_history_limit(),
                self.terminal_history_rotate_lines(),
            )?;
            self.set_agent_pane_screen(pane_id.to_string(), conversation_id, screen);
        }
        self.agent_pane_screen(pane_id)
            .map(|_| ())
            .ok_or_else(|| MezError::invalid_state("agent pane screen was not initialized"))
    }

    /// Validates that replay records belong to the pane's active conversation.
    fn validate_agent_presentation_replay_target(
        &self,
        pane_id: &str,
        entries: &[AgentPresentationEntry],
    ) -> Result<String> {
        let (conversation_id, _size) = self.agent_presentation_target(pane_id)?;
        if entries
            .iter()
            .any(|entry| entry.conversation_id != conversation_id)
        {
            return Err(MezError::invalid_state(
                "agent presentation replay target does not match the active conversation",
            ));
        }
        Ok(conversation_id)
    }

    /// Runs the append agent user prompt to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_user_prompt_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        prompt: &str,
    ) -> Result<()> {
        let display_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let rendered_lines = wrapped_prefixed_agent_terminal_lines("user> ", prompt, display_width);
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::UserPrompt,
            rendered_lines.as_slice(),
            &[],
            Some((prompt, AGENT_PRESENTATION_USER_PROMPT_CONTENT_TYPE)),
        )
    }

    /// Appends the parent-supplied prompt at the top of a spawned subagent pane.
    ///
    /// Subagent pane logs should expose the exact parent instruction that
    /// started the child turn so follow-up inspection does not require looking
    /// back through the parent pane.
    pub(crate) fn append_agent_parent_prompt_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        prompt: &str,
    ) -> Result<()> {
        let display_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let mut rendered_lines =
            wrapped_prefixed_agent_terminal_lines("parent> ", prompt, display_width);
        attach_agent_name_marker_span(
            &mut rendered_lines,
            AGENT_PARENT_PROMPT_NAME_MARKER,
            agent_name_marker_rendition(
                self.presentation
                    .settings
                    .ui_theme
                    .colors
                    .agent_transcript_parent,
            ),
        );
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::UserPrompt,
            rendered_lines.as_slice(),
            &[],
            Some((prompt, AGENT_PRESENTATION_PARENT_PROMPT_CONTENT_TYPE)),
        )
    }

    /// Appends one received peer message to the pane log in prompt style.
    ///
    /// This compatibility entry point is used by direct presentation tests.
    /// Receipt-owned delivery uses the identity-bearing variant so durable
    /// replay and persistence settlement can remain recipient scoped.
    #[cfg(test)]
    pub(crate) fn append_agent_received_peer_message_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        peer_label: &str,
        content_type: &str,
        payload: &str,
    ) -> Result<()> {
        self.append_agent_received_peer_message_to_terminal_buffer_with_receive_identity(
            pane_id,
            None,
            peer_label,
            content_type,
            payload,
        )
    }

    /// Appends one received peer message with an optional stable receive identity.
    #[cfg(test)]
    pub(crate) fn append_agent_received_peer_message_to_terminal_buffer_with_receive_identity(
        &mut self,
        pane_id: &str,
        receive_identity: Option<&str>,
        peer_label: &str,
        content_type: &str,
        payload: &str,
    ) -> Result<()> {
        self.append_agent_peer_message_to_terminal_buffer(
            pane_id,
            &PeerMessagePresentation {
                receive_identity,
                peer_label,
                content_type: Some(content_type),
                payload,
                direct_parent: false,
                presentation_eligible: false,
            },
        )
    }

    /// Appends a receiver receipt that was selected for presentation at commit time.
    pub(crate) fn append_committed_received_peer_message_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        receive_identity: &str,
        peer_label: &str,
        content_type: &str,
        payload: &str,
    ) -> Result<()> {
        self.append_agent_peer_message_to_terminal_buffer(
            pane_id,
            &PeerMessagePresentation {
                receive_identity: Some(receive_identity),
                peer_label,
                content_type: Some(content_type),
                payload,
                direct_parent: false,
                presentation_eligible: true,
            },
        )
    }

    /// Appends one received direct-parent MMP message with parent marker semantics.
    #[cfg(test)]
    pub(crate) fn append_agent_received_direct_parent_message_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        content_type: &str,
        payload: &str,
    ) -> Result<()> {
        self.append_agent_received_direct_parent_message_to_terminal_buffer_with_receive_identity(
            pane_id,
            None,
            content_type,
            payload,
        )
    }

    /// Appends one received direct-parent message with an optional stable receive identity.
    #[cfg(test)]
    pub(crate) fn append_agent_received_direct_parent_message_to_terminal_buffer_with_receive_identity(
        &mut self,
        pane_id: &str,
        receive_identity: Option<&str>,
        content_type: &str,
        payload: &str,
    ) -> Result<()> {
        self.append_agent_peer_message_to_terminal_buffer(
            pane_id,
            &PeerMessagePresentation {
                receive_identity,
                peer_label: "parent",
                content_type: Some(content_type),
                payload,
                direct_parent: true,
                presentation_eligible: false,
            },
        )
    }

    /// Appends a direct-parent receipt selected for presentation at commit time.
    pub(crate) fn append_committed_received_direct_parent_message_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        receive_identity: &str,
        content_type: &str,
        payload: &str,
    ) -> Result<()> {
        self.append_agent_peer_message_to_terminal_buffer(
            pane_id,
            &PeerMessagePresentation {
                receive_identity: Some(receive_identity),
                peer_label: "parent",
                content_type: Some(content_type),
                payload,
                direct_parent: true,
                presentation_eligible: true,
            },
        )
    }

    /// Resolves the pane peer-message echo mode from the effective config.
    ///
    /// Reading the effective configuration at echo time means a config reload
    /// takes effect without new plumbing. An absent, unreadable, or unknown value
    /// keeps the documented `normal` default.
    fn agent_peer_message_log_mode(&self) -> PeerMessageLogMode {
        runtime_effective_config_value(self.integration.config_layers())
            .map(|value| runtime_agent_peer_message_log_mode_from_config(&value))
            .unwrap_or(PeerMessageLogMode::Normal)
    }

    /// Renders one received peer-message log line at live or replay geometry.
    ///
    /// Both the live writer and the replay decoder hand it the same raw peer
    /// name and payload: the peer
    /// context bound is applied here and nowhere else, so the wrapping,
    /// continuation indentation, truncation marker, and persisted source shape
    /// stay identical for ordinary and direct-parent received traffic and for
    /// live and replayed lines.
    ///
    /// A payload the projection suppresses writes no row and no presentation
    /// record, so replay cannot resurrect a line the live pane never showed.
    /// The same rule covers the runtime bridge echo: normal mode returns before
    /// any row or record exists, while verbose mode logs the full bounded payload
    /// for every received sender and media type.
    fn append_agent_peer_message_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        presentation: &PeerMessagePresentation<'_>,
    ) -> Result<()> {
        let log_mode = self.agent_peer_message_log_mode();
        if !presentation.presentation_eligible
            && !runtime_peer_message_presentation_is_visible(log_mode, presentation.content_type)
        {
            return Ok(());
        }
        let body = peer_message_echo_payload(presentation.payload);
        let display_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let rendered_label = peer_message_echo_label(presentation.peer_label);
        let prefix = format!("{rendered_label}> ");
        let copy_group = presentation
            .receive_identity
            .unwrap_or(presentation.peer_label);
        let markdown = runtime_peer_message_presentation_is_markdown(presentation.content_type);
        let mut rendered_lines = if markdown {
            peer_message_markdown_rendered_lines(
                prefix.as_str(),
                body.as_str(),
                &self.presentation.settings.ui_theme,
                self.agent_terminal_markdown_terminal_width(pane_id)?,
            )
        } else {
            peer_message_echo_rendered_lines(
                prefix.as_str(),
                body.as_str(),
                display_width,
                copy_group,
            )
        };
        let marker_rendition = if presentation.direct_parent {
            self.presentation
                .settings
                .ui_theme
                .colors
                .agent_transcript_parent
        } else {
            self.presentation
                .settings
                .ui_theme
                .colors
                .agent_transcript_peer_sender
        };
        attach_agent_name_marker_span(
            &mut rendered_lines,
            format!("{rendered_label}>").as_str(),
            agent_name_marker_rendition(marker_rendition),
        );
        let source = peer_message_presentation_source(
            presentation.receive_identity,
            presentation.peer_label,
            presentation.payload,
            presentation.content_type,
            presentation.direct_parent,
            true,
        );
        let copy_lines = if markdown {
            std::iter::once(
                presentation
                    .payload
                    .trim_end_matches(['\r', '\n'])
                    .to_string(),
            )
            .chain(std::iter::repeat_n(
                AGENT_COPY_SKIP_LINE.to_string(),
                rendered_lines.len().saturating_sub(1),
            ))
            .collect::<Vec<_>>()
        } else {
            rendered_lines
                .iter()
                .map(|line| {
                    line.copy_text
                        .clone()
                        .unwrap_or_else(|| AGENT_COPY_SKIP_LINE.to_string())
                })
                .collect::<Vec<_>>()
        };
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::UserPrompt,
            rendered_lines.as_slice(),
            &copy_lines,
            Some((
                source.as_str(),
                AGENT_PRESENTATION_PEER_MESSAGE_CONTENT_TYPE,
            )),
        )
    }

    /// Queues one retained received-peer source for durable persistence without
    /// touching the live terminal screen.
    ///
    /// A receipt calls this only after its single live row was accepted and an
    /// earlier durable append failed. Reusing the retained label, media type,
    /// payload, and receipt identity keeps a retry source-identical while
    /// preventing a second pane row.
    pub(crate) fn persist_received_peer_message_presentation_only(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        turn_id: &str,
        presentation: &PeerMessagePresentation<'_>,
    ) -> Result<()> {
        let Some((active_conversation_id, ephemeral)) = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| (session.session_id.clone(), session.ephemeral))
        else {
            return Ok(());
        };
        if ephemeral || active_conversation_id != conversation_id {
            return Ok(());
        }
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Ok(());
        };
        let terminal_width = self
            .agent_presentation_terminal_width(pane_id)
            .ok_or_else(|| {
                MezError::invalid_state("peer presentation retry target has no terminal width")
            })?;
        let display_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let rendered_label = peer_message_echo_label(presentation.peer_label);
        let prefix = format!("{rendered_label}> ");
        let copy_group = presentation
            .receive_identity
            .unwrap_or(presentation.peer_label);
        let rendered_lines = peer_message_echo_rendered_lines(
            prefix.as_str(),
            peer_message_echo_payload(presentation.payload).as_str(),
            display_width,
            copy_group,
        );
        let source = peer_message_presentation_source(
            presentation.receive_identity,
            presentation.peer_label,
            presentation.payload,
            presentation.content_type,
            presentation.direct_parent,
            true,
        );
        let entry = AgentPresentationEntry {
            conversation_id: conversation_id.to_string(),
            sequence: 0,
            created_at_unix_seconds: current_unix_seconds().max(1),
            pane_id: pane_id.to_string(),
            turn_id: (!turn_id.is_empty()).then(|| turn_id.to_string()),
            terminal_width,
            style_names: vec![
                AgentTerminalPresentationStyle::UserPrompt
                    .persistence_name()
                    .to_string();
                rendered_lines.len()
            ],
            display_lines: rendered_lines
                .iter()
                .map(|line| line.display.clone())
                .collect(),
            copy_lines: Vec::new(),
            ansi_text: None,
            source_text: Some(source),
            source_content_type: Some(AGENT_PRESENTATION_PEER_MESSAGE_CONTENT_TYPE.to_string()),
        };
        if self.persistence.transcript_uses_adapter() {
            let path = store.presentation_path(conversation_id)?;
            self.persistence.queue_presentation(
                crate::runtime::RuntimeSideEffect::PersistPresentationEntries {
                    store,
                    path,
                    entries: vec![entry],
                },
            );
        } else {
            let mut entry = entry;
            entry.sequence = store.next_presentation_sequence(conversation_id)?;
            store.append_presentation(&entry)?;
        }
        Ok(())
    }

    /// Runs the append agent assistant text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_assistant_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        self.append_agent_assistant_content_to_terminal_buffer(
            pane_id,
            text,
            AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE,
        )
    }

    /// Appends assistant output using its declared presentation media type.
    pub(crate) fn append_agent_assistant_content_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
        content_type: &str,
    ) -> Result<()> {
        let frame_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let table_width = self.agent_terminal_markdown_terminal_width(pane_id)?;
        let projection = self.streaming_say_projection(
            &RuntimeStreamingSayAction {
                status: mez_agent::SayStatus::Final,
                content_type: content_type.to_string(),
                text: text.to_string(),
                complete: true,
            },
            frame_width,
            table_width,
        );
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            projection.style,
            &projection.rendered_lines,
            &projection.copy_lines,
            Some((text, content_type)),
        )
    }

    /// Persists accepted rationale without repainting its promoted rows. Static
    /// and streamed producers share the exact response-bound semantic envelope.
    fn persist_activity_rationale_projection(
        &mut self,
        pane_id: &str,
        execution: &mez_agent::AgentTurnExecution,
        rows: (String, Vec<String>, Vec<String>),
        text: &str,
    ) -> Result<()> {
        let encoded = self
            .activity_rationale_source(pane_id, execution, text)?
            .and_then(|source| source.encode().ok());
        let source =
            encoded
                .as_deref()
                .map_or((text, AGENT_PRESENTATION_THINKING_CONTENT_TYPE), |source| {
                    (
                        source,
                        crate::storage::transcript::activity::ACTIVITY_CONTENT_TYPE,
                    )
                });
        self.persist_agent_presentation_entry(
            pane_id,
            vec![rows.0; rows.1.len()],
            rows.1,
            rows.2,
            String::new(),
            Some(source),
        );
        Ok(())
    }

    /// Persists accepted action components without repainting promoted rows.
    /// Original media types and bounded source remain the replay authority.
    fn persist_activity_action_projection(
        &mut self,
        pane_id: &str,
        execution: &mez_agent::AgentTurnExecution,
        owner: (
            usize,
            crate::storage::transcript::activity::ActivityComponentKind,
        ),
        rows: (String, Vec<String>, Vec<String>),
        source: (&str, &str),
    ) -> Result<()> {
        let encoded = self
            .activity_action_source(pane_id, execution, owner.0, owner.1, source)?
            .and_then(|activity| activity.encode().ok());
        let source = encoded.as_deref().map_or(source, |source| {
            (
                source,
                crate::storage::transcript::activity::ACTIVITY_CONTENT_TYPE,
            )
        });
        self.persist_agent_presentation_entry(
            pane_id,
            vec![rows.0; rows.1.len()],
            rows.1,
            rows.2,
            String::new(),
            Some(source),
        );
        Ok(())
    }

    /// Persists one durable user-visible agent presentation entry.
    pub(super) fn persist_agent_presentation_entry(
        &mut self,
        pane_id: &str,
        style_names: Vec<String>,
        display_lines: Vec<String>,
        copy_lines: Vec<String>,
        ansi_text: String,
        source: Option<(&str, &str)>,
    ) {
        if self
            .presentation
            .agent_presentation_replay_panes
            .contains(pane_id)
            || display_lines.is_empty()
            || style_names.len() != display_lines.len()
        {
            return;
        }
        self.presentation
            .agent_presentation_projection_cache
            .remove(pane_id);
        let Some((conversation_id, running_turn_id, ephemeral)) =
            self.agent_shell_store().get(pane_id).map(|session| {
                (
                    session.session_id.clone(),
                    session.running_turn_id.clone(),
                    session.ephemeral,
                )
            })
        else {
            return;
        };
        if ephemeral {
            return;
        }
        self.presentation
            .invalidate_agent_presentation_replay_cache(&conversation_id);
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return;
        };
        let Some(terminal_width) = self.agent_presentation_terminal_width(pane_id) else {
            return;
        };
        let mut entry = AgentPresentationEntry {
            conversation_id,
            sequence: 0,
            created_at_unix_seconds: current_unix_seconds().max(1),
            pane_id: pane_id.to_string(),
            turn_id: running_turn_id,
            terminal_width,
            style_names,
            display_lines,
            copy_lines,
            ansi_text: (!ansi_text.is_empty()).then_some(ansi_text),
            source_text: source.map(|(text, _content_type)| text.to_string()),
            source_content_type: source.map(|(_text, content_type)| content_type.to_string()),
        };
        if entry.source_content_type.as_deref()
            == Some(crate::storage::transcript::activity::ACTIVITY_CONTENT_TYPE)
        {
            if let Ok(activity) = crate::storage::transcript::activity::ActivitySource::decode(
                entry.source_text.as_deref().unwrap_or_default(),
            ) {
                entry.turn_id = Some(activity.turn_id);
            } else {
                return;
            }
        }
        if self.persistence.transcript_uses_adapter() {
            let Ok(path) = store.presentation_path(&entry.conversation_id) else {
                return;
            };
            self.persistence.queue_presentation(
                crate::runtime::RuntimeSideEffect::PersistPresentationEntries {
                    store,
                    path,
                    entries: vec![entry],
                },
            );
        } else if let Ok(sequence) = store.next_presentation_sequence(&entry.conversation_id) {
            entry.sequence = sequence;
            let _ = store.append_presentation(&entry);
        }
    }

    /// Runs the append agent status text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_status_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        let lines = text
            .trim_end_matches(['\r', '\n'])
            .lines()
            .map(sanitized_agent_terminal_line)
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &lines,
            AgentTerminalPresentationStyle::Status,
        )
    }

    /// Appends one bounded sandbox-mapping warning to the retained agent log.
    ///
    /// Warning identities are deduplicated for the lifetime of the active pane
    /// environment. The warning is visible regardless of verbose-mode state.
    pub(crate) fn append_sandbox_mapping_warning_once(
        &mut self,
        pane_id: &str,
        warning_id: &str,
        detail: &str,
    ) -> Result<()> {
        let identity = format!(
            "{pane_id}\0{}\0{warning_id}",
            self.session.config_generation
        );
        if !self
            .process
            .sandbox_mapping_warnings_emitted
            .insert(identity)
        {
            return Ok(());
        }
        let detail = detail
            .chars()
            .filter(|character| !character.is_control())
            .take(512)
            .collect::<String>();
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!(
                "agent warning: sandbox omitted unavailable host mapping: {detail}. The sandbox remains active with reduced access."
            ),
        )
    }

    /// Runs the append agent verbose status text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_verbose_status_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        if self.agent_verbose_enabled(pane_id) {
            self.append_agent_status_text_to_terminal_buffer(pane_id, text)?;
        }
        Ok(())
    }

    /// Appends transient PTY diagnostics without granting terminal controls
    /// authority over the retained agent transcript surface.
    pub(crate) fn append_agent_pty_diagnostic_bytes_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        bytes: &[u8],
    ) -> Result<()> {
        let source_lines = String::from_utf8_lossy(bytes)
            .trim_end_matches(['\r', '\n'])
            .lines()
            .map(sanitized_agent_terminal_line)
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        if source_lines.is_empty() {
            return Ok(());
        }
        let content_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let lines = source_lines
            .into_iter()
            .flat_map(|line| {
                wrap_rich_text_line_to_width_with_source_ranges_hard(
                    RichTextLine {
                        display: line,
                        style_spans: Vec::new(),
                        copy_text: None,
                        kind: RichTextLineKind::Normal,
                    },
                    content_width,
                )
                .into_iter()
                .map(|wrapped| wrapped.line.display)
            })
            .collect::<Vec<_>>();
        self.ensure_current_agent_presentation_screen(pane_id)?;
        self.retire_agent_streaming_say_before_pane_write(pane_id)?;
        let ui_theme = self.presentation.settings.ui_theme.clone();
        let (conversation_id, mut screen, preview_presentation) =
            self.agent_shell_preview_write_base(pane_id)?;
        let mut rendered = String::new();
        let cursor = screen.cursor_state();
        let current_line_has_content = screen
            .visible_lines()
            .get(cursor.row)
            .is_some_and(|line| !line.trim().is_empty());
        if cursor.column == 0 && !current_line_has_content {
            rendered.push('\r');
        } else {
            rendered.push_str("\r\n");
        }
        for line in lines {
            append_styled_agent_terminal_line(
                &mut rendered,
                AgentTerminalPresentationStyle::Status,
                &line,
                &ui_theme,
            );
            rendered.push_str("\x1b[0m\r\n");
        }
        Self::feed_agent_terminal_screen(
            &mut screen,
            rendered.as_bytes(),
            "appending transient agent PTY diagnostics",
        )?;
        self.install_agent_shell_preview_write(
            pane_id,
            &conversation_id,
            screen,
            preview_presentation,
        )
    }

    /// Runs the append agent thinking text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_thinking_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        self.append_agent_thinking_with_activity(pane_id, text, None)
    }

    /// Uses the existing rationale renderer with explicit accepted component
    /// identity. Missing identity and replay retain the legacy source contract.
    pub(in crate::runtime) fn append_agent_thinking_with_activity(
        &mut self,
        pane_id: &str,
        text: &str,
        activity: Option<crate::storage::transcript::activity::ActivitySource>,
    ) -> Result<()> {
        if self.agent_thinking_enabled(pane_id) {
            let content_width = self.agent_terminal_markdown_frame_width(pane_id)?;
            let rendition = agent_terminal_label_rendition(
                AgentTerminalPresentationStyle::Status,
                &self.presentation.settings.ui_theme,
            );
            let rendered_lines = agent_thinking_display_lines_for_width(text, content_width)
                .into_iter()
                .map(|display| {
                    let length = UnicodeWidthStr::width(display.as_str());
                    RichTextLine {
                        display,
                        style_spans: vec![TerminalStyleSpan {
                            start: 0,
                            length,
                            rendition,
                        }],
                        copy_text: None,
                        kind: mez_mux::render::RichTextLineKind::Normal,
                    }
                })
                .collect::<Vec<_>>();
            let encoded = activity.and_then(|mut activity| {
                activity.content_type = AGENT_PRESENTATION_THINKING_CONTENT_TYPE.to_string();
                activity.source = text.to_string();
                activity.preview_source = None;
                activity.encode().ok()
            });
            self.append_agent_terminal_rendered_lines_to_buffer(
                pane_id,
                AgentTerminalPresentationStyle::Status,
                &rendered_lines,
                &[],
                Some(encoded.as_deref().map_or(
                    (text, AGENT_PRESENTATION_THINKING_CONTENT_TYPE),
                    |source| {
                        (
                            source,
                            crate::storage::transcript::activity::ACTIVITY_CONTENT_TYPE,
                        )
                    },
                )),
            )?;
        }
        Ok(())
    }

    /// Appends one structured macro lifecycle transition in the parent pane.
    pub(crate) fn append_agent_macro_status_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        macro_name: &str,
        step_index: Option<usize>,
        total_steps: usize,
        status: &str,
    ) -> Result<()> {
        let content_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let rendered_lines = agent_macro_lifecycle_display_lines_for_width(
            macro_name,
            step_index,
            total_steps,
            status,
            content_width,
        )
        .into_iter()
        .map(|display| RichTextLine {
            display,
            style_spans: Vec::new(),
            copy_text: None,
            kind: mez_mux::render::RichTextLineKind::Normal,
        })
        .collect::<Vec<_>>();
        let source = serde_json::to_string(&(macro_name, step_index, total_steps, status, false))
            .map_err(|error| {
            MezError::invalid_state(format!("macro lifecycle source encoding failed: {error}"))
        })?;
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Status,
            &rendered_lines,
            &[],
            Some((&source, AGENT_PRESENTATION_MACRO_LIFECYCLE_CONTENT_TYPE)),
        )
    }

    /// Appends one failed macro lifecycle transition in the parent pane.
    pub(crate) fn append_agent_macro_error_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        macro_name: &str,
        step_index: usize,
        total_steps: usize,
        status: &str,
    ) -> Result<()> {
        let content_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let rendered_lines = agent_macro_lifecycle_display_lines_for_width(
            macro_name,
            Some(step_index),
            total_steps,
            status,
            content_width,
        )
        .into_iter()
        .map(|display| RichTextLine {
            display,
            style_spans: Vec::new(),
            copy_text: None,
            kind: mez_mux::render::RichTextLineKind::Normal,
        })
        .collect::<Vec<_>>();
        let source =
            serde_json::to_string(&(macro_name, Some(step_index), total_steps, status, true))
                .map_err(|error| {
                    MezError::invalid_state(format!(
                        "macro lifecycle source encoding failed: {error}"
                    ))
                })?;
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Error,
            &rendered_lines,
            &[],
            Some((&source, AGENT_PRESENTATION_MACRO_LIFECYCLE_CONTENT_TYPE)),
        )
    }

    /// Runs the append agent error text to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_error_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        let lines = text
            .trim_end_matches(['\r', '\n'])
            .lines()
            .map(sanitized_agent_terminal_line)
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        self.append_agent_terminal_lines_to_buffer(
            pane_id,
            &lines,
            AgentTerminalPresentationStyle::Error,
        )
    }

    /// Runs the append agent command preview to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_command_preview_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        command: &str,
    ) -> Result<()> {
        self.append_agent_command_preview_source_to_terminal_buffer(pane_id, command, false)
    }

    /// Appends one bounded command source with replay-supplied omission state.
    fn append_agent_command_preview_source_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        command: &str,
        source_was_truncated: bool,
    ) -> Result<()> {
        self.append_agent_command_preview_with_activity(
            pane_id,
            command,
            source_was_truncated,
            None,
        )
    }

    /// Renders command intent with explicit producer identity when available.
    /// The envelope uses the same bounded source and omission renderer as replay.
    pub(in crate::runtime) fn append_agent_command_preview_with_activity(
        &mut self,
        pane_id: &str,
        command: &str,
        source_was_truncated: bool,
        activity: Option<crate::storage::transcript::activity::ActivitySource>,
    ) -> Result<()> {
        /// Defines the MAX AGENT COMMAND PREVIEW LINES const used by this subsystem.
        ///
        /// Keeping this value documented makes the contract explicit at the module
        /// boundary and avoids relying on call-site inference.
        const MAX_AGENT_COMMAND_PREVIEW_LINES: usize = 10;
        let columns = self
            .agent_pane_screen(pane_id)
            .map(|screen| usize::from(screen.size().columns))
            .or_else(|| {
                self.find_pane_descriptor(pane_id)
                    .map(|descriptor| usize::from(descriptor.size.columns))
            })
            .unwrap_or(80);
        let display_columns = bounded_agent_terminal_presentation_columns(
            columns,
            self.presentation.settings.terminal_agent_wrap_column_cap,
        );
        let prefix_width =
            UnicodeWidthStr::width(AGENT_TERMINAL_MESSAGE_PREFIX) + UnicodeWidthStr::width("$ ");
        let content_columns = display_columns.saturating_sub(prefix_width).max(1);
        let mut source = bounded_command_preview_source(command);
        source.truncated |= source_was_truncated;
        let rendered_lines = command_preview_terminal_rendered_lines(
            &source.text,
            source.truncated,
            content_columns,
            MAX_AGENT_COMMAND_PREVIEW_LINES,
            self.shell_classification_for_pane(pane_id),
            &self.presentation.settings.ui_theme,
        );
        let copy_lines = rendered_lines
            .iter()
            .map(|line| line.display.clone())
            .collect::<Vec<_>>();
        let content_type = if source.truncated {
            AGENT_PRESENTATION_TRUNCATED_COMMAND_PREVIEW_CONTENT_TYPE
        } else {
            AGENT_PRESENTATION_COMMAND_PREVIEW_CONTENT_TYPE
        };
        let encoded = activity.and_then(|mut activity| {
            activity.content_type = content_type.to_string();
            activity.source = source.text.clone();
            activity.preview_source = None;
            activity.encode().ok()
        });
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Command,
            &rendered_lines,
            &copy_lines,
            Some(
                encoded
                    .as_deref()
                    .map_or((&source.text, content_type), |encoded| {
                        (
                            encoded,
                            crate::storage::transcript::activity::ACTIVITY_CONTENT_TYPE,
                        )
                    }),
            ),
        )
    }

    /// Runs the append agent terminal lines to buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn append_agent_terminal_lines_to_buffer(
        &mut self,
        pane_id: &str,
        lines: &[String],
        style: AgentTerminalPresentationStyle,
    ) -> Result<()> {
        let styled_lines = lines
            .iter()
            .map(|line| (style, line.clone()))
            .collect::<Vec<_>>();
        self.append_agent_terminal_styled_lines_to_buffer(pane_id, &styled_lines)
    }

    /// Feeds agent-owned presentation bytes into a terminal screen.
    ///
    /// Agent presentation content is model-authored, so terminal rendering must
    /// contain parser defects to the presentation batch instead of allowing a
    /// panic to cross the runtime state boundary.
    ///
    /// # Parameters
    /// - `screen`: The pane screen receiving rendered bytes.
    /// - `bytes`: The already-sanitized terminal bytes to feed.
    /// - `context`: A short description of the presentation operation.
    pub(super) fn feed_agent_terminal_screen(
        screen: &mut TerminalScreen,
        bytes: &[u8],
        context: &str,
    ) -> Result<()> {
        screen.set_wrap_continuation_prefix(AGENT_TERMINAL_MESSAGE_PREFIX);
        catch_agent_terminal_presentation_panic(context, || {
            screen.feed(bytes);
        })
    }

    /// Retires provisional provider output before an unrelated pane write.
    ///
    /// Streaming owns only the exact screen generation it installed. Ordinary
    /// status, prompt, and completion writes remove that provisional generation
    /// first so later worker projections cannot replace the unrelated output.
    fn retire_agent_streaming_say_before_pane_write(&mut self, pane_id: &str) -> Result<()> {
        if self
            .presentation
            .agent_streaming_say_presentations
            .contains_key(pane_id)
        {
            self.discard_agent_streaming_say_presentation(pane_id, None)?;
        }
        if let Some(preview) = self
            .presentation
            .agent_pending_final_say_previews
            .remove(pane_id)
            && self.agent_pane_screen_lineage(pane_id, &preview.conversation_id)
                == Some(preview.installed_lineage)
        {
            self.update_agent_streaming_screen(
                pane_id,
                &preview.conversation_id,
                preview.without_final_screen.as_ref().clone(),
            )?;
        }
        Ok(())
    }

    /// Appends agent terminal lines with per-line presentation styles.
    ///
    /// Diff previews need additions, deletions, headers, and context to carry
    /// different colors while still flowing through the same pane-buffer gutter
    /// logic as normal agent transcript entries.
    pub(crate) fn append_agent_terminal_styled_lines_to_buffer(
        &mut self,
        pane_id: &str,
        styled_lines: &[(AgentTerminalPresentationStyle, String)],
    ) -> Result<()> {
        self.append_agent_terminal_styled_lines_with_source(pane_id, styled_lines, None)
    }

    /// Appends ordinary styled rows with an explicit identity-bearing source
    /// when supplied by their producer. Replay and legacy rows use the same UI.
    pub(in crate::runtime) fn append_agent_terminal_styled_lines_with_source(
        &mut self,
        pane_id: &str,
        styled_lines: &[(AgentTerminalPresentationStyle, String)],
        source_override: Option<(&str, &str)>,
    ) -> Result<()> {
        if styled_lines.is_empty() {
            return Ok(());
        }
        let content_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let mut wrapped_styled_lines = Vec::new();
        let mut wrapped_copy_lines = Vec::new();
        for (style, line) in styled_lines {
            let wrapped = wrap_rich_text_line_to_width_with_source_ranges_hard(
                RichTextLine {
                    display: line.clone(),
                    style_spans: Vec::new(),
                    copy_text: None,
                    kind: RichTextLineKind::Normal,
                },
                content_width,
            );
            wrapped_copy_lines.extend((0..wrapped.len()).map(|index| {
                if index == 0 {
                    line.clone()
                } else {
                    AGENT_COPY_SKIP_LINE.to_string()
                }
            }));
            wrapped_styled_lines.extend(
                wrapped
                    .into_iter()
                    .map(|wrapped| (*style, wrapped.line.display)),
            );
        }
        self.ensure_current_agent_presentation_screen(pane_id)?;
        self.retire_agent_streaming_say_before_pane_write(pane_id)?;
        let ui_theme = self.presentation.settings.ui_theme.clone();
        let (conversation_id, mut screen, preview_presentation) =
            self.agent_shell_preview_write_base(pane_id)?;
        let ansi_text = {
            let mut bytes = String::new();
            let cursor = screen.cursor_state();
            let current_line_has_content = screen
                .visible_lines()
                .get(cursor.row)
                .is_some_and(|line| !line.trim().is_empty());
            if cursor.column == 0 && !current_line_has_content {
                bytes.push('\r');
            } else {
                bytes.push_str("\r\n");
            }
            for (style, line) in &wrapped_styled_lines {
                append_styled_agent_terminal_line(&mut bytes, *style, line, &ui_theme);
                bytes.push_str("\x1b[0m\r\n");
            }
            Self::feed_agent_terminal_screen(
                &mut screen,
                bytes.as_bytes(),
                "appending styled agent lines",
            )?;
            screen.set_recent_normal_copy_texts(&wrapped_copy_lines, AGENT_COPY_SKIP_LINE);
            bytes
        };
        self.install_agent_shell_preview_write(
            pane_id,
            &conversation_id,
            screen,
            preview_presentation,
        )?;
        self.persist_agent_presentation_entry(
            pane_id,
            wrapped_styled_lines
                .iter()
                .map(|(style, _line)| style.persistence_name().to_string())
                .collect(),
            wrapped_styled_lines
                .iter()
                .map(|(_style, line)| line.clone())
                .collect(),
            wrapped_copy_lines,
            ansi_text,
            source_override.or(serde_json::to_string(
                &styled_lines
                    .iter()
                    .map(|(style, line)| (style.persistence_name(), line))
                    .collect::<Vec<_>>(),
            )
            .ok()
            .as_deref()
            .map(|source| (source, AGENT_PRESENTATION_STYLED_LINES_CONTENT_TYPE))),
        );
        Ok(())
    }

    /// Appends cap-aware rich pane-log rows while retaining style spans.
    ///
    /// This path is intentionally separate from general rendered transcript
    /// output: Markdown tables, diagrams, diffs, and command previews own
    /// specialized wrapping policies that the pane-log hard cap must not replace.
    fn append_agent_terminal_log_rendered_lines_to_buffer(
        &mut self,
        pane_id: &str,
        style: AgentTerminalPresentationStyle,
        rendered_lines: &[RichTextLine],
        source: Option<(&str, &str)>,
    ) -> Result<()> {
        let content_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let wrapped_lines = rendered_lines
            .iter()
            .cloned()
            .flat_map(|line| {
                wrap_rich_text_line_to_width_with_source_ranges_hard(line, content_width)
                    .into_iter()
                    .map(|wrapped| wrapped.line)
            })
            .collect::<Vec<_>>();
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            style,
            &wrapped_lines,
            &[],
            source,
        )
    }

    /// Appends transformed assistant display lines while preserving raw copy text.
    fn append_agent_terminal_rendered_lines_to_buffer(
        &mut self,
        pane_id: &str,
        style: AgentTerminalPresentationStyle,
        rendered_lines: &[RichTextLine],
        copy_lines: &[String],
        source: Option<(&str, &str)>,
    ) -> Result<()> {
        if rendered_lines.is_empty() {
            return Ok(());
        }
        self.ensure_current_agent_presentation_screen(pane_id)?;
        self.retire_agent_streaming_say_before_pane_write(pane_id)?;
        let ui_theme = self.presentation.settings.ui_theme.clone();
        let (conversation_id, mut screen, preview_presentation) =
            self.agent_shell_preview_write_base(pane_id)?;
        let ansi_text = {
            let mut bytes = String::new();
            let cursor = screen.cursor_state();
            let current_line_has_content = screen
                .visible_lines()
                .get(cursor.row)
                .is_some_and(|line| !line.trim().is_empty());
            if cursor.column == 0 && !current_line_has_content {
                bytes.push('\r');
            } else {
                bytes.push_str("\r\n");
            }
            for line in rendered_lines {
                append_styled_agent_terminal_rendered_line(&mut bytes, style, line, &ui_theme);
                bytes.push_str("\x1b[0m\r\n");
            }
            Self::feed_agent_terminal_screen(
                &mut screen,
                bytes.as_bytes(),
                "appending rendered agent lines",
            )?;
            screen.set_recent_normal_copy_texts(copy_lines, AGENT_COPY_SKIP_LINE);
            bytes
        };
        self.install_agent_shell_preview_write(
            pane_id,
            &conversation_id,
            screen,
            preview_presentation,
        )?;
        self.persist_agent_presentation_entry(
            pane_id,
            vec![style.persistence_name().to_string(); rendered_lines.len()],
            rendered_lines
                .iter()
                .map(|line| line.display.clone())
                .collect(),
            copy_lines.to_vec(),
            ansi_text,
            source,
        );
        Ok(())
    }

    /// Returns the provider-only base and current composite for a new stream.
    ///
    /// Active shell previews remain independently owned above the provider base.
    /// Stale preview lineage is discarded without changing the current pane.
    fn agent_streaming_base_screen(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
    ) -> Result<TerminalScreen> {
        let current_screen = self.agent_pane_screen(pane_id).cloned().ok_or_else(|| {
            MezError::invalid_state("streaming presentation screen was not initialized")
        })?;
        let current_lineage = self
            .agent_pane_screen_lineage(pane_id, conversation_id)
            .ok_or_else(|| {
                MezError::invalid_state("streaming presentation lineage was not initialized")
            })?;
        let preview = self
            .presentation
            .agent_shell_output_previews
            .get(pane_id)
            .cloned();
        if let Some(preview) = preview.as_ref()
            && preview.conversation_id == conversation_id
            && preview.installed_lineage == current_lineage
        {
            return Ok(preview.baseline_screen.as_ref().clone());
        }
        if preview.is_some() {
            self.presentation
                .agent_shell_output_previews
                .remove(pane_id);
        }
        Ok(current_screen)
    }

    /// Appends one existing styled prefix for a provisional plain-text source.
    fn append_agent_streaming_plain_started(
        &mut self,
        pane_id: &str,
        style: AgentTerminalPresentationStyle,
        prefix: &str,
        context: &str,
    ) -> Result<()> {
        let presentation = self
            .presentation
            .agent_streaming_say_presentations
            .get(pane_id)
            .ok_or_else(|| MezError::invalid_state("streaming presentation is unavailable"))?;
        let conversation_id = presentation.conversation_id.clone();
        let mut candidate = presentation.provider_screen.as_ref().clone();
        let cursor = candidate.cursor_state();
        let current_line_has_content = candidate
            .visible_lines()
            .get(cursor.row)
            .is_some_and(|line| !line.trim().is_empty());
        let mut bytes = if cursor.column == 0 && !current_line_has_content {
            "\r".to_string()
        } else {
            "\r\n".to_string()
        };
        append_styled_agent_terminal_line(
            &mut bytes,
            style,
            prefix,
            &self.presentation.settings.ui_theme,
        );
        Self::feed_agent_terminal_screen(&mut candidate, bytes.as_bytes(), context)?;
        self.update_agent_streaming_screen(pane_id, &conversation_id, candidate)
            .map(|_| ())
    }

    /// Atomically appends the literal assistant label for a newly started action.
    fn append_agent_streaming_say_started(&mut self, pane_id: &str) -> Result<()> {
        let presentation = self
            .presentation
            .agent_streaming_say_presentations
            .get(pane_id)
            .ok_or_else(|| MezError::invalid_state("streaming say presentation is unavailable"))?;
        let conversation_id = presentation.conversation_id.clone();
        let mut candidate = presentation.provider_screen.as_ref().clone();
        let cursor = candidate.cursor_state();
        let current_line_has_content = candidate
            .visible_lines()
            .get(cursor.row)
            .is_some_and(|line| !line.trim().is_empty());
        let mut bytes = if cursor.column == 0 && !current_line_has_content {
            "\r".to_string()
        } else {
            "\r\n".to_string()
        };
        append_styled_agent_terminal_line(
            &mut bytes,
            AgentTerminalPresentationStyle::Assistant,
            "mez> ",
            &self.presentation.settings.ui_theme,
        );
        Self::feed_agent_terminal_screen(
            &mut candidate,
            bytes.as_bytes(),
            "starting streaming say literal source",
        )?;
        self.update_agent_streaming_screen(pane_id, &conversation_id, candidate)
            .map(|_| ())
    }
}

/// Validated response settlement, isolated from ordinary pane writes.
/// The service remains the sole state owner; exact source and lineage checks
/// gate promotion, rollback, and durable action-order publication.
mod settlement;

impl RuntimeSessionService {
    /// Clears validated-promotion bookkeeping after deferred presentation settles.
    pub(crate) fn clear_promoted_agent_streaming_say_actions(
        &mut self,
        pane_id: &str,
        turn_id: &str,
    ) {
        self.presentation
            .agent_promoted_streaming_say_actions
            .remove(&(pane_id.to_string(), turn_id.to_string()));
    }

    /// Clears accepted sender-row identities owned by one terminal turn.
    ///
    /// The identity only prevents duplicate live appends while a turn can be
    /// retried or resumed. Terminal turn cleanup releases it because durable
    /// sender replay is owned by the later persistence phase.
    pub(crate) fn clear_settled_outbound_message_actions(&mut self, turn_id: &str) {
        self.presentation
            .agent_settled_outbound_message_actions
            .retain(|(_pane_id, candidate_turn_id, _action_id)| candidate_turn_id != turn_id);
    }

    /// Appends model-authored action summary text as normal-mode thinking logs.
    pub(crate) fn append_agent_action_model_thinking_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        action: &AgentAction,
        execution: Option<&mez_agent::AgentTurnExecution>,
    ) -> Result<bool> {
        let thinking_lines = agent_action_model_thinking_lines(action);
        if thinking_lines.is_empty() {
            return Ok(false);
        }
        let text = thinking_lines.join("\n");
        let activity = if let Some(execution) = execution {
            if let Some(ordinal) = execution.response.action_batch.as_ref().and_then(|batch| {
                batch
                    .actions
                    .iter()
                    .position(|candidate| candidate == action)
            }) {
                self.activity_action_source(
                    pane_id,
                    execution,
                    ordinal,
                    crate::storage::transcript::activity::ActivityComponentKind::Summary,
                    (&text, AGENT_PRESENTATION_THINKING_CONTENT_TYPE),
                )?
            } else {
                None
            }
        } else {
            None
        };
        self.append_agent_thinking_with_activity(pane_id, &text, activity)?;
        Ok(true)
    }

    /// Appends a sanitized mutating-action diff preview to the pane buffer.
    ///
    /// The source text is the cleaned shell observation captured from the hidden
    /// transaction, so this path never exposes shell prompts or Mezzanine wrapper
    /// traffic while still giving users a copyable summary of filesystem changes.
    pub(crate) fn append_agent_diff_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        text: &str,
    ) -> Result<()> {
        let display_width = self.agent_terminal_markdown_frame_width(pane_id)?;
        let rendered_lines = readable_agent_diff_display_lines_for_width(
            text,
            &self.presentation.settings.ui_theme,
            display_width,
        );
        self.append_agent_terminal_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::DiffContext,
            &rendered_lines,
            &[],
            Some((text, "text/x-diff; charset=utf-8")),
        )
    }

    /// Records successful patch diffs for `/list-modified-files`.
    ///
    /// The source text is the same cleaned shell observation used for the
    /// normal diff preview, so counts are derived from the semantic patch diff
    /// rather than from shell echo or wrapper traffic.
    pub(crate) fn record_agent_modified_files_from_diff(&mut self, pane_id: &str, text: &str) {
        let source_lines = cleaned_agent_diff_source_lines(text);
        for section in parse_unified_diff_sections(&source_lines) {
            let path = diff_section_path(&section).to_string();
            if path.is_empty() || path == "/dev/null" {
                continue;
            }
            let added = section
                .lines
                .iter()
                .filter(|line| line.marker == '+')
                .count();
            let removed = section
                .lines
                .iter()
                .filter(|line| line.marker == '-')
                .count();
            self.record_agent_modified_file_delta(pane_id, path, added, removed);
        }
    }

    /// Appends a single human-readable action execution line to the pane.
    ///
    /// Semantic file/search and runtime URL actions should be legible in normal
    /// mode without dumping generated commands or result payloads. The line
    /// uses span-level styling so the action remains salient without forcing
    /// arguments to inherit the same visual weight.
    pub(crate) fn append_agent_action_execution_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        action: &AgentAction,
    ) -> Result<bool> {
        let Some(header) = agent_action_execution_display_header(action) else {
            return Ok(false);
        };
        self.append_agent_action_execution_header_to_terminal_buffer(pane_id, action, &header)?;
        Ok(true)
    }

    /// Appends one action execution row using a runtime-selected header.
    ///
    /// Multi-transaction actions use this entry point when the active
    /// transaction has a more precise display target than the model action.
    pub(crate) fn append_agent_action_execution_header_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        action: &AgentAction,
        header: &str,
    ) -> Result<()> {
        // Only the execution owner can settle a provider-projected header.
        // Matching the turn, action id and final display text prevents a
        // provisional preview from becoming execution evidence by itself.
        let accepted_key = self
            .agent_shell_store()
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_ref())
            .map(|turn_id| (pane_id.to_string(), turn_id.clone(), action.id.clone()));
        if let Some(key) = accepted_key
            && self
                .presentation
                .agent_accepted_streaming_headers
                .get(&key)
                .is_some_and(|accepted| accepted == header)
        {
            self.presentation
                .agent_accepted_streaming_headers
                .remove(&key);
            return Ok(());
        }
        let thinking_lines = agent_action_model_thinking_lines(action);
        if !thinking_lines.is_empty() && self.agent_thinking_enabled(pane_id) {
            self.append_agent_thinking_text_to_terminal_buffer(
                pane_id,
                &thinking_lines.join("\n"),
            )?;
        }
        let rendered_line =
            agent_action_execution_rendered_line(header, &self.presentation.settings.ui_theme);
        self.append_agent_terminal_log_rendered_lines_to_buffer(
            pane_id,
            AgentTerminalPresentationStyle::Status,
            &[rendered_line],
            Some((header, AGENT_PRESENTATION_ACTION_HEADER_CONTENT_TYPE)),
        )?;
        Ok(())
    }

    /// Appends a bounded, human-readable action result preview to the pane.
    ///
    /// Normal mode uses this renderer for mutating semantic action diffs. Other
    /// result previews remain reserved for elevated log levels.
    pub(crate) fn append_agent_action_result_text_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        action: &AgentAction,
        result: &ActionResult,
        text: &str,
    ) -> Result<()> {
        if agent_action_result_uses_diff_preview(action) {
            return self.append_agent_diff_text_to_terminal_buffer(pane_id, text);
        }
        if result.is_error {
            return Ok(());
        }
        let Some(header) = agent_action_result_display_header(action) else {
            return Ok(());
        };
        let mut styled_lines = vec![(AgentTerminalPresentationStyle::Command, header)];
        styled_lines.extend(
            bounded_agent_action_result_display_lines(text)
                .into_iter()
                .map(|line| (AgentTerminalPresentationStyle::Status, line)),
        );
        self.append_agent_terminal_styled_lines_to_buffer(pane_id, &styled_lines)
    }

    /// Records ordered settled result identity from its existing response owner,
    /// not from display headers or adjacency. Retained source is presentation
    /// data only; display bounds never replace the full accepted result source.
    pub(crate) fn append_ordered_activity_result(
        &mut self,
        pane_id: &str,
        owner: (&str, usize, Option<&str>),
        action: &AgentAction,
        result: &ActionResult,
        text: &str,
        intent: crate::storage::transcript::activity::ActivityIntent,
    ) -> Result<()> {
        use crate::storage::transcript::activity::{
            ACTIVITY_CONTENT_TYPE, ActivityComponentKind, ActivitySource,
        };
        let (response_id, ordinal, transaction) = owner;
        let Some(session) = self.agent_shell_store().get(pane_id) else {
            return Ok(());
        };
        let diff = agent_action_result_uses_diff_preview(action);
        if result.action_id != action.id || !result.is_terminal() {
            return Err(MezError::invalid_state(
                "activity result has mismatched or nonterminal action ownership",
            ));
        }
        if result.is_error && !diff {
            return self
                .append_agent_action_result_text_to_terminal_buffer(pane_id, action, result, text);
        }
        let mut preview_lines = Vec::new();
        if !diff {
            let Some(header) = agent_action_result_display_header(action) else {
                return Ok(());
            };
            preview_lines.push((AgentTerminalPresentationStyle::Command, header));
            preview_lines.extend(
                bounded_agent_action_result_display_lines(text)
                    .into_iter()
                    .map(|line| (AgentTerminalPresentationStyle::Status, line)),
            );
        }
        let preview_source = if diff {
            None
        } else {
            Some(
                serde_json::to_string(
                    &preview_lines
                        .iter()
                        .map(|(style, line)| (style.persistence_name(), line))
                        .collect::<Vec<_>>(),
                )
                .map_err(|error| {
                    MezError::invalid_args(format!("activity preview encoding failed: {error}"))
                })?,
            )
        };
        let source = if diff {
            text.to_string()
        } else {
            serde_json::to_string(&vec![(
                AgentTerminalPresentationStyle::Status.persistence_name(),
                text,
            )])
            .map_err(|error| {
                MezError::invalid_args(format!("activity styled source encoding failed: {error}"))
            })?
        };
        let activity = ActivitySource {
            version: 1,
            conversation_id: session.session_id.clone(),
            turn_id: result.turn_id.clone(),
            response_id: response_id.to_string(),
            action_id: Some(action.id.clone()),
            action_ordinal: Some(ordinal),
            transaction: transaction.map(str::to_string),
            kind: ActivityComponentKind::Result,
            status: format!("{:?}", result.status).to_ascii_lowercase(),
            content_type: if diff {
                "text/x-diff; charset=utf-8"
            } else {
                AGENT_PRESENTATION_STYLED_LINES_CONTENT_TYPE
            }
            .to_string(),
            source,
            preview_source,
            intent,
        };
        // Results beyond presentation retention keep the preexisting bounded
        // preview; never fail an already settled action to add optional UI data.
        let encoded = match activity.encode() {
            Ok(encoded) => encoded,
            Err(_) => {
                return self.append_agent_action_result_text_to_terminal_buffer(
                    pane_id, action, result, text,
                );
            }
        };
        if diff {
            let width = self.agent_terminal_markdown_frame_width(pane_id)?;
            let lines = readable_agent_diff_display_lines_for_width(
                text,
                &self.presentation.settings.ui_theme,
                width,
            );
            return self.append_agent_terminal_rendered_lines_to_buffer(
                pane_id,
                AgentTerminalPresentationStyle::DiffContext,
                &lines,
                &[],
                Some((&encoded, ACTIVITY_CONTENT_TYPE)),
            );
        }
        self.append_agent_terminal_styled_lines_with_source(
            pane_id,
            &preview_lines,
            Some((&encoded, ACTIVITY_CONTENT_TYPE)),
        )
    }

    /// Returns whether a cleaned action result preview should render in normal
    /// logging mode.
    pub(crate) fn agent_action_result_renders_in_normal_mode(&self, action: &AgentAction) -> bool {
        agent_action_result_uses_diff_preview(action)
    }

    /// Runs the agent verbose enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn agent_verbose_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_verbose_status())
    }

    /// Runs the agent thinking enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn agent_thinking_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_thinking())
    }

    /// Runs the agent debug enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn agent_debug_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_debug())
    }

    /// Runs the agent trace enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn agent_trace_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_trace())
    }

    /// Runs the agent shell view enabled operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn agent_shell_view_enabled(&self, pane_id: &str) -> bool {
        self.agent_shell_store()
            .get(pane_id)
            .is_some_and(|session| session.log_level.shows_shell_view())
    }

    /// Runs the agent diagnostic level name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn agent_diagnostic_level_name(&self, pane_id: &str) -> Option<&'static str> {
        if self.agent_trace_enabled(pane_id) {
            Some("trace")
        } else if self.agent_debug_enabled(pane_id) {
            Some("debug")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        catch_agent_terminal_presentation_panic, peer_message_echo_rendered_lines,
        peer_message_markdown_rendered_lines, styled_agent_presentation_source_lines,
    };
    use crate::runtime::{PeerMessageLogMode, runtime_peer_message_presentation_is_visible};
    use unicode_width::UnicodeWidthStr;

    /// Verifies typed styled presentation source preserves valid style and text
    /// pairs while rejecting malformed payloads before replay reaches a pane.
    #[test]
    fn styled_agent_presentation_source_lines_decodes_valid_typed_records() {
        let decoded = styled_agent_presentation_source_lines(
            r#"[["user-prompt","user> restore me"],["status","agent: restored"]]"#,
        )
        .expect("valid typed styled presentation source should decode");

        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].1, "user> restore me");
        assert_eq!(decoded[1].1, "agent: restored");
        assert!(styled_agent_presentation_source_lines("not json").is_none());
    }

    /// Verifies a contained terminal parser panic becomes a contextual runtime
    /// error rather than reporting the dropped presentation batch as success.
    #[test]
    fn contained_agent_terminal_presentation_panic_propagates_contextual_error() {
        let error = catch_agent_terminal_presentation_panic("testing panic propagation", || {
            panic!("controlled terminal parser panic");
        })
        .expect_err("contained presentation panic must return an error");

        assert!(
            error.message().contains(
                "agent terminal presentation feed panicked while testing panic propagation"
            ),
            "{error:?}"
        );
    }

    /// Verifies the shared normal-mode predicate accepts canonical plaintext and
    /// both supported Markdown media types while rejecting other raw payloads.
    #[test]
    fn peer_message_normal_mode_accepts_safe_text_and_markdown() {
        for content_type in [
            Some("text/plain; charset=utf-8"),
            Some("text/markdown"),
            Some("text/markdown; charset=utf-8"),
        ] {
            assert!(runtime_peer_message_presentation_is_visible(
                PeerMessageLogMode::Normal,
                content_type
            ));
        }
        for content_type in [
            None,
            Some("text/plain"),
            Some("text/plain; charset=UTF-8"),
            Some("application/json"),
            Some("application/octet-stream"),
        ] {
            assert!(!runtime_peer_message_presentation_is_visible(
                PeerMessageLogMode::Normal,
                content_type
            ));
        }
        assert!(runtime_peer_message_presentation_is_visible(
            PeerMessageLogMode::Verbose,
            Some("application/json")
        ));
    }

    /// Verifies canonical plaintext remains literal, including JSON-looking
    /// content, and retains the normal prompt-style wrapping path.
    #[test]
    fn peer_message_canonical_plaintext_renders_literal_payload() {
        let lines = peer_message_echo_rendered_lines(
            "agent-%3> ",
            r#"{"output":"literal plaintext"}"#,
            80,
            "peer-canonical-plaintext",
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].display,
            r#"agent-%3> {"output":"literal plaintext"}"#
        );
        assert_eq!(
            lines[0].copy_text.as_deref(),
            Some(
                "\u{1e}mez-copy-source-line:peer-canonical-plaintext/0:{\"output\":\"literal plaintext\"}"
            )
        );
        assert!(lines[0].style_spans.is_empty());
    }

    /// Verifies wrapped peer rows retain their raw source line so copy-mode
    /// selection omits both the presentation indent and sender marker.
    #[test]
    fn peer_message_wrapping_retains_one_raw_source_copy_line() {
        let lines =
            peer_message_echo_rendered_lines("agent-%3> ", "alpha beta gamma", 20, "peer-wrapping");
        assert_eq!(
            lines
                .iter()
                .map(|line| line.display.as_str())
                .collect::<Vec<_>>(),
            ["agent-%3> alpha", "     beta gamma"]
        );
        assert_eq!(
            lines
                .iter()
                .map(|line| line.copy_text.as_deref())
                .collect::<Vec<_>>(),
            [
                Some("\u{1e}mez-copy-source-line:peer-wrapping/0:alpha beta gamma"),
                Some("\u{1e}mez-copy-skip-line"),
            ]
        );
    }

    /// A long first-row label cannot suppress a valid word boundary on a
    /// later authored Markdown row that carries only the five-space indent.
    #[test]
    fn peer_markdown_later_row_wraps_independently_of_long_label() {
        let lines = peer_message_markdown_rendered_lines(
            "agent-%123456789> ",
            "first  \none supercalifragilisticexpialidocious",
            &mez_mux::theme::UiTheme::default(),
            22,
        );
        let rows = lines
            .iter()
            .map(|line| line.display.as_str())
            .collect::<Vec<_>>();
        assert!(rows.contains(&"     one"), "{rows:?}");
        assert!(
            rows.iter().any(|row| row.starts_with("     super")),
            "{rows:?}"
        );
        assert!(
            rows.iter().all(|row| UnicodeWidthStr::width(*row) <= 22),
            "{rows:?}"
        );
    }
}
