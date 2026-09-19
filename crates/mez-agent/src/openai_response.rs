//! Provider-independent OpenAI Responses API response parsing.
//!
//! This module owns deterministic HTTP and SSE response parsing for
//! OpenAI-compatible Responses API calls, including native MAAP function-call
//! argument accumulation and provider token-usage extraction. Product
//! transports and conversion into product errors remain outside this crate.

use crate::ProviderTranscriptEvent;
use crate::accounting::ModelTokenUsage;
use crate::http::{DEFAULT_PROVIDER_MAX_RESPONSE_BYTES, parse_sse_events_with};
#[cfg(test)]
use crate::provider::ProviderResponseErrorKind;
use crate::provider::{
    MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME,
    ProviderOutputLimitContinuationDisposition, ProviderOutputLimitState, ProviderResponseError,
    ProviderResponseResult,
};
use crate::provider_diagnostics::{
    provider_failure_event_json as openai_provider_failure_event_json,
    sanitize_provider_primary_error_text,
};
use crate::schema::OpenAiMaapToolSurface;
use std::collections::BTreeMap;

/// Maximum native function-call argument bytes accepted from OpenAI responses.
const OPENAI_FUNCTION_CALL_ARGUMENT_LIMIT_BYTES: usize = DEFAULT_PROVIDER_MAX_RESPONSE_BYTES;

/// Selects the OpenAI response parser that matches the transport mode.
pub fn parse_openai_responses_provider_body(
    body: &str,
    fallback_model: &str,
    stream: bool,
) -> ProviderResponseResult<(
    String,
    String,
    ModelTokenUsage,
    Vec<ProviderTranscriptEvent>,
)> {
    if stream {
        parse_openai_responses_stream_body(body, fallback_model)
    } else {
        parse_openai_responses_http_body(body, fallback_model)
    }
}

/// Parses one non-streaming OpenAI Responses API body.
pub fn parse_openai_responses_http_body(
    body: &str,
    fallback_model: &str,
) -> ProviderResponseResult<(
    String,
    String,
    ModelTokenUsage,
    Vec<ProviderTranscriptEvent>,
)> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|error| {
        ProviderResponseError::invalid_state(format!("OpenAI response was not JSON: {error}"))
    })?;
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("OpenAI response contained an error");
        let message = sanitize_provider_primary_error_text(message);
        return Err(ProviderResponseError::invalid_state(message)
            .with_provider_failure_json(openai_provider_failure_event_json(&value)));
    }
    if value.get("status").and_then(serde_json::Value::as_str) == Some("incomplete") {
        let stop_reason = value
            .pointer("/incomplete_details/reason")
            .or_else(|| value.pointer("/response/incomplete_details/reason"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let safe_stop_reason = sanitize_provider_primary_error_text(stop_reason);
        let safe_partial_text = value
            .get("output_text")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| collect_openai_output_text(&value))
            .unwrap_or_default();
        let native_items = value
            .get("output")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter(|item| {
                        item.get("type").and_then(serde_json::Value::as_str)
                            == Some("function_call")
                    })
                    .count()
            })
            .unwrap_or(0);
        let continuation_disposition = if native_items == 0 {
            ProviderOutputLimitContinuationDisposition::ContinueVisibleText
        } else {
            ProviderOutputLimitContinuationDisposition::ReemitAtomicNativeCall
        };
        return Err(ProviderResponseError::invalid_state(format!(
            "OpenAI response returned an incomplete response: {safe_stop_reason}"
        ))
        .with_provider_failure_json(openai_provider_failure_event_json(&value))
        .with_output_limit_state(ProviderOutputLimitState::new(
            "openai",
            "responses",
            stop_reason,
            value
                .get("id")
                .or_else(|| value.pointer("/response/id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            safe_partial_text,
            0,
            native_items,
            openai_token_usage_from_response_value(&value),
            continuation_disposition,
        )));
    }
    if let Some(status) = value.get("status").and_then(serde_json::Value::as_str)
        && status != "completed"
    {
        return Err(
            ProviderResponseError::invalid_state("OpenAI response did not complete")
                .with_provider_failure_json(openai_provider_failure_event_json(&value)),
        );
    }
    let model = value
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(fallback_model)
        .to_string();
    let raw_text = collect_openai_maap_function_call_arguments(&value)?
        .or_else(|| {
            value
                .get("output_text")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| collect_openai_output_text(&value));
    let Some(raw_text) = raw_text else {
        return Err(ProviderResponseError::invalid_state(
            "OpenAI response did not contain text or MAAP function-call output",
        ));
    };
    let usage = openai_token_usage_from_response_value(&value);
    let provider_transcript_events = openai_response_output_event(&value);
    Ok((model, raw_text, usage, provider_transcript_events))
}

/// Incrementally accumulates one OpenAI Responses API SSE stream.
#[derive(Debug, Default)]
pub struct OpenAiResponsesStreamDecoder {
    model: Option<String>,
    completed: bool,
    usage: ModelTokenUsage,
    function_calls: BTreeMap<u64, OpenAiFunctionCallAccumulator>,
    completed_output_items: BTreeMap<u64, serde_json::Value>,
    completed_response_output: Option<Vec<serde_json::Value>>,
    output_item_text: String,
    delta_text: String,
}

impl OpenAiResponsesStreamDecoder {
    /// Applies one complete SSE event and returns display-safe text progress.
    pub fn push_event(
        &mut self,
        event: &crate::SseEvent,
    ) -> ProviderResponseResult<Option<String>> {
        let data = event.data.trim();
        if data == "[DONE]" {
            return Ok(None);
        }
        let value: serde_json::Value = serde_json::from_str(data).map_err(|error| {
            ProviderResponseError::invalid_state(format!(
                "OpenAI stream event was not JSON: {error}"
            ))
        })?;
        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
            let message = error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("OpenAI stream contained an error");
            let message = sanitize_provider_primary_error_text(message);
            return Err(ProviderResponseError::invalid_state(message)
                .with_provider_failure_json(openai_provider_failure_event_json(&value)));
        }
        let event_usage = openai_token_usage_from_response_value(&value);
        if !event_usage.is_zero() {
            self.usage = event_usage;
        }

        let event_type = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .or(event.name.as_deref())
            .unwrap_or_default();
        if self.model.is_none() {
            self.model = value
                .get("response")
                .and_then(|response| response.get("model"))
                .or_else(|| value.get("model"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
        }

        match event_type {
            "response.output_item.done" | "response.output_item.added" => {
                if let Some(item) = value.get("item") {
                    collect_openai_maap_function_call_event_item(
                        &mut self.function_calls,
                        &value,
                        item,
                    )?;
                    append_openai_response_item_text(item, &mut self.output_item_text);
                    if event_type == "response.output_item.done" {
                        let output_index = openai_output_index(&value).unwrap_or_default();
                        self.completed_output_items
                            .insert(output_index, item.clone());
                    }
                }
                Ok(None)
            }
            "response.output_text.delta" => {
                let delta = value
                    .get("delta")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                self.delta_text.push_str(delta);
                Ok((!delta.is_empty()).then(|| delta.to_string()))
            }
            "response.function_call_arguments.delta" => {
                if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                    let output_index = openai_output_index(&value).unwrap_or_default();
                    push_openai_function_call_argument_delta(
                        self.function_calls.entry(output_index).or_default(),
                        delta,
                    )?;
                }
                Ok(None)
            }
            "response.function_call_arguments.done" => {
                let output_index = openai_output_index(&value).unwrap_or_default();
                if let Some(item) = value.get("item") {
                    collect_openai_maap_function_call_event_item(
                        &mut self.function_calls,
                        &value,
                        item,
                    )?;
                }
                if let Some(arguments) = value
                    .get("arguments")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| value.get("item").and_then(openai_function_call_arguments))
                {
                    set_openai_function_call_complete_arguments(
                        self.function_calls.entry(output_index).or_default(),
                        arguments,
                    )?;
                }
                Ok(None)
            }
            "response.completed" => {
                self.completed = true;
                self.completed_response_output = value
                    .pointer("/response/output")
                    .and_then(serde_json::Value::as_array)
                    .cloned();
                Ok(None)
            }
            "response.failed" => Err(ProviderResponseError::invalid_state(
                openai_stream_event_error_detail(&value, "OpenAI stream failed"),
            )
            .with_provider_failure_json(openai_provider_failure_event_json(&value))),
            "response.incomplete" => {
                let stop_reason = value
                    .pointer("/response/incomplete_details/reason")
                    .or_else(|| value.pointer("/incomplete_details/reason"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown");
                let safe_partial_text = if self.output_item_text.is_empty() {
                    self.delta_text.clone()
                } else {
                    self.output_item_text.clone()
                };
                let complete_native_items = self
                    .function_calls
                    .values()
                    .filter(|call| call.complete_arguments.is_some())
                    .count();
                let incomplete_native_items = self
                    .function_calls
                    .len()
                    .saturating_sub(complete_native_items);
                let continuation_disposition = if incomplete_native_items == 0 {
                    ProviderOutputLimitContinuationDisposition::ContinueVisibleText
                } else {
                    ProviderOutputLimitContinuationDisposition::ReemitAtomicNativeCall
                };
                Err(
                    ProviderResponseError::invalid_state(openai_stream_event_error_detail(
                        &value,
                        "OpenAI stream returned an incomplete response",
                    ))
                    .with_provider_failure_json(openai_provider_failure_event_json(&value))
                    .with_output_limit_state(ProviderOutputLimitState::new(
                        "openai",
                        "responses",
                        stop_reason,
                        value
                            .pointer("/response/id")
                            .or_else(|| value.get("response_id"))
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        safe_partial_text,
                        complete_native_items,
                        incomplete_native_items,
                        self.usage,
                        continuation_disposition,
                    )),
                )
            }
            "message" | "" => {
                if let Some(text) = value.get("output_text").and_then(serde_json::Value::as_str) {
                    self.output_item_text.push_str(text);
                } else if let Some(text) = collect_openai_output_text(&value) {
                    self.output_item_text.push_str(&text);
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// Finalizes the accumulated stream into the existing response projection.
    pub fn finish(
        self,
        fallback_model: &str,
    ) -> ProviderResponseResult<(
        String,
        String,
        ModelTokenUsage,
        Vec<ProviderTranscriptEvent>,
    )> {
        if !self.completed {
            return Err(ProviderResponseError::invalid_state(
                "OpenAI stream closed before response.completed",
            ));
        }
        let output_item_text_empty = self.output_item_text.is_empty();
        let raw_text = if let Some(arguments) =
            collect_openai_maap_function_call_arguments_from_accumulators(&self.function_calls)?
        {
            arguments
        } else if output_item_text_empty {
            self.delta_text
        } else {
            self.output_item_text
        };
        if raw_text.is_empty() {
            return Err(ProviderResponseError::invalid_state(
                "OpenAI stream did not contain text or MAAP function-call output",
            ));
        }
        let native_output = self
            .completed_response_output
            .unwrap_or_else(|| self.completed_output_items.into_values().collect());
        let provider_transcript_events =
            ProviderTranscriptEvent::validated_openai_response_output(native_output)
                .into_iter()
                .collect();
        Ok((
            self.model.unwrap_or_else(|| fallback_model.to_string()),
            raw_text,
            self.usage,
            provider_transcript_events,
        ))
    }
}

/// Finalizes an already incrementally decoded OpenAI SSE event sequence.
pub fn parse_openai_responses_stream_events(
    events: impl IntoIterator<Item = crate::SseEvent>,
    fallback_model: &str,
) -> ProviderResponseResult<(
    String,
    String,
    ModelTokenUsage,
    Vec<ProviderTranscriptEvent>,
)> {
    let mut decoder = OpenAiResponsesStreamDecoder::default();
    for event in events {
        let _ = decoder.push_event(&event)?;
    }
    decoder.finish(fallback_model)
}

/// Parses one streaming OpenAI Responses API SSE body.
pub fn parse_openai_responses_stream_body(
    body: &str,
    fallback_model: &str,
) -> ProviderResponseResult<(
    String,
    String,
    ModelTokenUsage,
    Vec<ProviderTranscriptEvent>,
)> {
    let mut model = None;
    let mut completed = false;
    let mut usage = ModelTokenUsage::default();
    let mut function_calls = BTreeMap::<u64, OpenAiFunctionCallAccumulator>::new();
    let mut completed_output_items = BTreeMap::<u64, serde_json::Value>::new();
    let mut completed_response_output = None;
    let mut output_item_text = String::new();
    let mut delta_text = String::new();

    parse_sse_events_with(
        body,
        "OpenAI stream response did not contain SSE data events",
        |event_name, data| {
            let data = data.trim();
            if data == "[DONE]" {
                return Ok(());
            }
            let value: serde_json::Value = serde_json::from_str(data).map_err(|error| {
                ProviderResponseError::invalid_state(format!(
                    "OpenAI stream event was not JSON: {error}"
                ))
            })?;
            if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
                let message = error
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("OpenAI stream contained an error");
                let message = sanitize_provider_primary_error_text(message);
                return Err(ProviderResponseError::invalid_state(message)
                    .with_provider_failure_json(openai_provider_failure_event_json(&value)));
            }
            let event_usage = openai_token_usage_from_response_value(&value);
            if !event_usage.is_zero() {
                usage = event_usage;
            }

            let event_type = value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .or(event_name)
                .unwrap_or_default();
            if model.is_none() {
                model = value
                    .get("response")
                    .and_then(|response| response.get("model"))
                    .or_else(|| value.get("model"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
            }

            match event_type {
                "response.output_item.done" | "response.output_item.added" => {
                    if let Some(item) = value.get("item") {
                        collect_openai_maap_function_call_event_item(
                            &mut function_calls,
                            &value,
                            item,
                        )?;
                        append_openai_response_item_text(item, &mut output_item_text);
                        if event_type == "response.output_item.done" {
                            let output_index = openai_output_index(&value).unwrap_or_default();
                            completed_output_items.insert(output_index, item.clone());
                        }
                    }
                }
                "response.output_text.delta" => {
                    if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                        delta_text.push_str(delta);
                    }
                }
                "response.function_call_arguments.delta" => {
                    if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                        let output_index = openai_output_index(&value).unwrap_or_default();
                        push_openai_function_call_argument_delta(
                            function_calls.entry(output_index).or_default(),
                            delta,
                        )?;
                    }
                }
                "response.function_call_arguments.done" => {
                    let output_index = openai_output_index(&value).unwrap_or_default();
                    if let Some(item) = value.get("item") {
                        collect_openai_maap_function_call_event_item(
                            &mut function_calls,
                            &value,
                            item,
                        )?;
                    }
                    if let Some(arguments) = value
                        .get("arguments")
                        .and_then(serde_json::Value::as_str)
                        .or_else(|| value.get("item").and_then(openai_function_call_arguments))
                    {
                        set_openai_function_call_complete_arguments(
                            function_calls.entry(output_index).or_default(),
                            arguments,
                        )?;
                    }
                }
                "response.completed" => {
                    completed = true;
                    completed_response_output = value
                        .pointer("/response/output")
                        .and_then(serde_json::Value::as_array)
                        .cloned();
                }
                "response.failed" => {
                    return Err(ProviderResponseError::invalid_state(
                        openai_stream_event_error_detail(&value, "OpenAI stream failed"),
                    )
                    .with_provider_failure_json(openai_provider_failure_event_json(&value)));
                }
                "response.incomplete" => {
                    let stop_reason = value
                        .pointer("/response/incomplete_details/reason")
                        .or_else(|| value.pointer("/incomplete_details/reason"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    let safe_partial_text = if output_item_text.is_empty() {
                        delta_text.clone()
                    } else {
                        output_item_text.clone()
                    };
                    let complete_native_items = function_calls
                        .values()
                        .filter(|call| call.complete_arguments.is_some())
                        .count();
                    let incomplete_native_items =
                        function_calls.len().saturating_sub(complete_native_items);
                    let continuation_disposition = if incomplete_native_items == 0 {
                        ProviderOutputLimitContinuationDisposition::ContinueVisibleText
                    } else {
                        ProviderOutputLimitContinuationDisposition::ReemitAtomicNativeCall
                    };
                    return Err(ProviderResponseError::invalid_state(
                        openai_stream_event_error_detail(
                            &value,
                            "OpenAI stream returned an incomplete response",
                        ),
                    )
                    .with_provider_failure_json(openai_provider_failure_event_json(&value))
                    .with_output_limit_state(ProviderOutputLimitState::new(
                        "openai",
                        "responses",
                        stop_reason,
                        value
                            .pointer("/response/id")
                            .or_else(|| value.get("response_id"))
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        safe_partial_text,
                        complete_native_items,
                        incomplete_native_items,
                        usage,
                        continuation_disposition,
                    )));
                }
                "message" | "" => {
                    if let Some(text) = value.get("output_text").and_then(serde_json::Value::as_str)
                    {
                        output_item_text.push_str(text);
                    } else if let Some(text) = collect_openai_output_text(&value) {
                        output_item_text.push_str(&text);
                    }
                }
                _ => {}
            }
            Ok(())
        },
    )?;

    let output_item_text_empty = output_item_text.is_empty();
    if !completed {
        return Err(ProviderResponseError::invalid_state(
            "OpenAI stream closed before response.completed",
        ));
    }
    let raw_text = if let Some(arguments) =
        collect_openai_maap_function_call_arguments_from_accumulators(&function_calls)?
    {
        arguments
    } else if output_item_text_empty {
        delta_text
    } else {
        output_item_text
    };
    if raw_text.is_empty() {
        return Err(ProviderResponseError::invalid_state(
            "OpenAI stream did not contain text or MAAP function-call output",
        ));
    }
    let native_output =
        completed_response_output.unwrap_or_else(|| completed_output_items.into_values().collect());
    let provider_transcript_events =
        ProviderTranscriptEvent::validated_openai_response_output(native_output)
            .into_iter()
            .collect();
    Ok((
        model.unwrap_or_else(|| fallback_model.to_string()),
        raw_text,
        usage,
        provider_transcript_events,
    ))
}

/// Captures one complete non-streaming Responses output sequence when valid.
fn openai_response_output_event(value: &serde_json::Value) -> Vec<ProviderTranscriptEvent> {
    value
        .get("output")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .and_then(ProviderTranscriptEvent::validated_openai_response_output)
        .into_iter()
        .collect()
}

/// Extracts OpenAI-style token usage from a response or stream event object.
fn openai_token_usage_from_response_value(value: &serde_json::Value) -> ModelTokenUsage {
    let Some(usage) = value
        .get("usage")
        .or_else(|| value.pointer("/response/usage"))
    else {
        return ModelTokenUsage::default();
    };
    ModelTokenUsage {
        input_tokens: openai_usage_u64(usage, &["/input_tokens", "/prompt_tokens"]),
        output_tokens: openai_usage_u64(usage, &["/output_tokens", "/completion_tokens"]),
        reasoning_tokens: openai_usage_u64(
            usage,
            &[
                "/output_tokens_details/reasoning_tokens",
                "/completion_tokens_details/reasoning_tokens",
                "/reasoning_tokens",
            ],
        ),
        cached_input_tokens: openai_cached_input_tokens(usage),
        cache_write_input_tokens: openai_cache_write_input_tokens(usage),
    }
}

/// Returns the first unsigned integer found at one of the supplied JSON paths.
fn openai_usage_u64(value: &serde_json::Value, pointers: &[&str]) -> u64 {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(serde_json::Value::as_u64))
        .unwrap_or(0)
}

/// Returns cached input token accounting across OpenAI-compatible usage shapes.
fn openai_cached_input_tokens(value: &serde_json::Value) -> Option<u64> {
    [
        "/input_tokens_details/cached_tokens",
        "/prompt_tokens_details/cached_tokens",
        "/input_token_details/cached_tokens",
        "/prompt_token_details/cached_tokens",
        "/cached_input_tokens",
        "/cached_prompt_tokens",
        "/cached_tokens",
    ]
    .iter()
    .find_map(|pointer| value.pointer(pointer).and_then(serde_json::Value::as_u64))
}

/// Returns inclusive OpenAI cache-write accounting from Responses usage.
///
/// OpenAI reports writes as a subset of `input_tokens`, so callers retain this
/// detail for observability while the shared accounting total does not add it
/// again.
fn openai_cache_write_input_tokens(value: &serde_json::Value) -> Option<u64> {
    [
        "/input_tokens_details/cache_write_tokens",
        "/prompt_tokens_details/cache_write_tokens",
    ]
    .iter()
    .find_map(|pointer| value.pointer(pointer).and_then(serde_json::Value::as_u64))
}

/// Returns a human-readable error detail from an OpenAI stream event.
fn openai_stream_event_error_detail(value: &serde_json::Value, fallback: &str) -> String {
    value
        .pointer("/response/error/message")
        .or_else(|| value.pointer("/response/incomplete_details/reason"))
        .or_else(|| value.pointer("/error/message"))
        .or_else(|| value.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(|message| {
            format!(
                "{fallback}: {}",
                sanitize_provider_primary_error_text(message)
            )
        })
        .unwrap_or_else(|| fallback.to_string())
}

/// Collects output text chunks from a Responses API output array.
fn collect_openai_output_text(value: &serde_json::Value) -> Option<String> {
    let mut text = String::new();
    for item in value.get("output")?.as_array()? {
        append_openai_response_item_text(item, &mut text);
    }
    if text.is_empty() { None } else { Some(text) }
}

/// Appends text fragments from one Responses API output item into the caller buffer.
fn append_openai_response_item_text(item: &serde_json::Value, output: &mut String) {
    let Some(content) = item.get("content").and_then(serde_json::Value::as_array) else {
        return;
    };
    for content_item in content {
        let item_type = content_item
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if matches!(item_type, "output_text" | "text")
            && let Some(text) = content_item.get("text").and_then(serde_json::Value::as_str)
        {
            output.push_str(text);
        }
    }
}

/// Accumulates streaming OpenAI function-call state for one output index.
#[derive(Debug, Default)]
struct OpenAiFunctionCallAccumulator {
    /// Function name reported by the provider.
    name: Option<String>,
    /// Incrementally accumulated argument text.
    arguments: String,
    /// Complete argument text when the provider reports a completed snapshot.
    complete_arguments: Option<String>,
}

/// Collects native MAAP function-call arguments from a non-streaming response.
fn collect_openai_maap_function_call_arguments(
    value: &serde_json::Value,
) -> ProviderResponseResult<Option<String>> {
    let Some(output) = value.get("output").and_then(serde_json::Value::as_array) else {
        return Ok(None);
    };
    let arguments = output
        .iter()
        .filter(|item| {
            item.get("type").and_then(serde_json::Value::as_str) == Some("function_call")
        })
        .filter(|item| {
            openai_function_call_name(item).is_some_and(openai_function_call_name_is_maap)
        })
        .map(|item| {
            let arguments = openai_function_call_arguments(item).ok_or_else(|| {
                ProviderResponseError::invalid_state(
                    "OpenAI MAAP function call did not contain arguments",
                )
            })?;
            openai_function_call_arguments_string(arguments)
        })
        .collect::<ProviderResponseResult<Vec<_>>>()?;
    one_openai_maap_function_call_arguments(arguments)
}

/// Accumulates one streaming function-call item into the indexed call map.
fn collect_openai_maap_function_call_event_item(
    function_calls: &mut BTreeMap<u64, OpenAiFunctionCallAccumulator>,
    event: &serde_json::Value,
    item: &serde_json::Value,
) -> ProviderResponseResult<()> {
    if item.get("type").and_then(serde_json::Value::as_str) != Some("function_call") {
        return Ok(());
    }
    let output_index = openai_output_index(event).unwrap_or_default();
    let entry = function_calls.entry(output_index).or_default();
    if let Some(name) = openai_function_call_name(item) {
        entry.name = Some(name.to_string());
    }
    if let Some(arguments) = openai_function_call_arguments(item)
        && !arguments.is_empty()
    {
        set_openai_function_call_complete_arguments(entry, arguments)?;
    }
    Ok(())
}

/// Collects the final MAAP arguments from completed streaming accumulators.
fn collect_openai_maap_function_call_arguments_from_accumulators(
    function_calls: &BTreeMap<u64, OpenAiFunctionCallAccumulator>,
) -> ProviderResponseResult<Option<String>> {
    let arguments = function_calls
        .values()
        .filter(|call| {
            call.name
                .as_deref()
                .is_none_or(openai_function_call_name_is_maap)
        })
        .filter_map(|call| {
            let delta_arguments = if call.arguments.is_empty() {
                None
            } else {
                Some(&call.arguments)
            };
            call.complete_arguments
                .as_ref()
                .filter(|arguments| !arguments.is_empty())
                .or(delta_arguments)
                .cloned()
        })
        .collect::<Vec<_>>();
    one_openai_maap_function_call_arguments(arguments)
}

/// Reports whether an OpenAI function call name is a Mezzanine MAAP carrier.
fn openai_function_call_name_is_maap(name: &str) -> bool {
    name == OPENAI_MAAP_FUNCTION_TOOL_NAME
        || name == OpenAiMaapToolSurface::CurrentRequest.tool_name()
        || OpenAiMaapToolSurface::stable_surfaces()
            .iter()
            .any(|surface| name == surface.tool_name())
}

/// Appends or replaces streaming function-call arguments without unbounded growth.
///
/// Some Responses streaming paths send true deltas, while others send
/// cumulative snapshots in the `delta` field. Replacing when the new value
/// contains the previous buffer as a prefix keeps both shapes correct and
/// prevents repeated snapshots from growing memory without bound.
fn push_openai_function_call_argument_delta(
    call: &mut OpenAiFunctionCallAccumulator,
    delta: &str,
) -> ProviderResponseResult<()> {
    if delta.is_empty() {
        return Ok(());
    }
    if !call.arguments.is_empty() && delta.starts_with(&call.arguments) {
        call.arguments.clear();
        call.arguments.push_str(delta);
    } else {
        call.arguments.push_str(delta);
    }
    validate_openai_function_call_argument_size(&call.arguments)
}

/// Stores complete function-call arguments after enforcing the provider cap.
fn set_openai_function_call_complete_arguments(
    call: &mut OpenAiFunctionCallAccumulator,
    arguments: &str,
) -> ProviderResponseResult<()> {
    validate_openai_function_call_argument_size(arguments)?;
    call.complete_arguments = Some(arguments.to_string());
    Ok(())
}

/// Copies function-call arguments only after enforcing the provider cap.
fn openai_function_call_arguments_string(arguments: &str) -> ProviderResponseResult<String> {
    validate_openai_function_call_argument_size(arguments)?;
    Ok(arguments.to_string())
}

/// Rejects oversized native MAAP argument buffers before they can dominate memory.
fn validate_openai_function_call_argument_size(arguments: &str) -> ProviderResponseResult<()> {
    if arguments.len() > OPENAI_FUNCTION_CALL_ARGUMENT_LIMIT_BYTES {
        return Err(ProviderResponseError::invalid_state(format!(
            "OpenAI MAAP function call arguments exceeded {} bytes",
            OPENAI_FUNCTION_CALL_ARGUMENT_LIMIT_BYTES
        )));
    }
    Ok(())
}

/// Returns zero, one, or an error for collected native MAAP argument buffers.
fn one_openai_maap_function_call_arguments(
    arguments: Vec<String>,
) -> ProviderResponseResult<Option<String>> {
    match arguments.len() {
        0 => Ok(None),
        1 => Ok(arguments.into_iter().next()),
        _ => Err(ProviderResponseError::invalid_state(
            "OpenAI response contained multiple MAAP function calls in one turn",
        )),
    }
}

/// Returns the function-call name from supported OpenAI response item shapes.
fn openai_function_call_name(item: &serde_json::Value) -> Option<&str> {
    item.get("name")
        .or_else(|| item.pointer("/function/name"))
        .and_then(serde_json::Value::as_str)
}

/// Returns function-call arguments from supported OpenAI response item shapes.
fn openai_function_call_arguments(item: &serde_json::Value) -> Option<&str> {
    item.get("arguments")
        .or_else(|| item.pointer("/function/arguments"))
        .and_then(serde_json::Value::as_str)
}

/// Returns the response output index for streaming function-call events.
fn openai_output_index(value: &serde_json::Value) -> Option<u64> {
    value
        .get("output_index")
        .and_then(serde_json::Value::as_u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies unary Responses parsing preserves the complete ordered native
    /// output sequence needed by a stateless follow-up request.
    ///
    /// Encrypted reasoning, assistant phase, item identities, and function-call
    /// identity are provider-only continuity state and must survive unchanged.
    #[test]
    fn openai_http_parser_preserves_native_output_continuity() {
        let items = vec![
            serde_json::json!({
                "type": "reasoning",
                "id": "rs_1",
                "encrypted_content": "opaque-ciphertext"
            }),
            serde_json::json!({
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "phase": "commentary",
                "content": [{"type": "output_text", "text": "working"}]
            }),
            serde_json::json!({
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "submit_maap_action_batch",
                "arguments": "{\"actions\":[]}"
            }),
        ];
        let body = serde_json::json!({
            "model": "gpt-test",
            "output": items
        })
        .to_string();

        let (_, raw_text, _, events) = parse_openai_responses_http_body(&body, "fallback").unwrap();

        assert_eq!(raw_text, "{\"actions\":[]}");
        assert_eq!(
            events,
            vec![ProviderTranscriptEvent::OpenAiResponseOutput { items }]
        );
    }

    /// Verifies unary incomplete Responses never promote syntactically complete
    /// native arguments into executable output or durable provider continuity.
    #[test]
    fn openai_http_parser_rejects_incomplete_native_output() {
        let body = serde_json::json!({
            "id": "resp_cutoff",
            "model": "gpt-test",
            "status": "incomplete",
            "incomplete_details": { "reason": "max_output_tokens" },
            "usage": { "output_tokens": 12 },
            "output": [{
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "submit_maap_action_batch",
                "arguments": "{\"actions\":[]}"
            }]
        })
        .to_string();

        let error = parse_openai_responses_http_body(&body, "fallback").unwrap_err();

        assert_eq!(error.kind(), ProviderResponseErrorKind::InvalidState);
        assert!(error.message().contains("max_output_tokens"), "{error}");
        let state = error.output_limit_state().expect("output-limit state");
        assert_eq!(state.stop_reason, "max_output_tokens");
        assert_eq!(state.response_id.as_deref(), Some("resp_cutoff"));
        assert_eq!(state.complete_native_items, 0);
        assert_eq!(state.incomplete_native_items, 1);
        assert_eq!(
            state.continuation_disposition,
            ProviderOutputLimitContinuationDisposition::ReemitAtomicNativeCall
        );
    }

    /// Verifies a truncated function-call item is reemitted atomically even
    /// when the provider has not emitted its arguments field yet.
    #[test]
    fn openai_http_parser_rejects_incomplete_native_output_without_arguments() {
        let body = serde_json::json!({
            "id": "resp_cutoff_before_arguments",
            "model": "gpt-test",
            "status": "incomplete",
            "incomplete_details": { "reason": "max_output_tokens" },
            "output": [{
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "submit_maap_action_batch"
            }]
        })
        .to_string();

        let error = parse_openai_responses_http_body(&body, "fallback").unwrap_err();

        let state = error.output_limit_state().expect("output-limit state");
        assert_eq!(state.complete_native_items, 0);
        assert_eq!(state.incomplete_native_items, 1);
        assert_eq!(
            state.continuation_disposition,
            ProviderOutputLimitContinuationDisposition::ReemitAtomicNativeCall
        );
    }

    /// Verifies unary incomplete status details cannot expose provider-authored
    /// credential-shaped text through diagnostics or retained continuation state.
    #[test]
    fn openai_http_parser_sanitizes_incomplete_stop_reason() {
        const SENTINEL: &str = "sk-proj-OPENAIUNARYINCOMPLETESENTINEL000";
        let body = serde_json::json!({
            "id": "resp_incomplete",
            "model": "gpt-test",
            "status": "incomplete",
            "incomplete_details": {
                "reason": format!("cutoff Bearer {SENTINEL}")
            }
        })
        .to_string();

        let error = parse_openai_responses_http_body(&body, "fallback").unwrap_err();

        assert!(!error.message().contains(SENTINEL), "{}", error.message());
        assert!(!format!("{error:?}").contains(SENTINEL));
        assert!(!error.to_string().contains(SENTINEL));
        assert_eq!(
            error
                .output_limit_state()
                .expect("output-limit state")
                .stop_reason,
            "[REDACTED]"
        );
        let failure: serde_json::Value =
            serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
        assert!(!failure.to_string().contains(SENTINEL), "{failure}");
    }

    /// Verifies unary Responses statuses other than `completed` cannot promote
    /// action-bearing output after a provider reports failure or remains active.
    #[test]
    fn openai_http_parser_rejects_nonterminal_and_failed_native_output() {
        for status in ["failed", "cancelled", "in_progress"] {
            let body = serde_json::json!({
                "model": "gpt-test",
                "status": status,
                "output": [{
                    "type": "function_call",
                    "name": "submit_maap_action_batch",
                    "arguments": "{\"actions\":[] }"
                }]
            })
            .to_string();

            let error = parse_openai_responses_http_body(&body, "fallback").unwrap_err();
            assert_eq!(error.kind(), ProviderResponseErrorKind::InvalidState);
            assert_eq!(error.message(), "OpenAI response did not complete");
        }
    }

    /// Verifies streaming Responses parsing prefers the completed response
    /// snapshot and preserves its provider-native item order exactly.
    ///
    /// Incremental events may be fragmented or duplicated, so replay must use
    /// the authoritative completed output sequence when the provider supplies it.
    #[test]
    fn openai_stream_parser_preserves_completed_native_output_continuity() {
        let items = vec![
            serde_json::json!({
                "type": "reasoning",
                "id": "rs_1",
                "encrypted_content": "opaque-ciphertext"
            }),
            serde_json::json!({
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "submit_maap_action_batch",
                "arguments": "{\"actions\":[]}"
            }),
        ];
        let stream_body = format!(
            "event: response.output_item.done\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
            serde_json::json!({
                "type": "response.output_item.done",
                "output_index": 1,
                "item": items[1]
            }),
            serde_json::json!({
                "type": "response.completed",
                "response": {
                    "model": "gpt-test",
                    "output": items
                }
            })
        );

        let (_, raw_text, _, events) =
            parse_openai_responses_stream_body(&stream_body, "fallback").unwrap();

        assert_eq!(raw_text, "{\"actions\":[]}");
        assert_eq!(
            events,
            vec![ProviderTranscriptEvent::OpenAiResponseOutput { items }]
        );
    }

    /// Verifies incremental decoding rejects function-call output when the SSE
    /// stream ends before the provider declares a completed response.
    #[test]
    fn openai_stream_decoder_rejects_native_output_without_completion() {
        let event = crate::SseEvent {
            name: Some("response.output_item.done".to_string()),
            data: serde_json::json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "fc_cutoff",
                    "call_id": "call_cutoff",
                    "name": "submit_maap_action_batch",
                    "arguments": "{\"actions\":[]}"
                }
            })
            .to_string(),
        };
        let mut decoder = OpenAiResponsesStreamDecoder::default();

        decoder.push_event(&event).unwrap();
        let error = decoder.finish("gpt-test").unwrap_err();

        assert_eq!(error.kind(), ProviderResponseErrorKind::InvalidState);
        assert_eq!(
            error.message(),
            "OpenAI stream closed before response.completed"
        );
    }

    /// Verifies buffered SSE parsing rejects visible output when EOF arrives
    /// without a terminal `response.completed` event.
    #[test]
    fn openai_stream_body_rejects_text_without_completion() {
        let body = format!(
            "event: response.output_item.done\ndata: {}\n\n",
            serde_json::json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "partial output"}]
                }
            })
        );

        let error = parse_openai_responses_stream_body(&body, "gpt-test").unwrap_err();

        assert_eq!(error.kind(), ProviderResponseErrorKind::InvalidState);
        assert_eq!(
            error.message(),
            "OpenAI stream closed before response.completed"
        );
    }

    /// Verifies a terminal transport sentinel cannot promote native output
    /// without the authoritative `response.completed` provider event.
    #[test]
    fn openai_stream_decoder_rejects_done_without_response_completed() {
        let output = crate::SseEvent {
            name: Some("response.output_item.done".to_string()),
            data: serde_json::json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "id": "fc_done",
                    "call_id": "call_done",
                    "name": "submit_maap_action_batch",
                    "arguments": "{\"actions\":[]}"
                }
            })
            .to_string(),
        };
        let done = crate::SseEvent {
            name: None,
            data: "[DONE]".to_string(),
        };
        let mut decoder = OpenAiResponsesStreamDecoder::default();

        decoder.push_event(&output).unwrap();
        decoder.push_event(&done).unwrap();
        let error = decoder.finish("gpt-test").unwrap_err();

        assert_eq!(
            error.message(),
            "OpenAI stream closed before response.completed"
        );
    }

    /// Verifies buffered parsing treats `[DONE]` as transport termination, not
    /// as a completed Responses result that can promote visible text.
    #[test]
    fn openai_stream_body_rejects_done_without_response_completed() {
        let body = format!(
            "event: response.output_item.done\ndata: {}\n\ndata: [DONE]\n\n",
            serde_json::json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "partial output"}]
                }
            })
        );

        let error = parse_openai_responses_stream_body(&body, "gpt-test").unwrap_err();

        assert_eq!(
            error.message(),
            "OpenAI stream closed before response.completed"
        );
    }

    #[test]
    /// Verifies cached-token accounting distinguishes omitted provider fields from
    /// an explicit provider-reported zero.
    fn openai_response_parser_distinguishes_missing_and_zero_cached_tokens() {
        let missing_body = serde_json::json!({
            "model": "gpt-test",
            "usage": {
                "input_tokens": 42,
                "output_tokens": 11
            },
            "output_text": "ok"
        })
        .to_string();
        let zero_body = serde_json::json!({
            "model": "gpt-test",
            "usage": {
                "input_tokens": 42,
                "output_tokens": 11,
                "input_tokens_details": {
                    "cached_tokens": 0,
                    "cache_write_tokens": 0
                }
            },
            "output_text": "ok"
        })
        .to_string();
        let prompt_details_body = serde_json::json!({
            "model": "gpt-test",
            "usage": {
                "prompt_tokens": 42,
                "completion_tokens": 11,
                "prompt_tokens_details": {
                    "cached_tokens": 24
                }
            },
            "output_text": "ok"
        })
        .to_string();
        let controller_alias_body = serde_json::json!({
            "model": "gpt-test",
            "usage": {
                "input_tokens": 42,
                "output_tokens": 11,
                "cached_tokens": 0,
                "cached_input_tokens": 36
            },
            "output_text": "ok"
        })
        .to_string();
        let multi_cached_body = serde_json::json!({
            "model": "gpt-test",
            "usage": {
                "input_tokens": 42,
                "output_tokens": 11,
                "input_tokens_details": {
                    "cached_tokens": 12,
                    "cache_write_tokens": 9
                },
                "prompt_tokens_details": {
                    "cached_tokens": 8
                },
                "cached_input_tokens": 5
            },
            "output_text": "ok"
        })
        .to_string();
        let stream_body = format!(
            "event: response.output_item.done\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
            serde_json::json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "ok"}]
                }
            }),
            serde_json::json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_1",
                    "model": "gpt-test",
                    "usage": {
                        "input_tokens": 42,
                        "output_tokens": 11,
                        "input_tokens_details": {
                            "cached_tokens": 12
                        }
                    }
                }
            })
        );

        let (_, _, missing_usage, _) =
            parse_openai_responses_http_body(&missing_body, "gpt-test").unwrap();
        let (_, _, zero_usage, _) =
            parse_openai_responses_http_body(&zero_body, "gpt-test").unwrap();
        let (_, _, prompt_details_usage, _) =
            parse_openai_responses_http_body(&prompt_details_body, "gpt-test").unwrap();
        let (_, _, controller_alias_usage, _) =
            parse_openai_responses_http_body(&controller_alias_body, "gpt-test").unwrap();
        let (_, _, multi_cached_usage, _) =
            parse_openai_responses_http_body(&multi_cached_body, "gpt-test").unwrap();
        let (_, _, stream_usage, _) =
            parse_openai_responses_stream_body(&stream_body, "gpt-test").unwrap();

        assert_eq!(missing_usage.cached_input_tokens, None);
        assert_eq!(missing_usage.cached_input_tokens_display(), "unknown");
        assert_eq!(missing_usage.cached_input_hit_ratio_display(), "unknown");
        assert_eq!(zero_usage.cached_input_tokens, Some(0));
        assert_eq!(zero_usage.cache_write_input_tokens, Some(0));
        assert_eq!(zero_usage.cached_input_tokens_display(), "0");
        assert_eq!(zero_usage.cached_input_hit_ratio_display(), "0.00%");
        assert_eq!(prompt_details_usage.cached_input_tokens, Some(24));
        assert_eq!(
            prompt_details_usage.cached_input_hit_ratio_display(),
            "57.14%"
        );
        assert_eq!(controller_alias_usage.cached_input_tokens, Some(36));
        assert_eq!(multi_cached_usage.cached_input_tokens, Some(12));
        assert_eq!(multi_cached_usage.cache_write_input_tokens, Some(9));
        assert_eq!(stream_usage.cached_input_tokens, Some(12));
        assert_eq!(stream_usage.cache_write_input_tokens, None);
    }

    #[test]
    /// Verifies an OpenAI output cutoff retains safe visible text and marks
    /// streamed function-argument deltas as non-executable incomplete calls.
    fn openai_stream_output_limit_retains_safe_partial_state() {
        let stream_body = concat!(
            r#"event: response.output_text.delta
"#,
            r#"data: {"type":"response.output_text.delta","delta":"partial visible text"}

"#,
            r#"event: response.function_call_arguments.delta
"#,
            r#"data: {"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"actions\":[]"}

"#,
            r#"event: response.incomplete
"#,
            r#"data: {"type":"response.incomplete","response":{"id":"resp_cutoff","incomplete_details":{"reason":"max_output_tokens"},"usage":{"output_tokens":12}}}

"#
        );

        let error = parse_openai_responses_stream_body(stream_body, "gpt-test").unwrap_err();
        let state = error.output_limit_state().expect("output-limit state");
        assert_eq!(state.provider, "openai");
        assert_eq!(state.api, "responses");
        assert_eq!(state.stop_reason, "max_output_tokens");
        assert_eq!(state.response_id.as_deref(), Some("resp_cutoff"));
        assert_eq!(state.safe_partial_text, "partial visible text");
        assert_eq!(state.complete_native_items, 0);
        assert_eq!(state.incomplete_native_items, 1);
        assert_eq!(state.usage.output_tokens, 12);
        assert_eq!(
            state.continuation_disposition,
            ProviderOutputLimitContinuationDisposition::ReemitAtomicNativeCall
        );
    }

    #[test]
    /// Verifies OpenAI response parsing reports API errors and missing text.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    fn openai_response_parser_reports_api_errors_and_missing_text() {
        let error =
            parse_openai_responses_http_body(r#"{"error":{"message":"bad auth"}}"#, "gpt-test")
                .unwrap_err();
        assert_eq!(error.kind(), ProviderResponseErrorKind::InvalidState);
        assert!(error.message().contains("bad auth"));

        let missing =
            parse_openai_responses_http_body(r#"{"model":"gpt-test","output":[]}"#, "gpt-test")
                .unwrap_err();
        assert_eq!(missing.kind(), ProviderResponseErrorKind::InvalidState);
    }

    /// Verifies the unary Responses error path withholds provider-authored
    /// credential-shaped error text while retaining safe error identity.
    ///
    /// A provider error body is remote input. A bearer-token or API-key shape
    /// embedded in its message must never reach the error message, `Display`,
    /// `Debug`, or the structured failure payload, while the safe error type and
    /// code must survive the same boundary so retry and auth handling stay
    /// correct.
    #[test]
    fn openai_unary_provider_error_text_is_sanitized() {
        const SENTINEL: &str = "sk-proj-OPENAIUNARYSENTINEL00000000";
        let body = serde_json::json!({
            "error": {
                "type": "invalid_request_error",
                "code": "invalid_api_key",
                "message": format!("invalid api key: Bearer {SENTINEL}")
            }
        })
        .to_string();

        let error = parse_openai_responses_http_body(&body, "gpt-test").unwrap_err();

        assert_eq!(error.kind(), ProviderResponseErrorKind::InvalidState);
        assert_eq!(error.message(), "[REDACTED]");
        assert!(!error.message().contains(SENTINEL));
        assert!(!format!("{error:?}").contains(SENTINEL));
        assert!(!error.to_string().contains(SENTINEL));
        let failure: serde_json::Value =
            serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
        assert_eq!(failure["error"]["type"], "invalid_request_error");
        assert_eq!(failure["error"]["code"], "invalid_api_key");
        assert!(!failure.to_string().contains(SENTINEL), "{failure}");
    }

    /// Verifies the incremental Responses SSE decoder withholds
    /// provider-authored credential-shaped error text from an `error` event.
    ///
    /// The decoder is the streaming counterpart of the unary parser, so the same
    /// remote-message boundary must apply before the message can reach the error
    /// envelope, trace, or audit surfaces.
    #[test]
    fn openai_stream_decoder_error_text_is_sanitized() {
        const SENTINEL: &str = "sk-proj-OPENAIDECODERSENTINEL0000000";
        let event = crate::SseEvent {
            name: Some("error".to_string()),
            data: serde_json::json!({
                "error": {
                    "type": "server_error",
                    "code": "internal_error",
                    "message": format!("upstream failure; api_key={SENTINEL}")
                }
            })
            .to_string(),
        };
        let mut decoder = OpenAiResponsesStreamDecoder::default();

        let error = decoder.push_event(&event).unwrap_err();

        assert_eq!(error.kind(), ProviderResponseErrorKind::InvalidState);
        assert!(!error.message().contains(SENTINEL), "{}", error.message());
        assert!(!format!("{error:?}").contains(SENTINEL));
        assert!(!error.to_string().contains(SENTINEL));
        let failure: serde_json::Value =
            serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
        assert_eq!(failure["error"]["type"], "server_error");
        assert_eq!(failure["error"]["code"], "internal_error");
        assert!(!failure.to_string().contains(SENTINEL), "{failure}");
    }

    /// Verifies the whole-body Responses SSE parser withholds provider-authored
    /// credential-shaped error text from an `error` event.
    ///
    /// The whole-body parser is a distinct SSE entry point from the incremental
    /// decoder and the unary parser, so it must route provider-authored text
    /// through the shared diagnostics boundary before it reaches any rendered
    /// diagnostic.
    #[test]
    fn openai_stream_body_error_text_is_sanitized() {
        const SENTINEL: &str = "sk-proj-OPENAIBODYSENTINEL0000000000";
        let body = format!(
            "event: error\ndata: {}\n\n",
            serde_json::json!({
                "error": {
                    "type": "rate_limit_error",
                    "code": "rate_limit_exceeded",
                    "message": format!("rate limited: Authorization: Bearer {SENTINEL}")
                }
            })
        );

        let error = parse_openai_responses_stream_body(&body, "gpt-test").unwrap_err();

        assert_eq!(error.kind(), ProviderResponseErrorKind::InvalidState);
        assert_eq!(error.message(), "[REDACTED]");
        assert!(!format!("{error:?}").contains(SENTINEL));
        assert!(!error.to_string().contains(SENTINEL));
        let failure: serde_json::Value =
            serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
        assert_eq!(failure["error"]["type"], "rate_limit_error");
        assert!(!failure.to_string().contains(SENTINEL), "{failure}");
    }

    /// Verifies `response.failed` and `response.incomplete` events withhold
    /// provider-authored credential-shaped text from the display message, the
    /// structured failure payload, and retained continuation state.
    #[test]
    fn openai_stream_failed_and_incomplete_text_is_sanitized() {
        const FAILED_SENTINEL: &str = "sk-proj-OPENAIFAILEDSENTINEL0000000";
        let mut decoder = OpenAiResponsesStreamDecoder::default();
        let failed_event = crate::SseEvent {
            name: Some("response.failed".to_string()),
            data: serde_json::json!({
                "type": "response.failed",
                "response": {
                    "id": "resp_failed",
                    "error": {
                        "type": "server_error",
                        "code": "server_error",
                        "message": format!("server error; api_key={FAILED_SENTINEL}")
                    }
                }
            })
            .to_string(),
        };

        let failed = decoder.push_event(&failed_event).unwrap_err();

        assert!(
            !failed.message().contains(FAILED_SENTINEL),
            "{}",
            failed.message()
        );
        assert!(!format!("{failed:?}").contains(FAILED_SENTINEL));
        assert!(!failed.to_string().contains(FAILED_SENTINEL));
        let failure: serde_json::Value =
            serde_json::from_str(failed.provider_failure_json().unwrap()).unwrap();
        assert_eq!(failure["error"]["type"], "server_error");
        assert_eq!(failure["response_id"], "resp_failed");
        assert!(!failure.to_string().contains(FAILED_SENTINEL), "{failure}");

        const INCOMPLETE_SENTINEL: &str = "sk-proj-OPENAIINCOMPLETESENTINEL000";
        let incomplete_event = crate::SseEvent {
            name: Some("response.incomplete".to_string()),
            data: serde_json::json!({
                "type": "response.incomplete",
                "response": {
                    "id": "resp_incomplete",
                    "incomplete_details": {
                        "reason": format!("cutoff Bearer {INCOMPLETE_SENTINEL}")
                    }
                }
            })
            .to_string(),
        };

        let incomplete = decoder.push_event(&incomplete_event).unwrap_err();

        assert!(
            !incomplete.message().contains(INCOMPLETE_SENTINEL),
            "{}",
            incomplete.message()
        );
        assert!(!format!("{incomplete:?}").contains(INCOMPLETE_SENTINEL));
        assert!(!incomplete.to_string().contains(INCOMPLETE_SENTINEL));
        let state = incomplete.output_limit_state().expect("output-limit state");
        assert_eq!(state.stop_reason, "[REDACTED]");
        assert_eq!(state.response_id.as_deref(), Some("resp_incomplete"));
        let failure: serde_json::Value =
            serde_json::from_str(incomplete.provider_failure_json().unwrap()).unwrap();
        assert!(
            !failure.to_string().contains(INCOMPLETE_SENTINEL),
            "{failure}"
        );
    }
}
