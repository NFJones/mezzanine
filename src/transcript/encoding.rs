//! Transcript TSV encoding, decoding, and validation.
//!
//! The format is append-only and line-oriented so saved conversations remain
//! inspectable without a database. Fields are escaped for tabs, newlines,
//! carriage returns, and backslashes.

use crate::error::{MezError, Result};
use crate::terminal::{agent_log_wrap_width, wrap_agent_log_lines};
use mez_terminal::active_terminal_text_width;

use super::types::AgentPresentationEntry;
use mez_agent::transcript::{
    AgentSessionMetadata, TranscriptEntry, TranscriptRole, validate_conversation_id,
};
use mez_agent::{ModelTokenUsage, ModelTokenUsageKey};
use std::collections::BTreeMap;

/// Defines the TRANSCRIPT VERSION const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const TRANSCRIPT_VERSION: &str = "mez-agent-transcript/1";
/// Defines the PROMPT HISTORY VERSION const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const PROMPT_HISTORY_VERSION: &str = "mez-agent-prompt-history/1";
/// Defines the AGENT SESSION METADATA VERSION const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const AGENT_SESSION_METADATA_VERSION: &str = "mez-agent-session-metadata/1";
/// Defines the AGENT PRESENTATION VERSION const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const AGENT_PRESENTATION_VERSION: &str = "mez-agent-presentation/1";

/// Encodes one canonical transcript entry into the durable TSV format.
pub(super) fn encode_transcript_entry(entry: &TranscriptEntry) -> Result<String> {
    entry.validate()?;
    Ok([
        TRANSCRIPT_VERSION.to_string(),
        entry.conversation_id.clone(),
        entry.sequence.to_string(),
        entry.created_at_unix_seconds.to_string(),
        role_name(entry.role).to_string(),
        entry.turn_id.clone(),
        entry.agent_id.clone(),
        entry.pane_id.clone(),
        entry.content.clone(),
    ]
    .into_iter()
    .map(|field| escape_field(&field))
    .collect::<Vec<String>>()
    .join("\t"))
}

/// Decodes one canonical transcript entry from the durable TSV format.
pub(super) fn decode_transcript_entry(line: &str) -> Result<TranscriptEntry> {
    let fields = split_fields(line)?;
    if fields.len() != 9 || fields[0] != TRANSCRIPT_VERSION {
        return Err(MezError::invalid_args("invalid transcript entry"));
    }
    let entry = TranscriptEntry {
        conversation_id: fields[1].clone(),
        sequence: parse_u64(&fields[2], "sequence")?,
        created_at_unix_seconds: parse_u64(&fields[3], "created_at_unix_seconds")?,
        role: parse_role(&fields[4])?,
        turn_id: fields[5].clone(),
        agent_id: fields[6].clone(),
        pane_id: fields[7].clone(),
        content: fields[8].clone(),
    };
    entry.validate()?;
    Ok(entry)
}

impl AgentPresentationEntry {
    /// Returns a presentation entry whose display and copy rows obey the agent
    /// log wrapping contract for the recorded terminal width.
    pub(crate) fn normalized_for_agent_log_wrap(&self) -> Self {
        if self.style_names.len() != self.display_lines.len() {
            return self.clone();
        }
        let mut display_lines = Vec::new();
        let mut style_names = Vec::new();
        for (line, style_name) in self.display_lines.iter().zip(self.style_names.iter()) {
            for wrapped_line in
                wrap_agent_log_lines(std::slice::from_ref(line), self.terminal_width)
            {
                display_lines.push(wrapped_line);
                style_names.push(style_name.clone());
            }
        }
        let copy_lines = wrap_agent_log_lines(&self.copy_lines, self.terminal_width);
        let changed = display_lines != self.display_lines
            || style_names != self.style_names
            || copy_lines != self.copy_lines;
        Self {
            style_names,
            display_lines,
            copy_lines,
            ansi_text: if changed {
                None
            } else {
                self.ansi_text.clone()
            },
            ..self.clone()
        }
    }

    /// Validates one durable presentation entry before persistence or replay.
    pub fn validate(&self) -> Result<()> {
        validate_conversation_id(&self.conversation_id)?;
        if self.sequence == 0 || self.created_at_unix_seconds == 0 {
            return Err(MezError::invalid_args(
                "presentation sequence and creation time must be non-zero",
            ));
        }
        validate_non_empty("pane id", &self.pane_id)?;
        if let Some(turn_id) = self.turn_id.as_deref() {
            validate_non_empty("turn id", turn_id)?;
        }
        if self.terminal_width == 0 {
            return Err(MezError::invalid_args(
                "presentation terminal width must be non-zero",
            ));
        }
        if self.display_lines.is_empty() {
            return Err(MezError::invalid_args(
                "presentation display lines must not be empty",
            ));
        }
        if self.style_names.len() != self.display_lines.len() {
            return Err(MezError::invalid_args(
                "presentation style count must match display line count",
            ));
        }
        let wrap_width = agent_log_wrap_width(self.terminal_width);
        for line in &self.display_lines {
            validate_presentation_line_width("display", line, wrap_width)?;
        }
        for line in &self.copy_lines {
            validate_presentation_line_width("copy", line, wrap_width)?;
        }
        for style in &self.style_names {
            validate_non_empty("presentation style", style)?;
        }
        Ok(())
    }

    /// Encodes one presentation entry into the store's TSV format.
    pub(super) fn encode(&self) -> Result<String> {
        self.validate()?;
        let style_names = serde_json::to_string(&self.style_names).map_err(|error| {
            MezError::invalid_args(format!("presentation style encoding failed: {error}"))
        })?;
        let display_lines = serde_json::to_string(&self.display_lines).map_err(|error| {
            MezError::invalid_args(format!("presentation display encoding failed: {error}"))
        })?;
        let copy_lines = serde_json::to_string(&self.copy_lines).map_err(|error| {
            MezError::invalid_args(format!("presentation copy encoding failed: {error}"))
        })?;
        Ok([
            AGENT_PRESENTATION_VERSION.to_string(),
            self.conversation_id.clone(),
            self.sequence.to_string(),
            self.created_at_unix_seconds.to_string(),
            self.pane_id.clone(),
            self.turn_id.clone().unwrap_or_default(),
            self.terminal_width.to_string(),
            style_names,
            display_lines,
            copy_lines,
            self.ansi_text.clone().unwrap_or_default(),
        ]
        .into_iter()
        .map(|field| escape_field(&field))
        .collect::<Vec<String>>()
        .join("\t"))
    }

    /// Decodes one presentation entry from the store's TSV format.
    pub(super) fn decode(line: &str) -> Result<Self> {
        let fields = split_fields(line)?;
        if !(fields.len() == 10 || fields.len() == 11) || fields[0] != AGENT_PRESENTATION_VERSION {
            return Err(MezError::invalid_args("invalid presentation entry"));
        }
        let entry = Self {
            conversation_id: fields[1].clone(),
            sequence: parse_u64(&fields[2], "presentation sequence")?,
            created_at_unix_seconds: parse_u64(&fields[3], "presentation created_at_unix_seconds")?,
            pane_id: fields[4].clone(),
            turn_id: (!fields[5].is_empty()).then(|| fields[5].clone()),
            terminal_width: parse_u16(&fields[6], "presentation terminal_width")?,
            style_names: decode_string_vec(&fields[7], "presentation style names")?,
            display_lines: decode_string_vec(&fields[8], "presentation display lines")?,
            copy_lines: decode_string_vec(&fields[9], "presentation copy lines")?,
            ansi_text: fields.get(10).filter(|value| !value.is_empty()).cloned(),
        };
        let entry = entry.normalized_for_agent_log_wrap();
        entry.validate()?;
        Ok(entry)
    }
}

/// Validates one persisted presentation row against the effective wrap width.
fn validate_presentation_line_width(field: &str, line: &str, wrap_width: usize) -> Result<()> {
    if active_terminal_text_width(line) > wrap_width {
        return Err(MezError::invalid_args(format!(
            "presentation {field} line exceeds {wrap_width} display columns"
        )));
    }
    Ok(())
}

/// Runs the encode prompt history entry operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn encode_prompt_history_entry(prompt: &str) -> Result<String> {
    validate_non_empty("prompt history entry", prompt)?;
    Ok([PROMPT_HISTORY_VERSION, prompt]
        .into_iter()
        .map(escape_field)
        .collect::<Vec<String>>()
        .join("\t"))
}

/// Runs the decode prompt history entry operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn decode_prompt_history_entry(line: &str) -> Result<String> {
    let fields = split_fields(line)?;
    if fields.len() != 2 || fields[0] != PROMPT_HISTORY_VERSION {
        return Err(MezError::invalid_args("invalid prompt history entry"));
    }
    validate_non_empty("prompt history entry", &fields[1])?;
    Ok(fields[1].clone())
}

/// Encodes one agent-session metadata row into the store's TSV format.
pub(super) fn encode_agent_session_metadata(metadata: &AgentSessionMetadata) -> Result<String> {
    metadata.validate()?;
    let token_usage_by_model = encode_token_usage_by_model(&metadata.token_usage_by_model)?;
    let context_usage_snapshot =
        encode_context_usage_snapshot(metadata.context_usage_snapshot.as_ref())?;
    Ok([
        AGENT_SESSION_METADATA_VERSION.to_string(),
        metadata.mezzanine_session_id.clone(),
        metadata.pane_id.clone(),
        metadata.conversation_id.clone(),
        metadata.prompt_cache_lineage_id.clone(),
        metadata.visibility.clone(),
        metadata.running_turn_id.clone().unwrap_or_default(),
        metadata.transcript_entries.to_string(),
        metadata.log_level.clone(),
        metadata.pane_model_profile.clone().unwrap_or_default(),
        metadata.planning_enabled.to_string(),
        metadata.response_style.clone().unwrap_or_default(),
        metadata.directive.clone().unwrap_or_default(),
        metadata
            .routing_enabled
            .map(|enabled| enabled.to_string())
            .unwrap_or_default(),
        metadata.working_directory.clone().unwrap_or_default(),
        metadata.project_root.clone().unwrap_or_default(),
        metadata.token_usage.input_tokens.to_string(),
        metadata.token_usage.output_tokens.to_string(),
        metadata.token_usage.reasoning_tokens.to_string(),
        metadata
            .token_usage
            .cached_input_tokens
            .map(|tokens| tokens.to_string())
            .unwrap_or_default(),
        metadata
            .token_usage
            .cache_write_input_tokens
            .map(|tokens| tokens.to_string())
            .unwrap_or_default(),
        metadata.approval_policy.clone().unwrap_or_default(),
        metadata.context_usage.clone().unwrap_or_default(),
        token_usage_by_model,
        context_usage_snapshot,
    ]
    .into_iter()
    .map(|field| escape_field(&field))
    .collect::<Vec<String>>()
    .join("\t"))
}

/// Decodes one agent-session metadata row from the store's TSV format.
pub(super) fn decode_agent_session_metadata(line: &str) -> Result<AgentSessionMetadata> {
    let fields = split_fields(line)?;
    if !(fields.len() == 11
        || fields.len() == 12
        || fields.len() == 14
        || fields.len() == 18
        || fields.len() == 19
        || fields.len() == 20
        || fields.len() == 21
        || fields.len() == 22
        || fields.len() == 23
        || fields.len() == 24
        || fields.len() == 25)
        || fields[0] != AGENT_SESSION_METADATA_VERSION
    {
        return Err(MezError::invalid_args(
            "invalid agent session metadata entry",
        ));
    }
    let legacy_layout = fields.len() <= 22;
    let current_without_directive_layout = fields.len() == 23;
    let current_with_cache_write_layout = fields.len() == 25;
    let prompt_cache_lineage_id = if legacy_layout {
        fields[3].clone()
    } else {
        fields[4].clone()
    };
    let visibility_index = if legacy_layout { 4 } else { 5 };
    let running_turn_index = if legacy_layout { 5 } else { 6 };
    let transcript_entries_index = if legacy_layout { 6 } else { 7 };
    let log_level_index = if legacy_layout { 7 } else { 8 };
    let pane_model_profile_index = if legacy_layout { 8 } else { 9 };
    let planning_enabled_index = if legacy_layout { 9 } else { 10 };
    let response_style_index = if legacy_layout { 10 } else { 11 };
    let directive_index = if legacy_layout || current_without_directive_layout {
        None
    } else {
        Some(12)
    };
    let routing_enabled_index = if legacy_layout {
        11
    } else if current_without_directive_layout {
        12
    } else {
        13
    };
    let working_directory_index = if legacy_layout {
        12
    } else if current_without_directive_layout {
        13
    } else {
        14
    };
    let project_root_index = if legacy_layout {
        13
    } else if current_without_directive_layout {
        14
    } else {
        15
    };
    let token_usage_start = if legacy_layout {
        14
    } else if current_without_directive_layout {
        15
    } else {
        16
    };
    let approval_policy_index = if legacy_layout {
        18
    } else if current_without_directive_layout {
        19
    } else if current_with_cache_write_layout {
        21
    } else {
        20
    };
    let context_usage_index = if legacy_layout {
        19
    } else if current_without_directive_layout {
        20
    } else if current_with_cache_write_layout {
        22
    } else {
        21
    };
    let token_usage_by_model_index = if legacy_layout {
        20
    } else if current_without_directive_layout {
        21
    } else if current_with_cache_write_layout {
        23
    } else {
        22
    };
    let context_usage_snapshot_index = if legacy_layout {
        21
    } else if current_without_directive_layout {
        22
    } else if current_with_cache_write_layout {
        24
    } else {
        23
    };
    let token_usage = if fields.len() >= 18 {
        ModelTokenUsage {
            input_tokens: parse_u64(&fields[token_usage_start], "agent session input_tokens")?,
            output_tokens: parse_u64(
                &fields[token_usage_start + 1],
                "agent session output_tokens",
            )?,
            reasoning_tokens: parse_u64(
                &fields[token_usage_start + 2],
                "agent session reasoning_tokens",
            )?,
            cached_input_tokens: fields
                .get(token_usage_start + 3)
                .filter(|value| !value.is_empty())
                .map(|value| parse_u64(value, "agent session cached_input_tokens"))
                .transpose()?,
            cache_write_input_tokens: if current_with_cache_write_layout {
                fields
                    .get(token_usage_start + 4)
                    .filter(|value| !value.is_empty())
                    .map(|value| parse_u64(value, "agent session cache_write_input_tokens"))
                    .transpose()?
            } else {
                None
            },
        }
    } else {
        ModelTokenUsage::default()
    };
    let metadata = AgentSessionMetadata {
        mezzanine_session_id: fields[1].clone(),
        pane_id: fields[2].clone(),
        conversation_id: fields[3].clone(),
        prompt_cache_lineage_id,
        visibility: fields[visibility_index].clone(),
        running_turn_id: (!fields[running_turn_index].is_empty())
            .then(|| fields[running_turn_index].clone()),
        transcript_entries: parse_u64(
            &fields[transcript_entries_index],
            "agent session transcript_entries",
        )?,
        log_level: fields[log_level_index].clone(),
        pane_model_profile: (!fields[pane_model_profile_index].is_empty())
            .then(|| fields[pane_model_profile_index].clone()),
        planning_enabled: parse_bool(&fields[planning_enabled_index], "planning_enabled")?,
        response_style: (!fields[response_style_index].is_empty())
            .then(|| fields[response_style_index].clone()),
        directive: directive_index
            .and_then(|index| fields.get(index))
            .filter(|value| !value.is_empty())
            .cloned(),
        routing_enabled: fields
            .get(routing_enabled_index)
            .filter(|value| !value.is_empty())
            .map(|value| parse_bool(value, "routing_enabled"))
            .transpose()?,
        working_directory: fields
            .get(working_directory_index)
            .filter(|value| !value.is_empty())
            .cloned(),
        project_root: fields
            .get(project_root_index)
            .filter(|value| !value.is_empty())
            .cloned(),
        token_usage,
        token_usage_by_model: fields
            .get(token_usage_by_model_index)
            .filter(|value| !value.is_empty())
            .map(|value| decode_token_usage_by_model(value))
            .transpose()?
            .unwrap_or_default(),
        context_usage_snapshot: fields
            .get(context_usage_snapshot_index)
            .filter(|value| !value.is_empty())
            .map(|value| decode_context_usage_snapshot(value))
            .transpose()?,
        approval_policy: fields
            .get(approval_policy_index)
            .filter(|value| !value.is_empty())
            .cloned(),
        context_usage: fields
            .get(context_usage_index)
            .filter(|value| !value.is_empty())
            .cloned(),
    };
    metadata.validate()?;
    Ok(metadata)
}

/// Encodes per-model token usage into one stable TSV field.
fn encode_token_usage_by_model(
    usage_by_model: &BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
) -> Result<String> {
    if usage_by_model.is_empty() {
        return Ok(String::new());
    }
    let rows = usage_by_model
        .iter()
        .map(|(key, usage)| {
            serde_json::json!({
                "provider": key.provider,
                "model": key.model,
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "reasoning_tokens": usage.reasoning_tokens,
                "cached_input_tokens": usage.cached_input_tokens,
                "cache_write_input_tokens": usage.cache_write_input_tokens
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&rows).map_err(|error| {
        MezError::invalid_state(format!(
            "agent session token usage JSON encoding failed: {error}"
        ))
    })
}

/// Decodes per-model token usage from one stable TSV field.
fn decode_token_usage_by_model(
    value: &str,
) -> Result<BTreeMap<ModelTokenUsageKey, ModelTokenUsage>> {
    let rows = serde_json::from_str::<serde_json::Value>(value).map_err(|error| {
        MezError::invalid_args(format!(
            "agent session token usage JSON is invalid: {error}"
        ))
    })?;
    let Some(rows) = rows.as_array() else {
        return Err(MezError::invalid_args(
            "agent session token usage JSON must be an array",
        ));
    };
    let mut usage_by_model = BTreeMap::new();
    for row in rows {
        let object = row.as_object().ok_or_else(|| {
            MezError::invalid_args("agent session token usage row must be an object")
        })?;
        let provider = json_string_field(object, "provider")?;
        let model = json_string_field(object, "model")?;
        let usage = ModelTokenUsage {
            input_tokens: json_u64_field(object, "input_tokens")?,
            output_tokens: json_u64_field(object, "output_tokens")?,
            reasoning_tokens: json_u64_field(object, "reasoning_tokens")?,
            cached_input_tokens: json_optional_u64_field(object, "cached_input_tokens")?,
            cache_write_input_tokens: json_optional_u64_field(object, "cache_write_input_tokens")?,
        };
        usage_by_model
            .entry(ModelTokenUsageKey::new(provider, model))
            .or_insert(ModelTokenUsage::default())
            .add_assign(usage);
    }
    Ok(usage_by_model)
}

/// Encodes the last request-context snapshot into one stable TSV field.
fn encode_context_usage_snapshot(
    snapshot: Option<&mez_agent::AgentContextUsageSnapshot>,
) -> Result<String> {
    let Some(snapshot) = snapshot else {
        return Ok(String::new());
    };
    serde_json::to_string(&serde_json::json!({
        "input_tokens": snapshot.input_tokens,
        "context_window_tokens": snapshot.context_window_tokens,
        "cached_input_tokens": snapshot.cached_input_tokens,
    }))
    .map_err(|error| {
        MezError::invalid_state(format!(
            "agent session context usage snapshot encoding failed: {error}"
        ))
    })
}

/// Decodes the last request-context snapshot from one stable TSV field.
fn decode_context_usage_snapshot(value: &str) -> Result<mez_agent::AgentContextUsageSnapshot> {
    let object = serde_json::from_str::<serde_json::Value>(value).map_err(|error| {
        MezError::invalid_args(format!(
            "agent session context usage snapshot JSON is invalid: {error}"
        ))
    })?;
    let object = object.as_object().ok_or_else(|| {
        MezError::invalid_args("agent session context usage snapshot must be an object")
    })?;
    Ok(mez_agent::AgentContextUsageSnapshot {
        input_tokens: json_u64_field(object, "input_tokens")?,
        context_window_tokens: json_u64_field(object, "context_window_tokens")?,
        cached_input_tokens: json_optional_u64_field(object, "cached_input_tokens")?,
    })
}

/// Returns a required string field from a JSON object.
fn json_string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| MezError::invalid_args(format!("agent session token usage missing {field}")))
}

/// Returns a required unsigned integer field from a JSON object.
fn json_u64_field(object: &serde_json::Map<String, serde_json::Value>, field: &str) -> Result<u64> {
    object
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_args(format!("agent session token usage missing {field}")))
}

/// Returns an optional unsigned integer field from a JSON object.
fn json_optional_u64_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<u64>> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            MezError::invalid_args(format!(
                "agent session token usage field {field} must be u64"
            ))
        }),
    }
}

/// Runs the role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn role_name(role: TranscriptRole) -> &'static str {
    match role {
        TranscriptRole::User => "user",
        TranscriptRole::Assistant => "assistant",
        TranscriptRole::Tool => "tool",
        TranscriptRole::System => "system",
    }
}

/// Runs the parse role operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_role(value: &str) -> Result<TranscriptRole> {
    match value {
        "user" => Ok(TranscriptRole::User),
        "assistant" => Ok(TranscriptRole::Assistant),
        "tool" => Ok(TranscriptRole::Tool),
        "system" => Ok(TranscriptRole::System),
        _ => Err(MezError::invalid_args("unknown transcript role")),
    }
}

/// Runs the validate non empty operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_non_empty(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(MezError::invalid_args(format!("{label} must not be empty")))
    } else {
        Ok(())
    }
}

/// Runs the parse u64 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_u64(value: &str, label: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| MezError::invalid_args(format!("invalid transcript {label}")))
}

/// Runs the parse bool operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_bool(value: &str, label: &str) -> Result<bool> {
    value
        .parse::<bool>()
        .map_err(|_| MezError::invalid_args(format!("invalid transcript {label}")))
}

/// Parses one unsigned 16-bit integer from a transcript field.
fn parse_u16(value: &str, label: &str) -> Result<u16> {
    value
        .parse::<u16>()
        .map_err(|_| MezError::invalid_args(format!("invalid transcript {label}")))
}

/// Decodes a JSON string array stored inside one escaped TSV field.
fn decode_string_vec(value: &str, label: &str) -> Result<Vec<String>> {
    serde_json::from_str::<Vec<String>>(value)
        .map_err(|error| MezError::invalid_args(format!("invalid transcript {label}: {error}")))
}

/// Runs the escape field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn escape_field(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// Runs the split fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn split_fields(line: &str) -> Result<Vec<String>> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\t' => {
                fields.push(field);
                field = String::new();
            }
            '\\' => {
                let escaped = chars
                    .next()
                    .ok_or_else(|| MezError::invalid_args("trailing transcript escape"))?;
                field.push(match escaped {
                    '\\' => '\\',
                    't' => '\t',
                    'n' => '\n',
                    'r' => '\r',
                    _ => return Err(MezError::invalid_args("unsupported transcript escape")),
                });
            }
            _ => field.push(ch),
        }
    }
    fields.push(field);
    Ok(fields)
}
