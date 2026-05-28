//! Transcript TSV encoding, decoding, and validation.
//!
//! The format is append-only and line-oriented so saved conversations remain
//! inspectable without a database. Fields are escaped for tabs, newlines,
//! carriage returns, and backslashes.

use crate::error::{MezError, Result};

use super::types::{AgentPresentationEntry, AgentSessionMetadata, TranscriptEntry, TranscriptRole};
use crate::agent::{ModelTokenUsage, ModelTokenUsageKey};
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

impl TranscriptEntry {
    /// Validates identifiers, sequence metadata, and non-empty content.
    ///
    /// Returns invalid-arguments errors for malformed conversation ids, zero
    /// sequence or timestamp values, or empty required text fields.
    pub fn validate(&self) -> Result<()> {
        validate_conversation_id(&self.conversation_id)?;
        if self.sequence == 0 || self.created_at_unix_seconds == 0 {
            return Err(MezError::invalid_args(
                "transcript sequence and creation time must be non-zero",
            ));
        }
        validate_non_empty("turn id", &self.turn_id)?;
        validate_non_empty("agent id", &self.agent_id)?;
        validate_non_empty("pane id", &self.pane_id)?;
        validate_non_empty("transcript content", &self.content)?;
        Ok(())
    }

    /// Runs the encode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn encode(&self) -> Result<String> {
        self.validate()?;
        Ok([
            TRANSCRIPT_VERSION.to_string(),
            self.conversation_id.clone(),
            self.sequence.to_string(),
            self.created_at_unix_seconds.to_string(),
            role_name(self.role).to_string(),
            self.turn_id.clone(),
            self.agent_id.clone(),
            self.pane_id.clone(),
            self.content.clone(),
        ]
        .into_iter()
        .map(|field| escape_field(&field))
        .collect::<Vec<String>>()
        .join("\t"))
    }

    /// Runs the decode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn decode(line: &str) -> Result<Self> {
        let fields = split_fields(line)?;
        if fields.len() != 9 || fields[0] != TRANSCRIPT_VERSION {
            return Err(MezError::invalid_args("invalid transcript entry"));
        }
        let entry = Self {
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
}

impl AgentPresentationEntry {
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
        entry.validate()?;
        Ok(entry)
    }
}

/// Runs the validate conversation id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_conversation_id(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(MezError::invalid_args("conversation id is invalid"));
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

impl AgentSessionMetadata {
    /// Validates active agent session metadata before it is persisted or used.
    pub fn validate(&self) -> Result<()> {
        validate_non_empty("mezzanine session id", &self.mezzanine_session_id)?;
        validate_non_empty("pane id", &self.pane_id)?;
        validate_conversation_id(&self.conversation_id)?;
        validate_non_empty("prompt cache lineage id", &self.prompt_cache_lineage_id)?;
        validate_agent_visibility(&self.visibility)?;
        if let Some(turn_id) = self.running_turn_id.as_deref() {
            validate_non_empty("running turn id", turn_id)?;
        }
        validate_log_level(&self.log_level)?;
        if let Some(profile) = self.pane_model_profile.as_deref() {
            validate_non_empty("pane model profile", profile)?;
        }
        if let Some(style) = self.response_style.as_deref() {
            validate_non_empty("response style", style)?;
        }
        if let Some(working_directory) = self.working_directory.as_deref() {
            validate_non_empty("working directory", working_directory)?;
        }
        if let Some(project_root) = self.project_root.as_deref() {
            validate_non_empty("project root", project_root)?;
        }
        if let Some(approval_policy) = self.approval_policy.as_deref() {
            validate_agent_approval_policy(approval_policy)?;
        }
        if let Some(context_usage) = self.context_usage.as_deref() {
            validate_non_empty("context usage", context_usage)?;
        }
        if let Some(snapshot) = self.context_usage_snapshot {
            if snapshot.input_tokens == 0 {
                return Err(MezError::invalid_args(
                    "context usage snapshot input_tokens must be greater than zero",
                ));
            }
            if snapshot.context_window_tokens == 0 {
                return Err(MezError::invalid_args(
                    "context usage snapshot context_window_tokens must be greater than zero",
                ));
            }
        }
        for key in self.token_usage_by_model.keys() {
            validate_non_empty("token usage provider", &key.provider)?;
            validate_non_empty("token usage model", &key.model)?;
        }
        Ok(())
    }

    /// Encodes one agent-session metadata row into the store's TSV format.
    pub(super) fn encode(&self) -> Result<String> {
        self.validate()?;
        let token_usage_by_model = encode_token_usage_by_model(&self.token_usage_by_model)?;
        let context_usage_snapshot =
            encode_context_usage_snapshot(self.context_usage_snapshot.as_ref())?;
        Ok([
            AGENT_SESSION_METADATA_VERSION.to_string(),
            self.mezzanine_session_id.clone(),
            self.pane_id.clone(),
            self.conversation_id.clone(),
            self.prompt_cache_lineage_id.clone(),
            self.visibility.clone(),
            self.running_turn_id.clone().unwrap_or_default(),
            self.transcript_entries.to_string(),
            self.log_level.clone(),
            self.pane_model_profile.clone().unwrap_or_default(),
            self.planning_enabled.to_string(),
            self.response_style.clone().unwrap_or_default(),
            self.routing_enabled
                .map(|enabled| enabled.to_string())
                .unwrap_or_default(),
            self.working_directory.clone().unwrap_or_default(),
            self.project_root.clone().unwrap_or_default(),
            self.token_usage.input_tokens.to_string(),
            self.token_usage.output_tokens.to_string(),
            self.token_usage.reasoning_tokens.to_string(),
            self.token_usage
                .cached_input_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_default(),
            self.approval_policy.clone().unwrap_or_default(),
            self.context_usage.clone().unwrap_or_default(),
            token_usage_by_model,
            context_usage_snapshot,
        ]
        .into_iter()
        .map(|field| escape_field(&field))
        .collect::<Vec<String>>()
        .join("\t"))
    }

    /// Decodes one agent-session metadata row from the store's TSV format.
    pub(super) fn decode(line: &str) -> Result<Self> {
        let fields = split_fields(line)?;
        if !(fields.len() == 11
            || fields.len() == 12
            || fields.len() == 14
            || fields.len() == 18
            || fields.len() == 19
            || fields.len() == 20
            || fields.len() == 21
            || fields.len() == 22
            || fields.len() == 23)
            || fields[0] != AGENT_SESSION_METADATA_VERSION
        {
            return Err(MezError::invalid_args(
                "invalid agent session metadata entry",
            ));
        }
        let legacy_layout = fields.len() <= 22;
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
        let routing_enabled_index = if legacy_layout { 11 } else { 12 };
        let working_directory_index = if legacy_layout { 12 } else { 13 };
        let project_root_index = if legacy_layout { 13 } else { 14 };
        let token_usage_start = if legacy_layout { 14 } else { 15 };
        let approval_policy_index = if legacy_layout { 18 } else { 19 };
        let context_usage_index = if legacy_layout { 19 } else { 20 };
        let token_usage_by_model_index = if legacy_layout { 20 } else { 21 };
        let context_usage_snapshot_index = if legacy_layout { 21 } else { 22 };
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
            }
        } else {
            ModelTokenUsage::default()
        };
        let metadata = Self {
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
                "cached_input_tokens": usage.cached_input_tokens
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
    snapshot: Option<&crate::agent::AgentContextUsageSnapshot>,
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
fn decode_context_usage_snapshot(value: &str) -> Result<crate::agent::AgentContextUsageSnapshot> {
    let object = serde_json::from_str::<serde_json::Value>(value).map_err(|error| {
        MezError::invalid_args(format!(
            "agent session context usage snapshot JSON is invalid: {error}"
        ))
    })?;
    let object = object.as_object().ok_or_else(|| {
        MezError::invalid_args("agent session context usage snapshot must be an object")
    })?;
    Ok(crate::agent::AgentContextUsageSnapshot {
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

/// Validates the persisted agent shell visibility spelling.
fn validate_agent_visibility(value: &str) -> Result<()> {
    match value {
        "hidden" | "visible" | "hide-pending-task-completion" => Ok(()),
        _ => Err(MezError::invalid_args(
            "agent session visibility is invalid",
        )),
    }
}

/// Validates the persisted agent log level spelling.
fn validate_log_level(value: &str) -> Result<()> {
    match value {
        "normal" | "verbose" | "debug" | "trace" => Ok(()),
        _ => Err(MezError::invalid_args("agent session log level is invalid")),
    }
}

/// Validates the persisted approval-policy spelling.
fn validate_agent_approval_policy(value: &str) -> Result<()> {
    match value {
        "ask" | "auto-allow" | "full-access" => Ok(()),
        _ => Err(MezError::invalid_args(
            "agent session approval policy is invalid",
        )),
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
