//! OpenAI Responses API response parsing.
//!
//! This module owns HTTP and SSE response parsing for OpenAI-compatible
//! Responses API calls, including native MAAP function-call argument
//! accumulation and provider token-usage extraction.

use super::errors::openai_provider_failure_event_json;
use super::http::DEFAULT_PROVIDER_MAX_RESPONSE_BYTES;
use super::schema::OpenAiMaapToolSurface;
use super::{ModelTokenUsage, OPENAI_MAAP_FUNCTION_TOOL_NAME};
use crate::error::{MezError, Result};
use crate::sse::parse_sse_events_with;
use std::collections::BTreeMap;

/// Maximum native function-call argument bytes accepted from OpenAI responses.
const OPENAI_FUNCTION_CALL_ARGUMENT_LIMIT_BYTES: usize = DEFAULT_PROVIDER_MAX_RESPONSE_BYTES;

/// Selects the OpenAI response parser that matches the transport mode.
pub(super) fn parse_openai_responses_provider_body(
    body: &str,
    fallback_model: &str,
    stream: bool,
) -> Result<(String, String, ModelTokenUsage)> {
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
) -> Result<(String, String, ModelTokenUsage)> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|error| {
        MezError::invalid_state(format!("OpenAI response was not JSON: {error}"))
    })?;
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("OpenAI response contained an error");
        return Err(MezError::invalid_state(message)
            .with_provider_failure_json(openai_provider_failure_event_json(&value)));
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
        return Err(MezError::invalid_state(
            "OpenAI response did not contain text or MAAP function-call output",
        ));
    };
    let usage = openai_token_usage_from_response_value(&value);
    Ok((model, raw_text, usage))
}

/// Parses one streaming OpenAI Responses API SSE body.
pub fn parse_openai_responses_stream_body(
    body: &str,
    fallback_model: &str,
) -> Result<(String, String, ModelTokenUsage)> {
    let mut model = None;
    let mut completed = false;
    let mut usage = ModelTokenUsage::default();
    let mut function_calls = BTreeMap::<u64, OpenAiFunctionCallAccumulator>::new();
    let mut output_item_text = String::new();
    let mut delta_text = String::new();

    parse_sse_events_with(
        body,
        "OpenAI stream response did not contain SSE data events",
        |event_name, data| {
            let data = data.trim();
            if data == "[DONE]" {
                completed = true;
                return Ok(());
            }
            let value: serde_json::Value = serde_json::from_str(data).map_err(|error| {
                MezError::invalid_state(format!("OpenAI stream event was not JSON: {error}"))
            })?;
            if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
                let message = error
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("OpenAI stream contained an error");
                return Err(MezError::invalid_state(message)
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
                }
                "response.failed" => {
                    return Err(MezError::invalid_state(openai_stream_event_error_detail(
                        &value,
                        "OpenAI stream failed",
                    ))
                    .with_provider_failure_json(openai_provider_failure_event_json(&value)));
                }
                "response.incomplete" => {
                    return Err(MezError::invalid_state(openai_stream_event_error_detail(
                        &value,
                        "OpenAI stream returned an incomplete response",
                    ))
                    .with_provider_failure_json(openai_provider_failure_event_json(&value)));
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
        return Err(MezError::invalid_state(
            "OpenAI stream did not contain text or MAAP function-call output",
        ));
    }
    if !completed && output_item_text_empty && function_calls.is_empty() {
        return Err(MezError::invalid_state(
            "OpenAI stream closed before response.completed",
        ));
    }
    Ok((
        model.unwrap_or_else(|| fallback_model.to_string()),
        raw_text,
        usage,
    ))
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

/// Returns a human-readable error detail from an OpenAI stream event.
fn openai_stream_event_error_detail(value: &serde_json::Value, fallback: &str) -> String {
    value
        .pointer("/response/error/message")
        .or_else(|| value.pointer("/response/incomplete_details/reason"))
        .or_else(|| value.pointer("/error/message"))
        .or_else(|| value.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(|message| format!("{fallback}: {message}"))
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
) -> Result<Option<String>> {
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
                MezError::invalid_state("OpenAI MAAP function call did not contain arguments")
            })?;
            openai_function_call_arguments_string(arguments)
        })
        .collect::<Result<Vec<_>>>()?;
    one_openai_maap_function_call_arguments(arguments)
}

/// Accumulates one streaming function-call item into the indexed call map.
fn collect_openai_maap_function_call_event_item(
    function_calls: &mut BTreeMap<u64, OpenAiFunctionCallAccumulator>,
    event: &serde_json::Value,
    item: &serde_json::Value,
) -> Result<()> {
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
) -> Result<Option<String>> {
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
) -> Result<()> {
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
) -> Result<()> {
    validate_openai_function_call_argument_size(arguments)?;
    call.complete_arguments = Some(arguments.to_string());
    Ok(())
}

/// Copies function-call arguments only after enforcing the provider cap.
fn openai_function_call_arguments_string(arguments: &str) -> Result<String> {
    validate_openai_function_call_argument_size(arguments)?;
    Ok(arguments.to_string())
}

/// Rejects oversized native MAAP argument buffers before they can dominate memory.
fn validate_openai_function_call_argument_size(arguments: &str) -> Result<()> {
    if arguments.len() > OPENAI_FUNCTION_CALL_ARGUMENT_LIMIT_BYTES {
        return Err(MezError::invalid_state(format!(
            "OpenAI MAAP function call arguments exceeded {} bytes",
            OPENAI_FUNCTION_CALL_ARGUMENT_LIMIT_BYTES
        )));
    }
    Ok(())
}

/// Returns zero, one, or an error for collected native MAAP argument buffers.
fn one_openai_maap_function_call_arguments(arguments: Vec<String>) -> Result<Option<String>> {
    match arguments.len() {
        0 => Ok(None),
        1 => Ok(arguments.into_iter().next()),
        _ => Err(MezError::invalid_state(
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
